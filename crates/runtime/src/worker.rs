//! 背景求值執行緒：UI 送入 immutable graph snapshot，只處理最新的一份。

use crate::export::{ExportSpec, export};
use crate::{Image, Runtime};
use pixelnode_graph::Graph;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

pub enum Preview {
    /// 與 UI 共用 device：結果留在 GPU。`fresh` 為 true 時 texture 是新建的。
    Gpu { texture: wgpu::Texture, fresh: bool },
    /// 獨立 device：讀回 CPU。
    Cpu(Image),
}

pub enum Job {
    Eval { graph: Graph, frame: u32 },
    Export { graph: Graph, spec: ExportSpec },
}

pub enum Message {
    Eval(EvalResult),
    Export(Result<String, String>),
}

pub struct EvalResult {
    pub frame: u32,
    pub preview: Result<Preview, String>,
    pub elapsed: Duration,
    pub stats: crate::Stats,
    pub warnings: Vec<String>,
}

pub struct Evaluator {
    tx: Sender<Job>,
    rx: Receiver<Message>,
}

impl Evaluator {
    /// `shared` 為 UI 的 (device, queue, adapter 名稱)；None 時自建 device 並讀回 CPU。
    /// `notify` 在每次結果產生後呼叫（例如請 UI 重繪）。
    pub fn spawn(shared: Option<(wgpu::Device, wgpu::Queue, String)>, notify: impl Fn() + Send + 'static) -> Self {
        let (tx, job_rx) = mpsc::channel::<Job>();
        let (res_tx, rx) = mpsc::channel();
        let gpu_preview = shared.is_some();
        std::thread::Builder::new()
            .name("pixelnode-eval".into())
            .spawn(move || {
                let mut runtime = match shared {
                    Some((device, queue, name)) => Ok(Runtime::with_device(device, queue, name)),
                    None => Runtime::new(),
                };
                while let Ok(first) = job_rx.recv() {
                    // 匯出依序全部執行；求值只保留最新的一份（過時的 snapshot 丟棄）。
                    let mut latest_eval = None;
                    let mut exports = Vec::new();
                    for job in std::iter::once(first).chain(job_rx.try_iter()) {
                        match job {
                            Job::Eval { graph, frame } => latest_eval = Some((graph, frame)),
                            Job::Export { graph, spec } => exports.push((graph, spec)),
                        }
                    }
                    for (graph, spec) in exports {
                        let res = match &mut runtime {
                            Err(e) => Err(e.clone()),
                            Ok(rt) => export(rt, &graph, &spec),
                        };
                        if res_tx.send(Message::Export(res)).is_err() {
                            return;
                        }
                        notify();
                    }
                    let Some((graph, frame)) = latest_eval else { continue };
                    let start = Instant::now();
                    let opts = pixelnode_compiler::CompileOptions { frame, ..Default::default() };
                    let mut warnings = Vec::new();
                    let (preview, stats) = match &mut runtime {
                        Err(e) => (Err(e.clone()), Default::default()),
                        Ok(rt) => match pixelnode_compiler::compile_with(&graph, opts) {
                            Err(e) => (Err(e.to_string()), Default::default()),
                            Ok(plan) => {
                                let preview = if gpu_preview {
                                    let (texture, fresh) = rt.execute_to_display(&plan, frame);
                                    Preview::Gpu { texture, fresh }
                                } else {
                                    Preview::Cpu(rt.execute(&plan, frame))
                                };
                                warnings = std::mem::take(&mut rt.warnings);
                                (Ok(preview), rt.stats)
                            }
                        },
                    };
                    let msg = Message::Eval(EvalResult { frame, preview, elapsed: start.elapsed(), stats, warnings });
                    if res_tx.send(msg).is_err() {
                        return;
                    }
                    notify();
                }
            })
            .expect("failed to spawn eval thread");
        Self { tx, rx }
    }

    pub fn submit(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    /// 取得所有新結果（依序）。GPU 預覽的 `fresh` 旗標不能漏看，所以不只取最後一筆。
    pub fn poll(&self) -> Vec<Message> {
        self.rx.try_iter().collect()
    }
}

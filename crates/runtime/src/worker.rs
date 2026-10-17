//! 背景求值執行緒：UI 送入 immutable graph snapshot，只處理最新的一份。

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

pub struct EvalResult {
    pub preview: Result<Preview, String>,
    pub elapsed: Duration,
    pub stats: crate::Stats,
}

pub struct Evaluator {
    tx: Sender<Graph>,
    rx: Receiver<EvalResult>,
}

impl Evaluator {
    /// `shared` 為 UI 的 (device, queue, adapter 名稱)；None 時自建 device 並讀回 CPU。
    /// `notify` 在每次結果產生後呼叫（例如請 UI 重繪）。
    pub fn spawn(shared: Option<(wgpu::Device, wgpu::Queue, String)>, notify: impl Fn() + Send + 'static) -> Self {
        let (tx, job_rx) = mpsc::channel::<Graph>();
        let (res_tx, rx) = mpsc::channel();
        let gpu_preview = shared.is_some();
        std::thread::Builder::new()
            .name("pixelnode-eval".into())
            .spawn(move || {
                let mut runtime = match shared {
                    Some((device, queue, name)) => Ok(Runtime::with_device(device, queue, name)),
                    None => Runtime::new(),
                };
                while let Ok(mut graph) = job_rx.recv() {
                    // 丟棄過時的 snapshot，只算最新的。
                    while let Ok(newer) = job_rx.try_recv() {
                        graph = newer;
                    }
                    let start = Instant::now();
                    let (preview, stats) = match &mut runtime {
                        Err(e) => (Err(e.clone()), Default::default()),
                        Ok(rt) => match pixelnode_compiler::compile(&graph) {
                            Err(e) => (Err(e.to_string()), Default::default()),
                            Ok(plan) => {
                                let preview = if gpu_preview {
                                    let (texture, fresh) = rt.execute_to_display(&plan, 0);
                                    Preview::Gpu { texture, fresh }
                                } else {
                                    Preview::Cpu(rt.execute(&plan, 0))
                                };
                                (Ok(preview), rt.stats)
                            }
                        },
                    };
                    if res_tx.send(EvalResult { preview, elapsed: start.elapsed(), stats }).is_err() {
                        break;
                    }
                    notify();
                }
            })
            .expect("failed to spawn eval thread");
        Self { tx, rx }
    }

    pub fn submit(&self, graph: Graph) {
        let _ = self.tx.send(graph);
    }

    /// 取得所有新結果（依序）。GPU 預覽的 `fresh` 旗標不能漏看，所以不只取最後一筆。
    pub fn poll(&self) -> Vec<EvalResult> {
        self.rx.try_iter().collect()
    }
}

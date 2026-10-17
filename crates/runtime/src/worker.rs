//! 背景求值執行緒：UI 送入 immutable graph snapshot，只處理最新的一份。

use crate::{Image, Runtime};
use pixelnode_graph::Graph;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

pub struct EvalResult {
    pub image: Result<Image, String>,
    pub elapsed: Duration,
    pub stats: crate::Stats,
}

pub struct Evaluator {
    tx: Sender<Graph>,
    rx: Receiver<EvalResult>,
}

impl Evaluator {
    /// `notify` 在每次結果產生後呼叫（例如請 UI 重繪）。
    pub fn spawn(notify: impl Fn() + Send + 'static) -> Self {
        let (tx, job_rx) = mpsc::channel::<Graph>();
        let (res_tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("pixelnode-eval".into())
            .spawn(move || {
                let mut runtime = Runtime::new();
                while let Ok(mut graph) = job_rx.recv() {
                    // 丟棄過時的 snapshot，只算最新的。
                    while let Ok(newer) = job_rx.try_recv() {
                        graph = newer;
                    }
                    let start = Instant::now();
                    let (image, stats) = match &mut runtime {
                        Err(e) => (Err(e.clone()), Default::default()),
                        Ok(rt) => match pixelnode_compiler::compile(&graph) {
                            Err(e) => (Err(e.to_string()), Default::default()),
                            Ok(plan) => (Ok(rt.execute(&plan, 0)), rt.stats),
                        },
                    };
                    if res_tx.send(EvalResult { image, elapsed: start.elapsed(), stats }).is_err() {
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

    /// 取得最新結果（若有）。
    pub fn poll(&self) -> Option<EvalResult> {
        self.rx.try_iter().last()
    }
}

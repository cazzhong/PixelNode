//! 目標情境：100 節點、256×256、60 影格。
//!
//! `cargo bench -p pixelnode-bench`
//! 設 `PIXELNODE_BENCH_CHECK=1` 時，融合未勝過未融合即以非零結束（CI 用）。
//! 結果另寫入 `target/bench-pipeline.json`。

use pixelnode_bench::{animate_one, random_graph, tweak};
use pixelnode_compiler::{CompileOptions, compile_with};
use pixelnode_runtime::Runtime;
use std::time::{Duration, Instant};

const NODES: usize = 100;
const CANVAS: [u32; 2] = [256, 256];
const FRAMES: u32 = 60;

struct Row {
    name: &'static str,
    total: Duration,
    note: String,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() {
    let mut rt = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("略過 benchmark：{e}");
            return;
        }
    };
    let graph = random_graph(NODES, CANVAS, 42);
    let fused_opts = CompileOptions { fuse: true, ..Default::default() };
    let unfused_opts = CompileOptions { fuse: false, ..Default::default() };
    let mut rows = Vec::new();

    // 編譯（圖 → 計畫 + 全部 kernel 的 WGSL）
    let t = Instant::now();
    for _ in 0..FRAMES {
        let plan = compile_with(&graph, fused_opts).unwrap();
        for k in 0..plan.kernels.len() {
            std::hint::black_box(plan.kernel_wgsl(k));
        }
    }
    rows.push(Row { name: "compile+wgsl ×60", total: t.elapsed(), note: String::new() });

    let fused = compile_with(&graph, fused_opts).unwrap();
    let unfused = compile_with(&graph, unfused_opts).unwrap();

    // 暖機：編譯 pipeline（shader 編譯不計入逐格時間）。
    rt.run(&fused, 0);
    rt.clear_cache();
    rt.run(&unfused, 0);

    // 冷啟動：每格清空輸出快取（等同整張圖都與時間相關的最壞情況）。
    for (name, plan) in [("cold unfused ×60", &unfused), ("cold fused ×60", &fused)] {
        let t = Instant::now();
        for f in 0..FRAMES {
            rt.clear_cache();
            rt.run(plan, f);
        }
        rows.push(Row { name, total: t.elapsed(), note: format!("{} kernels", plan.kernels.len()) });
    }

    // 暖快取：圖與時間無關，整段動畫只算第一格。
    rt.clear_cache();
    let t = Instant::now();
    let mut dispatched = 0;
    for f in 0..FRAMES {
        rt.run(&fused, f);
        dispatched += rt.stats.dispatched;
    }
    rows.push(Row { name: "cached fused ×60", total: t.elapsed(), note: format!("{dispatched} dispatches") });

    // 動畫：只有一個 noise 的 phase 有 keyframe；其餘分支與時間無關，
    // 第一輪後每格只重算受影響的 kernel；第二輪整段命中影格快取。
    let mut g = graph.clone();
    animate_one(&mut g, FRAMES);
    rt.clear_cache();
    for name in ["animated 1 track, 1st loop ×60", "animated 1 track, 2nd loop ×60"] {
        let t = Instant::now();
        let mut dispatched = 0;
        for f in 0..FRAMES {
            let plan = compile_with(&g, CompileOptions { frame: f, ..fused_opts }).unwrap();
            rt.run(&plan, f);
            dispatched += rt.stats.dispatched;
        }
        rows.push(Row { name, total: t.elapsed(), note: format!("{dispatched} dispatches") });
    }

    // 拖 slider：每格改一個上游參數，含重新編譯計畫與讀回預覽。
    let mut g = graph.clone();
    let t = Instant::now();
    let (mut dispatched, mut compiled) = (0, 0);
    for f in 0..FRAMES {
        tweak(&mut g, f);
        let plan = compile_with(&g, fused_opts).unwrap();
        std::hint::black_box(rt.execute(&plan, f));
        dispatched += rt.stats.dispatched;
        compiled += rt.stats.compiled;
    }
    rows.push(Row {
        name: "slider drag ×60",
        total: t.elapsed(),
        note: format!("{dispatched} dispatches, {compiled} shader 重編"),
    });

    println!(
        "\nPixelNode pipeline — {} 節點, {}×{}, {FRAMES} 影格, adapter: {}\n",
        graph.nodes.len(),
        CANVAS[0],
        CANVAS[1],
        rt.adapter_name
    );
    println!("| 情境 | 總計 (ms) | 每格 (ms) | 備註 |\n|---|---:|---:|---|");
    for r in &rows {
        println!("| {} | {:.2} | {:.3} | {} |", r.name, ms(r.total), ms(r.total) / FRAMES as f64, r.note);
    }

    let json: Vec<String> = rows
        .iter()
        .map(|r| format!("{{\"name\":\"{}\",\"total_ms\":{:.3},\"note\":\"{}\"}}", r.name, ms(r.total), r.note))
        .collect();
    let target = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../target").into());
    let _ = std::fs::write(format!("{target}/bench-pipeline.json"), format!("[{}]\n", json.join(",")));

    let get = |n: &str| rows.iter().find(|r| r.name == n).unwrap().total;
    let speedup = ms(get("cold unfused ×60")) / ms(get("cold fused ×60"));
    println!("\n融合加速：{speedup:.2}×");
    if std::env::var_os("PIXELNODE_BENCH_CHECK").is_some() && speedup < 1.0 {
        eprintln!("效能回歸：融合版本比未融合慢");
        std::process::exit(1);
    }
}

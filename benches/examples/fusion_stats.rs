//! 印出 benchmark 圖的融合統計：`cargo run -p pixelnode-bench --example fusion_stats`
use pixelnode_bench::random_graph;

fn main() {
    let g = random_graph(100, [256, 256], 42);
    let plan = pixelnode_compiler::compile(&g).unwrap();
    let mut uses = vec![0usize; plan.steps.len()];
    for s in &plan.steps {
        for i in s.inputs.iter().flatten() {
            uses[*i] += 1;
        }
    }
    let mut by_kind = std::collections::BTreeMap::new();
    for k in &plan.kernels {
        let root = &plan.steps[k.members.last().unwrap().step];
        let i = k.members.last().unwrap().step;
        *by_kind.entry((root.def.kind, uses[i].min(5))).or_insert(0) += 1;
    }
    println!("steps {} kernels {}", plan.steps.len(), plan.kernels.len());
    for ((kind, u), c) in by_kind {
        println!("{kind:14} uses={u} ×{c}");
    }
}

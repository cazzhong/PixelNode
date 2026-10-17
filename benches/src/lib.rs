//! Benchmark 用的可重現測試圖產生器。

use pixelnode_graph::{Graph, Link, Node, NodeId, Track, Value};

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn unit(&mut self) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.0
    }
}

/// 產生約 `target` 個節點的 DAG：來源（noise / solid）→ 大量 color_adjust / blend
/// → 單一 Output。約 20% 的連線重用已被消費的節點，製造融合邊界。
pub fn random_graph(target: usize, canvas: [u32; 2], seed: u64) -> Graph {
    let mut rng = Lcg(seed);
    let mut g = Graph { canvas, ..Default::default() };
    let mut next_id = 1u64;
    let mut frontier: Vec<NodeId> = Vec::new();
    let mut all: Vec<NodeId> = Vec::new();

    let mut add = |g: &mut Graph, kind: &str, rng: &mut Lcg| {
        let id = NodeId(next_id);
        next_id += 1;
        let mut node = Node { id, kind: kind.into(), params: Default::default(), pos: [0.0; 2], anim: Default::default() };
        match kind {
            "noise" => {
                node.params.insert("type".into(), Value::Enum(rng.below(3) as u32));
                node.params.insert("scale".into(), Value::Float(2.0 + rng.unit() * 20.0));
                node.params.insert("seed".into(), Value::Int(rng.below(1000) as i32));
            }
            "solid" => {
                node.params.insert("color".into(), Value::Color([rng.unit(), rng.unit(), rng.unit(), 1.0]));
            }
            "color_adjust" => {
                node.params.insert("hue".into(), Value::Float(rng.unit() - 0.5));
                node.params.insert("contrast".into(), Value::Float(0.5 + rng.unit()));
            }
            "blend" => {
                node.params.insert("mode".into(), Value::Enum(rng.below(6) as u32));
                node.params.insert("opacity".into(), Value::Float(0.3 + rng.unit() * 0.7));
            }
            _ => {}
        }
        g.nodes.push(node);
        id
    };
    let connect = |g: &mut Graph, from: NodeId, to: NodeId, port: usize| {
        g.links.push(Link { from_node: from, from_port: 0, to_node: to, to_port: port });
    };
    let pick = |rng: &mut Lcg, frontier: &mut Vec<NodeId>, all: &[NodeId]| {
        if frontier.is_empty() || rng.below(5) == 0 {
            all[rng.below(all.len())]
        } else {
            frontier.swap_remove(rng.below(frontier.len()))
        }
    };

    let sources = (target / 5).max(2);
    for _ in 0..sources {
        let kind = if rng.below(4) == 0 { "solid" } else { "noise" };
        let id = add(&mut g, kind, &mut rng);
        frontier.push(id);
        all.push(id);
    }
    // 預留收斂與 Output 所需的節點數。
    while g.nodes.len() + frontier.len() < target {
        let id = if rng.below(2) == 0 {
            let id = add(&mut g, "color_adjust", &mut rng);
            let a = pick(&mut rng, &mut frontier, &all);
            connect(&mut g, a, id, 0);
            id
        } else {
            let id = add(&mut g, "blend", &mut rng);
            let a = pick(&mut rng, &mut frontier, &all);
            let b = pick(&mut rng, &mut frontier, &all);
            connect(&mut g, a, id, 0);
            connect(&mut g, b, id, 1);
            id
        };
        frontier.push(id);
        all.push(id);
    }
    // 收斂成單一節點。
    while frontier.len() > 1 {
        let a = frontier.swap_remove(0);
        let b = frontier.swap_remove(0);
        let id = add(&mut g, "blend", &mut rng);
        connect(&mut g, a, id, 0);
        connect(&mut g, b, id, 1);
        frontier.push(id);
    }
    let out = add(&mut g, "output", &mut rng);
    connect(&mut g, frontier[0], out, 0);
    g
}

/// 讓最後一個 noise 的 phase 在 `frames` 格內 0→1 循環。
pub fn animate_one(g: &mut Graph, frames: u32) {
    if let Some(n) = g.nodes.iter_mut().rev().find(|n| n.kind == "noise") {
        let mut t = Track::default();
        t.set_key(0, Value::Float(0.0));
        t.set_key(frames, Value::Float(1.0));
        n.anim.insert("phase".into(), t);
    }
}

/// 調整某個 noise 節點的參數，模擬拖 slider。
pub fn tweak(g: &mut Graph, frame: u32) {
    if let Some(n) = g.nodes.iter_mut().find(|n| n.kind == "noise") {
        n.params.insert("scale".into(), Value::Float(4.0 + frame as f32 * 0.1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_is_valid_and_fully_reachable() {
        let g = random_graph(100, [256, 256], 1);
        assert!((95..=110).contains(&g.nodes.len()), "{}", g.nodes.len());
        let plan = pixelnode_compiler::compile(&g).unwrap();
        assert_eq!(plan.steps.len(), g.nodes.len());
    }
}

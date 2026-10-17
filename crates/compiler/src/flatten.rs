//! Subgraph / Loop 展開：編譯前把實例節點替換成子圖內容（Loop 依次數展開），
//! 得到單層圖，融合器因此能跨子圖邊界融合。

use crate::CompileError;
use pixelnode_graph::{Graph, Link, Node, NodeId, Value};
use std::collections::HashMap;

/// 子圖巢狀展開的最大深度（防止子圖互相引用造成無限展開）。
pub const MAX_DEPTH: usize = 8;
/// Loop 最多展開次數。
pub const MAX_ITERATIONS: u32 = 64;

pub fn needs_flatten(g: &Graph) -> bool {
    g.nodes.iter().any(|n| is_container(&n.kind) || matches!(n.kind.as_str(), "group_in" | "group_out" | "loop_index"))
}

fn is_container(kind: &str) -> bool {
    matches!(kind, "subgraph" | "loop")
}

fn text_param(n: &Node, name: &str) -> String {
    match n.params.get(name) {
        Some(Value::Text(t)) => t.clone(),
        _ => String::new(),
    }
}

fn int_param(n: &Node, name: &str, default: i32) -> i32 {
    match n.params.get(name) {
        Some(Value::Int(i)) => *i,
        Some(Value::Float(f)) => f.round() as i32,
        _ => default,
    }
}

/// 端點：某節點的某個輸出 port。
type Source = (NodeId, usize);

pub fn flatten(root: &Graph) -> Result<Graph, CompileError> {
    let mut next_id = max_id(root) + 1;
    let mut out = Graph { subgraphs: Default::default(), ..root.clone() };
    expand(&mut out, root, &mut next_id, 0)?;
    // 頂層殘留的 group_in（不在子圖中）→ 無來源；group_out / loop_index 也一併移除。
    strip_interface(&mut out, &HashMap::new());
    Ok(out)
}

fn max_id(g: &Graph) -> u64 {
    let own = g.nodes.iter().map(|n| n.id.0).max().unwrap_or(0);
    g.subgraphs.values().map(max_id).max().unwrap_or(0).max(own)
}

/// 把 `g` 內所有 Subgraph / Loop 實例展開（遞迴），子圖定義取自 `lib`。
fn expand(g: &mut Graph, lib: &Graph, next_id: &mut u64, depth: usize) -> Result<(), CompileError> {
    while let Some(pos) = g.nodes.iter().position(|n| is_container(&n.kind)) {
        if depth >= MAX_DEPTH {
            return Err(CompileError::Subgraph("子圖巢狀過深（可能互相引用）".into()));
        }
        let inst = g.nodes.remove(pos);
        let name = text_param(&inst, "name");
        let def = lib
            .subgraphs
            .get(&name)
            .ok_or_else(|| CompileError::Subgraph(format!("找不到子圖「{name}」")))?;
        let iterations = if inst.kind == "loop" {
            int_param(&inst, "iterations", 4).clamp(1, MAX_ITERATIONS as i32) as u32
        } else {
            1
        };

        // 實例的外部輸入來源（port 0 / 1）。
        let ext: [Option<Source>; 2] = std::array::from_fn(|port| {
            g.links.iter().find(|l| l.to_node == inst.id && l.to_port == port).map(|l| (l.from_node, l.from_port))
        });
        g.links.retain(|l| l.to_node != inst.id);

        let mut feedback = ext[0];
        let mut result: Option<Source> = None;
        for k in 0..iterations {
            // 複製子圖（先遞迴展開子圖內部的實例）。
            let mut body = Graph { subgraphs: Default::default(), ..def.clone() };
            body.nodes.retain(|n| n.kind != "output");
            expand(&mut body, lib, next_id, depth + 1)?;
            let remap: HashMap<NodeId, NodeId> = body
                .nodes
                .iter()
                .map(|n| {
                    *next_id += 1;
                    (n.id, NodeId(*next_id))
                })
                .collect();
            for mut n in body.nodes {
                n.id = remap[&n.id];
                if n.kind == "loop_index" {
                    n.params.insert("value".into(), Value::Float(k as f32));
                    n.params.insert("count".into(), Value::Float(iterations as f32));
                }
                g.nodes.push(n);
            }
            for l in body.links {
                g.links.push(Link { from_node: remap[&l.from_node], to_node: remap[&l.to_node], ..l });
            }
            // 介面：group_in 0 = 回饋（第一次為外部輸入 1），group_in 1 = 外部輸入 2。
            let inputs = [feedback, ext[1]];
            let ins: Vec<NodeId> = remap.values().copied().collect();
            result = connect_interface(g, &ins, inputs);
            feedback = result;
        }

        // 實例的下游改接到最後一次的結果。
        g.links = std::mem::take(&mut g.links)
            .into_iter()
            .filter_map(|l| {
                if l.from_node != inst.id {
                    return Some(l);
                }
                result.map(|(n, p)| Link { from_node: n, from_port: p, ..l })
            })
            .collect();
    }
    Ok(())
}

/// 處理剛複製進來、id 在 `ids` 中的 group_in / group_out 節點：
/// group_in 的下游改接外部來源，回傳 group_out 的輸入來源，並移除介面節點。
fn connect_interface(g: &mut Graph, ids: &[NodeId], inputs: [Option<Source>; 2]) -> Option<Source> {
    let mut sources: HashMap<NodeId, Option<Source>> = HashMap::new();
    let mut result = None;
    for n in g.nodes.iter().filter(|n| ids.contains(&n.id)) {
        match n.kind.as_str() {
            "group_in" => {
                let idx = match n.params.get("index") {
                    Some(Value::Enum(i)) => *i as usize,
                    _ => 0,
                };
                sources.insert(n.id, inputs.get(idx).copied().flatten());
            }
            "group_out" => {
                result = g.links.iter().find(|l| l.to_node == n.id).map(|l| (l.from_node, l.from_port));
            }
            _ => {}
        }
    }
    let outs: Vec<NodeId> = g.nodes.iter().filter(|n| ids.contains(&n.id) && n.kind == "group_out").map(|n| n.id).collect();
    for id in outs {
        sources.insert(id, None);
    }
    strip_interface(g, &sources);
    result
}

/// 移除介面節點；指向它們的連線依 `sources` 改接（沒有來源就刪掉）。
/// `sources` 沒列出的 group_in / group_out / 頂層殘留介面一律視為無來源。
fn strip_interface(g: &mut Graph, sources: &HashMap<NodeId, Option<Source>>) {
    let is_iface = |n: &Node| matches!(n.kind.as_str(), "group_in" | "group_out");
    let iface: HashMap<NodeId, Option<Source>> = g
        .nodes
        .iter()
        .filter(|n| is_iface(n) && (sources.is_empty() || sources.contains_key(&n.id)))
        .map(|n| (n.id, sources.get(&n.id).copied().flatten()))
        .collect();
    g.links = std::mem::take(&mut g.links)
        .into_iter()
        .filter_map(|l| {
            if iface.contains_key(&l.to_node) {
                return None;
            }
            match iface.get(&l.from_node) {
                None => Some(l),
                Some(src) => src.map(|(n, p)| Link { from_node: n, from_port: p, ..l }),
            }
        })
        .collect();
    g.nodes.retain(|n| !iface.contains_key(&n.id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixelnode_graph::Value;

    fn node(id: u64, kind: &str) -> Node {
        Node { id: NodeId(id), kind: kind.into(), params: Default::default(), pos: [0.0; 2], anim: Default::default() }
    }
    fn link(a: u64, b: u64, port: usize) -> Link {
        Link { from_node: NodeId(a), from_port: 0, to_node: NodeId(b), to_port: port }
    }
    fn named(id: u64, kind: &str, name: &str) -> Node {
        let mut n = node(id, kind);
        n.params.insert("name".into(), Value::Text(name.into()));
        n
    }

    /// 子圖 "tint"：group_in → color_adjust → group_out
    fn lib_graph(root_nodes: Vec<Node>, root_links: Vec<Link>) -> Graph {
        let sub = Graph {
            nodes: vec![node(1, "group_in"), node(2, "color_adjust"), node(3, "group_out")],
            links: vec![link(1, 2, 0), link(2, 3, 0)],
            ..Default::default()
        };
        let mut g = Graph { nodes: root_nodes, links: root_links, ..Default::default() };
        g.subgraphs.insert("tint".into(), sub);
        g
    }

    #[test]
    fn subgraph_inlines() {
        let g = lib_graph(
            vec![node(10, "noise"), named(11, "subgraph", "tint"), node(12, "output")],
            vec![link(10, 11, 0), link(11, 12, 0)],
        );
        let f = flatten(&g).unwrap();
        let kinds: Vec<&str> = f.nodes.iter().map(|n| n.kind.as_str()).collect();
        assert_eq!(kinds.iter().filter(|k| **k == "color_adjust").count(), 1);
        assert!(!kinds.iter().any(|k| matches!(*k, "subgraph" | "group_in" | "group_out")));
        let plan = crate::compile(&g).unwrap();
        let order: Vec<&str> = plan.steps.iter().map(|s| s.def.kind).collect();
        assert_eq!(order, vec!["noise", "color_adjust", "output"]);
        assert_eq!(plan.kernels.len(), 1, "跨子圖邊界仍融合成一個 kernel");
    }

    #[test]
    fn loop_unrolls_with_feedback_and_index() {
        let mut g = lib_graph(
            vec![node(10, "noise"), named(11, "loop", "tint"), node(12, "output")],
            vec![link(10, 11, 0), link(11, 12, 0)],
        );
        g.nodes[1].params.insert("iterations".into(), Value::Int(5));
        // 子圖內用 loop_index 驅動 hue
        let sub = g.subgraphs.get_mut("tint").unwrap();
        sub.nodes.push(node(4, "loop_index"));
        let hue_port = pixelnode_nodes::find("color_adjust").unwrap().param_port_of(2).unwrap();
        sub.links.push(Link { from_node: NodeId(4), from_port: 0, to_node: NodeId(2), to_port: hue_port });
        let plan = crate::compile(&g).unwrap();
        let adj: Vec<_> = plan.steps.iter().filter(|s| s.def.kind == "color_adjust").collect();
        assert_eq!(adj.len(), 5);
        let hues: Vec<f32> = adj.iter().map(|s| s.params[2][0]).collect();
        assert_eq!(hues, vec![0.0, 1.0, 2.0, 3.0, 4.0], "第 k 次的 loop index = k，且依回饋順序串接");
    }

    #[test]
    fn missing_and_recursive_subgraphs_error() {
        let g = lib_graph(vec![named(1, "subgraph", "nope"), node(2, "output")], vec![link(1, 2, 0)]);
        assert!(matches!(crate::compile(&g), Err(CompileError::Subgraph(_))));

        let mut g = lib_graph(vec![named(1, "subgraph", "a"), node(2, "output")], vec![link(1, 2, 0)]);
        g.subgraphs.insert(
            "a".into(),
            Graph { nodes: vec![named(1, "subgraph", "a"), node(2, "group_out")], links: vec![link(1, 2, 0)], ..Default::default() },
        );
        assert!(matches!(crate::compile(&g), Err(CompileError::Subgraph(_))));
    }
}

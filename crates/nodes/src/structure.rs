//! 結構節點：Subgraph、Loop 與子圖介面。編譯前由 compiler 展開，不會產生 kernel。

use crate::{Category, NodeDef, ParamDef, ParamKind, PortDef, choice, int};
use pixelnode_graph::{DataType, Value};

const TWO_IN: &[PortDef] =
    &[PortDef { name: "輸入 1", ty: DataType::Surface }, PortDef { name: "輸入 2", ty: DataType::Surface }];
const SURF_OUT: &[PortDef] = &[PortDef { name: "out", ty: DataType::Surface }];

/// 這些種類由 compiler 展開，執行期不存在。
pub const STRUCTURAL: &[&str] = &["subgraph", "loop", "group_in", "group_out", "loop_index"];

fn name_param() -> ParamDef {
    ParamDef { name: "name", label: "子圖", kind: ParamKind::Text { multiline: false }, default: Value::Text(String::new()) }
}

fn placeholder(kind: &'static str, title: &'static str, inputs: &'static [PortDef], outputs: &'static [PortDef], params: Vec<ParamDef>) -> NodeDef {
    NodeDef {
        kind,
        title,
        category: Category::Structure,
        inputs,
        outputs,
        params,
        per_pixel: true,
        time_invariant: true,
        cost: 0,
        wgsl: "fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(0.0); }",
        eval: None,
    }
}

pub(crate) fn all() -> Vec<NodeDef> {
    vec![
        placeholder("subgraph", "Subgraph", TWO_IN, SURF_OUT, vec![name_param()]),
        placeholder("loop", "Loop", TWO_IN, SURF_OUT, vec![name_param(), int("iterations", "次數", 1, 64, 4)]),
        placeholder("group_in", "Group In", &[], SURF_OUT, vec![choice("index", "對應", &["輸入 1（Loop 回饋）", "輸入 2"], 0)]),
        placeholder("group_out", "Group Out", &[PortDef { name: "in", ty: DataType::Surface }], &[], vec![]),
        NodeDef {
            // 數值節點：展開 Loop 時由 compiler 寫入 value / count。
            outputs: &[PortDef { name: "第幾次", ty: DataType::Float }],
            eval: Some(|p, _| p[0].clone()),
            ..placeholder(
                "loop_index",
                "Loop Index",
                &[],
                &[],
                vec![
                    ParamDef { name: "value", label: "", kind: ParamKind::Internal, default: Value::Float(0.0) },
                    ParamDef { name: "count", label: "", kind: ParamKind::Internal, default: Value::Float(1.0) },
                ],
            )
        },
    ]
}

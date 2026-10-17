//! 節點圖資料模型、型別系統與序列化。無 GPU 依賴。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 存檔格式版本；格式變動時遞增並在 [`Graph::from_json`] 中做 migration。
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataType {
    Surface,
    Float,
    Int,
    Vec2,
    Vec3,
    Vec4,
    Color,
    Gradient,
    Curve,
    Path,
    Particle,
}

impl DataType {
    /// `from` 能否接到型別為 `self` 的輸入（含隱式轉換）。
    pub fn accepts(self, from: DataType) -> bool {
        use DataType::*;
        self == from
            || matches!(
                (from, self),
                (Int, Float)
                    | (Float, Color)
                    | (Float, Vec4)
                    | (Color, Vec4)
                    | (Vec4, Color)
                    | (Float, Surface)
                    | (Color, Surface)
            )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v")]
pub enum Value {
    Float(f32),
    Int(i32),
    Bool(bool),
    Vec2([f32; 2]),
    Color([f32; 4]),
    /// 列舉型參數，存選項索引。
    Enum(u32),
}

impl Value {
    /// 打包成一個 vec4 uniform 槽位。
    pub fn to_vec4(&self) -> [f32; 4] {
        match *self {
            Value::Float(f) => [f, 0.0, 0.0, 0.0],
            Value::Int(i) => [i as f32, 0.0, 0.0, 0.0],
            Value::Bool(b) => [b as u8 as f32, 0.0, 0.0, 0.0],
            Value::Vec2([x, y]) => [x, y, 0.0, 0.0],
            Value::Color(c) => c,
            Value::Enum(e) => [e as f32, 0.0, 0.0, 0.0],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u64);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: String,
    pub params: BTreeMap<String, Value>,
    #[serde(default)]
    pub pos: [f32; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Link {
    pub from_node: NodeId,
    pub from_port: usize,
    pub to_node: NodeId,
    pub to_port: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub version: u32,
    pub canvas: [u32; 2],
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
}

impl Default for Graph {
    fn default() -> Self {
        Self { version: FORMAT_VERSION, canvas: [128, 128], nodes: Vec::new(), links: Vec::new() }
    }
}

#[derive(Debug)]
pub enum LoadError {
    Json(serde_json::Error),
    UnsupportedVersion(u32),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Json(e) => write!(f, "JSON 解析失敗：{e}"),
            LoadError::UnsupportedVersion(v) => write!(f, "不支援的檔案版本 {v}（目前 {FORMAT_VERSION}）"),
        }
    }
}

impl std::error::Error for LoadError {}

impl Graph {
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// 某輸入 port 的來源（每個輸入最多一條連線）。
    pub fn input_source(&self, node: NodeId, port: usize) -> Option<(NodeId, usize)> {
        self.links
            .iter()
            .find(|l| l.to_node == node && l.to_port == port)
            .map(|l| (l.from_node, l.from_port))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("graph serialization cannot fail")
    }

    pub fn from_json(s: &str) -> Result<Self, LoadError> {
        let raw: serde_json::Value = serde_json::from_str(s).map_err(LoadError::Json)?;
        let version = raw.get("version").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        if version > FORMAT_VERSION || version == 0 {
            return Err(LoadError::UnsupportedVersion(version));
        }
        // 未來版本的 migration 在此逐步升級 `raw`。
        let mut g: Graph = serde_json::from_value(raw).map_err(LoadError::Json)?;
        g.version = FORMAT_VERSION;
        Ok(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Graph {
        let mut params = BTreeMap::new();
        params.insert("scale".into(), Value::Float(4.0));
        params.insert("color".into(), Value::Color([1.0, 0.5, 0.0, 1.0]));
        Graph {
            nodes: vec![
                Node { id: NodeId(1), kind: "noise".into(), params, pos: [0.0, 0.0] },
                Node { id: NodeId(2), kind: "output".into(), params: BTreeMap::new(), pos: [200.0, 0.0] },
            ],
            links: vec![Link { from_node: NodeId(1), from_port: 0, to_node: NodeId(2), to_port: 0 }],
            ..Default::default()
        }
    }

    #[test]
    fn json_roundtrip() {
        let g = sample();
        assert_eq!(Graph::from_json(&g.to_json()).unwrap(), g);
    }

    #[test]
    fn rejects_future_version() {
        let mut g = sample();
        g.version = FORMAT_VERSION + 1;
        assert!(matches!(Graph::from_json(&g.to_json()), Err(LoadError::UnsupportedVersion(_))));
    }

    #[test]
    fn implicit_conversions() {
        assert!(DataType::Color.accepts(DataType::Float));
        assert!(DataType::Surface.accepts(DataType::Color));
        assert!(!DataType::Float.accepts(DataType::Surface));
    }

    #[test]
    fn input_source_lookup() {
        let g = sample();
        assert_eq!(g.input_source(NodeId(2), 0), Some((NodeId(1), 0)));
        assert_eq!(g.input_source(NodeId(1), 0), None);
    }
}

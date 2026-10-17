//! 宣告式節點定義：port / 參數描述 + 一段 WGSL 函式片段。
//!
//! WGSL 片段契約（供執行器包裝，日後供融合器拼接）：
//! - 定義 `fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32>`。
//! - 可用 `P[i]`（第 i 個參數，打包為 vec4）、`in_at(k, px)`（第 k 個輸入）、
//!   `SIZE`（輸出尺寸 vec2<u32>）、`FRAME`（影格 u32）。
//! - 可額外宣告以 `<kind>_` 為前綴的輔助函式；共用輔助函式放在 [`COMMON_WGSL`]。

use pixelnode_graph::{DataType, Value};

#[derive(Debug, Clone, Copy)]
pub struct PortDef {
    pub name: &'static str,
    pub ty: DataType,
}

#[derive(Debug, Clone, Copy)]
pub enum ParamKind {
    Float { min: f32, max: f32 },
    Int { min: i32, max: i32 },
    Color,
    Enum(&'static [&'static str]),
}

#[derive(Debug, Clone)]
pub struct ParamDef {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: ParamKind,
    pub default: Value,
}

#[derive(Debug, Clone)]
pub struct NodeDef {
    pub kind: &'static str,
    pub title: &'static str,
    pub inputs: &'static [PortDef],
    pub outputs: &'static [PortDef],
    pub params: Vec<ParamDef>,
    /// 逐像素節點可被融合；需鄰域 / 全域資訊者為 false。
    pub per_pixel: bool,
    /// 與時間無關（動畫中只需算一次）。
    pub time_invariant: bool,
    pub wgsl: &'static str,
}

impl NodeDef {
    pub fn default_params(&self) -> std::collections::BTreeMap<String, Value> {
        self.params.iter().map(|p| (p.name.to_string(), p.default.clone())).collect()
    }
}

/// 每個節點最多可用的參數槽位數（對應 uniform 中的 `array<vec4<f32>, MAX_PARAMS>`）。
pub const MAX_PARAMS: usize = 8;
/// 每個節點最多的輸入數。
pub const MAX_INPUTS: usize = 2;

const SURF_OUT: &[PortDef] = &[PortDef { name: "out", ty: DataType::Surface }];

pub const COMMON_WGSL: &str = r#"
fn pn_hash2(p: vec2<f32>, seed: f32) -> f32 {
    var q = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031 + seed * 0.0137);
    q = q + dot(q, q.yzx + 33.33);
    return fract((q.x + q.y) * q.z);
}
fn pn_grad2(p: vec2<f32>, seed: f32) -> vec2<f32> {
    let a = pn_hash2(p, seed) * 6.2831853;
    return vec2<f32>(cos(a), sin(a));
}
fn pn_rgb2hsv(c: vec3<f32>) -> vec3<f32> {
    let k = vec4<f32>(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
    let p = mix(vec4<f32>(c.bg, k.wz), vec4<f32>(c.gb, k.xy), step(c.b, c.g));
    let q = mix(vec4<f32>(p.xyw, c.r), vec4<f32>(c.r, p.yzx), step(p.x, c.r));
    let d = q.x - min(q.w, q.y);
    let e = 1.0e-10;
    return vec3<f32>(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}
fn pn_hsv2rgb(c: vec3<f32>) -> vec3<f32> {
    let p = abs(fract(c.xxx + vec3<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0);
    return c.z * mix(vec3<f32>(1.0), clamp(p - 1.0, vec3<f32>(0.0), vec3<f32>(1.0)), c.y);
}
"#;

pub fn registry() -> &'static [NodeDef] {
    static REG: std::sync::OnceLock<Vec<NodeDef>> = std::sync::OnceLock::new();
    REG.get_or_init(|| vec![solid(), noise(), color_adjust(), blend(), output()])
}

pub fn find(kind: &str) -> Option<&'static NodeDef> {
    registry().iter().find(|d| d.kind == kind)
}

fn f(name: &'static str, label: &'static str, min: f32, max: f32, v: f32) -> ParamDef {
    ParamDef { name, label, kind: ParamKind::Float { min, max }, default: Value::Float(v) }
}

fn solid() -> NodeDef {
    NodeDef {
        kind: "solid",
        title: "Solid",
        inputs: &[],
        outputs: SURF_OUT,
        params: vec![ParamDef {
            name: "color",
            label: "顏色",
            kind: ParamKind::Color,
            default: Value::Color([0.2, 0.4, 0.8, 1.0]),
        }],
        per_pixel: true,
        time_invariant: true,
        wgsl: "fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> { return P[0]; }",
    }
}

fn noise() -> NodeDef {
    NodeDef {
        kind: "noise",
        title: "Noise",
        inputs: &[],
        outputs: SURF_OUT,
        params: vec![
            ParamDef {
                name: "type",
                label: "類型",
                kind: ParamKind::Enum(&["Value", "Perlin", "Cellular"]),
                default: Value::Enum(1),
            },
            f("scale", "縮放", 1.0, 64.0, 8.0),
            ParamDef { name: "seed", label: "種子", kind: ParamKind::Int { min: 0, max: 9999 }, default: Value::Int(0) },
            ParamDef { name: "octaves", label: "八度", kind: ParamKind::Int { min: 1, max: 8 }, default: Value::Int(3) },
            f("contrast", "對比", 0.1, 4.0, 1.0),
        ],
        per_pixel: true,
        time_invariant: true,
        wgsl: r#"
fn noise_value(p: vec2<f32>, s: f32) -> f32 {
    let i = floor(p); let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = pn_hash2(i, s); let b = pn_hash2(i + vec2<f32>(1.0, 0.0), s);
    let c = pn_hash2(i + vec2<f32>(0.0, 1.0), s); let d = pn_hash2(i + vec2<f32>(1.0, 1.0), s);
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}
fn noise_perlin(p: vec2<f32>, s: f32) -> f32 {
    let i = floor(p); let f = fract(p);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = dot(pn_grad2(i, s), f);
    let b = dot(pn_grad2(i + vec2<f32>(1.0, 0.0), s), f - vec2<f32>(1.0, 0.0));
    let c = dot(pn_grad2(i + vec2<f32>(0.0, 1.0), s), f - vec2<f32>(0.0, 1.0));
    let d = dot(pn_grad2(i + vec2<f32>(1.0, 1.0), s), f - vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y) * 0.7071 + 0.5;
}
fn noise_cellular(p: vec2<f32>, s: f32) -> f32 {
    let i = floor(p); let f = fract(p);
    var md = 8.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let o = vec2<f32>(f32(x), f32(y));
            let pt = vec2<f32>(pn_hash2(i + o, s), pn_hash2(i + o, s + 71.0));
            md = min(md, length(o + pt - f));
        }
    }
    return clamp(md, 0.0, 1.0);
}
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let kind = i32(P[0].x);
    let seed = P[2].x;
    let oct = i32(P[3].x);
    var p = uv * P[1].x;
    var amp = 0.5; var sum = 0.0; var norm = 0.0;
    for (var o = 0; o < oct; o++) {
        var n: f32;
        if (kind == 0) { n = noise_value(p, seed); }
        else if (kind == 1) { n = noise_perlin(p, seed); }
        else { n = noise_cellular(p, seed); }
        sum += n * amp; norm += amp; amp *= 0.5; p = p * 2.0 + vec2<f32>(17.3, 9.1);
    }
    let v = clamp((sum / norm - 0.5) * P[4].x + 0.5, 0.0, 1.0);
    return vec4<f32>(v, v, v, 1.0);
}
"#,
    }
}

fn color_adjust() -> NodeDef {
    NodeDef {
        kind: "color_adjust",
        title: "Color Adjust",
        inputs: &[PortDef { name: "in", ty: DataType::Surface }],
        outputs: SURF_OUT,
        params: vec![
            f("brightness", "亮度", -1.0, 1.0, 0.0),
            f("contrast", "對比", 0.0, 3.0, 1.0),
            f("hue", "色相", -0.5, 0.5, 0.0),
            f("saturation", "飽和度", 0.0, 2.0, 1.0),
            ParamDef { name: "tint", label: "染色", kind: ParamKind::Color, default: Value::Color([1.0, 1.0, 1.0, 1.0]) },
        ],
        per_pixel: true,
        time_invariant: true,
        wgsl: r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let src = in_at(0u, px);
    var c = (src.rgb - 0.5) * P[1].x + 0.5 + P[0].x;
    var hsv = pn_rgb2hsv(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)));
    hsv.x = fract(hsv.x + P[2].x);
    hsv.y = clamp(hsv.y * P[3].x, 0.0, 1.0);
    c = pn_hsv2rgb(hsv) * P[4].rgb;
    return vec4<f32>(c, src.a * P[4].a);
}
"#,
    }
}

fn blend() -> NodeDef {
    NodeDef {
        kind: "blend",
        title: "Blend",
        inputs: &[PortDef { name: "bg", ty: DataType::Surface }, PortDef { name: "fg", ty: DataType::Surface }],
        outputs: SURF_OUT,
        params: vec![
            ParamDef {
                name: "mode",
                label: "模式",
                kind: ParamKind::Enum(&["Normal", "Multiply", "Add", "Screen", "Overlay", "Difference"]),
                default: Value::Enum(0),
            },
            f("opacity", "不透明度", 0.0, 1.0, 1.0),
        ],
        per_pixel: true,
        time_invariant: true,
        wgsl: r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let a = in_at(0u, px);
    let b = in_at(1u, px);
    let m = i32(P[0].x);
    var c: vec3<f32>;
    if (m == 1) { c = a.rgb * b.rgb; }
    else if (m == 2) { c = min(a.rgb + b.rgb, vec3<f32>(1.0)); }
    else if (m == 3) { c = 1.0 - (1.0 - a.rgb) * (1.0 - b.rgb); }
    else if (m == 4) { c = select(1.0 - 2.0 * (1.0 - a.rgb) * (1.0 - b.rgb), 2.0 * a.rgb * b.rgb, a.rgb < vec3<f32>(0.5)); }
    else if (m == 5) { c = abs(a.rgb - b.rgb); }
    else { c = b.rgb; }
    let t = b.a * P[1].x;
    return vec4<f32>(mix(a.rgb, c, t), max(a.a, t));
}
"#,
    }
}

fn output() -> NodeDef {
    NodeDef {
        kind: "output",
        title: "Output",
        inputs: &[PortDef { name: "in", ty: DataType::Surface }],
        outputs: &[],
        params: vec![],
        per_pixel: true,
        time_invariant: true,
        wgsl: "fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> { return in_at(0u, px); }",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_consistent() {
        let reg = registry();
        for d in reg {
            assert!(d.params.len() <= MAX_PARAMS, "{}", d.kind);
            assert!(d.inputs.len() <= MAX_INPUTS, "{}", d.kind);
            assert!(d.wgsl.contains("fn node_fn"), "{}", d.kind);
        }
        let mut kinds: Vec<_> = reg.iter().map(|d| d.kind).collect();
        kinds.dedup();
        assert_eq!(kinds.len(), reg.len());
    }
}

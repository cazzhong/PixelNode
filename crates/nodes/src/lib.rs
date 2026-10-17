//! 宣告式節點定義：port / 參數描述 + 一段 WGSL 函式片段。
//!
//! WGSL 片段契約（供執行器包裝，日後供融合器拼接）：
//! - 定義 `fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32>`。
//! - 可用 `P[i]`（第 i 個參數，打包為 vec4）、`in_at(k, px)`（第 k 個輸入）、
//!   `SIZE`（輸出尺寸 vec2<u32>）、`FRAME`（影格 u32）。
//! - 可額外宣告輔助函式（融合時會自動加前綴避免撞名）；共用輔助函式放在 [`COMMON_WGSL`]。
//! - `in_at` 可在任意位置取樣（例如 Transform）；被融合的上游會在該位置重算。

mod library;

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
    Text { multiline: bool },
}

#[derive(Debug, Clone)]
pub struct ParamDef {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: ParamKind,
    pub default: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Generate,
    Transform,
    Filter,
    Color,
    Composite,
    Output,
}

impl Category {
    pub const ALL: [Category; 6] =
        [Category::Generate, Category::Transform, Category::Filter, Category::Color, Category::Composite, Category::Output];

    pub fn label(self) -> &'static str {
        match self {
            Category::Generate => "生成",
            Category::Transform => "變換",
            Category::Filter => "濾鏡",
            Category::Color => "顏色",
            Category::Composite => "合成",
            Category::Output => "輸出",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NodeDef {
    pub kind: &'static str,
    pub title: &'static str,
    pub category: Category,
    pub inputs: &'static [PortDef],
    pub outputs: &'static [PortDef],
    pub params: Vec<ParamDef>,
    /// 每個輸出像素對每個輸入只取樣一次 → 上游可融合進來重算。
    /// 需鄰域（Blur / Outline）或全域資訊者為 false，成為融合邊界。
    pub per_pixel: bool,
    /// 與時間無關（動畫中只需算一次）。
    pub time_invariant: bool,
    /// 每像素相對成本估計（約略 ALU 量），融合器用來決定共用節點要不要重算。
    pub cost: u32,
    pub wgsl: &'static str,
}

impl NodeDef {
    pub fn default_params(&self) -> std::collections::BTreeMap<String, Value> {
        self.params.iter().map(|p| (p.name.to_string(), p.default.clone())).collect()
    }
}

/// 每個節點最多可用的參數槽位數（對應 uniform 中的 `array<vec4<f32>, MAX_PARAMS>`）。
pub const MAX_PARAMS: usize = 16;
/// 每個節點最多的輸入數。
pub const MAX_INPUTS: usize = 2;

const SURF_OUT: &[PortDef] = &[PortDef { name: "out", ty: DataType::Surface }];

pub const COMMON_WGSL: &str = r#"
fn pn_hash2(p: vec2<f32>, seed: f32) -> f32 {
    var q = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031 + seed * 0.0137);
    q = q + dot(q, q.yzx + 33.33);
    return fract((q.x + q.y) * q.z);
}
fn pn_luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.299, 0.587, 0.114));
}
fn pn_inside(p: vec2<i32>, size: vec2<u32>) -> bool {
    return all(p >= vec2<i32>(0)) && all(p < vec2<i32>(size));
}
fn pn_clamp_px(p: vec2<i32>, size: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(clamp(p, vec2<i32>(0), vec2<i32>(size) - vec2<i32>(1)));
}
fn pn_wrap_px(p: vec2<i32>, size: vec2<u32>) -> vec2<u32> {
    let s = vec2<i32>(size);
    return vec2<u32>(((p % s) + s) % s);
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
    REG.get_or_init(|| {
        let mut v = vec![solid(), noise(), color_adjust(), blend(), output()];
        v.extend(library::all());
        for d in &mut v {
            d.cost = match d.kind {
                "solid" | "output" | "checker" | "mirror" | "posterize" => 1,
                "noise" => 40,
                "blur" | "outline" => 25,
                "color_adjust" | "dither" | "hsv" => 4,
                "palette_map" => 10,
                "pixel_sort" => 60,
                _ => 3,
            };
        }
        v
    })
}

pub fn find(kind: &str) -> Option<&'static NodeDef> {
    registry().iter().find(|d| d.kind == kind)
}

fn f(name: &'static str, label: &'static str, min: f32, max: f32, v: f32) -> ParamDef {
    ParamDef { name, label, kind: ParamKind::Float { min, max }, default: Value::Float(v) }
}

fn int(name: &'static str, label: &'static str, min: i32, max: i32, v: i32) -> ParamDef {
    ParamDef { name, label, kind: ParamKind::Int { min, max }, default: Value::Int(v) }
}

fn color(name: &'static str, label: &'static str, c: [f32; 4]) -> ParamDef {
    ParamDef { name, label, kind: ParamKind::Color, default: Value::Color(c) }
}

fn choice(name: &'static str, label: &'static str, opts: &'static [&'static str], v: u32) -> ParamDef {
    ParamDef { name, label, kind: ParamKind::Enum(opts), default: Value::Enum(v) }
}

const SURF_IN: &[PortDef] = &[PortDef { name: "in", ty: DataType::Surface }];

fn solid() -> NodeDef {
    NodeDef {
        kind: "solid",
        category: Category::Generate,
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
        cost: 0,
        wgsl: "fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> { return P[0]; }",
    }
}

fn noise() -> NodeDef {
    NodeDef {
        kind: "noise",
        category: Category::Generate,
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
            f("offset_x", "位移 X", -64.0, 64.0, 0.0),
            f("offset_y", "位移 Y", -64.0, 64.0, 0.0),
            // 相位 0→1 可無縫循環（梯度 / 特徵點繞圈）。
            f("phase", "相位", 0.0, 1.0, 0.0),
        ],
        per_pixel: true,
        time_invariant: true,
        cost: 0,
        wgsl: r#"
fn noise_lattice(i: vec2<f32>, s: f32, ph: f32) -> f32 {
    return 0.5 + 0.5 * sin(6.2831853 * (pn_hash2(i, s) + ph));
}
fn noise_value(p: vec2<f32>, s: f32, ph: f32) -> f32 {
    let i = floor(p); let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = noise_lattice(i, s, ph); let b = noise_lattice(i + vec2<f32>(1.0, 0.0), s, ph);
    let c = noise_lattice(i + vec2<f32>(0.0, 1.0), s, ph); let d = noise_lattice(i + vec2<f32>(1.0, 1.0), s, ph);
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}
fn noise_grad(i: vec2<f32>, s: f32, ph: f32) -> vec2<f32> {
    let a = (pn_hash2(i, s) + ph) * 6.2831853;
    return vec2<f32>(cos(a), sin(a));
}
fn noise_perlin(p: vec2<f32>, s: f32, ph: f32) -> f32 {
    let i = floor(p); let f = fract(p);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = dot(noise_grad(i, s, ph), f);
    let b = dot(noise_grad(i + vec2<f32>(1.0, 0.0), s, ph), f - vec2<f32>(1.0, 0.0));
    let c = dot(noise_grad(i + vec2<f32>(0.0, 1.0), s, ph), f - vec2<f32>(0.0, 1.0));
    let d = dot(noise_grad(i + vec2<f32>(1.0, 1.0), s, ph), f - vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y) * 0.7071 + 0.5;
}
fn noise_cellular(p: vec2<f32>, s: f32, ph: f32) -> f32 {
    let i = floor(p); let f = fract(p);
    var md = 8.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let o = vec2<f32>(f32(x), f32(y));
            let a = 6.2831853 * (pn_hash2(i + o, s + 71.0) + ph);
            let pt = vec2<f32>(0.5) + 0.4 * vec2<f32>(cos(a), sin(a)) * pn_hash2(i + o, s);
            md = min(md, length(o + pt - f));
        }
    }
    return clamp(md, 0.0, 1.0);
}
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let kind = i32(P[0].x);
    let seed = P[2].x;
    let oct = i32(P[3].x);
    let ph = P[7].x;
    var p = uv * P[1].x + vec2<f32>(P[5].x, P[6].x);
    var amp = 0.5; var sum = 0.0; var norm = 0.0;
    for (var o = 0; o < oct; o++) {
        var n: f32;
        if (kind == 0) { n = noise_value(p, seed, ph); }
        else if (kind == 1) { n = noise_perlin(p, seed, ph); }
        else { n = noise_cellular(p, seed, ph); }
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
        category: Category::Color,
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
        cost: 0,
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
        category: Category::Composite,
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
        cost: 0,
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
        category: Category::Output,
        title: "Output",
        inputs: &[PortDef { name: "in", ty: DataType::Surface }],
        outputs: &[],
        params: vec![],
        per_pixel: true,
        time_invariant: true,
        cost: 0,
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
            for p in &d.params {
                let ok = matches!(
                    (p.kind, &p.default),
                    (ParamKind::Float { .. }, Value::Float(_))
                        | (ParamKind::Int { .. }, Value::Int(_))
                        | (ParamKind::Color, Value::Color(_))
                        | (ParamKind::Enum(_), Value::Enum(_))
                        | (ParamKind::Text { .. }, Value::Text(_))
                );
                assert!(ok, "{}.{} 預設值型別不符", d.kind, p.name);
            }
        }
        assert!(reg.len() >= 24);
        let mut kinds: Vec<_> = reg.iter().map(|d| d.kind).collect();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), reg.len());
    }
}

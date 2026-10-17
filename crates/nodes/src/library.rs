//! 第一批擴充節點：生成、變換、濾鏡、顏色、合成。

use crate::{Category, NodeDef, PortDef, SURF_IN, SURF_OUT, choice, color, f, int};
use pixelnode_graph::DataType;

const TRANSPARENT: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

pub(crate) fn all() -> Vec<NodeDef> {
    vec![
        gradient(),
        shape(),
        checker(),
        transform(),
        tile(),
        mirror(),
        blur(),
        outline(),
        dither(),
        posterize(),
        levels(),
        color_replace(),
        gradient_map(),
        palette_map(),
        hsv(),
        crop(),
        mask(),
        composite(),
        pixel_sort(),
        image(),
    ]
}

fn def(
    kind: &'static str,
    title: &'static str,
    category: Category,
    inputs: &'static [PortDef],
    params: Vec<crate::ParamDef>,
    per_pixel: bool,
    wgsl: &'static str,
) -> NodeDef {
    NodeDef { kind, title, category, inputs, outputs: SURF_OUT, params, per_pixel, time_invariant: true, cost: 0, wgsl, eval: None }
}

// ---------------------------------------------------------------- 生成

fn gradient() -> NodeDef {
    def(
        "gradient",
        "Gradient",
        Category::Generate,
        &[],
        vec![
            choice("type", "類型", &["Linear", "Radial", "Angular"], 0),
            color("a", "起點", BLACK),
            color("b", "終點", WHITE),
            f("angle", "角度", -180.0, 180.0, 0.0),
            int("steps", "色階", 0, 32, 0),
        ],
        true,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let p = uv - 0.5;
    let k = i32(P[0].x);
    let a = radians(P[3].x);
    var t: f32;
    if (k == 0) { t = dot(p, vec2<f32>(cos(a), sin(a))) + 0.5; }
    else if (k == 1) { t = length(p) * 2.0; }
    else { t = fract(atan2(p.y, p.x) / 6.2831853 + 0.5 - a / 6.2831853); }
    t = clamp(t, 0.0, 1.0);
    let steps = P[4].x;
    if (steps >= 2.0) { t = min(floor(t * steps), steps - 1.0) / (steps - 1.0); }
    return mix(P[1], P[2], t);
}
"#,
    )
}

fn shape() -> NodeDef {
    def(
        "shape",
        "Shape",
        Category::Generate,
        &[],
        vec![
            choice("type", "形狀", &["Circle", "Rectangle", "Diamond", "Ring"], 0),
            f("size_x", "寬", 0.0, 1.0, 0.5),
            f("size_y", "高", 0.0, 1.0, 0.5),
            f("offset_x", "位移 X", -256.0, 256.0, 0.0),
            f("offset_y", "位移 Y", -256.0, 256.0, 0.0),
            color("color", "顏色", WHITE),
            color("background", "背景", TRANSPARENT),
        ],
        true,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let s = vec2<f32>(SIZE);
    let c = s * 0.5 + vec2<f32>(P[3].x, P[4].x);
    let half = max(vec2<f32>(P[1].x, P[2].x) * s * 0.5, vec2<f32>(0.5));
    let d = (vec2<f32>(px) + 0.5 - c) / half;
    let k = i32(P[0].x);
    var inside: bool;
    if (k == 0) { inside = dot(d, d) <= 1.0; }
    else if (k == 1) { inside = all(abs(d) <= vec2<f32>(1.0)); }
    else if (k == 2) { inside = abs(d.x) + abs(d.y) <= 1.0; }
    else { let r = dot(d, d); inside = r <= 1.0 && r >= 0.45; }
    return select(P[6], P[5], inside);
}
"#,
    )
}

fn checker() -> NodeDef {
    def(
        "checker",
        "Checker",
        Category::Generate,
        &[],
        vec![int("size", "格子大小", 1, 128, 8), color("a", "顏色 A", WHITE), color("b", "顏色 B", [0.6, 0.6, 0.6, 1.0])],
        true,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let c = px / max(u32(P[0].x), 1u);
    return select(P[2], P[1], ((c.x + c.y) & 1u) == 0u);
}
"#,
    )
}

// ---------------------------------------------------------------- 變換

const WRAP_MODES: &[&str] = &["透明", "延伸邊緣", "重複"];

fn transform() -> NodeDef {
    def(
        "transform",
        "Transform",
        Category::Transform,
        SURF_IN,
        vec![
            f("offset_x", "位移 X", -256.0, 256.0, 0.0),
            f("offset_y", "位移 Y", -256.0, 256.0, 0.0),
            f("scale", "縮放", 0.25, 8.0, 1.0),
            f("rotation", "旋轉", -180.0, 180.0, 0.0),
            choice("wrap", "邊界", WRAP_MODES, 0),
        ],
        true,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let c = vec2<f32>(SIZE) * 0.5;
    let p = vec2<f32>(px) + 0.5 - c - vec2<f32>(P[0].x, P[1].x);
    let a = -radians(P[3].x);
    let r = vec2<f32>(p.x * cos(a) - p.y * sin(a), p.x * sin(a) + p.y * cos(a)) / max(P[2].x, 0.001);
    let q = vec2<i32>(floor(r + c));
    let m = i32(P[4].x);
    if (m == 0) {
        if (!pn_inside(q, SIZE)) { return vec4<f32>(0.0); }
        return in_at(0u, vec2<u32>(q));
    }
    if (m == 1) { return in_at(0u, pn_clamp_px(q, SIZE)); }
    return in_at(0u, pn_wrap_px(q, SIZE));
}
"#,
    )
}

fn tile() -> NodeDef {
    def(
        "tile",
        "Tile",
        Category::Transform,
        SURF_IN,
        vec![
            int("count_x", "橫向數", 1, 16, 2),
            int("count_y", "縱向數", 1, 16, 2),
            f("shift", "交錯", 0.0, 1.0, 0.0),
        ],
        true,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let n = vec2<u32>(u32(max(P[0].x, 1.0)), u32(max(P[1].x, 1.0)));
    let cell = max(SIZE / n, vec2<u32>(1u));
    let row = px.y / cell.y;
    let shift = u32(P[2].x * f32(cell.x)) * (row & 1u);
    let local = vec2<u32>((px.x + shift) % cell.x, px.y % cell.y);
    // 每格縮小成整張圖（最近鄰、整數比例）。
    return in_at(0u, min(local * SIZE / cell, SIZE - vec2<u32>(1u)));
}
"#,
    )
}

fn mirror() -> NodeDef {
    def(
        "mirror",
        "Mirror",
        Category::Transform,
        SURF_IN,
        vec![choice("mode", "模式", &["水平翻轉", "垂直翻轉", "左→右鏡射", "上→下鏡射", "四向鏡射"], 2)],
        true,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let m = i32(P[0].x);
    let last = SIZE - vec2<u32>(1u);
    var q = px;
    if (m == 0) { q.x = last.x - px.x; }
    else if (m == 1) { q.y = last.y - px.y; }
    else {
        if ((m == 2 || m == 4) && px.x >= SIZE.x / 2u) { q.x = last.x - px.x; }
        if ((m == 3 || m == 4) && px.y >= SIZE.y / 2u) { q.y = last.y - px.y; }
    }
    return in_at(0u, q);
}
"#,
    )
}

// ---------------------------------------------------------------- 濾鏡

fn blur() -> NodeDef {
    def(
        "blur",
        "Blur",
        Category::Filter,
        SURF_IN,
        vec![int("radius", "半徑", 1, 8, 1)],
        false,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let r = i32(P[0].x);
    var acc = vec4<f32>(0.0);
    for (var y = -r; y <= r; y++) {
        for (var x = -r; x <= r; x++) {
            acc += in_at(0u, pn_clamp_px(vec2<i32>(px) + vec2<i32>(x, y), SIZE));
        }
    }
    let n = f32((2 * r + 1) * (2 * r + 1));
    return acc / n;
}
"#,
    )
}

fn outline() -> NodeDef {
    def(
        "outline",
        "Outline",
        Category::Filter,
        SURF_IN,
        vec![
            color("color", "顏色", BLACK),
            int("thickness", "粗細", 1, 4, 1),
            choice("side", "位置", &["外側", "內側"], 0),
            f("threshold", "Alpha 門檻", 0.01, 1.0, 0.5),
            choice("corners", "角落", &["四方向", "含斜角"], 0),
        ],
        false,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let src = in_at(0u, px);
    let t = i32(P[1].x);
    let inside = i32(P[2].x) == 1;
    let th = P[3].x;
    let diag = i32(P[4].x) == 1;
    let me = src.a >= th;
    var hit = false;
    for (var y = -t; y <= t; y++) {
        for (var x = -t; x <= t; x++) {
            if ((x == 0 && y == 0) || (!diag && abs(x) + abs(y) > t)) { continue; }
            let q = vec2<i32>(px) + vec2<i32>(x, y);
            var o = false;
            if (pn_inside(q, SIZE)) { o = in_at(0u, vec2<u32>(q)).a >= th; }
            if ((!inside && !me && o) || (inside && me && !o)) { hit = true; }
        }
    }
    if (hit) { return P[0]; }
    return src;
}
"#,
    )
}

fn dither() -> NodeDef {
    def(
        "dither",
        "Dither",
        Category::Filter,
        SURF_IN,
        vec![
            int("levels", "色階", 2, 16, 4),
            choice("pattern", "矩陣", &["Bayer 2×2", "Bayer 4×4", "Bayer 8×8"], 1),
            f("strength", "強度", 0.0, 1.0, 1.0),
        ],
        true,
        r#"
fn bayer(p: vec2<u32>, bits: u32) -> f32 {
    var v = 0u;
    for (var i = 0u; i < bits; i++) {
        let xb = (p.x >> i) & 1u;
        let yb = (p.y >> i) & 1u;
        v = v * 4u + 2u * (xb ^ yb) + yb;
    }
    return (f32(v) + 0.5) / f32(1u << (2u * bits));
}
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let src = in_at(0u, px);
    let l = P[0].x - 1.0;
    let b = (bayer(px, u32(P[1].x) + 1u) - 0.5) * P[2].x;
    let c = clamp(floor(src.rgb * l + 0.5 + b), vec3<f32>(0.0), vec3<f32>(l)) / l;
    return vec4<f32>(c, src.a);
}
"#,
    )
}

fn posterize() -> NodeDef {
    def(
        "posterize",
        "Posterize",
        Category::Filter,
        SURF_IN,
        vec![int("levels", "色階", 2, 32, 4)],
        true,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let src = in_at(0u, px);
    let l = P[0].x - 1.0;
    return vec4<f32>(round(src.rgb * l) / l, src.a);
}
"#,
    )
}

// ---------------------------------------------------------------- 顏色

fn levels() -> NodeDef {
    def(
        "levels",
        "Levels",
        Category::Color,
        SURF_IN,
        vec![
            f("in_black", "輸入黑", 0.0, 1.0, 0.0),
            f("in_white", "輸入白", 0.0, 1.0, 1.0),
            f("gamma", "Gamma", 0.1, 4.0, 1.0),
            f("out_black", "輸出黑", 0.0, 1.0, 0.0),
            f("out_white", "輸出白", 0.0, 1.0, 1.0),
        ],
        true,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let src = in_at(0u, px);
    let t = clamp((src.rgb - P[0].x) / max(P[1].x - P[0].x, 1.0e-4), vec3<f32>(0.0), vec3<f32>(1.0));
    let g = pow(t, vec3<f32>(1.0 / max(P[2].x, 1.0e-3)));
    return vec4<f32>(mix(vec3<f32>(P[3].x), vec3<f32>(P[4].x), g), src.a);
}
"#,
    )
}

fn color_replace() -> NodeDef {
    def(
        "color_replace",
        "Color Replace",
        Category::Color,
        SURF_IN,
        vec![
            color("from", "原色", WHITE),
            color("to", "替換為", [1.0, 0.0, 0.0, 1.0]),
            f("tolerance", "容差", 0.0, 1.0, 0.1),
        ],
        true,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let src = in_at(0u, px);
    if (distance(src.rgb, P[0].rgb) <= P[2].x * 1.7320508) {
        return vec4<f32>(P[1].rgb, src.a * P[1].a);
    }
    return src;
}
"#,
    )
}

fn gradient_map() -> NodeDef {
    def(
        "gradient_map",
        "Gradient Map",
        Category::Color,
        SURF_IN,
        vec![
            int("count", "顏色數", 2, 4, 4),
            int("steps", "色階", 0, 32, 0),
            color("c0", "暗部", [0.08, 0.06, 0.15, 1.0]),
            color("c1", "中暗", [0.35, 0.2, 0.4, 1.0]),
            color("c2", "中亮", [0.85, 0.45, 0.35, 1.0]),
            color("c3", "亮部", [1.0, 0.9, 0.6, 1.0]),
        ],
        true,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let src = in_at(0u, px);
    var t = clamp(pn_luma(src.rgb), 0.0, 1.0);
    let steps = P[1].x;
    if (steps >= 2.0) { t = min(floor(t * steps), steps - 1.0) / (steps - 1.0); }
    let segs = clamp(P[0].x, 2.0, 4.0) - 1.0;
    let x = t * segs;
    let i = min(u32(floor(x)), u32(segs) - 1u);
    let local = x - f32(i);
    var lo: vec4<f32>; var hi: vec4<f32>;
    if (i == 0u) { lo = P[2]; hi = P[3]; }
    else if (i == 1u) { lo = P[3]; hi = P[4]; }
    else { lo = P[4]; hi = P[5]; }
    // 只有兩色時直接用 c0 → c3。
    if (segs < 1.5) { lo = P[2]; hi = P[5]; }
    let c = mix(lo, hi, local);
    return vec4<f32>(c.rgb, src.a * c.a);
}
"#,
    )
}

pub const PALETTE_PRESETS: &[&str] = &["自訂", "Game Boy", "PICO-8", "Sweetie 16", "CGA", "1-bit"];

fn palette_map() -> NodeDef {
    let mut params = vec![
        choice("preset", "調色盤", PALETTE_PRESETS, 1),
        int("count", "自訂色數", 2, 11, 4),
        choice("mode", "對應方式", &["最接近顏色", "依亮度"], 0),
        choice("dither", "Dither", &["無", "Bayer 4×4"], 0),
        f("dither_amount", "Dither 強度", 0.0, 1.0, 0.5),
    ];
    let defaults = [
        [0.10, 0.11, 0.17, 1.0],
        [0.36, 0.15, 0.36, 1.0],
        [0.69, 0.24, 0.33, 1.0],
        [0.94, 0.49, 0.34, 1.0],
        [1.0, 0.80, 0.46, 1.0],
        [0.65, 0.94, 0.44, 1.0],
        [0.22, 0.72, 0.39, 1.0],
        [0.15, 0.44, 0.47, 1.0],
        [0.16, 0.21, 0.44, 1.0],
        [0.23, 0.36, 0.79, 1.0],
        [0.96, 0.96, 0.96, 1.0],
    ];
    const NAMES: [&str; 11] = ["c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10"];
    const LABELS: [&str; 11] =
        ["顏色 1", "顏色 2", "顏色 3", "顏色 4", "顏色 5", "顏色 6", "顏色 7", "顏色 8", "顏色 9", "顏色 10", "顏色 11"];
    for i in 0..11 {
        params.push(color(NAMES[i], LABELS[i], defaults[i]));
    }
    def(
        "palette_map",
        "Palette Map",
        Category::Color,
        SURF_IN,
        params,
        true,
        concat!(
            include_str!("palette.wgsl"),
            r#"
fn pal_len() -> u32 {
    let p = i32(P[0].x);
    if (p == 0) { return u32(clamp(P[1].x, 2.0, 11.0)); }
    return preset_len(p);
}
fn pal_at(i: u32) -> vec3<f32> {
    let p = i32(P[0].x);
    if (p == 0) { return P[5u + min(i, 10u)].rgb; }
    return preset_color(p, i);
}
fn bayer4(p: vec2<u32>) -> f32 {
    var v = 0u;
    for (var i = 0u; i < 2u; i++) {
        let xb = (p.x >> i) & 1u;
        let yb = (p.y >> i) & 1u;
        v = v * 4u + 2u * (xb ^ yb) + yb;
    }
    return (f32(v) + 0.5) / 16.0 - 0.5;
}
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let src = in_at(0u, px);
    let n = pal_len();
    var c = src.rgb;
    if (i32(P[3].x) == 1) {
        c = c + bayer4(px) * P[4].x / f32(n - 1u);
    }
    var best = 0u;
    if (i32(P[2].x) == 1) {
        best = min(u32(floor(clamp(pn_luma(c), 0.0, 1.0) * f32(n))), n - 1u);
    } else {
        var bd = 1.0e9;
        for (var i = 0u; i < n; i++) {
            let d = distance(c, pal_at(i));
            if (d < bd) { bd = d; best = i; }
        }
    }
    return vec4<f32>(pal_at(best), src.a);
}
"#
        ),
    )
}

fn hsv() -> NodeDef {
    def(
        "hsv",
        "HSV",
        Category::Color,
        SURF_IN,
        vec![
            f("hue", "色相偏移", -0.5, 0.5, 0.0),
            f("saturation", "飽和度", 0.0, 2.0, 1.0),
            f("value", "明度", 0.0, 2.0, 1.0),
        ],
        true,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let src = in_at(0u, px);
    var h = pn_rgb2hsv(clamp(src.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    h.x = fract(h.x + P[0].x);
    h.y = clamp(h.y * P[1].x, 0.0, 1.0);
    h.z = clamp(h.z * P[2].x, 0.0, 1.0);
    return vec4<f32>(pn_hsv2rgb(h), src.a);
}
"#,
    )
}

fn crop() -> NodeDef {
    def(
        "crop",
        "Crop",
        Category::Transform,
        SURF_IN,
        vec![
            int("x", "X", 0, 2048, 0),
            int("y", "Y", 0, 2048, 0),
            int("width", "寬", 1, 2048, 32),
            int("height", "高", 1, 2048, 32),
            choice("place", "放置", &["保留原位", "移到左上"], 0),
        ],
        true,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let o = vec2<u32>(u32(P[0].x), u32(P[1].x));
    let s = vec2<u32>(u32(P[2].x), u32(P[3].x));
    if (i32(P[4].x) == 1) {
        if (any(px >= s)) { return vec4<f32>(0.0); }
        let q = px + o;
        if (any(q >= SIZE)) { return vec4<f32>(0.0); }
        return in_at(0u, q);
    }
    if (any(px < o) || any(px >= o + s)) { return vec4<f32>(0.0); }
    return in_at(0u, px);
}
"#,
    )
}

// ---------------------------------------------------------------- 合成

fn mask() -> NodeDef {
    def(
        "mask",
        "Mask",
        Category::Composite,
        &[PortDef { name: "in", ty: DataType::Surface }, PortDef { name: "mask", ty: DataType::Surface }],
        vec![choice("source", "遮罩來源", &["亮度", "Alpha"], 0), choice("invert", "反轉", &["否", "是"], 0)],
        true,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let src = in_at(0u, px);
    let m = in_at(1u, px);
    var v = select(pn_luma(m.rgb) * m.a, m.a, i32(P[0].x) == 1);
    if (i32(P[1].x) == 1) { v = 1.0 - v; }
    return vec4<f32>(src.rgb, src.a * v);
}
"#,
    )
}

fn composite() -> NodeDef {
    def(
        "composite",
        "Composite",
        Category::Composite,
        &[PortDef { name: "bg", ty: DataType::Surface }, PortDef { name: "fg", ty: DataType::Surface }],
        vec![
            int("x", "X", -2048, 2048, 0),
            int("y", "Y", -2048, 2048, 0),
            int("scale", "整數倍率", 1, 16, 1),
            f("opacity", "不透明度", 0.0, 1.0, 1.0),
        ],
        true,
        r#"
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let bg = in_at(0u, px);
    let s = max(i32(P[2].x), 1);
    let q = (vec2<i32>(px) - vec2<i32>(i32(P[0].x), i32(P[1].x)));
    if (any(q < vec2<i32>(0))) { return bg; }
    let src = q / s;
    if (!pn_inside(src, SIZE)) { return bg; }
    let fg = in_at(1u, vec2<u32>(src));
    let a = fg.a * P[3].x;
    return vec4<f32>(mix(bg.rgb, fg.rgb, a), a + bg.a * (1.0 - a));
}
"#,
    )
}

fn pixel_sort() -> NodeDef {
    def(
        "pixel_sort",
        "Pixel Sort",
        Category::Filter,
        SURF_IN,
        vec![
            choice("direction", "方向", &["橫向", "縱向"], 0),
            choice("order", "順序", &["暗→亮", "亮→暗"], 0),
            f("low", "亮度下限", 0.0, 1.0, 0.25),
            f("high", "亮度上限", 0.0, 1.0, 1.0),
        ],
        false,
        r#"
fn key(c: vec4<f32>) -> u32 {
    return u32(clamp(pn_luma(c.rgb), 0.0, 1.0) * 255.0 + 0.5);
}
fn sel(c: vec4<f32>) -> bool {
    let l = pn_luma(c.rgb);
    return l >= P[2].x && l <= P[3].x;
}
fn at(line: u32, i: u32) -> vec4<f32> {
    if (i32(P[0].x) == 1) { return in_at(0u, vec2<u32>(line, i)); }
    return in_at(0u, vec2<u32>(i, line));
}
// 只排序亮度落在 [下限, 上限] 的連續區段。以 256 級亮度做穩定的計數排序：
// 每個像素 O(區段長 + 256)，不需要跨執行緒同步。
fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {
    let vertical = i32(P[0].x) == 1;
    let line = select(px.y, px.x, vertical);
    let pos = select(px.x, px.y, vertical);
    let len = select(SIZE.x, SIZE.y, vertical);
    let me = at(line, pos);
    if (!sel(me)) { return me; }
    var s = pos;
    while (s > 0u && sel(at(line, s - 1u))) { s -= 1u; }
    var e = pos + 1u;
    while (e < len && sel(at(line, e))) { e += 1u; }
    var hist: array<u32, 256>;
    for (var i = s; i < e; i++) { hist[key(at(line, i))] += 1u; }
    var k = pos - s;
    if (i32(P[1].x) == 1) { k = e - 1u - pos; }
    var bucket = 0u;
    var before = 0u;
    for (var b = 0u; b < 256u; b++) {
        if (before + hist[b] > k) { bucket = b; break; }
        before += hist[b];
    }
    var nth = k - before;
    for (var i = s; i < e; i++) {
        let c = at(line, i);
        if (key(c) == bucket) {
            if (nth == 0u) { return c; }
            nth -= 1u;
        }
    }
    return me;
}
"#,
    )
}

fn image() -> NodeDef {
    def(
        "image",
        "Image In",
        Category::Generate,
        &[],
        vec![
            crate::ParamDef {
                name: "path",
                label: "PNG 路徑",
                kind: crate::ParamKind::Text { multiline: false },
                default: pixelnode_graph::Value::Text(String::new()),
            },
            choice("fit", "貼合", &["原尺寸置中", "整數縮放置中", "拉伸", "左上原尺寸"], 1),
        ],
        // 由 runtime 直接上傳 texture，不跑 shader；永遠是融合邊界。
        false,
        "fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(0.0); }",
    )
}

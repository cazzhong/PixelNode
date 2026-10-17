//! 圖編譯：型別檢查 → pull-based 拓撲排序（含循環偵測）→ 節點融合 → WGSL 生成。
//!
//! 融合規則：一個逐像素節點若只有唯一一個消費者、且消費者也是逐像素節點，
//! 就併入消費者的 kernel（不落地成 texture）。被多個節點共用、但子樹夠便宜的
//! 節點，會複製進每個消費者重算（[`INLINE_COST_BUDGET`]）。其餘節點（Output、
//! 昂貴的共用節點、非逐像素）是融合邊界，各自是一個 kernel 的 root。
//!
//! 參數一律走 storage buffer，WGSL 只依「子圖結構」生成，[`Kernel::shader_key`]
//! 相同就共用已編譯的 pipeline，改參數不重編。

use pixelnode_graph::{Graph, NodeId};
use pixelnode_nodes::{COMMON_WGSL, MAX_INPUTS, MAX_PARAMS, NodeDef};

use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

mod schedule;

/// 單一 kernel 最多可讀取的輸入 texture 數。
pub const MAX_KERNEL_TEXTURES: usize = 8;
/// 被多個節點共用的子樹，若成本不超過此值就在每個消費者內重算，
/// 省下一次 texture 落地與 dispatch。
pub const INLINE_COST_BUDGET: u32 = 12;
/// 重算最多複製到幾個消費者。
pub const INLINE_MAX_USES: usize = 4;

#[derive(Debug, Clone, PartialEq)]
pub enum CompileError {
    UnknownKind(String),
    Cycle(NodeId),
    TypeMismatch { to: NodeId, port: usize },
    BadPort { node: NodeId, port: usize },
    NoOutput,
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::UnknownKind(k) => write!(f, "未知節點類型：{k}"),
            CompileError::Cycle(n) => write!(f, "偵測到循環（節點 {}）", n.0),
            CompileError::TypeMismatch { to, port } => write!(f, "型別不符：節點 {} 輸入 {port}", to.0),
            CompileError::BadPort { node, port } => write!(f, "無效的 port：節點 {} #{port}", node.0),
            CompileError::NoOutput => write!(f, "圖中沒有 Output 節點"),
        }
    }
}

impl std::error::Error for CompileError {}

#[derive(Debug, Clone, Copy)]
pub struct CompileOptions {
    /// false 時每個節點各自一個 kernel（用於 benchmark 對照與除錯）。
    pub fuse: bool,
    /// 取樣 keyframe 軌道的影格。
    pub frame: u32,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self { fuse: true, frame: 0 }
    }
}

#[derive(Debug, Clone)]
pub struct Step {
    pub node: NodeId,
    pub def: &'static NodeDef,
    /// 每個輸入對應的來源 step 索引（未連接為 None）。
    pub inputs: [Option<usize>; MAX_INPUTS],
    pub params: [[f32; 4]; MAX_PARAMS],
    /// hash(種類 + 參數 + 輸入 hash + 尺寸)，作為輸出快取 key。
    pub hash: u64,
    /// 自身或上游有 keyframe / 與時間相關 → 輸出會隨影格變化。
    pub animated: bool,
    /// 第一個文字參數（例如圖片路徑、自訂程式碼）。
    pub text: Option<String>,
}

/// kernel 內某成員的某個輸入從哪裡來。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputSrc {
    None,
    /// 同一 kernel 內的成員（索引為 `members` 位置）。
    Fused(usize),
    /// 讀取 texture 槽位（`textures` 位置）。
    Texture(usize),
}

#[derive(Debug, Clone)]
pub struct Member {
    pub step: usize,
    pub inputs: [InputSrc; MAX_INPUTS],
    /// 在 kernel 參數區塊中的起始 vec4 位置。
    pub param_offset: usize,
}

#[derive(Debug, Clone)]
pub struct Kernel {
    /// 拓撲順序，最後一個是 root。
    pub members: Vec<Member>,
    /// 輸入 texture 來自哪些先前的 kernel（`Plan::kernels` 索引）。
    pub textures: Vec<usize>,
    pub params: Vec<[f32; 4]>,
    /// 子圖結構 hash（不含參數值）→ shader / pipeline 快取 key。
    pub shader_key: u64,
    /// root 輸出的內容 hash → 輸出快取 key。
    pub hash: u64,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub canvas: [u32; 2],
    pub steps: Vec<Step>,
    /// 依相依順序；最後一個輸出即 Output。
    pub kernels: Vec<Kernel>,
}

impl Plan {
    pub fn output_kernel(&self) -> &Kernel {
        self.kernels.last().expect("plan always has an output kernel")
    }

    /// 生成 kernel 的 WGSL。只依結構決定，相同 `shader_key` 產生相同原始碼。
    pub fn kernel_wgsl(&self, k: usize) -> String {
        let kernel = &self.kernels[k];
        let mut src = String::new();
        src.push_str(
            "struct Info { size: vec4<u32> };\n\
             struct Params { p: array<vec4<f32>> };\n\
             @group(0) @binding(0) var<uniform> U: Info;\n\
             @group(0) @binding(1) var<storage, read> PB: Params;\n",
        );
        for t in 0..MAX_KERNEL_TEXTURES {
            let _ = writeln!(src, "@group(0) @binding({}) var t{t}: texture_2d<f32>;", t + 2);
        }
        let _ = writeln!(
            src,
            "@group(0) @binding({}) var out_tex: texture_storage_2d<rgba8unorm, write>;",
            MAX_KERNEL_TEXTURES + 2
        );
        src.push_str(
            "fn pn_load(t: texture_2d<f32>, px: vec2<u32>) -> vec4<f32> {\n\
             \x20   let d = textureDimensions(t);\n\
             \x20   return textureLoad(t, min(px, d - vec2<u32>(1u)), 0);\n}\n\
             fn pn_uv(px: vec2<u32>) -> vec2<f32> { return (vec2<f32>(px) + 0.5) / vec2<f32>(U.size.xy); }\n",
        );
        src.push_str(COMMON_WGSL);

        for (m, member) in kernel.members.iter().enumerate() {
            let def = self.steps[member.step].def;
            let prefix = format!("m{m}_");
            let _ = writeln!(src, "// ---- m{m}: {} ----", def.kind);
            let _ = writeln!(src, "fn {prefix}in(k: u32, px: vec2<u32>) -> vec4<f32> {{");
            for (k, input) in member.inputs.iter().enumerate().take(def.inputs.len()) {
                let expr = match input {
                    InputSrc::None => "vec4<f32>(0.0)".to_string(),
                    InputSrc::Fused(j) => format!("m{j}_node_fn(px, pn_uv(px))"),
                    InputSrc::Texture(t) => format!("pn_load(t{t}, px)"),
                };
                let _ = writeln!(src, "    if (k == {k}u) {{ return {expr}; }}");
            }
            src.push_str("    return vec4<f32>(0.0);\n}\n");
            let step = &self.steps[member.step];
            let body = if def.kind == "custom" {
                custom_wgsl(step.text.as_deref().unwrap_or("return vec4<f32>(0.0);"))
            } else {
                def.wgsl.to_string()
            };
            src.push_str(&instantiate(&body, &prefix, member.param_offset));
            src.push('\n');
        }

        let root = kernel.members.len() - 1;
        let _ = write!(
            src,
            "@compute @workgroup_size(8, 8)\n\
             fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{\n\
             \x20   if (gid.x >= U.size.x || gid.y >= U.size.y) {{ return; }}\n\
             \x20   let c = m{root}_node_fn(gid.xy, pn_uv(gid.xy));\n\
             \x20   textureStore(out_tex, gid.xy, clamp(c, vec4<f32>(0.0), vec4<f32>(1.0)));\n}}\n"
        );
        src
    }
}

/// 自訂 WGSL 節點：使用者只寫函式內容，這裡補上可用的變數。
pub fn custom_wgsl(user: &str) -> String {
    format!(
        "fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32> {{\n\
         \x20   let a = in_at(0u, px);\n    let b = in_at(1u, px);\n\
         \x20   let p1 = P[0].x; let p2 = P[1].x; let p3 = P[2].x; let p4 = P[3].x;\n\
         \x20   let c1 = P[4]; let c2 = P[5];\n\
         \x20   let frame = f32(FRAME); let size = vec2<f32>(SIZE);\n\
         {user}\n}}\n"
    )
}

/// 把節點片段實例化：頂層函式加前綴、參數改讀 storage buffer 的對應位置。
fn instantiate(wgsl: &str, prefix: &str, param_offset: usize) -> String {
    let fns: Vec<&str> = wgsl
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("fn "))
        .filter_map(|rest| rest.split(|c: char| !is_ident(c)).next())
        .collect();
    let mut out = String::with_capacity(wgsl.len() + 64);
    let bytes = wgsl.as_bytes();
    let mut i = 0;
    while i < wgsl.len() {
        let c = wgsl[i..].chars().next().unwrap();
        if is_ident(c) && (i == 0 || !is_ident(bytes[i - 1] as char)) {
            let end = wgsl[i..].find(|c: char| !is_ident(c)).map_or(wgsl.len(), |e| i + e);
            let word = &wgsl[i..end];
            match word {
                "P" if wgsl[end..].starts_with('[') => {
                    let _ = write!(out, "PB.p[{param_offset}u + ");
                    i = end + 1;
                    continue;
                }
                "in_at" => out.push_str(&format!("{prefix}in")),
                "SIZE" => out.push_str("U.size.xy"),
                "FRAME" => out.push_str("U.size.z"),
                w if fns.contains(&w) => {
                    out.push_str(prefix);
                    out.push_str(w);
                }
                w => out.push_str(w),
            }
            i = end;
        } else {
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

pub fn compile(graph: &Graph) -> Result<Plan, CompileError> {
    compile_with(graph, CompileOptions::default())
}

pub fn compile_with(graph: &Graph, opts: CompileOptions) -> Result<Plan, CompileError> {
    let defs: HashMap<&'static str, &'static NodeDef> =
        pixelnode_nodes::registry().iter().map(|d| (d.kind, d)).collect();
    let def_of = |id: NodeId| -> Result<&'static NodeDef, CompileError> {
        let n = graph.node(id).ok_or(CompileError::NoOutput)?;
        defs.get(n.kind.as_str()).copied().ok_or_else(|| CompileError::UnknownKind(n.kind.clone()))
    };

    for l in &graph.links {
        let from = def_of(l.from_node)?;
        let to = def_of(l.to_node)?;
        let out = from.outputs.get(l.from_port).ok_or(CompileError::BadPort { node: l.from_node, port: l.from_port })?;
        let (_, in_ty) = to.input_port(l.to_port).ok_or(CompileError::BadPort { node: l.to_node, port: l.to_port })?;
        if !in_ty.accepts(out.ty) {
            return Err(CompileError::TypeMismatch { to: l.to_node, port: l.to_port });
        }
    }

    let out_id = graph.nodes.iter().find(|n| n.kind == "output").map(|n| n.id).ok_or(CompileError::NoOutput)?;
    let mut sched = schedule::Scheduler::new(graph, &defs, opts.frame);
    sched.visit(out_id)?;
    let steps = sched.steps;
    let kernels = fuse(&steps, opts);
    Ok(Plan { canvas: graph.canvas, steps, kernels })
}

fn fuse(steps: &[Step], opts: CompileOptions) -> Vec<Kernel> {
    let n = steps.len();
    let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, s) in steps.iter().enumerate() {
        for src in s.inputs.iter().flatten() {
            consumers[*src].push(i);
        }
    }
    // 拓撲順序：輸入一定在前，所以計算 i 時其上游的 root / 成本都已確定。
    let mut root = vec![true; n];
    let mut subtree_cost = vec![0u32; n];
    for i in 0..n {
        let own: u32 = steps[i].def.cost
            + steps[i].inputs.iter().flatten().map(|&s| if root[s] { 1 } else { subtree_cost[s] }).sum::<u32>();
        subtree_cost[i] = own;
        let uses = consumers[i].len();
        let fusable = opts.fuse
            && i != n - 1
            && steps[i].def.per_pixel
            && consumers[i].iter().all(|&c| steps[c].def.per_pixel);
        // 靜態子樹餵給會動的節點：若不便宜就落地，讓它跨影格留在快取裡。
        let cache_boundary =
            !steps[i].animated && own > INLINE_COST_BUDGET && consumers[i].iter().any(|&c| steps[c].animated);
        root[i] = cache_boundary
            || !(fusable && (uses == 1 || (uses <= INLINE_MAX_USES && own <= INLINE_COST_BUDGET)));
    }

    loop {
        match build_kernels(steps, &root) {
            Ok(k) => return k,
            // texture 槽位不夠：把該 kernel 最大的一個融合分支升為 root 後重試。
            Err(split) => root[split] = true,
        }
    }
}

/// 失敗時回傳要拆開的成員（root 的直接融合輸入中，子樹最大的那個）。
fn build_kernels(steps: &[Step], root: &[bool]) -> Result<Vec<Kernel>, usize> {
    let mut kernel_of = vec![usize::MAX; steps.len()];
    let mut kernels = Vec::new();

    for r in (0..steps.len()).filter(|&i| root[i]) {
        // 收集 r 的融合子樹（後序 = 拓撲順序）。
        // 重算的共用節點在同一 kernel 內可能經由兩條路徑到達，只收一次。
        fn collect(i: usize, steps: &[Step], root: &[bool], top: usize, out: &mut Vec<usize>) {
            for src in steps[i].inputs.iter().flatten() {
                // 子樹很小（通常 < 20），線性查找比雜湊快。
                if !root[*src] && *src != top && !out.contains(src) {
                    collect(*src, steps, root, top, out);
                }
            }
            out.push(i);
        }
        let mut order = Vec::new();
        collect(r, steps, root, r, &mut order);

        let pos: HashMap<usize, usize> = order.iter().enumerate().map(|(m, &s)| (s, m)).collect();
        let mut textures: Vec<usize> = Vec::new();
        let mut members = Vec::with_capacity(order.len());
        let mut params = Vec::new();
        let mut key = std::collections::hash_map::DefaultHasher::new();

        for &s in &order {
            let step = &steps[s];
            let mut inputs = [InputSrc::None; MAX_INPUTS];
            for (slot, src) in inputs.iter_mut().zip(step.inputs) {
                if let Some(src) = src {
                    *slot = if root[src] {
                        let k = kernel_of[src];
                        let t = textures.iter().position(|&x| x == k).unwrap_or_else(|| {
                            textures.push(k);
                            textures.len() - 1
                        });
                        InputSrc::Texture(t)
                    } else {
                        InputSrc::Fused(pos[&src])
                    };
                }
            }
            let param_offset = params.len();
            params.extend_from_slice(&step.params[..step.def.params.len()]);
            step.def.kind.hash(&mut key);
            if step.def.kind == "custom" {
                step.text.hash(&mut key);
            }
            inputs.hash(&mut key);
            members.push(Member { step: s, inputs, param_offset });
        }

        if textures.len() > MAX_KERNEL_TEXTURES {
            let subtree = |s: usize| {
                let mut v = Vec::new();
                collect(s, steps, root, r, &mut v);
                v.len()
            };
            let split = steps[r]
                .inputs
                .iter()
                .flatten()
                .copied()
                .filter(|&s| !root[s])
                .max_by_key(|&s| subtree(s))
                .expect("over-limit kernel must have a fused input");
            return Err(split);
        }
        textures.len().hash(&mut key);

        kernel_of[r] = kernels.len();
        kernels.push(Kernel { members, textures, params, shader_key: key.finish(), hash: steps[r].hash });
    }
    Ok(kernels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixelnode_graph::{Link, Node, Value};

    fn node(id: u64, kind: &str) -> Node {
        Node { id: NodeId(id), kind: kind.into(), params: Default::default(), pos: [0.0; 2], anim: Default::default() }
    }
    fn link(a: u64, b: u64, port: usize) -> Link {
        Link { from_node: NodeId(a), from_port: 0, to_node: NodeId(b), to_port: port }
    }
    fn chain() -> Graph {
        Graph {
            nodes: vec![node(1, "noise"), node(2, "color_adjust"), node(3, "output"), node(4, "solid")],
            links: vec![link(1, 2, 0), link(2, 3, 0)],
            ..Default::default()
        }
    }

    #[test]
    fn orders_dependencies_and_prunes_unreachable() {
        let plan = compile(&chain()).unwrap();
        let order: Vec<_> = plan.steps.iter().map(|s| s.node.0).collect();
        assert_eq!(order, vec![1, 2, 3]);
        assert_eq!(plan.steps[2].inputs[0], Some(1));
    }

    #[test]
    fn detects_cycle() {
        let g = Graph {
            nodes: vec![node(1, "color_adjust"), node(2, "color_adjust"), node(3, "output")],
            links: vec![link(1, 2, 0), link(2, 1, 0), link(2, 3, 0)],
            ..Default::default()
        };
        assert!(matches!(compile(&g), Err(CompileError::Cycle(_))));
    }

    #[test]
    fn hash_changes_with_upstream_param() {
        let mut g = chain();
        let h1 = compile(&g).unwrap().output_kernel().hash;
        g.nodes[0].params.insert("scale".into(), Value::Float(3.0));
        assert_ne!(h1, compile(&g).unwrap().output_kernel().hash);
    }

    #[test]
    fn missing_output() {
        let g = Graph { nodes: vec![node(1, "noise")], ..Default::default() };
        assert_eq!(compile(&g).unwrap_err(), CompileError::NoOutput);
    }

    #[test]
    fn chain_fuses_into_one_kernel() {
        let plan = compile(&chain()).unwrap();
        assert_eq!(plan.kernels.len(), 1);
        assert_eq!(plan.kernels[0].members.len(), 3);
        let unfused = compile_with(&chain(), CompileOptions { fuse: false, ..Default::default() }).unwrap();
        assert_eq!(unfused.kernels.len(), 3);
    }

    #[test]
    fn static_expensive_branch_is_cached_before_animated_node() {
        let mut g = chain();
        let mut t = pixelnode_graph::Track::default();
        t.set_key(0, Value::Float(0.0));
        t.set_key(10, Value::Float(0.5));
        g.nodes[1].anim.insert("hue".into(), t); // color_adjust 會動，noise 靜態
        let plan = compile(&g).unwrap();
        assert_eq!(plan.kernels.len(), 2, "noise 應在動態節點前落地");
    }

    #[test]
    fn cheap_shared_node_is_recomputed() {
        // solid 同時餵給 color_adjust 與 blend：便宜，直接在一個 kernel 內共用。
        let g = Graph {
            nodes: vec![node(1, "solid"), node(2, "color_adjust"), node(3, "blend"), node(4, "output")],
            links: vec![link(1, 2, 0), link(1, 3, 0), link(2, 3, 1), link(3, 4, 0)],
            ..Default::default()
        };
        let plan = compile(&g).unwrap();
        assert_eq!(plan.kernels.len(), 1);
        assert_eq!(plan.kernels[0].members.len(), 4); // solid 只出現一次
    }

    #[test]
    fn shared_node_is_fusion_boundary() {
        // 昂貴的 noise 同時餵給 blend 的兩個輸入 → 必須落地成 texture。
        let g = Graph {
            nodes: vec![node(1, "noise"), node(2, "color_adjust"), node(3, "blend"), node(4, "output")],
            links: vec![link(1, 2, 0), link(1, 3, 0), link(2, 3, 1), link(3, 4, 0)],
            ..Default::default()
        };
        let plan = compile(&g).unwrap();
        assert_eq!(plan.kernels.len(), 2);
        assert_eq!(plan.kernels[1].textures, vec![0]);
        assert_eq!(plan.kernels[1].members.len(), 3); // color_adjust + blend + output
    }

    #[test]
    fn param_change_keeps_shader_key() {
        let mut g = chain();
        let a = compile(&g).unwrap();
        g.nodes[1].params.insert("hue".into(), Value::Float(0.25));
        let b = compile(&g).unwrap();
        assert_eq!(a.kernels[0].shader_key, b.kernels[0].shader_key);
        assert_ne!(a.kernels[0].params, b.kernels[0].params);
        assert_eq!(a.kernel_wgsl(0), b.kernel_wgsl(0));
    }

    #[test]
    fn static_graph_hash_is_frame_independent() {
        let g = chain();
        let at = |f| compile_with(&g, CompileOptions { frame: f, ..Default::default() }).unwrap().output_kernel().hash;
        assert_eq!(at(0), at(7));
    }

    #[test]
    fn keyframes_change_params_per_frame() {
        let mut g = chain();
        let mut t = pixelnode_graph::Track::default();
        t.set_key(0, Value::Float(0.0));
        t.set_key(10, Value::Float(1.0));
        g.nodes[0].anim.insert("phase".into(), t);
        let plan = |f| compile_with(&g, CompileOptions { frame: f, ..Default::default() }).unwrap();
        let (a, b) = (plan(0), plan(5));
        assert_ne!(a.output_kernel().hash, b.output_kernel().hash);
        assert_eq!(a.output_kernel().shader_key, b.output_kernel().shader_key);
        assert_eq!(b.kernels[0].params[7], [0.5, 0.0, 0.0, 0.0]); // noise 第 8 個參數 = phase
    }

    #[test]
    fn value_nodes_drive_params() {
        // time(循環) → wave → noise.phase
        let mut g = chain();
        g.nodes.push(node(10, "time"));
        g.nodes.push(node(11, "wave"));
        let noise_def = pixelnode_nodes::find("noise").unwrap();
        let phase_port = noise_def.param_port_of(7).unwrap();
        let wave_t = pixelnode_nodes::find("wave").unwrap().param_port_of(0).unwrap();
        g.links.push(Link { from_node: NodeId(10), from_port: 0, to_node: NodeId(11), to_port: wave_t });
        g.links.push(Link { from_node: NodeId(11), from_port: 0, to_node: NodeId(1), to_port: phase_port });
        let at = |f| compile_with(&g, CompileOptions { frame: f, ..Default::default() }).unwrap();
        let (a, b) = (at(0), at(6));
        assert!(a.steps.iter().all(|s| !s.def.is_value()), "數值節點不產生 step");
        assert_ne!(a.output_kernel().hash, b.output_kernel().hash);
        assert!(a.steps[0].animated);
        // timeline 預設 24 格：第 6 格 t = 0.25 → sin(π/2) → 0.5 + 0.5 = 1.0
        assert!((b.steps[0].params[7][0] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn value_into_surface_becomes_solid() {
        let g = Graph {
            nodes: vec![node(1, "make_color"), node(2, "output")],
            links: vec![link(1, 2, 0)],
            ..Default::default()
        };
        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps[0].def.kind, "solid");
        assert_eq!(plan.steps[0].params[0], [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn value_cycle_detected() {
        let math = pixelnode_nodes::find("math").unwrap();
        let a_port = math.param_port_of(1).unwrap();
        let g = Graph {
            nodes: vec![node(1, "math"), node(2, "math"), node(3, "solid"), node(4, "output")],
            links: vec![
                Link { from_node: NodeId(1), from_port: 0, to_node: NodeId(2), to_port: a_port },
                Link { from_node: NodeId(2), from_port: 0, to_node: NodeId(1), to_port: a_port },
                Link { from_node: NodeId(2), from_port: 0, to_node: NodeId(3), to_port: 0 },
                link(3, 4, 0),
            ],
            ..Default::default()
        };
        assert!(matches!(compile(&g), Err(CompileError::Cycle(_))));
    }

    #[test]
    fn duplicate_kinds_get_unique_names() {
        let g = Graph {
            nodes: vec![node(1, "noise"), node(2, "noise"), node(3, "blend"), node(4, "output")],
            links: vec![link(1, 3, 0), link(2, 3, 1), link(3, 4, 0)],
            ..Default::default()
        };
        let plan = compile(&g).unwrap();
        assert_eq!(plan.kernels.len(), 1);
        let src = plan.kernel_wgsl(0);
        assert!(src.contains("fn m0_noise_perlin") && src.contains("fn m1_noise_perlin"));
        assert!(src.contains("PB.p[8u + 1]"), "第二個 noise 的參數從 8 開始");
    }
}

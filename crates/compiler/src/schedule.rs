//! 從 Output 往回 DFS：只排程可達節點（pull-based）、偵測循環、
//! 在 CPU 上求值數值節點並解析參數連線。

use crate::{CompileError, Step};
use pixelnode_graph::{Graph, Node, NodeId, Value};
use pixelnode_nodes::{EvalCtx, MAX_INPUTS, MAX_PARAMS, NodeDef, convert};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

#[derive(Clone, Copy)]
enum Mark {
    Visiting,
    Done(usize),
}

pub(crate) struct Scheduler<'a> {
    defs: &'a HashMap<&'static str, &'static NodeDef>,
    nodes: HashMap<NodeId, &'a Node>,
    sources: HashMap<(NodeId, usize), (NodeId, usize)>,
    canvas: [u32; 2],
    frame: u32,
    ctx: EvalCtx,
    marks: HashMap<NodeId, Mark>,
    values: HashMap<(NodeId, usize), (Value, bool)>,
    value_visiting: HashSet<NodeId>,
    pub steps: Vec<Step>,
}

impl<'a> Scheduler<'a> {
    pub fn new(graph: &'a Graph, defs: &'a HashMap<&'static str, &'static NodeDef>, frame: u32) -> Self {
        Self {
            defs,
            nodes: graph.nodes.iter().map(|n| (n.id, n)).collect(),
            sources: graph.links.iter().map(|l| ((l.to_node, l.to_port), (l.from_node, l.from_port))).collect(),
            canvas: graph.canvas,
            frame,
            ctx: EvalCtx { frame, frames: graph.timeline.frames, fps: graph.timeline.fps },
            marks: HashMap::new(),
            values: HashMap::new(),
            value_visiting: HashSet::new(),
            steps: Vec::new(),
        }
    }

    fn node(&self, id: NodeId) -> Result<(&'a Node, &'static NodeDef), CompileError> {
        let node = *self.nodes.get(&id).ok_or(CompileError::NoOutput)?;
        let def = *self.defs.get(node.kind.as_str()).ok_or_else(|| CompileError::UnknownKind(node.kind.clone()))?;
        Ok((node, def))
    }

    /// 解析參數：keyframe / 靜態值，若參數 port 有連線則以上游數值覆蓋。
    /// 回傳 (參數值, 是否隨時間變化)。
    fn resolve_params(&mut self, node: &Node, def: &NodeDef) -> Result<(Vec<Value>, bool), CompileError> {
        let mut animated = false;
        let mut out = Vec::with_capacity(def.params.len());
        for (i, p) in def.params.iter().enumerate() {
            let mut v = node.param_at(p.name, self.frame).unwrap_or_else(|| p.default.clone());
            animated |= node.anim.get(p.name).is_some_and(|t| t.keys.len() > 1);
            if let Some(port) = def.param_port_of(i)
                && let Some(&(src, sp)) = self.sources.get(&(node.id, port))
            {
                let (sv, sa) = self.value(src, sp)?;
                v = convert(&sv, p.kind);
                animated |= sa;
            }
            out.push(v);
        }
        Ok((out, animated))
    }

    /// 數值節點的輸出（memo 化）。
    fn value(&mut self, id: NodeId, port: usize) -> Result<(Value, bool), CompileError> {
        if let Some(v) = self.values.get(&(id, port)) {
            return Ok(v.clone());
        }
        if !self.value_visiting.insert(id) {
            return Err(CompileError::Cycle(id));
        }
        let (node, def) = self.node(id)?;
        let eval = def.eval.ok_or(CompileError::TypeMismatch { to: id, port })?;
        let (params, animated) = self.resolve_params(node, def)?;
        let v = (eval(&params, &self.ctx), animated || !def.time_invariant);
        self.value_visiting.remove(&id);
        self.values.insert((id, port), v.clone());
        Ok(v)
    }

    /// 排程 Surface 節點，回傳其 step 索引。
    pub fn visit(&mut self, id: NodeId) -> Result<usize, CompileError> {
        match self.marks.get(&id) {
            Some(Mark::Done(i)) => return Ok(*i),
            Some(Mark::Visiting) => return Err(CompileError::Cycle(id)),
            None => {}
        }
        self.marks.insert(id, Mark::Visiting);
        let (node, def) = self.node(id)?;

        let mut inputs = [None; MAX_INPUTS];
        for (port, slot) in inputs.iter_mut().enumerate().take(def.inputs.len()) {
            if let Some(&(src, sp)) = self.sources.get(&(id, port)) {
                let (_, src_def) = self.node(src)?;
                *slot = Some(if src_def.is_value() {
                    // 隱式轉換：數值 / 顏色 → 純色 Surface。
                    let (v, animated) = self.value(src, sp)?;
                    let solid = pixelnode_nodes::find("solid").expect("solid node exists");
                    let mut params = [[0.0f32; 4]; MAX_PARAMS];
                    params[0] = convert(&v, pixelnode_nodes::ParamKind::Color).to_vec4();
                    self.push(src, solid, [None; MAX_INPUTS], params, animated, None)
                } else {
                    self.visit(src)?
                });
            }
        }

        let (values, params_animated) = self.resolve_params(node, def)?;
        let mut params = [[0.0f32; 4]; MAX_PARAMS];
        let mut text = None;
        for (slot, v) in params.iter_mut().zip(&values) {
            if let (Value::Text(t), None) = (v, &text) {
                text = Some(t.clone());
            }
            *slot = v.to_vec4();
        }
        let animated = !def.time_invariant
            || params_animated
            || inputs.iter().flatten().any(|&i| self.steps[i].animated);

        let idx = self.push(id, def, inputs, params, animated, text);
        self.marks.insert(id, Mark::Done(idx));
        Ok(idx)
    }

    fn push(
        &mut self,
        node: NodeId,
        def: &'static NodeDef,
        inputs: [Option<usize>; MAX_INPUTS],
        params: [[f32; 4]; MAX_PARAMS],
        animated: bool,
        text: Option<String>,
    ) -> usize {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        def.kind.hash(&mut h);
        self.canvas.hash(&mut h);
        // 與時間無關的節點不把影格納入 hash：參數值相同就跨影格共用快取。
        if !def.time_invariant {
            self.frame.hash(&mut h);
        }
        for v in params.iter().flatten() {
            v.to_bits().hash(&mut h);
        }
        for i in inputs {
            i.map(|i| self.steps[i].hash).hash(&mut h);
        }
        text.hash(&mut h);
        self.steps.push(Step { node, def, inputs, params, hash: h.finish(), animated, text });
        self.steps.len() - 1
    }
}

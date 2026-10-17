//! PixelNode 編輯器：節點畫布 + 即時預覽 + 時間軸。

mod history;
mod timeline;

use eframe::egui;
use egui_snarl::ui::{PinInfo, SnarlPin, SnarlStyle, SnarlViewer, SnarlWidget, get_selected_nodes};
use egui_snarl::{InPin, InPinId, NodeId, OutPin, OutPinId, Snarl};
use history::{Command, History};
use pixelnode_graph::{DataType, Graph, Interp, Link, Node, Timeline, Track, Value};
use pixelnode_nodes::{Category, NodeDef, ParamKind};
use pixelnode_runtime::export::{ExportKind, ExportSpec};
use pixelnode_runtime::worker::{EvalResult, Evaluator, Job, Message, Preview};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, PartialEq)]
pub struct NodeData {
    pub kind: String,
    pub params: BTreeMap<String, Value>,
    pub anim: BTreeMap<String, Track>,
}

impl NodeData {
    pub fn new(def: &NodeDef) -> Self {
        Self { kind: def.kind.to_string(), params: def.default_params(), anim: BTreeMap::new() }
    }

    pub fn param_at(&self, name: &str, frame: u32) -> Option<Value> {
        self.anim.get(name).and_then(|t| t.sample(frame)).or_else(|| self.params.get(name).cloned())
    }
}

fn graph_id() -> egui::Id {
    egui::Id::new("pixelnode-graph")
}

const SURFACE_COLOR: egui::Color32 = egui::Color32::from_rgb(0xb0, 0x7c, 0xe8);

fn pin_color(ty: DataType) -> egui::Color32 {
    match ty {
        DataType::Surface => SURFACE_COLOR,
        DataType::Float => egui::Color32::from_rgb(0x6c, 0xc6, 0x6c),
        DataType::Int => egui::Color32::from_rgb(0x4c, 0xb8, 0xb8),
        DataType::Color => egui::Color32::from_rgb(0xe8, 0xc0, 0x4c),
        _ => egui::Color32::GRAY,
    }
}

struct Viewer<'a> {
    defs: &'a HashMap<String, NodeDef>,
    history: &'a mut History,
    frame: u32,
}

const KEY_ON: egui::Color32 = egui::Color32::from_rgb(0xf5, 0xc5, 0x42);
const KEY_TRACK: egui::Color32 = egui::Color32::from_rgb(0xc0, 0x80, 0x30);

impl Viewer<'_> {
    fn def(&self, node: &NodeData) -> &NodeDef {
        &self.defs[&node.kind]
    }
}

impl Viewer<'_> {
    /// ◆ 目前影格有 key；◇（橘）有軌道但此格無 key；◇（灰）未動畫。
    /// 左鍵切換 key，右鍵設定此 key 的插值方式。
    fn key_button(
        &mut self,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
        node: NodeId,
        name: &str,
        track: Option<&Track>,
        current: &Value,
    ) {
        let frame = self.frame;
        let key = track.and_then(|t| t.key_at(frame));
        let (glyph, color) = match (track, key) {
            (_, Some(_)) => ("◆", KEY_ON),
            (Some(_), None) => ("◇", KEY_TRACK),
            (None, _) => ("◇", ui.visuals().weak_text_color()),
        };
        let resp = ui
            .add(egui::Button::new(egui::RichText::new(glyph).color(color)).frame(false))
            .on_hover_text("左鍵：新增 / 移除 keyframe　右鍵：插值方式");
        let mut new_track = None;
        if resp.clicked() {
            let mut t = track.cloned().unwrap_or_default();
            if key.is_some() {
                t.remove_key(frame);
            } else {
                t.set_key(frame, current.clone());
            }
            new_track = Some(if t.is_empty() { None } else { Some(t) });
        }
        if let (Some(t), Some(k)) = (track, key) {
            resp.context_menu(|ui| {
                for interp in Interp::ALL {
                    if ui.selectable_label(k.interp == interp, interp.label()).clicked() {
                        let mut t = t.clone();
                        if let Some(k) = t.keys.iter_mut().find(|k| k.frame == frame) {
                            k.interp = interp;
                        }
                        new_track = Some(Some(t));
                        ui.close();
                    }
                }
            });
        }
        if let Some(new) = new_track {
            match &new {
                Some(t) => snarl[node].anim.insert(name.to_string(), t.clone()),
                None => snarl[node].anim.remove(name),
            };
            self.history.seal();
            self.history.record(Command::SetTrack { node, name: name.to_string(), old: track.cloned(), new });
            self.history.seal();
        }
    }
}

impl SnarlViewer<NodeData> for Viewer<'_> {
    fn title(&mut self, node: &NodeData) -> String {
        self.def(node).title.to_string()
    }

    fn inputs(&mut self, node: &NodeData) -> usize {
        self.def(node).input_count()
    }

    fn outputs(&mut self, node: &NodeData) -> usize {
        self.def(node).outputs.len()
    }

    fn show_input(&mut self, pin: &InPin, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> impl SnarlPin + 'static {
        let def = self.def(&snarl[pin.id.node]);
        let (name, ty) = def.input_port(pin.id.input).unwrap_or(("?", DataType::Surface));
        if pin.id.input < def.inputs.len() {
            ui.label(name);
            PinInfo::circle().with_fill(pin_color(ty))
        } else {
            // 參數 port：小字標籤，連線後該參數由上游數值驅動。
            ui.label(egui::RichText::new(name).small().weak());
            PinInfo::circle().with_fill(pin_color(ty))
        }
    }

    fn show_output(&mut self, pin: &OutPin, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> impl SnarlPin + 'static {
        let port = self.def(&snarl[pin.id.node]).outputs[pin.id.output];
        ui.label(port.name);
        PinInfo::circle().with_fill(pin_color(port.ty))
    }

    fn has_body(&mut self, node: &NodeData) -> bool {
        !self.def(node).params.is_empty()
    }

    fn show_body(
        &mut self,
        node: NodeId,
        inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) {
        let def = self.defs[&snarl[node].kind].clone();
        ui.vertical(|ui| {
            ui.set_max_width(if def.params.iter().any(|p| matches!(p.kind, ParamKind::Text { multiline: true })) {
                340.0
            } else {
                200.0
            });
            for (pi, p) in def.params.iter().enumerate() {
                let linked = def
                    .param_port_of(pi)
                    .and_then(|port| inputs.get(port))
                    .is_some_and(|pin| !pin.remotes.is_empty());
                if linked {
                    ui.horizontal(|ui| {
                        ui.add_space(18.0);
                        ui.label(p.label);
                        ui.weak("← 已連線");
                    });
                    continue;
                }
                let frame = self.frame;
                let track = snarl[node].anim.get(p.name).cloned();
                let old = snarl[node].param_at(p.name, frame).unwrap_or_else(|| p.default.clone());
                let mut v = old.clone();
                ui.horizontal(|ui| {
                    self.key_button(ui, snarl, node, p.name, track.as_ref(), &old);
                    ui.label(p.label);
                    match (&mut v, p.kind) {
                        (Value::Float(x), ParamKind::Float { min, max }) => {
                            ui.add(egui::Slider::new(x, min..=max));
                        }
                        (Value::Int(x), ParamKind::Int { min, max }) => {
                            ui.add(egui::DragValue::new(x).range(min..=max));
                        }
                        (Value::Color(c), ParamKind::Color) => {
                            ui.color_edit_button_rgba_unmultiplied(c);
                        }
                        (Value::Enum(e), ParamKind::Enum(opts)) => {
                            egui::ComboBox::from_id_salt((node, p.name))
                                .selected_text(opts.get(*e as usize).copied().unwrap_or("?"))
                                .show_ui(ui, |ui| {
                                    for (i, o) in opts.iter().enumerate() {
                                        ui.selectable_value(e, i as u32, *o);
                                    }
                                });
                        }
                        (Value::Text(t), ParamKind::Text { multiline }) => {
                            let edit = if multiline {
                                egui::TextEdit::multiline(t).code_editor().desired_rows(6).desired_width(320.0)
                            } else {
                                egui::TextEdit::singleline(t).desired_width(180.0)
                            };
                            ui.add(edit);
                        }
                        _ => {
                            ui.weak("（型別不符）");
                        }
                    }
                });
                if v != old {
                    match track {
                        // 有軌道：編輯即在目前影格設 key。
                        Some(t) => {
                            let mut new = t.clone();
                            new.set_key(frame, v);
                            snarl[node].anim.insert(p.name.to_string(), new.clone());
                            self.history.record(Command::SetTrack {
                                node,
                                name: p.name.to_string(),
                                old: Some(t),
                                new: Some(new),
                            });
                        }
                        None => {
                            snarl[node].params.insert(p.name.to_string(), v.clone());
                            self.history.record(Command::SetParam { node, name: p.name.to_string(), old, new: v });
                        }
                    }
                }
            }
        });
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<NodeData>) {
        let out_ty = self.def(&snarl[from.id.node]).outputs[from.id.output].ty;
        let Some((_, in_ty)) = self.def(&snarl[to.id.node]).input_port(to.id.input) else { return };
        if !in_ty.accepts(out_ty) || from.id.node == to.id.node {
            return;
        }
        // 每個輸入只允許一條連線：先拔掉舊的。
        let mut batch: Vec<_> = to.remotes.iter().map(|&o| Command::Disconnect(o, to.id)).collect();
        snarl.drop_inputs(to.id);
        snarl.connect(from.id, to.id);
        batch.push(Command::Connect(from.id, to.id));
        self.history.record(Command::Batch(batch));
    }

    fn disconnect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<NodeData>) {
        snarl.disconnect(from.id, to.id);
        self.history.record(Command::Disconnect(from.id, to.id));
    }

    fn drop_inputs(&mut self, pin: &InPin, snarl: &mut Snarl<NodeData>) {
        let batch = pin.remotes.iter().map(|&o| Command::Disconnect(o, pin.id)).collect();
        snarl.drop_inputs(pin.id);
        self.history.record(Command::Batch(batch));
    }

    fn drop_outputs(&mut self, pin: &OutPin, snarl: &mut Snarl<NodeData>) {
        let batch = pin.remotes.iter().map(|&i| Command::Disconnect(pin.id, i)).collect();
        snarl.drop_outputs(pin.id);
        self.history.record(Command::Batch(batch));
    }

    fn has_graph_menu(&mut self, _pos: egui::Pos2, _snarl: &mut Snarl<NodeData>) -> bool {
        true
    }

    fn show_graph_menu(&mut self, pos: egui::Pos2, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
        ui.label("新增節點");
        for cat in Category::ALL {
            ui.menu_button(cat.label(), |ui| {
                for def in pixelnode_nodes::registry().iter().filter(|d| d.category == cat) {
                    if ui.button(def.title).clicked() {
                        let data = NodeData::new(def);
                        let id = snarl.insert_node(pos, data.clone());
                        self.history.record(Command::AddNode { id, pos, data });
                        ui.close();
                    }
                }
            });
        }
    }

    fn has_node_menu(&mut self, _node: &NodeData) -> bool {
        true
    }

    fn show_node_menu(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) {
        if ui.button("刪除").clicked() {
            remove_node(snarl, self.history, node);
            ui.close();
        }
    }
}

fn remove_node(snarl: &mut Snarl<NodeData>, history: &mut History, node: NodeId) {
    let mut batch: Vec<Command> = snarl
        .wires()
        .filter(|(o, i)| o.node == node || i.node == node)
        .map(|(o, i)| Command::Disconnect(o, i))
        .collect();
    let pos = snarl.get_node_info(node).map(|n| n.pos).unwrap_or_default();
    let data = snarl.remove_node(node);
    batch.push(Command::RemoveNode { id: node, pos, data });
    history.record(Command::Batch(batch));
}

fn snarl_to_graph(snarl: &Snarl<NodeData>, canvas: [u32; 2], timeline: Timeline) -> Graph {
    let nodes = snarl
        .nodes_pos_ids()
        .map(|(id, pos, d)| Node {
            id: pixelnode_graph::NodeId(id.0 as u64),
            kind: d.kind.clone(),
            params: d.params.clone(),
            pos: [pos.x, pos.y],
            anim: d.anim.clone(),
        })
        .collect();
    let mut links: Vec<Link> = snarl
        .wires()
        .map(|(o, i)| Link {
            from_node: pixelnode_graph::NodeId(o.node.0 as u64),
            from_port: o.output,
            to_node: pixelnode_graph::NodeId(i.node.0 as u64),
            to_port: i.input,
        })
        .collect();
    links.sort_by_key(|l| (l.to_node, l.to_port, l.from_node, l.from_port));
    Graph { canvas, nodes, links, timeline, ..Default::default() }
}

fn graph_to_snarl(graph: &Graph) -> Snarl<NodeData> {
    let mut snarl = Snarl::new();
    let mut ids = HashMap::new();
    for n in &graph.nodes {
        let id = snarl.insert_node(egui::pos2(n.pos[0], n.pos[1]), NodeData { kind: n.kind.clone(), params: n.params.clone(), anim: n.anim.clone() });
        ids.insert(n.id, id);
    }
    for l in &graph.links {
        if let (Some(&a), Some(&b)) = (ids.get(&l.from_node), ids.get(&l.to_node)) {
            snarl.connect(OutPinId { node: a, output: l.from_port }, InPinId { node: b, input: l.to_port });
        }
    }
    snarl
}

fn default_snarl() -> Snarl<NodeData> {
    let mut s = Snarl::new();
    let d = |k| NodeData::new(pixelnode_nodes::find(k).unwrap());
    // 示範：相位 0→1 跨整段時間軸，循環播放無接縫。
    let mut noise = d("noise");
    let mut phase = Track::default();
    phase.set_key(0, Value::Float(0.0));
    phase.set_key(Timeline::default().frames, Value::Float(1.0));
    noise.anim.insert("phase".into(), phase);
    let noise = s.insert_node(egui::pos2(0.0, 0.0), noise);
    let mut adj = d("color_adjust");
    adj.params.insert("tint".into(), Value::Color([1.0, 0.6, 0.3, 1.0]));
    let adj = s.insert_node(egui::pos2(320.0, 0.0), adj);
    let out = s.insert_node(egui::pos2(640.0, 0.0), d("output"));
    s.connect(OutPinId { node: noise, output: 0 }, InPinId { node: adj, input: 0 });
    s.connect(OutPinId { node: adj, output: 0 }, InPinId { node: out, input: 0 });
    s
}

enum PreviewTex {
    /// 與求值共用 device：直接顯示 GPU texture。
    Native { id: egui::TextureId, size: egui::Vec2 },
    /// 退路：CPU 讀回後上傳。
    Managed(egui::TextureHandle),
}

impl PreviewTex {
    fn id_size(&self) -> (egui::TextureId, egui::Vec2) {
        match self {
            PreviewTex::Native { id, size } => (*id, *size),
            PreviewTex::Managed(t) => (t.id(), t.size_vec2()),
        }
    }
}

struct App {
    snarl: Snarl<NodeData>,
    style: SnarlStyle,
    defs: HashMap<String, NodeDef>,
    history: History,
    canvas: [u32; 2],
    evaluator: Evaluator,
    /// 上次送出的求值 snapshot（不含節點位置），用於避免重複送出。
    submitted: Option<(Graph, u32)>,
    last: Option<EvalResult>,
    preview: Option<PreviewTex>,
    render_state: Option<eframe::egui_wgpu::RenderState>,
    path: String,
    status: String,
    timeline: Timeline,
    frame: u32,
    playing: bool,
    play_clock: f64,
    export_dir: String,
    export_scale: u32,
    exporting: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let render_state = cc.wgpu_render_state.clone();
        let shared = render_state
            .as_ref()
            .map(|rs| (rs.device.clone(), rs.queue.clone(), rs.adapter.get_info().name));
        Self {
            snarl: default_snarl(),
            style: SnarlStyle { collapsible: Some(true), ..SnarlStyle::new() },
            defs: pixelnode_nodes::registry().iter().map(|d| (d.kind.to_string(), d.clone())).collect(),
            history: History::default(),
            canvas: [128, 128],
            evaluator: Evaluator::spawn(shared, move || ctx.request_repaint()),
            render_state,
            submitted: None,
            last: None,
            preview: None,
            path: "graph.pixelnode.json".into(),
            status: String::new(),
            timeline: Timeline::default(),
            frame: 0,
            playing: false,
            play_clock: 0.0,
            export_dir: "export".into(),
            export_scale: 4,
            exporting: false,
        }
    }

    fn save(&mut self) {
        let json = self.graph().to_json();
        self.status = match std::fs::write(&self.path, json) {
            Ok(()) => format!("已儲存 {}", self.path),
            Err(e) => format!("儲存失敗：{e}"),
        };
    }

    fn load(&mut self) {
        let result = std::fs::read_to_string(&self.path)
            .map_err(|e| e.to_string())
            .and_then(|s| Graph::from_json(&s).map_err(|e| e.to_string()));
        match result {
            Ok(g) => {
                self.snarl = graph_to_snarl(&g);
                self.canvas = g.canvas;
                self.timeline = g.timeline;
                self.frame = 0;
                self.history.clear();
                self.status = format!("已載入 {}", self.path);
            }
            Err(e) => self.status = format!("載入失敗：{e}"),
        }
    }

    /// 把拖進視窗的 PNG 建成 Image In 節點。
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let files = ctx.input(|i| i.raw.dropped_files.clone());
        let def = pixelnode_nodes::find("image").expect("image node exists");
        for (k, f) in files.iter().enumerate() {
            let Some(path) = &f.path else { continue };
            if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) {
                self.status = format!("只支援 PNG：{}", path.display());
                continue;
            }
            let mut data = NodeData::new(def);
            data.params.insert("path".into(), Value::Text(path.to_string_lossy().into_owned()));
            let pos = egui::pos2(-320.0, 40.0 + k as f32 * 160.0);
            let id = self.snarl.insert_node(pos, data.clone());
            self.history.record(Command::AddNode { id, pos, data });
            self.status = format!("已加入圖片：{}", path.display());
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() {
            return;
        }
        let (space, left, right) = ctx.input(|i| {
            (i.key_pressed(egui::Key::Space), i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight))
        });
        if space {
            self.playing = !self.playing;
        }
        let n = self.timeline.frames.max(1);
        if left {
            self.frame = (self.frame + n - 1) % n;
        }
        if right {
            self.frame = (self.frame + 1) % n;
        }
        let (undo, redo, delete) = ctx.input_mut(|i| {
            let redo = i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Z,
            )) || i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Y));
            let undo = i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z));
            (undo, redo, i.key_pressed(egui::Key::Delete))
        });
        if undo {
            self.history.undo(&mut self.snarl);
        }
        if redo {
            self.history.redo(&mut self.snarl);
        }
        if delete {
            for id in get_selected_nodes(graph_id(), ctx) {
                if self.snarl.get_node(id).is_some() {
                    remove_node(&mut self.snarl, &mut self.history, id);
                }
            }
        }
    }

    fn graph(&self) -> Graph {
        snarl_to_graph(&self.snarl, self.canvas, self.timeline)
    }

    fn submit_if_changed(&mut self) {
        let mut g = self.graph();
        g.nodes.iter_mut().for_each(|n| n.pos = [0.0; 2]);
        if self.submitted.as_ref().is_none_or(|(sg, sf)| *sg != g || *sf != self.frame) {
            self.evaluator.submit(Job::Eval { graph: g.clone(), frame: self.frame });
            self.submitted = Some((g, self.frame));
        }
    }

    fn export(&mut self, kind: ExportKind) {
        let dir = std::path::PathBuf::from(&self.export_dir);
        let path = match kind {
            ExportKind::Gif => dir.join("anim.gif"),
            ExportKind::PngSequence => dir.join("frames"),
            ExportKind::SpriteSheet { .. } => dir.join("sheet.png"),
        };
        self.exporting = true;
        self.status = "匯出中…".into();
        let spec = ExportSpec { kind, path, scale: self.export_scale };
        self.evaluator.submit(Job::Export { graph: self.graph(), spec });
    }

    fn receive(&mut self, ctx: &egui::Context) {
        for msg in self.evaluator.poll() {
            let res = match msg {
                Message::Eval(res) => res,
                Message::Export(r) => {
                    self.exporting = false;
                    self.status = match r {
                        Ok(s) => format!("已匯出：{s}"),
                        Err(e) => format!("匯出失敗：{e}"),
                    };
                    continue;
                }
            };
            match &res.preview {
                Ok(Preview::Gpu { texture, fresh }) => {
                    let size = egui::vec2(texture.width() as f32, texture.height() as f32);
                    let rs = self.render_state.as_ref().expect("GPU preview requires wgpu render state");
                    let mut renderer = rs.renderer.write();
                    if let Some(PreviewTex::Native { id, size: s }) = &mut self.preview {
                        *s = size;
                        if *fresh {
                            let view = texture.create_view(&Default::default());
                            renderer.update_egui_texture_from_wgpu_texture(
                                &rs.device,
                                &view,
                                eframe::wgpu::FilterMode::Nearest,
                                *id,
                            );
                        }
                    } else {
                        let view = texture.create_view(&Default::default());
                        let id = renderer.register_native_texture(&rs.device, &view, eframe::wgpu::FilterMode::Nearest);
                        self.preview = Some(PreviewTex::Native { id, size });
                    }
                }
                Ok(Preview::Cpu(img)) => {
                    let color =
                        egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.rgba);
                    match &mut self.preview {
                        Some(PreviewTex::Managed(t)) => t.set(color, egui::TextureOptions::NEAREST),
                        _ => {
                            self.preview =
                                Some(PreviewTex::Managed(ctx.load_texture("preview", color, egui::TextureOptions::NEAREST)))
                        }
                    }
                }
                Err(_) => {}
            }
            self.last = Some(res);
        }
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("預覽");
        ui.horizontal(|ui| {
            ui.label("畫布");
            for v in &mut self.canvas {
                ui.add(egui::DragValue::new(v).range(1..=2048));
            }
        });
        if let Some(res) = &self.last {
            match &res.preview {
                Ok(_) => ui.label(format!(
                    "{:.2} ms · {} kernels（執行 {} / 快取 {}）",
                    res.elapsed.as_secs_f64() * 1000.0,
                    res.stats.kernels,
                    res.stats.dispatched,
                    res.stats.cached
                )),
                Err(e) => ui.colored_label(egui::Color32::LIGHT_RED, e),
            };
            for w in &res.warnings {
                ui.colored_label(egui::Color32::from_rgb(0xe8, 0xb0, 0x4c), format!("⚠ {w}"));
            }
        }
        ui.separator();
        if let Some(tex) = &self.preview {
            // 整數縮放，point filtering。
            let (id, size) = tex.id_size();
            let avail = ui.available_size();
            let scale = (avail.x / size.x).min(avail.y / size.y).floor().max(1.0);
            ui.image((id, size * scale));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_shortcuts(&ctx);
        self.handle_dropped_files(&ctx);
        self.receive(&ctx);

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("新建").clicked() {
                    self.snarl = default_snarl();
                    self.history.clear();
                }
                ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(240.0));
                if ui.button("儲存").clicked() {
                    self.save();
                }
                if ui.button("載入").clicked() {
                    self.load();
                }
                ui.separator();
                if ui.add_enabled(self.history.can_undo(), egui::Button::new("復原")).clicked() {
                    self.history.undo(&mut self.snarl);
                }
                if ui.add_enabled(self.history.can_redo(), egui::Button::new("重做")).clicked() {
                    self.history.redo(&mut self.snarl);
                }
                ui.separator();
                ui.weak("右鍵新增節點 · Del 刪除 · Ctrl+Z/Y · 空白鍵播放 · ←→ 逐格");
                ui.label(&self.status);
            });
        });

        if self.playing {
            let dt = ctx.input(|i| i.stable_dt) as f64;
            self.play_clock += dt;
            let step = 1.0 / self.timeline.fps.max(1) as f64;
            while self.play_clock >= step {
                self.play_clock -= step;
                self.frame = (self.frame + 1) % self.timeline.frames.max(1);
            }
            ctx.request_repaint();
        }

        egui::Panel::bottom("timeline").show(ui, |ui| self.timeline_panel(ui));
        egui::Panel::right("preview").default_size(360.0).show(ui, |ui| self.preview_panel(ui));

        egui::CentralPanel::default().show(ui, |ui| {
            let mut viewer = Viewer { defs: &self.defs, history: &mut self.history, frame: self.frame };
            SnarlWidget::new().id(graph_id()).style(self.style).show(&mut self.snarl, &mut viewer, ui);
        });

        // 滑鼠放開時結束參數合併，讓下一次拖曳成為獨立的 undo 步驟。
        if ctx.input(|i| i.pointer.any_released()) {
            self.history.seal();
        }

        self.submit_if_changed();
    }
}

fn install_cjk_font(ctx: &egui::Context) {
    let candidates = [r"C:\Windows\Fonts\msjh.ttc", r"C:\Windows\Fonts\msyh.ttc", r"C:\Windows\Fonts\mingliu.ttc"];
    let Some(bytes) = candidates.iter().find_map(|p| std::fs::read(p).ok()) else { return };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("cjk".into(), egui::FontData::from_owned(bytes).into());
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("cjk".into());
    }
    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1400.0, 800.0]).with_title("PixelNode"),
        ..Default::default()
    };
    eframe::run_native("PixelNode", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snarl_graph_roundtrip() {
        let s = default_snarl();
        let tl = Timeline { frames: 8, fps: 10 };
        let g = snarl_to_graph(&s, [64, 32], tl);
        let g2 = Graph::from_json(&g.to_json()).unwrap();
        assert_eq!(g2.timeline, tl);
        assert_eq!(g, snarl_to_graph(&graph_to_snarl(&g2), [64, 32], tl));
        assert!(g.nodes.iter().any(|n| n.anim.contains_key("phase")));
        assert!(pixelnode_compiler::compile(&g).is_ok());
    }
}

#[cfg(test)]
mod example_tests {
    #[test]
    fn bundled_examples_compile() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let g = pixelnode_graph::Graph::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
            pixelnode_compiler::compile(&g).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        }
    }
}

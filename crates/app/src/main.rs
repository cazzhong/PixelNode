//! PixelNode 編輯器：節點畫布 + 即時預覽。

mod history;

use eframe::egui;
use egui_snarl::ui::{PinInfo, SnarlPin, SnarlStyle, SnarlViewer, SnarlWidget, get_selected_nodes};
use egui_snarl::{InPin, InPinId, NodeId, OutPin, OutPinId, Snarl};
use history::{Command, History};
use pixelnode_graph::{Graph, Link, Node, Value};
use pixelnode_nodes::{NodeDef, ParamKind};
use pixelnode_runtime::worker::{EvalResult, Evaluator};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, PartialEq)]
pub struct NodeData {
    pub kind: String,
    pub params: BTreeMap<String, Value>,
}

impl NodeData {
    pub fn new(def: &NodeDef) -> Self {
        Self { kind: def.kind.to_string(), params: def.default_params() }
    }
}

fn graph_id() -> egui::Id {
    egui::Id::new("pixelnode-graph")
}

const SURFACE_COLOR: egui::Color32 = egui::Color32::from_rgb(0xb0, 0x7c, 0xe8);

struct Viewer<'a> {
    defs: &'a HashMap<String, NodeDef>,
    history: &'a mut History,
}

impl Viewer<'_> {
    fn def(&self, node: &NodeData) -> &NodeDef {
        &self.defs[&node.kind]
    }
}

impl SnarlViewer<NodeData> for Viewer<'_> {
    fn title(&mut self, node: &NodeData) -> String {
        self.def(node).title.to_string()
    }

    fn inputs(&mut self, node: &NodeData) -> usize {
        self.def(node).inputs.len()
    }

    fn outputs(&mut self, node: &NodeData) -> usize {
        self.def(node).outputs.len()
    }

    fn show_input(&mut self, pin: &InPin, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> impl SnarlPin + 'static {
        ui.label(self.def(&snarl[pin.id.node]).inputs[pin.id.input].name);
        PinInfo::circle().with_fill(SURFACE_COLOR)
    }

    fn show_output(&mut self, pin: &OutPin, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> impl SnarlPin + 'static {
        ui.label(self.def(&snarl[pin.id.node]).outputs[pin.id.output].name);
        PinInfo::circle().with_fill(SURFACE_COLOR)
    }

    fn has_body(&mut self, node: &NodeData) -> bool {
        !self.def(node).params.is_empty()
    }

    fn show_body(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) {
        let def = self.defs[&snarl[node].kind].clone();
        ui.vertical(|ui| {
            ui.set_max_width(200.0);
            for p in &def.params {
                let old = snarl[node].params.get(p.name).cloned().unwrap_or_else(|| p.default.clone());
                let mut v = old.clone();
                ui.horizontal(|ui| {
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
                        _ => {
                            ui.weak("（型別不符）");
                        }
                    }
                });
                if v != old {
                    snarl[node].params.insert(p.name.to_string(), v.clone());
                    self.history.record(Command::SetParam { node, name: p.name.to_string(), old, new: v });
                }
            }
        });
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<NodeData>) {
        let out_ty = self.def(&snarl[from.id.node]).outputs[from.id.output].ty;
        let in_ty = self.def(&snarl[to.id.node]).inputs[to.id.input].ty;
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
        for def in pixelnode_nodes::registry() {
            if ui.button(def.title).clicked() {
                let data = NodeData::new(def);
                let id = snarl.insert_node(pos, data.clone());
                self.history.record(Command::AddNode { id, pos, data });
                ui.close();
            }
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

fn snarl_to_graph(snarl: &Snarl<NodeData>, canvas: [u32; 2]) -> Graph {
    let nodes = snarl
        .nodes_pos_ids()
        .map(|(id, pos, d)| Node {
            id: pixelnode_graph::NodeId(id.0 as u64),
            kind: d.kind.clone(),
            params: d.params.clone(),
            pos: [pos.x, pos.y],
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
    Graph { canvas, nodes, links, ..Default::default() }
}

fn graph_to_snarl(graph: &Graph) -> Snarl<NodeData> {
    let mut snarl = Snarl::new();
    let mut ids = HashMap::new();
    for n in &graph.nodes {
        let id = snarl.insert_node(egui::pos2(n.pos[0], n.pos[1]), NodeData { kind: n.kind.clone(), params: n.params.clone() });
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
    let noise = s.insert_node(egui::pos2(0.0, 0.0), d("noise"));
    let mut adj = d("color_adjust");
    adj.params.insert("tint".into(), Value::Color([1.0, 0.6, 0.3, 1.0]));
    let adj = s.insert_node(egui::pos2(320.0, 0.0), adj);
    let out = s.insert_node(egui::pos2(640.0, 0.0), d("output"));
    s.connect(OutPinId { node: noise, output: 0 }, InPinId { node: adj, input: 0 });
    s.connect(OutPinId { node: adj, output: 0 }, InPinId { node: out, input: 0 });
    s
}

struct App {
    snarl: Snarl<NodeData>,
    style: SnarlStyle,
    defs: HashMap<String, NodeDef>,
    history: History,
    canvas: [u32; 2],
    evaluator: Evaluator,
    /// 上次送出的求值 snapshot（不含節點位置），用於避免重複送出。
    submitted: Option<Graph>,
    last: Option<EvalResult>,
    preview: Option<egui::TextureHandle>,
    path: String,
    status: String,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        Self {
            snarl: default_snarl(),
            style: SnarlStyle { collapsible: Some(true), ..SnarlStyle::new() },
            defs: pixelnode_nodes::registry().iter().map(|d| (d.kind.to_string(), d.clone())).collect(),
            history: History::default(),
            canvas: [128, 128],
            evaluator: Evaluator::spawn(move || ctx.request_repaint()),
            submitted: None,
            last: None,
            preview: None,
            path: "graph.pixelnode.json".into(),
            status: String::new(),
        }
    }

    fn save(&mut self) {
        let json = snarl_to_graph(&self.snarl, self.canvas).to_json();
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
                self.history.clear();
                self.status = format!("已載入 {}", self.path);
            }
            Err(e) => self.status = format!("載入失敗：{e}"),
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() {
            return;
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

    fn submit_if_changed(&mut self) {
        let mut g = snarl_to_graph(&self.snarl, self.canvas);
        g.nodes.iter_mut().for_each(|n| n.pos = [0.0; 2]);
        if self.submitted.as_ref() != Some(&g) {
            self.evaluator.submit(g.clone());
            self.submitted = Some(g);
        }
    }

    fn receive(&mut self, ctx: &egui::Context) {
        if let Some(res) = self.evaluator.poll() {
            if let Ok(img) = &res.image {
                let color = egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.rgba);
                match &mut self.preview {
                    Some(t) => t.set(color, egui::TextureOptions::NEAREST),
                    None => self.preview = Some(ctx.load_texture("preview", color, egui::TextureOptions::NEAREST)),
                }
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
            match &res.image {
                Ok(_) => ui.label(format!(
                    "{:.2} ms · {} kernels（執行 {} / 快取 {}）",
                    res.elapsed.as_secs_f64() * 1000.0,
                    res.stats.kernels,
                    res.stats.dispatched,
                    res.stats.cached
                )),
                Err(e) => ui.colored_label(egui::Color32::LIGHT_RED, e),
            };
        }
        ui.separator();
        if let Some(tex) = &self.preview {
            // 整數縮放，point filtering。
            let size = tex.size_vec2();
            let avail = ui.available_size();
            let scale = (avail.x / size.x).min(avail.y / size.y).floor().max(1.0);
            ui.image((tex.id(), size * scale));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_shortcuts(&ctx);
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
                ui.weak("右鍵畫布新增節點 · Del 刪除 · Ctrl+Z / Ctrl+Y");
                ui.label(&self.status);
            });
        });

        egui::Panel::right("preview").default_size(360.0).show(ui, |ui| self.preview_panel(ui));

        egui::CentralPanel::default().show(ui, |ui| {
            let mut viewer = Viewer { defs: &self.defs, history: &mut self.history };
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
        let g = snarl_to_graph(&s, [64, 32]);
        let g2 = snarl_to_graph(&graph_to_snarl(&Graph::from_json(&g.to_json()).unwrap()), [64, 32]);
        assert_eq!(g, g2);
        assert!(pixelnode_compiler::compile(&g).is_ok());
    }
}

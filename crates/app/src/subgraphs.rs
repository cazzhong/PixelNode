//! 子圖管理：切換編輯中的圖、新增 / 刪除 / 改名、把選取節點收合成子圖。
//!
//! `App.snarl` / `App.history` 永遠是「正在編輯的圖」；其他圖暫存在 `App.parked`，
//! 切換時互換。每張圖有自己的 Undo 歷史。

use crate::history::{Command, History};
use crate::{App, NodeData, graph_id, graph_to_snarl, snarl_to_graph};
use eframe::egui;
use egui_snarl::ui::get_selected_nodes;
use egui_snarl::{InPinId, NodeId, OutPinId, Snarl};
use pixelnode_graph::{Graph, Value};
use std::collections::{BTreeMap, HashSet};

pub(crate) struct Parked {
    pub snarl: Snarl<NodeData>,
    pub history: History,
}

/// 新子圖的預設內容：Group In → Group Out。
pub(crate) fn default_subgraph() -> Snarl<NodeData> {
    let mut s = Snarl::new();
    let d = |k| NodeData::new(pixelnode_nodes::find(k).unwrap());
    let a = s.insert_node(egui::pos2(0.0, 0.0), d("group_in"));
    let b = s.insert_node(egui::pos2(360.0, 0.0), d("group_out"));
    s.connect(OutPinId { node: a, output: 0 }, InPinId { node: b, input: 0 });
    s
}

fn is_instance(d: &NodeData) -> bool {
    matches!(d.kind.as_str(), "subgraph" | "loop")
}

fn instance_name(d: &NodeData) -> Option<&str> {
    match d.params.get("name") {
        Some(Value::Text(t)) if is_instance(d) => Some(t),
        _ => None,
    }
}

impl App {
    /// 所有子圖名稱（含正在編輯的）。
    pub(crate) fn subgraph_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.parked.keys().flatten().cloned().collect();
        if let Some(n) = &self.editing {
            v.push(n.clone());
        }
        v.sort();
        v
    }

    fn snarl_of(&self, key: &Option<String>) -> Option<&Snarl<NodeData>> {
        if *key == self.editing { Some(&self.snarl) } else { self.parked.get(key).map(|p| &p.snarl) }
    }

    /// 組出完整的圖（根圖 + 所有子圖）。
    pub(crate) fn graph(&self) -> Graph {
        let root = self.snarl_of(&None).expect("root graph always exists");
        let mut g = snarl_to_graph(root, self.canvas, self.timeline);
        for name in self.subgraph_names() {
            let s = self.snarl_of(&Some(name.clone())).expect("listed subgraph exists");
            g.subgraphs.insert(name, snarl_to_graph(s, self.canvas, self.timeline));
        }
        g
    }

    /// 以載入的圖取代全部內容。
    pub(crate) fn set_graph(&mut self, g: &Graph) {
        self.parked.clear();
        self.editing = None;
        self.snarl = graph_to_snarl(g);
        self.history.clear();
        for (name, sub) in &g.subgraphs {
            self.parked.insert(Some(name.clone()), Parked { snarl: graph_to_snarl(sub), history: History::default() });
        }
    }

    pub(crate) fn switch_to(&mut self, target: Option<String>) {
        if target == self.editing {
            return;
        }
        let Some(next) = self.parked.remove(&target) else { return };
        let cur = Parked {
            snarl: std::mem::replace(&mut self.snarl, next.snarl),
            history: std::mem::replace(&mut self.history, next.history),
        };
        self.parked.insert(self.editing.take(), cur);
        self.rename_buf = target.clone().unwrap_or_default();
        self.editing = target;
    }

    fn unique_name(&self, base: &str) -> String {
        let names = self.subgraph_names();
        (1..).map(|i| format!("{base} {i}")).find(|n| !names.contains(n)).unwrap()
    }

    fn for_each_snarl(&mut self, mut f: impl FnMut(&mut Snarl<NodeData>)) {
        f(&mut self.snarl);
        for p in self.parked.values_mut() {
            f(&mut p.snarl);
        }
    }

    fn uses_of(&self, name: &str) -> usize {
        let mut n = self.snarl.nodes().filter(|d| instance_name(d) == Some(name)).count();
        for p in self.parked.values() {
            n += p.snarl.nodes().filter(|d| instance_name(d) == Some(name)).count();
        }
        n
    }

    pub(crate) fn subgraph_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("圖");
        if ui.selectable_label(self.editing.is_none(), "◉ 根圖").clicked() {
            self.switch_to(None);
        }
        for name in self.subgraph_names() {
            let label = format!("▣ {name}（{} 處使用）", self.uses_of(&name));
            if ui.selectable_label(self.editing.as_deref() == Some(&name), label).clicked() {
                self.switch_to(Some(name));
            }
        }
        ui.separator();
        if ui.button("＋ 新增子圖").clicked() {
            let name = self.unique_name("子圖");
            self.parked.insert(Some(name.clone()), Parked { snarl: default_subgraph(), history: History::default() });
            self.switch_to(Some(name));
        }
        if ui.button("從選取建立子圖").on_hover_text("最多 2 個外部輸入、1 個輸出").clicked() {
            match self.collapse_selection(ui.ctx()) {
                Ok(name) => self.status = format!("已建立子圖「{name}」"),
                Err(e) => self.status = e,
            }
        }

        if let Some(cur) = self.editing.clone() {
            ui.separator();
            ui.label("目前子圖");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.rename_buf).desired_width(110.0));
                if ui.button("改名").clicked() {
                    self.rename_subgraph(&cur);
                }
            });
            let uses = self.uses_of(&cur);
            if ui.add_enabled(uses == 0, egui::Button::new("刪除此子圖")).on_disabled_hover_text("仍有節點使用").clicked() {
                self.editing = None;
                let root = self.parked.remove(&None).expect("root parked while editing subgraph");
                self.snarl = root.snarl;
                self.history = root.history;
                self.status = format!("已刪除子圖「{cur}」");
            }
            ui.add_space(6.0);
            ui.weak("Group In / Out 是子圖的介面。Loop 時「輸入 1」接上一次的結果，Loop Index 提供次數。");
        }
    }

    fn rename_subgraph(&mut self, old: &str) {
        let new = self.rename_buf.trim().to_string();
        if new.is_empty() || new == old {
            return;
        }
        if self.subgraph_names().contains(&new) {
            self.status = format!("名稱「{new}」已存在");
            return;
        }
        self.for_each_snarl(|s| {
            for d in s.nodes_mut() {
                if instance_name(d) == Some(old) {
                    d.params.insert("name".into(), Value::Text(new.clone()));
                }
            }
        });
        self.editing = Some(new.clone());
        self.status = format!("已改名為「{new}」");
    }

    /// 把選取的節點收合成新子圖，原處換成 Subgraph 節點（一步 Undo 可還原）。
    fn collapse_selection(&mut self, ctx: &egui::Context) -> Result<String, String> {
        let sel: HashSet<NodeId> =
            get_selected_nodes(graph_id(), ctx).into_iter().filter(|id| self.snarl.get_node(*id).is_some()).collect();
        if sel.is_empty() {
            return Err("請先選取節點（Shift+點選，或 Shift+拖曳框選）".into());
        }
        if sel.iter().any(|id| matches!(self.snarl[*id].kind.as_str(), "output" | "group_in" | "group_out")) {
            return Err("Output / Group In / Group Out 不能收進子圖".into());
        }
        let wires: Vec<(OutPinId, InPinId)> = self.snarl.wires().collect();
        let incoming: Vec<_> = wires.iter().filter(|(o, i)| sel.contains(&i.node) && !sel.contains(&o.node)).collect();
        let outgoing: Vec<_> = wires.iter().filter(|(o, i)| sel.contains(&o.node) && !sel.contains(&i.node)).collect();
        for (_, i) in &incoming {
            let def = &self.defs[&self.snarl[i.node].kind];
            if i.input >= def.inputs.len() {
                return Err("有參數連線跨越邊界，目前子圖只支援 Surface 輸入".into());
            }
        }
        let mut ext_sources: Vec<OutPinId> = Vec::new();
        for (o, _) in &incoming {
            if !ext_sources.contains(o) {
                ext_sources.push(*o);
            }
        }
        let mut out_sources: Vec<OutPinId> = Vec::new();
        for (o, _) in &outgoing {
            if !out_sources.contains(o) {
                out_sources.push(*o);
            }
        }
        if ext_sources.len() > 2 || out_sources.len() > 1 {
            return Err(format!(
                "子圖最多 2 個外部輸入、1 個輸出（目前 {} 個輸入、{} 個輸出）",
                ext_sources.len(),
                out_sources.len()
            ));
        }

        // 1. 建子圖內容
        let name = self.unique_name("子圖");
        let mut sub = Snarl::new();
        let mut map = BTreeMap::new();
        let (mut minx, mut maxx, mut sumy) = (f32::MAX, f32::MIN, 0.0);
        for &id in &sel {
            let info = self.snarl.get_node_info(id).expect("selected node exists");
            minx = minx.min(info.pos.x);
            maxx = maxx.max(info.pos.x);
            sumy += info.pos.y;
            map.insert(id, sub.insert_node(info.pos, info.value.clone()));
        }
        let cy = sumy / sel.len() as f32;
        for (o, i) in &wires {
            if let (Some(&a), Some(&b)) = (map.get(&o.node), map.get(&i.node)) {
                sub.connect(OutPinId { node: a, output: o.output }, InPinId { node: b, input: i.input });
            }
        }
        for (k, src) in ext_sources.iter().enumerate() {
            let mut gi = NodeData::new(pixelnode_nodes::find("group_in").unwrap());
            gi.params.insert("index".into(), Value::Enum(k as u32));
            let gid = sub.insert_node(egui::pos2(minx - 320.0, cy + k as f32 * 120.0), gi);
            for (o, i) in &incoming {
                if o == src {
                    sub.connect(OutPinId { node: gid, output: 0 }, InPinId { node: map[&i.node], input: i.input });
                }
            }
        }
        let go = sub.insert_node(egui::pos2(maxx + 360.0, cy), NodeData::new(pixelnode_nodes::find("group_out").unwrap()));
        if let Some(o) = out_sources.first() {
            sub.connect(OutPinId { node: map[&o.node], output: o.output }, InPinId { node: go, input: 0 });
        }

        // 2. 原圖：移除選取、放上 Subgraph 節點並接線（記成一個 Batch）
        let mut batch: Vec<Command> = wires
            .iter()
            .filter(|(o, i)| sel.contains(&o.node) || sel.contains(&i.node))
            .map(|&(o, i)| Command::Disconnect(o, i))
            .collect();
        for &id in &sel {
            let pos = self.snarl.get_node_info(id).unwrap().pos;
            let data = self.snarl.remove_node(id);
            batch.push(Command::RemoveNode { id, pos, data });
        }
        let mut inst = NodeData::new(pixelnode_nodes::find("subgraph").unwrap());
        inst.params.insert("name".into(), Value::Text(name.clone()));
        let pos = egui::pos2((minx + maxx) * 0.5, cy);
        let iid = self.snarl.insert_node(pos, inst.clone());
        batch.push(Command::AddNode { id: iid, pos, data: inst });
        for (k, src) in ext_sources.iter().enumerate() {
            let to = InPinId { node: iid, input: k };
            self.snarl.connect(*src, to);
            batch.push(Command::Connect(*src, to));
        }
        if !out_sources.is_empty() {
            let from = OutPinId { node: iid, output: 0 };
            for (_, i) in &outgoing {
                self.snarl.connect(from, *i);
                batch.push(Command::Connect(from, *i));
            }
        }
        self.history.record(Command::Batch(batch));
        self.parked.insert(Some(name.clone()), Parked { snarl: sub, history: History::default() });
        Ok(name)
    }
}

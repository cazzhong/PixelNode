//! Undo/Redo：Command pattern，只記 diff。

use crate::NodeData;
use eframe::egui;
use egui_snarl::{InPinId, NodeId, OutPinId, Snarl};
use pixelnode_graph::Value;

#[derive(Debug, Clone)]
pub enum Command {
    AddNode { id: NodeId, pos: egui::Pos2, data: NodeData },
    RemoveNode { id: NodeId, pos: egui::Pos2, data: NodeData },
    Connect(OutPinId, InPinId),
    Disconnect(OutPinId, InPinId),
    SetParam { node: NodeId, name: String, old: Value, new: Value },
    Batch(Vec<Command>),
}

impl Command {
    fn inverse(&self) -> Command {
        match self.clone() {
            Command::AddNode { id, pos, data } => Command::RemoveNode { id, pos, data },
            Command::RemoveNode { id, pos, data } => Command::AddNode { id, pos, data },
            Command::Connect(o, i) => Command::Disconnect(o, i),
            Command::Disconnect(o, i) => Command::Connect(o, i),
            Command::SetParam { node, name, old, new } => Command::SetParam { node, name, old: new, new: old },
            Command::Batch(v) => Command::Batch(v.iter().rev().map(Command::inverse).collect()),
        }
    }

    /// 套用到 snarl。重新插入的節點若拿到不同 id，回傳 (舊, 新) 以便修正歷史。
    fn apply(&self, snarl: &mut Snarl<NodeData>, remaps: &mut Vec<(NodeId, NodeId)>) {
        let map = |id: NodeId, remaps: &Vec<(NodeId, NodeId)>| {
            remaps.iter().rev().find(|(o, _)| *o == id).map_or(id, |(_, n)| *n)
        };
        match self {
            Command::AddNode { id, pos, data } => {
                let new = snarl.insert_node(*pos, data.clone());
                if new != *id {
                    remaps.push((*id, new));
                }
            }
            Command::RemoveNode { id, .. } => {
                snarl.remove_node(map(*id, remaps));
            }
            Command::Connect(o, i) => {
                let o = OutPinId { node: map(o.node, remaps), output: o.output };
                let i = InPinId { node: map(i.node, remaps), input: i.input };
                snarl.connect(o, i);
            }
            Command::Disconnect(o, i) => {
                let o = OutPinId { node: map(o.node, remaps), output: o.output };
                let i = InPinId { node: map(i.node, remaps), input: i.input };
                snarl.disconnect(o, i);
            }
            Command::SetParam { node, name, new, .. } => {
                if let Some(n) = snarl.get_node_mut(map(*node, remaps)) {
                    n.params.insert(name.clone(), new.clone());
                }
            }
            Command::Batch(v) => v.iter().for_each(|c| c.apply(snarl, remaps)),
        }
    }

    fn remap(&mut self, from: NodeId, to: NodeId) {
        let f = |id: &mut NodeId| {
            if *id == from {
                *id = to;
            }
        };
        match self {
            Command::AddNode { id, .. } | Command::RemoveNode { id, .. } => f(id),
            Command::Connect(o, i) | Command::Disconnect(o, i) => {
                f(&mut o.node);
                f(&mut i.node);
            }
            Command::SetParam { node, .. } => f(node),
            Command::Batch(v) => v.iter_mut().for_each(|c| c.remap(from, to)),
        }
    }
}

#[derive(Default)]
pub struct History {
    undo: Vec<Command>,
    redo: Vec<Command>,
}

impl History {
    /// 記錄一個「已經套用」的命令。連續調整同一參數會合併成一步。
    pub fn record(&mut self, cmd: Command) {
        self.redo.clear();
        if let (
            Some(Command::SetParam { node: n0, name: a, new: last, .. }),
            Command::SetParam { node: n1, name: b, new, .. },
        ) = (self.undo.last_mut(), &cmd)
            && n0 == n1
            && a == b
        {
            *last = new.clone();
            return;
        }
        self.undo.push(cmd);
    }

    /// 結束參數合併（例如開始編輯另一個東西時）。
    pub fn seal(&mut self) {
        if matches!(self.undo.last(), Some(Command::SetParam { .. })) {
            self.undo.push(Command::Batch(vec![]));
        }
    }

    pub fn undo(&mut self, snarl: &mut Snarl<NodeData>) -> bool {
        self.step(snarl, true)
    }

    pub fn redo(&mut self, snarl: &mut Snarl<NodeData>) -> bool {
        self.step(snarl, false)
    }

    fn step(&mut self, snarl: &mut Snarl<NodeData>, undo: bool) -> bool {
        loop {
            let (src, dst) = if undo { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
            let Some(cmd) = src.pop() else { return false };
            let empty = matches!(&cmd, Command::Batch(v) if v.is_empty());
            let run = if undo { cmd.inverse() } else { cmd.clone() };
            let mut remaps = Vec::new();
            run.apply(snarl, &mut remaps);
            dst.push(cmd);
            for (from, to) in remaps {
                self.undo.iter_mut().chain(self.redo.iter_mut()).for_each(|c| c.remap(from, to));
            }
            if !empty {
                return true;
            }
        }
    }

    pub fn can_undo(&self) -> bool {
        self.undo.iter().any(|c| !matches!(c, Command::Batch(v) if v.is_empty()))
    }

    pub fn can_redo(&self) -> bool {
        self.redo.iter().any(|c| !matches!(c, Command::Batch(v) if v.is_empty()))
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(kind: &str) -> NodeData {
        NodeData::new(pixelnode_nodes::find(kind).unwrap())
    }

    #[test]
    fn undo_redo_add_connect_param_remove() {
        let mut s = Snarl::new();
        let mut h = History::default();
        let a = s.insert_node(egui::pos2(0.0, 0.0), data("solid"));
        h.record(Command::AddNode { id: a, pos: egui::pos2(0.0, 0.0), data: data("solid") });
        let b = s.insert_node(egui::pos2(100.0, 0.0), data("output"));
        h.record(Command::AddNode { id: b, pos: egui::pos2(100.0, 0.0), data: data("output") });
        let (o, i) = (OutPinId { node: a, output: 0 }, InPinId { node: b, input: 0 });
        s.connect(o, i);
        h.record(Command::Connect(o, i));
        for v in [0.1, 0.2, 0.3] {
            let old = s[a].params["color"].clone();
            let new = Value::Color([v, 0.0, 0.0, 1.0]);
            s[a].params.insert("color".into(), new.clone());
            h.record(Command::SetParam { node: a, name: "color".into(), old, new });
        }
        // 移除 a（含連線）
        let wires: Vec<_> = s.wires().filter(|(o, _)| o.node == a).collect();
        let pos = s.get_node_info(a).unwrap().pos;
        let d = s.remove_node(a);
        let mut batch: Vec<_> = wires.iter().map(|&(o, i)| Command::Disconnect(o, i)).collect();
        batch.push(Command::RemoveNode { id: a, pos, data: d });
        h.record(Command::Batch(batch));

        assert!(h.undo(&mut s)); // 節點與連線一起回來
        assert!(s.get_node(a).is_some());
        assert_eq!(s.wires().count(), 1);
        assert!(h.undo(&mut s)); // 參數三次合併成一步
        assert_eq!(s[a].params["color"], data("solid").params["color"]);
        assert!(h.undo(&mut s));
        assert_eq!(s.wires().count(), 0);
        assert!(h.redo(&mut s));
        assert!(h.redo(&mut s));
        assert_eq!(s[a].params["color"], Value::Color([0.3, 0.0, 0.0, 1.0]));
        assert_eq!(s.wires().count(), 1);
    }
}

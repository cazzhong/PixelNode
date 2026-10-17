//! 時間軸面板：播放控制、影格條（含 keyframe 標記）、匯出。

use crate::{App, KEY_ON, graph_id};
use eframe::egui;
use egui_snarl::ui::get_selected_nodes;
use pixelnode_runtime::export::ExportKind;

impl App {
    pub(crate) fn timeline_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let n = self.timeline.frames.max(1);
            if ui.button("⏮").on_hover_text("第一格").clicked() {
                self.frame = 0;
            }
            if ui.button("◀").on_hover_text("上一格 (←)").clicked() {
                self.frame = (self.frame + n - 1) % n;
            }
            let play = if self.playing { "⏸" } else { "▶" };
            if ui.button(play).on_hover_text("播放 / 暫停 (空白鍵)").clicked() {
                self.playing = !self.playing;
                self.play_clock = 0.0;
            }
            if ui.button("▶|").on_hover_text("下一格 (→)").clicked() {
                self.frame = (self.frame + 1) % n;
            }
            ui.label(format!("影格 {:>3} / {n}", self.frame + 1));
            ui.separator();
            ui.label("影格數");
            ui.add(egui::DragValue::new(&mut self.timeline.frames).range(1..=999));
            ui.label("FPS");
            ui.add(egui::DragValue::new(&mut self.timeline.fps).range(1..=60));
            self.frame = self.frame.min(self.timeline.frames.max(1) - 1);

            ui.separator();
            ui.label("匯出");
            ui.add(egui::DragValue::new(&mut self.export_scale).range(1..=16).suffix("×"));
            ui.add(egui::TextEdit::singleline(&mut self.export_dir).desired_width(120.0));
            ui.add_enabled_ui(!self.exporting, |ui| {
                if ui.button("GIF").clicked() {
                    self.export(ExportKind::Gif);
                }
                if ui.button("PNG 序列").clicked() {
                    self.export(ExportKind::PngSequence);
                }
                if ui.button("Sprite Sheet").clicked() {
                    self.export(ExportKind::SpriteSheet { columns: 0 });
                }
            });
        });
        ui.add_space(2.0);
        self.frame_bar(ui);
        ui.add_space(4.0);
    }

    /// 影格條：點擊 / 拖曳跳格；◆ 標示 keyframe（有選取節點時只顯示選取者的）。
    fn frame_bar(&mut self, ui: &mut egui::Ui) {
        let n = self.timeline.frames.max(1);
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::click_and_drag());
        let cell = rect.width() / n as f32;
        if let Some(pos) = resp.interact_pointer_pos() {
            self.frame = (((pos.x - rect.left()) / cell).floor().max(0.0) as u32).min(n - 1);
        }

        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        painter.rect_filled(rect, 3.0, visuals.extreme_bg_color);
        let tick = visuals.weak_text_color().gamma_multiply(0.4);
        let label_every = ((40.0 / cell).ceil() as u32).max(1);
        for f in 0..n {
            let x = rect.left() + f as f32 * cell;
            if f > 0 {
                painter.vline(x, rect.y_range(), egui::Stroke::new(1.0, tick));
            }
            if f % label_every == 0 && cell * label_every as f32 >= 18.0 {
                painter.text(
                    egui::pos2(x + 2.0, rect.top() + 1.0),
                    egui::Align2::LEFT_TOP,
                    (f + 1).to_string(),
                    egui::FontId::monospace(9.0),
                    visuals.weak_text_color(),
                );
            }
        }

        // 播放頭
        let head = egui::Rect::from_min_size(
            egui::pos2(rect.left() + self.frame as f32 * cell, rect.top()),
            egui::vec2(cell.max(2.0), rect.height()),
        );
        painter.rect_filled(head, 0.0, visuals.selection.bg_fill.gamma_multiply(0.6));

        // keyframe 標記
        let selected = get_selected_nodes(graph_id(), ui.ctx());
        let mut frames: Vec<u32> = self
            .snarl
            .node_ids()
            .filter(|(id, _)| selected.is_empty() || selected.contains(id))
            .flat_map(|(_, d)| d.anim.values().flat_map(|t| t.keys.iter().map(|k| k.frame)))
            .filter(|&f| f < n)
            .collect();
        frames.sort_unstable();
        frames.dedup();
        let y = rect.bottom() - 9.0;
        for f in frames {
            let c = egui::pos2(rect.left() + (f as f32 + 0.5) * cell, y);
            let r = 4.5;
            painter.add(egui::Shape::convex_polygon(
                vec![c + egui::vec2(0.0, -r), c + egui::vec2(r, 0.0), c + egui::vec2(0.0, r), c + egui::vec2(-r, 0.0)],
                KEY_ON,
                egui::Stroke::new(1.0, egui::Color32::BLACK),
            ));
        }
    }
}

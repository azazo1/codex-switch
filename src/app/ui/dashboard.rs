use super::CodexSwitchApp;
use eframe::egui::{self, Color32};
use std::time::{Duration, Instant};

const COPY_FEEDBACK_DURATION: Duration = Duration::from_secs(2);

impl CodexSwitchApp {
    pub(super) fn dashboard_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Codex Switch");
        ui.horizontal(|ui| {
            ui.label("监听地址");
            ui.text_edit_singleline(&mut self.bind_addr);
            if self.server.is_none() {
                if ui.button("启动").clicked() {
                    self.start_server();
                }
            } else if ui.button("停止").clicked() {
                self.stop_server();
            }
        });
        ui.label(format!("Base URL: http://{}/v1", self.bind_addr));
        ui.horizontal(|ui| {
            ui.label("本地访问 key");
            if ui.button(&self.local_key).on_hover_text("点击复制").clicked() {
                ui.ctx().copy_text(self.local_key.clone());
                self.local_key_copied_at = Some(Instant::now());
            }
            if ui.button("刷新 key").on_hover_text("生成新的本地访问 key").clicked() {
                self.refresh_local_key();
            }
            if let Some(copied_at) = self.local_key_copied_at {
                let elapsed = copied_at.elapsed();
                if elapsed < COPY_FEEDBACK_DURATION {
                    draw_copy_success_icon(ui);
                    ui.colored_label(copy_success_color(), "已复制");
                    ui.ctx()
                        .request_repaint_after(COPY_FEEDBACK_DURATION - elapsed);
                } else {
                    self.local_key_copied_at = None;
                }
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(format!("总请求: {}", self.stats.total_requests));
            ui.label(format!("总 token: {}", self.stats.total_tokens));
            ui.label(format!("今日请求: {}", self.stats.today_requests));
            ui.label(format!("今日 token: {}", self.stats.today_tokens));
        });
        ui.separator();
        ui.heading("按上游统计");
        for item in &self.provider_stats {
            ui.label(format!(
                "{} ({}) - 请求 {} - token {}",
                item.upstream_name, item.upstream_id, item.requests, item.total_tokens
            ));
        }
    }
}

fn draw_copy_success_icon(ui: &mut egui::Ui) {
    let size = egui::vec2(14.0, 14.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let stroke = egui::Stroke::new(2.0, copy_success_color());
    let p1 = egui::pos2(rect.left() + 2.0, rect.center().y + 0.5);
    let p2 = egui::pos2(rect.left() + 5.5, rect.bottom() - 3.0);
    let p3 = egui::pos2(rect.right() - 2.0, rect.top() + 3.0);
    ui.painter().line_segment([p1, p2], stroke);
    ui.painter().line_segment([p2, p3], stroke);
}

fn copy_success_color() -> Color32 {
    Color32::from_rgb(34, 197, 94)
}

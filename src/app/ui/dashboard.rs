use super::CodexSwitchApp;
use eframe::egui;

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
        ui.label(format!("本地访问 key: {}", self.local_key));
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

use super::CodexSwitchApp;
use eframe::egui;

impl CodexSwitchApp {
    pub(super) fn logs_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("最近请求");
        egui::ScrollArea::vertical().show(ui, |ui| {
            for log in &self.logs {
                ui.label(format!(
                    "{} {} status={} tokens={} {}",
                    log.upstream_name.clone().unwrap_or_default(),
                    log.endpoint,
                    log.status,
                    log.usage.total_tokens,
                    log.error.clone().unwrap_or_default()
                ));
            }
        });
    }
}

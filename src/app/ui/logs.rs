use super::{CodexSwitchApp, tokens};
use eframe::egui;

impl CodexSwitchApp {
    pub(super) fn logs_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("最近请求");
        let mut token_display_mode = self.token_display_mode;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (index, log) in self.logs.iter().enumerate() {
                ui.group(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(log.upstream_name.clone().unwrap_or_default());
                        ui.label(&log.endpoint);
                        if let Some(model) = &log.model {
                            ui.label(format!("model={model}"));
                        }
                        ui.label(format!("status={}", log.status));
                    });
                    ui.horizontal_wrapped(|ui| {
                        tokens::usage_tokens(ui, &mut token_display_mode, &log.usage);
                        tokens::estimated_cost(
                            ui,
                            "估算",
                            self.log_estimated_cost_usd.get(index).copied().flatten(),
                        );
                        ui.label(format!("耗时 {} ms", log.duration_ms));
                        if let Some(first_token_ms) = log.first_token_ms {
                            ui.label(format!("首 token {} ms", first_token_ms));
                        }
                    });
                    if let Some(error) = &log.error
                        && !error.is_empty()
                    {
                        ui.label(error);
                    }
                });
            }
        });
        self.token_display_mode = token_display_mode;
    }
}

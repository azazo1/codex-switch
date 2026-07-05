use super::CodexSwitchApp;
use crate::core::models::UpstreamKind;
use eframe::egui;

impl CodexSwitchApp {
    pub(super) fn oauth_quota_ui(&mut self, ui: &mut egui::Ui) {
        let upstreams: Vec<_> = self
            .upstreams
            .clone()
            .into_iter()
            .filter(|upstream| upstream.kind == UpstreamKind::CodexOauth)
            .collect();
        if upstreams.is_empty() {
            return;
        }
        ui.separator();
        ui.heading("Codex 额度");
        for upstream in upstreams {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("{} [{}]", upstream.name, upstream.kind.as_str()));
                    if ui
                        .add_enabled(
                            !self.quota_query_pending,
                            egui::Button::new("查 Codex 额度"),
                        )
                        .clicked()
                    {
                        self.query_selected_quota(&upstream.id);
                    }
                });
                if let Some((_, Some(snapshot))) = self
                    .quota_snapshots
                    .iter()
                    .find(|(id, _)| id == &upstream.id)
                {
                    ui.label(format!(
                        "5h: {}%, 7d: {}%",
                        fmt_percent(snapshot.used_5h_percent),
                        fmt_percent(snapshot.used_7d_percent)
                    ));
                }
            });
        }
    }
}

fn fmt_percent(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.1}"))
        .unwrap_or_else(|| "未知".to_string())
}

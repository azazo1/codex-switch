use super::CodexSwitchApp;
use crate::core::models::UpstreamKind;
use eframe::egui;

impl CodexSwitchApp {
    pub(super) fn quota_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("额度和余额");
        let upstreams = self.upstreams.clone();
        for upstream in upstreams {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("{} [{}]", upstream.name, upstream.kind.as_str()));
                    if upstream.kind == UpstreamKind::CodexOauth
                        && ui.button("查 Codex 额度").clicked()
                    {
                        self.query_selected_quota(&upstream.id);
                    }
                    if upstream.kind == UpstreamKind::RelayApiKey && ui.button("查余额").clicked() {
                        self.query_selected_balance(&upstream.id);
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
                if let Some((_, Some(snapshot))) = self
                    .balance_snapshots
                    .iter()
                    .find(|(id, _)| id == &upstream.id)
                {
                    ui.label(format!(
                        "余额: {} {}",
                        snapshot
                            .remaining
                            .map(|v| format!("{v:.4}"))
                            .unwrap_or_else(|| "未知".to_string()),
                        snapshot.unit.clone().unwrap_or_default()
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

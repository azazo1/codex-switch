use super::CodexSwitchApp;
use crate::core::models::{BalanceSnapshot, UpstreamKind, WireApi};
use eframe::egui;

impl CodexSwitchApp {
    pub(super) fn upstreams_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("添加 API Key 上游");
        ui.horizontal(|ui| {
            ui.label("名称");
            ui.text_edit_singleline(&mut self.relay_name);
        });
        ui.horizontal(|ui| {
            ui.label("Base URL");
            ui.text_edit_singleline(&mut self.relay_base_url);
        });
        ui.horizontal(|ui| {
            ui.label("API Key");
            ui.add(egui::TextEdit::singleline(&mut self.relay_api_key).password(true));
        });
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.relay_wire_api, WireApi::Responses, "Responses");
            ui.radio_value(
                &mut self.relay_wire_api,
                WireApi::ChatCompletions,
                "Chat Completions",
            );
            ui.checkbox(&mut self.relay_supports_compact, "支持 compact");
            if ui.button("添加").clicked() {
                self.add_relay();
            }
        });
        ui.separator();
        ui.heading("Codex OAuth");
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.oauth_start_pending,
                    egui::Button::new("开始登录"),
                )
                .clicked()
            {
                self.start_oauth();
            }
            if ui
                .add_enabled(
                    self.oauth_device.is_some() && !self.oauth_poll_pending,
                    egui::Button::new("轮询授权"),
                )
                .clicked()
            {
                self.poll_oauth();
            }
        });
        if let Some(device) = &self.oauth_device {
            ui.label(format!("访问: {}", device.verification_uri));
            ui.label(format!("用户码: {}", device.user_code));
            ui.label(format!(
                "轮询间隔: {} 秒, 有效期: {} 秒",
                device.interval, device.expires_in
            ));
        }
        self.oauth_quota_ui(ui);
        ui.separator();
        ui.heading("上游列表");
        let upstreams = self.upstreams.clone();
        let balance_snapshots = self.balance_snapshots.clone();
        let mut changed = Vec::new();
        let mut deleted = Vec::new();
        let mut edit = None;
        let mut query_balance = None;
        egui::Grid::new("upstreams_grid")
            .striped(true)
            .num_columns(6)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                ui.strong("启用");
                ui.strong("名称");
                ui.strong("类型");
                ui.strong("Base URL");
                ui.strong("余额");
                ui.strong("操作");
                ui.end_row();

                for upstream in &upstreams {
                    let mut enabled = upstream.enabled;
                    if ui.checkbox(&mut enabled, "").changed() {
                        changed.push((upstream.id.clone(), enabled));
                    }
                    ui.label(&upstream.name)
                        .on_hover_text(format!("id: {}", upstream.id));
                    ui.label(upstream.kind.as_str());
                    ui.label(upstream.base_url.as_str());
                    if upstream.kind == UpstreamKind::RelayApiKey {
                        let snapshot = balance_snapshots
                            .iter()
                            .find(|(id, _)| id == &upstream.id)
                            .and_then(|(_, snapshot)| snapshot.as_ref());
                        let (balance_text, balance_detail) = format_balance_snapshot(snapshot);
                        let response = ui.label(balance_text);
                        if let Some(detail) = balance_detail {
                            response.on_hover_text(detail);
                        }
                    } else {
                        ui.label("-");
                    }
                    ui.horizontal(|ui| {
                        if upstream.kind == UpstreamKind::RelayApiKey
                            && ui
                                .add_enabled(
                                    !self.balance_query_pending,
                                    egui::Button::new("查余额"),
                                )
                                .clicked()
                        {
                            query_balance = Some(upstream.id.clone());
                        }
                        if ui.button("编辑").clicked() {
                            edit = Some(upstream.clone());
                        }
                        if ui.button("删除").clicked() {
                            deleted.push(upstream.id.clone());
                        }
                    });
                    ui.end_row();
                }
            });
        if let Some(upstream) = edit {
            self.open_upstream_editor(upstream);
        }
        if let Some(id) = query_balance {
            self.query_selected_balance(&id);
        }
        let should_refresh = !deleted.is_empty() || !changed.is_empty();
        for (id, enabled) in changed {
            if let Err(err) = self
                .runtime
                .block_on(self.state.store.set_upstream_enabled(&id, enabled))
            {
                self.status = format!("更新启用状态失败: {err}");
            }
        }
        for id in deleted {
            if let Err(err) = self.runtime.block_on(self.state.store.delete_upstream(&id)) {
                self.status = format!("删除失败: {err}");
            }
        }
        if should_refresh {
            self.refresh_all();
        }
        self.show_upstream_editor(ui.ctx());
    }
}

fn format_balance_snapshot(snapshot: Option<&BalanceSnapshot>) -> (String, Option<String>) {
    let Some(snapshot) = snapshot else {
        return ("未查询".to_string(), None);
    };
    if !snapshot.is_valid {
        return (
            "失败".to_string(),
            snapshot
                .message
                .as_deref()
                .map(|message| format!("失败: {message}")),
        );
    }
    let amount = snapshot
        .remaining
        .map(|value| format!("{value:.4}"))
        .unwrap_or_else(|| "未知".to_string());
    let unit = snapshot.unit.as_deref().unwrap_or("");
    if unit.is_empty() {
        (amount, None)
    } else {
        (format!("{amount} {unit}"), None)
    }
}

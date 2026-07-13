use super::{CodexSwitchApp, DeleteAction};
use crate::core::models::{
    BalanceSnapshot, CacheKeepaliveMode, UpstreamCacheKeepaliveSettings, UpstreamKind, WireApi,
    UpstreamBalanceAlertSettings,
};
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
            ui.label("代理 URL");
            ui.add(
                egui::TextEdit::singleline(&mut self.relay_proxy_url)
                    .hint_text("留空使用系统代理"),
            );
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
        self.oauth_accounts_ui(ui);
        ui.separator();
        ui.heading("上游列表");
        let upstreams = self.upstreams.clone();
        let balance_snapshots = self.balance_snapshots.clone();
        let cache_settings = self.cache_keepalive_settings.clone();
        let balance_alert_settings = self.balance_alert_settings.clone();
        let mut changed = Vec::new();
        let mut delete_requested = None;
        let mut edit = None;
        let mut query_balance = None;
        egui::ScrollArea::vertical()
            .id_salt("upstreams_list")
            .max_height(ui.available_height())
            .show(ui, |ui| {
                egui::Grid::new("upstreams_grid")
                    .striped(true)
                    .num_columns(7)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        ui.strong("启用");
                        ui.strong("名称");
                        ui.strong("Base URL");
                        ui.strong("缓存保持");
                        ui.strong("余额");
                        ui.strong("余额提醒");
                        ui.strong("操作");
                        ui.end_row();

                        for upstream in &upstreams {
                            let mut enabled = upstream.enabled;
                            if ui.checkbox(&mut enabled, "").changed() {
                                changed.push((upstream.id.clone(), enabled));
                            }
                            ui.label(&upstream.name)
                                .on_hover_text(format!("id: {}", upstream.id));
                            ui.label(upstream.base_url.as_str());
                            cache_keepalive_label(ui, cache_settings.get(&upstream.id));
                            if upstream.kind == UpstreamKind::RelayApiKey {
                                balance_snapshot_label(
                                    ui,
                                    balance_snapshot_for(&balance_snapshots, &upstream.id),
                                );
                            } else {
                                ui.label("-");
                            }
                            if upstream.kind == UpstreamKind::RelayApiKey {
                                balance_alert_label(ui, balance_alert_settings.get(&upstream.id));
                            } else {
                                ui.label("-");
                            }
                            ui.horizontal(|ui| {
                                if upstream.kind == UpstreamKind::RelayApiKey
                                    && ui
                                        .add_enabled(
                                            !self.balance_query_pending_ids.contains(&upstream.id),
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
                                    delete_requested = Some(upstream.clone());
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
        if let Some(upstream) = edit {
            self.open_upstream_editor(upstream);
        }
        if let Some(id) = query_balance {
            self.query_selected_balance(&id);
        }
        if let Some(upstream) = delete_requested {
            self.request_delete(
                DeleteAction::Upstream(upstream.id),
                "删除上游",
                format!("确认删除上游 \"{}\"? 此操作无法撤销.", upstream.name),
            );
        }
        let should_refresh = !changed.is_empty();
        for (id, enabled) in changed {
            if let Err(err) = self
                .runtime
                .block_on(self.state.store.set_upstream_enabled(&id, enabled))
            {
                self.status = format!("更新启用状态失败: {err}");
            }
        }
        if should_refresh {
            self.refresh_all();
        }
        self.show_upstream_editor(ui.ctx());
    }

    pub(super) fn delete_upstream(&mut self, id: &str) {
        match self.runtime.block_on(self.state.store.delete_upstream(id)) {
            Ok(()) => {
                self.status = "上游已删除".to_string();
                self.refresh_all();
            }
            Err(err) => self.status = format!("删除上游失败: {err}"),
        }
    }
}

fn balance_alert_label(
    ui: &mut egui::Ui,
    settings: Option<&UpstreamBalanceAlertSettings>,
) {
    let Some(settings) = settings else {
        ui.label("关闭");
        return;
    };
    if !settings.enabled {
        ui.label("关闭");
        return;
    }
    let response = if settings.alert_active {
        ui.colored_label(egui::Color32::RED, format!("不足 <= {:.4}", settings.threshold))
    } else {
        ui.label(format!("<= {:.4}", settings.threshold))
    };
    response.on_hover_text(format!("每 {} 秒检查一次", settings.interval_seconds));
}

pub(super) fn cache_keepalive_label(
    ui: &mut egui::Ui,
    settings: Option<&UpstreamCacheKeepaliveSettings>,
) {
    let Some(settings) = settings else {
        ui.label("关闭");
        return;
    };
    if !settings.enabled || settings.mode == CacheKeepaliveMode::Off {
        ui.label("关闭");
        return;
    }
    ui.label(format!(
        "{} / {} 秒",
        settings.mode.as_str(),
        settings.interval_seconds
    ))
    .on_hover_text(format!(
        "最大空闲 {} 秒, 最小缓存 {} tokens, 最大缓存 {} tokens, 最大会话 {}",
        settings.max_idle_seconds,
        settings.min_cacheable_tokens,
        settings.max_cacheable_tokens,
        settings.max_active_sessions
    ));
}

pub(super) fn balance_snapshot_for<'a>(
    snapshots: &'a [(String, Option<BalanceSnapshot>)],
    upstream_id: &str,
) -> Option<&'a BalanceSnapshot> {
    snapshots
        .iter()
        .find(|(id, _)| id == upstream_id)
        .and_then(|(_, snapshot)| snapshot.as_ref())
}

pub(super) fn balance_snapshot_label(ui: &mut egui::Ui, snapshot: Option<&BalanceSnapshot>) {
    let (balance_text, balance_detail) = format_balance_snapshot(snapshot);
    let response = ui.label(balance_text);
    if let Some(detail) = balance_detail {
        response.on_hover_text(detail);
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

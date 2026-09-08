use super::{CodexSwitchApp, DeleteAction};
use crate::core::models::{
    ApiKeyAuthScheme, BalanceSnapshot, CacheKeepaliveMode, UpstreamBalanceAlertSettings,
    UpstreamCacheKeepaliveSettings, UpstreamKind, WireApi,
};
use crate::core::upstream_detection::{self, DetectedKind};
use crate::core::upstream_transfer::UpstreamExport;
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
            let response = ui.text_edit_singleline(&mut self.relay_base_url);
            if response.changed() {
                self.apply_relay_detection_hint();
            }
            let detected = upstream_detection::detect_upstream(&self.relay_base_url);
            if detected.kind != DetectedKind::Unknown {
                ui.label(format!("识别: {}", detected.kind.label()))
                    .on_hover_text(
                        "依据 Base URL 在本地判断, 不发起任何请求; 结果只作为默认值, 可以随时手改.",
                    );
            }
            if let Some(hint) = detected.base_url_hint(&self.relay_base_url)
                && ui
                    .button(format!("补全为 {hint}"))
                    .on_hover_text("当前地址下没有模型列表端点, 点一下补全路径.")
                    .clicked()
            {
                self.relay_base_url = hint.to_string();
                self.apply_relay_detection_hint();
            }
        });
        ui.horizontal(|ui| {
            ui.label("代理 URL");
            ui.add(
                egui::TextEdit::singleline(&mut self.relay_proxy_url).hint_text("留空使用系统代理"),
            );
        });
        ui.horizontal(|ui| {
            ui.label("API Key");
            ui.add(egui::TextEdit::singleline(&mut self.relay_api_key).password(true));
        });
        ui.horizontal(|ui| {
            ui.label("Wire API");
            if ui
                .radio_value(&mut self.relay_wire_api, WireApi::Responses, "Responses")
                .clicked()
            {
                self.relay_api_key_auth_scheme = ApiKeyAuthScheme::Bearer;
                self.relay_wire_api_touched = true;
                self.relay_auth_touched = true;
            }
            if ui
                .radio_value(
                    &mut self.relay_wire_api,
                    WireApi::ChatCompletions,
                    "Chat Completions",
                )
                .clicked()
            {
                self.relay_api_key_auth_scheme = ApiKeyAuthScheme::Bearer;
                self.relay_wire_api_touched = true;
                self.relay_auth_touched = true;
            }
            if ui
                .radio_value(
                    &mut self.relay_wire_api,
                    WireApi::AnthropicMessages,
                    "Anthropic Messages",
                )
                .clicked()
            {
                self.relay_api_key_auth_scheme = ApiKeyAuthScheme::XApiKey;
                self.relay_supports_compact = false;
                self.relay_wire_api_touched = true;
                self.relay_auth_touched = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("API Key 认证");
            if ui
                .radio_value(
                    &mut self.relay_api_key_auth_scheme,
                    ApiKeyAuthScheme::Bearer,
                    "Bearer",
                )
                .clicked()
            {
                self.relay_auth_touched = true;
            }
            if ui
                .radio_value(
                    &mut self.relay_api_key_auth_scheme,
                    ApiKeyAuthScheme::XApiKey,
                    "x-api-key",
                )
                .clicked()
            {
                self.relay_auth_touched = true;
            }
            ui.add_enabled_ui(self.relay_wire_api != WireApi::AnthropicMessages, |ui| {
                ui.checkbox(&mut self.relay_supports_compact, "支持 compact");
            });
            ui.add_enabled_ui(self.relay_wire_api == WireApi::ChatCompletions, |ui| {
                if ui
                    .checkbox(
                        &mut self.relay_filter_chat_server_tools,
                        "过滤 server_tool",
                    )
                    .on_hover_text(
                        "丢弃 web_search 等非 function 工具, 适合 OpenCode 等仅支持 function 的 Chat 上游.",
                    )
                    .changed()
                {
                    self.relay_filter_touched = true;
                }
            });
            if ui.button("添加").clicked() {
                self.add_relay();
            }
        });
        ui.separator();
        self.oauth_accounts_ui(ui);
        ui.separator();
        let mut export_all = false;
        let mut export_enabled = false;
        ui.horizontal(|ui| {
            ui.heading("上游列表");
            if ui.button("导入上游").clicked() {
                self.upstream_import_open = true;
            }
            if ui
                .button("导出所有上游")
                .on_hover_text("把全部上游导出为 JSON 并复制到剪贴板")
                .clicked()
            {
                export_all = true;
            }
            if ui
                .button("导出已启用上游")
                .on_hover_text("把已启用的上游导出为 JSON 并复制到剪贴板")
                .clicked()
            {
                export_enabled = true;
            }
        });
        if export_all {
            self.export_upstreams_to_clipboard(ui.ctx(), false);
        }
        if export_enabled {
            self.export_upstreams_to_clipboard(ui.ctx(), true);
        }
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
                        ui.strong("余额刷新");
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
        self.show_upstream_import(ui.ctx());
    }

    pub(super) fn show_upstream_import(&mut self, ctx: &egui::Context) {
        if !self.upstream_import_open {
            return;
        }
        let mut open = true;
        let mut close_requested = false;
        let mut import_requested = false;
        let text = &mut self.upstream_import_text;
        egui::Window::new("导入上游")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label("粘贴导出的上游 JSON, 单条和批量导出的格式相同, 导入后会生成新的上游记录.");
                egui::ScrollArea::vertical()
                    .id_salt("upstream_import_text_scroll")
                    .max_height(280.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(text)
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .hint_text(r#"{"version": 1, "upstreams": [...]}"#),
                        );
                    });
                ui.horizontal(|ui| {
                    if ui.button("导入").clicked() {
                        import_requested = true;
                    }
                    if ui.button("取消").clicked() {
                        close_requested = true;
                    }
                });
            });
        self.upstream_import_open = open && !close_requested;
        if import_requested {
            self.import_upstream_from_text();
        }
    }

    fn import_upstream_from_text(&mut self) {
        let payload = match UpstreamExport::from_json(&self.upstream_import_text) {
            Ok(payload) => payload,
            Err(err) => {
                self.status = format!("导入失败: {err}");
                return;
            }
        };
        match self
            .runtime
            .block_on(self.state.store.import_upstreams(&payload))
        {
            Ok(result) => {
                self.upstream_import_text.clear();
                self.upstream_import_open = false;
                let mut message = format!("已导入 {} 个上游", result.imported.len());
                if result.skipped_peer_nodes > 0 {
                    message.push_str(&format!(
                        ", 已跳过 {} 个 peer 节点上游",
                        result.skipped_peer_nodes
                    ));
                }
                self.status = message;
                self.refresh_all();
            }
            Err(err) => self.status = format!("导入失败: {err}"),
        }
    }

    fn export_upstreams_to_clipboard(&mut self, ctx: &egui::Context, only_enabled: bool) {
        let result = self
            .runtime
            .block_on(self.state.store.export_upstreams(only_enabled));
        match result {
            Ok(result) if result.export.upstreams.is_empty() => {
                self.status = if only_enabled {
                    "没有已启用的上游可导出".to_string()
                } else {
                    "没有上游可导出".to_string()
                };
            }
            Ok(result) => match result.export.to_json() {
                Ok(json) => {
                    ctx.copy_text(json);
                    let mut message = format!(
                        "已导出 {} 个上游到剪贴板, JSON 包含 API Key 等凭据, 请注意保管",
                        result.export.upstreams.len()
                    );
                    if result.skipped_peer_nodes > 0 {
                        message.push_str(&format!(
                            ", 已跳过 {} 个 peer 节点上游",
                            result.skipped_peer_nodes
                        ));
                    }
                    self.status = message;
                }
                Err(err) => self.status = format!("导出上游失败: {err}"),
            },
            Err(err) => self.status = format!("导出上游失败: {err}"),
        }
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

fn balance_alert_label(ui: &mut egui::Ui, settings: Option<&UpstreamBalanceAlertSettings>) {
    let Some(settings) = settings else {
        ui.label("关闭");
        return;
    };
    if !settings.enabled {
        ui.label("关闭");
        return;
    }
    if !settings.alert_enabled {
        ui.label(format!("刷新 / {} 秒", settings.interval_seconds))
            .on_hover_text("仅自动刷新余额, 不发送系统提醒");
        return;
    }
    let response = if settings.alert_active {
        ui.colored_label(
            egui::Color32::RED,
            format!("不足 <= {:.4}", settings.threshold),
        )
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

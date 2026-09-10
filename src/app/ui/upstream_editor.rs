use super::{CodexSwitchApp, token_amount};
use crate::app::http;
use crate::balance;
use crate::core::models::{
    ApiKeyAuthScheme, BalanceProvider, CacheKeepaliveMode, ErrorRetryPolicy, UnknownModalityPolicy,
    Upstream, UpstreamBalanceAlertSettings, UpstreamCacheKeepaliveSettings, UpstreamKind, WireApi,
};
use crate::core::upstream_detection::{self, DetectedKind};
use eframe::egui;

const BALANCE_PROVIDERS: &[BalanceProvider] = &[
    BalanceProvider::Auto,
    BalanceProvider::DeepSeek,
    BalanceProvider::StepFun,
    BalanceProvider::SiliconFlowCn,
    BalanceProvider::SiliconFlowGlobal,
    BalanceProvider::OpenRouter,
    BalanceProvider::Novita,
    BalanceProvider::Zhipu,
    BalanceProvider::Sub2Api,
    BalanceProvider::NewApi,
    BalanceProvider::Unsupported,
];

#[derive(Clone)]
pub(super) struct UpstreamEditor {
    upstream: Upstream,
    cache_keepalive: UpstreamCacheKeepaliveSettings,
    balance_alert: UpstreamBalanceAlertSettings,
    min_cacheable_tokens_input: String,
    max_cacheable_tokens_input: String,
    api_key: String,
    newapi_user_key: String,
    newapi_user_id: String,
    /// 点过 "应用识别结果" 之后显示的一次性提示.
    apply_status: String,
}

impl UpstreamEditor {
    fn new(
        upstream: Upstream,
        cache_keepalive: UpstreamCacheKeepaliveSettings,
        balance_alert: UpstreamBalanceAlertSettings,
    ) -> Self {
        let min_cacheable_tokens_input =
            token_amount::format_token_input(cache_keepalive.min_cacheable_tokens);
        let max_cacheable_tokens_input =
            token_amount::format_token_input(cache_keepalive.max_cacheable_tokens);
        Self {
            upstream,
            cache_keepalive,
            balance_alert,
            min_cacheable_tokens_input,
            max_cacheable_tokens_input,
            api_key: String::new(),
            newapi_user_key: String::new(),
            newapi_user_id: String::new(),
            apply_status: String::new(),
        }
    }
}

impl CodexSwitchApp {
    pub(super) fn open_upstream_editor(&mut self, upstream: Upstream) {
        let cache_keepalive = match self
            .runtime
            .block_on(self.state.store.cache_keepalive_settings(&upstream.id))
        {
            Ok(settings) => settings,
            Err(err) => {
                self.status = format!("读取缓存保持设置失败: {err}");
                UpstreamCacheKeepaliveSettings::new(upstream.id.clone())
            }
        };
        let balance_alert = match self
            .runtime
            .block_on(self.state.store.balance_alert_settings(&upstream.id))
        {
            Ok(settings) => settings,
            Err(err) => {
                self.status = format!("读取余额刷新设置失败: {err}");
                UpstreamBalanceAlertSettings::new(upstream.id.clone())
            }
        };
        self.upstream_editor = Some(UpstreamEditor::new(
            upstream,
            cache_keepalive,
            balance_alert,
        ));
    }

    pub(super) fn show_upstream_editor(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.upstream_editor else {
            return;
        };
        let mut open = true;
        let mut action = EditorAction::None;
        egui::Window::new("编辑上游")
            .collapsible(false)
            .resizable(true)
            .open(&mut open)
            .show(ctx, |ui| {
                editor.form_ui(ui);
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        action = EditorAction::Save;
                    }
                    if ui.button("取消").clicked() {
                        action = EditorAction::Cancel;
                    }
                    let export_response = ui
                        .add_enabled(
                            editor.upstream.kind != UpstreamKind::PeerNode,
                            egui::Button::new("导出"),
                        )
                        .on_hover_text(
                            "导出已保存的上游配置 (含凭据) 为 JSON 并复制到剪贴板, 当前未保存的修改不会被导出",
                        )
                        .on_disabled_hover_text(
                            "peer 节点上游与节点配对关系绑定, 无法通过导出迁移, 请在目标设备上重新配对",
                        );
                    if export_response.clicked() {
                        action = EditorAction::Export;
                    }
                });
            });
        if !open {
            action = EditorAction::Cancel;
        }
        match action {
            EditorAction::None => {}
            EditorAction::Cancel => {
                self.upstream_editor = None;
            }
            EditorAction::Save => {
                self.save_upstream_editor();
            }
            EditorAction::Export => {
                let id = self
                    .upstream_editor
                    .as_ref()
                    .map(|editor| editor.upstream.id.clone());
                if let Some(id) = id {
                    self.export_upstream_to_clipboard(ctx, &id);
                }
            }
        }
    }

    fn export_upstream_to_clipboard(&mut self, ctx: &egui::Context, id: &str) {
        match self.runtime.block_on(self.state.store.export_upstream(id)) {
            Ok(Some(export)) => match export.to_json() {
                Ok(json) => {
                    ctx.copy_text(json);
                    self.status =
                        "已导出上游到剪贴板, JSON 包含 API Key 等凭据, 请注意保管".to_string();
                }
                Err(err) => self.status = format!("导出上游失败: {err}"),
            },
            Ok(None) => self.status = "导出失败: 上游不存在或不可导出".to_string(),
            Err(err) => self.status = format!("导出上游失败: {err}"),
        }
    }

    fn save_upstream_editor(&mut self) {
        let Some(editor) = self.upstream_editor.clone() else {
            return;
        };
        let mut upstream = editor.upstream;
        upstream.name = upstream.name.trim().to_string();
        upstream.base_url = upstream.base_url.trim().to_string();
        upstream.proxy_url = upstream
            .proxy_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if let Some(proxy_url) = upstream.proxy_url.as_deref()
            && let Err(err) = http::validate_proxy_url(proxy_url)
        {
            self.status = format!("代理 URL 无效: {err}");
            return;
        }
        upstream.weight = upstream.weight.max(1);
        if upstream.wire_api == WireApi::AnthropicMessages {
            upstream.supports_compact = false;
        }
        let api_key = editor.api_key.trim().to_string();
        let newapi_user_key = editor.newapi_user_key.trim().to_string();
        let newapi_user_id = editor.newapi_user_id.trim().to_string();
        let uses_newapi_balance = uses_newapi_balance(&upstream);
        let mut balance_alert = editor.balance_alert;
        balance_alert.upstream_id = upstream.id.clone();
        balance_alert.interval_seconds = balance_alert.interval_seconds.max(60);
        if !balance_alert.enabled {
            balance_alert.alert_enabled = false;
        }
        if !balance_alert.threshold.is_finite() || balance_alert.threshold < 0.0 {
            if balance_alert.alert_enabled {
                self.status = "余额提醒阈值必须是大于等于 0 的数字".to_string();
                return;
            }
            balance_alert.threshold = 5.0;
        }
        let mut cache_keepalive = editor.cache_keepalive;
        cache_keepalive.upstream_id = upstream.id.clone();
        cache_keepalive.interval_seconds = cache_keepalive.interval_seconds.max(60);
        cache_keepalive.max_idle_seconds = cache_keepalive.max_idle_seconds.max(60);
        cache_keepalive.min_cacheable_tokens =
            match token_amount::parse_token_amount(&editor.min_cacheable_tokens_input) {
                Ok(value) => value.max(1024),
                Err(err) => {
                    self.status = format!("最小缓存 tokens 无效: {err}");
                    return;
                }
            };
        cache_keepalive.max_cacheable_tokens =
            match token_amount::parse_token_amount(&editor.max_cacheable_tokens_input) {
                Ok(value) => value.max(cache_keepalive.min_cacheable_tokens),
                Err(err) => {
                    self.status = format!("最大缓存 tokens 无效: {err}");
                    return;
                }
            };
        cache_keepalive.max_active_sessions = cache_keepalive.max_active_sessions.max(1);
        if upstream.kind != UpstreamKind::RelayApiKey {
            cache_keepalive.enabled = false;
            cache_keepalive.mode = CacheKeepaliveMode::Off;
            balance_alert.enabled = false;
            balance_alert.alert_enabled = false;
        }

        if upstream.name.is_empty() {
            self.status = "上游名称不能为空".to_string();
            return;
        }
        if matches!(
            upstream.kind,
            UpstreamKind::RelayApiKey | UpstreamKind::PeerNode
        ) && upstream.base_url.is_empty()
        {
            self.status = "Base URL 不能为空".to_string();
            return;
        }
        let result = self.runtime.block_on(async {
            self.state.store.save_upstream(&upstream).await?;
            self.state
                .store
                .save_cache_keepalive_settings(&cache_keepalive)
                .await?;
            self.state
                .store
                .save_balance_alert_settings(&balance_alert)
                .await?;
            if !cache_keepalive.is_active() {
                self.state
                    .cache_keepalive
                    .disable_upstream_sessions(&upstream.id, "settings disabled")
                    .await;
            }
            if upstream.kind == UpstreamKind::RelayApiKey && !api_key.is_empty() {
                self.state
                    .credentials
                    .put(&upstream.id, balance::API_KEY_CREDENTIAL, &api_key)
                    .await?;
            }
            if upstream.kind == UpstreamKind::RelayApiKey
                && uses_newapi_balance
                && !newapi_user_key.is_empty()
            {
                self.state
                    .credentials
                    .put(
                        &upstream.id,
                        balance::NEWAPI_USER_KEY_CREDENTIAL,
                        &newapi_user_key,
                    )
                    .await?;
            }
            if upstream.kind == UpstreamKind::RelayApiKey
                && uses_newapi_balance
                && !newapi_user_id.is_empty()
            {
                self.state
                    .credentials
                    .put(
                        &upstream.id,
                        balance::NEWAPI_USER_ID_CREDENTIAL,
                        &newapi_user_id,
                    )
                    .await?;
            }
            anyhow::Ok(())
        });
        match result {
            Ok(()) => {
                self.status = "上游已保存".to_string();
                self.upstream_editor = None;
                if upstream.kind == UpstreamKind::PeerNode {
                    self.state.events.bump_peers();
                }
                self.refresh_all();
            }
            Err(err) => {
                self.status = format!("保存上游失败: {err}");
            }
        }
    }
}

impl UpstreamEditor {
    fn form_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("类型");
            ui.label(self.upstream.kind.as_str());
        });
        ui.horizontal(|ui| {
            ui.label("名称");
            ui.text_edit_singleline(&mut self.upstream.name);
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.upstream.enabled, "启用");
            ui.label("优先级");
            ui.add(egui::DragValue::new(&mut self.upstream.priority).speed(1));
            ui.label("权重");
            ui.add(
                egui::DragValue::new(&mut self.upstream.weight)
                    .range(1..=i64::MAX)
                    .speed(1),
            );
            ui.label("价格倍率");
            ui.add(
                egui::DragValue::new(&mut self.upstream.price_multiplier)
                    .speed(0.05)
                    .range(0.0..=1000.0),
            )
            .on_hover_text(
                "内置估算成本 = 模型官方价 x 倍率. 启用计价脚本后需自行使用 ctx.multiplier. 只影响成本统计, 不影响请求转发.",
            );
        });
        ui.horizontal(|ui| {
            ui.label("错误重试");
            egui::ComboBox::from_id_salt("upstream_error_retry_policy")
                .selected_text(error_retry_policy_label(self.upstream.error_retry_policy))
                .show_ui(ui, |ui| {
                    for policy in ErrorRetryPolicy::ALL {
                        ui.selectable_value(
                            &mut self.upstream.error_retry_policy,
                            policy,
                            error_retry_policy_label(policy),
                        );
                    }
                })
                .response
                .on_hover_text(
                    "仅临时错误会将容量和普通限流错误改写为可由客户端重试的响应. 全部上游错误还会处理上下文, 额度, 策略和无效请求错误. 它不会触发当前请求的上游切换.",
                );
        });
        ui.checkbox(
            &mut self.upstream.strip_multimodal_for_text_models,
            "为非多模态模型去除多模态输入",
        )
        .on_hover_text(
            "开启后, 当请求模型被识别为不支持图片等媒体输入时, 自动把图片, 音频和文件替换为说明模型不支持对应输入, 并带媒体类型与大小的文字描述.",
        );
        ui.add_enabled_ui(self.upstream.strip_multimodal_for_text_models, |ui| {
            ui.horizontal(|ui| {
                ui.label("未知模态模型");
                ui.radio_value(
                    &mut self.upstream.unknown_modality_policy,
                    UnknownModalityPolicy::TextOnly,
                    "单模态",
                );
                ui.radio_value(
                    &mut self.upstream.unknown_modality_policy,
                    UnknownModalityPolicy::Multimodal,
                    "多模态",
                );
            });
        })
        .response
        .on_hover_text("模型列表和 models.dev 都没有能力信息时, 按此配置决定是否清理多模态输入.");
        ui.horizontal(|ui| {
            ui.label("代理 URL");
            let proxy_url = self.upstream.proxy_url.get_or_insert_with(String::new);
            ui.add(egui::TextEdit::singleline(proxy_url).hint_text("留空使用系统代理"));
        });
        match self.upstream.kind {
            UpstreamKind::RelayApiKey => self.relay_form_ui(ui),
            UpstreamKind::CodexOauth => self.oauth_form_ui(ui),
            UpstreamKind::PeerNode => self.peer_form_ui(ui),
        }
    }

    fn relay_form_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Base URL");
            let response = ui.text_edit_singleline(&mut self.upstream.base_url);
            if response.changed() {
                self.apply_status.clear();
            }
            let detected = upstream_detection::detect_upstream(&self.upstream.base_url);
            if detected.kind != DetectedKind::Unknown {
                ui.label(format!("识别: {}", detected.kind.label()))
                    .on_hover_text(
                        "依据 Base URL 在本地判断, 不发起任何请求; 模型列表按此结果解析.",
                    );
            }
            // 中转站可能只识别出余额 provider, 此时同样允许套用.
            if detected.has_suggestion()
                && ui
                    .button("应用识别结果")
                    .on_hover_text(
                        "按识别结果改写 Base URL, Wire API, 认证方式, 过滤开关和余额 provider, 保存后生效.",
                    )
                    .clicked()
            {
                let changed = detected.apply_to(&mut self.upstream);
                self.apply_status = if changed.is_empty() {
                    "当前设置与识别结果一致".to_string()
                } else {
                    format!("已按识别结果改写: {}", changed.join(", "))
                };
            }
        });
        if !self.apply_status.is_empty() {
            ui.label(&self.apply_status);
        }
        ui.horizontal(|ui| {
            ui.label("API Key");
            ui.add(
                egui::TextEdit::singleline(&mut self.api_key)
                    .password(true)
                    .hint_text("留空则不修改"),
            );
        });
        if uses_newapi_balance(&self.upstream) {
            ui.horizontal(|ui| {
                ui.label("NewApi 用户 Key");
                ui.add(
                    egui::TextEdit::singleline(&mut self.newapi_user_key)
                        .password(true)
                        .hint_text("仅余额查询使用, 留空则不修改"),
                );
            });
            ui.horizontal(|ui| {
                ui.label("NewApi 用户 ID");
                ui.add(
                    egui::TextEdit::singleline(&mut self.newapi_user_id)
                        .password(true)
                        .hint_text("仅余额查询使用, 留空则不修改"),
                );
            });
        }
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.upstream.wire_api, WireApi::Responses, "Responses");
            ui.radio_value(
                &mut self.upstream.wire_api,
                WireApi::ChatCompletions,
                "Chat Completions",
            );
            if ui
                .radio_value(
                    &mut self.upstream.wire_api,
                    WireApi::AnthropicMessages,
                    "Anthropic Messages",
                )
                .clicked()
            {
                self.upstream.api_key_auth_scheme = ApiKeyAuthScheme::XApiKey;
                self.upstream.supports_compact = false;
            }
        });
        ui.horizontal(|ui| {
            ui.label("API Key 认证");
            ui.radio_value(
                &mut self.upstream.api_key_auth_scheme,
                ApiKeyAuthScheme::Bearer,
                "Bearer",
            );
            ui.radio_value(
                &mut self.upstream.api_key_auth_scheme,
                ApiKeyAuthScheme::XApiKey,
                "x-api-key",
            );
            ui.add_enabled_ui(
                self.upstream.wire_api != WireApi::AnthropicMessages,
                |ui| {
                    ui.checkbox(&mut self.upstream.supports_compact, "支持 compact");
                },
            );
            ui.add_enabled_ui(
                self.upstream.wire_api == WireApi::ChatCompletions,
                |ui| {
                    ui.checkbox(
                        &mut self.upstream.filter_chat_server_tools,
                        "过滤 server_tool",
                    )
                    .on_hover_text(
                        "丢弃 web_search 等非 function 工具, 适合 OpenCode 等仅支持 function 的 Chat 上游.",
                    );
                },
            );
        });
        provider_combo(ui, &mut self.upstream.balance_provider);
        if let Some(provider) = upstream_detection::detect_upstream(&self.upstream.base_url)
            .suggestion
            .balance_provider
        {
            ui.label(format!("识别为: {}", provider.as_str()))
                .on_hover_text(
                    "依据 Base URL 判断的余额 provider, 可手动选择覆盖或点上方按钮写入.",
                );
        }
        balance_alert_form(ui, &mut self.balance_alert);
        ui.separator();
        cache_keepalive_form(
            ui,
            &mut self.cache_keepalive,
            &mut self.min_cacheable_tokens_input,
            &mut self.max_cacheable_tokens_input,
        );
    }

    fn peer_form_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("节点 URL");
            ui.text_edit_singleline(&mut self.upstream.base_url);
        });
        ui.label("节点上游走独立 mTLS, 不会使用本地访问 key, 也不会做协议转换.");
    }

    fn oauth_form_ui(&mut self, ui: &mut egui::Ui) {
        ui.checkbox(&mut self.upstream.supports_compact, "支持 compact");
        ui.horizontal(|ui| {
            ui.label("账号 ID");
            ui.label(self.upstream.chatgpt_account_id.as_deref().unwrap_or(""));
        });
        ui.horizontal(|ui| {
            ui.label("邮箱");
            ui.label(self.upstream.email.as_deref().unwrap_or(""));
        });
        ui.horizontal(|ui| {
            ui.label("套餐");
            ui.label(self.upstream.plan_type.as_deref().unwrap_or(""));
        });
        ui.separator();
        ui.label("缓存保持仅支持 Relay API Key 上游");
    }
}

fn error_retry_policy_label(policy: ErrorRetryPolicy) -> &'static str {
    match policy {
        ErrorRetryPolicy::Off => "关闭",
        ErrorRetryPolicy::Transient => "仅临时错误",
        ErrorRetryPolicy::All => "全部上游错误",
    }
}

fn balance_alert_form(ui: &mut egui::Ui, settings: &mut UpstreamBalanceAlertSettings) {
    ui.separator();
    ui.heading("余额自动刷新");
    ui.horizontal(|ui| {
        if ui
            .checkbox(&mut settings.enabled, "启用自动刷新")
            .on_hover_text("按检查间隔查询并覆盖余额快照, 不要求开启系统提醒")
            .changed()
            && !settings.enabled
        {
            settings.alert_enabled = false;
        }
        ui.label("检查间隔秒");
        ui.add(
            egui::DragValue::new(&mut settings.interval_seconds)
                .range(60..=i64::MAX)
                .speed(60),
        );
    });
    ui.add_enabled_ui(settings.enabled, |ui| {
        ui.horizontal(|ui| {
            ui.checkbox(&mut settings.alert_enabled, "启用系统提醒")
                .on_hover_text("关闭时只定时刷新余额快照, 不比较阈值, 也不发送系统通知");
            ui.add_enabled_ui(settings.alert_enabled, |ui| {
                ui.label("余额阈值");
                ui.add(
                    egui::DragValue::new(&mut settings.threshold)
                        .range(0.0..=f64::MAX)
                        .speed(0.5)
                        .max_decimals(4),
                );
            });
        });
    });
}

fn cache_keepalive_form(
    ui: &mut egui::Ui,
    settings: &mut UpstreamCacheKeepaliveSettings,
    min_cacheable_tokens_input: &mut String,
    max_cacheable_tokens_input: &mut String,
) {
    ui.heading("缓存保持");
    ui.horizontal(|ui| {
        ui.checkbox(&mut settings.enabled, "启用");
        egui::ComboBox::from_label("模式")
            .selected_text(settings.mode.as_str())
            .show_ui(ui, |ui| {
                for mode in CacheKeepaliveMode::ALL {
                    ui.selectable_value(&mut settings.mode, mode, mode.as_str());
                }
            });
        ui.checkbox(
            &mut settings.prefer_extended_retention,
            "优先 24h retention",
        );
    });
    ui.horizontal(|ui| {
        ui.label("间隔秒");
        ui.add(
            egui::DragValue::new(&mut settings.interval_seconds)
                .range(60..=i64::MAX)
                .speed(10),
        );
        ui.label("最大空闲秒");
        ui.add(
            egui::DragValue::new(&mut settings.max_idle_seconds)
                .range(60..=i64::MAX)
                .speed(60),
        );
    });
    ui.horizontal(|ui| {
        ui.label("最小缓存 tokens");
        ui.add_sized(
            [92.0, 20.0],
            egui::TextEdit::singleline(min_cacheable_tokens_input),
        )
        .on_hover_text("支持 1024, 64K, 1.5M, 2B");
        ui.label("最大缓存 tokens");
        ui.add_sized(
            [92.0, 20.0],
            egui::TextEdit::singleline(max_cacheable_tokens_input),
        )
        .on_hover_text("支持 128K, 230K, 1M, 2B");
    });
    ui.horizontal(|ui| {
        ui.label("最大会话数");
        ui.add_sized(
            [56.0, 20.0],
            egui::DragValue::new(&mut settings.max_active_sessions)
                .range(1..=i64::MAX)
                .speed(1),
        );
    });
}

fn provider_combo(ui: &mut egui::Ui, provider: &mut BalanceProvider) {
    egui::ComboBox::from_label("余额 provider")
        .selected_text(provider.as_str())
        .show_ui(ui, |ui| {
            for value in BALANCE_PROVIDERS {
                ui.selectable_value(provider, *value, value.as_str());
            }
        });
}

fn uses_newapi_balance(upstream: &Upstream) -> bool {
    upstream.balance_provider == BalanceProvider::NewApi
        || (upstream.balance_provider == BalanceProvider::Auto
            && balance::detect_provider(&upstream.base_url) == Some(BalanceProvider::NewApi))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorAction {
    None,
    Save,
    Cancel,
    Export,
}

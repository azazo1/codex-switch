use super::CodexSwitchApp;
use crate::balance;
use crate::core::models::{
    BalanceProvider, CacheKeepaliveMode, Upstream, UpstreamCacheKeepaliveSettings, UpstreamKind,
    WireApi,
};
use eframe::egui;

const BALANCE_PROVIDERS: &[BalanceProvider] = &[
    BalanceProvider::Auto,
    BalanceProvider::DeepSeek,
    BalanceProvider::StepFun,
    BalanceProvider::SiliconFlowCn,
    BalanceProvider::SiliconFlowGlobal,
    BalanceProvider::OpenRouter,
    BalanceProvider::Novita,
    BalanceProvider::Sub2Api,
    BalanceProvider::NewApi,
    BalanceProvider::Unsupported,
];

#[derive(Clone)]
pub(super) struct UpstreamEditor {
    upstream: Upstream,
    cache_keepalive: UpstreamCacheKeepaliveSettings,
    min_cacheable_tokens_input: String,
    max_cacheable_tokens_input: String,
    api_key: String,
    newapi_user_key: String,
    newapi_user_id: String,
}

impl UpstreamEditor {
    fn new(upstream: Upstream, cache_keepalive: UpstreamCacheKeepaliveSettings) -> Self {
        let min_cacheable_tokens_input =
            format_token_input(cache_keepalive.min_cacheable_tokens);
        let max_cacheable_tokens_input =
            format_token_input(cache_keepalive.max_cacheable_tokens);
        Self {
            upstream,
            cache_keepalive,
            min_cacheable_tokens_input,
            max_cacheable_tokens_input,
            api_key: String::new(),
            newapi_user_key: String::new(),
            newapi_user_id: String::new(),
        }
    }
}

impl CodexSwitchApp {
    pub(super) fn open_upstream_editor(&mut self, upstream: Upstream) {
        let settings = match self
            .runtime
            .block_on(self.state.store.cache_keepalive_settings(&upstream.id))
        {
            Ok(settings) => settings,
            Err(err) => {
                self.status = format!("读取缓存保持设置失败: {err}");
                UpstreamCacheKeepaliveSettings::new(upstream.id.clone())
            }
        };
        self.upstream_editor = Some(UpstreamEditor::new(upstream, settings));
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
        }
    }

    fn save_upstream_editor(&mut self) {
        let Some(editor) = self.upstream_editor.clone() else {
            return;
        };
        let mut upstream = editor.upstream;
        upstream.name = upstream.name.trim().to_string();
        upstream.base_url = upstream.base_url.trim().to_string();
        upstream.weight = upstream.weight.max(1);
        let api_key = editor.api_key.trim().to_string();
        let newapi_user_key = editor.newapi_user_key.trim().to_string();
        let newapi_user_id = editor.newapi_user_id.trim().to_string();
        let uses_newapi_balance = uses_newapi_balance(&upstream);
        let mut cache_keepalive = editor.cache_keepalive;
        cache_keepalive.upstream_id = upstream.id.clone();
        cache_keepalive.interval_seconds = cache_keepalive.interval_seconds.max(60);
        cache_keepalive.max_idle_seconds = cache_keepalive.max_idle_seconds.max(60);
        cache_keepalive.min_cacheable_tokens =
            match parse_token_amount(&editor.min_cacheable_tokens_input) {
                Ok(value) => value.max(1024),
                Err(err) => {
                    self.status = format!("最小缓存 tokens 无效: {err}");
                    return;
                }
            };
        cache_keepalive.max_cacheable_tokens =
            match parse_token_amount(&editor.max_cacheable_tokens_input) {
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
        }

        if upstream.name.is_empty() {
            self.status = "上游名称不能为空".to_string();
            return;
        }
        if upstream.kind == UpstreamKind::RelayApiKey && upstream.base_url.is_empty() {
            self.status = "Relay Base URL 不能为空".to_string();
            return;
        }
        let result = self.runtime.block_on(async {
            self.state.store.save_upstream(&upstream).await?;
            self.state
                .store
                .save_cache_keepalive_settings(&cache_keepalive)
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
            ui.add(egui::DragValue::new(&mut self.upstream.weight).speed(1));
        });
        match self.upstream.kind {
            UpstreamKind::RelayApiKey => self.relay_form_ui(ui),
            UpstreamKind::CodexOauth => self.oauth_form_ui(ui),
        }
    }

    fn relay_form_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Base URL");
            ui.text_edit_singleline(&mut self.upstream.base_url);
        });
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
            ui.checkbox(&mut self.upstream.supports_compact, "支持 compact");
        });
        provider_combo(ui, &mut self.upstream.balance_provider);
        if self.upstream.balance_provider == BalanceProvider::Auto
            && let Some(provider) = balance::detect_provider(&self.upstream.base_url)
        {
            ui.label(format!("自动识别: {}", provider.as_str()));
        }
        ui.separator();
        cache_keepalive_form(
            ui,
            &mut self.cache_keepalive,
            &mut self.min_cacheable_tokens_input,
            &mut self.max_cacheable_tokens_input,
        );
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
        ui.checkbox(&mut settings.prefer_extended_retention, "优先 24h retention");
    });
    ui.horizontal(|ui| {
        ui.label("间隔秒");
        ui.add(egui::DragValue::new(&mut settings.interval_seconds).speed(10));
        ui.label("最大空闲秒");
        ui.add(egui::DragValue::new(&mut settings.max_idle_seconds).speed(60));
    });
    ui.horizontal(|ui| {
        ui.label("最小缓存 tokens");
        ui.text_edit_singleline(min_cacheable_tokens_input)
            .on_hover_text("支持 1024, 64K, 1.5M");
        ui.label("最大缓存 tokens");
        ui.text_edit_singleline(max_cacheable_tokens_input)
            .on_hover_text("支持 128K, 230K, 1M");
        ui.label("最大会话数");
        ui.add(egui::DragValue::new(&mut settings.max_active_sessions).speed(1));
    });
}

fn parse_token_amount(input: &str) -> Result<i64, String> {
    let value = input.trim().replace('_', "");
    if value.is_empty() {
        return Err("不能为空".to_string());
    }
    let (number, multiplier) = match value.chars().last().unwrap_or_default() {
        'k' | 'K' => (&value[..value.len() - 1], 1_000.0),
        'm' | 'M' => (&value[..value.len() - 1], 1_000_000.0),
        _ => (value.as_str(), 1.0),
    };
    let parsed = number
        .trim()
        .parse::<f64>()
        .map_err(|_| "请输入数字, 例如 1024, 64K, 1.5M".to_string())?;
    if !parsed.is_finite() || parsed < 0.0 {
        return Err("必须是非负数字".to_string());
    }
    Ok((parsed * multiplier).round() as i64)
}

fn format_token_input(value: i64) -> String {
    if value >= 1_000_000 && value % 1_000_000 == 0 {
        format!("{}M", value / 1_000_000)
    } else if value >= 1_000 && value % 1_000 == 0 {
        format!("{}K", value / 1_000)
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_human_readable_token_amounts() {
        assert_eq!(parse_token_amount("1024").unwrap(), 1024);
        assert_eq!(parse_token_amount("230K").unwrap(), 230_000);
        assert_eq!(parse_token_amount("1.5M").unwrap(), 1_500_000);
    }

    #[test]
    fn formats_token_amounts_for_editor() {
        assert_eq!(format_token_input(230_000), "230K");
        assert_eq!(format_token_input(1_000_000), "1M");
        assert_eq!(format_token_input(1536), "1536");
    }
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
}

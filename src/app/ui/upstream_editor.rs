use super::CodexSwitchApp;
use crate::balance;
use crate::core::models::{BalanceProvider, Upstream, UpstreamKind, WireApi};
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
    api_key: String,
    newapi_user_key: String,
    newapi_user_id: String,
}

impl UpstreamEditor {
    fn new(upstream: Upstream) -> Self {
        Self {
            upstream,
            api_key: String::new(),
            newapi_user_key: String::new(),
            newapi_user_id: String::new(),
        }
    }
}

impl CodexSwitchApp {
    pub(super) fn open_upstream_editor(&mut self, upstream: Upstream) {
        self.upstream_editor = Some(UpstreamEditor::new(upstream));
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

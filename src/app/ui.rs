use crate::app::state::AppState;
use crate::balance;
use crate::core::models::{
    BalanceProvider, BalanceSnapshot, DashboardStats, ProviderStats, QuotaSnapshot, RequestLog,
    Upstream, WireApi,
};
use crate::oauth;
use crate::proxy::{self, ServerHandle};
use crate::quota as quota_api;
use data::load_view_data;
use eframe::egui;
use std::sync::Arc;
use tokio::runtime::Runtime;
use upstream_editor::UpstreamEditor;

mod dashboard;
mod data;
mod logs;
mod quota;
mod upstream_editor;
mod upstreams;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Dashboard,
    Upstreams,
    Quota,
    Logs,
}

pub struct CodexSwitchApp {
    runtime: Arc<Runtime>,
    state: AppState,
    tab: Tab,
    server: Option<ServerHandle>,
    bind_addr: String,
    local_key: String,
    local_key_copied_at: Option<std::time::Instant>,
    status: String,
    upstreams: Vec<Upstream>,
    stats: DashboardStats,
    provider_stats: Vec<ProviderStats>,
    logs: Vec<RequestLog>,
    relay_name: String,
    relay_base_url: String,
    relay_api_key: String,
    relay_wire_api: WireApi,
    relay_supports_compact: bool,
    oauth_device: Option<oauth::DeviceFlow>,
    quota_snapshots: Vec<(String, Option<QuotaSnapshot>)>,
    balance_snapshots: Vec<(String, Option<BalanceSnapshot>)>,
    upstream_editor: Option<UpstreamEditor>,
}

impl CodexSwitchApp {
    pub fn new(runtime: Arc<Runtime>, state: AppState) -> Self {
        let bind_addr = runtime
            .block_on(state.store.get_setting("bind_addr"))
            .ok()
            .flatten()
            .unwrap_or_else(|| "127.0.0.1:15721".to_string());
        let local_key = runtime
            .block_on(state.store.get_setting("local_access_key"))
            .ok()
            .flatten()
            .unwrap_or_default();
        let mut app = Self {
            runtime,
            state,
            tab: Tab::Dashboard,
            server: None,
            bind_addr,
            local_key,
            local_key_copied_at: None,
            status: "就绪".to_string(),
            upstreams: Vec::new(),
            stats: DashboardStats::default(),
            provider_stats: Vec::new(),
            logs: Vec::new(),
            relay_name: String::new(),
            relay_base_url: String::new(),
            relay_api_key: String::new(),
            relay_wire_api: WireApi::Responses,
            relay_supports_compact: true,
            oauth_device: None,
            quota_snapshots: Vec::new(),
            balance_snapshots: Vec::new(),
            upstream_editor: None,
        };
        app.refresh_all();
        app
    }

    fn refresh_all(&mut self) {
        match self.runtime.block_on(load_view_data(&self.state)) {
            Ok(data) => {
                self.upstreams = data.upstreams;
                self.stats = data.stats;
                self.provider_stats = data.provider_stats;
                self.logs = data.logs;
                self.quota_snapshots = data.quota_snapshots;
                self.balance_snapshots = data.balance_snapshots;
            }
            Err(err) => {
                self.status = format!("刷新失败: {err}");
            }
        }
    }

    fn start_server(&mut self) {
        if self.server.is_some() {
            self.status = "服务已经在运行".to_string();
            return;
        }
        let bind_addr = self.bind_addr.clone();
        if let Err(err) = self
            .runtime
            .block_on(self.state.store.set_setting("bind_addr", &bind_addr))
        {
            self.status = format!("保存监听地址失败: {err}");
            return;
        }
        let state = self.state.clone();
        match self
            .runtime
            .block_on(proxy::start_server(bind_addr.clone(), state))
        {
            Ok(handle) => {
                self.server = Some(handle);
                self.status = format!("服务已启动: http://{bind_addr}");
            }
            Err(err) => {
                self.status = format!("服务启动失败: {err}");
            }
        }
    }

    fn stop_server(&mut self) {
        if let Some(handle) = self.server.take() {
            handle.stop();
            self.status = "服务已停止".to_string();
        }
    }

    fn refresh_local_key(&mut self) {
        let key = format!("cs-{}", uuid::Uuid::new_v4());
        match self
            .runtime
            .block_on(self.state.store.set_setting("local_access_key", &key))
        {
            Ok(()) => {
                self.local_key = key;
                self.local_key_copied_at = None;
                self.status = "本地访问 key 已刷新, Codex 需要使用新 key".to_string();
            }
            Err(err) => {
                self.status = format!("刷新 key 失败: {err}");
            }
        }
    }

    fn add_relay(&mut self) {
        let name = self.relay_name.trim().to_string();
        let base_url = self.relay_base_url.trim().to_string();
        let api_key = self.relay_api_key.trim().to_string();
        if name.is_empty() || base_url.is_empty() || api_key.is_empty() {
            self.status = "名称, Base URL 和 API Key 都不能为空".to_string();
            return;
        }
        let provider = balance::detect_provider(&base_url).unwrap_or(BalanceProvider::Auto);
        let upstream = Upstream::new_relay(
            name,
            base_url,
            self.relay_wire_api,
            self.relay_supports_compact,
            provider,
        );
        let result = self.runtime.block_on(async {
            self.state.store.save_upstream(&upstream).await?;
            self.state.secrets.put(&upstream.id, "api_key", &api_key).await
        });
        match result {
            Ok(()) => {
                self.relay_name.clear();
                self.relay_base_url.clear();
                self.relay_api_key.clear();
                self.status = "已添加中转站上游".to_string();
                self.refresh_all();
            }
            Err(err) => self.status = format!("添加失败: {err}"),
        }
    }

    fn start_oauth(&mut self) {
        match self.runtime.block_on(oauth::start_device_flow(&self.state.http)) {
            Ok(device) => {
                self.status = format!("打开 {} 并输入 {}", device.verification_uri, device.user_code);
                self.oauth_device = Some(device);
            }
            Err(err) => self.status = format!("OAuth 启动失败: {err}"),
        }
    }

    fn poll_oauth(&mut self) {
        let Some(device) = self.oauth_device.clone() else {
            self.status = "没有进行中的 OAuth 流程".to_string();
            return;
        };
        match self
            .runtime
            .block_on(oauth::poll_device_flow(&self.state, &device))
        {
            Ok(Some(upstream)) => {
                self.status = format!("OAuth 账号已添加: {}", upstream.name);
                self.oauth_device = None;
                self.refresh_all();
            }
            Ok(None) => {
                self.status = "等待用户授权中".to_string();
            }
            Err(err) => self.status = format!("OAuth 轮询失败: {err}"),
        }
    }

    fn query_selected_quota(&mut self, upstream_id: &str) {
        match self
            .runtime
            .block_on(quota_api::query_and_store(&self.state, upstream_id))
        {
            Ok(_) => {
                self.status = "额度已刷新".to_string();
                self.refresh_all();
            }
            Err(err) => self.status = format!("额度查询失败: {err}"),
        }
    }

    fn query_selected_balance(&mut self, upstream_id: &str) {
        match self
            .runtime
            .block_on(balance::query_and_store(&self.state, upstream_id))
        {
            Ok(_) => {
                self.status = "余额已刷新".to_string();
                self.refresh_all();
            }
            Err(err) => self.status = format!("余额查询失败: {err}"),
        }
    }
}

impl eframe::App for CodexSwitchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                tab_button(ui, &mut self.tab, Tab::Dashboard, "仪表盘");
                tab_button(ui, &mut self.tab, Tab::Upstreams, "上游");
                tab_button(ui, &mut self.tab, Tab::Quota, "额度");
                tab_button(ui, &mut self.tab, Tab::Logs, "日志");
                if ui.button("刷新").clicked() {
                    self.refresh_all();
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            match self.tab {
                Tab::Dashboard => self.dashboard_ui(ui),
                Tab::Upstreams => self.upstreams_ui(ui),
                Tab::Quota => self.quota_ui(ui),
                Tab::Logs => self.logs_ui(ui),
            }
            ui.separator();
            ui.label(&self.status);
            if self.state.secrets.fallback_used() {
                ui.label("系统 keyring 不可用, 当前使用本地回退密钥加密 secret");
            }
        });
    }
}

fn tab_button(ui: &mut egui::Ui, tab: &mut Tab, value: Tab, text: &str) {
    if ui.selectable_label(*tab == value, text).clicked() {
        *tab = value;
    }
}

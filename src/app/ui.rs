use crate::app::state::AppState;
use crate::balance;
use crate::core::models::{
    BalanceProvider, BalanceSnapshot, DashboardStats, ProviderStats, QuotaSnapshot, RequestLog,
    ScheduleGroup, ScheduleGroupMember, Upstream, WireApi,
};
use crate::live::LiveRequestSnapshot;
use crate::oauth;
use crate::pricing;
use crate::proxy::{self, ServerHandle};
use crate::quota as quota_api;
use data::load_view_data;
use eframe::egui;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use scheduler::ScheduleGroupEditor;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::runtime::Runtime;
use upstream_editor::UpstreamEditor;

const LOG_PAGE_SIZE: usize = 20;

mod dashboard;
mod active;
mod data;
mod logs;
mod quota;
mod scheduler;
mod tokens;
mod upstream_editor;
mod upstreams;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Dashboard,
    Upstreams,
    Scheduler,
    ActiveConnections,
    Logs,
}

enum UiTaskEvent {
    OAuthStarted(anyhow::Result<oauth::DeviceFlow>),
    OAuthPolled(anyhow::Result<Option<Upstream>>),
    QuotaQueried(anyhow::Result<()>),
    BalanceQueried {
        upstream_id: String,
        result: anyhow::Result<()>,
    },
    PriceCacheFetched(anyhow::Result<usize>),
    PriceCacheOnceFetched(anyhow::Result<pricing::PriceFetchSummary>),
}

pub struct CodexSwitchApp {
    runtime: Arc<Runtime>,
    state: AppState,
    task_tx: UnboundedSender<UiTaskEvent>,
    task_rx: UnboundedReceiver<UiTaskEvent>,
    tab: Tab,
    server: Option<ServerHandle>,
    bind_addr: String,
    local_key: String,
    local_key_copied_at: Option<Instant>,
    last_seen_request_log_version: u64,
    last_seen_live_stream_version: u64,
    price_fetch_started: bool,
    price_fetch_pending: bool,
    status: String,
    upstreams: Vec<Upstream>,
    schedule_groups: Vec<ScheduleGroup>,
    schedule_members: BTreeMap<String, Vec<ScheduleGroupMember>>,
    current_schedule_group_id: Option<String>,
    schedule_group_editor: Option<ScheduleGroupEditor>,
    new_schedule_group: ScheduleGroupEditor,
    stats: DashboardStats,
    provider_stats: Vec<ProviderStats>,
    logs: Vec<RequestLog>,
    live_connections: Vec<LiveRequestSnapshot>,
    log_page: usize,
    log_page_size: usize,
    log_total_count: i64,
    total_estimated_cost_usd: Option<f64>,
    today_estimated_cost_usd: Option<f64>,
    provider_estimated_cost_usd: BTreeMap<String, Option<f64>>,
    log_estimated_cost_usd: Vec<Option<f64>>,
    price_cache_count: i64,
    price_cache_age_seconds: Option<i64>,
    token_display_mode: tokens::TokenDisplayMode,
    oauth_start_pending: bool,
    oauth_poll_pending: bool,
    quota_query_pending: bool,
    balance_query_pending_ids: BTreeSet<String>,
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
    pub fn new(runtime: Arc<Runtime>, state: AppState, egui_ctx: egui::Context) -> Self {
        state.events.set_repaint_requester(move || {
            egui_ctx.request_repaint();
        });
        let (task_tx, task_rx) = tokio::sync::mpsc::unbounded_channel();
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
        let last_seen_request_log_version = state.events.request_log_version();
        let last_seen_live_stream_version = state.events.live_stream_version();
        let mut app = Self {
            runtime,
            state,
            task_tx,
            task_rx,
            tab: Tab::Dashboard,
            server: None,
            bind_addr,
            local_key,
            local_key_copied_at: None,
            last_seen_request_log_version,
            last_seen_live_stream_version,
            price_fetch_started: false,
            price_fetch_pending: false,
            status: "就绪".to_string(),
            upstreams: Vec::new(),
            schedule_groups: Vec::new(),
            schedule_members: BTreeMap::new(),
            current_schedule_group_id: None,
            schedule_group_editor: None,
            new_schedule_group: ScheduleGroupEditor::new_empty(),
            stats: DashboardStats::default(),
            provider_stats: Vec::new(),
            logs: Vec::new(),
            live_connections: Vec::new(),
            log_page: 0,
            log_page_size: LOG_PAGE_SIZE,
            log_total_count: 0,
            total_estimated_cost_usd: None,
            today_estimated_cost_usd: None,
            provider_estimated_cost_usd: BTreeMap::new(),
            log_estimated_cost_usd: Vec::new(),
            price_cache_count: 0,
            price_cache_age_seconds: None,
            token_display_mode: tokens::TokenDisplayMode::Human,
            oauth_start_pending: false,
            oauth_poll_pending: false,
            quota_query_pending: false,
            balance_query_pending_ids: BTreeSet::new(),
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
        app.fetch_price_cache_once();
        app
    }

    fn maybe_auto_refresh(&mut self, ctx: &egui::Context) {
        ctx.request_repaint_after(Duration::from_secs(1));
        let live_version = self.state.events.live_stream_version();
        if live_version != self.last_seen_live_stream_version {
            self.last_seen_live_stream_version = live_version;
            self.live_connections = self.state.live_requests.snapshots();
        }
        let version = self.state.events.request_log_version();
        if version != self.last_seen_request_log_version {
            self.last_seen_request_log_version = version;
            self.refresh_all();
        }
    }

    fn drain_task_events(&mut self) {
        while let Ok(event) = self.task_rx.try_recv() {
            match event {
                UiTaskEvent::OAuthStarted(result) => {
                    self.oauth_start_pending = false;
                    match result {
                        Ok(device) => {
                            self.status = format!(
                                "打开 {} 并输入 {}",
                                device.verification_uri, device.user_code
                            );
                            self.oauth_device = Some(device);
                        }
                        Err(err) => self.status = format!("OAuth 启动失败: {err}"),
                    }
                }
                UiTaskEvent::OAuthPolled(result) => {
                    self.oauth_poll_pending = false;
                    match result {
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
                UiTaskEvent::QuotaQueried(result) => {
                    self.quota_query_pending = false;
                    match result {
                        Ok(()) => {
                            self.status = "额度已刷新".to_string();
                            self.refresh_all();
                        }
                        Err(err) => self.status = format!("额度查询失败: {err}"),
                    }
                }
                UiTaskEvent::BalanceQueried {
                    upstream_id,
                    result,
                } => {
                    self.balance_query_pending_ids.remove(&upstream_id);
                    match result {
                        Ok(()) => {
                            self.status = "余额已刷新".to_string();
                            self.refresh_all();
                        }
                        Err(err) => self.status = format!("余额查询失败: {err}"),
                    }
                }
                UiTaskEvent::PriceCacheFetched(result) => {
                    self.price_fetch_pending = false;
                    match result {
                        Ok(count) => {
                            self.status = format!("模型价格已获取: {count} 条");
                            self.refresh_all();
                        }
                        Err(err) => self.status = format!("模型价格获取失败: {err}"),
                    }
                }
                UiTaskEvent::PriceCacheOnceFetched(result) => {
                    self.price_fetch_pending = false;
                    match result {
                        Ok(summary) => {
                            if summary.fetched {
                                self.status = format!("模型价格已获取: {} 条", summary.count);
                                self.refresh_all();
                            } else if summary.count > 0 {
                                self.status = format!("模型价格缓存可用: {} 条", summary.count);
                            }
                        }
                        Err(err) => {
                            self.status = format!("模型价格获取失败, 将使用已有缓存: {err}");
                        }
                    }
                }
            }
        }
    }

    fn refresh_all(&mut self) {
        let log_limit = self.log_page_size as i64;
        let log_offset = (self.log_page * self.log_page_size) as i64;
        match self
            .runtime
            .block_on(load_view_data(&self.state, log_limit, log_offset))
        {
            Ok(data) => {
                self.upstreams = data.upstreams;
                self.schedule_groups = data.schedule_groups;
                self.schedule_members = data.schedule_members;
                self.current_schedule_group_id = data.current_schedule_group_id;
                self.sync_schedule_group_editor();
                self.stats = data.stats;
                self.provider_stats = data.provider_stats;
                self.logs = data.logs;
                self.log_total_count = data.log_total_count;
                self.total_estimated_cost_usd = data.total_estimated_cost_usd;
                self.today_estimated_cost_usd = data.today_estimated_cost_usd;
                self.provider_estimated_cost_usd = data.provider_estimated_cost_usd;
                self.log_estimated_cost_usd = data.log_estimated_cost_usd;
                self.price_cache_count = data.price_cache_count;
                self.price_cache_age_seconds = data.price_cache_age_seconds;
                self.quota_snapshots = data.quota_snapshots;
                self.balance_snapshots = data.balance_snapshots;
            }
            Err(err) => {
                self.status = format!("刷新失败: {err}");
            }
        }
    }

    fn sync_schedule_group_editor(&mut self) {
        if let Some(editor) = &self.schedule_group_editor {
            let exists = self
                .schedule_groups
                .iter()
                .any(|group| group.id == editor.group.id);
            if !exists {
                self.schedule_group_editor = None;
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
            self.state.credentials.put(&upstream.id, "api_key", &api_key).await
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
        if self.oauth_start_pending {
            return;
        }
        self.oauth_start_pending = true;
        self.status = "正在启动 OAuth 登录".to_string();
        let http = self.state.http.clone();
        let tx = self.task_tx.clone();
        self.runtime.spawn(async move {
            let result = oauth::start_device_flow(&http).await;
            let _ = tx.send(UiTaskEvent::OAuthStarted(result));
        });
    }

    fn poll_oauth(&mut self) {
        if self.oauth_poll_pending {
            return;
        }
        let Some(device) = self.oauth_device.clone() else {
            self.status = "没有进行中的 OAuth 流程".to_string();
            return;
        };
        self.oauth_poll_pending = true;
        self.status = "正在轮询 OAuth 授权".to_string();
        let state = self.state.clone();
        let tx = self.task_tx.clone();
        self.runtime.spawn(async move {
            let result = oauth::poll_device_flow(&state, &device).await;
            let _ = tx.send(UiTaskEvent::OAuthPolled(result));
        });
    }

    fn query_selected_quota(&mut self, upstream_id: &str) {
        if self.quota_query_pending {
            return;
        }
        self.quota_query_pending = true;
        self.status = "正在查询额度".to_string();
        let state = self.state.clone();
        let upstream_id = upstream_id.to_string();
        let tx = self.task_tx.clone();
        self.runtime.spawn(async move {
            let result = quota_api::query_and_store(&state, &upstream_id)
                .await
                .map(|_| ());
            let _ = tx.send(UiTaskEvent::QuotaQueried(result));
        });
    }

    fn query_selected_balance(&mut self, upstream_id: &str) {
        if self.balance_query_pending_ids.contains(upstream_id) {
            return;
        }
        self.balance_query_pending_ids
            .insert(upstream_id.to_string());
        self.status = "正在查询余额".to_string();
        let state = self.state.clone();
        let upstream_id = upstream_id.to_string();
        let tx = self.task_tx.clone();
        self.runtime.spawn(async move {
            let result = balance::query_and_store(&state, &upstream_id)
                .await
                .map(|_| ());
            let _ = tx.send(UiTaskEvent::BalanceQueried {
                upstream_id,
                result,
            });
        });
    }

    fn fetch_price_cache(&mut self) {
        if self.price_fetch_pending {
            return;
        }
        self.price_fetch_pending = true;
        self.status = "正在获取模型价格".to_string();
        let state = self.state.clone();
        let tx = self.task_tx.clone();
        self.runtime.spawn(async move {
            let result = pricing::fetch_price_cache(&state).await;
            let _ = tx.send(UiTaskEvent::PriceCacheFetched(result));
        });
    }

    fn fetch_price_cache_once(&mut self) {
        if self.price_fetch_started {
            return;
        }
        self.price_fetch_started = true;
        self.price_fetch_pending = true;
        self.status = "正在检查模型价格缓存".to_string();
        let state = self.state.clone();
        let tx = self.task_tx.clone();
        self.runtime.spawn(async move {
            let result = pricing::fetch_price_cache_once(&state).await;
            let _ = tx.send(UiTaskEvent::PriceCacheOnceFetched(result));
        });
    }
}

impl eframe::App for CodexSwitchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.maybe_auto_refresh(ctx);
        self.drain_task_events();

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                tab_button(ui, &mut self.tab, Tab::Dashboard, "仪表盘");
                tab_button(ui, &mut self.tab, Tab::Upstreams, "上游");
                tab_button(ui, &mut self.tab, Tab::Scheduler, "调度组");
                tab_button(ui, &mut self.tab, Tab::ActiveConnections, "活跃连接");
                tab_button(ui, &mut self.tab, Tab::Logs, "日志");
                if ui.button("刷新").clicked() {
                    self.refresh_all();
                }
            });
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(&self.status);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            match self.tab {
                Tab::Dashboard => self.dashboard_ui(ui),
                Tab::Upstreams => self.upstreams_ui(ui),
                Tab::Scheduler => self.scheduler_ui(ui),
                Tab::ActiveConnections => self.active_connections_ui(ui),
                Tab::Logs => self.logs_ui(ui),
            }
        });
    }
}

fn tab_button(ui: &mut egui::Ui, tab: &mut Tab, value: Tab, text: &str) {
    if ui.selectable_label(*tab == value, text).clicked() {
        *tab = value;
    }
}

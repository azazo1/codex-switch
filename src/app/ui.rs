use crate::app::tray::{TrayCommand, TrayController};
use crate::app::{http, platform, state::AppState};
use crate::balance;
use crate::cache_keepalive::CacheKeepaliveSessionSnapshot;
use crate::core::models::{
    BalanceProvider, BalanceSnapshot, DashboardStats, DatabaseInfo, ProviderStats, QuotaSnapshot,
    RequestLog, ScheduleGroup, ScheduleGroupChild, ScheduleGroupMember, ScheduleRouteRule,
    Upstream, UpstreamCacheKeepaliveSettings, WireApi,
};
use crate::live::LiveRequestSnapshot;
use crate::oauth;
use crate::pricing;
use crate::proxy::{self, ServerHandle};
use crate::quota as quota_api;
use crate::storage::RequestLogFilter;
use chrono::{Datelike, Local, TimeZone, Timelike, Utc};
use data::load_view_data;
use eframe::egui;
use scheduler::ScheduleGroupEditor;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use upstream_editor::UpstreamEditor;

const LOG_PAGE_SIZE: usize = 20;
const ACTIVE_TAB_COUNT_MAX: usize = 999;
const REQUEST_LOG_POLL_INTERVAL: Duration = Duration::from_secs(10);
const HIDDEN_REPAINT_INTERVAL: Duration = Duration::from_secs(5);
const CACHE_KEEPALIVE_VISIBLE_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

mod active;
mod cache_keepalive;
mod dashboard;
mod data;
mod logs;
mod quota;
mod scheduler;
mod token_amount;
mod tokens;
mod upstream_editor;
mod upstreams;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Dashboard,
    Upstreams,
    Scheduler,
    CacheKeepalive,
    ActiveConnections,
    Logs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogRetentionChoice {
    OneDay,
    OneWeek,
    OneMonth,
    OneYear,
    Count,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct LogFilterState {
    model: Option<String>,
    upstream: Option<String>,
    reasoning_effort: Option<String>,
    endpoint: Option<String>,
    status: LogStatusFilter,
    status_custom: I64RangeFilter,
    price_usd: F64RangeFilter,
    started_at: LogDateTimeFilter,
    ended_at: LogDateTimeFilter,
    duration_ms: I64RangeFilter,
    first_token_ms: I64RangeFilter,
    input_tokens: I64RangeFilter,
    output_tokens: I64RangeFilter,
    cache_read_tokens: I64RangeFilter,
    cache_creation_tokens: I64RangeFilter,
    total_tokens: I64RangeFilter,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum LogStatusFilter {
    #[default]
    All,
    Success,
    Error,
    ClientError,
    ServerError,
    Custom,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct I64RangeFilter {
    min: String,
    max: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct F64RangeFilter {
    min: String,
    max: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LogDateTimeFilter {
    enabled: bool,
    value: LogDateTimeValue,
}

impl Default for LogDateTimeFilter {
    fn default() -> Self {
        Self {
            enabled: false,
            value: LogDateTimeValue::now(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LogDateTimeValue {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

impl LogDateTimeValue {
    fn now() -> Self {
        let now = Local::now();
        Self {
            year: now.year(),
            month: now.month(),
            day: now.day(),
            hour: now.hour(),
            minute: now.minute(),
            second: now.second(),
        }
    }
}

impl LogFilterState {
    fn is_active(&self) -> bool {
        self.active_count() > 0
    }

    fn active_count(&self) -> usize {
        let mut count = [
            self.model.is_some(),
            self.upstream.is_some(),
            self.reasoning_effort.is_some(),
            self.endpoint.is_some(),
            self.status != LogStatusFilter::All,
            self.price_usd.is_active(),
            self.started_at.enabled,
            self.ended_at.enabled,
            self.duration_ms.is_active(),
            self.first_token_ms.is_active(),
            self.input_tokens.is_active(),
            self.output_tokens.is_active(),
            self.cache_read_tokens.is_active(),
            self.cache_creation_tokens.is_active(),
            self.total_tokens.is_active(),
        ]
        .into_iter()
        .filter(|active| *active)
        .count();
        if self.status == LogStatusFilter::Custom && !self.status_custom.is_active() {
            count += 1;
        }
        count
    }

    fn to_runtime_filter(&self) -> Result<RequestLogFilter, String> {
        validate_i64_range("状态码", &self.status_custom)?;
        validate_i64_range("耗时", &self.duration_ms)?;
        validate_i64_range("首 token", &self.first_token_ms)?;
        validate_token_range("输入 tokens", &self.input_tokens)?;
        validate_token_range("输出 tokens", &self.output_tokens)?;
        validate_token_range("缓存输入 tokens", &self.cache_read_tokens)?;
        validate_token_range("写入缓存 tokens", &self.cache_creation_tokens)?;
        validate_token_range("总 tokens", &self.total_tokens)?;
        validate_f64_range("费用", &self.price_usd)?;
        let (status_min, status_max) = self.status_range()?;
        let started_at = self.started_at.to_utc("开始时间")?;
        let ended_at = self.ended_at.to_utc("结束时间")?;
        if let (Some(started_at), Some(ended_at)) = (started_at, ended_at)
            && started_at > ended_at
        {
            return Err("开始时间不能晚于结束时间".to_string());
        }

        Ok(RequestLogFilter {
            model: self.model.clone(),
            upstream: self.upstream.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            endpoint: self.endpoint.clone(),
            status_min,
            status_max,
            duration_ms_min: self.duration_ms.min_value("耗时")?,
            duration_ms_max: self.duration_ms.max_value("耗时")?,
            first_token_ms_min: self.first_token_ms.min_value("首 token")?,
            first_token_ms_max: self.first_token_ms.max_value("首 token")?,
            input_tokens_min: self.input_tokens.min_token_value("输入 tokens")?,
            input_tokens_max: self.input_tokens.max_token_value("输入 tokens")?,
            output_tokens_min: self.output_tokens.min_token_value("输出 tokens")?,
            output_tokens_max: self.output_tokens.max_token_value("输出 tokens")?,
            cache_read_tokens_min: self.cache_read_tokens.min_token_value("缓存输入 tokens")?,
            cache_read_tokens_max: self.cache_read_tokens.max_token_value("缓存输入 tokens")?,
            cache_creation_tokens_min: self
                .cache_creation_tokens
                .min_token_value("写入缓存 tokens")?,
            cache_creation_tokens_max: self
                .cache_creation_tokens
                .max_token_value("写入缓存 tokens")?,
            total_tokens_min: self.total_tokens.min_token_value("总 tokens")?,
            total_tokens_max: self.total_tokens.max_token_value("总 tokens")?,
            estimated_cost_usd_min: self.price_usd.min_value("费用")?,
            estimated_cost_usd_max: self.price_usd.max_value("费用")?,
            started_at,
            ended_at,
        })
    }

    fn status_range(&self) -> Result<(Option<i64>, Option<i64>), String> {
        match self.status {
            LogStatusFilter::All => Ok((None, None)),
            LogStatusFilter::Success => Ok((Some(200), Some(399))),
            LogStatusFilter::Error => Ok((Some(400), None)),
            LogStatusFilter::ClientError => Ok((Some(400), Some(499))),
            LogStatusFilter::ServerError => Ok((Some(500), Some(599))),
            LogStatusFilter::Custom => Ok((
                self.status_custom.min_value("状态码")?,
                self.status_custom.max_value("状态码")?,
            )),
        }
    }
}

impl I64RangeFilter {
    fn is_active(&self) -> bool {
        !self.min.trim().is_empty() || !self.max.trim().is_empty()
    }

    fn min_value(&self, label: &str) -> Result<Option<i64>, String> {
        parse_optional_i64(label, &self.min)
    }

    fn max_value(&self, label: &str) -> Result<Option<i64>, String> {
        parse_optional_i64(label, &self.max)
    }

    fn min_token_value(&self, label: &str) -> Result<Option<i64>, String> {
        token_amount::parse_optional_token_amount(label, &self.min)
    }

    fn max_token_value(&self, label: &str) -> Result<Option<i64>, String> {
        token_amount::parse_optional_token_amount(label, &self.max)
    }
}

impl F64RangeFilter {
    fn is_active(&self) -> bool {
        !self.min.trim().is_empty() || !self.max.trim().is_empty()
    }

    fn min_value(&self, label: &str) -> Result<Option<f64>, String> {
        parse_optional_f64(label, &self.min)
    }

    fn max_value(&self, label: &str) -> Result<Option<f64>, String> {
        parse_optional_f64(label, &self.max)
    }
}

impl LogDateTimeFilter {
    fn to_utc(self, label: &str) -> Result<Option<chrono::DateTime<Utc>>, String> {
        if !self.enabled {
            return Ok(None);
        }
        let Some(local_time) = Local
            .with_ymd_and_hms(
                self.value.year,
                self.value.month,
                self.value.day,
                self.value.hour,
                self.value.minute,
                self.value.second,
            )
            .single()
        else {
            return Err(format!("{label} 不是有效本地时间"));
        };
        Ok(Some(local_time.with_timezone(&Utc)))
    }
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
    Tray(TrayCommand),
}

pub struct CodexSwitchApp {
    runtime: Arc<Runtime>,
    state: AppState,
    task_tx: UnboundedSender<UiTaskEvent>,
    task_rx: UnboundedReceiver<UiTaskEvent>,
    tab: Tab,
    server: Option<ServerHandle>,
    tray: Option<TrayController>,
    tray_init_failed: bool,
    exit_requested: bool,
    exit_confirm_open: bool,
    log_filter_open: bool,
    log_cleanup_open: bool,
    window_hidden_to_tray: bool,
    background_reopen: platform::BackgroundReopenMonitor,
    bind_addr: String,
    local_key: String,
    local_key_copied_at: Option<Instant>,
    local_key_refresh_open: bool,
    local_key_refresh_value: String,
    last_seen_request_log_version: u64,
    last_request_log_poll_at: Instant,
    last_seen_live_stream_version: u64,
    last_seen_cache_keepalive_version: u64,
    last_cache_keepalive_refresh_at: Instant,
    price_fetch_started: bool,
    price_fetch_pending: bool,
    status: String,
    upstreams: Vec<Upstream>,
    cache_keepalive_settings: BTreeMap<String, UpstreamCacheKeepaliveSettings>,
    schedule_groups: Vec<ScheduleGroup>,
    schedule_members: BTreeMap<String, Vec<ScheduleGroupMember>>,
    schedule_children: BTreeMap<String, Vec<ScheduleGroupChild>>,
    schedule_route_rules: BTreeMap<String, Vec<ScheduleRouteRule>>,
    scheduler_route_max_hops: i64,
    current_schedule_group_id: Option<String>,
    schedule_group_editor: Option<ScheduleGroupEditor>,
    new_schedule_group: ScheduleGroupEditor,
    stats: DashboardStats,
    provider_stats: Vec<ProviderStats>,
    logs: Vec<RequestLog>,
    live_connections: Vec<LiveRequestSnapshot>,
    cache_keepalive_sessions: Vec<CacheKeepaliveSessionSnapshot>,
    selected_cache_keepalive_key: Option<String>,
    log_page: usize,
    log_page_size: usize,
    log_total_count: i64,
    log_filter_editor: LogFilterState,
    log_filter_applied: LogFilterState,
    log_runtime_filter: RequestLogFilter,
    log_retention_choice: LogRetentionChoice,
    log_retention_count: i64,
    total_estimated_cost_usd: Option<f64>,
    today_estimated_cost_usd: Option<f64>,
    provider_estimated_cost_usd: BTreeMap<String, Option<f64>>,
    log_estimated_cost_usd: Vec<Option<f64>>,
    price_cache_count: i64,
    price_cache_age_seconds: Option<i64>,
    database_info: DatabaseInfo,
    token_display_mode: tokens::TokenDisplayMode,
    oauth_start_pending: bool,
    oauth_poll_pending: bool,
    quota_query_pending: bool,
    balance_query_pending_ids: BTreeSet<String>,
    relay_name: String,
    relay_base_url: String,
    relay_proxy_url: String,
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
        let last_seen_cache_keepalive_version = state.events.cache_keepalive_version();
        let mut app = Self {
            runtime,
            state,
            task_tx,
            task_rx,
            tab: Tab::Dashboard,
            server: None,
            tray: None,
            tray_init_failed: false,
            exit_requested: false,
            exit_confirm_open: false,
            log_filter_open: false,
            log_cleanup_open: false,
            window_hidden_to_tray: false,
            background_reopen: platform::BackgroundReopenMonitor::default(),
            bind_addr,
            local_key,
            local_key_copied_at: None,
            local_key_refresh_open: false,
            local_key_refresh_value: String::new(),
            last_seen_request_log_version,
            last_request_log_poll_at: Instant::now(),
            last_seen_live_stream_version,
            last_seen_cache_keepalive_version,
            last_cache_keepalive_refresh_at: Instant::now(),
            price_fetch_started: false,
            price_fetch_pending: false,
            status: "就绪".to_string(),
            upstreams: Vec::new(),
            cache_keepalive_settings: BTreeMap::new(),
            schedule_groups: Vec::new(),
            schedule_members: BTreeMap::new(),
            schedule_children: BTreeMap::new(),
            schedule_route_rules: BTreeMap::new(),
            scheduler_route_max_hops: 8,
            current_schedule_group_id: None,
            schedule_group_editor: None,
            new_schedule_group: ScheduleGroupEditor::new_empty(),
            stats: DashboardStats::default(),
            provider_stats: Vec::new(),
            logs: Vec::new(),
            live_connections: Vec::new(),
            cache_keepalive_sessions: Vec::new(),
            selected_cache_keepalive_key: None,
            log_page: 0,
            log_page_size: LOG_PAGE_SIZE,
            log_total_count: 0,
            log_filter_editor: LogFilterState::default(),
            log_filter_applied: LogFilterState::default(),
            log_runtime_filter: RequestLogFilter::default(),
            log_retention_choice: LogRetentionChoice::OneMonth,
            log_retention_count: 1000,
            total_estimated_cost_usd: None,
            today_estimated_cost_usd: None,
            provider_estimated_cost_usd: BTreeMap::new(),
            log_estimated_cost_usd: Vec::new(),
            price_cache_count: 0,
            price_cache_age_seconds: None,
            database_info: DatabaseInfo::default(),
            token_display_mode: tokens::TokenDisplayMode::Human,
            oauth_start_pending: false,
            oauth_poll_pending: false,
            quota_query_pending: false,
            balance_query_pending_ids: BTreeSet::new(),
            relay_name: String::new(),
            relay_base_url: String::new(),
            relay_proxy_url: String::new(),
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
        if self.window_hidden_to_tray {
            ctx.request_repaint_after(HIDDEN_REPAINT_INTERVAL);
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(500));
        let live_version = self.state.events.live_stream_version();
        if live_version != self.last_seen_live_stream_version {
            self.last_seen_live_stream_version = live_version;
            self.live_connections = self.state.live_requests.snapshots();
        }
        let cache_keepalive_version = self.state.events.cache_keepalive_version();
        if cache_keepalive_version != self.last_seen_cache_keepalive_version {
            self.last_seen_cache_keepalive_version = cache_keepalive_version;
            self.refresh_cache_keepalive_sessions();
        }
        if self.tab == Tab::CacheKeepalive
            && self.last_cache_keepalive_refresh_at.elapsed()
                >= CACHE_KEEPALIVE_VISIBLE_REFRESH_INTERVAL
        {
            self.refresh_cache_keepalive_sessions();
        }
        let version = self.state.events.request_log_version();
        if version != self.last_seen_request_log_version {
            self.last_seen_request_log_version = version;
            self.refresh_all();
            return;
        }
        if self.last_request_log_poll_at.elapsed() < REQUEST_LOG_POLL_INTERVAL {
            return;
        }
        self.last_request_log_poll_at = Instant::now();
        let log_count = self.runtime.block_on(self.state.store.request_log_count());
        match log_count {
            Ok(count) if count != self.log_total_count => {
                tracing::debug!(
                    previous = self.log_total_count,
                    current = count,
                    "request log count changed, refreshing current log page"
                );
                self.refresh_all();
            }
            Ok(_) => {}
            Err(err) => {
                tracing::debug!(error = %err, "failed to poll request log count");
            }
        }
    }

    fn ensure_tray(&mut self, ctx: &egui::Context) {
        if self.tray.is_some() || self.tray_init_failed {
            return;
        }
        let tx = self.task_tx.clone();
        let tray = TrayController::new(self.server.is_some(), ctx.clone(), move |command| {
            if let Err(err) = tx.send(UiTaskEvent::Tray(command)) {
                tracing::debug!(error = %err, "failed to send tray command");
            }
        });
        match tray {
            Ok(tray) => {
                self.tray = Some(tray);
            }
            Err(err) => {
                self.tray_init_failed = true;
                self.status = format!("系统托盘初始化失败: {err}");
                tracing::warn!(error = %err, "failed to initialize system tray");
            }
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.viewport().close_requested()) {
            return;
        }
        if self.exit_requested || self.tray.is_none() {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.window_hidden_to_tray = true;
        self.background_reopen.mark_hidden();
        platform::hide_from_dock();
        self.status = "窗口已隐藏到系统托盘".to_string();
    }

    fn handle_dock_reopen(&mut self, ctx: &egui::Context) {
        if !self.window_hidden_to_tray {
            return;
        }
        if self.background_reopen.should_show_hidden_window() {
            self.show_main_window(ctx);
        }
    }

    fn handle_tray_command(&mut self, ctx: &egui::Context, command: TrayCommand) {
        match command {
            TrayCommand::ShowWindow => self.show_main_window(ctx),
            TrayCommand::ToggleService => {
                if self.server.is_some() {
                    self.stop_server();
                } else {
                    self.start_server();
                }
            }
            TrayCommand::Quit => {
                self.exit_app(ctx);
            }
        }
    }

    fn exit_app(&mut self, ctx: &egui::Context) {
        self.exit_requested = true;
        self.stop_server();
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn show_main_window(&mut self, ctx: &egui::Context) {
        platform::show_in_dock();
        self.window_hidden_to_tray = false;
        self.background_reopen.mark_shown();
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.status = "主界面已打开".to_string();
        self.refresh_all();
    }

    fn sync_tray_service_state(&mut self) {
        if let Some(tray) = &self.tray {
            tray.set_server_running(self.server.is_some());
        }
    }

    fn drain_task_events(&mut self, ctx: &egui::Context) {
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
                            self.refresh_all_if_visible();
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
                            self.refresh_all_if_visible();
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
                            self.refresh_all_if_visible();
                        }
                        Err(err) => self.status = format!("余额查询失败: {err}"),
                    }
                }
                UiTaskEvent::PriceCacheFetched(result) => {
                    self.price_fetch_pending = false;
                    match result {
                        Ok(count) => {
                            self.status = format!("模型价格已获取: {count} 条");
                            self.refresh_all_if_visible();
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
                                self.refresh_all_if_visible();
                            } else if summary.count > 0 {
                                self.status = format!("模型价格缓存可用: {} 条", summary.count);
                            }
                        }
                        Err(err) => {
                            self.status = format!("模型价格获取失败, 将使用已有缓存: {err}");
                        }
                    }
                }
                UiTaskEvent::Tray(command) => self.handle_tray_command(ctx, command),
            }
        }
    }

    fn refresh_all(&mut self) {
        let log_limit = self.log_page_size as i64;
        let log_offset = (self.log_page * self.log_page_size) as i64;
        match self.runtime.block_on(load_view_data(
            &self.state,
            log_limit,
            log_offset,
            &self.log_runtime_filter,
        )) {
            Ok(data) => {
                self.upstreams = data.upstreams;
                self.cache_keepalive_settings = data.cache_keepalive_settings;
                self.schedule_groups = data.schedule_groups;
                self.schedule_members = data.schedule_members;
                self.schedule_children = data.schedule_children;
                self.schedule_route_rules = data.schedule_route_rules;
                self.scheduler_route_max_hops = data.scheduler_route_max_hops;
                self.current_schedule_group_id = data.current_schedule_group_id;
                self.sync_schedule_group_editor();
                self.stats = data.stats;
                self.provider_stats = data.provider_stats;
                self.logs = data.logs;
                self.refresh_cache_keepalive_sessions();
                self.log_total_count = data.log_total_count;
                self.last_seen_request_log_version = self.state.events.request_log_version();
                self.total_estimated_cost_usd = data.total_estimated_cost_usd;
                self.today_estimated_cost_usd = data.today_estimated_cost_usd;
                self.provider_estimated_cost_usd = data.provider_estimated_cost_usd;
                self.log_estimated_cost_usd = data.log_estimated_cost_usd;
                self.price_cache_count = data.price_cache_count;
                self.price_cache_age_seconds = data.price_cache_age_seconds;
                self.database_info = data.database_info;
                self.quota_snapshots = data.quota_snapshots;
                self.balance_snapshots = data.balance_snapshots;
            }
            Err(err) => {
                self.status = format!("刷新失败: {err}");
            }
        }
    }

    fn refresh_all_if_visible(&mut self) {
        if self.window_hidden_to_tray {
            return;
        }
        self.refresh_all();
    }

    fn refresh_cache_keepalive_sessions(&mut self) {
        self.cache_keepalive_sessions = self
            .runtime
            .block_on(self.state.cache_keepalive.snapshots());
        tracing::trace!(
            count = self.cache_keepalive_sessions.len(),
            "cache keepalive sessions refreshed"
        );
        self.last_cache_keepalive_refresh_at = Instant::now();
        self.last_seen_cache_keepalive_version = self.state.events.cache_keepalive_version();
        if self
            .selected_cache_keepalive_key
            .as_ref()
            .is_some_and(|key| {
                self.cache_keepalive_sessions
                    .iter()
                    .any(|session| &session.key == key)
            })
        {
            return;
        }
        self.selected_cache_keepalive_key = self
            .cache_keepalive_sessions
            .first()
            .map(|session| session.key.clone());
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
            self.sync_tray_service_state();
            return;
        }
        let bind_addr = self.bind_addr.clone();
        if let Err(err) = self
            .runtime
            .block_on(self.state.store.set_setting("bind_addr", &bind_addr))
        {
            self.status = format!("保存监听地址失败: {err}");
            self.sync_tray_service_state();
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
                self.sync_tray_service_state();
            }
            Err(err) => {
                self.status = format!("服务启动失败: {err}");
                self.sync_tray_service_state();
            }
        }
    }

    fn stop_server(&mut self) {
        if let Some(handle) = self.server.take() {
            handle.stop();
            self.status = "服务已停止".to_string();
        }
        self.sync_tray_service_state();
    }

    fn refresh_local_key(&mut self, key: String) {
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
        let proxy_url = self.relay_proxy_url.trim().to_string();
        let api_key = self.relay_api_key.trim().to_string();
        if name.is_empty() || base_url.is_empty() || api_key.is_empty() {
            self.status = "名称, Base URL 和 API Key 都不能为空".to_string();
            return;
        }
        if let Err(err) = http::validate_proxy_url(&proxy_url) {
            self.status = format!("代理 URL 无效: {err}");
            return;
        }
        let provider = balance::detect_provider(&base_url).unwrap_or(BalanceProvider::Auto);
        let mut upstream = Upstream::new_relay(
            name,
            base_url,
            self.relay_wire_api,
            self.relay_supports_compact,
            provider,
        );
        if !proxy_url.is_empty() {
            upstream.proxy_url = Some(proxy_url);
        }
        let result = self.runtime.block_on(async {
            self.state.store.save_upstream(&upstream).await?;
            self.state
                .credentials
                .put(&upstream.id, balance::API_KEY_CREDENTIAL, &api_key)
                .await?;
            anyhow::Ok(())
        });
        match result {
            Ok(()) => {
                self.relay_name.clear();
                self.relay_base_url.clear();
                self.relay_proxy_url.clear();
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
        self.ensure_tray(ctx);
        self.handle_close_request(ctx);
        self.handle_dock_reopen(ctx);
        self.maybe_auto_refresh(ctx);
        self.drain_task_events(ctx);

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                tab_button(ui, &mut self.tab, Tab::Dashboard, "仪表盘");
                tab_button(ui, &mut self.tab, Tab::Upstreams, "上游");
                tab_button(ui, &mut self.tab, Tab::Scheduler, "调度组");
                tab_button(
                    ui,
                    &mut self.tab,
                    Tab::CacheKeepalive,
                    &cache_keepalive_tab_text(self.cache_keepalive_sessions.len()),
                );
                tab_button(
                    ui,
                    &mut self.tab,
                    Tab::ActiveConnections,
                    &active_connections_tab_text(self.live_connections.len()),
                );
                tab_button(ui, &mut self.tab, Tab::Logs, "日志");
                if ui.button("刷新").clicked() {
                    self.refresh_all();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("退出").clicked() {
                        self.exit_confirm_open = true;
                    }
                });
            });
        });
        self.exit_confirm_window(ctx);

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(&self.status);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Dashboard => self.dashboard_ui(ui),
            Tab::Upstreams => self.upstreams_ui(ui),
            Tab::Scheduler => self.scheduler_ui(ui),
            Tab::CacheKeepalive => self.cache_keepalive_ui(ui),
            Tab::ActiveConnections => self.active_connections_ui(ui),
            Tab::Logs => self.logs_ui(ui),
        });
    }
}

impl CodexSwitchApp {
    fn exit_confirm_window(&mut self, ctx: &egui::Context) {
        if !self.exit_confirm_open {
            return;
        }
        let mut open = self.exit_confirm_open;
        egui::Window::new("确认退出")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("确认退出 Codex Switch?");
                ui.horizontal(|ui| {
                    if ui.button("退出").clicked() {
                        self.exit_confirm_open = false;
                        self.exit_app(ctx);
                    }
                    if ui.button("取消").clicked() {
                        self.exit_confirm_open = false;
                    }
                });
            });
        self.exit_confirm_open = open && self.exit_confirm_open;
    }
}

fn tab_button(ui: &mut egui::Ui, tab: &mut Tab, value: Tab, text: &str) {
    if ui.selectable_label(*tab == value, text).clicked() {
        *tab = value;
    }
}

fn active_connections_tab_text(count: usize) -> String {
    format!("活跃连接({:03})", count.min(ACTIVE_TAB_COUNT_MAX))
}

fn cache_keepalive_tab_text(count: usize) -> String {
    format!("缓存保持({:03})", count.min(ACTIVE_TAB_COUNT_MAX))
}

fn validate_i64_range(label: &str, value: &I64RangeFilter) -> Result<(), String> {
    let min = value.min_value(label)?;
    let max = value.max_value(label)?;
    if let (Some(min), Some(max)) = (min, max)
        && min > max
    {
        return Err(format!("{label} 最小值不能大于最大值"));
    }
    Ok(())
}

fn validate_token_range(label: &str, value: &I64RangeFilter) -> Result<(), String> {
    let min = value.min_token_value(label)?;
    let max = value.max_token_value(label)?;
    if let (Some(min), Some(max)) = (min, max)
        && min > max
    {
        return Err(format!("{label} 最小值不能大于最大值"));
    }
    Ok(())
}

fn validate_f64_range(label: &str, value: &F64RangeFilter) -> Result<(), String> {
    let min = value.min_value(label)?;
    let max = value.max_value(label)?;
    if let (Some(min), Some(max)) = (min, max)
        && min > max
    {
        return Err(format!("{label} 最小值不能大于最大值"));
    }
    Ok(())
}

fn parse_optional_i64(label: &str, value: &str) -> Result<Option<i64>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<i64>()
        .map(Some)
        .map_err(|_| format!("{label} 需要填写整数"))
}

fn parse_optional_f64(label: &str, value: &str) -> Result<Option<f64>, String> {
    let value = value.trim().trim_start_matches('$');
    if value.is_empty() {
        return Ok(None);
    }
    let parsed = value
        .parse::<f64>()
        .map_err(|_| format!("{label} 需要填写数字"))?;
    if !parsed.is_finite() {
        return Err(format!("{label} 需要填写有效数字"));
    }
    Ok(Some(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_filter_state_parses_token_units() {
        let state = LogFilterState {
            input_tokens: I64RangeFilter {
                min: "64K".to_string(),
                max: String::new(),
            },
            output_tokens: I64RangeFilter {
                min: String::new(),
                max: "1.5M".to_string(),
            },
            total_tokens: I64RangeFilter {
                min: "2B".to_string(),
                max: String::new(),
            },
            ..Default::default()
        };

        let filter = state.to_runtime_filter().unwrap();

        assert_eq!(filter.input_tokens_min, Some(64_000));
        assert_eq!(filter.output_tokens_max, Some(1_500_000));
        assert_eq!(filter.total_tokens_min, Some(2_000_000_000));
    }
}

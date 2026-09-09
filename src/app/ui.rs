use super::window_state::{self, PersistedWindowSettings};
use crate::app::tray::{TrayBadgeMetric, TrayCommand, TrayController, TrayStats};
use crate::app::{http, platform, state::AppState};
use crate::balance;
use crate::cache_keepalive::CacheKeepaliveSessionSnapshot;
use crate::core::model_capabilities::ModelCapabilityCache;
use crate::core::models::{
    ApiKeyAuthScheme, BalanceProvider, BalanceSnapshot, DashboardStats, DatabaseInfo, NodePeer,
    PeerPairingRequest, ProviderStats, QuotaSnapshot, RequestLog, ScheduleGroup,
    ScheduleGroupChild, ScheduleGroupMember, ScheduleRouteRule, TemporaryAccessKey, Upstream,
    UpstreamBalanceAlertSettings, UpstreamCacheKeepaliveSettings, WireApi,
};
use crate::core::upstream_detection::{self, DetectedKind};
use crate::live::{LiveOutputSettings, LiveRequestSnapshot};
use crate::peer::discovery::DiscoveredPeer;
use crate::pricing;
use crate::proxy::{self, ServerHandle};
use crate::quota as quota_api;
use crate::storage::RequestLogFilter;
use crate::update::UpdateState;
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
const TRAY_STATS_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// 字节数的人类可读展示, total 存在时输出 "x / y" 形式.
fn format_bytes(received: u64, total: Option<u64>) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    let pretty = |value: u64| {
        let value = value as f64;
        if value >= MB {
            format!("{:.1} MB", value / MB)
        } else if value >= KB {
            format!("{:.1} KB", value / KB)
        } else {
            format!("{value} B")
        }
    };
    match total {
        Some(total) => format!("{} / {}", pretty(received), pretty(total)),
        None => pretty(received),
    }
}

fn model_modality_label(
    cache: &ModelCapabilityCache,
    upstream_id: Option<&str>,
    model: Option<&str>,
) -> String {
    let Some(model) = model.filter(|value| !value.is_empty()) else {
        return "未知".to_string();
    };
    match cache.get(upstream_id.unwrap_or_default(), model) {
        Some(true) => "多模态".to_string(),
        Some(false) => "文本".to_string(),
        None => "未知".to_string(),
    }
}

mod active;
mod cache_keepalive;
mod dashboard;
mod data;
mod logs;
mod model_test;
mod oauth;
mod peers;
mod quota;
mod scheduler;
mod temp_keys;
mod token_amount;
mod tokens;
mod upstream_editor;
mod upstreams;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Dashboard,
    Upstreams,
    Peers,
    Scheduler,
    CacheKeepalive,
    ActiveConnections,
    TempKeys,
    ModelTest,
    Logs,
}

#[derive(Debug, Clone, Copy)]
enum ScheduleRuleOwner {
    NewGroup,
    GroupEditor,
}

#[derive(Debug, Clone)]
enum DeleteAction {
    Upstream(String),
    ScheduleGroup(String),
    TemporaryAccessKey(String),
    ScheduleRouteRule {
        owner: ScheduleRuleOwner,
        id: String,
    },
}

#[derive(Debug, Clone)]
struct DeleteConfirmation {
    title: String,
    message: String,
    action: DeleteAction,
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
    target_model: Option<String>,
    upstream: Option<String>,
    reasoning_effort: Option<String>,
    endpoint: Option<String>,
    status: LogStatusFilter,
    source: LogSourceFilter,
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
enum LogSourceFilter {
    #[default]
    All,
    Proxy,
    TestBench,
}

impl LogSourceFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "全部来源",
            Self::Proxy => "仅代理",
            Self::TestBench => "仅测试台",
        }
    }
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
            self.target_model.is_some(),
            self.upstream.is_some(),
            self.reasoning_effort.is_some(),
            self.endpoint.is_some(),
            self.status != LogStatusFilter::All,
            self.source != LogSourceFilter::All,
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
        let source = match self.source {
            LogSourceFilter::All => None,
            LogSourceFilter::Proxy => Some(crate::core::models::RequestLogSource::Proxy),
            LogSourceFilter::TestBench => Some(crate::core::models::RequestLogSource::TestBench),
        };
        let started_at = self.started_at.to_utc("开始时间")?;
        let ended_at = self.ended_at.to_utc("结束时间")?;
        if let (Some(started_at), Some(ended_at)) = (started_at, ended_at)
            && started_at > ended_at
        {
            return Err("开始时间不能晚于结束时间".to_string());
        }

        Ok(RequestLogFilter {
            model: self.model.clone(),
            target_model: self.target_model.clone(),
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
            source,
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
    OAuthStarted {
        task_id: String,
        result: anyhow::Result<crate::oauth::DeviceFlow>,
    },
    OAuthPolled {
        task_id: String,
        result: anyhow::Result<oauth::OAuthPollTaskResult>,
    },
    OAuthImportProgress {
        batch_id: String,
        progress: crate::oauth::OAuthImportProgress,
    },
    OAuthImportFinished {
        batch_id: String,
        result: crate::oauth::OAuthImportBatchResult,
    },
    QuotaQueried(anyhow::Result<()>),
    BalanceQueried {
        upstream_id: String,
        result: anyhow::Result<()>,
    },
    PriceCacheFetched {
        price: anyhow::Result<usize>,
        fx: anyhow::Result<Option<pricing::fx::UsdCnyRate>>,
    },
    PriceCacheOnceFetched {
        price: anyhow::Result<pricing::PriceFetchSummary>,
        fx: anyhow::Result<Option<pricing::fx::UsdCnyRate>>,
    },
    PeerPaired(anyhow::Result<String>),
    ModelTestModelsFetched {
        upstream_id: String,
        result: anyhow::Result<Vec<String>>,
    },
    ModelTestDelta {
        kind: model_test::ModelTestKind,
        part: crate::proxy::forward::model_test::ModelTestStreamPart,
    },
    ModelTestFinished {
        kind: model_test::ModelTestKind,
        result: crate::proxy::forward::model_test::ModelTestOutcome,
    },
    Tray(TrayCommand),
}

pub struct CodexSwitchApp {
    runtime: Arc<Runtime>,
    state: AppState,
    task_tx: UnboundedSender<UiTaskEvent>,
    task_rx: UnboundedReceiver<UiTaskEvent>,
    tab: Tab,
    server: Option<ServerHandle>,
    peer_server: Option<crate::peer::server::PeerServerHandle>,
    tray: Option<TrayController>,
    tray_init_failed: bool,
    tray_badge_metric: TrayBadgeMetric,
    tray_badge_metric_secondary: TrayBadgeMetric,
    last_tray_stats_refresh_at: Instant,
    exit_requested: bool,
    exit_confirm_open: bool,
    update_window_open: bool,
    delete_confirmation: Option<DeleteConfirmation>,
    log_filter_open: bool,
    log_cleanup_open: bool,
    window_hidden_to_tray: bool,
    hide_on_launch: bool,
    start_server_on_launch: bool,
    start_peer_on_launch: bool,
    #[cfg(target_os = "macos")]
    dock_icon_follows_window: bool,
    last_good_window: Option<PersistedWindowSettings>,
    background_reopen: platform::BackgroundReopenMonitor,
    last_seen_show_window_version: u64,
    last_seen_exit_request_version: u64,
    bind_addr: String,
    local_key: String,
    local_key_copied_at: Option<Instant>,
    local_key_refresh_open: bool,
    local_key_refresh_value: String,
    last_seen_request_log_version: u64,
    last_request_log_poll_at: Instant,
    last_seen_live_stream_version: u64,
    last_live_output_rate_refresh_at: Instant,
    last_seen_cache_keepalive_version: u64,
    last_seen_balance_snapshot_version: u64,
    last_seen_peer_version: u64,
    last_cache_keepalive_refresh_at: Instant,
    price_fetch_started: bool,
    price_fetch_pending: bool,
    status: String,
    upstreams: Vec<Upstream>,
    cache_keepalive_settings: BTreeMap<String, UpstreamCacheKeepaliveSettings>,
    balance_alert_settings: BTreeMap<String, UpstreamBalanceAlertSettings>,
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
    live_output_settings: LiveOutputSettings,
    live_tail_scroll_states: BTreeMap<String, active::LiveTailScrollState>,
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
    usd_cny_rate: Option<pricing::fx::UsdCnyRate>,
    currency_display_mode: tokens::CurrencyDisplayMode,
    database_info: DatabaseInfo,
    token_display_mode: tokens::TokenDisplayMode,
    debug_log_enabled: bool,
    debug_log_path: String,
    proxy_log_path: String,
    network_har_path: String,
    log_rotation_size_mb: u64,
    log_max_files: usize,
    oauth_ui: oauth::OAuthUiState,
    quota_query_pending: bool,
    balance_query_pending_ids: BTreeSet<String>,
    relay_name: String,
    relay_base_url: String,
    relay_proxy_url: String,
    relay_api_key: String,
    relay_wire_api: WireApi,
    relay_api_key_auth_scheme: ApiKeyAuthScheme,
    relay_supports_compact: bool,
    relay_filter_chat_server_tools: bool,
    /// 用户是否手动选过 Wire API 与认证方式, 选过之后不再被识别结果覆盖.
    relay_wire_api_touched: bool,
    relay_auth_touched: bool,
    /// 用户是否手动勾过过滤 server_tool.
    relay_filter_touched: bool,
    quota_snapshots: Vec<(String, Option<QuotaSnapshot>)>,
    balance_snapshots: Vec<(String, Option<BalanceSnapshot>)>,
    upstream_editor: Option<UpstreamEditor>,
    upstream_import_open: bool,
    upstream_import_text: String,
    node_fingerprint: String,
    node_id: String,
    node_display_name: String,
    peer_bind_addr: String,
    mdns_discovery_enabled: bool,
    lnd_discovery_enabled: bool,
    lnd_server_url: String,
    lnd_bearer_token: String,
    lnd_discovery_domain: String,
    lnd_settings_open: bool,
    lnd_settings_url: String,
    lnd_settings_token: String,
    lnd_settings_domain: String,
    peer_direct_address: String,
    peer_direct_fingerprint: String,
    pairing_pending: bool,
    node_peers: Vec<NodePeer>,
    pairing_requests: Vec<PeerPairingRequest>,
    discovered_peers: Vec<DiscoveredPeer>,
    temporary_access_keys: Vec<TemporaryAccessKey>,
    temp_keys_ui: temp_keys::TempKeysUiState,
    model_test_ui: model_test::ModelTestUiState,
}

impl CodexSwitchApp {
    pub fn new(
        runtime: Arc<Runtime>,
        state: AppState,
        egui_ctx: egui::Context,
        storage: Option<&dyn eframe::Storage>,
        hide_on_launch: bool,
    ) -> Self {
        let repaint_ctx = egui_ctx.clone();
        state.events.set_repaint_requester(move || {
            repaint_ctx.request_repaint();
        });
        let persisted_window = storage.and_then(window_state::PersistedWindowSettings::load);
        let last_good_window = persisted_window
            .as_ref()
            .filter(|settings| settings.is_valid())
            .cloned();
        if persisted_window
            .as_ref()
            .is_some_and(|settings| !settings.is_valid())
        {
            tracing::warn!("invalid persisted window state, using default window size");
            egui_ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                window_state::DEFAULT_WINDOW_SIZE,
            ));
        }
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
        let live_output_settings = LiveOutputSettings::default();
        let rotation_config = runtime
            .block_on(crate::logging::LogRotationConfig::load(&state.store))
            .unwrap_or_default();
        let debug_log_path = crate::logging::main_log_file_path()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let proxy_log_path = crate::logging::proxy_log_file_path()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let network_har_path = crate::logging::network_har_file_path()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let last_seen_request_log_version = state.events.request_log_version();
        let last_seen_live_stream_version = state.events.live_stream_version();
        let last_seen_cache_keepalive_version = state.events.cache_keepalive_version();
        let last_seen_balance_snapshot_version = state.events.balance_snapshot_version();
        let last_seen_peer_version = state.events.peer_version();
        let tray_badge_metric = runtime
            .block_on(state.store.get_setting("tray_badge_metric"))
            .ok()
            .flatten()
            .and_then(|value| value.parse().ok())
            .unwrap_or_default();
        let tray_badge_metric_secondary = runtime
            .block_on(state.store.get_setting("tray_badge_metric_secondary"))
            .ok()
            .flatten()
            .and_then(|value| value.parse().ok())
            .unwrap_or(TrayBadgeMetric::None);
        let usd_cny_rate = runtime
            .block_on(pricing::fx::load_usd_cny_rate(&state))
            .ok()
            .flatten();
        #[cfg(target_os = "macos")]
        let dock_icon_follows_window = runtime
            .block_on(
                state
                    .store
                    .get_setting(super::SETTING_DOCK_ICON_FOLLOWS_WINDOW),
            )
            .ok()
            .flatten()
            .as_deref()
            != Some("false");
        let start_server_on_launch = runtime
            .block_on(
                state
                    .store
                    .get_setting(super::SETTING_START_SERVER_ON_LAUNCH),
            )
            .ok()
            .flatten()
            .as_deref()
            == Some("true");
        let start_peer_on_launch = runtime
            .block_on(state.store.get_setting(super::SETTING_START_PEER_ON_LAUNCH))
            .ok()
            .flatten()
            .as_deref()
            == Some("true");
        let last_seen_show_window_version = state.events.show_window_version();
        let last_seen_exit_request_version = state.events.exit_request_version();
        let mut app = Self {
            runtime,
            state,
            task_tx,
            task_rx,
            tab: Tab::Dashboard,
            server: None,
            peer_server: None,
            tray: None,
            tray_init_failed: false,
            tray_badge_metric,
            tray_badge_metric_secondary,
            last_tray_stats_refresh_at: Instant::now(),
            exit_requested: false,
            exit_confirm_open: false,
            update_window_open: false,
            delete_confirmation: None,
            log_filter_open: false,
            log_cleanup_open: false,
            window_hidden_to_tray: hide_on_launch,
            hide_on_launch,
            start_server_on_launch,
            start_peer_on_launch,
            #[cfg(target_os = "macos")]
            dock_icon_follows_window,
            last_good_window,
            background_reopen: platform::BackgroundReopenMonitor::default(),
            last_seen_show_window_version,
            last_seen_exit_request_version,
            bind_addr,
            local_key,
            local_key_copied_at: None,
            local_key_refresh_open: false,
            local_key_refresh_value: String::new(),
            last_seen_request_log_version,
            last_request_log_poll_at: Instant::now(),
            last_seen_live_stream_version,
            last_live_output_rate_refresh_at: Instant::now(),
            last_seen_cache_keepalive_version,
            last_seen_balance_snapshot_version,
            last_seen_peer_version,
            last_cache_keepalive_refresh_at: Instant::now(),
            price_fetch_started: false,
            price_fetch_pending: false,
            status: "就绪".to_string(),
            upstreams: Vec::new(),
            cache_keepalive_settings: BTreeMap::new(),
            balance_alert_settings: BTreeMap::new(),
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
            live_output_settings,
            live_tail_scroll_states: BTreeMap::new(),
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
            usd_cny_rate,
            currency_display_mode: tokens::CurrencyDisplayMode::Usd,
            database_info: DatabaseInfo::default(),
            token_display_mode: tokens::TokenDisplayMode::Human,
            debug_log_enabled: rotation_config.enabled,
            debug_log_path,
            proxy_log_path,
            network_har_path,
            log_rotation_size_mb: rotation_config.size_mb,
            log_max_files: rotation_config.max_files,
            oauth_ui: oauth::OAuthUiState::default(),
            quota_query_pending: false,
            balance_query_pending_ids: BTreeSet::new(),
            relay_name: String::new(),
            relay_base_url: String::new(),
            relay_proxy_url: String::new(),
            relay_api_key: String::new(),
            relay_wire_api: WireApi::Responses,
            relay_api_key_auth_scheme: ApiKeyAuthScheme::Bearer,
            relay_supports_compact: true,
            relay_filter_chat_server_tools: false,
            relay_wire_api_touched: false,
            relay_auth_touched: false,
            relay_filter_touched: false,
            quota_snapshots: Vec::new(),
            balance_snapshots: Vec::new(),
            upstream_editor: None,
            upstream_import_open: false,
            upstream_import_text: String::new(),
            node_fingerprint: String::new(),
            node_id: String::new(),
            node_display_name: String::new(),
            peer_bind_addr: crate::peer::protocol::DEFAULT_PEER_BIND_ADDR.to_string(),
            mdns_discovery_enabled: false,
            lnd_discovery_enabled: false,
            lnd_server_url: String::new(),
            lnd_bearer_token: String::new(),
            lnd_discovery_domain: String::new(),
            lnd_settings_open: false,
            lnd_settings_url: String::new(),
            lnd_settings_token: String::new(),
            lnd_settings_domain: String::new(),
            peer_direct_address: String::new(),
            peer_direct_fingerprint: String::new(),
            pairing_pending: false,
            node_peers: Vec::new(),
            pairing_requests: Vec::new(),
            discovered_peers: Vec::new(),
            temporary_access_keys: Vec::new(),
            temp_keys_ui: temp_keys::TempKeysUiState::default(),
            model_test_ui: model_test::ModelTestUiState::default(),
        };
        let _ = crate::logging::set_debug_log_enabled(app.debug_log_enabled);
        if hide_on_launch {
            // 启动时仅驻留托盘: 窗口以不可见状态创建, dock 图标按设置一并隐藏.
            #[cfg(target_os = "macos")]
            {
                if dock_icon_follows_window {
                    platform::hide_from_dock();
                }
            }
            app.background_reopen.mark_hidden();
            app.status = "已按设置在后台启动, 可从系统托盘打开主界面".to_string();
            tracing::info!("app started hidden to tray (hide on launch enabled)");
        }
        app.refresh_all();
        app.fetch_price_cache_once();
        // 自动启动放在 refresh_all 之后, 保证监听地址等配置已完成加载.
        if start_server_on_launch {
            tracing::info!("auto-starting proxy server on launch");
            app.start_server();
        }
        if start_peer_on_launch {
            tracing::info!("auto-starting peer listener on launch");
            app.start_peer_server();
        }
        app
    }

    fn maybe_auto_refresh(&mut self, ctx: &egui::Context) {
        self.drive_oauth_tasks();
        self.update_live_connections(ctx);
        if self.window_hidden_to_tray {
            ctx.request_repaint_after(HIDDEN_REPAINT_INTERVAL);
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(500));
        let cache_keepalive_version = self.state.events.cache_keepalive_version();
        if cache_keepalive_version != self.last_seen_cache_keepalive_version {
            self.last_seen_cache_keepalive_version = cache_keepalive_version;
            self.refresh_cache_keepalive_sessions();
        }
        let peer_version = self.state.events.peer_version();
        if peer_version != self.last_seen_peer_version {
            self.last_seen_peer_version = peer_version;
            self.refresh_peer_lists();
        }
        let balance_snapshot_version = self.state.events.balance_snapshot_version();
        if balance_snapshot_version != self.last_seen_balance_snapshot_version {
            self.last_seen_balance_snapshot_version = balance_snapshot_version;
            self.refresh_all();
            return;
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
        let auto_check_updates = self.state.update.auto_check_value();
        let tray = TrayController::new(
            self.server.is_some(),
            auto_check_updates,
            self.tray_badge_metric,
            self.tray_badge_metric_secondary,
            ctx.clone(),
            move |command| {
                if let Err(err) = tx.send(UiTaskEvent::Tray(command)) {
                    tracing::debug!(error = %err, "failed to send tray command");
                }
            },
        );
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
        tracing::info!("main window hidden to tray");
        #[cfg(target_os = "macos")]
        {
            if self.dock_icon_follows_window {
                platform::hide_from_dock();
            }
        }
        #[cfg(not(target_os = "macos"))]
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

    /// 处理来自单实例通知与 Ctrl+C 信号的外部请求.
    fn handle_app_events(&mut self, ctx: &egui::Context) {
        let exit_version = self.state.events.exit_request_version();
        if exit_version != self.last_seen_exit_request_version {
            self.last_seen_exit_request_version = exit_version;
            if !self.exit_requested {
                tracing::info!("graceful exit requested by signal");
                self.exit_app(ctx);
            }
            return;
        }
        let show_window_version = self.state.events.show_window_version();
        if show_window_version != self.last_seen_show_window_version {
            self.last_seen_show_window_version = show_window_version;
            if !self.exit_requested {
                self.show_main_window(ctx);
            }
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
            TrayCommand::CheckUpdates => {
                self.state.update.check_now();
                self.update_window_open = true;
                self.status = "正在检查更新...".to_string();
            }
            TrayCommand::ToggleAutoCheck => {
                let enabled = !self.state.update.auto_check_value();
                self.runtime
                    .block_on(self.state.update.set_auto_check(enabled));
                if let Some(tray) = &self.tray {
                    tray.set_auto_check_checked(enabled);
                }
                self.status = if enabled {
                    "已开启启动时自动检查更新".to_string()
                } else {
                    "已关闭启动时自动检查更新".to_string()
                };
            }
            TrayCommand::ThemeChanged(dark) => {
                if let Some(tray) = &mut self.tray
                    && let Err(err) = tray.set_theme(dark)
                {
                    tracing::warn!(error = %err, "failed to update tray icon for theme");
                }
            }
            TrayCommand::SetBadgeMetric(metric) => {
                self.tray_badge_metric = metric;
                if let Err(err) = self.runtime.block_on(
                    self.state
                        .store
                        .set_setting("tray_badge_metric", metric.as_str()),
                ) {
                    self.status = format!("保存托盘标题行 1 设置失败: {err}");
                } else {
                    self.status = format!("托盘标题行 1 已切换: {}", metric.label());
                }
                if let Some(tray) = &mut self.tray {
                    tray.set_badge_metric(metric);
                }
            }
            TrayCommand::SetBadgeMetricSecondary(metric) => {
                self.tray_badge_metric_secondary = metric;
                if let Err(err) = self.runtime.block_on(
                    self.state
                        .store
                        .set_setting("tray_badge_metric_secondary", metric.as_str()),
                ) {
                    self.status = format!("保存托盘标题行 2 设置失败: {err}");
                } else {
                    self.status = format!("托盘标题行 2 已切换: {}", metric.label());
                }
                if let Some(tray) = &mut self.tray {
                    tray.set_badge_metric_secondary(metric);
                }
            }
        }
    }

    fn exit_app(&mut self, ctx: &egui::Context) {
        self.exit_requested = true;
        self.stop_peer_server();
        self.stop_server();
        self.state.release_single_instance();
        tracing::info!("application exiting");
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
        self.last_tray_stats_refresh_at = Instant::now() - TRAY_STATS_REFRESH_INTERVAL;
        self.sync_tray_stats();
    }

    fn sync_tray_stats(&mut self) {
        if self.last_tray_stats_refresh_at.elapsed() < TRAY_STATS_REFRESH_INTERVAL {
            return;
        }
        self.last_tray_stats_refresh_at = Instant::now();
        let mut stats = TrayStats::from_live(
            &self.live_connections,
            self.stats.today_requests,
            self.cache_keepalive_sessions.len(),
            self.server.is_some(),
        );

        let active_upstream_id = self
            .live_connections
            .iter()
            .rfind(|item| item.finished_at.is_none())
            .and_then(|item| {
                self.upstreams
                    .iter()
                    .find(|u| Some(&u.name) == item.upstream_name.as_ref())
                    .map(|u| u.id.clone())
            })
            .or_else(|| self.logs.first().and_then(|log| log.upstream_id.clone()));

        let current_balance = active_upstream_id.and_then(|id| {
            self.balance_snapshots
                .iter()
                .find(|(uid, _)| uid == &id)
                .and_then(|(_, snap)| snap.as_ref())
                .filter(|snap| snap.is_valid)
                .and_then(|snap| {
                    snap.remaining
                        .map(|rem| (rem, snap.unit.clone().unwrap_or_default()))
                })
        });

        stats.current_balance = current_balance.map(|x| (x.0, x.1.parse().unwrap()));
        if let Some(tray) = &mut self.tray {
            tray.set_stats(stats);
        }
    }

    fn drain_task_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.task_rx.try_recv() {
            match event {
                UiTaskEvent::OAuthStarted { task_id, result } => {
                    self.handle_oauth_started(task_id, result);
                }
                UiTaskEvent::OAuthPolled { task_id, result } => {
                    self.handle_oauth_polled(task_id, result);
                }
                UiTaskEvent::OAuthImportProgress { batch_id, progress } => {
                    self.handle_oauth_import_progress(batch_id, progress);
                }
                UiTaskEvent::OAuthImportFinished { batch_id, result } => {
                    self.handle_oauth_import_finished(batch_id, result);
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
                UiTaskEvent::PriceCacheFetched { price, fx } => {
                    self.price_fetch_pending = false;
                    let fx_suffix = self.apply_fx_result(&fx);
                    match price {
                        Ok(count) => {
                            self.status = format!("模型信息已获取: {count} 条{fx_suffix}");
                            self.refresh_all_if_visible();
                        }
                        Err(err) => self.status = format!("模型信息获取失败: {err}{fx_suffix}"),
                    }
                }
                UiTaskEvent::PeerPaired(result) => {
                    self.pairing_pending = false;
                    match result {
                        Ok(message) => {
                            self.status = message;
                            self.refresh_all_if_visible();
                        }
                        Err(err) => self.status = format!("节点配对失败: {err}"),
                    }
                }
                UiTaskEvent::PriceCacheOnceFetched { price, fx } => {
                    self.price_fetch_pending = false;
                    let fx_suffix = self.apply_fx_result(&fx);
                    match price {
                        Ok(summary) => {
                            if summary.fetched {
                                self.status =
                                    format!("模型信息已获取: {} 条{fx_suffix}", summary.count);
                                self.refresh_all_if_visible();
                            } else if summary.count > 0 {
                                self.status =
                                    format!("模型信息缓存可用: {} 条{fx_suffix}", summary.count);
                            }
                        }
                        Err(err) => {
                            self.status =
                                format!("模型信息获取失败, 将使用已有缓存: {err}{fx_suffix}");
                        }
                    }
                }
                UiTaskEvent::ModelTestModelsFetched {
                    upstream_id,
                    result,
                } => {
                    self.handle_model_test_models_fetched(upstream_id, result);
                }
                UiTaskEvent::ModelTestDelta { kind, part } => {
                    self.handle_model_test_delta(kind, part);
                }
                UiTaskEvent::ModelTestFinished { kind, result } => {
                    self.handle_model_test_finished(kind, result);
                }
                UiTaskEvent::Tray(command) => self.handle_tray_command(ctx, command),
            }
        }
    }

    fn refresh_from_button(&mut self) {
        self.restart_peer_discovery();
        self.refresh_all();
    }

    fn restart_peer_discovery(&mut self) {
        if self.peer_server.is_none() {
            self.state.peers.stop_discovery();
            return;
        }
        if let Err(err) = self.runtime.block_on(
            self.state
                .peers
                .start_discovery(&self.state.store, self.state.events.clone()),
        ) {
            self.status = format!("刷新节点发现失败: {err}");
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
                self.balance_alert_settings = data.balance_alert_settings;
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
                self.last_seen_balance_snapshot_version =
                    self.state.events.balance_snapshot_version();
                self.refresh_peer_views();
                self.temporary_access_keys = self
                    .runtime
                    .block_on(self.state.store.list_temporary_access_keys())
                    .unwrap_or_default();
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

    fn start_peer_server(&mut self) {
        if self.peer_server.is_some() {
            self.status = "节点监听已经在运行".to_string();
            return;
        }
        let bind_addr = self.peer_bind_addr.clone();
        if let Err(err) = self
            .runtime
            .block_on(self.state.store.set_setting("peer_bind_addr", &bind_addr))
        {
            self.status = format!("保存节点监听地址失败: {err}");
            return;
        }
        match self.runtime.block_on(proxy::start_peer_listener(
            bind_addr.clone(),
            self.state.clone(),
        )) {
            Ok(handle) => {
                self.peer_server = Some(handle);
                if let Err(err) = self.runtime.block_on(
                    self.state
                        .peers
                        .start_discovery(&self.state.store, self.state.events.clone()),
                ) {
                    self.stop_peer_server();
                    self.status = format!("节点发现启动失败: {err}");
                    return;
                }
                self.status = format!("节点监听已启动: {bind_addr}");
            }
            Err(err) => {
                self.status = format!("节点监听启动失败: {err}");
            }
        }
    }

    fn stop_peer_server(&mut self) {
        self.state.peers.stop_discovery();
        if let Some(handle) = self.peer_server.take() {
            handle.stop();
            self.status = "节点监听已停止".to_string();
        }
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

    /// 依据 Base URL 的识别结果刷新新增表单的默认值, 不发起网络请求.
    /// 只在表单尚未被手动调整过的维度上写入, 避免覆盖用户的选择.
    fn apply_relay_detection_hint(&mut self) {
        let detected = upstream_detection::detect_upstream(&self.relay_base_url);
        if detected.kind == DetectedKind::Unknown {
            return;
        }
        if let Some(wire_api) = detected.suggestion.wire_api
            && !self.relay_wire_api_touched
        {
            self.relay_wire_api = wire_api;
        }
        if let Some(scheme) = detected.suggestion.api_key_auth_scheme
            && !self.relay_auth_touched
        {
            self.relay_api_key_auth_scheme = scheme;
        }
        if let Some(filter) = detected.suggestion.filter_chat_server_tools
            && !self.relay_filter_touched
        {
            self.relay_filter_chat_server_tools = filter;
        }
        if self.relay_wire_api == WireApi::AnthropicMessages {
            self.relay_supports_compact = false;
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
        let detected = upstream_detection::detect_upstream(&base_url).kind;
        let mut upstream = Upstream::new_relay(
            name,
            base_url,
            self.relay_wire_api,
            self.relay_supports_compact,
            provider,
        );
        upstream.api_key_auth_scheme = self.relay_api_key_auth_scheme;
        upstream.filter_chat_server_tools = self.relay_filter_chat_server_tools;
        if upstream.wire_api == WireApi::AnthropicMessages {
            upstream.supports_compact = false;
        }
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
                self.relay_wire_api_touched = false;
                self.relay_auth_touched = false;
                self.relay_filter_touched = false;
                self.status = if detected == DetectedKind::Unknown {
                    "已添加 API Key 上游".to_string()
                } else {
                    format!("已添加 API Key 上游 (识别为 {})", detected.label())
                };
                self.refresh_all();
            }
            Err(err) => self.status = format!("添加失败: {err}"),
        }
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
        self.status = "正在获取模型信息和汇率".to_string();
        let state = self.state.clone();
        let tx = self.task_tx.clone();
        self.runtime.spawn(async move {
            let (price, fx) = tokio::join!(pricing::fetch_price_cache(&state), async {
                pricing::fx::fetch_usd_cny_rate(&state).await.map(Some)
            });
            let _ = tx.send(UiTaskEvent::PriceCacheFetched { price, fx });
        });
    }

    fn fetch_price_cache_once(&mut self) {
        if self.price_fetch_started {
            return;
        }
        self.price_fetch_started = true;
        self.price_fetch_pending = true;
        self.status = "正在检查模型信息缓存".to_string();
        let state = self.state.clone();
        let tx = self.task_tx.clone();
        self.runtime.spawn(async move {
            let (price, fx) = tokio::join!(
                pricing::fetch_price_cache_once(&state),
                pricing::fx::ensure_usd_cny_rate(&state, pricing::fx::RATE_MAX_AGE_SECS)
            );
            let _ = tx.send(UiTaskEvent::PriceCacheOnceFetched { price, fx });
        });
    }

    /// 应用汇率获取结果, 返回用于拼接状态文本的后缀.
    fn apply_fx_result(&mut self, fx: &anyhow::Result<Option<pricing::fx::UsdCnyRate>>) -> String {
        match fx {
            Ok(Some(rate)) => {
                self.usd_cny_rate = Some(*rate);
                format!(", 汇率 1 USD = {:.4} CNY", rate.rate)
            }
            Ok(None) => String::new(),
            Err(err) => {
                tracing::warn!(error = %err, "USD/CNY exchange rate fetch failed");
                format!(", 汇率获取失败: {err}")
            }
        }
    }
}

impl eframe::App for CodexSwitchApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.last_good_window =
            window_state::sanitize_on_save(storage, self.last_good_window.as_ref());
    }

    // Tray 命令必须在 logic 中处理, 因为隐藏窗口不会调用 ui.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_app_events(ctx);
        self.ensure_tray(ctx);
        self.handle_close_request(ctx);
        self.handle_dock_reopen(ctx);
        self.maybe_auto_refresh(ctx);
        self.sync_tray_stats();
        self.drain_task_events(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                tab_button(ui, &mut self.tab, Tab::Dashboard, "仪表盘");
                tab_button(ui, &mut self.tab, Tab::TempKeys, "临时 Key");
                tab_button(ui, &mut self.tab, Tab::Upstreams, "上游");
                tab_button(ui, &mut self.tab, Tab::Peers, "节点");
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
                    &active_connections_tab_text(active::active_connection_count(
                        &self.live_connections,
                    )),
                );
                tab_button(ui, &mut self.tab, Tab::ModelTest, "测试台");
                tab_button(ui, &mut self.tab, Tab::Logs, "日志");
                if ui.button("刷新").clicked() {
                    self.refresh_from_button();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("退出").clicked() {
                        self.exit_confirm_open = true;
                    }
                });
            });
        });
        self.exit_confirm_window(&ctx);

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(&self.status);
                ui.separator();
                match self.state.update.state() {
                    UpdateState::Available(info) => {
                        if ui
                            .link(egui::RichText::new(format!("新版本 {} 可用", info.tag)).strong())
                            .clicked()
                        {
                            self.update_window_open = true;
                        }
                    }
                    UpdateState::ReadyToRestart | UpdateState::DmgOpened => {
                        if ui
                            .link(egui::RichText::new("更新已就绪, 点击查看").strong())
                            .clicked()
                        {
                            self.update_window_open = true;
                        }
                    }
                    _ => {
                        ui.weak(crate::app::display_version());
                    }
                }
            });
        });

        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Dashboard => self.dashboard_ui(ui),
            Tab::Upstreams => self.upstreams_ui(ui),
            Tab::Peers => self.peers_ui(ui),
            Tab::Scheduler => self.scheduler_ui(ui),
            Tab::CacheKeepalive => self.cache_keepalive_ui(ui),
            Tab::ActiveConnections => self.active_connections_ui(ui),
            Tab::TempKeys => self.temp_keys_ui(ui),
            Tab::ModelTest => self.model_test_ui(ui),
            Tab::Logs => self.logs_ui(ui),
        });
        self.delete_confirmation_window(&ctx);
        self.update_window(&ctx);
    }
}

impl CodexSwitchApp {
    fn request_delete(
        &mut self,
        action: DeleteAction,
        title: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.delete_confirmation = Some(DeleteConfirmation {
            title: title.into(),
            message: message.into(),
            action,
        });
    }

    fn delete_confirmation_window(&mut self, ctx: &egui::Context) {
        let Some(confirmation) = self.delete_confirmation.clone() else {
            return;
        };
        let mut open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Window::new(&confirmation.title)
            .id(egui::Id::new("delete_confirmation"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(&confirmation.message);
                ui.horizontal(|ui| {
                    if ui.button("确认删除").clicked() {
                        confirmed = true;
                    }
                    if ui.button("取消").clicked() {
                        cancelled = true;
                    }
                });
            });
        if confirmed {
            self.delete_confirmation = None;
            self.execute_delete(confirmation.action);
        } else if cancelled || !open {
            self.delete_confirmation = None;
        }
    }

    fn execute_delete(&mut self, action: DeleteAction) {
        match action {
            DeleteAction::Upstream(id) => self.delete_upstream(&id),
            DeleteAction::ScheduleGroup(id) => self.delete_schedule_group(&id),
            DeleteAction::TemporaryAccessKey(id) => self.delete_temporary_access_key(&id),
            DeleteAction::ScheduleRouteRule { owner, id } => {
                let editor = match owner {
                    ScheduleRuleOwner::NewGroup => Some(&mut self.new_schedule_group),
                    ScheduleRuleOwner::GroupEditor => self.schedule_group_editor.as_mut(),
                };
                if let Some(editor) = editor {
                    editor.route_rules.retain(|rule| rule.id != id);
                    self.status = "调度规则已从编辑内容中删除".to_string();
                }
            }
        }
    }

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

    fn update_window(&mut self, ctx: &egui::Context) {
        if !self.update_window_open {
            return;
        }
        let update_state = self.state.update.state();
        let mut open = self.update_window_open;
        egui::Window::new("软件更新")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                self.update_window_body(ctx, ui, &update_state);
            });
        self.update_window_open = open;
    }

    fn update_window_body(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, state: &UpdateState) {
        ui.label(format!("当前版本: {}", crate::app::display_version()));
        match state {
            UpdateState::Idle | UpdateState::UpToDate => {
                ui.label("已是最新版本.");
                if ui.button("重新检查").clicked() {
                    self.state.update.check_now();
                    self.status = "正在检查更新...".to_string();
                }
            }
            UpdateState::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("正在检查更新...");
                });
            }
            UpdateState::Available(info) => {
                ui.label(format!("最新版本: {}", info.tag));
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        ui.set_min_width(360.0);
                        ui.label(&info.body);
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("立即更新").clicked() {
                        self.state.update.install_now();
                        self.status = "正在下载更新...".to_string();
                    }
                    if ui.button("跳过此版本").clicked() {
                        let tag = info.tag.clone();
                        self.runtime.block_on(self.state.update.skip_version(&tag));
                        self.update_window_open = false;
                        self.status = format!("已跳过版本 {tag}");
                    }
                    if ui.button("查看 Release 页").clicked() {
                        match platform::open_url(&info.html_url) {
                            Ok(()) => self.status = "已在浏览器打开 Release 页".to_string(),
                            Err(err) => self.status = format!("打开 Release 页失败: {err}"),
                        }
                    }
                });
            }
            UpdateState::Downloading { received, total } => {
                ui.label("正在下载更新...");
                match total {
                    Some(total) if *total > 0 => {
                        let progress = *received as f32 / *total as f32;
                        ui.add(
                            egui::ProgressBar::new(progress)
                                .show_percentage()
                                .desired_width(360.0),
                        );
                        ui.label(format_bytes(*received, Some(*total)));
                    }
                    _ => {
                        ui.spinner();
                        ui.label(format!("已下载 {}", format_bytes(*received, None)));
                    }
                }
                if ui.button("取消更新").clicked() {
                    self.state.update.cancel_install();
                    self.status = "已取消更新下载".to_string();
                }
            }
            UpdateState::ReadyToRestart => {
                ui.label("更新已安装完成, 重启应用后生效.");
                if ui.button("重启应用").clicked() {
                    // 先释放单实例锁, 避免新进程在旧进程退出前抢锁失败.
                    self.state.release_single_instance();
                    match self.state.update.restart() {
                        Ok(()) => self.exit_app(ctx),
                        Err(err) => {
                            tracing::warn!(error = %err, "failed to restart app after update");
                            self.status = format!("重启失败: {err}");
                        }
                    }
                }
            }
            UpdateState::DmgOpened => {
                ui.label(
                    "安装镜像已在系统中打开, 请将 Codex Switch 拖入 Applications 文件夹完成安装, \
                     之后重新启动应用.",
                );
            }
            UpdateState::Failed(message) => {
                ui.colored_label(egui::Color32::RED, format!("更新失败: {message}"));
                if ui.button("重新检查").clicked() {
                    self.state.update.check_now();
                    self.status = "正在检查更新...".to_string();
                }
            }
        }
        ui.separator();
        let mut auto_check = self.state.update.auto_check_value();
        if ui.checkbox(&mut auto_check, "启动时自动检查更新").changed() {
            self.runtime
                .block_on(self.state.update.set_auto_check(auto_check));
            if let Some(tray) = &self.tray {
                tray.set_auto_check_checked(auto_check);
            }
        }
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

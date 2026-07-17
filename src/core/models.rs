use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpstreamKind {
    RelayApiKey,
    CodexOauth,
}

impl UpstreamKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RelayApiKey => "relay_api_key",
            Self::CodexOauth => "codex_oauth",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "codex_oauth" => Self::CodexOauth,
            _ => Self::RelayApiKey,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireApi {
    Responses,
    ChatCompletions,
}

impl WireApi {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::ChatCompletions => "chat_completions",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "chat" | "openai_chat" | "chat_completions" => Self::ChatCompletions,
            _ => Self::Responses,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorRetryPolicy {
    Off,
    Transient,
    All,
}

impl ErrorRetryPolicy {
    pub const ALL: [Self; 3] = [Self::Off, Self::Transient, Self::All];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Transient => "transient",
            Self::All => "all",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "transient" => Self::Transient,
            "all" => Self::All,
            _ => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BalanceProvider {
    Auto,
    DeepSeek,
    StepFun,
    SiliconFlowCn,
    SiliconFlowGlobal,
    OpenRouter,
    Novita,
    Sub2Api,
    NewApi,
    Unsupported,
}

impl BalanceProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::DeepSeek => "deepseek",
            Self::StepFun => "stepfun",
            Self::SiliconFlowCn => "siliconflow_cn",
            Self::SiliconFlowGlobal => "siliconflow_global",
            Self::OpenRouter => "openrouter",
            Self::Novita => "novita",
            Self::Sub2Api => "sub2api",
            Self::NewApi => "newapi",
            Self::Unsupported => "unsupported",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "deepseek" => Self::DeepSeek,
            "stepfun" => Self::StepFun,
            "siliconflow_cn" => Self::SiliconFlowCn,
            "siliconflow_global" => Self::SiliconFlowGlobal,
            "openrouter" => Self::OpenRouter,
            "novita" => Self::Novita,
            "sub2api" => Self::Sub2Api,
            "newapi" | "new-api" | "oneapi" | "one-api" => Self::NewApi,
            "unsupported" => Self::Unsupported,
            _ => Self::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScheduleMode {
    Random,
    RoundRobin,
    Failover,
    Fixed,
    ModelMapping,
}

impl ScheduleMode {
    pub const ALL: [Self; 5] = [
        Self::Random,
        Self::RoundRobin,
        Self::Failover,
        Self::Fixed,
        Self::ModelMapping,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::RoundRobin => "round_robin",
            Self::Failover => "failover",
            Self::Fixed => "fixed",
            Self::ModelMapping => "model_mapping",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "round_robin" | "polling" => Self::RoundRobin,
            "fixed" => Self::Fixed,
            "failover" => Self::Failover,
            "model_mapping" | "model-mapping" | "mapping" => Self::ModelMapping,
            _ => Self::Random,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Upstream {
    pub id: String,
    pub kind: UpstreamKind,
    pub name: String,
    pub base_url: String,
    pub wire_api: WireApi,
    pub supports_compact: bool,
    pub error_retry_policy: ErrorRetryPolicy,
    pub enabled: bool,
    pub priority: i64,
    pub weight: i64,
    pub proxy_url: Option<String>,
    pub balance_provider: BalanceProvider,
    pub chatgpt_account_id: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub token_expires_at: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Upstream {
    pub fn new_relay(
        name: String,
        base_url: String,
        wire_api: WireApi,
        supports_compact: bool,
        balance_provider: BalanceProvider,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            kind: UpstreamKind::RelayApiKey,
            name,
            base_url,
            wire_api,
            supports_compact,
            error_retry_policy: ErrorRetryPolicy::Off,
            enabled: true,
            priority: 0,
            weight: 1,
            proxy_url: None,
            balance_provider,
            chatgpt_account_id: None,
            email: None,
            plan_type: None,
            token_expires_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn new_codex_oauth(
        name: String,
        chatgpt_account_id: String,
        email: Option<String>,
        plan_type: Option<String>,
        token_expires_at: Option<i64>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            kind: UpstreamKind::CodexOauth,
            name,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            wire_api: WireApi::Responses,
            supports_compact: true,
            error_retry_policy: ErrorRetryPolicy::Off,
            enabled: true,
            priority: 10,
            weight: 1,
            proxy_url: None,
            balance_provider: BalanceProvider::Unsupported,
            chatgpt_account_id: Some(chatgpt_account_id),
            email,
            plan_type,
            token_expires_at,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleGroup {
    pub id: String,
    pub name: String,
    pub mode: ScheduleMode,
    pub use_all_upstreams: bool,
    pub fixed_target_kind: ScheduleRouteTargetKind,
    pub fixed_upstream_id: Option<String>,
    pub fixed_group_id: Option<String>,
    pub failure_threshold: i64,
    pub failover_on_balance: bool,
    pub failover_on_network: bool,
    pub failover_on_5xx: bool,
    pub affinity_ttl_seconds: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ScheduleGroup {
    pub fn new(name: String) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            mode: ScheduleMode::Failover,
            use_all_upstreams: true,
            fixed_target_kind: ScheduleRouteTargetKind::Upstream,
            fixed_upstream_id: None,
            fixed_group_id: None,
            failure_threshold: 1,
            failover_on_balance: true,
            failover_on_network: true,
            failover_on_5xx: true,
            affinity_ttl_seconds: 1800,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleGroupMember {
    pub group_id: String,
    pub upstream_id: String,
    pub enabled: bool,
    pub priority: i64,
    pub weight: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ScheduleGroupMember {
    pub fn new(group_id: String, upstream_id: String) -> Self {
        let now = Utc::now();
        Self {
            group_id,
            upstream_id,
            enabled: true,
            priority: 0,
            weight: 1,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleGroupChild {
    pub group_id: String,
    pub target_group_id: String,
    pub enabled: bool,
    pub priority: i64,
    pub weight: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ScheduleGroupChild {
    pub fn new(group_id: String, target_group_id: String) -> Self {
        let now = Utc::now();
        Self {
            group_id,
            target_group_id,
            enabled: true,
            priority: 0,
            weight: 1,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScheduleRouteTargetKind {
    Group,
    Upstream,
}

impl ScheduleRouteTargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Upstream => "upstream",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "upstream" => Self::Upstream,
            _ => Self::Group,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRouteRule {
    pub id: String,
    pub group_id: String,
    pub name: String,
    pub enabled: bool,
    pub pattern: String,
    pub target_kind: ScheduleRouteTargetKind,
    pub target_group_id: Option<String>,
    pub target_upstream_id: Option<String>,
    pub target_model: Option<String>,
    pub priority: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ScheduleRouteRule {
    pub fn new(group_id: String) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            group_id,
            name: String::new(),
            enabled: true,
            pattern: String::new(),
            target_kind: ScheduleRouteTargetKind::Group,
            target_group_id: None,
            target_upstream_id: None,
            target_model: None,
            priority: 0,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub total_tokens: i64,
}

impl TokenUsage {
    pub fn finish(&mut self) {
        if self.total_tokens == 0 {
            self.total_tokens = self.input_tokens
                + self.output_tokens
                + self.cache_read_tokens
                + self.cache_creation_tokens;
        }
    }

    pub fn merge_max(&mut self, other: &Self) {
        self.input_tokens = self.input_tokens.max(other.input_tokens);
        self.output_tokens = self.output_tokens.max(other.output_tokens);
        self.cache_read_tokens = self.cache_read_tokens.max(other.cache_read_tokens);
        self.cache_creation_tokens = self.cache_creation_tokens.max(other.cache_creation_tokens);
        self.total_tokens = self.total_tokens.max(other.total_tokens);
        self.finish();
    }

    pub fn cached_input_tokens(&self) -> i64 {
        self.cache_read_tokens
    }

    pub fn uncached_input_tokens(&self) -> i64 {
        let cache_tokens = self.cache_read_tokens + self.cache_creation_tokens;
        if self.input_tokens >= cache_tokens {
            self.input_tokens - cache_tokens
        } else {
            self.input_tokens
        }
    }
}

#[derive(Debug, Clone)]
pub struct RequestLog {
    pub ts: Option<DateTime<Utc>>,
    pub upstream_id: Option<String>,
    pub upstream_name: Option<String>,
    pub endpoint: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub status: i64,
    pub usage: TokenUsage,
    pub estimated_cost_usd: Option<f64>,
    pub duration_ms: i64,
    pub first_token_ms: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DashboardStats {
    pub total_requests: i64,
    pub total_usage: TokenUsage,
    pub today_requests: i64,
    pub today_usage: TokenUsage,
}

#[derive(Debug, Clone, Default)]
pub struct DatabaseInfo {
    pub path: String,
    pub main_file_bytes: u64,
    pub wal_file_bytes: u64,
    pub shm_file_bytes: u64,
    pub page_count: i64,
    pub page_size: i64,
    pub freelist_count: i64,
    pub request_log_count: i64,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderStats {
    pub upstream_id: String,
    pub upstream_name: String,
    pub requests: i64,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, Default)]
pub struct ModelUsageStats {
    pub upstream_id: Option<String>,
    pub model: Option<String>,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, Default)]
pub struct ModelPrice {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub model_name: String,
    pub input_usd_per_million: Option<f64>,
    pub cached_input_usd_per_million: Option<f64>,
    pub cache_write_usd_per_million: Option<f64>,
    pub output_usd_per_million: Option<f64>,
    pub currency: String,
    pub source: String,
    pub official: bool,
    pub fetched_at: i64,
    pub raw_json: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheKeepaliveMode {
    Off,
    Smart,
    Always,
}

impl CacheKeepaliveMode {
    pub const ALL: [Self; 3] = [Self::Off, Self::Smart, Self::Always];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Smart => "smart",
            Self::Always => "always",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "always" => Self::Always,
            "smart" => Self::Smart,
            _ => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamCacheKeepaliveSettings {
    pub upstream_id: String,
    pub enabled: bool,
    pub mode: CacheKeepaliveMode,
    pub interval_seconds: i64,
    pub max_idle_seconds: i64,
    pub min_cacheable_tokens: i64,
    pub max_cacheable_tokens: i64,
    pub max_active_sessions: i64,
    pub prefer_extended_retention: bool,
}

impl UpstreamCacheKeepaliveSettings {
    pub fn new(upstream_id: String) -> Self {
        Self {
            upstream_id,
            enabled: false,
            mode: CacheKeepaliveMode::Smart,
            interval_seconds: 300,
            max_idle_seconds: 3600,
            min_cacheable_tokens: 1024,
            max_cacheable_tokens: 128000,
            max_active_sessions: 32,
            prefer_extended_retention: false,
        }
    }

    pub fn is_active(&self) -> bool {
        self.enabled && self.mode != CacheKeepaliveMode::Off
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    pub upstream_id: String,
    pub used_5h_percent: Option<f64>,
    pub reset_5h_seconds: Option<i64>,
    pub window_5h_minutes: Option<i64>,
    pub used_7d_percent: Option<f64>,
    pub reset_7d_seconds: Option<i64>,
    pub window_7d_minutes: Option<i64>,
    pub fetched_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BalanceSnapshot {
    pub upstream_id: String,
    pub provider: String,
    pub remaining: Option<f64>,
    pub total: Option<f64>,
    pub used: Option<f64>,
    pub unit: Option<String>,
    pub is_valid: bool,
    pub message: Option<String>,
    pub fetched_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamBalanceAlertSettings {
    pub upstream_id: String,
    pub enabled: bool,
    pub threshold: f64,
    pub interval_seconds: i64,
    pub last_checked_at: Option<i64>,
    pub alert_active: bool,
}

impl UpstreamBalanceAlertSettings {
    pub fn new(upstream_id: String) -> Self {
        Self {
            upstream_id,
            enabled: false,
            threshold: 5.0,
            interval_seconds: 1800,
            last_checked_at: None,
            alert_active: false,
        }
    }
}

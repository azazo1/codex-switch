use crate::app::state::AppState;
use crate::core::models::{
    BalanceSnapshot, DashboardStats, DatabaseInfo, ProviderStats, QuotaSnapshot, RequestLog,
    ScheduleGroup, ScheduleGroupChild, ScheduleGroupMember, ScheduleRouteRule, Upstream,
    UpstreamBalanceAlertSettings, UpstreamCacheKeepaliveSettings,
};
use crate::storage::RequestLogFilter;
use std::collections::BTreeMap;

pub(super) struct ViewData {
    pub upstreams: Vec<Upstream>,
    pub cache_keepalive_settings: BTreeMap<String, UpstreamCacheKeepaliveSettings>,
    pub balance_alert_settings: BTreeMap<String, UpstreamBalanceAlertSettings>,
    pub schedule_groups: Vec<ScheduleGroup>,
    pub schedule_members: BTreeMap<String, Vec<ScheduleGroupMember>>,
    pub schedule_children: BTreeMap<String, Vec<ScheduleGroupChild>>,
    pub schedule_route_rules: BTreeMap<String, Vec<ScheduleRouteRule>>,
    pub scheduler_route_max_hops: i64,
    pub current_schedule_group_id: Option<String>,
    pub stats: DashboardStats,
    pub provider_stats: Vec<ProviderStats>,
    pub logs: Vec<RequestLog>,
    pub log_total_count: i64,
    pub total_estimated_cost_usd: Option<f64>,
    pub today_estimated_cost_usd: Option<f64>,
    pub provider_estimated_cost_usd: BTreeMap<String, Option<f64>>,
    pub log_estimated_cost_usd: Vec<Option<f64>>,
    pub price_cache_count: i64,
    pub price_cache_age_seconds: Option<i64>,
    pub database_info: DatabaseInfo,
    pub quota_snapshots: Vec<(String, Option<QuotaSnapshot>)>,
    pub balance_snapshots: Vec<(String, Option<BalanceSnapshot>)>,
}

pub(super) async fn load_view_data(
    state: &AppState,
    log_limit: i64,
    log_offset: i64,
    log_filter: &RequestLogFilter,
) -> anyhow::Result<ViewData> {
    let upstreams = state.store.list_upstreams().await?;
    let mut cache_keepalive_settings = BTreeMap::new();
    let mut balance_alert_settings = BTreeMap::new();
    for upstream in &upstreams {
        cache_keepalive_settings.insert(
            upstream.id.clone(),
            state.store.cache_keepalive_settings(&upstream.id).await?,
        );
        balance_alert_settings.insert(
            upstream.id.clone(),
            state.store.balance_alert_settings(&upstream.id).await?,
        );
    }
    let schedule_groups = state.store.list_schedule_groups().await?;
    let current_schedule_group_id = state.store.get_setting("current_schedule_group_id").await?;
    let mut schedule_members = BTreeMap::new();
    let mut schedule_children = BTreeMap::new();
    let mut schedule_route_rules = BTreeMap::new();
    for group in &schedule_groups {
        schedule_members.insert(
            group.id.clone(),
            state.store.list_schedule_group_members(&group.id).await?,
        );
        schedule_children.insert(
            group.id.clone(),
            state.store.list_schedule_group_children(&group.id).await?,
        );
        schedule_route_rules.insert(
            group.id.clone(),
            state.store.list_schedule_route_rules(&group.id).await?,
        );
    }
    let scheduler_route_max_hops = state.store.scheduler_route_max_hops().await?;
    let stats = state.store.dashboard_stats().await?;
    let provider_stats = state.store.provider_stats().await?;
    let log_total_count = state.store.request_log_count_filtered(log_filter).await?;
    let logs = state
        .store
        .recent_logs_page_filtered(log_limit, log_offset, log_filter)
        .await?;
    let log_estimated_cost_usd = logs.iter().map(|log| log.estimated_cost_usd).collect();
    let costs = state.store.estimated_cost_summary().await?;
    let total_estimated_cost_usd = costs.total_usd;
    let today_estimated_cost_usd = costs.today_usd;
    let provider_estimated_cost_usd = costs.by_upstream;
    let price_cache_count = state.store.model_price_count().await?;
    let price_cache_age_seconds = state.store.model_price_cache_age_seconds().await?;
    let database_info = state.store.database_info().await?;
    let mut quota_snapshots = Vec::new();
    let mut balance_snapshots = Vec::new();
    for upstream in &upstreams {
        quota_snapshots.push((
            upstream.id.clone(),
            state.store.get_quota_snapshot(&upstream.id).await?,
        ));
        balance_snapshots.push((
            upstream.id.clone(),
            state.store.get_balance_snapshot(&upstream.id).await?,
        ));
    }
    Ok(ViewData {
        upstreams,
        cache_keepalive_settings,
        balance_alert_settings,
        schedule_groups,
        schedule_members,
        schedule_children,
        schedule_route_rules,
        scheduler_route_max_hops,
        current_schedule_group_id,
        stats,
        provider_stats,
        logs,
        log_total_count,
        total_estimated_cost_usd,
        today_estimated_cost_usd,
        provider_estimated_cost_usd,
        log_estimated_cost_usd,
        price_cache_count,
        price_cache_age_seconds,
        database_info,
        quota_snapshots,
        balance_snapshots,
    })
}

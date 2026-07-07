use crate::app::state::AppState;
use crate::core::models::{
    BalanceSnapshot, DashboardStats, DatabaseInfo, ModelUsageStats, ProviderStats, QuotaSnapshot,
    RequestLog, ScheduleGroup, ScheduleGroupMember, Upstream,
};
use crate::pricing;
use std::collections::BTreeMap;

pub(super) struct ViewData {
    pub upstreams: Vec<Upstream>,
    pub schedule_groups: Vec<ScheduleGroup>,
    pub schedule_members: BTreeMap<String, Vec<ScheduleGroupMember>>,
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
) -> anyhow::Result<ViewData> {
    let upstreams = state.store.list_upstreams().await?;
    let schedule_groups = state.store.list_schedule_groups().await?;
    let current_schedule_group_id = state.store.get_setting("current_schedule_group_id").await?;
    let mut schedule_members = BTreeMap::new();
    for group in &schedule_groups {
        schedule_members.insert(
            group.id.clone(),
            state.store.list_schedule_group_members(&group.id).await?,
        );
    }
    let stats = state.store.dashboard_stats().await?;
    let provider_stats = state.store.provider_stats().await?;
    let log_total_count = state.store.request_log_count().await?;
    let logs = state.store.recent_logs_page(log_limit, log_offset).await?;
    let total_model_usage = state.store.model_usage_stats(false).await?;
    let today_model_usage = state.store.model_usage_stats(true).await?;
    let total_estimated_cost_usd = estimate_model_usage_cost(state, &total_model_usage).await?;
    let today_estimated_cost_usd = estimate_model_usage_cost(state, &today_model_usage).await?;
    let provider_estimated_cost_usd =
        estimate_provider_costs(state, &total_model_usage, &provider_stats).await?;
    let log_estimated_cost_usd = estimate_log_costs(state, &logs).await?;
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
        schedule_groups,
        schedule_members,
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

async fn estimate_model_usage_cost(
    state: &AppState,
    rows: &[ModelUsageStats],
) -> anyhow::Result<Option<f64>> {
    let mut total = 0.0;
    let mut matched = false;
    for row in rows {
        let Some(model) = row.model.as_deref() else {
            continue;
        };
        let Some(price) = state.store.find_model_price(model).await? else {
            continue;
        };
        total += pricing::estimate_usage_cost(&row.usage, &price).total_usd();
        matched = true;
    }
    Ok(matched.then_some(total))
}

async fn estimate_provider_costs(
    state: &AppState,
    rows: &[ModelUsageStats],
    providers: &[ProviderStats],
) -> anyhow::Result<BTreeMap<String, Option<f64>>> {
    let mut totals: BTreeMap<String, (f64, bool)> = BTreeMap::new();
    for provider in providers {
        totals.insert(provider.upstream_id.clone(), (0.0, false));
    }
    for row in rows {
        let upstream_id = row.upstream_id.clone().unwrap_or_else(|| "none".to_string());
        let Some(model) = row.model.as_deref() else {
            continue;
        };
        let Some(price) = state.store.find_model_price(model).await? else {
            continue;
        };
        let entry = totals.entry(upstream_id).or_insert((0.0, false));
        entry.0 += pricing::estimate_usage_cost(&row.usage, &price).total_usd();
        entry.1 = true;
    }
    Ok(totals
        .into_iter()
        .map(|(key, (value, matched))| (key, matched.then_some(value)))
        .collect())
}

async fn estimate_log_costs(
    state: &AppState,
    logs: &[RequestLog],
) -> anyhow::Result<Vec<Option<f64>>> {
    let mut result = Vec::with_capacity(logs.len());
    for log in logs {
        let cost = match log.model.as_deref() {
            Some(model) => state
                .store
                .find_model_price(model)
                .await?
                .map(|price| pricing::estimate_usage_cost(&log.usage, &price).total_usd()),
            None => None,
        };
        result.push(cost);
    }
    Ok(result)
}

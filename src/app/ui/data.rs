use crate::app::state::AppState;
use crate::core::models::{
    BalanceSnapshot, DashboardStats, ProviderStats, QuotaSnapshot, RequestLog, Upstream,
};

pub(super) struct ViewData {
    pub upstreams: Vec<Upstream>,
    pub stats: DashboardStats,
    pub provider_stats: Vec<ProviderStats>,
    pub logs: Vec<RequestLog>,
    pub quota_snapshots: Vec<(String, Option<QuotaSnapshot>)>,
    pub balance_snapshots: Vec<(String, Option<BalanceSnapshot>)>,
}

pub(super) async fn load_view_data(state: &AppState) -> anyhow::Result<ViewData> {
    let upstreams = state.store.list_upstreams().await?;
    let stats = state.store.dashboard_stats().await?;
    let provider_stats = state.store.provider_stats().await?;
    let logs = state.store.recent_logs(100).await?;
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
        stats,
        provider_stats,
        logs,
        quota_snapshots,
        balance_snapshots,
    })
}

use crate::app::state::AppState;
use crate::core::models::{
    BalanceSnapshot, DashboardStats, ProviderStats, QuotaSnapshot, RequestLog, ScheduleGroup,
    ScheduleGroupMember, Upstream,
};
use std::collections::BTreeMap;

pub(super) struct ViewData {
    pub upstreams: Vec<Upstream>,
    pub schedule_groups: Vec<ScheduleGroup>,
    pub schedule_members: BTreeMap<String, Vec<ScheduleGroupMember>>,
    pub current_schedule_group_id: Option<String>,
    pub stats: DashboardStats,
    pub provider_stats: Vec<ProviderStats>,
    pub logs: Vec<RequestLog>,
    pub quota_snapshots: Vec<(String, Option<QuotaSnapshot>)>,
    pub balance_snapshots: Vec<(String, Option<BalanceSnapshot>)>,
}

pub(super) async fn load_view_data(state: &AppState) -> anyhow::Result<ViewData> {
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
        schedule_groups,
        schedule_members,
        current_schedule_group_id,
        stats,
        provider_stats,
        logs,
        quota_snapshots,
        balance_snapshots,
    })
}

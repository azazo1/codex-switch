use crate::app::AppState;
use crate::core::models::UpstreamKind;
use crate::scheduler::SchedulerPlan;

pub(super) async fn selection_plan(
    state: &AppState,
    body: &[u8],
    endpoint: &str,
    model: Option<&str>,
    responses_api: bool,
    compact: bool,
) -> anyhow::Result<SchedulerPlan> {
    let group = state.store.current_schedule_group().await?;
    let upstreams = state.store.schedule_group_upstreams(&group).await?;
    let upstreams = upstreams
        .into_iter()
        .filter(|upstream| {
            (!compact || upstream.supports_compact)
                && (!responses_api
                    || upstream.kind == UpstreamKind::CodexOauth
                    || !upstream.base_url.is_empty())
        })
        .collect::<Vec<_>>();
    state
        .scheduler
        .plan(group, upstreams, body, endpoint, model)
        .await
}

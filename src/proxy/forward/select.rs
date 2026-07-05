use crate::app::AppState;
use crate::core::models::{Upstream, UpstreamKind};

pub(super) async fn select_upstream(
    state: &AppState,
    responses_api: bool,
    compact: bool,
) -> anyhow::Result<Upstream> {
    let upstreams = state.store.enabled_upstreams().await?;
    upstreams
        .into_iter()
        .find(|upstream| {
            (!compact || upstream.supports_compact)
                && (!responses_api
                    || upstream.kind == UpstreamKind::CodexOauth
                    || !upstream.base_url.is_empty())
        })
        .ok_or_else(|| anyhow::anyhow!("no available upstream"))
}

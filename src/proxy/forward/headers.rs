use crate::app::AppState;
use crate::core::models::{Upstream, UpstreamKind};
use crate::oauth;
use axum::http::HeaderMap;

pub(super) async fn apply_headers(
    state: &AppState,
    upstream: &Upstream,
    mut request: reqwest::RequestBuilder,
    headers: &HeaderMap,
) -> anyhow::Result<reqwest::RequestBuilder> {
    for (name, value) in headers {
        let name_str = name.as_str().to_ascii_lowercase();
        if should_forward_header(&name_str) {
            request = request.header(name.as_str(), value.as_bytes());
        }
    }
    match upstream.kind {
        UpstreamKind::RelayApiKey => {
            let api_key = state
                .credentials
                .get(&upstream.id, "api_key")
                .await?
                .ok_or_else(|| anyhow::anyhow!("missing api key"))?;
            request = request.bearer_auth(api_key);
        }
        UpstreamKind::CodexOauth => {
            let token = oauth::valid_access_token(state, upstream).await?;
            request = request
                .bearer_auth(token)
                .header(
                    "chatgpt-account-id",
                    upstream.chatgpt_account_id.clone().unwrap_or_default(),
                )
                .header("openai-beta", "codex-1")
                .header("originator", "Codex Desktop")
                .header("Version", "0.144.2")
                .header("Session_Id", uuid::Uuid::new_v4().to_string())
                .header("Accept", "application/json");
        }
    }
    Ok(request)
}

fn should_forward_header(name: &str) -> bool {
    matches!(
        name,
        "accept"
            | "accept-language"
            | "content-type"
            | "conversation_id"
            | "openai-beta"
            | "originator"
            | "session_id"
            | "user-agent"
            | "x-codex-turn-state"
            | "x-codex-turn-metadata"
    )
}

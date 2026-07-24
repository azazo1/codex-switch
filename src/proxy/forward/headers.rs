use crate::app::AppState;
use crate::core::models::{Upstream, UpstreamKind, WireApi};
use crate::oauth;
use crate::proxy::upstream_auth;
use axum::http::HeaderMap;

pub(super) async fn apply_headers(
    state: &AppState,
    upstream: &Upstream,
    mut request: reqwest::RequestBuilder,
    headers: &HeaderMap,
    client_wire_api: Option<WireApi>,
) -> anyhow::Result<reqwest::RequestBuilder> {
    let preserve_anthropic = client_wire_api == Some(WireApi::AnthropicMessages)
        && upstream.wire_api == WireApi::AnthropicMessages;
    for (name, value) in headers {
        let name_str = name.as_str().to_ascii_lowercase();
        if should_forward_header(&name_str, preserve_anthropic) {
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
            request = upstream_auth::apply_api_key_auth(request, upstream, &api_key);
            if upstream.wire_api == WireApi::AnthropicMessages
                && !(preserve_anthropic && headers.contains_key("anthropic-version"))
            {
                request = upstream_auth::apply_anthropic_version(request, upstream);
            }
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

fn should_forward_header(name: &str, preserve_anthropic: bool) -> bool {
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
    ) || preserve_anthropic && matches!(name, "anthropic-version" | "anthropic-beta")
}

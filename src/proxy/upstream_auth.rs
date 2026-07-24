use crate::core::models::{ApiKeyAuthScheme, Upstream, WireApi};

pub(crate) const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

pub(crate) fn apply_api_key_auth(
    request: reqwest::RequestBuilder,
    upstream: &Upstream,
    api_key: &str,
) -> reqwest::RequestBuilder {
    match upstream.api_key_auth_scheme {
        ApiKeyAuthScheme::Bearer => request.bearer_auth(api_key),
        ApiKeyAuthScheme::XApiKey => request.header("x-api-key", api_key),
    }
}

pub(crate) fn apply_anthropic_version(
    request: reqwest::RequestBuilder,
    upstream: &Upstream,
) -> reqwest::RequestBuilder {
    if upstream.wire_api == WireApi::AnthropicMessages {
        request.header("anthropic-version", DEFAULT_ANTHROPIC_VERSION)
    } else {
        request
    }
}

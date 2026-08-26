use crate::app::AppState;
use crate::proxy::forward::{self, OpenAiEndpoint};
use crate::proxy::transform;
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::sync::Arc;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

#[derive(Clone)]
pub struct ProxyState {
    pub app: AppState,
}

pub fn build_router(state: AppState) -> Router {
    let state = Arc::new(ProxyState { app: state });
    let api = Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/models", get(models))
        .route("/v1/models/:id", get(model))
        .route("/models/:id", get(model))
        .route("/v1/responses", post(responses).get(ws_placeholder))
        .route("/v1/responses/*subpath", post(responses_subpath))
        .route("/responses", post(responses).get(ws_placeholder))
        .route("/responses/*subpath", post(responses_subpath))
        .route(
            "/backend-api/codex/responses",
            post(responses).get(ws_placeholder),
        )
        .route(
            "/backend-api/codex/responses/*subpath",
            post(responses_subpath),
        )
        .route("/v1/chat/completions", post(chat_completions))
        .route("/chat/completions", post(chat_completions))
        .route("/v1/messages", post(messages))
        .route("/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/messages/count_tokens", post(count_tokens))
        .route("/v1/images/*subpath", post(images))
        .route("/images/*subpath", post(images))
        .layer(DefaultBodyLimit::disable())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    Router::new()
        .fallback_service(api)
        .layer(middleware::from_fn(rewrite_incoming_api_path))
}

async fn rewrite_incoming_api_path(mut request: Request, next: Next) -> Response {
    if let Some(uri) = rewrite_request_uri(request.uri()) {
        tracing::debug!(
            original = %request.uri(),
            rewritten = %uri,
            "rewrite incoming api path"
        );
        *request.uri_mut() = uri;
    }
    next.run(request).await
}

fn rewrite_request_uri(uri: &Uri) -> Option<Uri> {
    let new_path = transform::canonicalize_incoming_path(uri.path())?;
    let path_and_query = match uri.query() {
        Some(query) => format!("{new_path}?{query}"),
        None => new_path,
    };
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = path_and_query.parse().ok();
    Uri::from_parts(parts).ok()
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn models(
    State(state): State<Arc<ProxyState>>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    forward::handle_models(state.app.clone(), headers, uri, None).await
}

async fn model(
    State(state): State<Arc<ProxyState>>,
    Path(id): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    forward::handle_models(state.app.clone(), headers, uri, Some(id)).await
}

async fn responses(
    State(state): State<Arc<ProxyState>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward::handle_openai(
        state.app.clone(),
        method,
        uri,
        headers,
        body,
        None,
        OpenAiEndpoint::Responses,
    )
    .await
}

async fn responses_subpath(
    State(state): State<Arc<ProxyState>>,
    Path(subpath): Path<String>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward::handle_openai(
        state.app.clone(),
        method,
        uri,
        headers,
        body,
        Some(format!("/{subpath}")),
        OpenAiEndpoint::Responses,
    )
    .await
}

async fn chat_completions(
    State(state): State<Arc<ProxyState>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward::handle_openai(
        state.app.clone(),
        method,
        uri,
        headers,
        body,
        None,
        OpenAiEndpoint::ChatCompletions,
    )
    .await
}

async fn messages(
    State(state): State<Arc<ProxyState>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward::handle_openai(
        state.app.clone(),
        method,
        uri,
        headers,
        body,
        None,
        OpenAiEndpoint::AnthropicMessages,
    )
    .await
}

async fn count_tokens(
    State(state): State<Arc<ProxyState>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward::handle_openai(
        state.app.clone(),
        method,
        uri,
        headers,
        body,
        None,
        OpenAiEndpoint::AnthropicCountTokens,
    )
    .await
}

async fn images(
    State(state): State<Arc<ProxyState>>,
    Path(subpath): Path<String>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward::handle_openai(
        state.app.clone(),
        method,
        uri,
        headers,
        body,
        Some(format!("/{subpath}")),
        OpenAiEndpoint::Images,
    )
    .await
}

async fn ws_placeholder() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "Responses WebSocket mode is reserved but not implemented in this version",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_keepalive::CacheKeepaliveRuntime;
    use crate::core::models::{
        ApiKeyAuthScheme, BalanceProvider, CacheKeepaliveMode, ErrorRetryPolicy, ScheduleGroup,
        ScheduleGroupMember, ScheduleMode, ScheduleRouteRule, ScheduleRouteTargetKind, Upstream,
        TemporaryAccessKey, UnknownModalityPolicy, UpstreamCacheKeepaliveSettings, WireApi,
    };
    use crate::storage::{Store, credentials::CredentialStore};
    use axum::{body::Body, http::header, routing::get};
    use futures_util::StreamExt;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::{net::TcpListener, sync::Mutex};

    #[derive(Clone, Copy)]
    enum MockMode {
        BalanceError,
        ForbiddenError,
        InvalidPromptError,
        ModelsJson,
        ModelsCapabilitiesJson,
        ResponsesJson,
        ResponsesThenForbidden,
        ResponsesSse,
        ResponsesNamespaceSse,
        ChatJson,
        ChatSse,
        ChatToolSse,
        ChatCustomToolSse,
        SlowChatSse,
        AnthropicJson,
        AnthropicSse,
        CountTokens,
        NotFound,
        ImagesJson,
    }

    #[derive(Clone)]
    struct MockUpstream {
        hits: Arc<Mutex<Vec<MockHit>>>,
        mode: MockMode,
    }

    #[derive(Debug, Clone)]
    struct MockHit {
        path: String,
        authorization: Option<String>,
        x_api_key: Option<String>,
        anthropic_version: Option<String>,
        anthropic_beta: Option<String>,
        body: Value,
    }

    #[tokio::test]
    async fn models_route_queries_upstream_models() {
        let (mock_base, hits) = spawn_mock(MockMode::ModelsJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        let proxy_base = spawn_proxy(state).await;
        let response = reqwest::Client::new()
            .get(format!("{proxy_base}/v1/models"))
            .bearer_auth("local-test")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value = response.json::<Value>().await.unwrap();
        assert_eq!(value["object"], "list");
        assert_eq!(value["data"][0]["id"], "gpt-mock");

        let hits = hits.lock().await;
        assert_eq!(hits[0].path, "/v1/models");
        assert_eq!(hits[0].authorization.as_deref(), Some("Bearer sk-test"));
    }

    #[tokio::test]
    async fn temporary_key_counts_successful_requests_and_tokens() {
        let (mock_base, hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        state
            .store
            .create_temporary_access_key(&TemporaryAccessKey::new(
                "temp-one".to_string(),
                "shared".to_string(),
                "cs-tmp-one".to_string(),
                Some(1),
                Some(1000),
                None,
            ))
            .await
            .unwrap();
        let proxy_base = spawn_proxy(state.clone()).await;
        let client = reqwest::Client::new();

        let response = client
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("cs-tmp-one")
            .json(&json!({"model":"gpt-test","input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        wait_for_log_count(&state, 1).await;
        let stored = state
            .store
            .find_temporary_access_key("cs-tmp-one")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.requests_used, 1);
        assert_eq!(stored.tokens_used, 5);

        let response = client
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("cs-tmp-one")
            .json(&json!({"model":"gpt-test","input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let value = response.json::<Value>().await.unwrap();
        assert_eq!(value["error"]["type"], "rate_limit_error");
        assert_eq!(hits.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn temporary_key_token_limit_rejects_next_request() {
        let (mock_base, hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        state
            .store
            .create_temporary_access_key(&TemporaryAccessKey::new(
                "temp-token".to_string(),
                String::new(),
                "cs-tmp-token".to_string(),
                None,
                Some(4),
                None,
            ))
            .await
            .unwrap();
        let proxy_base = spawn_proxy(state.clone()).await;
        let client = reqwest::Client::new();

        let response = client
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("cs-tmp-token")
            .json(&json!({"model":"gpt-test","input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        wait_for_log_count(&state, 1).await;
        let stored = state
            .store
            .find_temporary_access_key("cs-tmp-token")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.tokens_used, 5);

        let response = client
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("cs-tmp-token")
            .json(&json!({"model":"gpt-test","input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let value = response.json::<Value>().await.unwrap();
        assert_eq!(value["error"]["type"], "rate_limit_error");
        assert_eq!(hits.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn temporary_key_rejects_disabled_expired_and_unknown_keys() {
        let (mock_base, hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        state
            .store
            .create_temporary_access_key(&TemporaryAccessKey::new(
                "temp-disabled".to_string(),
                String::new(),
                "cs-tmp-disabled".to_string(),
                None,
                None,
                None,
            ))
            .await
            .unwrap();
        state
            .store
            .create_temporary_access_key(&TemporaryAccessKey::new(
                "temp-expired".to_string(),
                String::new(),
                "cs-tmp-expired".to_string(),
                None,
                None,
                Some(chrono::Utc::now().timestamp() - 10),
            ))
            .await
            .unwrap();
        state
            .store
            .set_temporary_access_key_enabled("temp-disabled", false)
            .await
            .unwrap();
        let proxy_base = spawn_proxy(state).await;
        let client = reqwest::Client::new();

        for key in ["cs-tmp-disabled", "cs-tmp-expired", "cs-tmp-unknown"] {
            let response = client
                .post(format!("{proxy_base}/v1/responses"))
                .bearer_auth(key)
                .json(&json!({"model":"gpt-test","input":"hello"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let value = response.json::<Value>().await.unwrap();
            assert_eq!(value["error"]["type"], "authentication_error");
        }
        assert_eq!(hits.lock().await.len(), 0);
    }

    #[tokio::test]
    async fn primary_local_key_does_not_consume_temporary_quota() {
        let (mock_base, _) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        state
            .store
            .create_temporary_access_key(&TemporaryAccessKey::new(
                "temp-primary".to_string(),
                String::new(),
                "cs-tmp-primary".to_string(),
                Some(1),
                Some(10),
                None,
            ))
            .await
            .unwrap();
        let proxy_base = spawn_proxy(state.clone()).await;

        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"gpt-test","input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        wait_for_log_count(&state, 1).await;
        let stored = state
            .store
            .find_temporary_access_key("cs-tmp-primary")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.requests_used, 0);
        assert_eq!(stored.tokens_used, 0);
    }

    #[tokio::test]
    async fn successful_models_request_consumes_temporary_request_count() {
        let (mock_base, _) = spawn_mock(MockMode::ModelsJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        state
            .store
            .create_temporary_access_key(&TemporaryAccessKey::new(
                "temp-models".to_string(),
                String::new(),
                "cs-tmp-models".to_string(),
                Some(1),
                None,
                None,
            ))
            .await
            .unwrap();
        let proxy_base = spawn_proxy(state.clone()).await;

        let response = reqwest::Client::new()
            .get(format!("{proxy_base}/v1/models"))
            .bearer_auth("cs-tmp-models")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let stored = state
            .store
            .find_temporary_access_key("cs-tmp-models")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.requests_used, 1);
        assert_eq!(stored.tokens_used, 0);
    }

    #[tokio::test]
    async fn responses_routes_keep_subpaths() {
        let (mock_base, hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        let proxy_base = spawn_proxy(state.clone()).await;
        let client = reqwest::Client::new();

        for path in [
            "/v1/responses",
            "/responses/compact",
            "/backend-api/codex/responses/input_tokens",
        ] {
            let response = client
                .post(format!("{proxy_base}{path}"))
                .bearer_auth("local-test")
                .json(&json!({"model":"gpt-test","input":"hello"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        let hits = hits.lock().await;
        let paths = hits.iter().map(|hit| hit.path.as_str()).collect::<Vec<_>>();
        assert_eq!(
            paths,
            [
                "/v1/responses",
                "/v1/responses/compact",
                "/v1/responses/input_tokens"
            ]
        );
        assert!(
            hits.iter()
                .all(|hit| hit.authorization.as_deref() == Some("Bearer sk-test"))
        );

        let logs = state.store.recent_logs(10).await.unwrap();
        assert_eq!(logs.len(), 3);
        assert!(logs.iter().any(|log| log.endpoint == "/responses/compact"));
    }

    #[tokio::test]
    async fn responses_route_accepts_large_payloads() {
        let (mock_base, hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        let proxy_base = spawn_proxy(state).await;
        let input = "x".repeat(3 * 1024 * 1024);

        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"gpt-test","input":input}))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let hits = hits.lock().await;
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].body["input"].as_str().map(str::len),
            Some(3 * 1024 * 1024)
        );
    }

    #[tokio::test]
    async fn chat_wire_converts_responses_request_and_response() {
        let (mock_base, hits) = spawn_mock(MockMode::ChatJson).await;
        let state = test_state(&mock_base, WireApi::ChatCompletions).await;
        let proxy_base = spawn_proxy(state.clone()).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"gpt-test","input":"hello","stream":false}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value = response.json::<Value>().await.unwrap();
        assert_eq!(value["object"], "response");
        assert_eq!(value["usage"]["input_tokens"], 4);
        assert_eq!(value["usage"]["output_tokens"], 5);

        let hits = hits.lock().await;
        assert_eq!(hits[0].path, "/v1/chat/completions");
        assert_eq!(hits[0].body["messages"][0]["content"], "hello");

        let logs = state.store.recent_logs(1).await.unwrap();
        assert_eq!(logs[0].usage.total_tokens, 9);
        assert_eq!(logs[0].endpoint, "/responses");
    }

    #[tokio::test]
    async fn chat_wire_preserves_upstream_error_response() {
        let (mock_base, _hits) = spawn_mock(MockMode::BalanceError).await;
        let state = test_state(&mock_base, WireApi::ChatCompletions).await;
        let proxy_base = spawn_proxy(state).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"domestic-coder","input":"hello"}))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
        let value = response.json::<Value>().await.unwrap();
        assert_eq!(value["error"]["message"], "insufficient balance");
        assert!(value.get("object").is_none());
    }

    #[tokio::test]
    async fn chat_route_downgrades_developer_role() {
        let (mock_base, hits) = spawn_mock(MockMode::ChatJson).await;
        let state = test_state(&mock_base, WireApi::ChatCompletions).await;
        let proxy_base = spawn_proxy(state).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/chat/completions"))
            .bearer_auth("local-test")
            .json(&json!({
                "model":"domestic-chat",
                "messages":[
                    {"role":"developer","content":"follow the rules"},
                    {"role":"user","content":"hello"}
                ]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let hits = hits.lock().await;
        assert_eq!(hits[0].body["messages"][0]["role"], "system");
        assert_eq!(hits[0].body["messages"][1]["role"], "user");
    }

    #[tokio::test]
    async fn chat_route_accepts_non_v1_version_and_completion_alias() {
        let (mock_base, hits) = spawn_mock(MockMode::ChatJson).await;
        let state = test_state(&mock_base, WireApi::ChatCompletions).await;
        let proxy_base = spawn_proxy(state).await;
        let client = reqwest::Client::new();
        let body = json!({
            "model":"gpt-test",
            "messages":[{"role":"user","content":"hello"}]
        });

        for path in ["/v4/chat/completions", "/v4/chat/completion"] {
            let response = client
                .post(format!("{proxy_base}{path}"))
                .bearer_auth("local-test")
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        let hits = hits.lock().await;
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| hit.path == "/v1/chat/completions"));
    }

    #[tokio::test]
    async fn chat_upstream_keeps_versioned_base_url() {
        let (mock_base, hits) = spawn_mock(MockMode::ChatJson).await;
        let state = test_state(&format!("{mock_base}/v4"), WireApi::ChatCompletions).await;
        let proxy_base = spawn_proxy(state).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/chat/completions"))
            .bearer_auth("local-test")
            .json(&json!({
                "model":"gpt-test",
                "messages":[{"role":"user","content":"hello"}]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let hits = hits.lock().await;
        assert_eq!(hits[0].path, "/v4/chat/completions");
    }

    #[tokio::test]
    async fn final_send_normalizes_developer_role_for_responses_wire() {
        let (mock_base, hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        let proxy_base = spawn_proxy(state).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({
                "model":"domestic-chat",
                "messages":[{"role":"developer","content":"follow the rules"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let hits = hits.lock().await;
        assert_eq!(hits[0].body["messages"][0]["role"], "system");
    }

    #[tokio::test]
    async fn images_route_forwards_generations_request() {
        let (mock_base, hits) = spawn_mock(MockMode::ImagesJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        let proxy_base = spawn_proxy(state.clone()).await;

        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/images/generations"))
            .bearer_auth("local-test")
            .json(&json!({"model":"gpt-image-1","prompt":"a small test image"}))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let value = response.json::<Value>().await.unwrap();
        assert_eq!(value["data"][0]["b64_json"], "mock-image");

        let hits = hits.lock().await;
        assert_eq!(hits[0].path, "/v1/images/generations");
        assert_eq!(hits[0].authorization.as_deref(), Some("Bearer sk-test"));
        assert_eq!(hits[0].body["model"], "gpt-image-1");
        drop(hits);

        let logs = state.store.recent_logs(1).await.unwrap();
        assert_eq!(logs[0].endpoint, "/images/generations");
        assert_eq!(logs[0].model.as_deref(), Some("gpt-image-1"));
    }

    #[tokio::test]
    async fn chat_sse_is_converted_and_usage_is_recorded() {
        let (mock_base, _hits) = spawn_mock(MockMode::ChatSse).await;
        let state = test_state(&mock_base, WireApi::ChatCompletions).await;
        let proxy_base = spawn_proxy(state.clone()).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"gpt-test","input":"hello","stream":true}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let text = response.text().await.unwrap();
        assert!(text.contains("event: response.output_text.delta"));
        assert!(text.contains("event: response.completed"));

        let logs = state.store.recent_logs(1).await.unwrap();
        assert_eq!(logs[0].usage.input_tokens, 2);
        assert_eq!(logs[0].usage.output_tokens, 3);
        assert_eq!(logs[0].usage.total_tokens, 5);
        assert!(logs[0].first_token_ms.is_some());
    }

    #[tokio::test]
    async fn chat_wire_converts_codex_tool_calls_end_to_end() {
        let (mock_base, hits) = spawn_mock(MockMode::ChatToolSse).await;
        let state = test_state(&mock_base, WireApi::ChatCompletions).await;
        let proxy_base = spawn_proxy(state.clone()).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/responses"))
            .bearer_auth("local-test")
            .json(&json!({
                "model":"domestic-coder",
                "instructions":"Use tools when needed",
                "input":[{
                    "type":"message",
                    "role":"user",
                    "content":[{"type":"input_text","text":"read the source"}]
                }],
                "tools":[{
                    "type":"function",
                    "name":"read_file",
                    "description":"Read one file",
                    "parameters":{
                        "type":"object",
                        "properties":{"path":{"type":"string"}},
                        "required":["path"]
                    },
                    "strict":false
                }],
                "stream":true
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let text = response.text().await.unwrap();
        assert!(text.contains("response.reasoning_summary_text.delta"));
        assert!(text.contains("response.reasoning_summary_text.done"));
        assert!(text.contains("\"delta\":\"need a file\""));
        assert!(text.contains("response.function_call_arguments.delta"));
        assert!(text.contains("response.function_call_arguments.done"));
        assert!(text.contains("\"type\":\"function_call\""));
        assert!(text.contains("codex-switch-reasoning-v1:"));
        assert!(text.contains("response.completed"));

        let hits = hits.lock().await;
        assert_eq!(hits[0].path, "/v1/chat/completions");
        assert_eq!(hits[0].body["messages"][0]["role"], "system");
        assert_eq!(hits[0].body["messages"][1]["content"], "read the source");
        assert_eq!(hits[0].body["tools"][0]["function"]["name"], "read_file");
        assert_eq!(hits[0].body["tools"][0]["function"]["strict"], false);
        drop(hits);

        let logs = state.store.recent_logs(1).await.unwrap();
        assert_eq!(logs[0].usage.input_tokens, 8);
        assert_eq!(logs[0].usage.output_tokens, 3);
    }

    #[tokio::test]
    async fn chat_wire_converts_additional_custom_and_namespace_tools_end_to_end() {
        let (mock_base, hits) = spawn_mock(MockMode::ChatCustomToolSse).await;
        let state = test_state(&mock_base, WireApi::ChatCompletions).await;
        let proxy_base = spawn_proxy(state.clone()).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/responses"))
            .bearer_auth("local-test")
            .json(&json!({
                "model":"domestic-coder",
                "input":[
                    {
                        "type":"additional_tools",
                        "role":"developer",
                        "tools":[
                            {
                                "type":"custom",
                                "name":"exec",
                                "description":"Run JavaScript tools",
                                "format":{
                                    "type":"grammar",
                                    "syntax":"lark",
                                    "definition":"start: source"
                                }
                            },
                            {
                                "type":"function",
                                "name":"wait",
                                "parameters":{"type":"object"},
                                "strict":true
                            },
                            {
                                "type":"namespace",
                                "name":"collaboration",
                                "tools":[{
                                    "type":"function",
                                    "name":"spawn_agent",
                                    "parameters":{"type":"object"}
                                }]
                            }
                        ]
                    },
                    {
                        "type":"message",
                        "role":"user",
                        "content":[{"type":"input_text","text":"patch the file"}]
                    }
                ],
                "tool_choice":"auto",
                "parallel_tool_calls":false,
                "stream":true
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let text = response.text().await.unwrap();
        assert!(text.contains("response.custom_tool_call_input.delta"));
        assert!(text.contains("response.custom_tool_call_input.done"));
        assert!(text.contains("\"type\":\"custom_tool_call\""));
        assert!(text.contains("\"name\":\"exec\""));
        assert!(text.contains("await tools.apply_patch()"));

        let hits = hits.lock().await;
        assert_eq!(hits[0].path, "/v1/chat/completions");
        assert_eq!(hits[0].body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(hits[0].body["messages"][0]["content"], "patch the file");
        assert_eq!(hits[0].body["tools"].as_array().unwrap().len(), 3);
        assert_eq!(hits[0].body["tools"][0]["function"]["name"], "exec");
        assert!(hits[0].body["tools"][0]["function"]["description"]
            .as_str()
            .unwrap()
            .contains("\"format\""));
        assert_eq!(hits[0].body["tools"][1]["function"]["strict"], true);
        assert_eq!(
            hits[0].body["tools"][2]["function"]["name"],
            "collaboration__spawn_agent"
        );
        assert_eq!(hits[0].body["tool_choice"], "auto");
        assert_eq!(hits[0].body["parallel_tool_calls"], false);
    }

    #[tokio::test]
    async fn chat_sse_is_recorded_when_client_stops_reading_early() {
        let (mock_base, _hits) = spawn_mock(MockMode::SlowChatSse).await;
        let state = test_state(&mock_base, WireApi::ChatCompletions).await;
        let proxy_base = spawn_proxy(state.clone()).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"gpt-test","input":"hello","stream":true}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let mut stream = response.bytes_stream();
        let mut saw_delta = false;
        for _ in 0..3 {
            let Some(chunk) = stream.next().await else {
                break;
            };
            let chunk = chunk.unwrap();
            let text = String::from_utf8_lossy(&chunk);
            if text.contains("response.output_text.delta") {
                saw_delta = true;
                break;
            }
        }
        assert!(saw_delta);
        drop(stream);
        wait_for_log_count(&state, 1).await;

        let logs = state.store.recent_logs(1).await.unwrap();
        assert_eq!(logs[0].upstream_name.as_deref(), Some("mock"));
        assert_eq!(logs[0].endpoint, "/responses");
        assert!(logs[0].first_token_ms.is_some());
    }

    #[tokio::test]
    async fn chat_sse_registers_cache_keepalive_before_done_can_be_dropped() {
        let (mock_base, _hits) = spawn_mock(MockMode::ChatSse).await;
        let state = test_state(&mock_base, WireApi::ChatCompletions).await;
        enable_cache_keepalive(&state, "mock").await;
        let proxy_base = spawn_proxy(state.clone()).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/chat/completions"))
            .bearer_auth("local-test")
            .json(&json!({
                "model":"gpt-test",
                "messages":[{"role":"user","content":"hello"}],
                "stream":true,
                "prompt_cache_key":"stable"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let mut stream = response.bytes_stream();
        let mut saw_done = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            let text = String::from_utf8_lossy(&chunk);
            if text.contains("data: [DONE]") {
                saw_done = true;
                break;
            }
        }
        assert!(saw_done);
        drop(stream);
        wait_for_cache_keepalive_count(&state, 1).await;

        let snapshots = state.cache_keepalive.snapshots().await;
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].cached_tokens, 2048);
        assert_eq!(snapshots[0].endpoint, "/chat/completions");
    }

    #[tokio::test]
    async fn responses_sse_registers_cache_keepalive_when_completed_event_is_dropped() {
        let (mock_base, _hits) = spawn_mock(MockMode::ResponsesSse).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        enable_cache_keepalive(&state, "mock").await;
        let proxy_base = spawn_proxy(state.clone()).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/responses"))
            .bearer_auth("local-test")
            .json(&json!({
                "model":"gpt-test",
                "input":"hello",
                "stream":true,
                "prompt_cache_key":"stable"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let mut stream = response.bytes_stream();
        let mut saw_completed = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            let text = String::from_utf8_lossy(&chunk);
            if text.contains("response.completed") {
                saw_completed = true;
                break;
            }
        }
        assert!(saw_completed);
        drop(stream);
        wait_for_cache_keepalive_count(&state, 1).await;

        let snapshots = state.cache_keepalive.snapshots().await;
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].cached_tokens, 4096);
        assert_eq!(snapshots[0].endpoint, "/responses");
    }

    #[tokio::test]
    async fn mapped_responses_sse_restores_client_model() {
        let (mock_base, hits) = spawn_mock(MockMode::ResponsesSse).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        set_group_mode(&state, "default", ScheduleMode::ModelMapping).await;
        let upstream = upstream_by_name(&state, "mock").await;
        let mut rule = ScheduleRouteRule::new("default".to_string());
        rule.name = "mapped".to_string();
        rule.pattern = "gpt-test".to_string();
        rule.target_kind = ScheduleRouteTargetKind::Upstream;
        rule.target_upstream_id = Some(upstream.id.clone());
        rule.target_model = Some("deepseek-v4-flash".to_string());
        state.store.save_schedule_route_rule(&rule).await.unwrap();
        let proxy_base = spawn_proxy(state).await;

        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"gpt-test","input":"hello","stream":true}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let mut body = String::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            body.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
        }
        assert!(body.contains("\"model\":\"gpt-test\""));
        assert!(!body.contains("\"model\":\"deepseek-v4-flash\""));
        let hits = hits.lock().await;
        assert_eq!(hits[0].body["model"], "deepseek-v4-flash");
    }

    #[tokio::test]
    async fn mapped_responses_sse_restores_namespaced_function_call() {
        let (mock_base, hits) = spawn_mock(MockMode::ResponsesNamespaceSse).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        set_group_mode(&state, "default", ScheduleMode::ModelMapping).await;
        let upstream = upstream_by_name(&state, "mock").await;
        let mut rule = ScheduleRouteRule::new("default".to_string());
        rule.name = "mapped".to_string();
        rule.pattern = "gpt-test".to_string();
        rule.target_kind = ScheduleRouteTargetKind::Upstream;
        rule.target_upstream_id = Some(upstream.id.clone());
        rule.target_model = Some("deepseek-v4-flash".to_string());
        state.store.save_schedule_route_rule(&rule).await.unwrap();
        let proxy_base = spawn_proxy(state).await;

        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"gpt-test","input":"hello","stream":true}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let mut body = String::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            body.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
        }
        assert!(body.contains("\"name\":\"js\""));
        assert!(body.contains("\"namespace\":\"mcp__node_repl\""));
        assert!(!body.contains("\"name\":\"mcp__node_repl__js\""));
        let hits = hits.lock().await;
        assert_eq!(hits[0].body["model"], "deepseek-v4-flash");
    }

    #[tokio::test]
    async fn failover_group_retries_balance_failure() {
        let (bad_base, bad_hits) = spawn_mock(MockMode::BalanceError).await;
        let (good_base, good_hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state_with_relays(vec![
            ("bad", bad_base.as_str(), WireApi::Responses, 10),
            ("good", good_base.as_str(), WireApi::Responses, 0),
        ])
        .await;
        let proxy_base = spawn_proxy(state.clone()).await;

        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"gpt-test","input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        assert_eq!(bad_hits.lock().await.len(), 1);
        assert_eq!(good_hits.lock().await.len(), 1);
        let logs = state.store.recent_logs(2).await.unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].upstream_name.as_deref(), Some("good"));
        assert_eq!(logs[1].upstream_name.as_deref(), Some("bad"));
        assert_eq!(
            logs[1].status,
            i64::from(StatusCode::PAYMENT_REQUIRED.as_u16())
        );
    }

    #[tokio::test]
    async fn failover_group_retries_client_error_and_replaces_affinity() {
        let (bad_base, bad_hits) = spawn_mock(MockMode::ResponsesThenForbidden).await;
        let (good_base, good_hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state_with_relays(vec![
            ("bad", bad_base.as_str(), WireApi::Responses, 10),
            ("good", good_base.as_str(), WireApi::Responses, 0),
        ])
        .await;
        let proxy_base = spawn_proxy(state).await;
        let client = reqwest::Client::new();
        let request = json!({
            "model":"gpt-test",
            "input":"hello",
            "prompt_cache_key":"stable"
        });

        let first = client
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(bad_hits.lock().await.len(), 1);
        assert_eq!(good_hits.lock().await.len(), 0);

        let second = client
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(bad_hits.lock().await.len(), 2);
        assert_eq!(good_hits.lock().await.len(), 1);

        let third = client
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(third.status(), StatusCode::OK);
        assert_eq!(bad_hits.lock().await.len(), 2);
        assert_eq!(good_hits.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn client_error_waits_for_failure_threshold() {
        let (bad_base, bad_hits) = spawn_mock(MockMode::ForbiddenError).await;
        let (good_base, good_hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state_with_relays(vec![
            ("bad", bad_base.as_str(), WireApi::Responses, 10),
            ("good", good_base.as_str(), WireApi::Responses, 0),
        ])
        .await;
        set_group_failure_threshold(&state, "default", 2).await;
        let proxy_base = spawn_proxy(state).await;
        let client = reqwest::Client::new();

        let first = client
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"gpt-test","input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::FORBIDDEN);
        assert_eq!(bad_hits.lock().await.len(), 1);
        assert_eq!(good_hits.lock().await.len(), 0);

        let second = client
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"gpt-test","input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(bad_hits.lock().await.len(), 2);
        assert_eq!(good_hits.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn error_retry_policy_does_not_bypass_client_error_threshold() {
        let (bad_base, bad_hits) = spawn_mock(MockMode::InvalidPromptError).await;
        let (good_base, good_hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state_with_relays(vec![
            ("bad", bad_base.as_str(), WireApi::Responses, 10),
            ("good", good_base.as_str(), WireApi::Responses, 0),
        ])
        .await;
        set_group_failure_threshold(&state, "default", 2).await;
        let mut upstream = upstream_by_name(&state, "bad").await;
        upstream.error_retry_policy = ErrorRetryPolicy::All;
        state.store.save_upstream(&upstream).await.unwrap();
        let proxy_base = spawn_proxy(state).await;
        let client = reqwest::Client::new();

        let first = client
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"gpt-test","input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            first.json::<Value>().await.unwrap()["error"]["code"],
            "rate_limit_exceeded"
        );
        assert_eq!(bad_hits.lock().await.len(), 1);
        assert_eq!(good_hits.lock().await.len(), 0);

        let second = client
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"gpt-test","input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(bad_hits.lock().await.len(), 2);
        assert_eq!(good_hits.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn model_route_can_jump_to_nested_group() {
        let (default_base, default_hits) = spawn_mock(MockMode::ResponsesJson).await;
        let (glm_base, glm_hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state_with_relays(vec![
            (
                "default-upstream",
                default_base.as_str(),
                WireApi::Responses,
                0,
            ),
            ("glm-upstream", glm_base.as_str(), WireApi::Responses, 0),
        ])
        .await;
        let default_upstream = upstream_by_name(&state, "default-upstream").await;
        let glm_upstream = upstream_by_name(&state, "glm-upstream").await;
        restrict_group_to_upstream(&state, "default", &default_upstream.id).await;
        set_group_mode(&state, "default", ScheduleMode::ModelMapping).await;
        let glm_group = save_group_with_upstream(&state, "GLM", &glm_upstream.id).await;
        let mut rule = ScheduleRouteRule::new("default".to_string());
        rule.name = "glm".to_string();
        rule.pattern = "glm-*".to_string();
        rule.target_kind = ScheduleRouteTargetKind::Group;
        rule.target_group_id = Some(glm_group.id.clone());
        rule.priority = 10;
        state.store.save_schedule_route_rule(&rule).await.unwrap();
        let proxy_base = spawn_proxy(state.clone()).await;

        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"glm-4.5","input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        assert_eq!(default_hits.lock().await.len(), 0);
        assert_eq!(glm_hits.lock().await.len(), 1);
        let logs = state.store.recent_logs(1).await.unwrap();
        assert_eq!(logs[0].upstream_name.as_deref(), Some("glm-upstream"));
    }

    #[tokio::test]
    async fn model_route_can_direct_to_upstream_and_rewrite_model_template() {
        let (image_base, image_hits) = spawn_mock(MockMode::ModelsJson).await;
        let state = test_state(&image_base, WireApi::Responses).await;
        set_group_mode(&state, "default", ScheduleMode::ModelMapping).await;
        let image_upstream = upstream_by_name(&state, "mock").await;
        let mut rule = ScheduleRouteRule::new("default".to_string());
        rule.name = "image".to_string();
        rule.pattern = "glm/*".to_string();
        rule.target_kind = ScheduleRouteTargetKind::Upstream;
        rule.target_upstream_id = Some(image_upstream.id.clone());
        rule.target_model = Some("*".to_string());
        state.store.save_schedule_route_rule(&rule).await.unwrap();
        let proxy_base = spawn_proxy(state.clone()).await;

        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"glm/glm-4.5","input":"draw"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let hits = image_hits.lock().await;
        assert_eq!(hits[0].body["model"], "glm-4.5");
        drop(hits);

        let response = reqwest::Client::new()
            .get(format!("{proxy_base}/v1/models"))
            .bearer_auth("local-test")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value = response.json::<Value>().await.unwrap();
        let ids = value["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"glm/gpt-mock"));
        assert!(!ids.contains(&"gpt-mock"));
        assert!(!ids.contains(&"glm/*"));
    }

    #[tokio::test]
    async fn models_route_reverse_maps_nested_model_group() {
        let (default_base, default_hits) = spawn_mock(MockMode::ResponsesJson).await;
        let (glm_base, glm_hits) = spawn_mock(MockMode::ModelsJson).await;
        let state = test_state_with_relays(vec![
            (
                "default-upstream",
                default_base.as_str(),
                WireApi::Responses,
                0,
            ),
            ("glm-upstream", glm_base.as_str(), WireApi::Responses, 0),
        ])
        .await;
        let default_upstream = upstream_by_name(&state, "default-upstream").await;
        let glm_upstream = upstream_by_name(&state, "glm-upstream").await;
        restrict_group_to_upstream(&state, "default", &default_upstream.id).await;
        set_group_mode(&state, "default", ScheduleMode::ModelMapping).await;
        let glm_group = save_group_with_upstream(&state, "GLM", &glm_upstream.id).await;
        let mut rule = ScheduleRouteRule::new("default".to_string());
        rule.name = "glm".to_string();
        rule.pattern = "glm/*".to_string();
        rule.target_kind = ScheduleRouteTargetKind::Group;
        rule.target_group_id = Some(glm_group.id.clone());
        rule.target_model = Some("*".to_string());
        state.store.save_schedule_route_rule(&rule).await.unwrap();
        let proxy_base = spawn_proxy(state.clone()).await;

        let response = reqwest::Client::new()
            .get(format!("{proxy_base}/v1/models"))
            .bearer_auth("local-test")
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let value = response.json::<Value>().await.unwrap();
        let ids = value["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(default_hits.lock().await.len(), 0);
        assert_eq!(glm_hits.lock().await.len(), 1);
        assert!(ids.contains(&"glm/gpt-mock"));
        assert!(!ids.contains(&"gpt-mock"));
    }

    #[tokio::test]
    async fn fixed_group_can_target_nested_schedule_group() {
        let (default_base, default_hits) = spawn_mock(MockMode::ResponsesJson).await;
        let (nested_base, nested_hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state_with_relays(vec![
            (
                "default-upstream",
                default_base.as_str(),
                WireApi::Responses,
                0,
            ),
            (
                "nested-upstream",
                nested_base.as_str(),
                WireApi::Responses,
                0,
            ),
        ])
        .await;
        let nested_upstream = upstream_by_name(&state, "nested-upstream").await;
        let nested_group = save_group_with_upstream(&state, "Nested", &nested_upstream.id).await;
        let mut default_group = state
            .store
            .get_schedule_group("default")
            .await
            .unwrap()
            .unwrap();
        default_group.mode = ScheduleMode::Fixed;
        default_group.fixed_target_kind = ScheduleRouteTargetKind::Group;
        default_group.fixed_group_id = Some(nested_group.id);
        state
            .store
            .save_schedule_group(&default_group)
            .await
            .unwrap();
        let proxy_base = spawn_proxy(state.clone()).await;

        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"gpt-test","input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(default_hits.lock().await.len(), 0);
        assert_eq!(nested_hits.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn model_route_cycle_returns_error_after_max_hops() {
        let (mock_base, hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        state
            .store
            .set_setting("scheduler_route_max_hops", "1")
            .await
            .unwrap();
        set_group_mode(&state, "default", ScheduleMode::ModelMapping).await;
        let mut loop_group = ScheduleGroup::new("Loop".to_string());
        loop_group.mode = ScheduleMode::ModelMapping;
        state.store.save_schedule_group(&loop_group).await.unwrap();
        let mut first = ScheduleRouteRule::new("default".to_string());
        first.name = "first".to_string();
        first.pattern = "*".to_string();
        first.target_kind = ScheduleRouteTargetKind::Group;
        first.target_group_id = Some(loop_group.id.clone());
        state.store.save_schedule_route_rule(&first).await.unwrap();
        let mut second = ScheduleRouteRule::new(loop_group.id.clone());
        second.name = "second".to_string();
        second.pattern = "*".to_string();
        second.target_kind = ScheduleRouteTargetKind::Group;
        second.target_group_id = Some("default".to_string());
        state.store.save_schedule_route_rule(&second).await.unwrap();
        let proxy_base = spawn_proxy(state).await;

        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({"model":"anything","input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let value = response.json::<Value>().await.unwrap();
        assert_eq!(value["error"]["type"], "proxy_error");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("模型路由超过最大跳转次数")
        );
        assert_eq!(hits.lock().await.len(), 0);
    }

    #[tokio::test]
    async fn models_route_skips_cyclic_model_groups() {
        let (mock_base, hits) = spawn_mock(MockMode::ModelsJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        set_group_mode(&state, "default", ScheduleMode::ModelMapping).await;
        let mut loop_group = ScheduleGroup::new("Loop".to_string());
        loop_group.mode = ScheduleMode::ModelMapping;
        state.store.save_schedule_group(&loop_group).await.unwrap();
        let mut first = ScheduleRouteRule::new("default".to_string());
        first.name = "first".to_string();
        first.pattern = "*".to_string();
        first.target_kind = ScheduleRouteTargetKind::Group;
        first.target_group_id = Some(loop_group.id.clone());
        state.store.save_schedule_route_rule(&first).await.unwrap();
        let mut second = ScheduleRouteRule::new(loop_group.id.clone());
        second.name = "second".to_string();
        second.pattern = "*".to_string();
        second.target_kind = ScheduleRouteTargetKind::Group;
        second.target_group_id = Some("default".to_string());
        state.store.save_schedule_route_rule(&second).await.unwrap();
        let proxy_base = spawn_proxy(state).await;

        let response = reqwest::Client::new()
            .get(format!("{proxy_base}/v1/models"))
            .bearer_auth("local-test")
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let value = response.json::<Value>().await.unwrap();
        assert!(value["data"].as_array().unwrap().is_empty());
        assert_eq!(hits.lock().await.len(), 0);
    }

    #[tokio::test]
    async fn text_protocol_matrix_converts_non_streaming_requests_and_responses() {
        let upstreams = [
            (WireApi::Responses, MockMode::ResponsesJson, "/v1/responses"),
            (
                WireApi::ChatCompletions,
                MockMode::ChatJson,
                "/v1/chat/completions",
            ),
            (
                WireApi::AnthropicMessages,
                MockMode::AnthropicJson,
                "/v1/messages",
            ),
        ];
        for (upstream_api, mode, expected_path) in upstreams {
            for client_api in [
                WireApi::Responses,
                WireApi::ChatCompletions,
                WireApi::AnthropicMessages,
            ] {
                let (mock_base, hits) = spawn_mock(mode).await;
                let state = test_state(&mock_base, upstream_api).await;
                let proxy_base = spawn_proxy(state.clone()).await;
                let client = reqwest::Client::new();
                let request = match client_api {
                    WireApi::Responses => client
                        .post(format!("{proxy_base}/v1/responses"))
                        .bearer_auth("local-test")
                        .json(&json!({"model":"test-model","input":"hello","stream":false})),
                    WireApi::ChatCompletions => client
                        .post(format!("{proxy_base}/v1/chat/completions"))
                        .bearer_auth("local-test")
                        .json(&json!({
                            "model":"test-model",
                            "messages":[{"role":"user","content":"hello"}],
                            "stream":false
                        })),
                    WireApi::AnthropicMessages => client
                        .post(format!("{proxy_base}/v1/messages"))
                        .header("x-api-key", "local-test")
                        .header("anthropic-version", "2023-06-01")
                        .json(&json!({
                            "model":"test-model",
                            "messages":[{"role":"user","content":"hello"}],
                            "max_tokens":64,
                            "stream":false
                        })),
                };
                let response = request.send().await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let value = response.json::<Value>().await.unwrap();
                match client_api {
                    WireApi::Responses => assert_eq!(
                        value["object"],
                        "response",
                        "upstream={upstream_api:?}, body={value}"
                    ),
                    WireApi::ChatCompletions => {
                        assert_eq!(
                            value["object"],
                            "chat.completion",
                            "upstream={upstream_api:?}, body={value}"
                        )
                    }
                    WireApi::AnthropicMessages => assert_eq!(
                        value["type"],
                        "message",
                        "upstream={upstream_api:?}, body={value}"
                    ),
                }

                let hits = hits.lock().await;
                assert_eq!(hits.len(), 1);
                assert_eq!(hits[0].path, expected_path);
                if upstream_api == WireApi::AnthropicMessages {
                    assert_eq!(hits[0].x_api_key.as_deref(), Some("sk-test"));
                    assert_eq!(
                        hits[0].anthropic_version.as_deref(),
                        Some("2023-06-01")
                    );
                    assert!(hits[0].authorization.is_none());
                    assert!(hits[0].body.get("messages").is_some());
                } else {
                    assert_eq!(hits[0].authorization.as_deref(), Some("Bearer sk-test"));
                }
            }
        }
    }

    #[tokio::test]
    async fn chat_upstream_can_filter_server_tools() {
        let (mock_base, hits) = spawn_mock(MockMode::ChatJson).await;
        let state = test_state(&mock_base, WireApi::ChatCompletions).await;
        let mut upstream = state.store.list_upstreams().await.unwrap().remove(0);
        upstream.filter_chat_server_tools = true;
        state.store.save_upstream(&upstream).await.unwrap();
        let proxy_base = spawn_proxy(state).await;

        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({
                "model":"test-model",
                "input":"hello",
                "stream":false,
                "tools":[
                    {"type":"function","name":"read_file","parameters":{"type":"object"}},
                    {"type":"web_search"},
                    {"type":"web_search_preview"}
                ]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let hits = hits.lock().await;
        assert_eq!(hits.len(), 1);
        let tools = hits[0].body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
    }

    #[tokio::test]
    async fn text_upstream_strips_multimodal_input_when_enabled() {
        let (mock_base, hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        let mut upstream = state.store.list_upstreams().await.unwrap().remove(0);
        upstream.strip_multimodal_for_text_models = true;
        state.store.save_upstream(&upstream).await.unwrap();
        let proxy_base = spawn_proxy(state).await;

        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({
                "model":"deepseek-v4-flash",
                "input":[{
                    "type":"message",
                    "role":"user",
                    "content":[
                        {"type":"input_text","text":"keep"},
                        {"type":"input_image","image_url":"data:image/png;base64,aGVsbG8="},
                        {"type":"input_audio","input_audio":"data:audio/wav;base64,YXVkaW8="},
                        {"type":"input_file","file_data":"data:application/pdf;base64,ZmlsZQ==","filename":"a.pdf"}
                    ]
                }],
                "stream":false
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let hits = hits.lock().await;
        assert_eq!(hits.len(), 1);
        let content = hits[0].body["input"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 4);
        assert_eq!(content[0]["text"], "keep");
        for part in content.iter().skip(1) {
            assert_eq!(part["type"], "input_text");
            let text = part["text"].as_str().unwrap();
            assert!(text.starts_with("[该模型不支持"));
        }
        assert!(content[1]["text"]
            .as_str()
            .unwrap()
            .contains("不支持图片输入"));
        assert!(content[3]["text"].as_str().unwrap().contains("a.pdf"));
    }

    #[tokio::test]
    async fn model_capability_cache_overrides_unknown_policy() {
        let (mock_base, hits) = spawn_mock(MockMode::ModelsCapabilitiesJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        let proxy_base = spawn_proxy(state.clone()).await;
        reqwest::Client::new()
            .get(format!("{proxy_base}/v1/models"))
            .bearer_auth("local-test")
            .send()
            .await
            .unwrap();

        let mut upstream = state.store.list_upstreams().await.unwrap().remove(0);
        upstream.strip_multimodal_for_text_models = true;
        state.store.save_upstream(&upstream).await.unwrap();
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({
                "model":"deepseek-v4-flash",
                "input":[{
                    "type":"message",
                    "role":"user",
                    "content":[
                        {"type":"input_text","text":"keep"},
                        {"type":"input_image","image_url":"data:image/png;base64,aGVsbG8="}
                    ]
                }],
                "stream":false
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let hits = hits.lock().await;
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, "/v1/models");
        assert_eq!(
            hits[1].body["input"][0]["content"][1]["type"],
            "input_image"
        );
    }

    #[tokio::test]
    async fn unknown_modality_policy_keeps_media_for_multimodal_policy() {
        let (mock_base, hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        let mut upstream = state.store.list_upstreams().await.unwrap().remove(0);
        upstream.strip_multimodal_for_text_models = true;
        upstream.unknown_modality_policy = UnknownModalityPolicy::Multimodal;
        state.store.save_upstream(&upstream).await.unwrap();
        let proxy_base = spawn_proxy(state).await;

        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .json(&json!({
                "model":"unknown-model",
                "input":[{
                    "type":"message",
                    "role":"user",
                    "content":[
                        {"type":"input_text","text":"keep"},
                        {"type":"input_image","image_url":"data:image/png;base64,aGVsbG8="}
                    ]
                }],
                "stream":false
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let hits = hits.lock().await;
        assert_eq!(
            hits[0].body["input"][0]["content"][1]["type"],
            "input_image"
        );
    }

    #[tokio::test]
    async fn text_protocol_matrix_converts_fragmented_streams() {
        let upstreams = [
            (WireApi::Responses, MockMode::ResponsesSse),
            (WireApi::ChatCompletions, MockMode::ChatSse),
            (WireApi::AnthropicMessages, MockMode::AnthropicSse),
        ];
        for (upstream_api, mode) in upstreams {
            for client_api in [
                WireApi::Responses,
                WireApi::ChatCompletions,
                WireApi::AnthropicMessages,
            ] {
                let (mock_base, _hits) = spawn_mock(mode).await;
                let state = test_state(&mock_base, upstream_api).await;
                let proxy_base = spawn_proxy(state.clone()).await;
                let client = reqwest::Client::new();
                let request = match client_api {
                    WireApi::Responses => client
                        .post(format!("{proxy_base}/v1/responses"))
                        .bearer_auth("local-test")
                        .json(&json!({"model":"test-model","input":"hello","stream":true})),
                    WireApi::ChatCompletions => client
                        .post(format!("{proxy_base}/v1/chat/completions"))
                        .bearer_auth("local-test")
                        .json(&json!({
                            "model":"test-model",
                            "messages":[{"role":"user","content":"hello"}],
                            "stream":true
                        })),
                    WireApi::AnthropicMessages => client
                        .post(format!("{proxy_base}/v1/messages"))
                        .header("x-api-key", "local-test")
                        .header("anthropic-version", "2023-06-01")
                        .json(&json!({
                            "model":"test-model",
                            "messages":[{"role":"user","content":"hello"}],
                            "max_tokens":64,
                            "stream":true
                        })),
                };
                let response = request.send().await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let text = response.text().await.unwrap();
                match client_api {
                    WireApi::Responses => assert!(
                        text.contains("response.completed"),
                        "upstream={upstream_api:?}, stream={text}"
                    ),
                    WireApi::ChatCompletions => {
                        assert!(
                            text.contains("chat.completion.chunk"),
                            "upstream={upstream_api:?}, stream={text}"
                        );
                        assert!(text.contains("data: [DONE]"), "stream={text}");
                    }
                    WireApi::AnthropicMessages => {
                        assert!(
                            text.contains("event: message_start"),
                            "upstream={upstream_api:?}, stream={text}"
                        );
                        assert!(text.contains("event: message_stop"), "stream={text}");
                    }
                }
                if upstream_api == WireApi::AnthropicMessages {
                    let logs = state.store.recent_logs(1).await.unwrap();
                    assert_eq!(logs[0].usage.input_tokens, 7);
                    assert_eq!(logs[0].usage.output_tokens, 2);
                    assert_eq!(logs[0].usage.cache_read_tokens, 3);
                    assert_eq!(logs[0].usage.total_tokens, 9);
                }
            }
        }
    }

    #[tokio::test]
    async fn anthropic_passthrough_preserves_private_blocks_and_beta_header() {
        let (mock_base, hits) = spawn_mock(MockMode::AnthropicJson).await;
        let state = test_state(&mock_base, WireApi::AnthropicMessages).await;
        let proxy_base = spawn_proxy(state).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/messages"))
            .header("x-api-key", "local-test")
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "files-api-2025-04-14")
            .json(&json!({
                "model":"claude-test",
                "messages":[{"role":"user","content":[{
                    "type":"document",
                    "source":{"type":"base64","media_type":"application/pdf","data":"AA=="},
                    "cache_control":{"type":"ephemeral"}
                }]}],
                "max_tokens":16,
                "private_field":{"keep":true}
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let hits = hits.lock().await;
        assert_eq!(hits[0].body["messages"][0]["content"][0]["type"], "document");
        assert_eq!(hits[0].body["private_field"]["keep"], true);
        assert_eq!(
            hits[0].anthropic_beta.as_deref(),
            Some("files-api-2025-04-14")
        );
    }

    #[tokio::test]
    async fn anthropic_upstream_can_use_bearer_without_forwarding_cross_protocol_beta() {
        let (mock_base, hits) = spawn_mock(MockMode::AnthropicJson).await;
        let state = test_state(&mock_base, WireApi::AnthropicMessages).await;
        let mut upstream = upstream_by_name(&state, "mock").await;
        upstream.api_key_auth_scheme = ApiKeyAuthScheme::Bearer;
        state.store.save_upstream(&upstream).await.unwrap();
        let proxy_base = spawn_proxy(state).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/responses"))
            .bearer_auth("local-test")
            .header("anthropic-beta", "must-not-forward")
            .json(&json!({"model":"claude-test","input":"hello","stream":false}))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let hits = hits.lock().await;
        assert_eq!(hits[0].authorization.as_deref(), Some("Bearer sk-test"));
        assert!(hits[0].x_api_key.is_none());
        assert_eq!(
            hits[0].anthropic_version.as_deref(),
            Some("2023-06-01")
        );
        assert!(hits[0].anthropic_beta.is_none());
    }

    #[tokio::test]
    async fn anthropic_cross_protocol_rejects_document_before_upstream_send() {
        let (mock_base, hits) = spawn_mock(MockMode::ResponsesJson).await;
        let state = test_state(&mock_base, WireApi::Responses).await;
        let proxy_base = spawn_proxy(state).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/messages"))
            .header("x-api-key", "local-test")
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model":"claude-test",
                "messages":[{"role":"user","content":[{"type":"document"}]}],
                "max_tokens":16
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let value = response.json::<Value>().await.unwrap();
        assert_eq!(value["type"], "error");
        assert_eq!(value["error"]["type"], "invalid_request_error");
        assert!(hits.lock().await.is_empty());
    }

    #[tokio::test]
    async fn count_tokens_uses_only_native_upstream_protocols() {
        let (responses_base, responses_hits) = spawn_mock(MockMode::CountTokens).await;
        let responses_state = test_state(&responses_base, WireApi::Responses).await;
        let responses_proxy = spawn_proxy(responses_state).await;
        let response = reqwest::Client::new()
            .post(format!("{responses_proxy}/v1/messages/count_tokens"))
            .header("x-api-key", "local-test")
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model":"claude-test",
                "messages":[{"role":"user","content":"hello"}]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["input_tokens"], 7);
        assert_eq!(responses_hits.lock().await[0].path, "/v1/responses/input_tokens");

        let (anthropic_base, anthropic_hits) = spawn_mock(MockMode::CountTokens).await;
        let anthropic_state = test_state(&anthropic_base, WireApi::AnthropicMessages).await;
        let anthropic_proxy = spawn_proxy(anthropic_state).await;
        let response = reqwest::Client::new()
            .post(format!("{anthropic_proxy}/v1/messages/count_tokens"))
            .header("x-api-key", "local-test")
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model":"claude-test",
                "messages":[{"role":"user","content":"hello"}]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["input_tokens"], 7);
        let anthropic_hits = anthropic_hits.lock().await;
        assert_eq!(anthropic_hits[0].path, "/v1/messages/count_tokens");
        assert_eq!(anthropic_hits[0].x_api_key.as_deref(), Some("sk-test"));

        let (chat_base, chat_hits) = spawn_mock(MockMode::CountTokens).await;
        let chat_state = test_state(&chat_base, WireApi::ChatCompletions).await;
        let chat_proxy = spawn_proxy(chat_state).await;
        let response = reqwest::Client::new()
            .post(format!("{chat_proxy}/messages/count_tokens"))
            .header("x-api-key", "local-test")
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model":"claude-test",
                "messages":[{"role":"user","content":"hello"}]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let value = response.json::<Value>().await.unwrap();
        assert_eq!(value["error"]["type"], "not_found_error");
        assert!(chat_hits.lock().await.is_empty());
    }

    #[tokio::test]
    async fn count_tokens_retries_native_capability_miss() {
        let (missing_base, missing_hits) = spawn_mock(MockMode::NotFound).await;
        let (working_base, working_hits) = spawn_mock(MockMode::CountTokens).await;
        let state = test_state_with_relays(vec![
            ("missing", missing_base.as_str(), WireApi::Responses, 10),
            (
                "working",
                working_base.as_str(),
                WireApi::AnthropicMessages,
                0,
            ),
        ])
        .await;
        set_group_mode(&state, "default", ScheduleMode::Failover).await;
        let proxy_base = spawn_proxy(state).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy_base}/v1/messages/count_tokens"))
            .header("x-api-key", "local-test")
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model":"claude-test",
                "messages":[{"role":"user","content":"hello"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["input_tokens"], 7);
        assert_eq!(missing_hits.lock().await.len(), 1);
        assert_eq!(working_hits.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn anthropic_models_support_pagination_detail_and_stored_auth() {
        let (mock_base, hits) = spawn_mock(MockMode::ModelsJson).await;
        let state = test_state(&mock_base, WireApi::AnthropicMessages).await;
        let proxy_base = spawn_proxy(state).await;
        let client = reqwest::Client::new();
        let response = client
            .get(format!("{proxy_base}/v1/models?limit=1"))
            .header("x-api-key", "local-test")
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "models-test")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value = response.json::<Value>().await.unwrap();
        assert_eq!(value["data"][0]["type"], "model");
        assert_eq!(value["first_id"], "gpt-mock");
        assert_eq!(value["last_id"], "gpt-mock");
        assert_eq!(value["has_more"], false);

        let response = client
            .get(format!("{proxy_base}/models/gpt-mock"))
            .header("x-api-key", "local-test")
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["id"], "gpt-mock");

        let hits = hits.lock().await;
        assert!(hits.iter().all(|hit| hit.x_api_key.as_deref() == Some("sk-test")));
        assert!(hits.iter().all(|hit| hit.authorization.is_none()));
        assert_eq!(hits[0].anthropic_beta.as_deref(), Some("models-test"));
    }

    async fn test_state(base_url: &str, wire_api: WireApi) -> AppState {
        test_state_with_relays(vec![("mock", base_url, wire_api, 0)]).await
    }

    async fn test_state_with_relays(relays: Vec<(&str, &str, WireApi, i64)>) -> AppState {
        let path =
            std::env::temp_dir().join(format!("codex-switch-test-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(path).await.unwrap();
        store
            .set_setting("local_access_key", "local-test")
            .await
            .unwrap();
        let credentials = CredentialStore::new_for_tests(store.clone());
        for (name, base_url, wire_api, priority) in relays {
            let mut upstream = Upstream::new_relay(
                name.to_string(),
                base_url.to_string(),
                wire_api,
                true,
                BalanceProvider::Unsupported,
            );
            upstream.priority = priority;
            store.save_upstream(&upstream).await.unwrap();
            credentials
                .put(&upstream.id, "api_key", "sk-test")
                .await
                .unwrap();
        }
        let events: crate::app::AppEvents = Default::default();
        let cache_keepalive = CacheKeepaliveRuntime::new(
            store.clone(),
            credentials.clone(),
            reqwest::Client::new(),
            events.clone(),
        );
        let oauth_accounts = crate::oauth::OAuthAccountService::new(store.clone());
        AppState {
            store,
            model_capabilities: Default::default(),
            credentials,
            oauth_accounts,
            http: reqwest::Client::new(),
            events,
            scheduler: Default::default(),
            live_requests: Default::default(),
            cache_keepalive,
        }
    }

    async fn upstream_by_name(state: &AppState, name: &str) -> Upstream {
        state
            .store
            .list_upstreams()
            .await
            .unwrap()
            .into_iter()
            .find(|upstream| upstream.name == name)
            .unwrap()
    }

    async fn restrict_group_to_upstream(state: &AppState, group_id: &str, upstream_id: &str) {
        let mut group = state
            .store
            .get_schedule_group(group_id)
            .await
            .unwrap()
            .unwrap();
        group.use_all_upstreams = false;
        state.store.save_schedule_group(&group).await.unwrap();
        for upstream in state.store.list_upstreams().await.unwrap() {
            let mut member = ScheduleGroupMember::new(group_id.to_string(), upstream.id.clone());
            member.enabled = upstream.id == upstream_id;
            state
                .store
                .save_schedule_group_member(&member)
                .await
                .unwrap();
        }
    }

    async fn set_group_mode(state: &AppState, group_id: &str, mode: ScheduleMode) {
        let mut group = state
            .store
            .get_schedule_group(group_id)
            .await
            .unwrap()
            .unwrap();
        group.mode = mode;
        state.store.save_schedule_group(&group).await.unwrap();
    }

    async fn set_group_failure_threshold(
        state: &AppState,
        group_id: &str,
        failure_threshold: i64,
    ) {
        let mut group = state
            .store
            .get_schedule_group(group_id)
            .await
            .unwrap()
            .unwrap();
        group.failure_threshold = failure_threshold;
        state.store.save_schedule_group(&group).await.unwrap();
    }

    async fn save_group_with_upstream(
        state: &AppState,
        name: &str,
        upstream_id: &str,
    ) -> ScheduleGroup {
        let mut group = ScheduleGroup::new(name.to_string());
        group.use_all_upstreams = false;
        state.store.save_schedule_group(&group).await.unwrap();
        for upstream in state.store.list_upstreams().await.unwrap() {
            let mut member = ScheduleGroupMember::new(group.id.clone(), upstream.id.clone());
            member.enabled = upstream.id == upstream_id;
            state
                .store
                .save_schedule_group_member(&member)
                .await
                .unwrap();
        }
        group
    }

    async fn enable_cache_keepalive(state: &AppState, upstream_name: &str) {
        let upstream = upstream_by_name(state, upstream_name).await;
        let mut settings = UpstreamCacheKeepaliveSettings::new(upstream.id);
        settings.enabled = true;
        settings.mode = CacheKeepaliveMode::Always;
        state
            .store
            .save_cache_keepalive_settings(&settings)
            .await
            .unwrap();
    }

    async fn spawn_proxy(state: AppState) -> String {
        spawn_server(build_router(state)).await
    }

    async fn spawn_mock(mode: MockMode) -> (String, Arc<Mutex<Vec<MockHit>>>) {
        let hits = Arc::new(Mutex::new(Vec::new()));
        let state = MockUpstream {
            hits: hits.clone(),
            mode,
        };
        let router = Router::new()
            .route("/*path", get(mock_handler).post(mock_handler))
            .layer(DefaultBodyLimit::disable())
            .with_state(state);
        (spawn_server(router).await, hits)
    }

    async fn spawn_server(router: Router) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}")
    }

    async fn wait_for_log_count(state: &AppState, expected: i64) {
        for _ in 0..20 {
            if state.store.request_log_count().await.unwrap() >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let count = state.store.request_log_count().await.unwrap();
        assert_eq!(count, expected);
    }

    async fn wait_for_cache_keepalive_count(state: &AppState, expected: usize) {
        for _ in 0..20 {
            if state.cache_keepalive.snapshots().await.len() >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let count = state.cache_keepalive.snapshots().await.len();
        assert_eq!(count, expected);
    }

    async fn mock_handler(
        State(state): State<MockUpstream>,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        let body = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
        let authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let x_api_key = headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let anthropic_version = headers
            .get("anthropic-version")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let anthropic_beta = headers
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let hit_count = {
            let mut hits = state.hits.lock().await;
            hits.push(MockHit {
                path: uri.path().to_string(),
                authorization,
                x_api_key,
                anthropic_version,
                anthropic_beta,
                body,
            });
            hits.len()
        };

        match state.mode {
            MockMode::BalanceError => (
                StatusCode::PAYMENT_REQUIRED,
                axum::Json(json!({"error":{"message":"insufficient balance"}})),
            )
                .into_response(),
            MockMode::ForbiddenError => (
                StatusCode::FORBIDDEN,
                axum::Json(json!({"error":{"message":"forbidden"}})),
            )
                .into_response(),
            MockMode::InvalidPromptError => (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({"error":{"code":"invalid_prompt"}})),
            )
                .into_response(),
            MockMode::ModelsJson => (
                StatusCode::OK,
                axum::Json(json!({
                    "object":"list",
                    "data":[{"id":"gpt-mock","object":"model","created":1,"owned_by":"mock-upstream"}]
                })),
            )
                .into_response(),
            MockMode::ModelsCapabilitiesJson => (
                StatusCode::OK,
                axum::Json(json!({
                    "object":"list",
                    "data":[{
                        "id":"deepseek-v4-flash",
                        "object":"model",
                        "capabilities":{"supports_image_input":true}
                    }]
                })),
            )
                .into_response(),
            MockMode::ResponsesThenForbidden if hit_count > 1 => (
                StatusCode::FORBIDDEN,
                axum::Json(json!({"error":{"message":"forbidden"}})),
            )
                .into_response(),
            MockMode::ResponsesJson | MockMode::ResponsesThenForbidden => (
                StatusCode::OK,
                axum::Json(json!({
                    "id":"resp_mock",
                    "object":"response",
                    "status":"completed",
                    "output":[],
                    "usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}
                })),
            )
                .into_response(),
            MockMode::ResponsesSse => {
                let stream = async_stream::stream! {
                    yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                        b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_mock\",\"model\":\"deepseek-v4-flash\",\"status\":\"in_progress\"}}\n\n",
                    ));
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                        b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_mock\",\"model\":\"deepseek-v4-flash\",\"status\":\"completed\",\"usage\":{\"input_tokens\":4096,\"output_tokens\":1,\"total_tokens\":4097,\"input_tokens_details\":{\"cached_tokens\":4096}}}}\n\n",
                    ));
                    tokio::time::sleep(Duration::from_millis(200)).await;
                };
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }
            MockMode::ResponsesNamespaceSse => {
                let stream = async_stream::stream! {
                    yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                        b"event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"status\":\"in_progress\",\"arguments\":\"\",\"call_id\":\"call_1\",\"name\":\"mcp__node_repl__js\"}}\n\n",
                    ));
                    yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                        b"event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"status\":\"completed\",\"arguments\":\"{\\\"code\\\":\\\"nodeRepl.write(1)\\\"}\",\"call_id\":\"call_1\",\"name\":\"mcp__node_repl__js\"}}\n\n",
                    ));
                    yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                        b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_mock\",\"model\":\"deepseek-v4-flash\",\"status\":\"completed\",\"output\":[{\"type\":\"function_call\",\"id\":\"fc_1\",\"status\":\"completed\",\"arguments\":\"{\\\"code\\\":\\\"nodeRepl.write(1)\\\"}\",\"call_id\":\"call_1\",\"name\":\"mcp__node_repl__js\"}]}}\n\n",
                    ));
                };
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }
            MockMode::ChatJson => (
                StatusCode::OK,
                axum::Json(json!({
                    "id":"chatcmpl_mock",
                    "object":"chat.completion",
                    "model":"gpt-test",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":4,"completion_tokens":5,"total_tokens":9}
                })),
            )
                .into_response(),
            MockMode::ChatSse => (
                [(header::CONTENT_TYPE, "text/event-stream")],
                concat!(
                    "data: {\"id\":\"chatcmpl_mock\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-test\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5,\"prompt_tokens_details\":{\"cached_tokens\":2048}}}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
                .into_response(),
            MockMode::ChatToolSse => (
                [(header::CONTENT_TYPE, "text/event-stream")],
                concat!(
                    "data: {\"model\":\"domestic-coder\",\"choices\":[{\"delta\":{\"reasoning_content\":\"need a file\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_read\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}]},\"finish_reason\":null}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"src/main.rs\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":3,\"total_tokens\":11}}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
                .into_response(),
            MockMode::ChatCustomToolSse => (
                [(header::CONTENT_TYPE, "text/event-stream")],
                concat!(
                    "data: {\"model\":\"domestic-coder\",\"choices\":[{\"delta\":{\"reasoning_content\":\"need a patch\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_exec\",\"type\":\"function\",\"function\":{\"name\":\"exec\",\"arguments\":\"{\\\"input\\\":\\\"await \"}}]},\"finish_reason\":null}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"tools.apply_patch()\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":4,\"total_tokens\":16}}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
                .into_response(),
            MockMode::SlowChatSse => {
                let stream = async_stream::stream! {
                    yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
                    ));
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                        b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}}\n\n",
                    ));
                    yield Ok::<_, std::convert::Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
                };
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }
            MockMode::AnthropicJson => (
                StatusCode::OK,
                axum::Json(json!({
                    "id":"msg_mock",
                    "type":"message",
                    "role":"assistant",
                    "model":"claude-test",
                    "content":[{"type":"text","text":"ok"}],
                    "stop_reason":"end_turn",
                    "stop_sequence":null,
                    "usage":{
                        "input_tokens":4,
                        "output_tokens":2,
                        "cache_read_input_tokens":3,
                        "cache_creation_input_tokens":1
                    }
                })),
            )
                .into_response(),
            MockMode::AnthropicSse => {
                let stream = async_stream::stream! {
                    yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                        b"event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_mock\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":4,\"cache_read_input_tokens\":3}}}\r\n\r\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                    ));
                    yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                        b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"o",
                    ));
                    yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                        b"k\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                    ));
                };
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }
            MockMode::CountTokens => (
                StatusCode::OK,
                axum::Json(json!({"input_tokens":7})),
            )
                .into_response(),
            MockMode::NotFound => (
                StatusCode::NOT_FOUND,
                axum::Json(json!({"error":{"type":"not_found_error","message":"unsupported"}})),
            )
                .into_response(),
            MockMode::ImagesJson => (
                StatusCode::OK,
                axum::Json(json!({
                    "created": 1,
                    "data": [{"b64_json": "mock-image"}]
                })),
            )
                .into_response(),
        }
    }
}

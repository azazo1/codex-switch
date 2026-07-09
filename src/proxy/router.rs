use crate::app::AppState;
use crate::proxy::forward::{self, OpenAiEndpoint};
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, Method, StatusCode, Uri},
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
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/models", get(models))
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
        .route("/v1/images/*subpath", post(images))
        .route("/images/*subpath", post(images))
        .layer(DefaultBodyLimit::disable())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn models(State(state): State<Arc<ProxyState>>, headers: HeaderMap) -> Response {
    forward::handle_models(state.app.clone(), headers).await
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
        BalanceProvider, CacheKeepaliveMode, ScheduleGroup, ScheduleGroupMember, ScheduleMode,
        ScheduleRouteRule, ScheduleRouteTargetKind, Upstream, UpstreamCacheKeepaliveSettings,
        WireApi,
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
        ModelsJson,
        ResponsesJson,
        ResponsesSse,
        ChatJson,
        ChatSse,
        SlowChatSse,
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
        AppState {
            store,
            credentials,
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
        state.hits.lock().await.push(MockHit {
            path: uri.path().to_string(),
            authorization,
            body,
        });

        match state.mode {
            MockMode::BalanceError => (
                StatusCode::PAYMENT_REQUIRED,
                axum::Json(json!({"error":{"message":"insufficient balance"}})),
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
            MockMode::ResponsesJson => (
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
                        b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_mock\",\"status\":\"in_progress\"}}\n\n",
                    ));
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                        b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_mock\",\"status\":\"completed\",\"usage\":{\"input_tokens\":4096,\"output_tokens\":1,\"total_tokens\":4097,\"input_tokens_details\":{\"cached_tokens\":4096}}}}\n\n",
                    ));
                    tokio::time::sleep(Duration::from_millis(200)).await;
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
                    "model":"gpt-test",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":4,"completion_tokens":5,"total_tokens":9}
                })),
            )
                .into_response(),
            MockMode::ChatSse => (
                [(header::CONTENT_TYPE, "text/event-stream")],
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5,\"prompt_tokens_details\":{\"cached_tokens\":2048}}}\n\n",
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

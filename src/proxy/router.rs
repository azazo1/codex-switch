use crate::app::AppState;
use crate::proxy::forward;
use axum::{
    Router,
    body::Bytes,
    extract::{Path, State},
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
    forward::handle_openai(state.app.clone(), method, uri, headers, body, None, true).await
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
        true,
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
    forward::handle_openai(state.app.clone(), method, uri, headers, body, None, false).await
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
    use crate::core::models::{BalanceProvider, Upstream, WireApi};
    use crate::storage::{Store, secrets::SecretStore};
    use axum::{
        http::header,
        routing::post,
    };
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio::{net::TcpListener, sync::Mutex};

    #[derive(Clone, Copy)]
    enum MockMode {
        ResponsesJson,
        ChatJson,
        ChatSse,
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
        let paths = hits
            .iter()
            .map(|hit| hit.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            ["/v1/responses", "/v1/responses/compact", "/v1/responses/input_tokens"]
        );
        assert!(hits.iter().all(|hit| hit.authorization.as_deref() == Some("Bearer sk-test")));

        let logs = state.store.recent_logs(10).await.unwrap();
        assert_eq!(logs.len(), 3);
        assert!(logs.iter().any(|log| log.endpoint == "/responses/compact"));
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

    async fn test_state(base_url: &str, wire_api: WireApi) -> AppState {
        let path = std::env::temp_dir()
            .join(format!("codex-switch-test-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(path).await.unwrap();
        store.set_setting("local_access_key", "local-test").await.unwrap();
        let secrets = SecretStore::new_for_tests(store.clone());
        let upstream = Upstream::new_relay(
            "mock".to_string(),
            base_url.to_string(),
            wire_api,
            true,
            BalanceProvider::Unsupported,
        );
        store.save_upstream(&upstream).await.unwrap();
        secrets.put(&upstream.id, "api_key", "sk-test").await.unwrap();
        AppState {
            store,
            secrets,
            http: reqwest::Client::new(),
        }
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
            .route("/*path", post(mock_handler))
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
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
                .into_response(),
        }
    }
}

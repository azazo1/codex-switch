use crate::app::AppState;
use crate::core::models::{RequestLog, TokenUsage, Upstream, UpstreamKind, WireApi};
use crate::proxy::transform;
use crate::quota;
use crate::scheduler::{self, SchedulerFailureKind};
use crate::usage;
use auth::validate_local_access;
use axum::{
    body::Bytes,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use headers::apply_headers;
use response::{build_response, convert_chat_sse_to_responses, to_axum_headers};
use select::selection_plan;
use serde_json::{Value, json};
use std::time::Instant;

mod auth;
mod headers;
mod models;
mod response;
mod select;

pub async fn handle_models(state: AppState, headers: HeaderMap) -> Response {
    if let Err(response) = validate_local_access(&state, &headers).await {
        return response;
    }
    match models::query_models(&state, &headers).await {
        Ok(value) => (StatusCode::OK, axum::Json(value)).into_response(),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            axum::Json(json!({"error":{"message":err.to_string(),"type":"proxy_error"}})),
        )
            .into_response(),
    }
}

pub async fn handle_openai(
    state: AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    subpath: Option<String>,
    responses_api: bool,
) -> Response {
    if let Err(response) = validate_local_access(&state, &headers).await {
        return response;
    }
    let started = Instant::now();
    let endpoint = if responses_api {
        format!(
            "/responses{}",
            subpath
                .clone()
                .unwrap_or_else(|| transform::responses_subpath_from_uri(uri.path()))
        )
    } else {
        "/chat/completions".to_string()
    };
    let model = usage::extract_model(&body);
    let compact = endpoint.starts_with("/responses/compact");

    let request = ForwardRequest {
        state: &state,
        method,
        uri: &uri,
        headers,
        body,
        endpoint: endpoint.clone(),
        responses_api,
        compact,
    };
    let result = forward_inner(request).await;

    match result {
        Ok(result) => {
            record_request_log(
                &state,
                RequestLog {
                    upstream_id: Some(result.upstream.id.clone()),
                    upstream_name: Some(result.upstream.name.clone()),
                    endpoint,
                    model,
                    status: i64::from(result.status.as_u16()),
                    usage: result.usage,
                    duration_ms: started.elapsed().as_millis() as i64,
                    first_token_ms: result.first_token_ms,
                    error: None,
                },
            )
            .await;
            result.response
        }
        Err(err) => {
            record_request_log(
                &state,
                RequestLog {
                    upstream_id: None,
                    upstream_name: None,
                    endpoint,
                    model,
                    status: 502,
                    usage: TokenUsage::default(),
                    duration_ms: started.elapsed().as_millis() as i64,
                    first_token_ms: None,
                    error: Some(err.to_string()),
                },
            )
            .await;
            (
                StatusCode::BAD_GATEWAY,
                axum::Json(json!({"error":{"message":err.to_string(),"type":"proxy_error"}})),
            )
                .into_response()
        }
    }
}

async fn record_request_log(state: &AppState, log: RequestLog) {
    match state.store.insert_request_log(log).await {
        Ok(()) => state.events.bump_request_logs(),
        Err(err) => tracing::warn!(error = %err, "failed to record request log"),
    }
}

struct ForwardResult {
    upstream: Upstream,
    status: StatusCode,
    usage: TokenUsage,
    first_token_ms: Option<i64>,
    failure_kind: Option<SchedulerFailureKind>,
    response: Response,
}

struct ForwardRequest<'a> {
    state: &'a AppState,
    method: Method,
    uri: &'a Uri,
    headers: HeaderMap,
    body: Bytes,
    endpoint: String,
    responses_api: bool,
    compact: bool,
}

async fn forward_inner(request: ForwardRequest<'_>) -> anyhow::Result<ForwardResult> {
    let plan = selection_plan(
        request.state,
        &request.body,
        &request.endpoint,
        usage::extract_model(&request.body).as_deref(),
        request.responses_api,
        request.compact,
    )
    .await?;
    let candidate_count = plan.candidates.len();
    let mut last_error = None;
    for (index, upstream) in plan.candidates.iter().cloned().enumerate() {
        match forward_with_upstream(&request, upstream.clone()).await {
            Ok(result) => {
                if let Some(failure) = result.failure_kind {
                    let count = request
                        .state
                        .scheduler
                        .record_failure(&plan.group.id, &upstream.id)
                        .await;
                    let should_retry = crate::scheduler::SchedulerRuntime::should_retry(
                        &plan.group,
                        failure,
                        count,
                    ) && index + 1 < candidate_count;
                    if should_retry {
                        tracing::warn!(
                            group_id = %plan.group.id,
                            upstream_id = %upstream.id,
                            upstream_name = %upstream.name,
                            failure = ?failure,
                            count,
                            "retrying request with next scheduled upstream"
                        );
                        continue;
                    }
                } else {
                    request
                        .state
                        .scheduler
                        .record_success(
                            &plan.group.id,
                            &upstream.id,
                            plan.affinity_key.as_deref(),
                            plan.group.affinity_ttl_seconds,
                        )
                        .await;
                }
                return Ok(result);
            }
            Err(err) => {
                let count = request
                    .state
                    .scheduler
                    .record_failure(&plan.group.id, &upstream.id)
                    .await;
                let should_retry = crate::scheduler::SchedulerRuntime::should_retry(
                    &plan.group,
                    SchedulerFailureKind::Network,
                    count,
                ) && index + 1 < candidate_count;
                if should_retry {
                    tracing::warn!(
                        group_id = %plan.group.id,
                        upstream_id = %upstream.id,
                        upstream_name = %upstream.name,
                        error = %err,
                        count,
                        "retrying network failure with next scheduled upstream"
                    );
                    last_error = Some(err);
                    continue;
                }
                return Err(err);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no scheduled upstream handled request")))
}

async fn forward_with_upstream(
    request: &ForwardRequest<'_>,
    upstream: Upstream,
) -> anyhow::Result<ForwardResult> {
    let mut target_body = request.body.to_vec();
    let target_url;
    if upstream.kind == UpstreamKind::CodexOauth {
        target_body = transform::normalize_oauth_body(&target_body, request.compact)?;
        target_url = format!(
            "https://chatgpt.com/backend-api/codex{}",
            request.endpoint
        );
    } else if request.responses_api && upstream.wire_api == WireApi::ChatCompletions {
        target_body = usage::responses_to_chat_json(&target_body)?;
        target_url = transform::build_endpoint(&upstream.base_url, "/chat/completions");
    } else if request.responses_api {
        target_url = transform::build_endpoint(&upstream.base_url, &request.endpoint);
    } else {
        target_url = transform::build_endpoint(&upstream.base_url, "/chat/completions");
    }

    let mut upstream_request = request
        .state
        .http
        .request(
            reqwest::Method::from_bytes(request.method.as_str().as_bytes())?,
            &target_url,
        )
        .body(target_body);
    upstream_request =
        apply_headers(request.state, &upstream, upstream_request, &request.headers).await?;
    if let Some(query) = request.uri.query() {
        tracing::debug!(query, "client query observed");
    }
    let response = upstream_request.send().await?;
    let status = StatusCode::from_u16(response.status().as_u16())?;
    let response_headers = response.headers().clone();
    if upstream.kind == UpstreamKind::CodexOauth
        && let Some(snapshot) =
            quota::snapshot_from_headers(&upstream.id, &to_axum_headers(&response_headers))
    {
        let _ = request.state.store.save_quota_snapshot(&snapshot).await;
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();

    if content_type.contains("text/event-stream") {
        let started = Instant::now();
        let mut first_token_ms = None;
        let mut all = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if first_token_ms.is_none() && !chunk.is_empty() {
                first_token_ms = Some(started.elapsed().as_millis() as i64);
            }
            all.extend_from_slice(&chunk);
        }
        let text = String::from_utf8_lossy(&all);
        let mut usage = usage::extract_usage_from_sse(&text);
        usage.finish();
        let failure_kind = scheduler::classify_response(status, &all);
        let response_body = if request.responses_api && upstream.wire_api == WireApi::ChatCompletions
        {
            convert_chat_sse_to_responses(&text)
        } else {
            all
        };
        Ok(ForwardResult {
            upstream,
            status,
            usage,
            first_token_ms,
            failure_kind,
            response: build_response(status, response_headers, response_body),
        })
    } else {
        let bytes = response.bytes().await?;
        let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        let mut usage = usage::extract_usage_from_json(&value);
        usage.finish();
        let failure_kind = scheduler::classify_response(status, &bytes);
        let response_body = if request.responses_api && upstream.wire_api == WireApi::ChatCompletions
        {
            serde_json::to_vec(&usage::chat_to_responses_json(&value))?
        } else {
            bytes.to_vec()
        };
        Ok(ForwardResult {
            upstream,
            status,
            usage,
            first_token_ms: None,
            failure_kind,
            response: build_response(status, response_headers, response_body),
        })
    }
}

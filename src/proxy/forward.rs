use crate::app::AppState;
use crate::cache_keepalive::CacheKeepaliveRegistration;
use crate::core::models::{TokenUsage, Upstream, UpstreamKind, WireApi};
use crate::live::LiveRequestMeta;
use crate::proxy::transform;
use crate::quota;
use crate::scheduler::{self, SchedulerFailureKind};
use crate::usage;
use auth::validate_local_access;
use async_stream::stream;
use axum::{
    body::Bytes,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use headers::apply_headers;
use logging::{ActiveRequestGuard, AttemptLog, LiveRequestGuard, StreamLogDraft, record_attempt_log};
use response::{build_response, build_stream_response, to_axum_headers};
use select::selection_plan;
use serde_json::{Value, json};
use std::io;
use std::time::Instant;
use tokio::sync::watch;

mod auth;
mod headers;
mod logging;
mod models;
mod response;
mod select;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenAiEndpoint {
    Responses,
    ChatCompletions,
    Images,
}

impl OpenAiEndpoint {
    fn endpoint(self, uri: &Uri, subpath: Option<String>) -> String {
        match self {
            Self::Responses => format!(
                "/responses{}",
                subpath.unwrap_or_else(|| transform::responses_subpath_from_uri(uri.path()))
            ),
            Self::ChatCompletions => "/chat/completions".to_string(),
            Self::Images => format!(
                "/images{}",
                subpath.unwrap_or_else(|| transform::images_subpath_from_uri(uri.path()))
            ),
        }
    }

    fn is_responses(self) -> bool {
        self == Self::Responses
    }
}

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
    endpoint_kind: OpenAiEndpoint,
) -> Response {
    if let Err(response) = validate_local_access(&state, &headers).await {
        return response;
    }
    let started = Instant::now();
    let endpoint = endpoint_kind.endpoint(&uri, subpath);
    let model = usage::extract_model(&body);
    let reasoning_effort = usage::extract_reasoning_effort(&body);
    let compact = endpoint.starts_with("/responses/compact");

    let request = ForwardRequest {
        state: &state,
        method,
        uri: &uri,
        headers,
        body,
        endpoint: endpoint.clone(),
        started,
        request_id: uuid::Uuid::new_v4().to_string(),
        model: model.clone(),
        reasoning_effort: reasoning_effort.clone(),
        endpoint_kind,
        compact,
    };
    let result = forward_inner(request).await;

    match result {
        Ok(result) => {
            if result.log_on_return {
                record_attempt_log(AttemptLog {
                    state: &state,
                    started,
                    upstream: Some(&result.upstream),
                    endpoint,
                    model,
                    reasoning_effort,
                    status: result.status,
                    usage: result.usage,
                    first_token_ms: result.first_token_ms,
                    error: None,
                })
                .await;
            }
            result.response
        }
        Err(err) => {
            let message = err.source.to_string();
            if !err.logged {
                record_attempt_log(AttemptLog {
                    state: &state,
                    started,
                    upstream: None,
                    endpoint,
                    model,
                    reasoning_effort,
                    status: StatusCode::BAD_GATEWAY,
                    usage: TokenUsage::default(),
                    first_token_ms: None,
                    error: Some(message.clone()),
                })
                .await;
            }
            (
                StatusCode::BAD_GATEWAY,
                axum::Json(json!({"error":{"message":message,"type":"proxy_error"}})),
            )
                .into_response()
        }
    }
}

struct ForwardResult {
    upstream: Upstream,
    status: StatusCode,
    usage: TokenUsage,
    first_token_ms: Option<i64>,
    failure_kind: Option<SchedulerFailureKind>,
    log_on_return: bool,
    response: Response,
}

struct ForwardFailure {
    source: anyhow::Error,
    logged: bool,
}

impl ForwardFailure {
    fn logged(source: anyhow::Error) -> Self {
        Self {
            source,
            logged: true,
        }
    }

    fn unlogged(source: anyhow::Error) -> Self {
        Self {
            source,
            logged: false,
        }
    }
}

impl From<anyhow::Error> for ForwardFailure {
    fn from(source: anyhow::Error) -> Self {
        Self::unlogged(source)
    }
}

struct ForwardRequest<'a> {
    state: &'a AppState,
    method: Method,
    uri: &'a Uri,
    headers: HeaderMap,
    body: Bytes,
    endpoint: String,
    started: Instant,
    request_id: String,
    model: Option<String>,
    reasoning_effort: Option<String>,
    endpoint_kind: OpenAiEndpoint,
    compact: bool,
}

async fn forward_inner(request: ForwardRequest<'_>) -> Result<ForwardResult, ForwardFailure> {
    let plan = selection_plan(
        request.state,
        &request.body,
        &request.endpoint,
        usage::extract_model(&request.body).as_deref(),
        request.endpoint_kind,
        request.compact,
    )
    .await?;
    for step in &plan.route_path {
        tracing::debug!(
            group_id = %step.group_id,
            rule_id = %step.rule_id,
            target_kind = %step.target_kind.as_str(),
            target_id = %step.target_id,
            "schedule route step"
        );
    }
    let candidate_count = plan.candidates.len();
    let mut last_error = None;
    for (index, upstream) in plan.candidates.iter().cloned().enumerate() {
        match forward_with_upstream(&request, upstream.clone(), plan.target_model.as_deref()).await {
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
                        record_attempt_log(AttemptLog {
                            state: request.state,
                            started: request.started,
                            upstream: Some(&upstream),
                            endpoint: request.endpoint.clone(),
                            model: request.model.clone(),
                            reasoning_effort: request.reasoning_effort.clone(),
                            status: result.status,
                            usage: result.usage.clone(),
                            first_token_ms: result.first_token_ms,
                            error: Some(format!("scheduler retry: {failure:?}")),
                        })
                        .await;
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
                    let error_message = err.to_string();
                    record_attempt_log(AttemptLog {
                        state: request.state,
                        started: request.started,
                        upstream: Some(&upstream),
                        endpoint: request.endpoint.clone(),
                        model: request.model.clone(),
                        reasoning_effort: request.reasoning_effort.clone(),
                        status: StatusCode::BAD_GATEWAY,
                        usage: TokenUsage::default(),
                        first_token_ms: None,
                        error: Some(error_message.clone()),
                    })
                    .await;
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
                let error_message = err.to_string();
                record_attempt_log(AttemptLog {
                    state: request.state,
                    started: request.started,
                    upstream: Some(&upstream),
                    endpoint: request.endpoint.clone(),
                    model: request.model.clone(),
                    reasoning_effort: request.reasoning_effort.clone(),
                    status: StatusCode::BAD_GATEWAY,
                    usage: TokenUsage::default(),
                    first_token_ms: None,
                    error: Some(error_message),
                })
                .await;
                return Err(ForwardFailure::logged(err));
            }
        }
    }
    Err(ForwardFailure::unlogged(
        last_error.unwrap_or_else(|| anyhow::anyhow!("no scheduled upstream handled request")),
    ))
}

async fn forward_with_upstream(
    request: &ForwardRequest<'_>,
    upstream: Upstream,
    target_model: Option<&str>,
) -> anyhow::Result<ForwardResult> {
    let mut target_body = match target_model {
        Some(model) => transform::rewrite_model(&request.body, model)?,
        None => request.body.to_vec(),
    };
    let effective_model = target_model
        .map(str::to_string)
        .or_else(|| request.model.clone());
    let target_url;
    if upstream.kind == UpstreamKind::CodexOauth {
        if !request.endpoint_kind.is_responses() {
            anyhow::bail!("codex oauth upstream is only available for responses requests");
        }
        target_body = transform::normalize_oauth_body(&target_body, request.compact)?;
        target_url = format!(
            "https://chatgpt.com/backend-api/codex{}",
            request.endpoint
        );
    } else if request.endpoint_kind.is_responses() && upstream.wire_api == WireApi::ChatCompletions
    {
        target_body = usage::responses_to_chat_json(&target_body)?;
        target_url = transform::build_endpoint(&upstream.base_url, "/chat/completions");
    } else if request.endpoint_kind.is_responses() || request.endpoint_kind == OpenAiEndpoint::Images
    {
        target_url = transform::build_endpoint(&upstream.base_url, &request.endpoint);
    } else {
        target_url = transform::build_endpoint(&upstream.base_url, "/chat/completions");
    }
    let keepalive_body = target_body.clone();

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
    let mut terminate_rx = request.state.live_requests.start(LiveRequestMeta {
        id: request.request_id.clone(),
        upstream_name: Some(upstream.name.clone()),
        endpoint: request.endpoint.clone(),
        model: request.model.clone(),
        reasoning_effort: request.reasoning_effort.clone(),
        streaming: false,
    });
    request.state.events.bump_live_streams();
    let mut active_guard =
        ActiveRequestGuard::new(request.state.clone(), request.request_id.clone());
    let response = send_upstream_request(upstream_request, &mut terminate_rx).await?;
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
        request
            .state
            .live_requests
            .set_streaming(&request.request_id, true);
        request.state.events.bump_live_streams();
        let stream = build_live_response_stream(LiveResponseStreamInput {
            request,
            upstream: upstream.clone(),
            keepalive_body,
            effective_model,
            status,
            response,
            active_guard,
            terminate_rx,
        });
        let failure_kind = scheduler::classify_response(status, &[]);
        Ok(ForwardResult {
            upstream,
            status,
            usage: TokenUsage::default(),
            first_token_ms: None,
            failure_kind,
            log_on_return: false,
            response: build_stream_response(status, response_headers, stream),
        })
    } else {
        let bytes = read_response_bytes(response, &mut terminate_rx).await?;
        active_guard.finish();
        let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        let mut usage = usage::extract_usage_from_json(&value);
        usage.finish();
        let failure_kind = scheduler::classify_response(status, &bytes);
        let response_body = if request.endpoint_kind.is_responses()
            && upstream.wire_api == WireApi::ChatCompletions
        {
            serde_json::to_vec(&usage::chat_to_responses_json(&value))?
        } else {
            bytes.to_vec()
        };
        maybe_register_cache_keepalive(
            request.state,
            &upstream,
            &request.endpoint,
            effective_model,
            keepalive_body,
            status,
            &usage,
        )
        .await;
        Ok(ForwardResult {
            upstream,
            status,
            usage,
            first_token_ms: None,
            failure_kind,
            log_on_return: true,
            response: build_response(status, response_headers, response_body),
        })
    }
}

async fn send_upstream_request(
    upstream_request: reqwest::RequestBuilder,
    terminate_rx: &mut watch::Receiver<bool>,
) -> anyhow::Result<reqwest::Response> {
    let send_future = upstream_request.send();
    tokio::pin!(send_future);
    loop {
        tokio::select! {
            changed = terminate_rx.changed() => {
                if termination_requested(changed, terminate_rx) {
                    anyhow::bail!("terminated by user");
                }
            }
            response = &mut send_future => {
                return Ok(response?);
            }
        }
    }
}

async fn read_response_bytes(
    response: reqwest::Response,
    terminate_rx: &mut watch::Receiver<bool>,
) -> anyhow::Result<Bytes> {
    let bytes_future = response.bytes();
    tokio::pin!(bytes_future);
    loop {
        tokio::select! {
            changed = terminate_rx.changed() => {
                if termination_requested(changed, terminate_rx) {
                    anyhow::bail!("terminated by user");
                }
            }
            bytes = &mut bytes_future => {
                return Ok(bytes?);
            }
        }
    }
}

fn termination_requested(
    changed: Result<(), watch::error::RecvError>,
    terminate_rx: &watch::Receiver<bool>,
) -> bool {
    changed.is_ok() && *terminate_rx.borrow()
}

struct LiveResponseStreamInput<'a> {
    request: &'a ForwardRequest<'a>,
    upstream: Upstream,
    keepalive_body: Vec<u8>,
    effective_model: Option<String>,
    status: StatusCode,
    response: reqwest::Response,
    active_guard: ActiveRequestGuard,
    terminate_rx: watch::Receiver<bool>,
}

fn build_live_response_stream(
    input: LiveResponseStreamInput<'_>,
) -> impl futures_util::Stream<Item = Result<Bytes, io::Error>> + Send + 'static {
    let LiveResponseStreamInput {
        request,
        upstream,
        keepalive_body,
        effective_model,
        status,
        response,
        active_guard,
        mut terminate_rx,
    } = input;
    let state = request.state.clone();
    let request_id = request.request_id.clone();
    let endpoint = request.endpoint.clone();
    let model = request.model.clone();
    let reasoning_effort = request.reasoning_effort.clone();
    let started = request.started;
    let convert_chat =
        request.endpoint_kind.is_responses() && upstream.wire_api == WireApi::ChatCompletions;
    let response_id = format!("resp_{}", uuid::Uuid::new_v4());
    let log_draft = StreamLogDraft::new(
        state.clone(),
        &upstream,
        endpoint.clone(),
        model.clone(),
        reasoning_effort.clone(),
        status,
        started,
    );
    let live_guard = LiveRequestGuard::from_active(active_guard, log_draft.clone());
    stream! {
        let mut live_guard = live_guard;

        let mut first_token_ms = None;
        let mut usage = TokenUsage::default();
        let mut sse_buffer = String::new();
        let mut upstream_stream = response.bytes_stream();
        let mut termination_closed = false;
        let mut cache_keepalive_registered = false;

        if convert_chat {
            let created = format!(
                "event: response.created\ndata: {}\n\n",
                json!({"type":"response.created","response":{"id":response_id,"object":"response","status":"in_progress","output":[]}})
            );
            yield Ok(Bytes::from(created));
        }

        loop {
            let next_chunk = if termination_closed {
                upstream_stream.next().await
            } else {
                tokio::select! {
                    changed = terminate_rx.changed() => {
                        match changed {
                            Ok(()) if *terminate_rx.borrow() => {
                                let error_message = "terminated by user".to_string();
                                log_draft.merge_usage(&usage);
                                log_draft
                                    .record_with_status(
                                        StatusCode::from_u16(499)
                                            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                                        Some(error_message.clone()),
                                    )
                                    .await;
                                live_guard.finish();
                                yield Err(io::Error::other(error_message));
                                return;
                            }
                            Ok(()) => continue,
                            Err(_) => {
                                termination_closed = true;
                                continue;
                            }
                        }
                    }
                    chunk = upstream_stream.next() => chunk,
                }
            };
            let Some(chunk) = next_chunk else {
                break;
            };
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(err) => {
                    let error_message = err.to_string();
                    log_draft.merge_usage(&usage);
                    log_draft.record(Some(error_message.clone())).await;
                    live_guard.finish();
                    yield Err(io::Error::other(error_message));
                    return;
                }
            };
            if first_token_ms.is_none() && !chunk.is_empty() {
                let elapsed = started.elapsed().as_millis() as i64;
                first_token_ms = Some(elapsed);
                log_draft.set_first_token_ms(elapsed);
            }
            let converted = process_live_sse_chunk(
                &state,
                &request_id,
                &response_id,
                convert_chat,
                &mut usage,
                &mut sse_buffer,
                &chunk,
            );
            log_draft.merge_usage(&usage);
            if !cache_keepalive_registered && usage.cache_read_tokens > 0 {
                usage.finish();
                log_draft.merge_usage(&usage);
                maybe_register_cache_keepalive(
                    &state,
                    &upstream,
                    &endpoint,
                    effective_model.clone(),
                    keepalive_body.clone(),
                    status,
                    &usage,
                )
                .await;
                cache_keepalive_registered = true;
            }
            if convert_chat {
                if !converted.is_empty() {
                    yield Ok(Bytes::from(converted));
                }
            } else {
                yield Ok(chunk);
            }
        }

        if !sse_buffer.is_empty() {
            sse_buffer.push_str("\n\n");
            let converted = process_complete_sse_blocks(
                &state,
                &request_id,
                &response_id,
                convert_chat,
                &mut usage,
                &mut sse_buffer,
            );
            log_draft.merge_usage(&usage);
            if convert_chat && !converted.is_empty() {
                yield Ok(Bytes::from(converted));
            }
        }
        usage.finish();
        log_draft.merge_usage(&usage);
        live_guard.finish();
        log_draft.record(None).await;
        if !cache_keepalive_registered {
            maybe_register_cache_keepalive(
                &state,
                &upstream,
                &endpoint,
                effective_model,
                keepalive_body,
                status,
                &usage,
            )
            .await;
        }
        if convert_chat {
            yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
        }
    }
}

async fn maybe_register_cache_keepalive(
    state: &AppState,
    upstream: &Upstream,
    endpoint: &str,
    model: Option<String>,
    body: Vec<u8>,
    status: StatusCode,
    usage: &TokenUsage,
) {
    let model_name = model.clone().unwrap_or_default();
    if upstream.kind != UpstreamKind::RelayApiKey {
        tracing::trace!(
            upstream_id = %upstream.id,
            upstream_name = %upstream.name,
            upstream_kind = upstream.kind.as_str(),
            endpoint,
            model = %model_name,
            "cache keepalive registration skipped: upstream is not relay api key"
        );
        return;
    }
    if endpoint.starts_with("/images") {
        tracing::trace!(
            upstream_id = %upstream.id,
            upstream_name = %upstream.name,
            endpoint,
            model = %model_name,
            "cache keepalive registration skipped: image endpoint"
        );
        return;
    }
    if !status.is_success() {
        tracing::debug!(
            upstream_id = %upstream.id,
            upstream_name = %upstream.name,
            endpoint,
            model = %model_name,
            status = %status.as_u16(),
            "cache keepalive registration skipped: upstream status is not success"
        );
        return;
    }
    if usage.cache_read_tokens <= 0 {
        tracing::trace!(
            upstream_id = %upstream.id,
            upstream_name = %upstream.name,
            endpoint,
            model = %model_name,
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            cache_read_tokens = usage.cache_read_tokens,
            total_tokens = usage.total_tokens,
            body_bytes = body.len(),
            "cache keepalive registration skipped: no cached input tokens"
        );
        return;
    }
    tracing::debug!(
        upstream_id = %upstream.id,
        upstream_name = %upstream.name,
        endpoint,
        model = %model_name,
        input_tokens = usage.input_tokens,
        output_tokens = usage.output_tokens,
        cache_read_tokens = usage.cache_read_tokens,
        total_tokens = usage.total_tokens,
        body_bytes = body.len(),
        "cache keepalive registration candidate"
    );
    state
        .cache_keepalive
        .register(CacheKeepaliveRegistration {
            upstream: upstream.clone(),
            endpoint: endpoint.to_string(),
            model,
            body,
            usage: usage.clone(),
        })
        .await;
}

fn process_live_sse_chunk(
    state: &AppState,
    request_id: &str,
    response_id: &str,
    convert_chat: bool,
    usage: &mut TokenUsage,
    buffer: &mut String,
    chunk: &[u8],
) -> Vec<u8> {
    let text = String::from_utf8_lossy(chunk);
    buffer.push_str(&text);
    process_complete_sse_blocks(state, request_id, response_id, convert_chat, usage, buffer)
}

fn process_complete_sse_blocks(
    state: &AppState,
    request_id: &str,
    response_id: &str,
    convert_chat: bool,
    usage: &mut TokenUsage,
    buffer: &mut String,
) -> Vec<u8> {
    let mut changed = false;
    let mut converted = Vec::new();
    while let Some((index, separator_len)) = find_sse_block_separator(buffer) {
        {
            let block = &buffer[..index];
            let item = usage::extract_usage_from_sse(block);
            usage.merge_max(&item);
            usage::for_each_sse_text_delta(block, |delta| {
                changed |= state.live_requests.append_delta(request_id, delta);
            });
            if convert_chat {
                converted.extend_from_slice(&convert_chat_sse_block(response_id, block));
            }
        }
        buffer.drain(..index + separator_len);
    }
    if changed {
        state.events.bump_live_streams();
    }
    converted
}

fn find_sse_block_separator(buffer: &str) -> Option<(usize, usize)> {
    let lf = buffer.find("\n\n").map(|index| (index, 2));
    let crlf = buffer.find("\r\n\r\n").map(|index| (index, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    }
}

fn convert_chat_sse_block(response_id: &str, block: &str) -> Vec<u8> {
    let mut out = String::new();
    for line in block.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(delta) = value
            .pointer("/choices/0/delta/content")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                value
                    .pointer("/choices/0/delta/reasoning_content")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
            })
        {
            out.push_str(&format!(
                "event: response.output_text.delta\ndata: {}\n\n",
                json!({"type":"response.output_text.delta","delta":delta})
            ));
        }
        if value.get("usage").is_some() {
            let usage = usage::chat_to_responses_json(&value)["usage"].clone();
            out.push_str(&format!(
                "event: response.completed\ndata: {}\n\n",
                json!({"type":"response.completed","response":{"id":response_id,"status":"completed","usage":usage}})
            ));
        }
    }
    out.into_bytes()
}

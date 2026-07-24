use crate::app::AppState;
use crate::cache_keepalive::CacheKeepaliveRegistration;
use crate::core::models::{ErrorRetryPolicy, TokenUsage, Upstream, UpstreamKind, WireApi};
use crate::live::LiveRequestMeta;
use crate::proxy::compat::{self, PreparedProtocolRequest, ProtocolConversionError, ProtocolSseBridge};
use crate::proxy::debug;
use crate::proxy::transform;
use crate::quota;
use crate::scheduler::{self, SchedulerFailureKind};
use crate::usage;
use async_stream::stream;
use auth::validate_local_access;
use axum::{
    body::Bytes,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use headers::apply_headers;
use logging::{
    ActiveRequestGuard, AttemptLog, LiveRequestGuard, StreamLogDraft, record_attempt_log,
};
use response::{build_response, build_stream_response, to_axum_headers};
use select::selection_plan;
use serde_json::{Value, json};
use std::io;
use std::time::Instant;
use tokio::sync::watch;

mod auth;
mod error_policy;
mod headers;
mod logging;
mod models;
mod response;
mod select;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenAiEndpoint {
    Responses,
    ChatCompletions,
    AnthropicMessages,
    AnthropicCountTokens,
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
            Self::AnthropicMessages => "/messages".to_string(),
            Self::AnthropicCountTokens => "/messages/count_tokens".to_string(),
            Self::Images => format!(
                "/images{}",
                subpath.unwrap_or_else(|| transform::images_subpath_from_uri(uri.path()))
            ),
        }
    }

    fn is_responses(self) -> bool {
        self == Self::Responses
    }

    fn client_wire_api(self) -> Option<WireApi> {
        match self {
            Self::Responses => Some(WireApi::Responses),
            Self::ChatCompletions => Some(WireApi::ChatCompletions),
            Self::AnthropicMessages | Self::AnthropicCountTokens => {
                Some(WireApi::AnthropicMessages)
            }
            Self::Images => None,
        }
    }

    fn is_count_tokens(self) -> bool {
        self == Self::AnthropicCountTokens
    }
}

pub async fn handle_models(state: AppState, headers: HeaderMap, uri: Uri, model_id: Option<String>) -> Response {
    let anthropic = headers.contains_key("anthropic-version");
    if let Err(response) = validate_local_access(&state, &headers, anthropic).await {
        return response;
    }
    match models::query_models(&state, &headers, &uri, model_id.as_deref()).await {
        Ok(value) => (StatusCode::OK, axum::Json(value)).into_response(),
        Err(err) if err.downcast_ref::<models::ModelNotFound>().is_some() => (
            StatusCode::NOT_FOUND,
            axum::Json(internal_error_value(
                anthropic.then_some(WireApi::AnthropicMessages),
                StatusCode::NOT_FOUND,
                &err.to_string(),
            )),
        )
            .into_response(),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            axum::Json(internal_error_value(
                anthropic.then_some(WireApi::AnthropicMessages),
                StatusCode::BAD_GATEWAY,
                &err.to_string(),
            )),
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
    if let Err(response) = validate_local_access(
        &state,
        &headers,
        endpoint_kind.client_wire_api() == Some(WireApi::AnthropicMessages),
    )
    .await
    {
        return response;
    }
    let started = Instant::now();
    let endpoint = endpoint_kind.endpoint(&uri, subpath);
    let model = usage::extract_model(&body);
    let reasoning_effort = usage::extract_reasoning_effort(&body);
    let compact = endpoint.starts_with("/responses/compact");
    let request_id = uuid::Uuid::new_v4().to_string();
    debug::log_body("client_request", &request_id, &endpoint, &body);

    let request = ForwardRequest {
        state: &state,
        method,
        uri: &uri,
        headers,
        body,
        endpoint: endpoint.clone(),
        started,
        request_id,
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
                    status: err.status,
                    usage: TokenUsage::default(),
                    first_token_ms: None,
                    error: Some(message.clone()),
                })
                .await;
            }
            (
                err.status,
                axum::Json(internal_error_value(
                    endpoint_kind.client_wire_api(),
                    err.status,
                    &message,
                )),
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
    status: StatusCode,
}

impl ForwardFailure {
    fn logged(source: anyhow::Error) -> Self {
        Self {
            source,
            logged: true,
            status: StatusCode::BAD_GATEWAY,
        }
    }

    fn unlogged(source: anyhow::Error) -> Self {
        Self {
            source,
            logged: false,
            status: StatusCode::BAD_GATEWAY,
        }
    }

    fn invalid_request(source: anyhow::Error) -> Self {
        Self {
            source,
            logged: false,
            status: StatusCode::BAD_REQUEST,
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
    let plan = match selection_plan(
        request.state,
        &request.body,
        &request.endpoint,
        usage::extract_model(&request.body).as_deref(),
        request.endpoint_kind,
        request.compact,
    )
    .await {
        Ok(plan) => plan,
        Err(error) if request.endpoint_kind.is_count_tokens() => {
            return Err(ForwardFailure {
                source: anyhow::anyhow!("no upstream supports native token counting: {error}"),
                logged: false,
                status: StatusCode::NOT_FOUND,
            });
        }
        Err(error) => return Err(error.into()),
    };
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
        match forward_with_upstream(&request, upstream.clone(), plan.target_model.as_deref()).await
        {
            Ok(result) => {
                if request.endpoint_kind.is_count_tokens()
                    && result.status == StatusCode::NOT_FOUND
                {
                    if index + 1 < candidate_count {
                        tracing::debug!(
                            upstream_id = %upstream.id,
                            upstream_name = %upstream.name,
                            "native count_tokens endpoint is unavailable, trying next candidate"
                        );
                        continue;
                    }
                    return Ok(result);
                }
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
                if err.downcast_ref::<ProtocolConversionError>().is_some() {
                    return Err(ForwardFailure::invalid_request(err));
                }
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
    Err(ForwardFailure::unlogged(last_error.unwrap_or_else(|| {
        anyhow::anyhow!("no scheduled upstream handled request")
    })))
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
    let upstream_wire_api = if upstream.kind == UpstreamKind::CodexOauth {
        WireApi::Responses
    } else {
        upstream.wire_api
    };
    let mut protocol_request = if let Some(client_wire_api) = request.endpoint_kind.client_wire_api() {
        Some(
            PreparedProtocolRequest::new(
                client_wire_api,
                upstream_wire_api,
                &target_body,
                request.model.clone(),
            )
            .map_err(anyhow::Error::new)?,
        )
    } else {
        None
    };
    if let Some(prepared) = &protocol_request {
        target_body = prepared.body.clone();
    }
    if upstream.kind == UpstreamKind::CodexOauth && !request.endpoint_kind.is_count_tokens() {
        target_body = transform::normalize_oauth_body(&target_body, request.compact)?;
    }
    let target_url = target_url(request, &upstream, upstream_wire_api);
    debug::log_body(
        "upstream_request",
        &request.request_id,
        &target_url,
        &target_body,
    );
    let keepalive_body = target_body.clone();

    let http = request.state.http_for_upstream(&upstream)?;
    let mut upstream_request = http
        .request(
            reqwest::Method::from_bytes(request.method.as_str().as_bytes())?,
            &target_url,
        )
        .body(target_body);
    upstream_request =
        apply_headers(
            request.state,
            &upstream,
            upstream_request,
            &request.headers,
            request.endpoint_kind.client_wire_api(),
        )
        .await?;
    if let Some(query) = request.uri.query() {
        tracing::debug!(query, "client query observed");
    }
    let mut terminate_rx = request.state.live_requests.start(LiveRequestMeta {
        id: request.request_id.clone(),
        upstream_name: Some(upstream.name.clone()),
        endpoint: request.endpoint.clone(),
        model: request.model.clone(),
        reasoning_effort: request.reasoning_effort.clone(),
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

    let streaming = content_type.contains("text/event-stream");
    if request
        .state
        .live_requests
        .confirm_response_kind(&request.request_id, streaming)
    {
        request.state.events.bump_live_streams();
    }

    if streaming {
        let stream = build_live_response_stream(LiveResponseStreamInput {
            request,
            upstream: upstream.clone(),
            keepalive_body,
            effective_model,
            status,
            response,
            protocol_bridge: protocol_request
                .take()
                .map(|prepared| prepared.sse_bridge)
                .filter(|_| status.is_success()),
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
        debug::log_body(
            "upstream_response",
            &request.request_id,
            &target_url,
            &bytes,
        );
        active_guard.finish();
        let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        let mut usage = usage::extract_usage_from_json(&value);
        usage.finish();
        let failure_kind = scheduler::classify_response(status, &bytes);
        let mut response_body = if status.is_success() && request.endpoint_kind.is_count_tokens() {
            bytes.to_vec()
        } else if status.is_success() {
            match protocol_request.as_ref() {
                Some(prepared) if !prepared.is_passthrough() => {
                    serde_json::to_vec(&prepared.convert_json_response(&value).map_err(anyhow::Error::new)?)?
                }
                _ => bytes.to_vec(),
            }
        } else if let Some(client_api) = request.endpoint_kind.client_wire_api()
            && protocol_request.as_ref().is_some_and(|prepared| !prepared.is_passthrough())
        {
            compat::error_response_json(status, &bytes, client_api)
        } else {
            bytes.to_vec()
        };
        let mut client_status = status;
        if let Some(rewritten) = error_policy::rewrite_json_response(
            status,
            &response_body,
            upstream.error_retry_policy,
        )
        {
            tracing::warn!(
                request_id = %request.request_id,
                upstream_id = %upstream.id,
                upstream_name = %upstream.name,
                policy = %upstream.error_retry_policy.as_str(),
                upstream_status = %status.as_u16(),
                client_status = %rewritten.status.as_u16(),
                "rewrote upstream response as retryable error"
            );
            client_status = rewritten.status;
            response_body = rewritten.body;
        }
        debug::log_body(
            "client_response",
            &request.request_id,
            &request.endpoint,
            &response_body,
        );
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
            response: build_response(client_status, response_headers, response_body),
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

fn target_url(
    request: &ForwardRequest<'_>,
    upstream: &Upstream,
    upstream_wire_api: WireApi,
) -> String {
    let endpoint = if request.endpoint_kind == OpenAiEndpoint::Images {
        request.endpoint.as_str()
    } else if request.endpoint_kind.is_count_tokens() {
        match upstream_wire_api {
            WireApi::Responses => "/responses/input_tokens",
            WireApi::AnthropicMessages => "/messages/count_tokens",
            WireApi::ChatCompletions => unreachable!("Chat count_tokens candidate was filtered"),
        }
    } else {
        match upstream_wire_api {
            WireApi::Responses => {
                if request.endpoint_kind.is_responses() {
                    request.endpoint.as_str()
                } else {
                    "/responses"
                }
            }
            WireApi::ChatCompletions => "/chat/completions",
            WireApi::AnthropicMessages => "/messages",
        }
    };
    if upstream.kind == UpstreamKind::CodexOauth {
        format!("https://chatgpt.com/backend-api/codex{endpoint}")
    } else {
        transform::build_endpoint(&upstream.base_url, endpoint)
    }
}

fn internal_error_value(
    client_api: Option<WireApi>,
    status: StatusCode,
    message: &str,
) -> Value {
    if client_api == Some(WireApi::AnthropicMessages) {
        let error_type = match status {
            StatusCode::BAD_REQUEST => "invalid_request_error",
            StatusCode::NOT_FOUND => "not_found_error",
            _ => "api_error",
        };
        json!({"type":"error","error":{"type":error_type,"message":message}})
    } else {
        json!({"error":{"message":message,"type":"proxy_error"}})
    }
}

struct LiveResponseStreamInput<'a> {
    request: &'a ForwardRequest<'a>,
    upstream: Upstream,
    keepalive_body: Vec<u8>,
    effective_model: Option<String>,
    status: StatusCode,
    response: reqwest::Response,
    protocol_bridge: Option<ProtocolSseBridge>,
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
        protocol_bridge,
        active_guard,
        mut terminate_rx,
    } = input;
    let state = request.state.clone();
    let request_id = request.request_id.clone();
    let endpoint = request.endpoint.clone();
    let model = request.model.clone();
    let reasoning_effort = request.reasoning_effort.clone();
    let started = request.started;
    let convert_protocol = protocol_bridge
        .as_ref()
        .is_some_and(|bridge| !bridge.is_passthrough());
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
        let mut sse_buffer = Vec::new();
        let mut upstream_stream = response.bytes_stream();
        let mut protocol_bridge = protocol_bridge;
        let mut error_rewriter = (!convert_protocol
            && upstream.error_retry_policy != ErrorRetryPolicy::Off)
            .then(|| error_policy::SseErrorRewriter::new(upstream.error_retry_policy));
        let mut error_rewrite_logged = false;
        let mut termination_closed = false;
        let mut cache_keepalive_registered = false;

        if let Some(bridge) = &mut protocol_bridge {
            let initial = bridge.initial_events();
            if !initial.is_empty() {
                yield Ok(Bytes::from(initial));
            }
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
            debug::log_body(
                "upstream_stream_chunk",
                &request_id,
                &endpoint,
                &chunk,
            );
            if first_token_ms.is_none() && !chunk.is_empty() {
                let elapsed = started.elapsed().as_millis() as i64;
                first_token_ms = Some(elapsed);
                log_draft.set_first_token_ms(elapsed);
            }
            let converted = process_live_sse_chunk(
                &state,
                &request_id,
                protocol_bridge.as_mut(),
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
            if convert_protocol {
                if !converted.is_empty() {
                    debug::log_body(
                        "client_stream_chunk",
                        &request_id,
                        &endpoint,
                        &converted,
                    );
                    yield Ok(Bytes::from(converted));
                }
            } else if let Some(rewriter) = &mut error_rewriter {
                let output = rewriter.push(&chunk);
                if output.rewrite_count > 0 && !error_rewrite_logged {
                    tracing::warn!(
                        request_id = %request_id,
                        upstream_id = %upstream.id,
                        upstream_name = %upstream.name,
                        policy = %upstream.error_retry_policy.as_str(),
                        "rewrote upstream SSE response as retryable error"
                    );
                    error_rewrite_logged = true;
                }
                if !output.bytes.is_empty() {
                    debug::log_body(
                        "client_stream_chunk",
                        &request_id,
                        &endpoint,
                        &output.bytes,
                    );
                    yield Ok(Bytes::from(output.bytes));
                }
            } else {
                debug::log_body(
                    "client_stream_chunk",
                    &request_id,
                    &endpoint,
                    &chunk,
                );
                yield Ok(chunk);
            }
        }

        if !sse_buffer.is_empty() {
            sse_buffer.extend_from_slice(b"\n\n");
            let converted = process_complete_sse_blocks(
                &state,
                &request_id,
                protocol_bridge.as_mut(),
                &mut usage,
                &mut sse_buffer,
            );
            log_draft.merge_usage(&usage);
            if convert_protocol && !converted.is_empty() {
                debug::log_body(
                    "client_stream_chunk",
                    &request_id,
                    &endpoint,
                    &converted,
                );
                yield Ok(Bytes::from(converted));
            }
        }
        if let Some(rewriter) = &mut error_rewriter {
            let output = rewriter.finish();
            if output.rewrite_count > 0 && !error_rewrite_logged {
                tracing::warn!(
                    request_id = %request_id,
                    upstream_id = %upstream.id,
                    upstream_name = %upstream.name,
                    policy = %upstream.error_retry_policy.as_str(),
                    "rewrote upstream SSE response as retryable error"
                );
            }
            if !output.bytes.is_empty() {
                debug::log_body(
                    "client_stream_chunk",
                    &request_id,
                    &endpoint,
                    &output.bytes,
                );
                yield Ok(Bytes::from(output.bytes));
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
        if convert_protocol
            && let Some(bridge) = &mut protocol_bridge {
            let final_events = bridge.finish();
            if !final_events.is_empty() {
                debug::log_body(
                    "client_stream_chunk",
                    &request_id,
                    &endpoint,
                    &final_events,
                );
                yield Ok(Bytes::from(final_events));
            }
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
    if endpoint.ends_with("/count_tokens") {
        tracing::trace!(
            upstream_id = %upstream.id,
            upstream_name = %upstream.name,
            endpoint,
            model = %model_name,
            "cache keepalive registration skipped: token counting endpoint"
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
    protocol_bridge: Option<&mut ProtocolSseBridge>,
    usage: &mut TokenUsage,
    buffer: &mut Vec<u8>,
    chunk: &[u8],
) -> Vec<u8> {
    buffer.extend_from_slice(chunk);
    process_complete_sse_blocks(state, request_id, protocol_bridge, usage, buffer)
}

fn process_complete_sse_blocks(
    state: &AppState,
    request_id: &str,
    mut protocol_bridge: Option<&mut ProtocolSseBridge>,
    usage: &mut TokenUsage,
    buffer: &mut Vec<u8>,
) -> Vec<u8> {
    let mut changed = false;
    let mut converted = Vec::new();
    while let Some((index, separator_len)) = find_sse_block_separator(buffer) {
        {
            let block = buffer[..index].to_vec();
            let block_text = String::from_utf8_lossy(&block);
            let item = usage::extract_usage_from_sse(&block_text);
            usage.merge_max(&item);
            if usage::has_anthropic_usage_event(&block_text) {
                usage.total_tokens = usage.input_tokens + usage.output_tokens;
            }
            usage::for_each_sse_text_delta(&block_text, |delta| {
                changed |= state.live_requests.append_delta(request_id, delta);
            });
            if let Some(bridge) = protocol_bridge.as_deref_mut() {
                let bridge_output = bridge.push_block(&block);
                let bridge_usage = usage::extract_usage_from_sse(&String::from_utf8_lossy(&bridge_output));
                usage.merge_max(&bridge_usage);
                converted.extend_from_slice(&bridge_output);
            }
        }
        buffer.drain(..index + separator_len);
    }
    if changed {
        state.events.bump_live_streams();
    }
    converted
}

fn find_sse_block_separator(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n").map(|index| (index, 2));
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n").map(|index| (index, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    }
}

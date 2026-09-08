//! 模型测试台执行器: 绕过调度组直连上游, 或经本地代理端口走完整链路,
//! 发送小体积测试请求并统计耗时, 首 token 延迟, tokens 与估算费用.
//! 流式请求会实时回传正文与思维链增量, 并捕获原始请求/响应报文供导出 HAR.
//! 请求日志照常落库供日志页查看, 测试台自身的记录随 outcome 交由 UI 保存在内存中.

use crate::app::AppState;
use crate::core::models::{
    RequestLog, RequestLogSource, TokenUsage, Upstream, UpstreamKind, WireApi,
};
use crate::peer::client::PeerHttpResponse;
use crate::pricing;
use crate::proxy::transform;
use crate::usage;

use super::TEST_GROUP_HEADER;
use super::TEST_SOURCE_HEADER;
use super::TEST_SOURCE_HEADER_VALUE;
use super::headers::{apply_headers, peer_request_headers};
use super::logging::record_request_log;
use crate::proxy::upstream_auth;

use axum::http::HeaderMap;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures_util::{StreamExt, stream::BoxStream};
use serde_json::{Value, json};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 经调度组模式固定使用的本地代理端点, 协议转换由代理内部完成.
const LOCAL_TEST_ENDPOINT: &str = "/v1/responses";

const INTERNAL_ERROR_STATUS: i64 = 502;

/// 原始响应报文捕获上限, 超出后截断, 防止异常响应撑爆内存.
const RESPONSE_CAPTURE_LIMIT: usize = 256 * 1024;

/// 流式增量类型, 区分正文与思维链.
#[derive(Debug, Clone)]
pub enum ModelTestStreamPart {
    Text(String),
    Reasoning(String),
}

/// 一次测试的原始请求与响应报文, 供导出 HAR 使用.
#[derive(Debug, Clone)]
pub struct ModelTestRawTrace {
    pub started_at: DateTime<Utc>,
    pub request_url: String,
    pub request_headers: Vec<(String, String)>,
    pub request_body: String,
    pub response_status: i64,
    pub response_content_type: String,
    pub response_headers: Vec<(String, String)>,
    pub response_body: String,
    pub response_truncated: bool,
}

impl ModelTestRawTrace {
    /// 转成 HAR 1.2 格式的单个 entry, time 取请求总耗时 (毫秒).
    /// 字段结构与 logging::har 的自动记录保持一致, 敏感头同样只输出占位符.
    pub fn to_har_entry(&self, duration_ms: i64, error: Option<&str>) -> Value {
        let headers = |pairs: &[(String, String)]| {
            Value::Array(
                pairs
                    .iter()
                    .map(|(name, value)| {
                        let value = if redact_header(name) {
                            "[REDACTED]".to_string()
                        } else {
                            value.clone()
                        };
                        json!({"name": name, "value": value})
                    })
                    .collect(),
            )
        };
        let status_text = reqwest::StatusCode::from_u16(
            self.response_status.clamp(0, u16::MAX as i64) as u16,
        )
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or_default();
        let mut entry = json!({
            "startedDateTime": self.started_at.with_timezone(&chrono::Local).to_rfc3339(),
            "time": duration_ms,
            "request": {
                "method": "POST",
                "url": self.request_url,
                "httpVersion": "HTTP/1.1",
                "headers": headers(&self.request_headers),
                "queryString": [],
                "cookies": [],
                "headersSize": -1,
                "bodySize": self.request_body.len(),
                "postData": {
                    "mimeType": "application/json",
                    "text": self.request_body,
                },
            },
            "response": {
                "status": self.response_status,
                "statusText": status_text,
                "httpVersion": "HTTP/1.1",
                "headers": headers(&self.response_headers),
                "content": {
                    "size": self.response_body.len(),
                    "mimeType": self.response_content_type,
                    "text": self.response_body,
                },
                "redirectURL": "",
                "headersSize": -1,
                "bodySize": self.response_body.len(),
            },
            "cache": {},
            "timings": {
                "send": 0,
                "wait": duration_ms,
                "receive": 0,
            },
        });
        if let Some(error) = error {
            entry["_error"] = json!(error);
        }
        if self.response_truncated {
            entry["_truncated"] = json!(true);
        }
        entry
    }
}

/// 导出 HAR 时需要脱敏的请求头, 与 logging::har 的策略一致.
fn redact_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "x-api-key" | "proxy-authorization" | "cookie" | "set-cookie"
    )
}

/// UI 侧接收流式增量的回调.
pub type ModelTestDeltaSink = Box<dyn Fn(ModelTestStreamPart) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct ModelTestMessage {
    pub role: String,
    pub content: String,
}

impl ModelTestMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelTestParams {
    pub model: String,
    pub messages: Vec<ModelTestMessage>,
    pub stream: bool,
    pub max_tokens: i64,
    /// 小写的 effort 值, 如 "low"/"medium"/"high", Anthropic 协议不使用.
    pub reasoning_effort: Option<String>,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct ModelTestOutcome {
    pub status: i64,
    pub duration_ms: i64,
    pub first_token_ms: Option<i64>,
    pub usage: TokenUsage,
    pub output_text: String,
    pub reasoning_text: String,
    pub estimated_cost_usd: Option<f64>,
    pub error: Option<String>,
    pub raw: Option<Arc<ModelTestRawTrace>>,
    /// 测试使用的模型名.
    pub model: String,
    /// 是否为流式请求, 用于区分首 token 未返回与非流式.
    pub stream: bool,
    /// 上游显示名, 经调度组测试时为 None.
    pub upstream_name: Option<String>,
    /// 实际请求的端点路径.
    pub endpoint: String,
}

impl ModelTestOutcome {
    pub fn is_success(&self) -> bool {
        self.error.is_none() && (200..300).contains(&self.status)
    }

    fn failed(status: i64, duration_ms: i64, error: String) -> Self {
        Self {
            status,
            duration_ms,
            first_token_ms: None,
            usage: TokenUsage::default(),
            output_text: String::new(),
            reasoning_text: String::new(),
            estimated_cost_usd: None,
            error: Some(error),
            raw: None,
            model: String::new(),
            stream: false,
            upstream_name: None,
            endpoint: String::new(),
        }
    }
}

/// 最终发出的请求信息, 用于组装原始报文.
#[derive(Debug, Clone)]
struct RequestTrace {
    started_at: DateTime<Utc>,
    url: String,
    headers: Vec<(String, String)>,
    body: String,
}

/// 直连指定上游执行一次测试. 请求日志照常写入数据库供日志页查看,
/// 测试台自身的记录由 UI 保存在内存中.
/// `api_key` 用于临时上游的明文密钥, 空字符串表示不带认证;
/// 保存过的上游传 None, 认证信息从凭据存储读取.
pub async fn run_direct_test(
    state: &AppState,
    upstream: &Upstream,
    api_key: Option<&str>,
    params: ModelTestParams,
    on_delta: Option<ModelTestDeltaSink>,
) -> ModelTestOutcome {
    let started = Instant::now();
    let started_at = Utc::now();
    let mut outcome = send_direct(state, upstream, api_key, &params, started, started_at, &on_delta)
        .await;
    outcome.estimated_cost_usd =
        estimate_outcome_cost(state, &params.model, &outcome.usage, Some(upstream)).await;
    outcome.model = params.model.clone();
    outcome.stream = params.stream;
    outcome.upstream_name = Some(upstream.name.clone());
    outcome.endpoint = endpoint_path(effective_wire_api(upstream)).to_string();
    record_request_log(
        state,
        RequestLog {
            ts: None,
            upstream_id: Some(upstream.id.clone()),
            upstream_name: Some(upstream.name.clone()),
            source: RequestLogSource::TestBench,
            endpoint: outcome.endpoint.clone(),
            model: Some(params.model.clone()),
            target_model: None,
            reasoning_effort: params.reasoning_effort.clone(),
            status: outcome.status,
            usage: outcome.usage.clone(),
            estimated_cost_usd: outcome.estimated_cost_usd,
            duration_ms: outcome.duration_ms,
            first_token_ms: outcome.first_token_ms,
            error: outcome.error.clone(),
        },
        None,
    )
    .await;
    outcome
}

/// 经本地代理端口执行一次测试, 请求日志由代理正常链路落库.
/// `group_id` 指定调度组, 缺省时使用当前调度组.
pub async fn run_scheduler_test(
    state: &AppState,
    bind_addr: &str,
    local_key: &str,
    group_id: Option<&str>,
    params: ModelTestParams,
    on_delta: Option<ModelTestDeltaSink>,
) -> ModelTestOutcome {
    let mut outcome =
        run_scheduler_test_inner(state, bind_addr, local_key, group_id, &params, on_delta).await;
    outcome.model = params.model;
    outcome.stream = params.stream;
    outcome.endpoint = LOCAL_TEST_ENDPOINT.to_string();
    outcome
}

async fn run_scheduler_test_inner(
    state: &AppState,
    bind_addr: &str,
    local_key: &str,
    group_id: Option<&str>,
    params: &ModelTestParams,
    on_delta: Option<ModelTestDeltaSink>,
) -> ModelTestOutcome {
    let started = Instant::now();
    let started_at = Utc::now();
    let url = format!("http://{}{LOCAL_TEST_ENDPOINT}", bind_addr.trim());
    let value = build_request_body(WireApi::Responses, params);
    let body = match serde_json::to_vec(&value) {
        Ok(body) => body,
        Err(err) => {
            return ModelTestOutcome::failed(
                INTERNAL_ERROR_STATUS,
                elapsed_ms(started),
                format!("构造测试请求失败: {err}"),
            );
        }
    };
    let request_body = serde_json::to_string_pretty(&value).unwrap_or_default();
    let mut request = state
        .http
        .post(&url)
        .header("content-type", "application/json")
        .bearer_auth(local_key)
        .header(TEST_SOURCE_HEADER, TEST_SOURCE_HEADER_VALUE)
        .body(body)
        .timeout(params.timeout);
    if let Some(group_id) = group_id.map(str::trim).filter(|value| !value.is_empty()) {
        request = request.header(TEST_GROUP_HEADER, group_id);
    }
    let Ok(built) = request.build() else {
        return ModelTestOutcome::failed(
            INTERNAL_ERROR_STATUS,
            elapsed_ms(started),
            "构造测试请求失败".to_string(),
        );
    };
    let trace = RequestTrace {
        started_at,
        url: built.url().to_string(),
        headers: header_pairs(built.headers()),
        body: request_body,
    };
    match state.http.execute(built).await {
        Ok(response) => {
            consume_response(
                TestResponse::Http(response),
                started,
                trace,
                on_delta.as_ref(),
            )
            .await
        }
        Err(err) => {
            let mut outcome = ModelTestOutcome::failed(
                INTERNAL_ERROR_STATUS,
                elapsed_ms(started),
                format!("无法连接本地代理 {url}: {err}"),
            );
            outcome.raw = Some(Arc::new(empty_response_trace(trace)));
            outcome
        }
    }
}

/// 拉取指定上游的模型 id 列表, OAuth 上游不支持.
pub async fn fetch_upstream_model_ids(
    state: &AppState,
    upstream: &Upstream,
) -> anyhow::Result<Vec<String>> {
    if upstream.kind == UpstreamKind::CodexOauth {
        anyhow::bail!("OAuth 上游无法拉取模型列表, 请手动输入模型名");
    }
    let items = super::models::query_relay_models(state, &HeaderMap::new(), upstream).await?;
    Ok(items
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
        .collect())
}

async fn send_direct(
    state: &AppState,
    upstream: &Upstream,
    api_key: Option<&str>,
    params: &ModelTestParams,
    started: Instant,
    started_at: DateTime<Utc>,
    on_delta: &Option<ModelTestDeltaSink>,
) -> ModelTestOutcome {
    let wire_api = effective_wire_api(upstream);
    let value = build_request_body(wire_api, params);
    let body = match serde_json::to_vec(&value) {
        Ok(body) => body,
        Err(err) => {
            return ModelTestOutcome::failed(
                INTERNAL_ERROR_STATUS,
                elapsed_ms(started),
                format!("构造测试请求失败: {err}"),
            );
        }
    };
    let request_body = serde_json::to_string_pretty(&value).unwrap_or_default();
    let url = target_url_for(upstream, wire_api);
    let (response, trace) = match send_request(
        state,
        upstream,
        api_key,
        &url,
        body,
        request_body,
        params.timeout,
        started_at,
    )
    .await
    {
        Ok(pair) => pair,
        Err((err, trace)) => {
            let mut outcome = ModelTestOutcome::failed(
                INTERNAL_ERROR_STATUS,
                elapsed_ms(started),
                err.to_string(),
            );
            if let Some(trace) = trace {
                outcome.raw = Some(Arc::new(empty_response_trace(trace)));
            }
            return outcome;
        }
    };
    consume_response(response, started, trace, on_delta.as_ref()).await
}

fn effective_wire_api(upstream: &Upstream) -> WireApi {
    if upstream.kind == UpstreamKind::CodexOauth {
        WireApi::Responses
    } else {
        upstream.wire_api
    }
}

fn endpoint_path(wire_api: WireApi) -> &'static str {
    match wire_api {
        WireApi::Responses => "/responses",
        WireApi::ChatCompletions => "/chat/completions",
        WireApi::AnthropicMessages => "/messages",
    }
}

fn target_url_for(upstream: &Upstream, wire_api: WireApi) -> String {
    let path = endpoint_path(wire_api);
    if upstream.kind == UpstreamKind::CodexOauth {
        format!("https://chatgpt.com/backend-api/codex{path}")
    } else {
        transform::build_endpoint(&upstream.base_url, path)
    }
}

type SendOutcome = Result<(TestResponse, RequestTrace), (anyhow::Error, Option<RequestTrace>)>;

#[allow(clippy::too_many_arguments)]
async fn send_request(
    state: &AppState,
    upstream: &Upstream,
    api_key: Option<&str>,
    url: &str,
    body: Vec<u8>,
    request_body: String,
    timeout: Duration,
    started_at: DateTime<Utc>,
) -> SendOutcome {
    match upstream.kind {
        UpstreamKind::PeerNode => {
            let send = async {
                let http = state.http_for_peer_upstream(upstream).await?;
                let mut headers = peer_request_headers(state, &HeaderMap::new(), None)?;
                headers.insert(
                    hyper::header::CONTENT_TYPE,
                    hyper::header::HeaderValue::from_static("application/json"),
                );
                let trace = RequestTrace {
                    started_at,
                    url: url.to_string(),
                    headers: headers
                        .iter()
                        .map(|(name, value)| {
                            (name.to_string(), value.to_str().unwrap_or("").to_string())
                        })
                        .collect(),
                    body: request_body,
                };
                let send_future = http.send("POST", url, headers, body);
                let response = tokio::time::timeout(timeout, send_future)
                    .await
                    .map_err(|_| anyhow::anyhow!("请求超时 ({}s)", timeout.as_secs().max(1)))??;
                Ok((TestResponse::Peer(response), trace))
            };
            send.await.map_err(|err| (err, None))
        }
        _ => {
            let send = async {
                let http = state.http_for_upstream(upstream)?;
                let mut request = http
                    .post(url)
                    .header("content-type", "application/json")
                    .body(body)
                    .timeout(timeout);
                request = match api_key {
                    // 临时上游: 空字符串表示不带认证, 非空直接使用.
                    Some(api_key) => {
                        let request = if api_key.is_empty() {
                            request
                        } else {
                            upstream_auth::apply_api_key_auth(request, upstream, api_key)
                        };
                        upstream_auth::apply_anthropic_version(request, upstream)
                    }
                    None => {
                        apply_headers(state, upstream, request, &HeaderMap::new(), None).await?
                    }
                };
                let built = request.build()?;
                let trace = RequestTrace {
                    started_at,
                    url: built.url().to_string(),
                    headers: header_pairs(built.headers()),
                    body: request_body,
                };
                let response = http.execute(built).await?;
                Ok((TestResponse::Http(response), trace))
            };
            send.await.map_err(|err| (err, None))
        }
    }
}

enum TestResponse {
    Http(reqwest::Response),
    Peer(PeerHttpResponse),
}

impl TestResponse {
    fn status(&self) -> i64 {
        match self {
            Self::Http(response) => i64::from(response.status().as_u16()),
            Self::Peer(response) => i64::from(response.status.as_u16()),
        }
    }

    fn is_streaming(&self) -> bool {
        let content_type = match self {
            Self::Http(response) => response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Self::Peer(response) => response
                .headers
                .get(hyper::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
        };
        content_type.is_some_and(|value| value.contains("text/event-stream"))
    }

    fn content_type(&self) -> String {
        let value = match self {
            Self::Http(response) => response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Self::Peer(response) => response
                .headers
                .get(hyper::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
        };
        value.unwrap_or_default().to_string()
    }

    fn response_headers(&self) -> Vec<(String, String)> {
        match self {
            Self::Http(response) => response.headers().iter().map(
                |(name, value)| (name.to_string(), value.to_str().unwrap_or("").to_string()),
            ).collect(),
            Self::Peer(response) => response.headers.iter().map(
                |(name, value)| (name.to_string(), value.to_str().unwrap_or("").to_string()),
            ).collect(),
        }
    }

    async fn bytes(self) -> anyhow::Result<Bytes> {
        match self {
            Self::Http(response) => Ok(response.bytes().await?),
            Self::Peer(response) => Ok(response.bytes().await?),
        }
    }

    fn into_stream(self) -> BoxStream<'static, Result<Bytes, io::Error>> {
        match self {
            Self::Http(response) => Box::pin(
                response
                    .bytes_stream()
                    .map(|item| item.map_err(io::Error::other)),
            ),
            Self::Peer(response) => Box::pin(response.bytes_stream()),
        }
    }
}

#[derive(Default)]
struct RawBodyCapture {
    buffer: Vec<u8>,
    text: String,
    truncated: bool,
}

impl RawBodyCapture {
    fn extend(&mut self, bytes: &[u8]) {
        if self.truncated {
            return;
        }
        let remaining = RESPONSE_CAPTURE_LIMIT.saturating_sub(self.buffer.len());
        if bytes.len() > remaining {
            self.buffer.extend_from_slice(&bytes[..remaining]);
            self.truncated = true;
        } else {
            self.buffer.extend_from_slice(bytes);
        }
        if self.buffer.len() >= RESPONSE_CAPTURE_LIMIT {
            self.truncated = true;
        }
        self.text = String::from_utf8_lossy(&self.buffer).into_owned();
    }
}

/// 消费上游响应: 非流式直接解析, 流式逐块解析并实时回传正文/思维链增量,
/// 同时捕获原始响应报文.
async fn consume_response(
    response: TestResponse,
    started: Instant,
    request_trace: RequestTrace,
    on_delta: Option<&ModelTestDeltaSink>,
) -> ModelTestOutcome {
    let status = response.status();
    let content_type = response.content_type();
    let response_headers = response.response_headers();
    let mut captured = RawBodyCapture::default();

    if !response.is_streaming() {
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                return finish_outcome(
                    request_trace,
                    response_headers,
                    captured,
                    status,
                    content_type,
                    elapsed_ms(started),
                    None,
                    TokenUsage::default(),
                    String::new(),
                    String::new(),
                    Some(format!("读取响应失败: {err}")),
                );
            }
        };
        captured.extend(&bytes);
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            return finish_outcome(
                request_trace,
                response_headers,
                captured,
                status,
                content_type,
                elapsed_ms(started),
                None,
                TokenUsage::default(),
                String::new(),
                String::new(),
                Some(
                    extract_error_message(&value)
                        .unwrap_or_else(|| format!("上游返回状态码 {status}")),
                ),
            );
        }
        let mut usage = usage::extract_usage_from_json(&value);
        usage.finish();
        let (output_text, reasoning_text) = json_parts(&value);
        return finish_outcome(
            request_trace,
            response_headers,
            captured,
            status,
            content_type,
            elapsed_ms(started),
            None,
            usage,
            output_text,
            reasoning_text,
            None,
        );
    }

    let mut stream = response.into_stream();
    let mut first_token_ms = None;
    let mut output = String::new();
    let mut reasoning = String::new();
    let mut usage = TokenUsage::default();
    let mut stream_error = None;
    loop {
        let Some(chunk) = stream.next().await else {
            break;
        };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(err) => {
                stream_error = Some(format!("读取流式响应失败: {err}"));
                break;
            }
        };
        if first_token_ms.is_none() && !chunk.is_empty() {
            first_token_ms = Some(elapsed_ms(started));
        }
        captured.extend(&chunk);
        while let Some((index, separator_len)) = super::find_sse_block_separator(&captured.buffer)
        {
            let block = String::from_utf8_lossy(&captured.buffer[..index]).into_owned();
            usage.merge_max(&usage::extract_usage_from_sse(&block));
            if usage::has_anthropic_usage_event(&block) {
                usage.total_tokens = usage.input_tokens + usage.output_tokens;
            }
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
                for (is_reasoning, text) in sse_event_parts(&value) {
                    if is_reasoning {
                        reasoning.push_str(&text);
                    } else {
                        output.push_str(&text);
                    }
                    if let Some(sink) = on_delta {
                        sink(if is_reasoning {
                            ModelTestStreamPart::Reasoning(text)
                        } else {
                            ModelTestStreamPart::Text(text)
                        });
                    }
                }
            }
            captured.buffer.drain(..index + separator_len);
        }
    }
    usage.finish();
    finish_outcome(
        request_trace,
        response_headers,
        captured,
        status,
        content_type,
        elapsed_ms(started),
        first_token_ms,
        usage,
        output,
        reasoning,
        stream_error,
    )
}

/// 汇总一次测试的最终结果, 并组装可查看的原始报文.
#[allow(clippy::too_many_arguments)]
fn finish_outcome(
    request_trace: RequestTrace,
    response_headers: Vec<(String, String)>,
    captured: RawBodyCapture,
    status: i64,
    content_type: String,
    duration_ms: i64,
    first_token_ms: Option<i64>,
    usage: TokenUsage,
    output_text: String,
    reasoning_text: String,
    error: Option<String>,
) -> ModelTestOutcome {
    let raw = Arc::new(ModelTestRawTrace {
        started_at: request_trace.started_at,
        request_url: request_trace.url,
        request_headers: request_trace.headers,
        request_body: request_trace.body,
        response_status: status,
        response_content_type: content_type,
        response_headers,
        response_body: captured.text,
        response_truncated: captured.truncated,
    });
    ModelTestOutcome {
        status,
        duration_ms,
        first_token_ms,
        usage,
        output_text,
        reasoning_text,
        estimated_cost_usd: None,
        error,
        raw: Some(raw),
        // 元数据字段由 run_direct_test / run_scheduler_test 在外层统一填充.
        model: String::new(),
        stream: false,
        upstream_name: None,
        endpoint: String::new(),
    }
}

/// 网络层失败时只有请求侧信息, 响应内容为空.
fn empty_response_trace(trace: RequestTrace) -> ModelTestRawTrace {
    ModelTestRawTrace {
        started_at: trace.started_at,
        request_url: trace.url,
        request_headers: trace.headers,
        request_body: trace.body,
        response_status: 0,
        response_content_type: String::new(),
        response_headers: Vec::new(),
        response_body: String::new(),
        response_truncated: false,
    }
}

async fn estimate_outcome_cost(
    state: &AppState,
    model: &str,
    usage: &TokenUsage,
    upstream: Option<&Upstream>,
) -> Option<f64> {
    let price = state.store.find_model_price(model).await.ok().flatten()?;
    let multiplier = upstream
        .map(|upstream| upstream.price_multiplier)
        .unwrap_or(1.0);
    Some(pricing::estimate_usage_cost(usage, &price).total_usd() * multiplier)
}

/// 按上游协议构造测试请求体, 兼顾单条与多轮消息.
pub(super) fn build_request_body(wire_api: WireApi, params: &ModelTestParams) -> Value {
    match wire_api {
        WireApi::Responses => {
            let input = params
                .messages
                .iter()
                .map(|message| {
                    let part_type = if message.role == "assistant" {
                        "output_text"
                    } else {
                        "input_text"
                    };
                    json!({
                        "role": message.role,
                        "content": [{"type": part_type, "text": message.content}],
                    })
                })
                .collect::<Vec<_>>();
            let mut body = json!({
                "model": params.model,
                "input": input,
                "store": false,
                "stream": params.stream,
                "max_output_tokens": params.max_tokens,
            });
            if let Some(effort) = &params.reasoning_effort {
                // 带 summary 上游才会下发 reasoning summary text, 与 Codex CLI 行为一致.
                body["reasoning"] = json!({"effort": effort, "summary": "auto"});
            }
            body
        }
        WireApi::ChatCompletions => {
            let messages = chat_style_messages(params);
            let mut body = json!({
                "model": params.model,
                "messages": messages,
                "store": false,
                "stream": params.stream,
                "max_tokens": params.max_tokens,
            });
            if params.stream {
                body["stream_options"] = json!({"include_usage": true});
            }
            if let Some(effort) = &params.reasoning_effort {
                body["reasoning_effort"] = json!(effort);
            }
            body
        }
        WireApi::AnthropicMessages => json!({
            "model": params.model,
            "messages": chat_style_messages(params),
            "stream": params.stream,
            "max_tokens": params.max_tokens,
        }),
    }
}

fn chat_style_messages(params: &ModelTestParams) -> Vec<Value> {
    params
        .messages
        .iter()
        .map(|message| json!({"role": message.role, "content": message.content}))
        .collect()
}

/// 解析一条 SSE 事件中的增量, 返回 (是否为思维链, 文本).
/// 兼容 ChatCompletions, Responses 与 Anthropic 三种流式事件格式.
pub(super) fn sse_event_parts(value: &Value) -> Vec<(bool, String)> {
    let mut parts = Vec::new();
    if let Some(text) = value
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        parts.push((false, text.to_string()));
    }
    for key in [
        "/choices/0/delta/reasoning_content",
        "/choices/0/delta/reasoning",
    ] {
        if let Some(text) = value
            .pointer(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            parts.push((true, text.to_string()));
        }
    }
    match value.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => {
            if let Some(text) = value
                .get("delta")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                parts.push((false, text.to_string()));
            }
        }
        Some(event_type)
            if event_type.starts_with("response.reasoning") && event_type.ends_with(".delta") =>
        {
            if let Some(text) = value
                .get("delta")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                parts.push((true, text.to_string()));
            }
        }
        _ => {}
    }
    if value.get("type").and_then(Value::as_str) == Some("content_block_delta") {
        match value.pointer("/delta/type").and_then(Value::as_str) {
            Some("text_delta") => {
                if let Some(text) = value
                    .pointer("/delta/text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    parts.push((false, text.to_string()));
                }
            }
            Some("thinking_delta") => {
                if let Some(text) = value
                    .pointer("/delta/thinking")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    parts.push((true, text.to_string()));
                }
            }
            _ => {}
        }
    }
    parts
}

/// 解析非流式响应 JSON, 返回 (正文, 思维链).
pub(super) fn json_parts(value: &Value) -> (String, String) {
    if let Some(text) = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
    {
        let reasoning = value
            .pointer("/choices/0/message/reasoning_content")
            .or_else(|| value.pointer("/choices/0/message/reasoning"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        return (text.to_string(), reasoning.to_string());
    }
    if let Some(items) = value.get("output").and_then(Value::as_array) {
        let mut text = String::new();
        let mut reasoning = String::new();
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    let Some(parts) = item.get("content").and_then(Value::as_array) else {
                        continue;
                    };
                    for part in parts {
                        if part.get("type").and_then(Value::as_str) == Some("output_text")
                            && let Some(part_text) = part.get("text").and_then(Value::as_str)
                        {
                            text.push_str(part_text);
                        }
                    }
                }
                Some("reasoning") => {
                    let Some(summaries) = item.get("summary").and_then(Value::as_array) else {
                        continue;
                    };
                    for part in summaries {
                        if part.get("type").and_then(Value::as_str) == Some("summary_text")
                            && let Some(part_text) = part.get("text").and_then(Value::as_str)
                        {
                            reasoning.push_str(part_text);
                        }
                    }
                }
                _ => {}
            }
        }
        return (text, reasoning);
    }
    if let Some(parts) = value.get("content").and_then(Value::as_array) {
        let mut text = String::new();
        let mut reasoning = String::new();
        for part in parts {
            match part.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(part_text) = part.get("text").and_then(Value::as_str) {
                        text.push_str(part_text);
                    }
                }
                Some("thinking") => {
                    if let Some(part_text) = part.get("thinking").and_then(Value::as_str) {
                        reasoning.push_str(part_text);
                    }
                }
                _ => {}
            }
        }
        return (text, reasoning);
    }
    (String::new(), String::new())
}

pub(super) fn extract_error_message(value: &Value) -> Option<String> {
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn header_pairs(headers: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_str().unwrap_or("").to_string()))
        .collect()
}

fn elapsed_ms(started: Instant) -> i64 {
    started.elapsed().as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(wire_api: WireApi) -> ModelTestParams {
        let _ = wire_api;
        ModelTestParams {
            model: "gpt-test".to_string(),
            messages: vec![
                ModelTestMessage::user("ping"),
                ModelTestMessage::assistant("pong"),
                ModelTestMessage::user("again"),
            ],
            stream: true,
            max_tokens: 64,
            reasoning_effort: Some("low".to_string()),
            timeout: Duration::from_secs(30),
        }
    }

    #[test]
    fn builds_responses_body_with_typed_content() {
        let body = build_request_body(WireApi::Responses, &params(WireApi::Responses));
        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["max_output_tokens"], 64);
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][1]["content"][0]["type"], "output_text");
        assert_eq!(body["reasoning"]["effort"], "low");
    }

    #[test]
    fn builds_chat_completions_body_with_usage_option() {
        let body = build_request_body(WireApi::ChatCompletions, &params(WireApi::ChatCompletions));
        assert_eq!(body["max_tokens"], 64);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["reasoning_effort"], "low");

        let mut no_stream = params(WireApi::ChatCompletions);
        no_stream.stream = false;
        no_stream.reasoning_effort = None;
        let body = build_request_body(WireApi::ChatCompletions, &no_stream);
        assert!(body.get("stream_options").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn builds_anthropic_body_without_reasoning() {
        let body =
            build_request_body(WireApi::AnthropicMessages, &params(WireApi::AnthropicMessages));
        assert_eq!(body["max_tokens"], 64);
        assert_eq!(body["messages"][0]["content"], "ping");
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn parses_stream_events_of_all_protocols() {
        let chat_text = json!({"choices":[{"delta":{"content":"hi"}}]});
        assert_eq!(sse_event_parts(&chat_text), vec![(false, "hi".to_string())]);

        let chat_reasoning = json!({"choices":[{"delta":{"reasoning_content":"hmm"}}]});
        assert_eq!(
            sse_event_parts(&chat_reasoning),
            vec![(true, "hmm".to_string())]
        );

        let responses_text = json!({"type":"response.output_text.delta","delta":"hello"});
        assert_eq!(
            sse_event_parts(&responses_text),
            vec![(false, "hello".to_string())]
        );

        let responses_reasoning =
            json!({"type":"response.reasoning_summary_text.delta","delta":"thinking"});
        assert_eq!(
            sse_event_parts(&responses_reasoning),
            vec![(true, "thinking".to_string())]
        );

        let anthropic_text =
            json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"yo"}});
        assert_eq!(
            sse_event_parts(&anthropic_text),
            vec![(false, "yo".to_string())]
        );

        let anthropic_thinking =
            json!({"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"..."}});
        assert_eq!(
            sse_event_parts(&anthropic_thinking),
            vec![(true, "...".to_string())]
        );

        let usage_chunk = json!({"choices":[{"delta":{}}],"usage":{"total_tokens":9}});
        assert!(sse_event_parts(&usage_chunk).is_empty());
    }

    #[test]
    fn parses_json_parts_of_all_shapes() {
        let chat = json!({"choices":[{"message":{"content":"hi","reasoning_content":"hmm"}}]});
        assert_eq!(json_parts(&chat), ("hi".to_string(), "hmm".to_string()));

        let responses = json!({"output":[
            {"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}]},
            {"type":"message","content":[
                {"type":"output_text","text":"a"},{"type":"output_text","text":"b"}
            ]}
        ]});
        assert_eq!(
            json_parts(&responses),
            ("ab".to_string(), "plan".to_string())
        );

        let anthropic = json!({"content":[
            {"type":"thinking","thinking":"deep"},
            {"type":"text","text":"c"}
        ]});
        assert_eq!(json_parts(&anthropic), ("c".to_string(), "deep".to_string()));
    }

    #[test]
    fn extracts_error_message_from_common_shapes() {
        assert_eq!(
            extract_error_message(&json!({"error":{"message":"boom"}})).as_deref(),
            Some("boom")
        );
        assert_eq!(
            extract_error_message(&json!({"error":"plain"})).as_deref(),
            Some("plain")
        );
        assert!(extract_error_message(&json!({"ok":true})).is_none());
    }

    #[test]
    fn builds_har_entry_with_request_and_response() {
        let trace = ModelTestRawTrace {
            started_at: Utc::now(),
            request_url: "https://relay.example.com/v1/chat/completions".to_string(),
            request_headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("authorization".to_string(), "Bearer cs-secret".to_string()),
            ],
            request_body: "{\"model\":\"gpt-test\"}".to_string(),
            response_status: 200,
            response_content_type: "application/json".to_string(),
            response_headers: vec![("server".to_string(), "test".to_string())],
            response_body: "{\"ok\":true}".to_string(),
            response_truncated: false,
        };

        let entry = trace.to_har_entry(123, None);
        assert_eq!(entry["request"]["method"], "POST");
        assert_eq!(entry["request"]["url"], trace.request_url);
        assert_eq!(entry["request"]["postData"]["text"], trace.request_body);
        assert_eq!(entry["response"]["status"], 200);
        assert_eq!(entry["response"]["content"]["text"], trace.response_body);
        assert_eq!(entry["time"], 123);
        assert_eq!(entry["timings"]["wait"], 123);
        assert_eq!(entry["request"]["headers"][0]["name"], "content-type");
        assert_eq!(entry["request"]["headers"][1]["value"], "[REDACTED]");
        assert!(entry.get("_error").is_none());

        let failed = trace.to_har_entry(456, Some("timeout"));
        assert_eq!(failed["_error"], "timeout");
    }
}

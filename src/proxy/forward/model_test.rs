//! 模型测试台执行器: 绕过调度组直连上游, 或经本地代理端口走完整链路,
//! 发送小体积测试请求并统计耗时, 首 token 延迟, tokens 与估算费用.

use crate::app::AppState;
use crate::core::models::{
    RequestLog, RequestLogSource, TokenUsage, Upstream, UpstreamKind, WireApi,
};
use crate::peer::client::PeerHttpResponse;
use crate::pricing;
use crate::proxy::transform;
use crate::usage;

use super::TEST_SOURCE_HEADER;
use super::headers::{apply_headers, peer_request_headers};
use super::logging::record_request_log;

use axum::http::HeaderMap;
use bytes::Bytes;
use futures_util::{StreamExt, stream::BoxStream};
use serde_json::{Value, json};
use std::io;
use std::time::{Duration, Instant};

/// 测试台请求经本地代理转发时携带的来源标记值.
pub(crate) const TEST_SOURCE_HEADER_VALUE: &str = "test-bench";

/// 经调度组模式固定使用的本地代理端点, 协议转换由代理内部完成.
const LOCAL_TEST_ENDPOINT: &str = "/v1/responses";

const INTERNAL_ERROR_STATUS: i64 = 502;

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
    pub estimated_cost_usd: Option<f64>,
    pub error: Option<String>,
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
            estimated_cost_usd: None,
            error: Some(error),
        }
    }
}

/// 直连指定上游执行一次测试, 并把结果写入 request_logs (来源标记为测试台).
pub async fn run_direct_test(
    state: &AppState,
    upstream: &Upstream,
    params: ModelTestParams,
) -> ModelTestOutcome {
    let started = Instant::now();
    let mut outcome = send_direct(state, upstream, &params, started).await;
    outcome.estimated_cost_usd = estimate_outcome_cost(
        state,
        &params.model,
        &outcome.usage,
        Some(upstream),
    )
    .await;
    let log = RequestLog {
        ts: None,
        upstream_id: Some(upstream.id.clone()),
        upstream_name: Some(upstream.name.clone()),
        source: RequestLogSource::TestBench,
        endpoint: endpoint_path(effective_wire_api(upstream)).to_string(),
        model: Some(params.model.clone()),
        target_model: None,
        reasoning_effort: params.reasoning_effort.clone(),
        status: outcome.status,
        usage: outcome.usage.clone(),
        estimated_cost_usd: outcome.estimated_cost_usd,
        duration_ms: outcome.duration_ms,
        first_token_ms: outcome.first_token_ms,
        error: outcome.error.clone(),
    };
    record_request_log(state, log, None).await;
    outcome
}

/// 经本地代理端口执行一次测试, 日志由代理正常链路落库.
pub async fn run_scheduler_test(
    state: &AppState,
    bind_addr: &str,
    local_key: &str,
    params: ModelTestParams,
) -> ModelTestOutcome {
    let started = Instant::now();
    let url = format!(
        "http://{}{LOCAL_TEST_ENDPOINT}",
        bind_addr.trim()
    );
    let body = match serde_json::to_vec(&build_request_body(WireApi::Responses, &params)) {
        Ok(body) => body,
        Err(err) => {
            return ModelTestOutcome::failed(
                INTERNAL_ERROR_STATUS,
                elapsed_ms(started),
                format!("构造测试请求失败: {err}"),
            );
        }
    };
    let request = state
        .http
        .post(&url)
        .header("content-type", "application/json")
        .bearer_auth(local_key)
        .header(TEST_SOURCE_HEADER, TEST_SOURCE_HEADER_VALUE)
        .body(body)
        .timeout(params.timeout);
    match request.send().await {
        Ok(response) => consume_response(TestResponse::Http(response), started).await,
        Err(err) => ModelTestOutcome::failed(
            INTERNAL_ERROR_STATUS,
            elapsed_ms(started),
            format!("无法连接本地代理 {url}: {err}"),
        ),
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
    params: &ModelTestParams,
    started: Instant,
) -> ModelTestOutcome {
    let wire_api = effective_wire_api(upstream);
    let body = match serde_json::to_vec(&build_request_body(wire_api, params)) {
        Ok(body) => body,
        Err(err) => {
            return ModelTestOutcome::failed(
                INTERNAL_ERROR_STATUS,
                elapsed_ms(started),
                format!("构造测试请求失败: {err}"),
            );
        }
    };
    let url = target_url_for(upstream, wire_api);
    let response = match send_request(state, upstream, &url, body, params.timeout).await {
        Ok(response) => response,
        Err(err) => {
            return ModelTestOutcome::failed(
                INTERNAL_ERROR_STATUS,
                elapsed_ms(started),
                err.to_string(),
            );
        }
    };
    consume_response(response, started).await
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

async fn send_request(
    state: &AppState,
    upstream: &Upstream,
    url: &str,
    body: Vec<u8>,
    timeout: Duration,
) -> anyhow::Result<TestResponse> {
    match upstream.kind {
        UpstreamKind::PeerNode => {
            let http = state.http_for_peer_upstream(upstream).await?;
            let mut headers = peer_request_headers(state, &HeaderMap::new(), None)?;
            headers.insert(
                hyper::header::CONTENT_TYPE,
                hyper::header::HeaderValue::from_static("application/json"),
            );
            let send_future = http.send("POST", url, headers, body);
            let response =
                tokio::time::timeout(timeout, send_future).await.map_err(|_| {
                    anyhow::anyhow!("请求超时 ({}s)", timeout.as_secs().max(1))
                })??;
            Ok(TestResponse::Peer(response))
        }
        _ => {
            let http = state.http_for_upstream(upstream)?;
            let mut request = http
                .post(url)
                .header("content-type", "application/json")
                .body(body)
                .timeout(timeout);
            request = apply_headers(state, upstream, request, &HeaderMap::new(), None).await?;
            let response = request.send().await?;
            Ok(TestResponse::Http(response))
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

async fn consume_response(response: TestResponse, started: Instant) -> ModelTestOutcome {
    let status = response.status();
    if !response.is_streaming() {
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                return ModelTestOutcome::failed(
                    status,
                    elapsed_ms(started),
                    format!("读取响应失败: {err}"),
                );
            }
        };
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            return ModelTestOutcome::failed(
                status,
                elapsed_ms(started),
                extract_error_message(&value)
                    .unwrap_or_else(|| format!("上游返回状态码 {status}")),
            );
        }
        let mut usage = usage::extract_usage_from_json(&value);
        usage.finish();
        return ModelTestOutcome {
            status,
            duration_ms: elapsed_ms(started),
            first_token_ms: None,
            usage,
            output_text: extract_output_text_from_json(&value),
            estimated_cost_usd: None,
            error: None,
        };
    }

    let mut stream = response.into_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let mut first_token_ms = None;
    let mut output = String::new();
    let mut usage = TokenUsage::default();
    loop {
        let Some(chunk) = stream.next().await else {
            break;
        };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(err) => {
                usage.finish();
                return ModelTestOutcome {
                    status,
                    duration_ms: elapsed_ms(started),
                    first_token_ms,
                    usage,
                    output_text: output,
                    estimated_cost_usd: None,
                    error: Some(format!("读取流式响应失败: {err}")),
                };
            }
        };
        if first_token_ms.is_none() && !chunk.is_empty() {
            first_token_ms = Some(elapsed_ms(started));
        }
        buffer.extend_from_slice(&chunk);
        while let Some((index, separator_len)) = super::find_sse_block_separator(&buffer) {
            let block = String::from_utf8_lossy(&buffer[..index]).into_owned();
            usage.merge_max(&usage::extract_usage_from_sse(&block));
            if usage::has_anthropic_usage_event(&block) {
                usage.total_tokens = usage.input_tokens + usage.output_tokens;
            }
            usage::for_each_sse_text_delta(&block, |delta| output.push_str(delta));
            buffer.drain(..index + separator_len);
        }
    }
    usage.finish();
    ModelTestOutcome {
        status,
        duration_ms: elapsed_ms(started),
        first_token_ms,
        usage,
        output_text: output,
        estimated_cost_usd: None,
        error: None,
    }
}

async fn estimate_outcome_cost(
    state: &AppState,
    model: &str,
    usage: &TokenUsage,
    upstream: Option<&Upstream>,
) -> Option<f64> {
    let price = state.store.find_model_price(model).await.ok().flatten()?;
    let multiplier = upstream.map(|upstream| upstream.price_multiplier).unwrap_or(1.0);
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
                body["reasoning"] = json!({"effort": effort});
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

pub(super) fn extract_output_text_from_json(value: &Value) -> String {
    if let Some(text) = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
    {
        return text.to_string();
    }
    if let Some(items) = value.get("output").and_then(Value::as_array) {
        let mut output = String::new();
        for item in items {
            let Some(parts) = item.get("content").and_then(Value::as_array) else {
                continue;
            };
            for part in parts {
                if part.get("type").and_then(Value::as_str) == Some("output_text")
                    && let Some(text) = part.get("text").and_then(Value::as_str)
                {
                    output.push_str(text);
                }
            }
        }
        if !output.is_empty() {
            return output;
        }
    }
    if let Some(parts) = value.get("content").and_then(Value::as_array) {
        let mut output = String::new();
        for part in parts {
            if part.get("type").and_then(Value::as_str) == Some("text")
                && let Some(text) = part.get("text").and_then(Value::as_str)
            {
                output.push_str(text);
            }
        }
        return output;
    }
    String::new()
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
        let body = build_request_body(WireApi::AnthropicMessages, &params(WireApi::AnthropicMessages));
        assert_eq!(body["max_tokens"], 64);
        assert_eq!(body["messages"][0]["content"], "ping");
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn extracts_output_text_from_all_shapes() {
        let chat = json!({"choices":[{"message":{"content":"hi"}}]});
        assert_eq!(extract_output_text_from_json(&chat), "hi");

        let responses = json!({"output":[{"content":[
            {"type":"output_text","text":"a"},{"type":"output_text","text":"b"}
        ]}]});
        assert_eq!(extract_output_text_from_json(&responses), "ab");

        let anthropic = json!({"content":[{"type":"text","text":"c"}]});
        assert_eq!(extract_output_text_from_json(&anthropic), "c");
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
}

use crate::core::models::WireApi;
use super::super::ChatResponseContext;
use axum::http::StatusCode;
use serde_json::{Value, json};

pub(crate) fn anthropic_to_responses_response_json(
    value: &Value,
    context: Option<&ChatResponseContext>,
) -> Value {
    let mut output = Vec::new();
    let mut message_parts = Vec::new();
    if let Some(content) = value.get("content").and_then(Value::as_array) {
        for block in content {
            match block.get("type").and_then(Value::as_str).unwrap_or_default() {
                "text" => message_parts.push(json!({
                    "type":"output_text",
                    "text":block.get("text").cloned().unwrap_or_else(|| json!("")),
                    "annotations":[]
                })),
                "thinking" => {
                    if block.get("signature").is_some() {
                        tracing::debug!("dropping Anthropic thinking signature during protocol conversion");
                    }
                    output.push(json!({
                        "id":new_id("rs"),
                        "type":"reasoning",
                        "summary":[{"type":"summary_text","text":block.get("thinking").cloned().unwrap_or_else(|| json!(""))}],
                        "status":"completed"
                    }));
                }
                "tool_use" => {
                    let item_id = new_id("fc");
                    let call_id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let arguments = argument_string(block.get("input"));
                    output.push(match context {
                        Some(context) => context.restore_tool_item(
                            &item_id,
                            "completed",
                            call_id,
                            name,
                            &arguments,
                        ),
                        None => json!({
                            "id":item_id,
                            "type":"function_call",
                            "call_id":call_id,
                            "name":name,
                            "arguments":arguments,
                            "status":"completed"
                        }),
                    });
                }
                "server_tool_use" if block.get("name").and_then(Value::as_str) == Some("web_search") => {
                    output.push(json!({
                        "id":block.get("id").cloned().unwrap_or_else(|| json!(new_id("ws"))),
                        "type":"web_search_call",
                        "action":block.get("input").cloned().unwrap_or_else(|| json!({})),
                        "status":"completed"
                    }));
                }
                _ => {}
            }
        }
    }
    if !message_parts.is_empty() {
        output.push(json!({
            "id":new_id("msg"),
            "type":"message",
            "role":"assistant",
            "content":message_parts,
            "status":"completed"
        }));
    }
    if output.is_empty() {
        output.push(json!({
            "id":new_id("msg"),
            "type":"message",
            "role":"assistant",
            "content":[{"type":"output_text","text":"","annotations":[]}],
            "status":"completed"
        }));
    }
    let status = if value.get("stop_reason").and_then(Value::as_str) == Some("max_tokens") {
        "incomplete"
    } else {
        "completed"
    };
    let usage = anthropic_usage_to_responses(value.get("usage"));
    let mut result = json!({
        "id":value.get("id").cloned().unwrap_or_else(|| json!(new_id("resp"))),
        "object":"response",
        "model":value.get("model").cloned().unwrap_or_else(|| json!("unknown")),
        "status":status,
        "output":output,
        "usage":usage
    });
    if status == "incomplete" {
        result["incomplete_details"] = json!({"reason":"max_output_tokens"});
    }
    result
}

pub(crate) fn responses_to_anthropic_response_json(value: &Value, model: Option<&str>) -> Value {
    let mut content = Vec::new();
    if let Some(output) = value.get("output").and_then(Value::as_array) {
        for item in output {
            match item.get("type").and_then(Value::as_str).unwrap_or_default() {
                "reasoning" => {
                    let text = item
                        .get("summary")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .collect::<String>();
                    if !text.is_empty() {
                        content.push(json!({"type":"thinking","thinking":text}));
                    }
                }
                "message" => {
                    if let Some(parts) = item.get("content").and_then(Value::as_array) {
                        for part in parts {
                            if part.get("type").and_then(Value::as_str) == Some("output_text") {
                                content.push(json!({
                                    "type":"text",
                                    "text":part.get("text").cloned().unwrap_or_else(|| json!(""))
                                }));
                            }
                        }
                    }
                }
                "function_call" | "custom_tool_call" => content.push(json!({
                    "type":"tool_use",
                    "id":anthropic_call_id(item),
                    "name":item.get("name").cloned().unwrap_or_else(|| json!("")),
                    "input":tool_input(item)
                })),
                "web_search_call" => content.push(json!({
                    "type":"server_tool_use",
                    "id":format!("srvtoolu_{}", item.get("id").and_then(Value::as_str).unwrap_or_default()),
                    "name":"web_search",
                    "input":item.get("action").cloned().unwrap_or_else(|| json!({}))
                })),
                _ => {}
            }
        }
    }
    if content.is_empty() {
        content.push(json!({"type":"text","text":""}));
    }
    let has_tool = content.iter().any(|block| {
        matches!(
            block.get("type").and_then(Value::as_str),
            Some("tool_use" | "server_tool_use")
        )
    });
    let stop_reason = if value.get("status").and_then(Value::as_str) == Some("incomplete") {
        "max_tokens"
    } else if has_tool {
        "tool_use"
    } else {
        "end_turn"
    };
    json!({
        "id":value.get("id").cloned().unwrap_or_else(|| json!(new_id("msg"))),
        "type":"message",
        "role":"assistant",
        "model":model.map(str::to_string).or_else(|| value.get("model").and_then(Value::as_str).map(str::to_string)).unwrap_or_else(|| "unknown".to_string()),
        "content":content,
        "stop_reason":stop_reason,
        "stop_sequence":Value::Null,
        "usage":responses_usage_to_anthropic(value.get("usage"))
    })
}

pub(crate) fn error_response_json(
    status: StatusCode,
    body: &[u8],
    client_api: WireApi,
) -> Vec<u8> {
    let value = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    let message = value
        .pointer("/error/message")
        .or_else(|| value.pointer("/response/error/message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| std::str::from_utf8(body).unwrap_or("upstream request failed"));
    let error_type = if client_api == WireApi::AnthropicMessages {
        match status {
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => "invalid_request_error",
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "authentication_error",
            StatusCode::NOT_FOUND => "not_found_error",
            StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
            status if status.as_u16() == 529 => "overloaded_error",
            _ => "api_error",
        }
    } else {
        value
            .pointer("/error/type")
            .and_then(Value::as_str)
            .unwrap_or("proxy_error")
    };
    let output = if client_api == WireApi::AnthropicMessages {
        json!({"type":"error","error":{"type":error_type,"message":message}})
    } else {
        json!({"error":{"type":error_type,"message":message}})
    };
    serde_json::to_vec(&output).unwrap_or_else(|_| body.to_vec())
}

fn anthropic_usage_to_responses(value: Option<&Value>) -> Value {
    let input = int_field(value, "input_tokens");
    let output = int_field(value, "output_tokens");
    let cache_read = int_field(value, "cache_read_input_tokens");
    let cache_creation = int_field(value, "cache_creation_input_tokens");
    let total_input = input + cache_read + cache_creation;
    json!({
        "input_tokens":total_input,
        "output_tokens":output,
        "total_tokens":total_input + output,
        "input_tokens_details":{"cached_tokens":cache_read},
        "cache_creation_input_tokens":cache_creation
    })
}

fn responses_usage_to_anthropic(value: Option<&Value>) -> Value {
    let input = int_field(value, "input_tokens");
    let output = int_field(value, "output_tokens");
    let cache_read = value
        .and_then(|usage| usage.pointer("/input_tokens_details/cached_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let cache_creation = int_field(value, "cache_creation_input_tokens");
    json!({
        "input_tokens":input.saturating_sub(cache_read + cache_creation),
        "output_tokens":output,
        "cache_read_input_tokens":cache_read,
        "cache_creation_input_tokens":cache_creation
    })
}

fn int_field(value: Option<&Value>, key: &str) -> i64 {
    value.and_then(|value| value.get(key)).and_then(Value::as_i64).unwrap_or(0)
}

fn tool_input(item: &Value) -> Value {
    if item.get("type").and_then(Value::as_str) == Some("custom_tool_call") {
        return match item.get("input") {
            Some(Value::String(input)) => serde_json::from_str(input).unwrap_or_else(|_| json!({"input":input})),
            Some(input) => input.clone(),
            None => json!({}),
        };
    }
    let arguments = argument_string(item.get("arguments"));
    serde_json::from_str(&arguments).unwrap_or_else(|_| json!({"input":arguments}))
}

fn anthropic_call_id(item: &Value) -> String {
    let id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if id.starts_with("toolu_") || id.starts_with("call_") {
        id.to_string()
    } else {
        format!("toolu_{id}")
    }
}

fn argument_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(value) if !value.is_null() => value.to_string(),
        _ => "{}".to_string(),
    }
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use serde_json::{Value, json};

const REASONING_PREFIX: &str = "codex-switch-reasoning-v1:";

pub(super) fn reasoning_from_response_item(item: &Value) -> Option<String> {
    if let Some(encoded) = item.get("encrypted_content").and_then(Value::as_str)
        && let Some(decoded) = decode_reasoning(encoded)
    {
        return Some(decoded);
    }
    item.get("summary")
        .and_then(Value::as_array)
        .map(|summary| {
            summary
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|summary| !summary.is_empty())
}

pub(super) fn reasoning_from_chat(value: &Value) -> Option<String> {
    for key in ["reasoning_content", "reasoning"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            return Some(text.to_string());
        }
    }
    value
        .get("reasoning_details")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .or_else(|| part.get("content"))
                        .and_then(Value::as_str)
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .filter(|text| !text.is_empty())
}

pub(super) fn encode_reasoning(reasoning: &str) -> String {
    format!(
        "{REASONING_PREFIX}{}",
        STANDARD_NO_PAD.encode(reasoning.as_bytes())
    )
}

fn decode_reasoning(value: &str) -> Option<String> {
    let encoded = value.strip_prefix(REASONING_PREFIX)?;
    let bytes = STANDARD_NO_PAD.decode(encoded).ok()?;
    String::from_utf8(bytes).ok()
}

pub(super) fn reasoning_item(reasoning: &str) -> Value {
    json!({
        "id":new_item_id("rs"),
        "type":"reasoning",
        "summary":[],
        "encrypted_content":encode_reasoning(reasoning),
        "status":"completed"
    })
}

pub(super) fn message_item(text: &str) -> Value {
    message_item_with_id(&new_item_id("msg"), text)
}

pub(super) fn message_item_with_id(id: &str, text: &str) -> Value {
    json!({
        "id":id,
        "type":"message",
        "role":"assistant",
        "content":[{"type":"output_text","text":text,"annotations":[]}],
        "status":"completed"
    })
}

pub(super) fn chat_message_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
        ),
        Value::Null => None,
        other => Some(json_text(other)),
    }
}

pub(super) fn normalize_chat_usage(usage: Option<&Value>) -> Value {
    let prompt_tokens = usage
        .and_then(|value| value.get("prompt_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let completion_tokens = usage
        .and_then(|value| value.get("completion_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let total_tokens = usage
        .and_then(|value| value.get("total_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(prompt_tokens + completion_tokens);
    let cached_tokens = usage
        .and_then(|value| value.pointer("/prompt_tokens_details/cached_tokens"))
        .and_then(Value::as_i64)
        .or_else(|| {
            usage
                .and_then(|value| value.get("prompt_cache_hit_tokens"))
                .and_then(Value::as_i64)
        })
        .unwrap_or(0);
    let reasoning_tokens = usage
        .and_then(|value| value.pointer("/completion_tokens_details/reasoning_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    json!({
        "input_tokens":prompt_tokens,
        "output_tokens":completion_tokens,
        "total_tokens":total_tokens,
        "input_tokens_details":{"cached_tokens":cached_tokens},
        "output_tokens_details":{"reasoning_tokens":reasoning_tokens}
    })
}

pub(super) fn response_id(value: &Value) -> String {
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| id.starts_with("resp_"))
        .map(str::to_string)
        .unwrap_or_else(|| new_item_id("resp"))
}

pub(super) fn argument_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(arguments)) => arguments.clone(),
        Some(value) => value.to_string(),
        None => "{}".to_string(),
    }
}

pub(super) fn custom_input(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|value| value.get("input").cloned())
        .map(|value| match value {
            Value::String(input) => input,
            other => json_text(&other),
        })
        .unwrap_or_else(|| arguments.to_string())
}

pub(super) fn json_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub(super) fn new_call_id() -> String {
    new_item_id("call")
}

pub(super) fn new_item_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

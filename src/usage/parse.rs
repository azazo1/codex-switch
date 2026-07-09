use crate::core::models::TokenUsage;
use serde_json::{Value, json};

pub fn extract_model(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("model").and_then(|v| v.as_str()).map(str::to_string))
}

pub fn extract_reasoning_effort(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body).ok().and_then(|value| {
        value
            .pointer("/reasoning/effort")
            .or_else(|| value.get("reasoning_effort"))
            .or_else(|| value.get("reasoningEffort"))
            .or_else(|| value.get("reasoning"))
            .and_then(Value::as_str)
            .map(format_reasoning_effort)
    })
}

pub fn extract_usage_from_json(value: &Value) -> TokenUsage {
    let usage = value
        .get("usage")
        .or_else(|| value.pointer("/response/usage"))
        .or_else(|| find_usage_value(value));
    usage.map(usage_from_value).unwrap_or_default()
}

pub fn extract_usage_from_sse(text: &str) -> TokenUsage {
    let mut usage = TokenUsage::default();
    let text = text.replace("\r\n", "\n");
    for block in text.split("\n\n") {
        for line in block.lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<Value>(data) {
                let item = extract_usage_from_json(&value);
                usage.merge_max(&item);
            }
        }
    }
    usage
}

fn find_usage_value(value: &Value) -> Option<&Value> {
    match value {
        Value::Object(map) => {
            if let Some(usage) = map.get("usage").filter(|value| looks_like_usage(value)) {
                return Some(usage);
            }
            map.values().find_map(find_usage_value)
        }
        Value::Array(values) => values.iter().find_map(find_usage_value),
        _ => None,
    }
}

fn looks_like_usage(value: &Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    [
        "input_tokens",
        "prompt_tokens",
        "output_tokens",
        "completion_tokens",
        "total_tokens",
        "input_tokens_details",
        "prompt_tokens_details",
        "cache_read_input_tokens",
    ]
    .iter()
    .any(|key| map.contains_key(*key))
}

pub fn for_each_sse_text_delta(text: &str, mut on_delta: impl FnMut(&str)) {
    let text = text.replace("\r\n", "\n");
    for block in text.split("\n\n") {
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
            if let Some(delta) = sse_text_delta(&value) {
                on_delta(delta);
            }
        }
    }
}

pub fn responses_to_chat_json(body: &[u8]) -> anyhow::Result<Vec<u8>> {
    let value: Value = serde_json::from_slice(body)?;
    let model = value
        .get("model")
        .cloned()
        .unwrap_or_else(|| json!("gpt-5"));
    let mut messages = Vec::new();
    if let Some(instructions) = value.get("instructions").and_then(|v| v.as_str()) {
        messages.push(json!({"role":"system","content":instructions}));
    }
    match value.get("input") {
        Some(Value::String(text)) => messages.push(json!({"role":"user","content":text})),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(message) = input_item_to_chat_message(item) {
                    messages.push(message);
                }
            }
        }
        Some(other) => messages.push(json!({"role":"user","content":other.to_string()})),
        None => {}
    }
    if messages.is_empty() {
        messages.push(json!({"role":"user","content":""}));
    }
    let mut result = json!({
        "model": model,
        "messages": messages,
        "stream": value.get("stream").and_then(|v| v.as_bool()).unwrap_or(false),
    });
    if let Some(max) = value.get("max_output_tokens") {
        result["max_tokens"] = max.clone();
    }
    if let Some(temp) = value.get("temperature") {
        result["temperature"] = temp.clone();
    }
    if result["stream"].as_bool() == Some(true) {
        result["stream_options"] = json!({"include_usage": true});
    }
    Ok(serde_json::to_vec(&result)?)
}

pub fn chat_to_responses_json(value: &Value) -> Value {
    let model = value
        .get("model")
        .cloned()
        .unwrap_or_else(|| json!("unknown"));
    let text = value
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    json!({
        "id": value.get("id").cloned().unwrap_or_else(|| json!(format!("resp_{}", uuid::Uuid::new_v4()))),
        "object": "response",
        "model": model,
        "status": "completed",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
            "status": "completed"
        }],
        "usage": normalize_chat_usage(value.get("usage"))
    })
}

fn input_item_to_chat_message(item: &Value) -> Option<Value> {
    let role = item
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("user");
    let content = match item.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .or_else(|| part.get("input_text"))
                    .and_then(|v| v.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => item
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    };
    Some(json!({"role": role, "content": content}))
}

fn usage_from_value(usage: &Value) -> TokenUsage {
    let input = int_field(usage, &["input_tokens", "prompt_tokens"]);
    let output = int_field(usage, &["output_tokens", "completion_tokens"]);
    let cache_read = usage
        .pointer("/input_tokens_details/cached_tokens")
        .or_else(|| usage.pointer("/prompt_tokens_details/cached_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or_else(|| int_field(usage, &["cache_read_input_tokens"]));
    let cache_creation = int_field(usage, &["cache_creation_input_tokens"]);
    let total = int_field(usage, &["total_tokens"]);
    let mut result = TokenUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_creation,
        total_tokens: total,
    };
    result.finish();
    result
}

fn normalize_chat_usage(usage: Option<&Value>) -> Value {
    let usage = usage.map(usage_from_value).unwrap_or_default();
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "total_tokens": usage.total_tokens,
        "input_tokens_details": {"cached_tokens": usage.cache_read_tokens}
    })
}

fn sse_text_delta(value: &Value) -> Option<&str> {
    value
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            value
                .pointer("/choices/0/delta/reasoning_content")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
        .or_else(|| {
            value
                .pointer("/choices/0/delta/refusal")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
        .or_else(|| {
            value
                .get("delta")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
}

fn int_field(value: &Value, keys: &[&str]) -> i64 {
    for key in keys {
        if let Some(number) = value.get(*key).and_then(Value::as_i64) {
            return number;
        }
    }
    0
}

fn format_reasoning_effort(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "xhigh" | "x-high" | "x_high" | "extra_high" | "extra-high" => "XHigh".to_string(),
        "high" => "High".to_string(),
        "medium" => "Medium".to_string(),
        "low" => "Low".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_usage_from_responses_json() {
        let usage = extract_usage_from_json(&json!({
            "usage":{"input_tokens":10,"output_tokens":3,"input_tokens_details":{"cached_tokens":2}}
        }));
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(usage.cache_read_tokens, 2);
    }

    #[test]
    fn extracts_usage_from_crlf_sse_nested_usage() {
        let usage = extract_usage_from_sse(concat!(
            "event: response.completed\r\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":4096,\"output_tokens\":1,\"input_tokens_details\":{\"cached_tokens\":2048}}}}\r\n",
            "\r\n"
        ));

        assert_eq!(usage.input_tokens, 4096);
        assert_eq!(usage.output_tokens, 1);
        assert_eq!(usage.cache_read_tokens, 2048);
    }

    #[test]
    fn converts_responses_to_chat() {
        let body = responses_to_chat_json(br#"{"model":"gpt","input":"hello","stream":true}"#)
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["messages"][0]["content"], "hello");
        assert_eq!(value["stream_options"]["include_usage"], true);
    }

    #[test]
    fn extracts_sse_text_deltas() {
        let mut out = String::new();
        for_each_sse_text_delta(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hel\"}\n\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            |delta| out.push_str(delta),
        );
        assert_eq!(out, "hello");
    }

    #[test]
    fn extracts_reasoning_effort() {
        assert_eq!(
            extract_reasoning_effort(br#"{"reasoning":{"effort":"xhigh"}}"#).as_deref(),
            Some("XHigh")
        );
        assert_eq!(
            extract_reasoning_effort(br#"{"reasoning_effort":"high"}"#).as_deref(),
            Some("High")
        );
    }
}

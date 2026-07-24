use crate::core::models::TokenUsage;
use serde_json::Value;

pub fn extract_model(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
}

pub fn extract_reasoning_effort(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
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

pub fn has_anthropic_usage_event(text: &str) -> bool {
    let text = text.replace("\r\n", "\n");
    text.split("\n\n").any(|block| {
        block.lines().any(|line| {
            line.strip_prefix("data:")
                .map(str::trim)
                .and_then(|data| serde_json::from_str::<Value>(data).ok())
                .is_some_and(|value| {
                    matches!(
                        value.get("type").and_then(Value::as_str),
                        Some("message_start" | "message_delta")
                    ) && (value.get("usage").is_some() || value.pointer("/message/usage").is_some())
                })
        })
    })
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
        "cache_creation_input_tokens",
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

fn usage_from_value(usage: &Value) -> TokenUsage {
    let raw_input = int_field(usage, &["input_tokens", "prompt_tokens"]);
    let output = int_field(usage, &["output_tokens", "completion_tokens"]);
    let cache_read = usage
        .pointer("/input_tokens_details/cached_tokens")
        .or_else(|| usage.pointer("/prompt_tokens_details/cached_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or_else(|| int_field(usage, &["cache_read_input_tokens"]));
    let cache_creation = int_field(usage, &["cache_creation_input_tokens"]);
    let anthropic_semantics = (usage.get("cache_read_input_tokens").is_some()
        || usage.get("cache_creation_input_tokens").is_some())
        && usage.get("input_tokens_details").is_none()
        && usage.get("prompt_tokens_details").is_none();
    let input = if anthropic_semantics {
        raw_input + cache_read + cache_creation
    } else {
        raw_input
    };
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(input + output);
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
        .or_else(|| {
            value
                .pointer("/delta/text")
                .or_else(|| value.pointer("/delta/thinking"))
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
    fn extracts_complete_anthropic_usage_event() {
        let usage = extract_usage_from_sse(concat!(
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"input_tokens\":4,\"output_tokens\":2,\"cache_read_input_tokens\":3,\"cache_creation_input_tokens\":1}}\n\n"
        ));

        assert_eq!(usage.input_tokens, 8);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(usage.cache_read_tokens, 3);
        assert_eq!(usage.cache_creation_tokens, 1);
        assert_eq!(usage.total_tokens, 10);
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

use crate::core::models::{TokenUsage, UpstreamCacheKeepaliveSettings, WireApi};
use serde_json::{Value, json};

pub(super) fn keepalive_body(
    body: &[u8],
    wire_api: WireApi,
    settings: &UpstreamCacheKeepaliveSettings,
) -> anyhow::Result<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(body)?;
    let use_extended_retention =
        settings.prefer_extended_retention && should_use_extended_retention(&value);
    let Some(obj) = value.as_object_mut() else {
        anyhow::bail!("request body is not a json object");
    };
    obj.insert("stream".to_string(), Value::Bool(false));
    obj.insert("store".to_string(), Value::Bool(false));
    match wire_api {
        WireApi::Responses => {
            obj.insert("max_output_tokens".to_string(), json!(1));
            if obj.contains_key("reasoning") {
                obj.insert("reasoning".to_string(), json!({"effort":"minimal"}));
            }
            if use_extended_retention {
                obj.insert("prompt_cache_retention".to_string(), json!("24h"));
            }
        }
        WireApi::ChatCompletions => {
            obj.insert("max_tokens".to_string(), json!(1));
            if obj.contains_key("reasoning_effort") {
                obj.insert("reasoning_effort".to_string(), json!("minimal"));
            }
        }
    }
    Ok(serde_json::to_vec(&value)?)
}

pub(super) fn normalized_keepalive_usage(mut usage: TokenUsage) -> TokenUsage {
    usage.finish();
    usage
}

fn should_use_extended_retention(value: &Value) -> bool {
    value
        .get("model")
        .and_then(Value::as_str)
        .map(model_supports_extended_retention)
        .unwrap_or(false)
}

fn model_supports_extended_retention(model: &str) -> bool {
    let model = model.trim();
    model.starts_with("gpt-5")
        || matches!(model, "gpt-4.1" | "openai/gpt-4.1")
        || model.starts_with("openai/gpt-5")
}

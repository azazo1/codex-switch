use serde_json::{Value, json};

pub(crate) fn normalize_responses_request(body: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(body)?;
    if let Some(input) = value.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            normalize_reasoning_item(item);
        }
    }
    Ok(serde_json::to_vec(&value)?)
}

fn normalize_reasoning_item(item: &mut Value) {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return;
    }
    let decoded_internal = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .and_then(super::decode_reasoning);
    let content_text = content_reasoning_text(item);
    let Some(object) = item.as_object_mut() else {
        return;
    };
    object.insert("content".to_string(), json!([]));
    if let Some(text) = decoded_internal.or(content_text) {
        object.insert(
            "summary".to_string(),
            json!([{"type":"summary_text","text":text}]),
        );
        object.remove("encrypted_content");
    }
}

fn content_reasoning_text(item: &Value) -> Option<String> {
    match item.get("content") {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let text = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<String>();
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_internal_reasoning_as_summary_text() {
        let body = json!({
            "model":"deepseek-v4-flash",
            "input":[
                {
                    "type":"reasoning",
                    "id":"rs_1",
                    "summary":[],
                    "encrypted_content":"codex-switch-reasoning-v1:aGVsbG8"
                },
                {
                    "type":"function_call",
                    "call_id":"call_1",
                    "name":"exec",
                    "arguments":"{}"
                }
            ]
        });

        let normalized = normalize_responses_request(&serde_json::to_vec(&body).unwrap()).unwrap();
        let value: Value = serde_json::from_slice(&normalized).unwrap();

        assert_eq!(
            value["input"][0]["summary"][0],
            json!({"type":"summary_text","text":"hello"})
        );
        assert_eq!(value["input"][0]["content"], json!([]));
        assert!(value["input"][0].get("encrypted_content").is_none());
    }

    #[test]
    fn keeps_summary_reasoning_with_empty_content() {
        let body = json!({
            "model":"responses-model",
            "input":[{
                "type":"reasoning",
                "id":"rs_1",
                "summary":[{"type":"summary_text","text":"short summary"}]
            }]
        });

        let normalized = normalize_responses_request(&serde_json::to_vec(&body).unwrap()).unwrap();
        let value: Value = serde_json::from_slice(&normalized).unwrap();

        assert_eq!(
            value["input"][0]["summary"][0],
            json!({"type":"summary_text","text":"short summary"})
        );
        assert_eq!(value["input"][0]["content"], json!([]));
    }

    #[test]
    fn moves_existing_reasoning_content_to_summary() {
        let body = json!({
            "model":"responses-model",
            "input":[{
                "type":"reasoning",
                "id":"rs_1",
                "summary":[],
                "content":[{"type":"reasoning_text","text":"think step by step"}]
            }]
        });

        let normalized = normalize_responses_request(&serde_json::to_vec(&body).unwrap()).unwrap();
        let value: Value = serde_json::from_slice(&normalized).unwrap();

        assert_eq!(
            value["input"][0]["summary"][0],
            json!({"type":"summary_text","text":"think step by step"})
        );
        assert_eq!(value["input"][0]["content"], json!([]));
    }
}

use serde_json::Value;

pub fn normalize_oauth_body(body: &[u8], compact: bool) -> anyhow::Result<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(body)?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("store".to_string(), Value::Bool(false));
        if compact {
            obj.remove("stream");
            obj.remove("prompt_cache_key");
            obj.remove("store");
        } else {
            obj.insert("stream".to_string(), Value::Bool(true));
        }
    }
    Ok(serde_json::to_vec(&value)?)
}

pub fn rewrite_model(body: &[u8], model: &str) -> anyhow::Result<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(body)?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("model".to_string(), Value::String(model.to_string()));
    }
    Ok(serde_json::to_vec(&value)?)
}

pub fn rewrite_response_model(body: &[u8], model: &str) -> anyhow::Result<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(body)?;
    rewrite_response_model_value(&mut value, model);
    Ok(serde_json::to_vec(&value)?)
}

fn rewrite_response_model_value(value: &mut Value, model: &str) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if let Some(current) = object.get_mut("model")
        && current.is_string()
    {
        *current = Value::String(model.to_string());
    }
    if let Some(response) = object.get_mut("response").and_then(Value::as_object_mut)
        && let Some(current) = response.get_mut("model")
        && current.is_string()
    {
        *current = Value::String(model.to_string());
    }
    if let Some(message) = object.get_mut("message").and_then(Value::as_object_mut)
        && let Some(current) = message.get_mut("model")
        && current.is_string()
    {
        *current = Value::String(model.to_string());
    }
}

pub fn responses_subpath_from_uri(path: &str) -> String {
    for marker in [
        "/v1/responses",
        "/responses",
        "/backend-api/codex/responses",
    ] {
        if let Some(rest) = path.strip_prefix(marker) {
            return rest.trim_end_matches('/').to_string();
        }
    }
    String::new()
}

pub fn images_subpath_from_uri(path: &str) -> String {
    for marker in ["/v1/images", "/images"] {
        if let Some(rest) = path.strip_prefix(marker) {
            return rest.trim_end_matches('/').to_string();
        }
    }
    String::new()
}

pub fn build_endpoint(base_url: &str, endpoint: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let endpoint = endpoint.trim_start_matches('/');
    if has_api_version_suffix(base) || has_api_version_prefix(endpoint) {
        format!("{base}/{endpoint}")
    } else {
        format!("{base}/v1/{endpoint}")
    }
}

pub fn canonicalize_incoming_path(path: &str) -> Option<String> {
    let mut canonical = path.to_string();
    let mut changed = false;

    if let Some((version, rest)) = split_leading_api_version(&canonical)
        && version != "v1"
    {
        canonical = format!("/v1{rest}");
        changed = true;
    }

    if let Some(rewritten) = rewrite_chat_completion_alias(&canonical) {
        canonical = rewritten;
        changed = true;
    }

    changed.then_some(canonical)
}

fn is_api_version_segment(segment: &str) -> bool {
    let Some(digits) = segment.strip_prefix('v') else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

fn has_api_version_suffix(path: &str) -> bool {
    path.rsplit('/').next().is_some_and(is_api_version_segment)
}

fn has_api_version_prefix(path: &str) -> bool {
    path.split('/').next().is_some_and(is_api_version_segment)
}

fn split_leading_api_version(path: &str) -> Option<(&str, &str)> {
    let trimmed = path.strip_prefix('/')?;
    match trimmed.split_once('/') {
        Some((segment, _)) if is_api_version_segment(segment) => {
            Some((segment, path.get(segment.len() + 1..)?))
        }
        None if is_api_version_segment(trimmed) => Some((trimmed, "")),
        _ => None,
    }
}

fn rewrite_chat_completion_alias(path: &str) -> Option<String> {
    if path == "/chat/completion" {
        return Some("/chat/completions".to_string());
    }
    let (version, rest) = split_leading_api_version(path)?;
    (rest == "/chat/completion").then(|| format!("/{version}/chat/completions"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_compact_subpath() {
        assert_eq!(
            responses_subpath_from_uri("/v1/responses/compact"),
            "/compact"
        );
        assert_eq!(
            responses_subpath_from_uri("/backend-api/codex/responses/compact/detail"),
            "/compact/detail"
        );
    }

    #[test]
    fn preserves_images_subpath() {
        assert_eq!(
            images_subpath_from_uri("/v1/images/generations"),
            "/generations"
        );
        assert_eq!(images_subpath_from_uri("/images/edits"), "/edits");
    }

    #[test]
    fn oauth_compact_removes_unsupported_fields() {
        let body = normalize_oauth_body(
            br#"{"model":"gpt","stream":true,"store":true,"prompt_cache_key":"x"}"#,
            true,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(value.get("stream").is_none());
        assert!(value.get("store").is_none());
        assert!(value.get("prompt_cache_key").is_none());
    }

    #[test]
    fn rewrites_root_model() {
        let body = rewrite_model(
            br#"{"model":"client-model","input":"hello"}"#,
            "target-model",
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["model"], "target-model");
    }

    #[test]
    fn rewrites_response_models() {
        let body = br#"{"type":"response.completed","model":"deepseek-v4-flash","response":{"id":"r1","model":"deepseek-v4-flash","output":[]},"message":{"id":"m1","model":"deepseek-v4-flash"}}"#;
        let rewritten = rewrite_response_model(body, "gpt-5.4").unwrap();
        let value: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(value["model"], "gpt-5.4");
        assert_eq!(value["response"]["model"], "gpt-5.4");
        assert_eq!(value["message"]["model"], "gpt-5.4");
    }

    #[test]
    fn build_endpoint_appends_v1_when_base_has_no_version() {
        assert_eq!(
            build_endpoint("https://api.example.com", "chat/completions"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            build_endpoint("https://api.example.com/", "/models"),
            "https://api.example.com/v1/models"
        );
    }

    #[test]
    fn build_endpoint_keeps_existing_api_version() {
        assert_eq!(
            build_endpoint("https://api.example.com/v1", "/chat/completions"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            build_endpoint("https://api.example.com/v4/", "chat/completions"),
            "https://api.example.com/v4/chat/completions"
        );
        assert_eq!(
            build_endpoint("https://api.example.com/openai/v4", "chat/completions"),
            "https://api.example.com/openai/v4/chat/completions"
        );
        assert_eq!(
            build_endpoint("https://api.example.com", "v4/chat/completions"),
            "https://api.example.com/v4/chat/completions"
        );
    }

    #[test]
    fn canonicalize_incoming_path_rewrites_non_v1_and_completion_alias() {
        assert_eq!(
            canonicalize_incoming_path("/v4/chat/completion"),
            Some("/v1/chat/completions".to_string())
        );
        assert_eq!(
            canonicalize_incoming_path("/v4/chat/completions"),
            Some("/v1/chat/completions".to_string())
        );
        assert_eq!(
            canonicalize_incoming_path("/chat/completion"),
            Some("/chat/completions".to_string())
        );
        assert_eq!(canonicalize_incoming_path("/v1/chat/completions"), None);
        assert_eq!(
            canonicalize_incoming_path("/v1/chat/completion"),
            Some("/v1/chat/completions".to_string())
        );
        assert_eq!(canonicalize_incoming_path("/health"), None);
        assert_eq!(
            canonicalize_incoming_path("/backend-api/codex/responses"),
            None
        );
    }
}

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

pub fn responses_subpath_from_uri(path: &str) -> String {
    for marker in ["/v1/responses", "/responses", "/backend-api/codex/responses"] {
        if let Some(rest) = path.strip_prefix(marker) {
            return rest.trim_end_matches('/').to_string();
        }
    }
    String::new()
}

pub fn build_endpoint(base_url: &str, endpoint: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let endpoint = endpoint.trim_start_matches('/');
    if base.ends_with("/v1") || endpoint.starts_with("v1/") {
        format!("{base}/{endpoint}")
    } else {
        format!("{base}/v1/{endpoint}")
    }
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
}

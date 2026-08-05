use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct ModelCapabilityCache {
    per_upstream: Arc<Mutex<HashMap<(String, String), bool>>>,
    global: Arc<Mutex<HashMap<String, bool>>>,
}

impl ModelCapabilityCache {
    pub fn get(&self, upstream_id: &str, model: &str) -> Option<bool> {
        let key = (upstream_id.to_string(), model.to_string());
        if let Some(value) = self
            .per_upstream
            .lock()
            .ok()
            .and_then(|cache| cache.get(&key).copied())
        {
            return Some(value);
        }
        let key = model.to_string();
        self.global
            .lock()
            .ok()
            .and_then(|cache| cache.get(&key).copied())
    }

    pub fn extend(
        &self,
        upstream_id: &str,
        entries: impl IntoIterator<Item = (String, bool)>,
    ) {
        if let Ok(mut cache) = self.per_upstream.lock() {
            for (model, multimodal) in entries {
                cache.insert((upstream_id.to_string(), model), multimodal);
            }
        }
    }

    pub fn extend_global(&self, entries: impl IntoIterator<Item = (String, bool)>) {
        if let Ok(mut cache) = self.global.lock() {
            for (model, multimodal) in entries {
                cache.insert(model, multimodal);
            }
        }
    }
}

pub fn model_multimodal_from_item(item: &Value) -> Option<bool> {
    for key in ["multimodal", "supports_image_input", "supports_vision", "vision"] {
        if let Some(value) = item.get(key).and_then(Value::as_bool) {
            return Some(value);
        }
    }
    if let Some(capabilities) = item.get("capabilities") {
        for key in [
            "image_input",
            "supports_image_input",
            "vision",
            "supports_vision",
            "multimodal",
            "images",
        ] {
            if let Some(value) = capabilities.get(key).and_then(Value::as_bool) {
                return Some(value);
            }
        }
    }
    if let Some(modalities) = item.get("modalities")
        && let Some(input) = modalities.get("input")
        && let Some(result) = modalities_bool(input)
    {
        return Some(result);
    }
    for key in ["modalities", "input_modalities"] {
        if let Some(value) = item.get(key)
            && let Some(result) = modalities_bool(value)
        {
            return Some(result);
        }
    }
    if let Some(modality) = item
        .pointer("/architecture/modality")
        .and_then(Value::as_str)
    {
        let lower = modality.to_ascii_lowercase();
        return Some(
            lower.contains("image")
                || lower.contains("audio")
                || lower.contains("video")
                || lower.contains("file"),
        );
    }
    None
}

fn modalities_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Array(items) => {
            let values = items.iter().filter_map(Value::as_str).collect::<Vec<_>>();
            if values.is_empty() {
                return None;
            }
            Some(values.iter().any(|value| {
                let value = value.to_ascii_lowercase();
                value == "image"
                    || value == "audio"
                    || value == "video"
                    || value == "file"
                    || value.contains("image")
                    || value.contains("audio")
            }))
        }
        Value::Object(object) => {
            let mut has_known = false;
            let mut has_media = false;
            for key in ["text", "image", "audio", "video", "file"] {
                if let Some(value) = object.get(key).and_then(Value::as_bool) {
                    has_known = true;
                    if key != "text" && value {
                        has_media = true;
                    }
                }
            }
            has_known.then_some(has_media)
        }
        Value::String(value) => {
            let value = value.to_ascii_lowercase();
            Some(
                value.contains("image")
                    || value.contains("audio")
                    || value.contains("video")
                    || value.contains("file"),
            )
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn session_cache_round_trips() {
        let cache = ModelCapabilityCache::default();
        assert_eq!(cache.get("upstream", "deepseek-v4-flash"), None);
        cache.extend("upstream", vec![("deepseek-v4-flash".to_string(), false)]);
        assert_eq!(
            cache.get("upstream", "deepseek-v4-flash"),
            Some(false)
        );
        assert_eq!(cache.get("other", "deepseek-v4-flash"), None);
    }

    #[test]
    fn global_cache_falls_back_for_any_upstream() {
        let cache = ModelCapabilityCache::default();
        cache.extend_global(vec![("gpt-5.2".to_string(), true)]);
        assert_eq!(cache.get("any-upstream", "gpt-5.2"), Some(true));
        assert_eq!(cache.get("any-upstream", "deepseek-v4-flash"), None);
    }

    #[test]
    fn parses_models_dev_modalities() {
        assert_eq!(
            model_multimodal_from_item(&json!({
                "id":"gpt-5.2",
                "modalities":{"input":["text","image"],"output":["text"]}
            })),
            Some(true)
        );
        assert_eq!(
            model_multimodal_from_item(&json!({
                "id":"deepseek-v4-flash",
                "modalities":{"input":["text"],"output":["text"]}
            })),
            Some(false)
        );
        assert_eq!(
            model_multimodal_from_item(&json!({"id":"gpt-5.2","architecture":{"modality":"text->text"}})),
            Some(false)
        );
    }
}

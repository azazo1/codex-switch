use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct ModelCapabilityCache {
    inner: Arc<Mutex<HashMap<(String, String), bool>>>,
}

impl ModelCapabilityCache {
    pub fn get(&self, upstream_id: &str, model: &str) -> Option<bool> {
        let key = (upstream_id.to_string(), model.to_string());
        self.inner
            .lock()
            .ok()
            .and_then(|cache| cache.get(&key).copied())
    }

    pub fn extend(
        &self,
        upstream_id: &str,
        entries: impl IntoIterator<Item = (String, bool)>,
    ) {
        if let Ok(mut cache) = self.inner.lock() {
            for (model, multimodal) in entries {
                cache.insert((upstream_id.to_string(), model), multimodal);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

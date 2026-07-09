use super::session::CacheKeepaliveSession;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub(super) fn session_key(
    upstream_id: &str,
    model: &str,
    endpoint: &str,
    body: &[u8],
) -> Option<String> {
    let value = serde_json::from_slice::<Value>(body).ok()?;
    let raw_session = find_string(&value, "prompt_cache_key")
        .or_else(|| find_string(&value, "conversation_id"))
        .or_else(|| find_string(&value, "session_id"))?;
    let mut hasher = Sha256::new();
    hasher.update(upstream_id.as_bytes());
    hasher.update(model.as_bytes());
    hasher.update(endpoint.as_bytes());
    hasher.update(raw_session.as_bytes());
    hasher.update(cacheable_prefix_fingerprint(&value).as_bytes());
    Some(format!("{:x}", hasher.finalize()))
}

fn cacheable_prefix_fingerprint(value: &Value) -> String {
    let prefix = match value {
        Value::Object(map) => {
            let mut prefix = serde_json::Map::new();
            for key in [
                "instructions",
                "messages",
                "tools",
                "text",
                "response_format",
            ] {
                if let Some(value) = map.get(key) {
                    prefix.insert(key.to_string(), value.clone());
                }
            }
            Value::Object(prefix)
        }
        _ => value.clone(),
    };
    serde_json::to_string(&prefix).unwrap_or_default()
}

fn find_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    match value {
        Value::Object(map) => {
            if let Some(found) = map.get(key).and_then(Value::as_str) {
                return Some(found);
            }
            map.values().find_map(|value| find_string(value, key))
        }
        Value::Array(values) => values.iter().find_map(|value| find_string(value, key)),
        _ => None,
    }
}

pub(super) fn trimmed_string(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

pub(super) fn short_hash(value: &str) -> String {
    value.chars().take(12).collect()
}

pub(super) fn prune_upstream_sessions(
    sessions: &mut HashMap<String, CacheKeepaliveSession>,
    upstream_id: &str,
    max_active_sessions: i64,
) {
    let limit = max_active_sessions.max(1) as usize;
    let mut keys = sessions
        .values()
        .filter(|session| session.upstream.id == upstream_id && session.disabled_reason.is_none())
        .map(|session| (session.key.clone(), session.last_activity_at))
        .collect::<Vec<_>>();
    if keys.len() <= limit {
        return;
    }
    keys.sort_by_key(|(_, last_activity_at)| *last_activity_at);
    let remove_count = keys.len().saturating_sub(limit);
    for (key, _) in keys.into_iter().take(remove_count) {
        sessions.remove(&key);
    }
}

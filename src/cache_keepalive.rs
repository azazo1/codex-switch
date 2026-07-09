use crate::balance::API_KEY_CREDENTIAL;
use crate::core::models::{
    CacheKeepaliveMode, RequestLog, TokenUsage, Upstream, UpstreamCacheKeepaliveSettings,
    UpstreamKind, WireApi,
};
use crate::pricing;
use crate::storage::{Store, credentials::CredentialStore};
use crate::{proxy::transform, usage};
use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const SCAN_INTERVAL: Duration = Duration::from_secs(30);
const INTERNAL_ENDPOINT: &str = "/internal/cache_keepalive";
const MAX_REASONABLE_OUTPUT_TOKENS: i64 = 8;

#[derive(Clone)]
pub struct CacheKeepaliveRuntime {
    inner: Arc<Mutex<CacheKeepaliveInner>>,
    store: Store,
    credentials: CredentialStore,
    http: reqwest::Client,
}

#[derive(Default)]
struct CacheKeepaliveInner {
    sessions: HashMap<String, CacheKeepaliveSession>,
}

#[derive(Clone)]
struct CacheKeepaliveSession {
    key: String,
    upstream: Upstream,
    endpoint: String,
    model: String,
    body: Vec<u8>,
    wire_api: WireApi,
    cached_tokens: i64,
    keepalive_count: i64,
    last_user_request_at: Instant,
    last_activity_at: Instant,
    next_keepalive_at: Instant,
    disabled_reason: Option<String>,
}

pub struct CacheKeepaliveRegistration {
    pub upstream: Upstream,
    pub endpoint: String,
    pub model: Option<String>,
    pub body: Vec<u8>,
    pub usage: TokenUsage,
}

impl CacheKeepaliveRuntime {
    pub fn new(store: Store, credentials: CredentialStore, http: reqwest::Client) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CacheKeepaliveInner::default())),
            store,
            credentials,
            http,
        }
    }

    pub fn start(&self) {
        let runtime = self.clone();
        tokio::spawn(async move {
            runtime.run().await;
        });
    }

    pub async fn register(&self, registration: CacheKeepaliveRegistration) {
        if registration.upstream.kind != UpstreamKind::RelayApiKey {
            return;
        }
        let Some(model) = registration.model.as_deref().and_then(trimmed_string) else {
            return;
        };
        let Ok(settings) = self
            .store
            .cache_keepalive_settings(&registration.upstream.id)
            .await
        else {
            tracing::debug!(
                upstream_id = %registration.upstream.id,
                "failed to load cache keepalive settings"
            );
            return;
        };
        if !settings.is_active()
            || registration.usage.cache_read_tokens < settings.min_cacheable_tokens.max(1024)
        {
            return;
        }
        let Some(session_key) = session_key(
            &registration.upstream.id,
            &model,
            &registration.endpoint,
            &registration.body,
        ) else {
            return;
        };
        let now = Instant::now();
        let interval = Duration::from_secs(settings.interval_seconds.max(60) as u64);
        let wire_api = registration.upstream.wire_api;
        let session = CacheKeepaliveSession {
            key: session_key.clone(),
            upstream: registration.upstream,
            endpoint: registration.endpoint,
            model,
            body: registration.body,
            wire_api,
            cached_tokens: registration.usage.cache_read_tokens,
            keepalive_count: 0,
            last_user_request_at: now,
            last_activity_at: now,
            next_keepalive_at: now + interval,
            disabled_reason: None,
        };
        let mut inner = self.inner.lock().await;
        inner.sessions.insert(session_key, session);
        prune_upstream_sessions(&mut inner.sessions, &settings.upstream_id, settings.max_active_sessions);
    }

    async fn run(self) {
        let mut ticker = tokio::time::interval(SCAN_INTERVAL);
        loop {
            ticker.tick().await;
            self.scan_once().await;
        }
    }

    async fn scan_once(&self) {
        let due_sessions = self.due_sessions().await;
        for session in due_sessions {
            if let Err(err) = self.keepalive_once(session).await {
                tracing::debug!(error = %err, "cache keepalive skipped");
            }
        }
    }

    async fn due_sessions(&self) -> Vec<CacheKeepaliveSession> {
        let now = Instant::now();
        let inner = self.inner.lock().await;
        inner
            .sessions
            .values()
            .filter(|session| session.disabled_reason.is_none() && session.next_keepalive_at <= now)
            .cloned()
            .collect()
    }

    async fn keepalive_once(&self, session: CacheKeepaliveSession) -> anyhow::Result<()> {
        let settings = self.store.cache_keepalive_settings(&session.upstream.id).await?;
        if !settings.is_active() {
            self.disable_session(&session.key, "settings disabled").await;
            return Ok(());
        }
        let idle = session.last_user_request_at.elapsed();
        if idle > Duration::from_secs(settings.max_idle_seconds.max(60) as u64) {
            self.disable_session(&session.key, "max idle exceeded").await;
            return Ok(());
        }
        if session.cached_tokens < settings.min_cacheable_tokens.max(1024) {
            self.disable_session(&session.key, "not enough cached tokens").await;
            return Ok(());
        }
        let Some(price) = self.store.find_model_price(&session.model).await? else {
            if settings.mode == CacheKeepaliveMode::Smart {
                self.disable_session(&session.key, "missing model price").await;
            } else {
                self.schedule_next(&session.key, &settings, None).await;
            }
            return Ok(());
        };
        if settings.mode == CacheKeepaliveMode::Smart
            && !pricing::should_keepalive_cache(
                session.cached_tokens,
                session.keepalive_count,
                &price,
            )
        {
            self.disable_session(&session.key, "smart cost rejected").await;
            return Ok(());
        }
        let started = Instant::now();
        let result = self.send_keepalive(&session, &settings).await;
        match result {
            Ok(usage) => {
                let usage = normalized_keepalive_usage(usage);
                let status = if usage.output_tokens > MAX_REASONABLE_OUTPUT_TOKENS {
                    self.disable_session(&session.key, "unexpected output tokens").await;
                    StatusCode::OK
                } else if usage.cache_read_tokens < settings.min_cacheable_tokens.max(1024) {
                    self.disable_session(&session.key, "cache miss").await;
                    StatusCode::OK
                } else {
                    self.schedule_next(&session.key, &settings, Some(usage.cache_read_tokens))
                        .await;
                    StatusCode::OK
                };
                self.record_log(&session, status, usage, started, None).await;
            }
            Err(err) => {
                self.disable_session(&session.key, "request failed").await;
                self.record_log(
                    &session,
                    StatusCode::BAD_GATEWAY,
                    TokenUsage::default(),
                    started,
                    Some(err.to_string()),
                )
                .await;
            }
        }
        Ok(())
    }

    async fn send_keepalive(
        &self,
        session: &CacheKeepaliveSession,
        settings: &UpstreamCacheKeepaliveSettings,
    ) -> anyhow::Result<TokenUsage> {
        let target_body = keepalive_body(&session.body, session.wire_api, settings)?;
        let target_url = match session.wire_api {
            WireApi::Responses => transform::build_endpoint(&session.upstream.base_url, &session.endpoint),
            WireApi::ChatCompletions => transform::build_endpoint(&session.upstream.base_url, "/chat/completions"),
        };
        let api_key = self
            .credentials
            .get(&session.upstream.id, API_KEY_CREDENTIAL)
            .await?
            .ok_or_else(|| anyhow::anyhow!("missing api key"))?;
        let response = self
            .http
            .post(target_url)
            .bearer_auth(api_key)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(target_body)
            .send()
            .await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            anyhow::bail!("cache keepalive upstream status {status}: {}", String::from_utf8_lossy(&bytes));
        }
        let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        let mut usage = usage::extract_usage_from_json(&value);
        usage.finish();
        Ok(usage)
    }

    async fn schedule_next(&self, key: &str, settings: &UpstreamCacheKeepaliveSettings, cached_tokens: Option<i64>) {
        let now = Instant::now();
        let interval = Duration::from_secs(settings.interval_seconds.max(60) as u64);
        let mut inner = self.inner.lock().await;
        if let Some(session) = inner.sessions.get_mut(key) {
            session.keepalive_count += 1;
            session.last_activity_at = now;
            session.next_keepalive_at = now + interval;
            if let Some(cached_tokens) = cached_tokens {
                session.cached_tokens = cached_tokens;
            }
        }
    }

    async fn disable_session(&self, key: &str, reason: &str) {
        let mut inner = self.inner.lock().await;
        if let Some(session) = inner.sessions.get_mut(key) {
            session.disabled_reason = Some(reason.to_string());
            tracing::info!(
                upstream_id = %session.upstream.id,
                model = %session.model,
                reason,
                "cache keepalive session disabled"
            );
        }
    }

    async fn record_log(
        &self,
        session: &CacheKeepaliveSession,
        status: StatusCode,
        usage: TokenUsage,
        started: Instant,
        error: Option<String>,
    ) {
        let log = RequestLog {
            ts: None,
            upstream_id: Some(session.upstream.id.clone()),
            upstream_name: Some(session.upstream.name.clone()),
            endpoint: INTERNAL_ENDPOINT.to_string(),
            model: Some(session.model.clone()),
            reasoning_effort: None,
            status: i64::from(status.as_u16()),
            usage,
            estimated_cost_usd: None,
            duration_ms: started.elapsed().as_millis() as i64,
            first_token_ms: None,
            error,
        };
        if let Err(err) = self.store.insert_request_log(log).await {
            tracing::warn!(error = %err, "failed to record cache keepalive log");
        }
    }
}

fn keepalive_body(
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

fn normalized_keepalive_usage(mut usage: TokenUsage) -> TokenUsage {
    usage.finish();
    usage
}

fn session_key(upstream_id: &str, model: &str, endpoint: &str, body: &[u8]) -> Option<String> {
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
            for key in ["instructions", "messages", "tools", "text", "response_format"] {
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

fn trimmed_string(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn prune_upstream_sessions(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keepalive_body_limits_responses_output() {
        let settings = UpstreamCacheKeepaliveSettings {
            prefer_extended_retention: true,
            ..UpstreamCacheKeepaliveSettings::new("upstream".to_string())
        };
        let body = keepalive_body(
            br#"{"model":"gpt-5","input":"hello","stream":true,"store":true}"#,
            WireApi::Responses,
            &settings,
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value["stream"], false);
        assert_eq!(value["store"], false);
        assert_eq!(value["max_output_tokens"], 1);
        assert_eq!(value["prompt_cache_retention"], "24h");
    }

    #[test]
    fn session_key_uses_upstream_and_cache_key() {
        let body = br#"{"model":"gpt-test","prompt_cache_key":"stable","input":"hello"}"#;
        let first = session_key("a", "gpt-test", "/responses", body).unwrap();
        let second = session_key("b", "gpt-test", "/responses", body).unwrap();

        assert_ne!(first, second);
    }
}

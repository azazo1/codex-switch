use super::body::{keepalive_body, normalized_keepalive_usage};
use super::key::{prune_upstream_sessions, session_key, short_hash, trimmed_string};
use super::session::{
    CacheKeepaliveInner, CacheKeepaliveRegistration, CacheKeepaliveSession,
    CacheKeepaliveSessionSnapshot,
};
use super::{
    DISABLED_SESSION_RETENTION, INTERNAL_ENDPOINT, KEEPALIVE_REQUEST_TIMEOUT,
    OUTPUT_TOKENS_WARNING_THRESHOLD, SCAN_INTERVAL,
};
use crate::app::{AppEvents, http};
use crate::balance::API_KEY_CREDENTIAL;
use crate::core::models::{
    CacheKeepaliveMode, RequestLog, TokenUsage, UpstreamCacheKeepaliveSettings, UpstreamKind,
    WireApi,
};
use crate::pricing;
use crate::storage::{Store, credentials::CredentialStore};
use crate::{proxy::{transform, upstream_auth}, usage};
use reqwest::StatusCode;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct CacheKeepaliveRuntime {
    pub(super) inner: Arc<Mutex<CacheKeepaliveInner>>,
    pub(super) store: Store,
    credentials: CredentialStore,
    http: reqwest::Client,
    events: AppEvents,
}

impl CacheKeepaliveRuntime {
    pub fn new(
        store: Store,
        credentials: CredentialStore,
        http: reqwest::Client,
        events: AppEvents,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CacheKeepaliveInner::default())),
            store,
            credentials,
            http,
            events,
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
            tracing::trace!(
                upstream_id = %registration.upstream.id,
                upstream_name = %registration.upstream.name,
                endpoint = %registration.endpoint,
                "cache keepalive session skipped: upstream is not relay api key"
            );
            return;
        }
        let Some(model) = registration.model.as_deref().and_then(trimmed_string) else {
            tracing::warn!(
                upstream_id = %registration.upstream.id,
                upstream_name = %registration.upstream.name,
                endpoint = %registration.endpoint,
                "cache keepalive session skipped: missing model"
            );
            return;
        };
        let Ok(settings) = self
            .store
            .cache_keepalive_settings(&registration.upstream.id)
            .await
        else {
            tracing::warn!(
                upstream_id = %registration.upstream.id,
                "failed to load cache keepalive settings"
            );
            return;
        };
        tracing::debug!(
            upstream_id = %registration.upstream.id,
            endpoint = %registration.endpoint,
            model = %model,
            enabled = settings.enabled,
            mode = settings.mode.as_str(),
            interval_seconds = settings.interval_seconds,
            max_idle_seconds = settings.max_idle_seconds,
            min_cacheable_tokens = settings.min_cacheable_tokens,
            max_cacheable_tokens = settings.max_cacheable_tokens,
            max_active_sessions = settings.max_active_sessions,
            prefer_extended_retention = settings.prefer_extended_retention,
            "cache keepalive settings loaded"
        );
        if !settings.is_active() {
            tracing::debug!(
                upstream_id = %registration.upstream.id,
                upstream_name = %registration.upstream.name,
                endpoint = %registration.endpoint,
                model = %model,
                enabled = settings.enabled,
                mode = settings.mode.as_str(),
                "cache keepalive session skipped: settings inactive"
            );
            return;
        }
        let min_cacheable_tokens = settings.min_cacheable_tokens.max(1024);
        if registration.usage.cache_read_tokens < min_cacheable_tokens {
            tracing::debug!(
                upstream_id = %registration.upstream.id,
                upstream_name = %registration.upstream.name,
                endpoint = %registration.endpoint,
                model = %model,
                cache_read_tokens = registration.usage.cache_read_tokens,
                min_cacheable_tokens,
                "cache keepalive session skipped: not enough cached tokens"
            );
            return;
        }
        let max_cacheable_tokens = settings.max_cacheable_tokens.max(min_cacheable_tokens);
        if registration.usage.cache_read_tokens > max_cacheable_tokens {
            tracing::debug!(
                upstream_id = %registration.upstream.id,
                upstream_name = %registration.upstream.name,
                endpoint = %registration.endpoint,
                model = %model,
                cache_read_tokens = registration.usage.cache_read_tokens,
                max_cacheable_tokens,
                "cache keepalive session skipped: too many cached tokens"
            );
            return;
        }
        let Some(session_key) = session_key(
            &registration.upstream.id,
            &model,
            &registration.endpoint,
            &registration.body,
        ) else {
            tracing::warn!(
                upstream_id = %registration.upstream.id,
                upstream_name = %registration.upstream.name,
                endpoint = %registration.endpoint,
                model = %model,
                cache_read_tokens = registration.usage.cache_read_tokens,
                body_bytes = registration.body.len(),
                "cache keepalive session skipped: missing session key"
            );
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
            disabled_at: None,
        };
        let mut inner = self.inner.lock().await;
        tracing::info!(
            upstream_id = %settings.upstream_id,
            endpoint = %session.endpoint,
            model = %session.model,
            session_key_prefix = %short_hash(&session.key),
            cache_read_tokens = session.cached_tokens,
            body_bytes = session.body.len(),
            keepalive_interval_seconds = settings.interval_seconds.max(60),
            "cache keepalive session registered"
        );
        inner.sessions.insert(session_key, session);
        prune_upstream_sessions(
            &mut inner.sessions,
            &settings.upstream_id,
            settings.max_active_sessions,
        );
        self.events.bump_cache_keepalive();
    }

    pub async fn snapshots(&self) -> Vec<CacheKeepaliveSessionSnapshot> {
        let now = Instant::now();
        let inner = self.inner.lock().await;
        let mut snapshots = inner
            .sessions
            .values()
            .map(|session| session.snapshot(now))
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| {
            left.upstream_name
                .cmp(&right.upstream_name)
                .then_with(|| left.model.cmp(&right.model))
                .then_with(|| left.key.cmp(&right.key))
        });
        snapshots
    }

    pub async fn remove_session(&self, key: &str) -> bool {
        let mut inner = self.inner.lock().await;
        let removed = inner.sessions.remove(key);
        let Some(session) = removed else {
            return false;
        };
        tracing::info!(
            upstream_id = %session.upstream.id,
            endpoint = %session.endpoint,
            model = %session.model,
            session_key_prefix = %short_hash(&session.key),
            "cache keepalive session removed"
        );
        self.events.bump_cache_keepalive();
        true
    }

    pub async fn disable_upstream_sessions(&self, upstream_id: &str, reason: &str) -> usize {
        let mut inner = self.inner.lock().await;
        let mut disabled = 0;
        let now = Instant::now();
        for session in inner.sessions.values_mut() {
            if session.upstream.id != upstream_id || session.disabled_reason.is_some() {
                continue;
            }
            session.disabled_reason = Some(reason.to_string());
            session.disabled_at = Some(now);
            disabled += 1;
            tracing::info!(
                upstream_id = %session.upstream.id,
                model = %session.model,
                reason,
                "cache keepalive session disabled"
            );
        }
        if disabled > 0 {
            self.events.bump_cache_keepalive();
            self.schedule_disabled_prune();
        }
        disabled
    }

    fn schedule_disabled_prune(&self) {
        let runtime = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(DISABLED_SESSION_RETENTION).await;
            runtime.prune_disabled_sessions().await;
        });
    }

    async fn run(self) {
        let mut ticker = tokio::time::interval(SCAN_INTERVAL);
        loop {
            ticker.tick().await;
            self.scan_once().await;
        }
    }

    async fn scan_once(&self) {
        self.prune_disabled_sessions().await;
        let due_sessions = self.due_sessions().await;
        if !due_sessions.is_empty() {
            tracing::info!(
                due_sessions = due_sessions.len(),
                "cache keepalive scan found due sessions"
            );
        }
        for session in due_sessions {
            if let Err(err) = self.keepalive_once(session).await {
                tracing::warn!(error = %err, "cache keepalive skipped");
            }
        }
    }

    pub(super) async fn prune_disabled_sessions(&self) {
        let now = Instant::now();
        let mut inner = self.inner.lock().await;
        let before = inner.sessions.len();
        inner.sessions.retain(|_, session| {
            let Some(disabled_at) = session.disabled_at else {
                return true;
            };
            now.duration_since(disabled_at) < DISABLED_SESSION_RETENTION
        });
        let removed = before.saturating_sub(inner.sessions.len());
        if removed > 0 {
            tracing::debug!(removed, "cache keepalive disabled sessions pruned");
            self.events.bump_cache_keepalive();
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
        let settings = self
            .store
            .cache_keepalive_settings(&session.upstream.id)
            .await?;
        if !settings.is_active() {
            self.disable_session(&session.key, "settings disabled")
                .await;
            return Ok(());
        }
        let idle = session.last_user_request_at.elapsed();
        if idle > Duration::from_secs(settings.max_idle_seconds.max(60) as u64) {
            self.disable_session(&session.key, "max idle exceeded")
                .await;
            return Ok(());
        }
        if session.cached_tokens < settings.min_cacheable_tokens.max(1024) {
            self.disable_session(&session.key, "not enough cached tokens")
                .await;
            return Ok(());
        }
        let max_cacheable_tokens = settings
            .max_cacheable_tokens
            .max(settings.min_cacheable_tokens.max(1024));
        if session.cached_tokens > max_cacheable_tokens {
            self.disable_session(&session.key, "too many cached tokens")
                .await;
            return Ok(());
        }
        let Some(price) = self.store.find_model_price(&session.model).await? else {
            if settings.mode == CacheKeepaliveMode::Smart {
                self.disable_session(&session.key, "missing model price")
                    .await;
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
            self.disable_session(&session.key, "smart cost rejected")
                .await;
            return Ok(());
        }
        let started = Instant::now();
        tracing::debug!(
            upstream_id = %session.upstream.id,
            endpoint = %session.endpoint,
            model = %session.model,
            session_key_prefix = %short_hash(&session.key),
            keepalive_count = session.keepalive_count,
            cached_tokens = session.cached_tokens,
            "cache keepalive request starting"
        );
        let result = self.send_keepalive(&session, &settings).await;
        match result {
            Ok(usage) => {
                let usage = normalized_keepalive_usage(usage);
                if usage.output_tokens > OUTPUT_TOKENS_WARNING_THRESHOLD {
                    tracing::warn!(
                        upstream_id = %session.upstream.id,
                        endpoint = %session.endpoint,
                        model = %session.model,
                        session_key_prefix = %short_hash(&session.key),
                        output_tokens = usage.output_tokens,
                        warning_threshold = OUTPUT_TOKENS_WARNING_THRESHOLD,
                        "cache keepalive response used more output tokens than expected"
                    );
                }
                let status = if usage.cache_read_tokens < settings.min_cacheable_tokens.max(1024) {
                    self.disable_session(&session.key, "cache miss").await;
                    StatusCode::OK
                } else {
                    self.schedule_next(&session.key, &settings, Some(usage.cache_read_tokens))
                        .await;
                    StatusCode::OK
                };
                self.record_log(&session, status, usage, started, None)
                    .await;
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
            WireApi::Responses => {
                transform::build_endpoint(&session.upstream.base_url, &session.endpoint)
            }
            WireApi::ChatCompletions => {
                transform::build_endpoint(&session.upstream.base_url, "/chat/completions")
            }
            WireApi::AnthropicMessages => {
                transform::build_endpoint(&session.upstream.base_url, "/messages")
            }
        };
        tracing::debug!(
            upstream_id = %session.upstream.id,
            endpoint = %session.endpoint,
            model = %session.model,
            target_url = %target_url,
            body_bytes = target_body.len(),
            timeout_seconds = KEEPALIVE_REQUEST_TIMEOUT.as_secs(),
            "cache keepalive request sending"
        );
        let api_key = self
            .credentials
            .get(&session.upstream.id, API_KEY_CREDENTIAL)
            .await?
            .ok_or_else(|| anyhow::anyhow!("missing api key"))?;
        let http = match session.upstream.proxy_url.as_deref() {
            Some(proxy_url) if !proxy_url.trim().is_empty() => http::build_client(Some(proxy_url))?,
            _ => self.http.clone(),
        };
        let request = http
            .post(target_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .timeout(KEEPALIVE_REQUEST_TIMEOUT)
            .body(target_body);
        let request = upstream_auth::apply_api_key_auth(request, &session.upstream, &api_key);
        let response = upstream_auth::apply_anthropic_version(request, &session.upstream)
            .send()
            .await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            anyhow::bail!(
                "cache keepalive upstream status {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        let mut usage = usage::extract_usage_from_json(&value);
        usage.finish();
        Ok(usage)
    }

    async fn schedule_next(
        &self,
        key: &str,
        settings: &UpstreamCacheKeepaliveSettings,
        cached_tokens: Option<i64>,
    ) {
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
            tracing::info!(
                upstream_id = %session.upstream.id,
                endpoint = %session.endpoint,
                model = %session.model,
                session_key_prefix = %short_hash(&session.key),
                keepalive_count = session.keepalive_count,
                cached_tokens = session.cached_tokens,
                next_keepalive_seconds = interval.as_secs(),
                "cache keepalive next run scheduled"
            );
            self.events.bump_cache_keepalive();
        }
    }

    pub(super) async fn disable_session(&self, key: &str, reason: &str) {
        let mut inner = self.inner.lock().await;
        if let Some(session) = inner.sessions.get_mut(key) {
            if session.disabled_reason.is_some() {
                return;
            }
            session.disabled_reason = Some(reason.to_string());
            session.disabled_at = Some(Instant::now());
            tracing::info!(
                upstream_id = %session.upstream.id,
                model = %session.model,
                reason,
                "cache keepalive session disabled"
            );
            self.events.bump_cache_keepalive();
            drop(inner);
            self.schedule_disabled_prune();
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
        match self.store.insert_request_log(log).await {
            Ok(()) => self.events.bump_request_logs(),
            Err(err) => tracing::warn!(error = %err, "failed to record cache keepalive log"),
        }
    }
}

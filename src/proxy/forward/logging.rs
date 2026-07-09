use crate::app::AppState;
use crate::core::models::{RequestLog, TokenUsage, Upstream};
use axum::http::StatusCode;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub(super) async fn record_request_log(state: &AppState, log: RequestLog) {
    match state.store.insert_request_log(log).await {
        Ok(()) => state.events.bump_request_logs(),
        Err(err) => tracing::warn!(error = %err, "failed to record request log"),
    }
}

pub(super) struct AttemptLog<'a> {
    pub(super) state: &'a AppState,
    pub(super) started: Instant,
    pub(super) upstream: Option<&'a Upstream>,
    pub(super) endpoint: String,
    pub(super) model: Option<String>,
    pub(super) reasoning_effort: Option<String>,
    pub(super) status: StatusCode,
    pub(super) usage: TokenUsage,
    pub(super) first_token_ms: Option<i64>,
    pub(super) error: Option<String>,
}

pub(super) async fn record_attempt_log(log: AttemptLog<'_>) {
    record_request_log(
        log.state,
        RequestLog {
            ts: None,
            upstream_id: log.upstream.map(|upstream| upstream.id.clone()),
            upstream_name: log.upstream.map(|upstream| upstream.name.clone()),
            endpoint: log.endpoint,
            model: log.model,
            reasoning_effort: log.reasoning_effort,
            status: i64::from(log.status.as_u16()),
            usage: log.usage,
            estimated_cost_usd: None,
            duration_ms: log.started.elapsed().as_millis() as i64,
            first_token_ms: log.first_token_ms,
            error: log.error,
        },
    )
    .await;
}

#[derive(Clone)]
pub(super) struct StreamLogDraft {
    state: AppState,
    upstream_id: String,
    upstream_name: String,
    endpoint: String,
    model: Option<String>,
    reasoning_effort: Option<String>,
    status: StatusCode,
    started: Instant,
    inner: Arc<Mutex<StreamLogState>>,
}

#[derive(Clone, Default)]
struct StreamLogState {
    usage: TokenUsage,
    first_token_ms: Option<i64>,
    error: Option<String>,
    recorded: bool,
}

impl StreamLogDraft {
    pub(super) fn new(
        state: AppState,
        upstream: &Upstream,
        endpoint: String,
        model: Option<String>,
        reasoning_effort: Option<String>,
        status: StatusCode,
        started: Instant,
    ) -> Self {
        Self {
            state,
            upstream_id: upstream.id.clone(),
            upstream_name: upstream.name.clone(),
            endpoint,
            model,
            reasoning_effort,
            status,
            started,
            inner: Arc::new(Mutex::new(StreamLogState::default())),
        }
    }

    pub(super) fn set_first_token_ms(&self, value: i64) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.first_token_ms.is_none() {
            inner.first_token_ms = Some(value);
        }
    }

    pub(super) fn merge_usage(&self, usage: &TokenUsage) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.usage.merge_max(usage);
    }

    pub(super) async fn record(&self, error: Option<String>) {
        let Some(log) = self.take_log(self.status, error) else {
            return;
        };
        record_request_log(&self.state, log).await;
    }

    pub(super) async fn record_with_status(&self, status: StatusCode, error: Option<String>) {
        let Some(log) = self.take_log(status, error) else {
            return;
        };
        record_request_log(&self.state, log).await;
    }

    pub(super) fn record_on_drop(&self) {
        let Some(log) = self.take_log(self.status, None) else {
            return;
        };
        let state = self.state.clone();
        tokio::spawn(async move {
            record_request_log(&state, log).await;
        });
    }

    fn take_log(&self, status: StatusCode, error: Option<String>) -> Option<RequestLog> {
        let Ok(mut inner) = self.inner.lock() else {
            return None;
        };
        if inner.recorded {
            return None;
        }
        inner.recorded = true;
        if inner.error.is_none() {
            inner.error = error;
        }
        let mut usage = inner.usage.clone();
        usage.finish();
        Some(RequestLog {
            ts: None,
            upstream_id: Some(self.upstream_id.clone()),
            upstream_name: Some(self.upstream_name.clone()),
            endpoint: self.endpoint.clone(),
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            status: i64::from(status.as_u16()),
            usage,
            estimated_cost_usd: None,
            duration_ms: self.started.elapsed().as_millis() as i64,
            first_token_ms: inner.first_token_ms,
            error: inner.error.clone(),
        })
    }
}

pub(super) struct ActiveRequestGuard {
    state: AppState,
    request_id: String,
    active: bool,
}

impl ActiveRequestGuard {
    pub(super) fn new(state: AppState, request_id: String) -> Self {
        Self {
            state,
            request_id,
            active: true,
        }
    }

    pub(super) fn finish(&mut self) {
        if !self.active {
            return;
        }
        self.state.live_requests.finish(&self.request_id);
        self.state.events.bump_live_streams();
        self.active = false;
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

pub(super) struct LiveRequestGuard {
    active: ActiveRequestGuard,
    log: StreamLogDraft,
}

impl LiveRequestGuard {
    pub(super) fn from_active(active: ActiveRequestGuard, log: StreamLogDraft) -> Self {
        Self { active, log }
    }

    pub(super) fn finish(&mut self) {
        self.active.finish();
    }
}

impl Drop for LiveRequestGuard {
    fn drop(&mut self) {
        self.log.record_on_drop();
    }
}

use crate::core::models::{TokenUsage, Upstream, WireApi};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Default)]
pub(super) struct CacheKeepaliveInner {
    pub(super) sessions: HashMap<String, CacheKeepaliveSession>,
}

#[derive(Clone)]
pub(super) struct CacheKeepaliveSession {
    pub(super) key: String,
    pub(super) upstream: Upstream,
    pub(super) endpoint: String,
    pub(super) model: String,
    pub(super) body: Vec<u8>,
    pub(super) wire_api: WireApi,
    pub(super) cached_tokens: i64,
    pub(super) keepalive_count: i64,
    pub(super) last_user_request_at: Instant,
    pub(super) last_activity_at: Instant,
    pub(super) next_keepalive_at: Instant,
    pub(super) disabled_reason: Option<String>,
    pub(super) disabled_at: Option<Instant>,
}

pub struct CacheKeepaliveRegistration {
    pub upstream: Upstream,
    pub endpoint: String,
    pub model: Option<String>,
    pub body: Vec<u8>,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone)]
pub struct CacheKeepaliveSessionSnapshot {
    pub key: String,
    pub upstream_id: String,
    pub upstream_name: String,
    pub endpoint: String,
    pub model: String,
    pub wire_api: WireApi,
    pub cached_tokens: i64,
    pub keepalive_count: i64,
    pub last_user_request_elapsed_seconds: i64,
    pub last_activity_elapsed_seconds: i64,
    pub next_keepalive_seconds: i64,
    pub disabled_reason: Option<String>,
    pub body_bytes: usize,
}

impl CacheKeepaliveSession {
    pub(super) fn snapshot(&self, now: Instant) -> CacheKeepaliveSessionSnapshot {
        CacheKeepaliveSessionSnapshot {
            key: self.key.clone(),
            upstream_id: self.upstream.id.clone(),
            upstream_name: self.upstream.name.clone(),
            endpoint: self.endpoint.clone(),
            model: self.model.clone(),
            wire_api: self.wire_api,
            cached_tokens: self.cached_tokens,
            keepalive_count: self.keepalive_count,
            last_user_request_elapsed_seconds: elapsed_seconds(now, self.last_user_request_at),
            last_activity_elapsed_seconds: elapsed_seconds(now, self.last_activity_at),
            next_keepalive_seconds: if self.next_keepalive_at > now {
                (self.next_keepalive_at - now).as_secs() as i64
            } else {
                0
            },
            disabled_reason: self.disabled_reason.clone(),
            body_bytes: self.body.len(),
        }
    }
}

fn elapsed_seconds(now: Instant, then: Instant) -> i64 {
    if now >= then {
        (now - then).as_secs() as i64
    } else {
        0
    }
}

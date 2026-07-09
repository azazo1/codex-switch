use super::body::keepalive_body;
use super::key::session_key;
use super::*;
use crate::app::AppEvents;
use crate::core::models::{
    BalanceProvider, TokenUsage, Upstream, UpstreamCacheKeepaliveSettings, WireApi,
};
use crate::storage::Store;
use crate::storage::credentials::CredentialStore;
use serde_json::{Value, json};
use std::time::Instant;

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

#[tokio::test]
async fn snapshots_are_sorted_and_include_runtime_fields() {
    let runtime = test_runtime().await;
    let upstream_b = upstream("b", "upstream-b");
    let upstream_a = upstream("a", "upstream-a");
    save_enabled_settings(&runtime, &upstream_b).await;
    save_enabled_settings(&runtime, &upstream_a).await;

    runtime
        .register(registration(&upstream_b, "model-b", "session-b", 4096))
        .await;
    runtime
        .register(registration(&upstream_a, "model-a", "session-a", 2048))
        .await;

    let snapshots = runtime.snapshots().await;

    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].upstream_name, "upstream-a");
    assert_eq!(snapshots[0].model, "model-a");
    assert_eq!(snapshots[0].cached_tokens, 2048);
    assert_eq!(snapshots[0].body_bytes, registration_body("model-a", "session-a").len());
    assert!(snapshots[0].next_keepalive_seconds > 0);
}

#[tokio::test]
async fn disabled_snapshot_keeps_reason() {
    let runtime = test_runtime().await;
    let upstream = upstream("a", "upstream-a");
    save_enabled_settings(&runtime, &upstream).await;
    runtime
        .register(registration(&upstream, "model-a", "session-a", 2048))
        .await;
    let key = runtime.snapshots().await[0].key.clone();

    runtime.disable_session(&key, "cache miss").await;
    let snapshots = runtime.snapshots().await;

    assert_eq!(snapshots[0].disabled_reason.as_deref(), Some("cache miss"));
}

#[tokio::test]
async fn register_skips_sessions_above_max_cacheable_tokens() {
    let runtime = test_runtime().await;
    let upstream = upstream("a", "upstream-a");
    runtime.store.save_upstream(&upstream).await.unwrap();
    let mut settings = UpstreamCacheKeepaliveSettings::new(upstream.id.clone());
    settings.enabled = true;
    settings.max_cacheable_tokens = 4096;
    runtime
        .store
        .save_cache_keepalive_settings(&settings)
        .await
        .unwrap();

    runtime
        .register(registration(&upstream, "model-a", "session-a", 8192))
        .await;

    assert!(runtime.snapshots().await.is_empty());
}

#[tokio::test]
async fn disable_upstream_sessions_marks_matching_sessions() {
    let runtime = test_runtime().await;
    let upstream = upstream("a", "upstream-a");
    save_enabled_settings(&runtime, &upstream).await;
    runtime
        .register(registration(&upstream, "model-a", "session-a", 2048))
        .await;

    let disabled = runtime
        .disable_upstream_sessions(&upstream.id, "settings disabled")
        .await;
    let snapshots = runtime.snapshots().await;

    assert_eq!(disabled, 1);
    assert_eq!(
        snapshots[0].disabled_reason.as_deref(),
        Some("settings disabled")
    );
}

#[tokio::test]
async fn prune_disabled_sessions_removes_expired_sessions() {
    let runtime = test_runtime().await;
    let upstream = upstream("a", "upstream-a");
    save_enabled_settings(&runtime, &upstream).await;
    runtime
        .register(registration(&upstream, "model-a", "session-a", 2048))
        .await;
    let key = runtime.snapshots().await[0].key.clone();
    runtime.disable_session(&key, "cache miss").await;
    {
        let mut inner = runtime.inner.lock().await;
        let session = inner.sessions.get_mut(&key).unwrap();
        session.disabled_at = Some(Instant::now() - DISABLED_SESSION_RETENTION);
    }

    runtime.prune_disabled_sessions().await;

    assert!(runtime.snapshots().await.is_empty());
}

async fn test_runtime() -> CacheKeepaliveRuntime {
    let path = std::env::temp_dir()
        .join(format!("codex-switch-cache-runtime-{}.sqlite", uuid::Uuid::new_v4()));
    let store = Store::open(path).await.unwrap();
    let credentials = CredentialStore::new_for_tests(store.clone());
    CacheKeepaliveRuntime::new(
        store,
        credentials,
        reqwest::Client::new(),
        AppEvents::default(),
    )
}

async fn save_enabled_settings(runtime: &CacheKeepaliveRuntime, upstream: &Upstream) {
    runtime.store.save_upstream(upstream).await.unwrap();
    let mut settings = UpstreamCacheKeepaliveSettings::new(upstream.id.clone());
    settings.enabled = true;
    runtime
        .store
        .save_cache_keepalive_settings(&settings)
        .await
        .unwrap();
}

fn registration(
    upstream: &Upstream,
    model: &str,
    session_id: &str,
    cached_tokens: i64,
) -> CacheKeepaliveRegistration {
    CacheKeepaliveRegistration {
        upstream: upstream.clone(),
        endpoint: "/responses".to_string(),
        model: Some(model.to_string()),
        body: registration_body(model, session_id),
        usage: TokenUsage {
            input_tokens: cached_tokens,
            cache_read_tokens: cached_tokens,
            total_tokens: cached_tokens,
            ..Default::default()
        },
    }
}

fn registration_body(model: &str, session_id: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "model": model,
        "prompt_cache_key": session_id,
        "input": "hello"
    }))
    .unwrap()
}

fn upstream(id: &str, name: &str) -> Upstream {
    let mut upstream = Upstream::new_relay(
        name.to_string(),
        "http://127.0.0.1".to_string(),
        WireApi::Responses,
        true,
        BalanceProvider::Unsupported,
    );
    upstream.id = id.to_string();
    upstream
}

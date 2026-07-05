use crate::app::AppState;
use crate::core::models::{QuotaSnapshot, UpstreamKind};
use crate::oauth;
use anyhow::{anyhow, Context};
use axum::http::HeaderMap;
use serde::Deserialize;

const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

#[derive(Debug, Deserialize)]
struct UsageResponse {
    rate_limit: Option<RateLimit>,
}

#[derive(Debug, Deserialize)]
struct RateLimit {
    primary_window: Option<RateLimitWindow>,
    secondary_window: Option<RateLimitWindow>,
}

#[derive(Debug, Deserialize)]
struct RateLimitWindow {
    used_percent: f64,
    limit_window_seconds: i64,
    reset_after_seconds: i64,
}

pub async fn query_and_store(
    state: &AppState,
    upstream_id: &str,
) -> anyhow::Result<QuotaSnapshot> {
    let upstream = state
        .store
        .get_upstream(upstream_id)
        .await?
        .ok_or_else(|| anyhow!("upstream not found"))?;
    if upstream.kind != UpstreamKind::CodexOauth {
        return Err(anyhow!("quota query only supports codex oauth upstreams"));
    }
    let token = oauth::valid_access_token(state, &upstream).await?;
    let account_id = upstream
        .chatgpt_account_id
        .as_deref()
        .ok_or_else(|| anyhow!("missing chatgpt_account_id"))?;
    let response = state
        .http
        .get(CHATGPT_USAGE_URL)
        .bearer_auth(token)
        .header("chatgpt-account-id", account_id)
        .header("openai-beta", "codex-1")
        .header("oai-language", "zh-CN")
        .header("originator", "Codex Desktop")
        .header("accept", "application/json")
        .send()
        .await
        .context("failed to query codex quota")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("quota query failed: {status} {body}"));
    }
    let body: UsageResponse = response.json().await.context("invalid quota response")?;
    let snapshot = snapshot_from_rate_limit(upstream_id, body.rate_limit.as_ref());
    state.store.save_quota_snapshot(&snapshot).await?;
    Ok(snapshot)
}

pub fn snapshot_from_headers(upstream_id: &str, headers: &HeaderMap) -> Option<QuotaSnapshot> {
    let primary = WindowCandidate {
        used_percent: header_f64(headers, "x-codex-primary-used-percent"),
        reset_after_seconds: header_i64(headers, "x-codex-primary-reset-after-seconds"),
        window_minutes: header_i64(headers, "x-codex-primary-window-minutes"),
    };
    let secondary = WindowCandidate {
        used_percent: header_f64(headers, "x-codex-secondary-used-percent"),
        reset_after_seconds: header_i64(headers, "x-codex-secondary-reset-after-seconds"),
        window_minutes: header_i64(headers, "x-codex-secondary-window-minutes"),
    };
    normalize_windows(upstream_id, primary, secondary)
}

fn snapshot_from_rate_limit(
    upstream_id: &str,
    rate_limit: Option<&RateLimit>,
) -> QuotaSnapshot {
    let primary = rate_limit
        .and_then(|r| r.primary_window.as_ref())
        .map(window_from_usage)
        .unwrap_or_default();
    let secondary = rate_limit
        .and_then(|r| r.secondary_window.as_ref())
        .map(window_from_usage)
        .unwrap_or_default();
    normalize_windows(upstream_id, primary, secondary).unwrap_or_else(|| QuotaSnapshot {
        upstream_id: upstream_id.to_string(),
        fetched_at: chrono::Utc::now().timestamp(),
        ..QuotaSnapshot::default()
    })
}

#[derive(Debug, Clone, Default)]
struct WindowCandidate {
    used_percent: Option<f64>,
    reset_after_seconds: Option<i64>,
    window_minutes: Option<i64>,
}

fn window_from_usage(window: &RateLimitWindow) -> WindowCandidate {
    WindowCandidate {
        used_percent: Some(window.used_percent),
        reset_after_seconds: Some(window.reset_after_seconds),
        window_minutes: Some(window.limit_window_seconds / 60),
    }
}

fn normalize_windows(
    upstream_id: &str,
    primary: WindowCandidate,
    secondary: WindowCandidate,
) -> Option<QuotaSnapshot> {
    if primary.used_percent.is_none()
        && primary.reset_after_seconds.is_none()
        && secondary.used_percent.is_none()
        && secondary.reset_after_seconds.is_none()
    {
        return None;
    }
    let primary_is_5h = match (primary.window_minutes, secondary.window_minutes) {
        (Some(p), Some(s)) => p < s,
        (Some(p), None) => p <= 360,
        (None, Some(s)) => s > 360,
        (None, None) => false,
    };
    let (five, seven) = if primary_is_5h {
        (primary, secondary)
    } else {
        (secondary, primary)
    };
    Some(QuotaSnapshot {
        upstream_id: upstream_id.to_string(),
        used_5h_percent: five.used_percent,
        reset_5h_seconds: five.reset_after_seconds,
        window_5h_minutes: five.window_minutes,
        used_7d_percent: seven.used_percent,
        reset_7d_seconds: seven.reset_after_seconds,
        window_7d_minutes: seven.window_minutes,
        fetched_at: chrono::Utc::now().timestamp(),
    })
}

fn header_f64(headers: &HeaderMap, name: &str) -> Option<f64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

fn header_i64(headers: &HeaderMap, name: &str) -> Option<i64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    #[test]
    fn normalizes_codex_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-codex-primary-used-percent", "12".parse().unwrap());
        headers.insert("x-codex-secondary-used-percent", "34".parse().unwrap());
        headers.insert("x-codex-primary-window-minutes", "300".parse().unwrap());
        headers.insert("x-codex-secondary-window-minutes", "10080".parse().unwrap());
        headers.insert("x-codex-primary-reset-after-seconds", "600".parse().unwrap());
        headers.insert("x-codex-secondary-reset-after-seconds", "86400".parse().unwrap());
        let snapshot = snapshot_from_headers("u1", &headers).unwrap();
        assert_eq!(snapshot.used_5h_percent, Some(12.0));
        assert_eq!(snapshot.used_7d_percent, Some(34.0));
        assert_eq!(snapshot.reset_5h_seconds, Some(600));
        assert_eq!(snapshot.reset_7d_seconds, Some(86400));
    }
}

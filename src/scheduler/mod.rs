use crate::core::models::{ScheduleGroup, ScheduleMode, ScheduleRouteTargetKind, Upstream};
use axum::http::StatusCode;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub struct SchedulerRuntime {
    inner: Arc<Mutex<SchedulerInner>>,
}

#[derive(Default)]
struct SchedulerInner {
    round_robin: HashMap<String, i64>,
    affinity: HashMap<String, AffinityEntry>,
    failures: HashMap<String, FailureState>,
}

struct AffinityEntry {
    upstream_id: String,
    expires_at: Instant,
}

#[derive(Default)]
struct FailureState {
    count: i64,
    last_failed_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct SchedulerPlan {
    pub group: ScheduleGroup,
    pub candidates: Vec<Upstream>,
    pub affinity_key: Option<String>,
    pub target_model: Option<String>,
    pub route_path: Vec<ScheduleRouteTraceStep>,
}

#[derive(Debug, Clone)]
pub struct ScheduleRouteTraceStep {
    pub group_id: String,
    pub rule_id: String,
    pub target_kind: ScheduleRouteTargetKind,
    pub target_id: String,
}

#[derive(Debug, Clone)]
pub struct DirectSchedulerPlan {
    pub group: ScheduleGroup,
    pub upstream: Upstream,
    pub target_model: Option<String>,
    pub route_path: Vec<ScheduleRouteTraceStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerFailureKind {
    Network,
    Balance,
    Server,
    OtherStatus,
}

impl SchedulerRuntime {
    pub async fn plan(
        &self,
        group: ScheduleGroup,
        upstreams: Vec<Upstream>,
        body: &[u8],
        endpoint: &str,
        model: Option<&str>,
    ) -> anyhow::Result<SchedulerPlan> {
        if upstreams.is_empty() {
            anyhow::bail!("schedule group has no available upstream");
        }

        let affinity_key = affinity_key_from_body(&group.id, body, endpoint, model);
        let mut inner = self.inner.lock().await;
        inner.purge_affinity();
        let affinity_upstream_id = affinity_key
            .as_ref()
            .and_then(|key| inner.affinity.get(key))
            .filter(|entry| entry.expires_at > Instant::now())
            .map(|entry| entry.upstream_id.clone());
        let has_affinity_candidate = affinity_upstream_id
            .as_ref()
            .is_some_and(|id| upstreams.iter().any(|upstream| upstream.id == *id));
        let mut candidates = match group.mode {
            ScheduleMode::Fixed => fixed_candidates(&group, upstreams)?,
            ScheduleMode::ModelMapping => {
                anyhow::bail!("model mapping schedule group has no matching route");
            }
            ScheduleMode::Random | ScheduleMode::RoundRobin if has_affinity_candidate => {
                affinity_candidates(upstreams, affinity_upstream_id.as_deref())
            }
            ScheduleMode::Random => random_candidates(&group, upstreams),
            ScheduleMode::RoundRobin => round_robin_candidates(&mut inner, &group, upstreams),
            ScheduleMode::Failover => failover_candidates(&inner, &group, upstreams),
        };

        if let Some(upstream_id) = affinity_upstream_id {
            move_candidate_to_front(&mut candidates, &upstream_id);
        }

        Ok(SchedulerPlan {
            group,
            candidates,
            affinity_key,
            target_model: None,
            route_path: Vec::new(),
        })
    }

    pub async fn plan_direct(
        &self,
        direct: DirectSchedulerPlan,
        body: &[u8],
        endpoint: &str,
        model: Option<&str>,
    ) -> anyhow::Result<SchedulerPlan> {
        let affinity_key = affinity_key_from_body(&direct.group.id, body, endpoint, model);
        Ok(SchedulerPlan {
            group: direct.group,
            candidates: vec![direct.upstream],
            affinity_key,
            target_model: direct.target_model,
            route_path: direct.route_path,
        })
    }

    pub async fn record_success(
        &self,
        group_id: &str,
        upstream_id: &str,
        affinity_key: Option<&str>,
        affinity_ttl_seconds: i64,
    ) {
        let mut inner = self.inner.lock().await;
        inner.failures.remove(&failure_key(group_id, upstream_id));
        if let Some(key) = affinity_key {
            inner.affinity.insert(
                key.to_string(),
                AffinityEntry {
                    upstream_id: upstream_id.to_string(),
                    expires_at: Instant::now()
                        + Duration::from_secs(affinity_ttl_seconds.max(60) as u64),
                },
            );
        }
    }

    pub async fn record_failure(&self, group_id: &str, upstream_id: &str) -> i64 {
        let mut inner = self.inner.lock().await;
        let state = inner
            .failures
            .entry(failure_key(group_id, upstream_id))
            .or_default();
        state.count += 1;
        state.last_failed_at = Some(Instant::now());
        state.count
    }

    pub fn should_retry(group: &ScheduleGroup, failure: SchedulerFailureKind, count: i64) -> bool {
        if group.mode != ScheduleMode::Failover {
            return false;
        }
        let threshold_reached = count >= group.failure_threshold.max(1);
        match failure {
            SchedulerFailureKind::Balance => group.failover_on_balance || threshold_reached,
            SchedulerFailureKind::Network => group.failover_on_network || threshold_reached,
            SchedulerFailureKind::Server => group.failover_on_5xx || threshold_reached,
            SchedulerFailureKind::OtherStatus => threshold_reached,
        }
    }
}

pub fn classify_response(status: StatusCode, body: &[u8]) -> Option<SchedulerFailureKind> {
    if looks_like_balance_failure(status, body) {
        return Some(SchedulerFailureKind::Balance);
    }
    if status.is_server_error() {
        return Some(SchedulerFailureKind::Server);
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Some(SchedulerFailureKind::OtherStatus);
    }
    None
}

fn fixed_candidates(
    group: &ScheduleGroup,
    upstreams: Vec<Upstream>,
) -> anyhow::Result<Vec<Upstream>> {
    let Some(id) = &group.fixed_upstream_id else {
        anyhow::bail!("fixed schedule group has no upstream selected");
    };
    let Some(upstream) = upstreams.into_iter().find(|upstream| upstream.id == *id) else {
        anyhow::bail!("fixed schedule upstream is not available");
    };
    Ok(vec![upstream])
}

fn affinity_candidates(upstreams: Vec<Upstream>, upstream_id: Option<&str>) -> Vec<Upstream> {
    let Some(upstream_id) = upstream_id else {
        return upstreams;
    };
    upstreams
        .into_iter()
        .find(|upstream| upstream.id == upstream_id)
        .map(|upstream| vec![upstream])
        .unwrap_or_default()
}

fn random_candidates(_group: &ScheduleGroup, upstreams: Vec<Upstream>) -> Vec<Upstream> {
    let mut candidates = upstreams;
    let Some(index) = weighted_random_index(&candidates) else {
        return candidates;
    };
    candidates.swap(0, index);
    candidates.truncate(1);
    candidates
}

fn round_robin_candidates(
    inner: &mut SchedulerInner,
    group: &ScheduleGroup,
    upstreams: Vec<Upstream>,
) -> Vec<Upstream> {
    let mut candidates = upstreams;
    let Some(total_weight) = total_weight(&candidates) else {
        return candidates;
    };
    let cursor = inner.round_robin.entry(group.id.clone()).or_default();
    let selected = weighted_index(&candidates, *cursor % total_weight).unwrap_or(0);
    *cursor = (*cursor + 1) % total_weight;
    candidates.swap(0, selected);
    candidates.truncate(1);
    candidates
}

fn failover_candidates(
    inner: &SchedulerInner,
    group: &ScheduleGroup,
    upstreams: Vec<Upstream>,
) -> Vec<Upstream> {
    let threshold = group.failure_threshold.max(1);
    let mut healthy = Vec::new();
    let mut unhealthy = Vec::new();
    for upstream in upstreams {
        let count = inner
            .failures
            .get(&failure_key(&group.id, &upstream.id))
            .map(|state| state.count)
            .unwrap_or_default();
        if count >= threshold {
            unhealthy.push(upstream);
        } else {
            healthy.push(upstream);
        }
    }
    if healthy.is_empty() {
        unhealthy
    } else {
        healthy.extend(unhealthy);
        healthy
    }
}

fn weighted_random_index(upstreams: &[Upstream]) -> Option<usize> {
    let total = total_weight(upstreams)?;
    let value = (uuid::Uuid::new_v4().as_u128() % total as u128) as i64;
    weighted_index(upstreams, value)
}

fn weighted_index(upstreams: &[Upstream], mut value: i64) -> Option<usize> {
    for (index, upstream) in upstreams.iter().enumerate() {
        let weight = upstream.weight.max(1);
        if value < weight {
            return Some(index);
        }
        value -= weight;
    }
    None
}

fn total_weight(upstreams: &[Upstream]) -> Option<i64> {
    let total = upstreams
        .iter()
        .map(|upstream| upstream.weight.max(1))
        .sum();
    if total > 0 { Some(total) } else { None }
}

fn move_candidate_to_front(candidates: &mut [Upstream], upstream_id: &str) {
    if let Some(index) = candidates
        .iter()
        .position(|candidate| candidate.id == upstream_id)
    {
        candidates.swap(0, index);
    }
}

fn affinity_key_from_body(
    group_id: &str,
    body: &[u8],
    endpoint: &str,
    model: Option<&str>,
) -> Option<String> {
    let value = serde_json::from_slice::<Value>(body).ok()?;
    let raw = find_string(&value, "prompt_cache_key")
        .or_else(|| find_string(&value, "conversation_id"))
        .or_else(|| find_string(&value, "session_id"))?;
    let mut hasher = Sha256::new();
    hasher.update(group_id.as_bytes());
    hasher.update(endpoint.as_bytes());
    if let Some(model) = model {
        hasher.update(model.as_bytes());
    }
    hasher.update(raw.as_bytes());
    Some(format!("{:x}", hasher.finalize()))
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

fn looks_like_balance_failure(status: StatusCode, body: &[u8]) -> bool {
    if status == StatusCode::PAYMENT_REQUIRED {
        return true;
    }
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    [
        "insufficient balance",
        "insufficient credit",
        "quota exceeded",
        "billing",
        "no credits",
        "not enough balance",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn failure_key(group_id: &str, upstream_id: &str) -> String {
    format!("{group_id}:{upstream_id}")
}

pub fn glob_captures(pattern: &str, value: &str) -> Option<Vec<String>> {
    let pattern_items = pattern.char_indices().collect::<Vec<_>>();
    let value_items = value.char_indices().collect::<Vec<_>>();
    glob_captures_inner(value, &pattern_items, &value_items, 0, 0, Vec::new())
}

pub fn rewrite_model_template(template: &str, captures: &[String]) -> String {
    let mut result = String::new();
    let mut capture_index = 0;
    for ch in template.chars() {
        if matches!(ch, '*' | '?') {
            if let Some(capture) = captures.get(capture_index) {
                result.push_str(capture);
                capture_index += 1;
            } else {
                result.push(ch);
            }
        } else {
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
fn is_exact_pattern(pattern: &str) -> bool {
    !pattern
        .as_bytes()
        .iter()
        .any(|byte| matches!(byte, b'*' | b'?'))
}

fn glob_captures_inner(
    value: &str,
    pattern_items: &[(usize, char)],
    value_items: &[(usize, char)],
    pattern_index: usize,
    value_index: usize,
    captures: Vec<String>,
) -> Option<Vec<String>> {
    if pattern_index == pattern_items.len() {
        return (value_index == value_items.len()).then_some(captures);
    }
    let (_, pattern_char) = pattern_items[pattern_index];
    match pattern_char {
        '*' => {
            let start = value_byte_index(value, value_items, value_index);
            for end_index in value_index..=value_items.len() {
                let end = value_byte_index(value, value_items, end_index);
                let mut next_captures = captures.clone();
                next_captures.push(value[start..end].to_string());
                if let Some(found) = glob_captures_inner(
                    value,
                    pattern_items,
                    value_items,
                    pattern_index + 1,
                    end_index,
                    next_captures,
                ) {
                    return Some(found);
                }
            }
            None
        }
        '?' => {
            if value_index >= value_items.len() {
                return None;
            }
            let start = value_byte_index(value, value_items, value_index);
            let end = value_byte_index(value, value_items, value_index + 1);
            let mut next_captures = captures;
            next_captures.push(value[start..end].to_string());
            glob_captures_inner(
                value,
                pattern_items,
                value_items,
                pattern_index + 1,
                value_index + 1,
                next_captures,
            )
        }
        literal => {
            if value_index >= value_items.len() || value_items[value_index].1 != literal {
                return None;
            }
            glob_captures_inner(
                value,
                pattern_items,
                value_items,
                pattern_index + 1,
                value_index + 1,
                captures,
            )
        }
    }
}

fn value_byte_index(value: &str, value_items: &[(usize, char)], index: usize) -> usize {
    value_items
        .get(index)
        .map(|(byte_index, _)| *byte_index)
        .unwrap_or(value.len())
}

impl SchedulerInner {
    fn purge_affinity(&mut self) {
        let now = Instant::now();
        self.affinity.retain(|_, entry| entry.expires_at > now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{BalanceProvider, WireApi};

    #[tokio::test]
    async fn round_robin_rotates_upstreams() {
        let runtime = SchedulerRuntime::default();
        let group = ScheduleGroup {
            mode: ScheduleMode::RoundRobin,
            ..ScheduleGroup::new("test".to_string())
        };
        let upstreams = vec![upstream("a"), upstream("b")];

        let first = runtime
            .plan(group.clone(), upstreams.clone(), b"{}", "/responses", None)
            .await
            .unwrap();
        let second = runtime
            .plan(group, upstreams, b"{}", "/responses", None)
            .await
            .unwrap();

        assert_ne!(first.candidates[0].id, second.candidates[0].id);
    }

    #[tokio::test]
    async fn affinity_keeps_prompt_cache_key_on_same_upstream() {
        let runtime = SchedulerRuntime::default();
        let group = ScheduleGroup {
            mode: ScheduleMode::RoundRobin,
            ..ScheduleGroup::new("test".to_string())
        };
        let body = br#"{"model":"gpt-test","prompt_cache_key":"stable"}"#;
        let upstreams = vec![upstream("a"), upstream("b")];

        let first = runtime
            .plan(
                group.clone(),
                upstreams.clone(),
                body,
                "/responses",
                Some("gpt-test"),
            )
            .await
            .unwrap();
        let selected = first.candidates[0].id.clone();
        runtime
            .record_success(&group.id, &selected, first.affinity_key.as_deref(), 1800)
            .await;
        let second = runtime
            .plan(group, upstreams, body, "/responses", Some("gpt-test"))
            .await
            .unwrap();

        assert_eq!(selected, second.candidates[0].id);
    }

    #[test]
    fn classifies_balance_failures() {
        let failure = classify_response(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"message":"insufficient balance"}}"#,
        );

        assert_eq!(failure, Some(SchedulerFailureKind::Balance));
    }

    #[test]
    fn matches_model_globs() {
        assert!(glob_captures("glm-*", "glm-4.5").is_some());
        assert!(glob_captures("gpt-image-?", "gpt-image-1").is_some());
        assert!(glob_captures("exact-model", "exact-model").is_some());
        assert!(glob_captures("glm-*", "qwen-3").is_none());
        assert!(glob_captures("gpt-image-?", "gpt-image-xl").is_none());
        assert!(is_exact_pattern("exact-model"));
        assert!(!is_exact_pattern("glm-*"));
    }

    #[test]
    fn captures_and_rewrites_model_templates() {
        let captures = glob_captures("glm/*", "glm/glm-4.5").unwrap();
        assert_eq!(captures, vec!["glm-4.5"]);
        assert_eq!(rewrite_model_template("*", &captures), "glm-4.5");

        let captures = glob_captures("vendor/*/model-?", "vendor/acme/model-a").unwrap();
        assert_eq!(captures, vec!["acme", "a"]);
        assert_eq!(rewrite_model_template("*/?", &captures), "acme/a");
        assert_eq!(
            rewrite_model_template("fixed-model", &captures),
            "fixed-model"
        );
    }

    fn upstream(id: &str) -> Upstream {
        let mut upstream = Upstream::new_relay(
            id.to_string(),
            "http://127.0.0.1".to_string(),
            WireApi::Responses,
            true,
            BalanceProvider::Unsupported,
        );
        upstream.id = id.to_string();
        upstream
    }
}

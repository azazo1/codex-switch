use crate::app::AppState;
use crate::core::models::{
    ScheduleGroup, ScheduleMode, ScheduleRouteTargetKind, Upstream, UpstreamKind,
};
use crate::proxy::transform;
use crate::scheduler::is_exact_pattern;
use axum::http::HeaderMap;
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};

use super::headers::apply_headers;

pub(super) async fn query_models(state: &AppState, headers: &HeaderMap) -> anyhow::Result<Value> {
    let group = state.store.current_schedule_group().await?;
    let max_hops = state.store.scheduler_route_max_hops().await?;
    let sources = reachable_model_sources(state, group, max_hops).await?;
    let mut models = Vec::new();
    let mut seen = BTreeSet::new();
    let mut errors = Vec::new();

    for alias in sources.aliases {
        push_models(&mut models, &mut seen, vec![alias]);
    }

    for upstream in sources.upstreams {
        match query_upstream_models(state, headers, &upstream).await {
            Ok(items) => push_models(&mut models, &mut seen, items),
            Err(err) => {
                tracing::warn!(
                    upstream_id = %upstream.id,
                    upstream_name = %upstream.name,
                    error = %err,
                    "failed to query upstream models"
                );
                errors.push(format!("{}: {err}", upstream.name));
            }
        }
    }

    if models.is_empty() && !errors.is_empty() {
        anyhow::bail!("failed to query models from all upstreams: {}", errors.join("; "));
    }

    Ok(json!({"object":"list","data":models}))
}

struct ModelSources {
    upstreams: Vec<Upstream>,
    aliases: Vec<Value>,
}

async fn reachable_model_sources(
    state: &AppState,
    root: ScheduleGroup,
    max_hops: i64,
) -> anyhow::Result<ModelSources> {
    let mut upstreams = Vec::new();
    let mut aliases = Vec::new();
    let mut seen_groups = BTreeSet::new();
    let mut seen_upstreams = BTreeSet::new();
    let mut queue = VecDeque::from([(root, 0_i64)]);
    while let Some((group, hops)) = queue.pop_front() {
        if !seen_groups.insert(group.id.clone()) {
            continue;
        }
        if group.mode != ScheduleMode::ModelMapping {
            for upstream in state
                .store
                .schedule_group_upstreams_nested(&group, max_hops)
                .await?
            {
                if seen_upstreams.insert(upstream.id.clone()) {
                    upstreams.push(upstream);
                }
            }
            continue;
        }
        let rules = state.store.list_schedule_route_rules(&group.id).await?;
        for rule in rules.into_iter().filter(|rule| rule.enabled) {
            if is_exact_pattern(&rule.pattern)
                && let Some(target_model) = rule.target_model.as_deref().map(str::trim)
                && !target_model.is_empty()
            {
                aliases.push(alias_model(&rule.pattern, target_model, &group.name));
            }
            if hops >= max_hops.max(1) {
                tracing::warn!(
                    group_id = %group.id,
                    rule_id = %rule.id,
                    max_hops,
                    "model source traversal skipped route beyond max hops"
                );
                continue;
            }
            match rule.target_kind {
                ScheduleRouteTargetKind::Group => {
                    let Some(target_group_id) =
                        rule.target_group_id.as_deref().map(str::trim).filter(|id| !id.is_empty())
                    else {
                        continue;
                    };
                    if let Some(target_group) = state.store.get_schedule_group(target_group_id).await? {
                        queue.push_back((target_group, hops + 1));
                    }
                }
                ScheduleRouteTargetKind::Upstream => {
                    let Some(target_upstream_id) =
                        rule.target_upstream_id.as_deref().map(str::trim).filter(|id| !id.is_empty())
                    else {
                        continue;
                    };
                    let Some(upstream) = state.store.get_upstream(target_upstream_id).await? else {
                        continue;
                    };
                    if upstream.enabled && seen_upstreams.insert(upstream.id.clone()) {
                        upstreams.push(upstream);
                    }
                }
            }
        }
    }
    Ok(ModelSources { upstreams, aliases })
}

async fn query_upstream_models(
    state: &AppState,
    headers: &HeaderMap,
    upstream: &Upstream,
) -> anyhow::Result<Vec<Value>> {
    match upstream.kind {
        UpstreamKind::RelayApiKey => query_relay_models(state, headers, upstream).await,
        UpstreamKind::CodexOauth => Ok(vec![fallback_model(upstream)]),
    }
}

async fn query_relay_models(
    state: &AppState,
    headers: &HeaderMap,
    upstream: &Upstream,
) -> anyhow::Result<Vec<Value>> {
    let target_url = transform::build_endpoint(&upstream.base_url, "/models");
    let request = state.http.get(target_url);
    let request = apply_headers(state, upstream, request, headers).await?;
    let response = request.send().await?;
    let status = response.status();
    let value = response.json::<Value>().await?;
    if !status.is_success() {
        anyhow::bail!(
            "{}",
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("models endpoint returned an error")
        );
    }
    Ok(normalize_models_response(&value, upstream))
}

fn push_models(models: &mut Vec<Value>, seen: &mut BTreeSet<String>, items: Vec<Value>) {
    for item in items {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        if seen.insert(id.to_string()) {
            models.push(item);
        }
    }
}

fn normalize_models_response(value: &Value, upstream: &Upstream) -> Vec<Value> {
    if let Some(items) = value.get("data").and_then(Value::as_array) {
        return items
            .iter()
            .filter_map(|item| normalize_model_item(item, upstream))
            .collect();
    }
    if let Some(items) = value.as_array() {
        return items
            .iter()
            .filter_map(|item| normalize_model_item(item, upstream))
            .collect();
    }
    Vec::new()
}

fn normalize_model_item(item: &Value, upstream: &Upstream) -> Option<Value> {
    let id = item
        .get("id")
        .or_else(|| item.get("name"))
        .and_then(Value::as_str)?;
    let mut model = item.as_object().cloned().unwrap_or_default();
    model.insert("id".to_string(), json!(id));
    model
        .entry("object".to_string())
        .or_insert_with(|| json!("model"));
    model
        .entry("created".to_string())
        .or_insert_with(|| json!(0));
    model
        .entry("owned_by".to_string())
        .or_insert_with(|| json!(upstream.name));
    Some(Value::Object(model))
}

fn fallback_model(upstream: &Upstream) -> Value {
    json!({
        "id": upstream.name,
        "object": "model",
        "created": 0,
        "owned_by": "codex-switch",
        "upstream_id": upstream.id
    })
}

fn alias_model(id: &str, target_model: &str, group_name: &str) -> Value {
    json!({
        "id": id,
        "object": "model",
        "created": 0,
        "owned_by": group_name,
        "target_model": target_model
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{BalanceProvider, WireApi};

    #[test]
    fn normalizes_openai_models_response() {
        let upstream = Upstream::new_relay(
            "mock".to_string(),
            "http://127.0.0.1".to_string(),
            WireApi::Responses,
            true,
            BalanceProvider::Unsupported,
        );
        let items = normalize_models_response(
            &json!({"object":"list","data":[{"id":"gpt-test"},{"name":"named-model"}]}),
            &upstream,
        );

        assert_eq!(items[0]["id"], "gpt-test");
        assert_eq!(items[0]["object"], "model");
        assert_eq!(items[1]["id"], "named-model");
        assert_eq!(items[1]["owned_by"], "mock");
    }
}

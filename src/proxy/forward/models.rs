use crate::app::AppState;
use crate::core::models::{
    ScheduleGroup, ScheduleMode, ScheduleRouteRule, ScheduleRouteTargetKind, Upstream,
    UpstreamKind,
};
use crate::proxy::transform;
use crate::scheduler::{glob_captures, rewrite_model_template};
use axum::http::HeaderMap;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::headers::apply_headers;

#[derive(Debug, thiserror::Error)]
#[error("model not found: {0}")]
pub(super) struct ModelNotFound(pub String);

pub(super) async fn query_models(
    state: &AppState,
    headers: &HeaderMap,
    uri: &axum::http::Uri,
    model_id: Option<&str>,
) -> anyhow::Result<Value> {
    let group = state.store.current_schedule_group().await?;
    let max_hops = state.store.scheduler_route_max_hops().await?;
    let sources = reachable_model_sources(state, group, max_hops).await?;
    let mut models = Vec::new();
    let mut seen = BTreeSet::new();
    let mut upstream_models: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut errors = Vec::new();

    for source in sources {
        let items = if let Some(items) = upstream_models.get(&source.upstream.id) {
            items.clone()
        } else {
            match query_upstream_models(state, headers, &source.upstream).await {
                Ok(items) => {
                    upstream_models.insert(source.upstream.id.clone(), items.clone());
                    items
                }
                Err(err) => {
                    tracing::warn!(
                        upstream_id = %source.upstream.id,
                        upstream_name = %source.upstream.name,
                        error = %err,
                        "failed to query upstream models"
                    );
                    errors.push(format!("{}: {err}", source.upstream.name));
                    continue;
                }
            }
        };
        push_models(
            &mut models,
            &mut seen,
            reverse_model_path(items, &source.rules),
        );
    }

    if models.is_empty() && !errors.is_empty() {
        anyhow::bail!(
            "failed to query models from all upstreams: {}",
            errors.join("; ")
        );
    }

    let anthropic = headers.contains_key("anthropic-version");
    if let Some(model_id) = model_id {
        let model = models
            .into_iter()
            .find(|model| model.get("id").and_then(Value::as_str) == Some(model_id))
            .ok_or_else(|| ModelNotFound(model_id.to_string()))?;
        return Ok(if anthropic {
            anthropic_model_item(&model)
        } else {
            model
        });
    }
    if anthropic {
        Ok(anthropic_model_page(models, uri.query()))
    } else {
        Ok(json!({"object":"list","data":models}))
    }
}

fn anthropic_model_page(models: Vec<Value>, query: Option<&str>) -> Value {
    let parameters = query
        .map(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let after = parameters.get("after_id").and_then(|id| {
        models
            .iter()
            .position(|model| model.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .map(|index| index + 1)
    });
    let before = parameters.get("before_id").and_then(|id| {
        models
            .iter()
            .position(|model| model.get("id").and_then(Value::as_str) == Some(id.as_str()))
    });
    let start = after.unwrap_or(0).min(models.len());
    let end = before.unwrap_or(models.len()).max(start).min(models.len());
    let limit = parameters
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, 1000);
    let available = &models[start..end];
    let data = available
        .iter()
        .take(limit)
        .map(anthropic_model_item)
        .collect::<Vec<_>>();
    let first_id = data.first().and_then(|item| item.get("id")).cloned().unwrap_or(Value::Null);
    let last_id = data.last().and_then(|item| item.get("id")).cloned().unwrap_or(Value::Null);
    json!({
        "data":data,
        "has_more":available.len() > limit,
        "first_id":first_id,
        "last_id":last_id
    })
}

fn anthropic_model_item(model: &Value) -> Value {
    let id = model.get("id").and_then(Value::as_str).unwrap_or_default();
    let created_at = model
        .get("created_at")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            model
                .get("created")
                .and_then(Value::as_i64)
                .and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0))
                .map(|value| value.to_rfc3339())
        })
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());
    json!({
        "type":"model",
        "id":id,
        "display_name":model.get("display_name").and_then(Value::as_str).unwrap_or(id),
        "created_at":created_at
    })
}

#[derive(Clone)]
struct ModelSource {
    upstream: Upstream,
    rules: Vec<ScheduleRouteRule>,
}

async fn reachable_model_sources(
    state: &AppState,
    root: ScheduleGroup,
    max_hops: i64,
) -> anyhow::Result<Vec<ModelSource>> {
    let mut sources = Vec::new();
    let mut seen_sources = BTreeSet::new();
    let mut queue = VecDeque::from([ModelSourceTraversal {
        group: root.clone(),
        rules: Vec::new(),
        group_path: vec![root.id.clone()],
    }]);
    while let Some(entry) = queue.pop_front() {
        if fixed_targets_group(&entry.group) {
            let Some(target_group_id) = entry
                .group
                .fixed_group_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
            else {
                continue;
            };
            push_target_group(
                state,
                &mut queue,
                entry,
                &target_group_id,
                max_hops,
                "fixed",
            )
            .await?;
            continue;
        }

        if entry.group.mode != ScheduleMode::ModelMapping {
            let upstreams = if entry.group.mode == ScheduleMode::Fixed {
                fixed_upstream(state, &entry.group).await?.into_iter().collect()
            } else {
                state
                    .store
                    .schedule_group_upstreams_nested(&entry.group, max_hops)
                    .await?
            };
            for upstream in upstreams {
                push_model_source(&mut sources, &mut seen_sources, upstream, entry.rules.clone());
            }
            continue;
        }

        let rules = state.store.list_schedule_route_rules(&entry.group.id).await?;
        for rule in rules.into_iter().filter(|rule| rule.enabled) {
            match rule.target_kind {
                ScheduleRouteTargetKind::Group => {
                    let Some(target_group_id) = rule
                        .target_group_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                    else {
                        continue;
                    };
                    let mut next = entry.clone();
                    next.rules.push(rule.clone());
                    push_target_group(
                        state,
                        &mut queue,
                        next,
                        target_group_id,
                        max_hops,
                        &rule.id,
                    )
                    .await?;
                }
                ScheduleRouteTargetKind::Upstream => {
                    let Some(target_upstream_id) = rule
                        .target_upstream_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                    else {
                        continue;
                    };
                    let Some(upstream) = state.store.get_upstream(target_upstream_id).await? else {
                        continue;
                    };
                    if upstream.enabled {
                        let mut rules = entry.rules.clone();
                        rules.push(rule);
                        push_model_source(&mut sources, &mut seen_sources, upstream, rules);
                    }
                }
            }
        }
    }
    Ok(sources)
}

#[derive(Clone)]
struct ModelSourceTraversal {
    group: ScheduleGroup,
    rules: Vec<ScheduleRouteRule>,
    group_path: Vec<String>,
}

async fn push_target_group(
    state: &AppState,
    queue: &mut VecDeque<ModelSourceTraversal>,
    mut entry: ModelSourceTraversal,
    target_group_id: &str,
    max_hops: i64,
    route_id: &str,
) -> anyhow::Result<()> {
    if entry.group_path.iter().any(|id| id == target_group_id) {
        tracing::warn!(
            group_id = %entry.group.id,
            target_group_id,
            route_id,
            "model source traversal skipped cyclic group route"
        );
        return Ok(());
    }
    if entry.group_path.len() as i64 > max_hops.max(1) {
        tracing::warn!(
            group_id = %entry.group.id,
            target_group_id,
            route_id,
            max_hops,
            "model source traversal skipped route beyond max hops"
        );
        return Ok(());
    }
    if let Some(target_group) = state.store.get_schedule_group(target_group_id).await? {
        entry.group_path.push(target_group.id.clone());
        entry.group = target_group;
        queue.push_back(entry);
    }
    Ok(())
}

async fn fixed_upstream(
    state: &AppState,
    group: &ScheduleGroup,
) -> anyhow::Result<Option<Upstream>> {
    let Some(upstream_id) = group
        .fixed_upstream_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return Ok(None);
    };
    Ok(state
        .store
        .get_upstream(upstream_id)
        .await?
        .filter(|upstream| upstream.enabled))
}

fn push_model_source(
    sources: &mut Vec<ModelSource>,
    seen: &mut BTreeSet<String>,
    upstream: Upstream,
    rules: Vec<ScheduleRouteRule>,
) {
    let route_key = rules
        .iter()
        .map(|rule| rule.id.as_str())
        .collect::<Vec<_>>()
        .join("/");
    let key = format!("{}:{route_key}", upstream.id);
    if seen.insert(key) {
        sources.push(ModelSource { upstream, rules });
    }
}

fn fixed_targets_group(group: &ScheduleGroup) -> bool {
    group.mode == ScheduleMode::Fixed && group.fixed_target_kind == ScheduleRouteTargetKind::Group
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
    let mut request = state.http_for_upstream(upstream)?.get(target_url);
    if upstream.wire_api == crate::core::models::WireApi::AnthropicMessages {
        request = request.query(&[("limit", "1000")]);
    }
    let client_wire_api = headers
        .contains_key("anthropic-version")
        .then_some(crate::core::models::WireApi::AnthropicMessages);
    let request = apply_headers(state, upstream, request, headers, client_wire_api).await?;
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

fn reverse_model_path(items: Vec<Value>, rules: &[ScheduleRouteRule]) -> Vec<Value> {
    let mut items = items;
    for rule in rules.iter().rev() {
        items = reverse_route_rule_models(items, rule);
        if items.is_empty() {
            break;
        }
    }
    items
}

fn reverse_route_rule_models(items: Vec<Value>, rule: &ScheduleRouteRule) -> Vec<Value> {
    items
        .into_iter()
        .filter_map(|item| {
            let downstream_model = item.get("id").and_then(Value::as_str)?.to_string();
            let visible_model = reverse_route_rule_model(rule, &downstream_model)?;
            Some(remap_model_item(item, &visible_model, &downstream_model))
        })
        .collect()
}

fn reverse_route_rule_model(rule: &ScheduleRouteRule, downstream_model: &str) -> Option<String> {
    let pattern = rule.pattern.trim();
    if pattern.is_empty() {
        return None;
    }
    if let Some(target_model) = rule
        .target_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        let captures = glob_captures(target_model, downstream_model)?;
        let visible_model = rewrite_model_template(pattern, &captures);
        return glob_captures(pattern, &visible_model).map(|_| visible_model);
    }
    glob_captures(pattern, downstream_model).map(|_| downstream_model.to_string())
}

fn remap_model_item(item: Value, visible_model: &str, downstream_model: &str) -> Value {
    let mut model = item.as_object().cloned().unwrap_or_default();
    model.insert("id".to_string(), json!(visible_model));
    if visible_model != downstream_model {
        model.insert("target_model".to_string(), json!(downstream_model));
    }
    Value::Object(model)
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

    #[test]
    fn reverse_maps_wildcard_route_models() {
        let mut rule = ScheduleRouteRule::new("group".to_string());
        rule.pattern = "glm/*".to_string();
        rule.target_model = Some("*".to_string());
        let items = reverse_route_rule_models(
            vec![json!({"id":"glm-4.5","object":"model","owned_by":"mock"})],
            &rule,
        );

        assert_eq!(items[0]["id"], "glm/glm-4.5");
        assert_eq!(items[0]["target_model"], "glm-4.5");
        assert_eq!(items[0]["owned_by"], "mock");
    }

    #[test]
    fn reverse_route_without_rewrite_filters_unmatched_models() {
        let mut rule = ScheduleRouteRule::new("group".to_string());
        rule.pattern = "glm-*".to_string();
        let items = reverse_route_rule_models(
            vec![
                json!({"id":"glm-4.5","object":"model"}),
                json!({"id":"gpt-mock","object":"model"}),
            ],
            &rule,
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], "glm-4.5");
    }
}

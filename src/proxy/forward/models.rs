use crate::app::AppState;
use crate::core::models::{Upstream, UpstreamKind};
use crate::proxy::transform;
use axum::http::HeaderMap;
use serde_json::{Value, json};
use std::collections::BTreeSet;

use super::headers::apply_headers;

pub(super) async fn query_models(state: &AppState, headers: &HeaderMap) -> anyhow::Result<Value> {
    let group = state.store.current_schedule_group().await?;
    let upstreams = state.store.schedule_group_upstreams(&group).await?;
    let mut models = Vec::new();
    let mut seen = BTreeSet::new();
    let mut errors = Vec::new();

    for upstream in upstreams {
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

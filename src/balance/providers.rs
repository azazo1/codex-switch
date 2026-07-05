use crate::app::AppState;
use crate::core::models::{BalanceProvider, BalanceSnapshot, UpstreamKind};
use anyhow::{Context, anyhow};
use serde_json::Value;

pub fn detect_provider(base_url: &str) -> Option<BalanceProvider> {
    let url = base_url.to_lowercase();
    if url.contains("api.deepseek.com") {
        Some(BalanceProvider::DeepSeek)
    } else if url.contains("api.stepfun.ai") || url.contains("api.stepfun.com") {
        Some(BalanceProvider::StepFun)
    } else if url.contains("api.siliconflow.cn") {
        Some(BalanceProvider::SiliconFlowCn)
    } else if url.contains("api.siliconflow.com") {
        Some(BalanceProvider::SiliconFlowGlobal)
    } else if url.contains("openrouter.ai") {
        Some(BalanceProvider::OpenRouter)
    } else if url.contains("api.novita.ai") {
        Some(BalanceProvider::Novita)
    } else {
        None
    }
}

pub async fn query_and_store(
    state: &AppState,
    upstream_id: &str,
) -> anyhow::Result<BalanceSnapshot> {
    let upstream = state
        .store
        .get_upstream(upstream_id)
        .await?
        .ok_or_else(|| anyhow!("upstream not found"))?;
    if upstream.kind != UpstreamKind::RelayApiKey {
        return Err(anyhow!("balance query only supports api key upstreams"));
    }
    let api_key = state
        .credentials
        .get(&upstream.id, "api_key")
        .await?
        .ok_or_else(|| anyhow!("missing api key"))?;
    let provider = match upstream.balance_provider {
        BalanceProvider::Auto => detect_provider(&upstream.base_url)
            .ok_or_else(|| anyhow!("unsupported balance provider"))?,
        BalanceProvider::Unsupported => return Err(anyhow!("unsupported balance provider")),
        provider => provider,
    };
    let snapshot = query_provider(state, &upstream.id, provider, &api_key).await?;
    state.store.save_balance_snapshot(&snapshot).await?;
    Ok(snapshot)
}

async fn query_provider(
    state: &AppState,
    upstream_id: &str,
    provider: BalanceProvider,
    api_key: &str,
) -> anyhow::Result<BalanceSnapshot> {
    let (url, unit) = match provider {
        BalanceProvider::DeepSeek => ("https://api.deepseek.com/user/balance", "CNY"),
        BalanceProvider::StepFun => ("https://api.stepfun.com/v1/accounts", "CNY"),
        BalanceProvider::SiliconFlowCn => ("https://api.siliconflow.cn/v1/user/info", "CNY"),
        BalanceProvider::SiliconFlowGlobal => ("https://api.siliconflow.com/v1/user/info", "USD"),
        BalanceProvider::OpenRouter => ("https://openrouter.ai/api/v1/credits", "USD"),
        BalanceProvider::Novita => ("https://api.novita.ai/v3/user/balance", "USD"),
        BalanceProvider::Auto | BalanceProvider::Unsupported => {
            return Err(anyhow!("unsupported balance provider"));
        }
    };
    let response = state
        .http
        .get(url)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .with_context(|| format!("failed to query balance provider {}", provider.as_str()))?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Ok(BalanceSnapshot {
            upstream_id: upstream_id.to_string(),
            provider: provider.as_str().to_string(),
            is_valid: false,
            message: Some(format!("HTTP {status}: {body}")),
            fetched_at: chrono::Utc::now().timestamp(),
            ..BalanceSnapshot::default()
        });
    }
    Ok(parse_balance(upstream_id, provider, unit, &body))
}

fn parse_balance(
    upstream_id: &str,
    provider: BalanceProvider,
    unit: &str,
    body: &Value,
) -> BalanceSnapshot {
    let mut snapshot = BalanceSnapshot {
        upstream_id: upstream_id.to_string(),
        provider: provider.as_str().to_string(),
        unit: Some(unit.to_string()),
        is_valid: true,
        fetched_at: chrono::Utc::now().timestamp(),
        ..BalanceSnapshot::default()
    };
    match provider {
        BalanceProvider::DeepSeek => {
            snapshot.remaining = body
                .get("balance_infos")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| parse_f64_field(item, "total_balance"));
            snapshot.is_valid = body
                .get("is_available")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
        }
        BalanceProvider::StepFun => {
            snapshot.remaining = parse_f64_field(body, "balance");
        }
        BalanceProvider::SiliconFlowCn | BalanceProvider::SiliconFlowGlobal => {
            let data = body.get("data").unwrap_or(body);
            snapshot.remaining = parse_f64_field(data, "totalBalance")
                .or_else(|| parse_f64_field(data, "balance"));
        }
        BalanceProvider::OpenRouter => {
            let data = body.get("data").unwrap_or(body);
            snapshot.total = parse_f64_field(data, "total_credits");
            snapshot.used = parse_f64_field(data, "total_usage");
            snapshot.remaining = match (snapshot.total, snapshot.used) {
                (Some(total), Some(used)) => Some(total - used),
                _ => None,
            };
            snapshot.is_valid = snapshot.remaining.map(|v| v > 0.0).unwrap_or(true);
        }
        BalanceProvider::Novita => {
            snapshot.remaining = parse_f64_field(body, "availableBalance").map(|v| v / 10000.0);
            snapshot.is_valid = snapshot.remaining.map(|v| v > 0.0).unwrap_or(true);
        }
        BalanceProvider::Auto | BalanceProvider::Unsupported => {}
    }
    snapshot
}

fn parse_f64_field(obj: &Value, field: &str) -> Option<f64> {
    obj.get(field)
        .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_balance_provider() {
        assert_eq!(
            detect_provider("https://api.deepseek.com/v1"),
            Some(BalanceProvider::DeepSeek)
        );
        assert_eq!(
            detect_provider("https://openrouter.ai/api/v1"),
            Some(BalanceProvider::OpenRouter)
        );
    }

    #[test]
    fn parses_openrouter_balance() {
        let snapshot = parse_balance(
            "u1",
            BalanceProvider::OpenRouter,
            "USD",
            &json!({"data":{"total_credits":10.0,"total_usage":3.5}}),
        );
        assert_eq!(snapshot.remaining, Some(6.5));
        assert_eq!(snapshot.total, Some(10.0));
    }
}

use crate::app::AppState;
use crate::core::models::{ModelPrice, TokenUsage};
use anyhow::Context;
use serde_json::Value;
use std::time::Duration;

const MODELS_DEV_API_URL: &str = "https://models.dev/api.json";

#[derive(Debug, Clone, Copy, Default)]
pub struct UsageCost {
    pub input_usd: f64,
    pub cached_input_usd: f64,
    pub cache_write_usd: f64,
    pub output_usd: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PriceFetchSummary {
    pub fetched: bool,
    pub count: i64,
}

impl UsageCost {
    pub fn total_usd(self) -> f64 {
        self.input_usd + self.cached_input_usd + self.cache_write_usd + self.output_usd
    }
}

pub async fn fetch_price_cache(state: &AppState) -> anyhow::Result<usize> {
    tracing::info!(url = MODELS_DEV_API_URL, "fetching model price cache");
    let value = state
        .http
        .get(MODELS_DEV_API_URL)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .context("failed to request models.dev catalog")?
        .error_for_status()
        .context("models.dev returned an error status")?
        .json::<Value>()
        .await
        .context("failed to parse models.dev catalog")?;
    let prices = parse_models_dev_prices(&value);
    let count = prices.len();
    state.store.replace_model_prices(&prices).await?;
    tracing::info!(count, "model price cache fetched");
    Ok(count)
}

pub async fn fetch_price_cache_once(state: &AppState) -> anyhow::Result<PriceFetchSummary> {
    let count = state.store.model_price_count().await?;
    if count > 0 {
        return Ok(PriceFetchSummary {
            fetched: false,
            count,
        });
    }
    let count = fetch_price_cache(state).await? as i64;
    Ok(PriceFetchSummary {
        fetched: true,
        count,
    })
}

pub fn estimate_usage_cost(usage: &TokenUsage, price: &ModelPrice) -> UsageCost {
    let input_price = price.input_usd_per_million.unwrap_or(0.0);
    let cached_price = price.cached_input_usd_per_million.unwrap_or(input_price);
    let cache_write_price = price.cache_write_usd_per_million.unwrap_or(input_price);
    let output_price = price.output_usd_per_million.unwrap_or(0.0);
    UsageCost {
        input_usd: usd_for_tokens(usage.uncached_input_tokens(), input_price),
        cached_input_usd: usd_for_tokens(usage.cached_input_tokens(), cached_price),
        cache_write_usd: usd_for_tokens(usage.cache_creation_tokens, cache_write_price),
        output_usd: usd_for_tokens(usage.output_tokens, output_price),
    }
}

fn usd_for_tokens(tokens: i64, usd_per_million: f64) -> f64 {
    tokens.max(0) as f64 * usd_per_million / 1_000_000.0
}

fn parse_models_dev_prices(value: &Value) -> Vec<ModelPrice> {
    let Some(providers) = value.as_object() else {
        return Vec::new();
    };
    let fetched_at = chrono::Utc::now().timestamp();
    let mut prices = Vec::new();
    for (provider_id, provider) in providers {
        let provider_name = provider
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(provider_id);
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (model_key, model) in models {
            let Some(cost) = model.get("cost") else {
                continue;
            };
            let input = cost_number(cost, "input");
            let output = cost_number(cost, "output");
            let cache_read = cost_number(cost, "cache_read");
            let cache_write = cost_number(cost, "cache_write");
            if input.is_none() && output.is_none() && cache_read.is_none() && cache_write.is_none()
            {
                continue;
            }
            let model_id = model
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(model_key)
                .to_string();
            let model_name = model
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&model_id)
                .to_string();
            prices.push(ModelPrice {
                provider_id: provider_id.to_string(),
                provider_name: provider_name.to_string(),
                model_id,
                model_name,
                input_usd_per_million: input,
                cached_input_usd_per_million: cache_read,
                cache_write_usd_per_million: cache_write,
                output_usd_per_million: output,
                currency: "USD".to_string(),
                source: MODELS_DEV_API_URL.to_string(),
                official: is_official_provider(provider_id),
                fetched_at,
                raw_json: serde_json::to_string(model).ok(),
            });
        }
    }
    prices
}

fn cost_number(cost: &Value, key: &str) -> Option<f64> {
    cost.get(key).and_then(Value::as_f64)
}

fn is_official_provider(provider_id: &str) -> bool {
    matches!(
        provider_id,
        "openai"
            | "anthropic"
            | "google"
            | "xai"
            | "deepseek"
            | "mistral"
            | "cohere"
            | "moonshotai"
            | "alibaba"
            | "alibaba-cn"
            | "zai"
            | "zai-cn"
    )
}

#[cfg(test)]
mod tests {
    use super::{estimate_usage_cost, parse_models_dev_prices};
    use crate::core::models::{ModelPrice, TokenUsage};
    use serde_json::json;

    #[test]
    fn parses_models_dev_costs() {
        let prices = parse_models_dev_prices(&json!({
            "openai": {
                "name": "OpenAI",
                "models": {
                    "gpt-5-codex": {
                        "id": "gpt-5-codex",
                        "name": "GPT-5-Codex",
                        "cost": {"input": 1.25, "output": 10, "cache_read": 0.125}
                    }
                }
            }
        }));
        assert_eq!(prices.len(), 1);
        assert_eq!(prices[0].provider_id, "openai");
        assert_eq!(prices[0].model_id, "gpt-5-codex");
        assert_eq!(prices[0].cached_input_usd_per_million, Some(0.125));
        assert!(prices[0].official);
    }

    #[test]
    fn estimates_cost_from_split_usage() {
        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 200,
            cache_read_tokens: 400,
            cache_creation_tokens: 100,
            total_tokens: 1300,
        };
        let price = ModelPrice {
            input_usd_per_million: Some(1.0),
            cached_input_usd_per_million: Some(0.1),
            cache_write_usd_per_million: Some(2.0),
            output_usd_per_million: Some(10.0),
            ..Default::default()
        };
        let cost = estimate_usage_cost(&usage, &price);
        let expected = 500.0 / 1_000_000.0
            + 400.0 * 0.1 / 1_000_000.0
            + 100.0 * 2.0 / 1_000_000.0
            + 200.0 * 10.0 / 1_000_000.0;
        assert!((cost.total_usd() - expected).abs() < 0.0000001);
    }

    #[test]
    fn estimates_cost_when_input_is_already_uncached() {
        let usage = TokenUsage {
            input_tokens: 500,
            output_tokens: 200,
            cache_read_tokens: 400,
            cache_creation_tokens: 200,
            total_tokens: 1300,
        };
        let price = ModelPrice {
            input_usd_per_million: Some(1.0),
            cached_input_usd_per_million: Some(0.1),
            cache_write_usd_per_million: Some(2.0),
            output_usd_per_million: Some(10.0),
            ..Default::default()
        };
        let cost = estimate_usage_cost(&usage, &price);
        let expected = 500.0 / 1_000_000.0
            + 400.0 * 0.1 / 1_000_000.0
            + 200.0 * 2.0 / 1_000_000.0
            + 200.0 * 10.0 / 1_000_000.0;
        assert!((cost.total_usd() - expected).abs() < 0.0000001);
    }
}

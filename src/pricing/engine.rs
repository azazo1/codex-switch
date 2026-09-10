use super::script::{lookup_price, CostEstimateEnv, CostEstimateInput, CostUpstream, PricingScript};
use super::estimate_usage_cost;
use crate::core::models::RequestLog;
use crate::storage::{Store, UpstreamPricingScript};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 全局脚本和各上游脚本的编译缓存. 估算时按 上游 > 全局 > 内置 取值.
#[derive(Clone)]
pub struct PricingEngine {
    global: PricingScript,
    upstreams: Arc<Mutex<HashMap<String, PricingScript>>>,
}

impl PricingEngine {
    #[allow(dead_code)]
    pub fn disabled() -> Self {
        Self::from_global(PricingScript::disabled())
    }

    pub fn from_global(global: PricingScript) -> Self {
        Self {
            global,
            upstreams: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn load(store: &Store) -> anyhow::Result<Self> {
        let engine = Self::from_global(PricingScript::load(store).await?);
        engine.reload_upstreams(store).await?;
        Ok(engine)
    }

    pub fn global(&self) -> &PricingScript {
        &self.global
    }

    pub fn upstream_script(&self, id: &str) -> Option<PricingScript> {
        self.lock_upstreams().get(id).cloned()
    }

    pub fn apply_upstream(&self, id: &str, enabled: bool, source: String) {
        let mut upstreams = self.lock_upstreams();
        let script = upstreams
            .entry(id.to_string())
            .or_insert_with(PricingScript::disabled);
        script.apply(enabled, source);
        tracing::info!(upstream_id = %id, "upstream pricing script applied");
    }

    pub fn remove_upstream(&self, id: &str) {
        self.lock_upstreams().remove(id);
    }

    pub async fn persist_upstream(&self, store: &Store, id: &str) -> anyhow::Result<()> {
        let script = self
            .upstream_script(id)
            .unwrap_or_else(PricingScript::disabled);
        store
            .save_upstream_pricing_script(&UpstreamPricingScript {
                upstream_id: id.to_string(),
                enabled: script.enabled(),
                source: script.source(),
            })
            .await
    }

    pub async fn reload_upstreams(&self, store: &Store) -> anyhow::Result<()> {
        let records = store.list_upstream_pricing_scripts().await?;
        let mut upstreams = HashMap::new();
        for record in records {
            let script = PricingScript::disabled();
            script.apply(record.enabled, record.source);
            upstreams.insert(record.upstream_id, script);
        }
        *self.lock_upstreams() = upstreams;
        Ok(())
    }

    pub fn status_label(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(label) = self.global.status_label() {
            parts.push(if label == "脚本已启用" {
                "全局脚本已启用".to_string()
            } else {
                "全局脚本编译失败, 已回退".to_string()
            });
        }
        let (ready, failed) = self.upstream_status_counts();
        if ready > 0 {
            parts.push(format!("{ready} 个上游脚本已启用"));
        }
        if failed > 0 {
            parts.push(format!("{failed} 个上游脚本编译失败"));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(", "))
        }
    }

    fn upstream_status_counts(&self) -> (usize, usize) {
        let mut ready = 0;
        let mut failed = 0;
        for script in self.lock_upstreams().values() {
            if script.is_ready() {
                ready += 1;
            } else if script.enabled() {
                failed += 1;
            }
        }
        (ready, failed)
    }

    fn lock_upstreams(&self) -> std::sync::MutexGuard<'_, HashMap<String, PricingScript>> {
        self.upstreams
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }
}

pub async fn estimate_request_cost(
    store: &Store,
    engine: &PricingEngine,
    env: &CostEstimateEnv,
    input: CostEstimateInput<'_>,
) -> Option<f64> {
    let price = lookup_price(store, input.model).await;
    let builtin = price
        .as_ref()
        .map(|price| estimate_usage_cost(input.usage, price).total_usd());
    let multiplier = input
        .upstream
        .map(|upstream| upstream.multiplier)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(1.0);
    let fallback = builtin.map(|value| value * multiplier);

    if let Some(upstream) = input.upstream
        && let Some(script) = engine.upstream_script(&upstream.id)
        && let Some(cost) = eval_layer(
            &script,
            env,
            input,
            price.as_ref(),
            builtin,
            input.model,
            Some(upstream.id.as_str()),
        )
    {
        return Some(cost);
    }

    if let Some(cost) = eval_layer(
        engine.global(),
        env,
        input,
        price.as_ref(),
        builtin,
        input.model,
        None,
    ) {
        return Some(cost);
    }

    fallback
}

pub async fn attach_estimated_cost(
    store: &Store,
    engine: &PricingEngine,
    log: &mut RequestLog,
) {
    let mut env = match super::script::load_cost_estimate_env(store).await {
        Ok(env) => env,
        Err(err) => {
            tracing::warn!(error = %err, "failed to load pricing env");
            CostEstimateEnv::now(None)
        }
    };
    if let Some(ts) = log.ts {
        env.now = ts;
    }
    let owned_upstream = resolve_cost_upstream(store, log).await;
    log.estimated_cost_usd = estimate_request_cost(
        store,
        engine,
        &env,
        CostEstimateInput {
            model: log.model.as_deref(),
            target_model: log.target_model.as_deref(),
            usage: &log.usage,
            upstream: owned_upstream.as_ref(),
        },
    )
    .await;
}

fn eval_layer(
    script: &PricingScript,
    env: &CostEstimateEnv,
    input: CostEstimateInput<'_>,
    price: Option<&crate::core::models::ModelPrice>,
    builtin: Option<f64>,
    model: Option<&str>,
    upstream_id: Option<&str>,
) -> Option<f64> {
    if !script.is_ready() {
        return None;
    }
    match script.eval_estimate(env, input, price, builtin) {
        Ok(Some(value)) => Some(value),
        Ok(None) => None,
        Err(error) => {
            script.record_runtime_error(error.clone());
            tracing::warn!(
                error = %error,
                model = model.unwrap_or(""),
                upstream_id = upstream_id.unwrap_or(""),
                "pricing script failed, trying next layer"
            );
            None
        }
    }
}

async fn resolve_cost_upstream(store: &Store, log: &RequestLog) -> Option<CostUpstream> {
    let id = log.upstream_id.as_deref()?;
    match store.get_upstream(id).await {
        Ok(Some(upstream)) => Some(CostUpstream::from_upstream(&upstream)),
        Ok(None) => Some(CostUpstream {
            id: id.to_string(),
            name: log.upstream_name.clone().unwrap_or_default(),
            kind: String::new(),
            base_url: String::new(),
            multiplier: 1.0,
        }),
        Err(err) => {
            tracing::warn!(error = %err, upstream_id = id, "failed to load upstream for pricing");
            Some(CostUpstream {
                id: id.to_string(),
                name: log.upstream_name.clone().unwrap_or_default(),
                kind: String::new(),
                base_url: String::new(),
                multiplier: 1.0,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{BalanceProvider, ModelPrice, TokenUsage, Upstream, WireApi};
    use chrono::{TimeZone, Utc};

    fn usage() -> TokenUsage {
        TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            total_tokens: 2_000_000,
            ..Default::default()
        }
    }

    fn env_at(hour_utc: u32) -> CostEstimateEnv {
        CostEstimateEnv {
            now: Utc.with_ymd_and_hms(2024, 6, 15, hour_utc, 30, 0).unwrap(),
            fx: None,
        }
    }

    async fn test_store() -> Store {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-pricing-engine-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        Store::open(path).await.unwrap()
    }

    async fn seed_price(store: &Store) {
        store
            .replace_model_prices(&[ModelPrice {
                provider_id: "openai".to_string(),
                provider_name: "OpenAI".to_string(),
                model_id: "gpt-test".to_string(),
                model_name: "GPT Test".to_string(),
                input_usd_per_million: Some(1.0),
                output_usd_per_million: Some(2.0),
                currency: "USD".to_string(),
                source: "test".to_string(),
                official: true,
                ..Default::default()
            }])
            .await
            .unwrap();
    }

    fn cost_upstream(id: &str, multiplier: f64) -> CostUpstream {
        CostUpstream {
            id: id.to_string(),
            name: "relay".to_string(),
            kind: "relay_api_key".to_string(),
            base_url: "https://example.test".to_string(),
            multiplier,
        }
    }

    fn input<'a>(
        usage: &'a TokenUsage,
        upstream: Option<&'a CostUpstream>,
    ) -> CostEstimateInput<'a> {
        CostEstimateInput {
            model: Some("gpt-test"),
            target_model: None,
            usage,
            upstream,
        }
    }

    #[tokio::test]
    async fn upstream_script_overrides_global() {
        let store = test_store().await;
        seed_price(&store).await;
        let engine = PricingEngine::disabled();
        engine.global().apply(true, "fn estimate(ctx) { 9.0 }".to_string());
        engine.apply_upstream("u1", true, "fn estimate(ctx) { 1.5 }".to_string());
        let usage = usage();
        let upstream = cost_upstream("u1", 1.5);
        let cost = estimate_request_cost(
            &store,
            &engine,
            &env_at(11),
            input(&usage, Some(&upstream)),
        )
        .await
        .unwrap();
        assert!((cost - 1.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn upstream_unit_falls_through_to_global() {
        let store = test_store().await;
        seed_price(&store).await;
        let engine = PricingEngine::disabled();
        engine.global().apply(true, "fn estimate(ctx) { 8.0 }".to_string());
        engine.apply_upstream("u1", true, "fn estimate(ctx) { () }".to_string());
        let usage = usage();
        let upstream = cost_upstream("u1", 1.0);
        let cost = estimate_request_cost(
            &store,
            &engine,
            &env_at(11),
            input(&usage, Some(&upstream)),
        )
        .await
        .unwrap();
        assert!((cost - 8.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn upstream_runtime_error_falls_through_to_global() {
        let store = test_store().await;
        seed_price(&store).await;
        let engine = PricingEngine::disabled();
        engine.global().apply(true, "fn estimate(ctx) { 8.0 }".to_string());
        engine.apply_upstream("u1", true, "fn estimate(ctx) { ctx.missing }".to_string());
        let usage = usage();
        let upstream = cost_upstream("u1", 1.0);
        let cost = estimate_request_cost(
            &store,
            &engine,
            &env_at(11),
            input(&usage, Some(&upstream)),
        )
        .await
        .unwrap();
        assert!((cost - 8.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn missing_upstream_script_uses_global() {
        let store = test_store().await;
        seed_price(&store).await;
        let engine = PricingEngine::disabled();
        engine.global().apply(true, "fn estimate(ctx) { 7.0 }".to_string());
        let usage = usage();
        let upstream = cost_upstream("u1", 1.0);
        let cost = estimate_request_cost(
            &store,
            &engine,
            &env_at(11),
            input(&usage, Some(&upstream)),
        )
        .await
        .unwrap();
        assert!((cost - 7.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn both_layers_skip_uses_builtin_multiplier() {
        let store = test_store().await;
        seed_price(&store).await;
        let engine = PricingEngine::disabled();
        engine.global().apply(true, "fn estimate(ctx) { () }".to_string());
        engine.apply_upstream("u1", true, "fn estimate(ctx) { () }".to_string());
        let usage = usage();
        let upstream = cost_upstream("u1", 1.5);
        let cost = estimate_request_cost(
            &store,
            &engine,
            &env_at(11),
            input(&usage, Some(&upstream)),
        )
        .await
        .unwrap();
        assert!((cost - 4.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn persist_and_load_upstream_script() {
        let store = test_store().await;
        let upstream = Upstream::new_relay(
            "relay".to_string(),
            "https://example.test".to_string(),
            WireApi::Responses,
            true,
            BalanceProvider::Unsupported,
        );
        store.save_upstream(&upstream).await.unwrap();
        let engine = PricingEngine::disabled();
        engine.apply_upstream(
            &upstream.id,
            true,
            "fn estimate(ctx) { 2.0 }".to_string(),
        );
        engine.persist_upstream(&store, &upstream.id).await.unwrap();
        let loaded = PricingEngine::load(&store).await.unwrap();
        let script = loaded.upstream_script(&upstream.id).unwrap();
        assert!(script.is_ready());
        assert_eq!(script.source(), "fn estimate(ctx) { 2.0 }");
    }
}

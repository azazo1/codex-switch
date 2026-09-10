use super::fx::UsdCnyRate;
use super::{estimate_usage_cost, usd_for_tokens};
use crate::core::models::{ModelPrice, RequestLog, TokenUsage, Upstream};
use crate::storage::Store;
use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use rhai::{CustomType, Dynamic, Engine, Scope, AST};
use std::sync::{Arc, Mutex};

pub const SETTING_PRICING_SCRIPT_ENABLED: &str = "pricing_script_enabled";
pub const SETTING_PRICING_SCRIPT: &str = "pricing_script";

pub const DEFAULT_SCRIPT: &str = r#"fn estimate(ctx) {
  // 返回值必须是 USD. 人民币报价先算 CNY 再除以汇率.
  let fx = if ctx.fx != () { ctx.fx.usd_cny } else { 7.2 };

  if ctx.model.contains("deepseek") {
    let cny = usd_for_tokens(ctx.usage.uncached_input_tokens, 2.0)
      + usd_for_tokens(ctx.usage.output_tokens, 8.0);
    return cny / fx;
  }

  let cost = if ctx.builtin != () { ctx.builtin * ctx.multiplier } else { () };
  if cost != () && ctx.local.hour >= 19 && ctx.local.hour < 23 {
    return cost * 1.2;
  }
  cost
}
"#;

const MAX_OPERATIONS: u64 = 10_000;
const MAX_CALL_LEVELS: usize = 32;
const MAX_EXPR_DEPTH: usize = 32;
const MAX_STRING_SIZE: usize = 4_096;
const MAX_ARRAY_SIZE: usize = 256;
const MAX_MAP_SIZE: usize = 256;
const MAX_SCRIPT_LOGS: usize = 32;

/// 脚本里的日历时间. Rhai 自带 Instant 只是单调时钟, 没有年月日小时.
#[derive(Debug, Clone, CustomType)]
#[rhai_type(name = "DateTime")]
pub struct ScriptDateTime {
    #[rhai_type(readonly)]
    pub unix: i64,
    #[rhai_type(readonly)]
    pub year: i64,
    #[rhai_type(readonly)]
    pub month: i64,
    #[rhai_type(readonly)]
    pub day: i64,
    #[rhai_type(readonly)]
    pub hour: i64,
    #[rhai_type(readonly)]
    pub minute: i64,
    #[rhai_type(readonly)]
    pub second: i64,
    /// ISO 星期: 1 = 周一, 7 = 周日.
    #[rhai_type(readonly)]
    pub weekday: i64,
}

#[derive(Debug, Clone, CustomType)]
#[rhai_type(name = "FxRate")]
struct ScriptFx {
    #[rhai_type(readonly)]
    usd_cny: f64,
    #[rhai_type(readonly)]
    fetched_at: i64,
}

#[derive(Debug, Clone, CustomType)]
#[rhai_type(name = "Usage")]
struct ScriptUsage {
    #[rhai_type(readonly)]
    input_tokens: i64,
    #[rhai_type(readonly)]
    output_tokens: i64,
    #[rhai_type(readonly)]
    cache_read_tokens: i64,
    #[rhai_type(readonly)]
    cache_creation_tokens: i64,
    #[rhai_type(readonly)]
    total_tokens: i64,
    #[rhai_type(readonly)]
    uncached_input_tokens: i64,
}

#[derive(Debug, Clone, CustomType)]
#[rhai_type(name = "Price")]
struct ScriptPrice {
    #[rhai_type(readonly)]
    model_id: String,
    #[rhai_type(readonly)]
    provider_id: String,
    #[rhai_type(readonly)]
    official: bool,
    #[rhai_type(readonly)]
    input: Dynamic,
    #[rhai_type(readonly)]
    cached_input: Dynamic,
    #[rhai_type(readonly)]
    cache_write: Dynamic,
    #[rhai_type(readonly)]
    output: Dynamic,
}

#[derive(Debug, Clone, CustomType)]
#[rhai_type(name = "Upstream")]
struct ScriptUpstream {
    #[rhai_type(readonly)]
    id: String,
    #[rhai_type(readonly)]
    name: String,
    #[rhai_type(readonly)]
    kind: String,
    #[rhai_type(readonly)]
    multiplier: f64,
}

#[derive(Debug, Clone, CustomType)]
#[rhai_type(name = "EstimateCtx")]
struct ScriptCtx {
    #[rhai_type(readonly)]
    model: String,
    #[rhai_type(readonly)]
    target_model: Dynamic,
    #[rhai_type(readonly)]
    usage: ScriptUsage,
    #[rhai_type(readonly)]
    price: Dynamic,
    #[rhai_type(readonly)]
    upstream: Dynamic,
    #[rhai_type(readonly)]
    builtin: Dynamic,
    #[rhai_type(readonly)]
    multiplier: f64,
    #[rhai_type(readonly)]
    fx: Dynamic,
    #[rhai_type(readonly)]
    now: i64,
    #[rhai_type(readonly)]
    utc: ScriptDateTime,
    #[rhai_type(readonly)]
    local: ScriptDateTime,
}

#[derive(Debug, Clone)]
pub struct CostUpstream {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub multiplier: f64,
}

impl CostUpstream {
    pub fn from_upstream(upstream: &Upstream) -> Self {
        Self {
            id: upstream.id.clone(),
            name: upstream.name.clone(),
            kind: upstream.kind.as_str().to_string(),
            multiplier: upstream.price_multiplier,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CostEstimateEnv {
    pub now: DateTime<Utc>,
    pub fx: Option<UsdCnyRate>,
}

impl CostEstimateEnv {
    pub fn now(fx: Option<UsdCnyRate>) -> Self {
        Self {
            now: Utc::now(),
            fx,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CostEstimateInput<'a> {
    pub model: Option<&'a str>,
    pub target_model: Option<&'a str>,
    pub usage: &'a TokenUsage,
    pub upstream: Option<&'a CostUpstream>,
}

#[derive(Debug, Clone, Default)]
pub struct PricingPreview {
    pub compile_error: Option<String>,
    pub runtime_error: Option<String>,
    pub script_cost: Option<f64>,
    pub builtin_cost: Option<f64>,
    pub logs: Vec<String>,
}

#[derive(Clone, Copy)]
enum ScriptLogLevel {
    Info,
    Warn,
    Debug,
}

#[derive(Clone, Default)]
struct ScriptLogSink {
    lines: Arc<Mutex<Vec<String>>>,
}

impl ScriptLogSink {
    fn push(&self, level: ScriptLogLevel, message: &str) {
        let line = format!("{} {message}", script_log_level_name(level));
        let mut lines = self.lines.lock().unwrap_or_else(|err| err.into_inner());
        if lines.len() >= MAX_SCRIPT_LOGS {
            lines.remove(0);
        }
        lines.push(line);
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.lines.lock().unwrap_or_else(|err| err.into_inner()))
    }
}

struct PricingInner {
    enabled: bool,
    source: String,
    ast: Option<AST>,
    compile_error: Option<String>,
    runtime_error: Option<String>,
    last_logs: Vec<String>,
}

#[derive(Clone)]
pub struct PricingScript {
    inner: Arc<Mutex<PricingInner>>,
}

impl PricingScript {
    pub fn disabled() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PricingInner {
                enabled: false,
                source: String::new(),
                ast: None,
                compile_error: None,
                runtime_error: None,
                last_logs: Vec::new(),
            })),
        }
    }

    pub async fn load(store: &Store) -> anyhow::Result<Self> {
        let enabled = store
            .get_setting(SETTING_PRICING_SCRIPT_ENABLED)
            .await?
            .as_deref()
            == Some("true");
        let source = store
            .get_setting(SETTING_PRICING_SCRIPT)
            .await?
            .unwrap_or_default();
        let script = Self::disabled();
        script.apply(enabled, source);
        Ok(script)
    }

    pub async fn persist(&self, store: &Store) -> anyhow::Result<()> {
        let (enabled, source) = {
            let inner = self.lock();
            (inner.enabled, inner.source.clone())
        };
        store
            .set_setting(
                SETTING_PRICING_SCRIPT_ENABLED,
                if enabled { "true" } else { "false" },
            )
            .await?;
        store.set_setting(SETTING_PRICING_SCRIPT, &source).await?;
        Ok(())
    }

    pub fn apply(&self, enabled: bool, source: String) {
        let compiled = compile_source(&source);
        let mut inner = self.lock();
        inner.enabled = enabled;
        inner.source = source;
        inner.runtime_error = None;
        inner.last_logs.clear();
        match compiled {
            Ok(ast) => {
                inner.ast = ast;
                inner.compile_error = None;
                if enabled {
                    tracing::info!("pricing script compiled");
                }
            }
            Err(error) => {
                inner.ast = None;
                inner.compile_error = Some(error.clone());
                tracing::warn!(error = %error, "pricing script compile failed");
            }
        }
    }

    pub fn enabled(&self) -> bool {
        self.lock().enabled
    }

    pub fn source(&self) -> String {
        self.lock().source.clone()
    }

    pub fn compile_error(&self) -> Option<String> {
        self.lock().compile_error.clone()
    }

    pub fn runtime_error(&self) -> Option<String> {
        self.lock().runtime_error.clone()
    }

    pub fn last_logs(&self) -> Vec<String> {
        self.lock().last_logs.clone()
    }

    pub fn is_ready(&self) -> bool {
        let inner = self.lock();
        inner.enabled && inner.ast.is_some()
    }

    pub fn status_label(&self) -> Option<&'static str> {
        let inner = self.lock();
        if !inner.enabled {
            return None;
        }
        if inner.ast.is_some() {
            Some("脚本已启用")
        } else {
            Some("脚本编译失败, 已回退")
        }
    }

    fn eval_estimate(
        &self,
        env: &CostEstimateEnv,
        input: CostEstimateInput<'_>,
        price: Option<&ModelPrice>,
        builtin: Option<f64>,
    ) -> Result<Option<f64>, String> {
        let ast = {
            let inner = self.lock();
            if !inner.enabled {
                return Ok(None);
            }
            let Some(ast) = inner.ast.clone() else {
                return Ok(None);
            };
            ast
        };
        let ctx = build_ctx(env, input, price, builtin);
        let sink = ScriptLogSink::default();
        let engine = build_engine_with_sink(Some(sink.clone()));
        let mut scope = Scope::new();
        let result = engine.call_fn::<Dynamic>(&mut scope, &ast, "estimate", (ctx,));
        self.append_logs(sink.take());
        let result = result.map_err(|err| err.to_string())?;
        dynamic_to_cost(result)
    }

    fn append_logs(&self, lines: Vec<String>) {
        if lines.is_empty() {
            return;
        }
        let mut inner = self.lock();
        inner.last_logs.extend(lines);
        let extra = inner.last_logs.len().saturating_sub(MAX_SCRIPT_LOGS);
        if extra > 0 {
            inner.last_logs.drain(..extra);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PricingInner> {
        self.inner.lock().unwrap_or_else(|err| err.into_inner())
    }
}

pub fn compile_check(source: &str) -> Result<(), String> {
    compile_source(source).map(|_| ())
}

pub async fn load_cost_estimate_env(store: &Store) -> anyhow::Result<CostEstimateEnv> {
    Ok(CostEstimateEnv {
        now: Utc::now(),
        fx: super::fx::load_usd_cny_rate_from_store(store).await?,
    })
}

pub async fn estimate_request_cost(
    store: &Store,
    script: &PricingScript,
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
    if !script.is_ready() {
        return fallback;
    }
    match script.eval_estimate(env, input, price.as_ref(), builtin) {
        Ok(Some(value)) => Some(value),
        Ok(None) => fallback,
        Err(error) => {
            script.lock().runtime_error = Some(error.clone());
            tracing::warn!(
                error = %error,
                model = input.model.unwrap_or(""),
                "pricing script failed, falling back"
            );
            fallback
        }
    }
}

pub async fn attach_estimated_cost(store: &Store, script: &PricingScript, log: &mut RequestLog) {
    let mut env = match load_cost_estimate_env(store).await {
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
        script,
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

pub async fn preview_estimate(
    store: &Store,
    source: &str,
    env: &CostEstimateEnv,
    input: CostEstimateInput<'_>,
) -> PricingPreview {
    let price = lookup_price(store, input.model).await;
    let builtin = price
        .as_ref()
        .map(|price| estimate_usage_cost(input.usage, price).total_usd());
    let multiplier = input
        .upstream
        .map(|upstream| upstream.multiplier)
        .unwrap_or(1.0);
    let builtin_cost = builtin.map(|value| value * multiplier);
    let ast = match compile_source(source) {
        Ok(Some(ast)) => ast,
        Ok(None) => {
            return PricingPreview {
                builtin_cost,
                ..PricingPreview::default()
            };
        }
        Err(error) => {
            return PricingPreview {
                compile_error: Some(error),
                builtin_cost,
                ..PricingPreview::default()
            };
        }
    };
    let ctx = build_ctx(env, input, price.as_ref(), builtin);
    let sink = ScriptLogSink::default();
    let engine = build_engine_with_sink(Some(sink.clone()));
    let mut scope = Scope::new();
    let result = engine.call_fn::<Dynamic>(&mut scope, &ast, "estimate", (ctx,));
    let logs = sink.take();
    match result {
        Ok(result) => match dynamic_to_cost(result) {
            Ok(script_cost) => PricingPreview {
                script_cost,
                builtin_cost,
                logs,
                ..PricingPreview::default()
            },
            Err(error) => PricingPreview {
                runtime_error: Some(error),
                builtin_cost,
                logs,
                ..PricingPreview::default()
            },
        },
        Err(err) => PricingPreview {
            runtime_error: Some(err.to_string()),
            builtin_cost,
            logs,
            ..PricingPreview::default()
        },
    }
}

async fn lookup_price(store: &Store, model: Option<&str>) -> Option<ModelPrice> {
    let model = model.filter(|value| !value.is_empty())?;
    match store.find_model_price(model).await {
        Ok(price) => price,
        Err(err) => {
            tracing::warn!(error = %err, model, "failed to lookup model price");
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
            multiplier: 1.0,
        }),
        Err(err) => {
            tracing::warn!(error = %err, upstream_id = id, "failed to load upstream for pricing");
            Some(CostUpstream {
                id: id.to_string(),
                name: log.upstream_name.clone().unwrap_or_default(),
                kind: String::new(),
                multiplier: 1.0,
            })
        }
    }
}

fn compile_source(source: &str) -> Result<Option<AST>, String> {
    if source.trim().is_empty() {
        return Ok(None);
    }
    let engine = build_engine();
    let ast = engine.compile(source).map_err(|err| err.to_string())?;
    let has_estimate = ast
        .iter_functions()
        .any(|func| func.name == "estimate" && func.params.len() == 1);
    if !has_estimate {
        return Err("脚本必须提供 fn estimate(ctx)".to_string());
    }
    Ok(Some(ast))
}

fn build_engine() -> Engine {
    build_engine_with_sink(None)
}

fn build_engine_with_sink(sink: Option<ScriptLogSink>) -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(MAX_OPERATIONS);
    engine.set_max_call_levels(MAX_CALL_LEVELS);
    engine.set_max_expr_depths(MAX_EXPR_DEPTH, MAX_EXPR_DEPTH);
    engine.set_max_string_size(MAX_STRING_SIZE);
    engine.set_max_array_size(MAX_ARRAY_SIZE);
    engine.set_max_map_size(MAX_MAP_SIZE);
    engine.disable_symbol("sleep");
    engine.disable_symbol("eval");
    let print_sink = sink.clone();
    engine.on_print(move |message| {
        emit_script_log(&print_sink, ScriptLogLevel::Info, message);
    });
    let debug_sink = sink.clone();
    engine.on_debug(move |message, _source, _pos| {
        emit_script_log(&debug_sink, ScriptLogLevel::Debug, message);
    });
    engine.build_type::<ScriptDateTime>();
    engine.build_type::<ScriptFx>();
    engine.build_type::<ScriptUsage>();
    engine.build_type::<ScriptPrice>();
    engine.build_type::<ScriptUpstream>();
    engine.build_type::<ScriptCtx>();
    engine.register_fn("usd_for_tokens", usd_for_tokens_i64_f64);
    engine.register_fn("usd_for_tokens", usd_for_tokens_i64_i64);
    engine.register_fn("usd_for_tokens", usd_for_tokens_f64_f64);
    register_script_log_fns(&mut engine, sink);
    engine
}

fn register_script_log_fns(engine: &mut Engine, sink: Option<ScriptLogSink>) {
    let log_sink = sink.clone();
    engine.register_fn("log", move |message: Dynamic| {
        emit_script_log(&log_sink, ScriptLogLevel::Info, &dynamic_to_text(&message));
    });
    let warn_sink = sink;
    engine.register_fn("warn", move |message: Dynamic| {
        emit_script_log(&warn_sink, ScriptLogLevel::Warn, &dynamic_to_text(&message));
    });
}

fn emit_script_log(sink: &Option<ScriptLogSink>, level: ScriptLogLevel, message: &str) {
    match level {
        ScriptLogLevel::Info => tracing::info!("{message}"),
        ScriptLogLevel::Warn => tracing::warn!("{message}"),
        ScriptLogLevel::Debug => tracing::debug!("{message}"),
    }
    if let Some(sink) = sink {
        sink.push(level, message);
    }
}

fn script_log_level_name(level: ScriptLogLevel) -> &'static str {
    match level {
        ScriptLogLevel::Info => "info",
        ScriptLogLevel::Warn => "warn",
        ScriptLogLevel::Debug => "debug",
    }
}

fn dynamic_to_text(value: &Dynamic) -> String {
    value.to_string()
}

fn usd_for_tokens_i64_f64(tokens: i64, usd_per_million: f64) -> f64 {
    usd_for_tokens(tokens, usd_per_million)
}

fn usd_for_tokens_i64_i64(tokens: i64, usd_per_million: i64) -> f64 {
    usd_for_tokens(tokens, usd_per_million as f64)
}

fn usd_for_tokens_f64_f64(tokens: f64, usd_per_million: f64) -> f64 {
    usd_for_tokens(tokens as i64, usd_per_million)
}

fn build_ctx(
    env: &CostEstimateEnv,
    input: CostEstimateInput<'_>,
    price: Option<&ModelPrice>,
    builtin: Option<f64>,
) -> ScriptCtx {
    let multiplier = input
        .upstream
        .map(|upstream| upstream.multiplier)
        .unwrap_or(1.0);
    ScriptCtx {
        model: input.model.unwrap_or("").to_string(),
        target_model: opt_string(input.target_model),
        usage: ScriptUsage {
            input_tokens: input.usage.input_tokens,
            output_tokens: input.usage.output_tokens,
            cache_read_tokens: input.usage.cache_read_tokens,
            cache_creation_tokens: input.usage.cache_creation_tokens,
            total_tokens: input.usage.total_tokens,
            uncached_input_tokens: input.usage.uncached_input_tokens(),
        },
        price: match price {
            Some(price) => Dynamic::from(ScriptPrice {
                model_id: price.model_id.clone(),
                provider_id: price.provider_id.clone(),
                official: price.official,
                input: opt_f64(price.input_usd_per_million),
                cached_input: opt_f64(price.cached_input_usd_per_million),
                cache_write: opt_f64(price.cache_write_usd_per_million),
                output: opt_f64(price.output_usd_per_million),
            }),
            None => Dynamic::UNIT,
        },
        upstream: match input.upstream {
            Some(upstream) => Dynamic::from(ScriptUpstream {
                id: upstream.id.clone(),
                name: upstream.name.clone(),
                kind: upstream.kind.clone(),
                multiplier: upstream.multiplier,
            }),
            None => Dynamic::UNIT,
        },
        builtin: opt_f64(builtin),
        multiplier,
        fx: match env.fx {
            Some(fx) => Dynamic::from(ScriptFx {
                usd_cny: fx.rate,
                fetched_at: fx.fetched_at,
            }),
            None => Dynamic::UNIT,
        },
        now: env.now.timestamp(),
        utc: script_datetime(env.now),
        local: script_datetime(env.now.with_timezone(&Local)),
    }
}

fn script_datetime<Tz: chrono::TimeZone>(dt: DateTime<Tz>) -> ScriptDateTime {
    ScriptDateTime {
        unix: dt.timestamp(),
        year: i64::from(dt.year()),
        month: i64::from(dt.month()),
        day: i64::from(dt.day()),
        hour: i64::from(dt.hour()),
        minute: i64::from(dt.minute()),
        second: i64::from(dt.second()),
        weekday: i64::from(dt.weekday().number_from_monday()),
    }
}

fn opt_f64(value: Option<f64>) -> Dynamic {
    match value {
        Some(value) if value.is_finite() => Dynamic::from(value),
        _ => Dynamic::UNIT,
    }
}

fn opt_string(value: Option<&str>) -> Dynamic {
    match value {
        Some(value) if !value.is_empty() => Dynamic::from(value.to_string()),
        _ => Dynamic::UNIT,
    }
}

fn dynamic_to_cost(value: Dynamic) -> Result<Option<f64>, String> {
    if value.is_unit() {
        return Ok(None);
    }
    let cost = if let Some(value) = value.clone().try_cast::<f64>() {
        value
    } else if let Some(value) = value.clone().try_cast::<i64>() {
        value as f64
    } else {
        return Err(format!("estimate 必须返回数字或 (), 实际类型 {}", value.type_name()));
    };
    if !cost.is_finite() || cost < 0.0 {
        return Err(format!("estimate 返回了无效费用 {cost}"));
    }
    Ok(Some(cost))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{ModelPrice, TokenUsage, Upstream, WireApi};
    use crate::core::models::BalanceProvider;
    use chrono::TimeZone;

    fn usage() -> TokenUsage {
        TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            total_tokens: 2_000_000,
            ..Default::default()
        }
    }

    fn env_at(hour_utc: u32, fx: Option<f64>) -> CostEstimateEnv {
        CostEstimateEnv {
            now: Utc.with_ymd_and_hms(2024, 6, 15, hour_utc, 30, 0).unwrap(),
            fx: fx.map(|rate| UsdCnyRate {
                rate,
                fetched_at: 1_000,
            }),
        }
    }

    async fn test_store() -> Store {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-pricing-{}.sqlite",
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

    fn input<'a>(usage: &'a TokenUsage, upstream: Option<&'a CostUpstream>) -> CostEstimateInput<'a> {
        CostEstimateInput {
            model: Some("gpt-test"),
            target_model: None,
            usage,
            upstream,
        }
    }

    #[tokio::test]
    async fn disabled_script_uses_builtin_multiplier() {
        let store = test_store().await;
        seed_price(&store).await;
        let script = PricingScript::disabled();
        let usage = usage();
        let upstream = CostUpstream {
            id: "u1".to_string(),
            name: "relay".to_string(),
            kind: "relay_api_key".to_string(),
            multiplier: 1.5,
        };
        let cost = estimate_request_cost(&store, &script, &env_at(11, None), input(&usage, Some(&upstream)))
            .await
            .unwrap();
        assert!((cost - 4.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn script_number_overrides_and_skips_host_multiplier() {
        let store = test_store().await;
        seed_price(&store).await;
        let script = PricingScript::disabled();
        script.apply(
            true,
            "fn estimate(ctx) { 9.0 }".to_string(),
        );
        let usage = usage();
        let upstream = CostUpstream {
            id: "u1".to_string(),
            name: "relay".to_string(),
            kind: "relay_api_key".to_string(),
            multiplier: 1.5,
        };
        let cost = estimate_request_cost(&store, &script, &env_at(11, None), input(&usage, Some(&upstream)))
            .await
            .unwrap();
        assert!((cost - 9.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn script_unit_falls_back_to_builtin() {
        let store = test_store().await;
        seed_price(&store).await;
        let script = PricingScript::disabled();
        script.apply(true, "fn estimate(ctx) { () }".to_string());
        let usage = usage();
        let cost = estimate_request_cost(&store, &script, &env_at(11, None), input(&usage, None))
            .await
            .unwrap();
        assert!((cost - 3.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn script_can_price_without_cache() {
        let store = test_store().await;
        let script = PricingScript::disabled();
        script.apply(
            true,
            "fn estimate(ctx) { usd_for_tokens(ctx.usage.uncached_input_tokens, 4.0) }".to_string(),
        );
        let usage = usage();
        let cost = estimate_request_cost(&store, &script, &env_at(11, None), input(&usage, None))
            .await
            .unwrap();
        assert!((cost - 4.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn preview_captures_print_log_and_warn() {
        let store = test_store().await;
        let usage = usage();
        let preview = preview_estimate(
            &store,
            r#"fn estimate(ctx) { print("hello"); log(ctx.model); warn("slow"); 1.0 }"#,
            &env_at(11, None),
            input(&usage, None),
        )
        .await;
        assert!((preview.script_cost.unwrap() - 1.0).abs() < 1e-9);
        assert!(preview.logs.iter().any(|line| line.contains("hello")));
        assert!(preview.logs.iter().any(|line| line.contains("gpt-test")));
        assert!(preview.logs.iter().any(|line| line.contains("slow")));
    }

    #[tokio::test]
    async fn script_reads_fx_and_utc_datetime() {
        let store = test_store().await;
        let script = PricingScript::disabled();
        script.apply(
            true,
            "fn estimate(ctx) {\n    if ctx.fx.usd_cny == 7.5 && ctx.utc.year == 2024 && ctx.utc.hour == 11 && ctx.utc.weekday == 6 {\n        return 1.25;\n    }\n    0.0\n}".to_string(),
        );
        let usage = usage();
        let cost = estimate_request_cost(
            &store,
            &script,
            &env_at(11, Some(7.5)),
            input(&usage, None),
        )
        .await
        .unwrap();
        assert!((cost - 1.25).abs() < 1e-9);
    }

    #[tokio::test]
    async fn compile_error_falls_back() {
        let store = test_store().await;
        seed_price(&store).await;
        let script = PricingScript::disabled();
        script.apply(true, "fn broken(".to_string());
        assert!(script.compile_error().is_some());
        assert!(!script.is_ready());
        let usage = usage();
        let cost = estimate_request_cost(&store, &script, &env_at(11, None), input(&usage, None))
            .await
            .unwrap();
        assert!((cost - 3.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn runtime_error_falls_back() {
        let store = test_store().await;
        seed_price(&store).await;
        let script = PricingScript::disabled();
        script.apply(true, "fn estimate(ctx) { ctx.missing }".to_string());
        let usage = usage();
        let cost = estimate_request_cost(&store, &script, &env_at(11, None), input(&usage, None))
            .await
            .unwrap();
        assert!((cost - 3.0).abs() < 1e-9);
        assert!(script.runtime_error().is_some());
    }

    #[tokio::test]
    async fn operation_limit_falls_back() {
        let store = test_store().await;
        seed_price(&store).await;
        let script = PricingScript::disabled();
        script.apply(true, "fn estimate(ctx) { loop {} }".to_string());
        let usage = usage();
        let cost = estimate_request_cost(&store, &script, &env_at(11, None), input(&usage, None))
            .await
            .unwrap();
        assert!((cost - 3.0).abs() < 1e-9);
    }

    #[test]
    fn compile_check_requires_estimate() {
        assert!(compile_check("let x = 1;").is_err());
        assert!(compile_check(DEFAULT_SCRIPT).is_ok());
    }

    #[tokio::test]
    async fn persist_roundtrip() {
        let store = test_store().await;
        let script = PricingScript::disabled();
        script.apply(true, "fn estimate(ctx) { 1.0 }".to_string());
        script.persist(&store).await.unwrap();
        let loaded = PricingScript::load(&store).await.unwrap();
        assert!(loaded.is_ready());
        assert_eq!(loaded.source(), "fn estimate(ctx) { 1.0 }");
    }

    #[tokio::test]
    async fn attach_uses_upstream_multiplier_without_script() {
        let store = test_store().await;
        seed_price(&store).await;
        let mut upstream = Upstream::new_relay(
            "relay-a".to_string(),
            "https://example.test".to_string(),
            WireApi::Responses,
            true,
            BalanceProvider::Unsupported,
        );
        upstream.price_multiplier = 1.5;
        store.save_upstream(&upstream).await.unwrap();
        let mut log = RequestLog {
            ts: Some(Utc.with_ymd_and_hms(2024, 6, 15, 11, 0, 0).unwrap()),
            upstream_id: Some(upstream.id.clone()),
            upstream_name: Some("relay-a".to_string()),
            source: crate::core::models::RequestLogSource::Proxy,
            endpoint: "/responses".to_string(),
            model: Some("gpt-test".to_string()),
            target_model: None,
            reasoning_effort: None,
            status: 200,
            usage: usage(),
            estimated_cost_usd: None,
            duration_ms: 10,
            first_token_ms: None,
            error: None,
        };
        attach_estimated_cost(&store, &PricingScript::disabled(), &mut log).await;
        assert!((log.estimated_cost_usd.unwrap() - 4.5).abs() < 1e-9);
    }
}

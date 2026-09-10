use crate::app::AppState;
use crate::storage::Store;
use anyhow::Context;
use serde_json::Value;
use std::time::Duration;

const ER_API_URL: &str = "https://open.er-api.com/v6/latest/USD";
const FRANKFURTER_API_URL: &str = "https://api.frankfurter.dev/v1/latest?base=USD&symbols=CNY";
const RATE_SETTING_KEY: &str = "usd_cny_rate";
const RATE_FETCHED_AT_SETTING_KEY: &str = "usd_cny_rate_fetched_at";
/// 汇率合理范围, 用于拦截解析异常.
const RATE_MIN: f64 = 0.5;
const RATE_MAX: f64 = 50.0;
/// 默认汇率缓存有效期, 超过后才会重新请求.
pub const RATE_MAX_AGE_SECS: i64 = 86_400;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsdCnyRate {
    pub rate: f64,
    pub fetched_at: i64,
}

impl UsdCnyRate {
    pub fn age_seconds(&self, now: i64) -> i64 {
        (now - self.fetched_at).max(0)
    }

    pub fn is_stale(&self, now: i64, max_age_secs: i64) -> bool {
        self.age_seconds(now) >= max_age_secs
    }
}

/// 强制获取最新汇率并写入设置缓存.
pub async fn fetch_usd_cny_rate(state: &AppState) -> anyhow::Result<UsdCnyRate> {
    tracing::info!("fetching USD/CNY exchange rate");
    let started = std::time::Instant::now();
    let rate = match fetch_from_er_api(state).await {
        Ok(rate) => rate,
        Err(er_err) => {
            tracing::warn!(error = %er_err, "primary exchange rate source failed, trying fallback");
            fetch_from_frankfurter(state).await?
        }
    };
    let record = UsdCnyRate {
        rate,
        fetched_at: chrono::Utc::now().timestamp(),
    };
    persist_rate(state, &record).await?;
    tracing::info!(
        rate,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "USD/CNY exchange rate fetched"
    );
    Ok(record)
}

/// 读取设置中缓存的汇率.
pub async fn load_usd_cny_rate(state: &AppState) -> anyhow::Result<Option<UsdCnyRate>> {
    load_usd_cny_rate_from_store(&state.store).await
}

/// 从 SQLite settings 读取汇率缓存, 供估算路径使用.
pub async fn load_usd_cny_rate_from_store(store: &Store) -> anyhow::Result<Option<UsdCnyRate>> {
    let Some(rate) = store.get_setting(RATE_SETTING_KEY).await? else {
        return Ok(None);
    };
    let Some(fetched_at) = store.get_setting(RATE_FETCHED_AT_SETTING_KEY).await? else {
        return Ok(None);
    };
    let rate = rate
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|rate| is_valid_rate(*rate));
    let fetched_at = fetched_at.trim().parse::<i64>().ok();
    match (rate, fetched_at) {
        (Some(rate), Some(fetched_at)) => Ok(Some(UsdCnyRate { rate, fetched_at })),
        _ => Ok(None),
    }
}

/// 缓存缺失或过期时获取新汇率, 否则直接返回缓存.
pub async fn ensure_usd_cny_rate(
    state: &AppState,
    max_age_secs: i64,
) -> anyhow::Result<Option<UsdCnyRate>> {
    let now = chrono::Utc::now().timestamp();
    if let Some(cached) = load_usd_cny_rate(state).await?
        && !cached.is_stale(now, max_age_secs)
    {
        return Ok(None);
    }
    fetch_usd_cny_rate(state).await.map(Some)
}

async fn persist_rate(state: &AppState, record: &UsdCnyRate) -> anyhow::Result<()> {
    state
        .store
        .set_setting(RATE_SETTING_KEY, &format!("{}", record.rate))
        .await?;
    state
        .store
        .set_setting(RATE_FETCHED_AT_SETTING_KEY, &record.fetched_at.to_string())
        .await?;
    Ok(())
}

async fn fetch_from_er_api(state: &AppState) -> anyhow::Result<f64> {
    let http = state.http()?;
    let value = http
        .get(ER_API_URL)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .context("failed to request open.er-api.com")?
        .error_for_status()
        .context("open.er-api.com returned an error status")?
        .json::<Value>()
        .await
        .context("failed to parse open.er-api.com response")?;
    parse_rate(&value, "open.er-api.com")
}

async fn fetch_from_frankfurter(state: &AppState) -> anyhow::Result<f64> {
    let http = state.http()?;
    let value = http
        .get(FRANKFURTER_API_URL)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .context("failed to request frankfurter.dev")?
        .error_for_status()
        .context("frankfurter.dev returned an error status")?
        .json::<Value>()
        .await
        .context("failed to parse frankfurter.dev response")?;
    parse_rate(&value, "frankfurter.dev")
}

fn parse_rate(value: &Value, source: &str) -> anyhow::Result<f64> {
    let rate = value
        .pointer("/rates/CNY")
        .and_then(Value::as_f64)
        .context(format!("{source} response missing CNY rate"))?;
    if !is_valid_rate(rate) {
        anyhow::bail!("{source} returned an implausible CNY rate: {rate}");
    }
    Ok(rate)
}

fn is_valid_rate(rate: f64) -> bool {
    rate.is_finite() && (RATE_MIN..RATE_MAX).contains(&rate)
}

#[cfg(test)]
mod tests {
    use super::{UsdCnyRate, is_valid_rate, parse_rate};
    use serde_json::json;

    #[test]
    fn parses_cny_rate_from_common_sources() {
        let er = parse_rate(
            &json!({"result": "success", "base_code": "USD", "rates": {"CNY": 7.2436}}),
            "open.er-api.com",
        )
        .unwrap();
        assert_eq!(er, 7.2436);
        let frankfurter = parse_rate(
            &json!({"base": "USD", "rates": {"CNY": 7.12}}),
            "frankfurter.dev",
        )
        .unwrap();
        assert_eq!(frankfurter, 7.12);
    }

    #[test]
    fn rejects_missing_or_implausible_rates() {
        assert!(parse_rate(&json!({"rates": {}}), "src").is_err());
        assert!(parse_rate(&json!({"rates": {"CNY": 0.0}}), "src").is_err());
        assert!(parse_rate(&json!({"rates": {"CNY": -7.0}}), "src").is_err());
        assert!(!is_valid_rate(f64::NAN));
    }

    #[test]
    fn detects_stale_cache() {
        let record = UsdCnyRate {
            rate: 7.2,
            fetched_at: 1_000,
        };
        assert!(!record.is_stale(1_000 + 86_399, 86_400));
        assert!(record.is_stale(1_000 + 86_400, 86_400));
    }
}

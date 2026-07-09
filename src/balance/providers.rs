use crate::app::AppState;
use crate::core::models::{BalanceProvider, BalanceSnapshot, UpstreamKind};
use anyhow::{Context, anyhow};
use reqwest::StatusCode;
use serde_json::Value;
use std::time::Duration;

const NEWAPI_QUOTA_PER_USD: f64 = 500000.0;
pub(crate) const API_KEY_CREDENTIAL: &str = "api_key";
pub(crate) const NEWAPI_USER_KEY_CREDENTIAL: &str = "newapi_user_key";
pub(crate) const NEWAPI_USER_ID_CREDENTIAL: &str = "newapi_user_id";

#[derive(Debug, Clone)]
struct BalanceCredentials {
    api_key: String,
    newapi_user_key: Option<String>,
    newapi_user_id: Option<String>,
}

impl BalanceCredentials {
    fn common_auth(&self, provider: BalanceProvider) -> anyhow::Result<CommonBalanceAuth<'_>> {
        match provider {
            BalanceProvider::NewApi => {
                let user_key = self
                    .newapi_user_key
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing new-api user key"))?;
                let user_id = self
                    .newapi_user_id
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing new-api user id"))?;
                Ok(CommonBalanceAuth::NewApi { user_key, user_id })
            }
            BalanceProvider::Auto => Ok(CommonBalanceAuth::Auto {
                api_key: &self.api_key,
                newapi_user_key: self.newapi_user_key.as_deref(),
                newapi_user_id: self.newapi_user_id.as_deref(),
            }),
            _ => Ok(CommonBalanceAuth::ApiKey {
                api_key: &self.api_key,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum CommonBalanceAuth<'a> {
    ApiKey {
        api_key: &'a str,
    },
    NewApi {
        user_key: &'a str,
        user_id: &'a str,
    },
    Auto {
        api_key: &'a str,
        newapi_user_key: Option<&'a str>,
        newapi_user_id: Option<&'a str>,
    },
}

#[derive(Debug, Clone, Copy)]
struct CommonBalanceHeaders<'a> {
    bearer: &'a str,
    newapi_user_id: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
enum CommonBalanceUrlKind {
    Generic,
    NewApi,
}

#[derive(Debug, Clone)]
struct CommonBalanceUrl {
    url: String,
    kind: CommonBalanceUrlKind,
}

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
    } else if url.contains("sub2api") {
        Some(BalanceProvider::Sub2Api)
    } else if url.contains("new-api") || url.contains("newapi") || url.contains("one-api") {
        Some(BalanceProvider::NewApi)
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
        .get(&upstream.id, API_KEY_CREDENTIAL)
        .await?
        .ok_or_else(|| anyhow!("missing api key"))?;
    let newapi_user_key = state
        .credentials
        .get(&upstream.id, NEWAPI_USER_KEY_CREDENTIAL)
        .await?;
    let newapi_user_id = state
        .credentials
        .get(&upstream.id, NEWAPI_USER_ID_CREDENTIAL)
        .await?;
    let credentials = BalanceCredentials {
        api_key,
        newapi_user_key,
        newapi_user_id,
    };
    let snapshot = match upstream.balance_provider {
        BalanceProvider::Auto => {
            if let Some(provider) = detect_provider(&upstream.base_url) {
                query_balance(
                    state,
                    &upstream.id,
                    provider,
                    &upstream.base_url,
                    &credentials,
                )
                .await?
            } else {
                query_common_panel(
                    state,
                    &upstream.id,
                    BalanceProvider::Auto,
                    &upstream.base_url,
                    credentials.common_auth(BalanceProvider::Auto)?,
                )
                .await?
            }
        }
        BalanceProvider::Unsupported => return Err(anyhow!("unsupported balance provider")),
        provider => {
            query_balance(
                state,
                &upstream.id,
                provider,
                &upstream.base_url,
                &credentials,
            )
            .await?
        }
    };
    state.store.save_balance_snapshot(&snapshot).await?;
    Ok(snapshot)
}

async fn query_balance(
    state: &AppState,
    upstream_id: &str,
    provider: BalanceProvider,
    base_url: &str,
    credentials: &BalanceCredentials,
) -> anyhow::Result<BalanceSnapshot> {
    match provider {
        BalanceProvider::Sub2Api | BalanceProvider::NewApi => {
            query_common_panel(
                state,
                upstream_id,
                provider,
                base_url,
                credentials.common_auth(provider)?,
            )
            .await
        }
        BalanceProvider::Auto => {
            query_common_panel(
                state,
                upstream_id,
                provider,
                base_url,
                credentials.common_auth(provider)?,
            )
            .await
        }
        BalanceProvider::Unsupported => Err(anyhow!("unsupported balance provider")),
        provider => query_provider(state, upstream_id, provider, &credentials.api_key).await,
    }
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
        BalanceProvider::Auto
        | BalanceProvider::Sub2Api
        | BalanceProvider::NewApi
        | BalanceProvider::Unsupported => {
            return Err(anyhow!("unsupported balance provider"));
        }
    };
    let response = state
        .http
        .get(url)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(15))
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
            snapshot.remaining =
                parse_f64_field(data, "totalBalance").or_else(|| parse_f64_field(data, "balance"));
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
        BalanceProvider::Sub2Api | BalanceProvider::NewApi => {
            if let Some(common) = parse_common_balance(upstream_id, provider, body) {
                return common;
            }
        }
        BalanceProvider::Auto | BalanceProvider::Unsupported => {}
    }
    snapshot
}

async fn query_common_panel(
    state: &AppState,
    upstream_id: &str,
    provider: BalanceProvider,
    base_url: &str,
    auth: CommonBalanceAuth<'_>,
) -> anyhow::Result<BalanceSnapshot> {
    let mut last_error = None;
    for item in common_balance_urls(base_url, provider) {
        let Some(headers) = auth.headers_for(item.kind) else {
            last_error = Some(format!("{}: missing new-api user key or user id", item.url));
            continue;
        };
        let mut request = state
            .http
            .get(&item.url)
            .bearer_auth(headers.bearer)
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(15));
        if let Some(user_id) = headers.newapi_user_id {
            request = request.header("New-Api-User", user_id);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(err) => {
                last_error = Some(format!("{}: {err}", item.url));
                continue;
            }
        };
        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();
        let body = serde_json::from_str::<Value>(&body_text).unwrap_or(Value::Null);
        if !status.is_success() {
            if should_try_next_common_status(status) {
                last_error = Some(format!("{}: HTTP {status}", item.url));
                continue;
            }
            return Ok(invalid_snapshot(
                upstream_id,
                provider,
                format!("HTTP {status}: {body_text}"),
            ));
        }
        if let Some(snapshot) = parse_common_balance(upstream_id, provider, &body) {
            return Ok(snapshot);
        }
        if let Some(snapshot) = parse_common_invalid(upstream_id, provider, &body) {
            last_error = Some(format!(
                "{}: {}",
                item.url,
                snapshot
                    .message
                    .as_deref()
                    .unwrap_or("balance query failed")
            ));
            continue;
        }
        last_error = Some(format!("{}: unsupported balance response {body}", item.url));
    }
    Ok(invalid_snapshot(
        upstream_id,
        provider,
        last_error.unwrap_or_else(|| "unsupported balance provider".to_string()),
    ))
}

impl<'a> CommonBalanceAuth<'a> {
    fn headers_for(self, kind: CommonBalanceUrlKind) -> Option<CommonBalanceHeaders<'a>> {
        match (self, kind) {
            (Self::ApiKey { api_key }, CommonBalanceUrlKind::Generic) => {
                Some(CommonBalanceHeaders {
                    bearer: api_key,
                    newapi_user_id: None,
                })
            }
            (Self::ApiKey { .. }, CommonBalanceUrlKind::NewApi) => None,
            (Self::NewApi { user_key, user_id }, CommonBalanceUrlKind::NewApi) => {
                Some(CommonBalanceHeaders {
                    bearer: user_key,
                    newapi_user_id: Some(user_id),
                })
            }
            (Self::NewApi { .. }, CommonBalanceUrlKind::Generic) => None,
            (Self::Auto { api_key, .. }, CommonBalanceUrlKind::Generic) => {
                Some(CommonBalanceHeaders {
                    bearer: api_key,
                    newapi_user_id: None,
                })
            }
            (
                Self::Auto {
                    newapi_user_key,
                    newapi_user_id,
                    ..
                },
                CommonBalanceUrlKind::NewApi,
            ) => Some(CommonBalanceHeaders {
                bearer: newapi_user_key?,
                newapi_user_id: Some(newapi_user_id?),
            }),
        }
    }
}

fn common_balance_urls(base_url: &str, provider: BalanceProvider) -> Vec<CommonBalanceUrl> {
    let mut urls = Vec::new();
    match provider {
        BalanceProvider::Sub2Api => {
            push_unique_url(
                &mut urls,
                append_path_url(base_url, "usage"),
                CommonBalanceUrlKind::Generic,
            );
            push_unique_url(
                &mut urls,
                root_path_url(base_url, "/v1/usage"),
                CommonBalanceUrlKind::Generic,
            );
        }
        BalanceProvider::NewApi => {
            push_newapi_urls(&mut urls, base_url);
        }
        BalanceProvider::Auto => {
            push_unique_url(
                &mut urls,
                append_path_url(base_url, "usage"),
                CommonBalanceUrlKind::Generic,
            );
            push_unique_url(
                &mut urls,
                root_path_url(base_url, "/v1/usage"),
                CommonBalanceUrlKind::Generic,
            );
            push_newapi_urls(&mut urls, base_url);
        }
        _ => {}
    }
    urls
}

fn push_newapi_urls(urls: &mut Vec<CommonBalanceUrl>, base_url: &str) {
    push_unique_url(
        urls,
        root_path_url(base_url, "/api/user/self"),
        CommonBalanceUrlKind::NewApi,
    );
}

fn append_path_url(base_url: &str, path: &str) -> Option<String> {
    let mut url = url::Url::parse(base_url).ok()?;
    let mut base_path = url.path().trim_end_matches('/').to_string();
    base_path.push('/');
    base_path.push_str(path.trim_start_matches('/'));
    url.set_path(&base_path);
    url.set_query(None);
    Some(url.to_string())
}

fn root_path_url(base_url: &str, path: &str) -> Option<String> {
    let mut url = url::Url::parse(base_url).ok()?;
    url.set_path(path);
    url.set_query(None);
    Some(url.to_string())
}

fn push_unique_url(
    urls: &mut Vec<CommonBalanceUrl>,
    url: Option<String>,
    kind: CommonBalanceUrlKind,
) {
    let Some(url) = url else {
        return;
    };
    if !urls.iter().any(|existing| existing.url == url) {
        urls.push(CommonBalanceUrl { url, kind });
    }
}

fn should_try_next_common_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::NOT_FOUND
            | StatusCode::METHOD_NOT_ALLOWED
            | StatusCode::BAD_REQUEST
            | StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
    )
}

fn parse_common_balance(
    upstream_id: &str,
    provider: BalanceProvider,
    body: &Value,
) -> Option<BalanceSnapshot> {
    let data = body.get("data").unwrap_or(body);
    let quota = data.get("quota").or_else(|| body.get("quota"));
    let quota_obj = quota.filter(|value| value.is_object());

    if let Some(snapshot) = parse_quota_object_balance(upstream_id, provider, data, quota_obj) {
        return Some(snapshot);
    }
    if matches!(provider, BalanceProvider::Auto | BalanceProvider::NewApi)
        && let Some(snapshot) = parse_newapi_quota_balance(upstream_id, provider, data, body)
    {
        return Some(snapshot);
    }

    let remaining = quota_obj
        .and_then(|q| number_any(q, &["remaining", "remain", "left"]))
        .or_else(|| {
            number_any(
                data,
                &[
                    "remaining",
                    "remain",
                    "remain_quota",
                    "left_quota",
                    "total_available",
                    "available",
                ],
            )
        })
        .or_else(|| number_any(data, &["balance", "available_balance", "availableBalance"]));

    let used = quota_obj
        .and_then(|q| number_any(q, &["used", "usage"]))
        .or_else(|| {
            number_any(
                data,
                &[
                    "used",
                    "used_quota",
                    "quota_used",
                    "total_usage",
                    "total_used",
                ],
            )
        });

    let total = quota_obj
        .and_then(|q| number_any(q, &["limit", "total"]))
        .or_else(|| {
            number_any(
                data,
                &[
                    "limit",
                    "total",
                    "total_quota",
                    "total_credits",
                    "total_granted",
                ],
            )
        });

    let remaining = remaining.or_else(|| match (total, used) {
        (Some(total), Some(used)) => Some((total - used).max(0.0)),
        _ => None,
    })?;

    let unit = string_any(data, &["unit", "currency"])
        .or_else(|| quota_obj.and_then(|q| string_any(q, &["unit", "currency"])))
        .unwrap_or_else(|| default_common_unit(provider, data).to_string());

    let is_valid = bool_any(data, &["isValid", "is_valid", "is_active"])
        .or_else(|| bool_any(body, &["isValid", "is_valid", "is_active"]))
        .unwrap_or(remaining > 0.0);
    let message = string_any(data, &["message", "error", "invalid_message"])
        .or_else(|| string_any(body, &["message", "error", "invalid_message"]));

    Some(BalanceSnapshot {
        upstream_id: upstream_id.to_string(),
        provider: provider.as_str().to_string(),
        remaining: Some(remaining),
        total,
        used,
        unit: Some(unit),
        is_valid,
        message,
        fetched_at: chrono::Utc::now().timestamp(),
    })
}

fn parse_quota_object_balance(
    upstream_id: &str,
    provider: BalanceProvider,
    data: &Value,
    quota_obj: Option<&Value>,
) -> Option<BalanceSnapshot> {
    let quota_obj = quota_obj?;
    let used = number_any(quota_obj, &["used", "usage"]);
    let total = number_any(quota_obj, &["limit", "total"]);
    let remaining = number_any(quota_obj, &["remaining", "remain", "left"]).or_else(|| {
        match (total, used) {
            (Some(total), Some(used)) => Some((total - used).max(0.0)),
            _ => None,
        }
    })?;
    let unit = string_any(quota_obj, &["unit", "currency"])
        .or_else(|| string_any(data, &["unit", "currency"]))
        .unwrap_or_else(|| default_common_unit(provider, data).to_string());
    let is_valid = bool_any(data, &["isValid", "is_valid", "is_active"]).unwrap_or(remaining > 0.0);
    let message = string_any(data, &["message", "error", "invalid_message"]);
    Some(BalanceSnapshot {
        upstream_id: upstream_id.to_string(),
        provider: provider.as_str().to_string(),
        remaining: Some(remaining),
        total,
        used,
        unit: Some(unit),
        is_valid,
        message,
        fetched_at: chrono::Utc::now().timestamp(),
    })
}

fn parse_newapi_quota_balance(
    upstream_id: &str,
    provider: BalanceProvider,
    data: &Value,
    body: &Value,
) -> Option<BalanceSnapshot> {
    let has_quota_shape = data.get("remain_quota").is_some()
        || data.get("remaining_quota").is_some()
        || data.get("used_quota").is_some()
        || data.get("quota_used").is_some();
    let remaining_units = number_any(data, &["remain_quota", "remaining_quota"]).or_else(|| {
        number_any(data, &["quota"])
            .filter(|_| has_quota_shape || provider == BalanceProvider::NewApi)
    })?;
    let used_units = number_any(data, &["used_quota", "quota_used"]);
    let total_units = number_any(data, &["total_quota", "quota_limit", "limit_quota"])
        .or_else(|| used_units.map(|used| remaining_units + used));
    let remaining = newapi_quota_to_usd(remaining_units);
    let used = used_units.map(newapi_quota_to_usd);
    let total = total_units.map(newapi_quota_to_usd);
    let unit = string_any(data, &["unit", "currency"]).unwrap_or_else(|| "USD".to_string());
    let is_valid = bool_any(data, &["isValid", "is_valid", "is_active"])
        .or_else(|| bool_any(body, &["isValid", "is_valid", "is_active", "success"]))
        .unwrap_or(remaining > 0.0);
    let message = string_any(data, &["message", "error", "invalid_message"])
        .or_else(|| string_any(body, &["message", "error", "invalid_message"]));
    Some(BalanceSnapshot {
        upstream_id: upstream_id.to_string(),
        provider: provider.as_str().to_string(),
        remaining: Some(remaining),
        total,
        used,
        unit: Some(unit),
        is_valid,
        message,
        fetched_at: chrono::Utc::now().timestamp(),
    })
}

fn parse_common_invalid(
    upstream_id: &str,
    provider: BalanceProvider,
    body: &Value,
) -> Option<BalanceSnapshot> {
    let data = body.get("data").unwrap_or(body);
    let explicit_invalid = [body, data]
        .iter()
        .any(|obj| bool_any(obj, &["success", "isValid", "is_valid", "is_active"]) == Some(false));
    if !explicit_invalid {
        return None;
    }
    let message = string_any(data, &["message", "error", "invalid_message"])
        .or_else(|| string_any(body, &["message", "error", "invalid_message"]))
        .unwrap_or_else(|| "balance query failed".to_string());
    Some(invalid_snapshot(upstream_id, provider, message))
}

fn default_common_unit(_provider: BalanceProvider, _data: &Value) -> &'static str {
    "USD"
}

fn newapi_quota_to_usd(value: f64) -> f64 {
    value / NEWAPI_QUOTA_PER_USD
}

fn invalid_snapshot(
    upstream_id: &str,
    provider: BalanceProvider,
    message: String,
) -> BalanceSnapshot {
    BalanceSnapshot {
        upstream_id: upstream_id.to_string(),
        provider: provider.as_str().to_string(),
        is_valid: false,
        message: Some(message),
        fetched_at: chrono::Utc::now().timestamp(),
        ..BalanceSnapshot::default()
    }
}

fn parse_f64_field(obj: &Value, field: &str) -> Option<f64> {
    obj.get(field).and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

fn number_any(obj: &Value, fields: &[&str]) -> Option<f64> {
    fields.iter().find_map(|field| parse_f64_field(obj, field))
}

fn string_any(obj: &Value, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| obj.get(*field).and_then(Value::as_str).map(str::to_string))
}

fn bool_any(obj: &Value, fields: &[&str]) -> Option<bool> {
    fields
        .iter()
        .find_map(|field| obj.get(*field).and_then(Value::as_bool))
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
        assert_eq!(
            detect_provider("https://example.com/new-api/v1"),
            Some(BalanceProvider::NewApi)
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

    #[test]
    fn parses_sub2api_usage_balance() {
        let snapshot = parse_common_balance(
            "u1",
            BalanceProvider::Sub2Api,
            &json!({
                "mode": "quota_limited",
                "isValid": true,
                "quota": {"limit": 10.0, "used": 3.0, "remaining": 7.0, "unit": "USD"}
            }),
        )
        .unwrap();
        assert_eq!(snapshot.remaining, Some(7.0));
        assert_eq!(snapshot.total, Some(10.0));
        assert_eq!(snapshot.used, Some(3.0));
        assert_eq!(snapshot.unit.as_deref(), Some("USD"));
        assert!(snapshot.is_valid);
    }

    #[test]
    fn parses_newapi_token_balance() {
        let snapshot = parse_common_balance(
            "u1",
            BalanceProvider::NewApi,
            &json!({
                "success": true,
                "data": {"remain_quota": 5000, "used_quota": 1200}
            }),
        )
        .unwrap();
        assert_eq!(snapshot.remaining, Some(0.01));
        assert_eq!(snapshot.used, Some(0.0024));
        assert_eq!(snapshot.total, Some(0.0124));
        assert_eq!(snapshot.unit.as_deref(), Some("USD"));
    }

    #[test]
    fn parses_newapi_user_self_balance() {
        let snapshot = parse_common_balance(
            "u1",
            BalanceProvider::NewApi,
            &json!({
                "success": true,
                "data": {"quota": 500000, "used_quota": 250000}
            }),
        )
        .unwrap();
        assert_eq!(snapshot.remaining, Some(1.0));
        assert_eq!(snapshot.used, Some(0.5));
        assert_eq!(snapshot.total, Some(1.5));
        assert_eq!(snapshot.unit.as_deref(), Some("USD"));
    }

    #[test]
    fn parses_sub2api_unrestricted_balance() {
        let snapshot = parse_common_balance(
            "u1",
            BalanceProvider::Sub2Api,
            &json!({
                "mode": "unrestricted",
                "isValid": true,
                "remaining": 2.5,
                "balance": 2.5,
                "unit": "USD"
            }),
        )
        .unwrap();
        assert_eq!(snapshot.remaining, Some(2.5));
        assert_eq!(snapshot.unit.as_deref(), Some("USD"));
    }

    #[test]
    fn parses_dashboard_credit_grants_balance() {
        let snapshot = parse_common_balance(
            "u1",
            BalanceProvider::Auto,
            &json!({
                "total_available": 6.25,
                "total_granted": 10.0,
                "total_used": 3.75
            }),
        )
        .unwrap();
        assert_eq!(snapshot.remaining, Some(6.25));
        assert_eq!(snapshot.total, Some(10.0));
        assert_eq!(snapshot.used, Some(3.75));
        assert_eq!(snapshot.unit.as_deref(), Some("USD"));
    }
}

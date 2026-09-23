//! 智谱 GLM Coding Plan 的额度窗口.
//!
//! `GET {host}/api/monitor/usage/quota/limit`, 请求头 `Authorization: <key>`
//! (智谱不加 Bearer 前缀; 实测加不加前缀都能通), 响应:
//! `{success, msg, data: {level, limits: [{type, unit, number, usage, currentValue, remaining, percentage, nextResetTime}]}}`
//!
//! `limits` 里 `percentage` 是已用百分比; `unit=3` 是 5 小时滚动窗口, `unit=6`
//! 是每周窗口. 官方套餐只有这两个窗口, 没有月度额度, 因此不产出月度分段.
//! 国内站 `open.bigmodel.cn` 与国际站 `api.z.ai` 共用同一后端与字段.
//!
//! 额度类型随套餐版本变化: 新套餐 (按积分计量) 返回 `CREDIT_LIMIT`, 老套餐返回
//! `TOKENS_LIMIT`, 两者都按 `unit` 归类; `unit` 缺失时才按重置时间兜底。

use super::{AuthScheme, PlanFuture, PlanQuota, PlanQuotaSpec, get_json, number};
use crate::core::models::{QuotaWindow, QuotaWindowKind};
use crate::logging::network::HttpClient;
use anyhow::anyhow;
use serde_json::Value;

/// `unit` 取值对应的窗口类型.
const FIVE_HOUR_UNIT: i64 = 3;
const WEEKLY_UNIT: i64 = 6;

pub(super) static SPEC: PlanQuotaSpec = PlanQuotaSpec {
    key: "zhipu_plan",
    matchers: &["bigmodel.cn", "api.z.ai"],
    money_fallback: true,
    query,
};

fn query<'a>(http: &'a HttpClient, api_key: &'a str, base_url: &'a str) -> PlanFuture<'a> {
    Box::pin(async move {
        let url = format!("{}/api/monitor/usage/quota/limit", host(base_url));
        let body = get_json(http, &url, api_key, AuthScheme::Raw).await?;
        if body.get("success").and_then(Value::as_bool) == Some(false) {
            let message = body
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or("额度查询被拒绝");
            return Err(anyhow!("{message}"));
        }
        let data = body.get("data");
        Ok(PlanQuota {
            provider: SPEC.key,
            windows: data.map(parse_windows).unwrap_or_default(),
            message: data
                .and_then(|data| data.get("level"))
                .and_then(Value::as_str)
                .map(|level| format!("套餐 {level}")),
            ..PlanQuota::default()
        })
    })
}

/// 额度接口与模型接口同族, 按用户填写的 Base URL 选 host.
fn host(base_url: &str) -> &'static str {
    let url = base_url.trim().to_ascii_lowercase();
    if url.contains("bigmodel.cn") {
        "https://open.bigmodel.cn"
    } else {
        "https://api.z.ai"
    }
}

/// 把 `data.limits` 解析成窗口, 顺序固定为 5h 与 weekly.
fn parse_windows(data: &Value) -> Vec<QuotaWindow> {
    type Entry = (Option<i64>, f64);
    let mut five_hour: Option<Entry> = None;
    let mut weekly: Option<Entry> = None;
    let mut unclassified: Vec<Entry> = Vec::new();

    if let Some(limits) = data.get("limits").and_then(Value::as_array) {
        for item in limits {
            if !is_quota_limit(item) {
                continue;
            }
            let used_percent = window_used_percent(item);
            let reset_at = item.get("nextResetTime").and_then(Value::as_i64);
            let entry = (reset_at, used_percent);
            match item.get("unit").and_then(Value::as_i64) {
                Some(FIVE_HOUR_UNIT) if five_hour.is_none() => five_hour = Some(entry),
                Some(WEEKLY_UNIT) if weekly.is_none() => weekly = Some(entry),
                // 只有 `unit` 完全缺失时才交给重置时间兜底, 其它周期 (例如日额度) 不占 5h / 1w 的位置.
                None => unclassified.push(entry),
                Some(_) => {}
            }
        }
    }

    // 没有重置时间的先归 5 小时窗口, 其余按重置时间升序填坑.
    // 不能只按时间排序, 周期末尾每周窗口可能先重置.
    unclassified.sort_by_key(|(reset_at, _)| (reset_at.is_some(), reset_at.unwrap_or(i64::MIN)));
    for entry in unclassified {
        if five_hour.is_none() {
            five_hour = Some(entry);
        } else if weekly.is_none() {
            weekly = Some(entry);
        }
    }

    let mut windows = Vec::new();
    if let Some((_, used_percent)) = five_hour {
        windows.push(QuotaWindow::new(QuotaWindowKind::FiveHour, used_percent));
    }
    if let Some((_, used_percent)) = weekly {
        windows.push(QuotaWindow::new(QuotaWindowKind::Weekly, used_percent));
    }
    windows
}

/// 该条目是不是额度窗口.
///
/// 新套餐按积分计量返回 `CREDIT_LIMIT`, 老套餐返回 `TOKENS_LIMIT`, 大小写不敏感;
/// 其它类型 (例如 `TIME_LIMIT`) 与窗口无关, 直接跳过.
fn is_quota_limit(item: &Value) -> bool {
    let limit_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    ["CREDIT_LIMIT", "TOKENS_LIMIT"]
        .iter()
        .any(|known| limit_type.eq_ignore_ascii_case(known))
}

/// 已用比例: 优先用上游给的 `percentage`, 缺失时按 `currentValue / usage` 换算.
fn window_used_percent(item: &Value) -> f64 {
    if let Some(percentage) = number(item, "percentage") {
        return percentage;
    }
    let used = number(item, "currentValue").unwrap_or(0.0);
    match number(item, "usage") {
        Some(cap) if cap > 0.0 => used / cap * 100.0,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn host_follows_the_base_url() {
        assert_eq!(host("https://open.bigmodel.cn/api/coding/paas/v4"), "https://open.bigmodel.cn");
        assert_eq!(host("https://api.z.ai/api/paas/v4"), "https://api.z.ai");
    }

    #[test]
    fn windows_use_the_unit_field() {
        let data = json!({
            "level": "pro",
            "limits": [
                {"type": "TOKENS_LIMIT", "unit": 6, "number": 7, "percentage": 42.0, "nextResetTime": 2},
                {"type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 1.0, "nextResetTime": 1}
            ]
        });
        assert_eq!(
            parse_windows(&data),
            vec![
                QuotaWindow::new(QuotaWindowKind::FiveHour, 1.0),
                QuotaWindow::new(QuotaWindowKind::Weekly, 42.0),
            ]
        );
    }

    #[test]
    fn windows_fall_back_to_reset_order() {
        let data = json!({
            "limits": [
                {"type": "TOKENS_LIMIT", "percentage": 42.0, "nextResetTime": 9_000},
                {"type": "TOKENS_LIMIT", "percentage": 3.0}
            ]
        });
        assert_eq!(
            parse_windows(&data),
            vec![
                QuotaWindow::new(QuotaWindowKind::FiveHour, 3.0),
                QuotaWindow::new(QuotaWindowKind::Weekly, 42.0),
            ]
        );
    }

    #[test]
    fn other_limit_types_are_ignored_case_insensitively() {
        let data = json!({
            "limits": [
                {"type": "tokens_limit", "unit": 3, "percentage": 5.0},
                {"type": "TIME_LIMIT", "unit": 6, "percentage": 90.0}
            ]
        });
        assert_eq!(
            parse_windows(&data),
            vec![QuotaWindow::new(QuotaWindowKind::FiveHour, 5.0)]
        );
    }

    /// 2026-09-23 实测 open.bigmodel.cn 的 lite 套餐响应, 新套餐按积分计量用 CREDIT_LIMIT.
    #[test]
    fn credit_limit_plan_returns_both_windows() {
        let data = json!({
            "limits": [
                {
                    "type": "CREDIT_LIMIT", "unit": 3, "number": 5,
                    "usage": 2000, "currentValue": 0, "remaining": 2000, "percentage": 0
                },
                {
                    "type": "CREDIT_LIMIT", "unit": 6, "number": 1,
                    "usage": 10000, "currentValue": 1876, "remaining": 8123,
                    "percentage": 18, "nextResetTime": 1_790_530_553_994i64
                }
            ],
            "level": "lite"
        });
        assert_eq!(
            parse_windows(&data),
            vec![
                QuotaWindow::new(QuotaWindowKind::FiveHour, 0.0),
                QuotaWindow::new(QuotaWindowKind::Weekly, 18.0),
            ]
        );
    }

    #[test]
    fn missing_percentage_falls_back_to_current_value_over_usage() {
        let data = json!({
            "limits": [
                {"type": "CREDIT_LIMIT", "unit": 3, "usage": 2000, "currentValue": 500},
                {"type": "CREDIT_LIMIT", "unit": 6, "usage": 0, "currentValue": 500}
            ]
        });
        assert_eq!(
            parse_windows(&data),
            vec![
                QuotaWindow::new(QuotaWindowKind::FiveHour, 25.0),
                QuotaWindow::new(QuotaWindowKind::Weekly, 0.0),
            ]
        );
    }

    #[test]
    fn unknown_units_do_not_take_the_five_hour_slot() {
        let data = json!({
            "limits": [
                {"type": "CREDIT_LIMIT", "unit": 1, "percentage": 90.0},
                {"type": "CREDIT_LIMIT", "unit": 3, "percentage": 5.0}
            ]
        });
        assert_eq!(
            parse_windows(&data),
            vec![QuotaWindow::new(QuotaWindowKind::FiveHour, 5.0)]
        );
    }

    #[test]
    fn pay_as_you_go_keys_have_no_windows() {
        assert!(parse_windows(&json!({"level": "unknown", "limits": []})).is_empty());
    }
}

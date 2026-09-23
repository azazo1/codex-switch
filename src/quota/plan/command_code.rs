//! Command Code 的额度窗口.
//!
//! Provider API (`https://api.commandcode.ai/provider/v1`) 与账号端点共用同一把
//! API Key, 刷新时并发查两个账号端点:
//! - `GET /alpha/billing/credits`:
//!   `{credits: {planId, monthlyCredits, purchasedCredits, freeCredits},
//!     windowLimits: {limited, fiveHour: {used, cap, resetAt}, weekly: {...}}}`
//! - `GET /alpha/billing/subscriptions`: `{data: {planId, status, currentPeriodEnd}}`
//!
//! 套餐只有 5 小时与每周两个上限窗口, 月度没有上限, 只有积分池, 因此月度窗口用
//! 剩余积分除以套餐积分池换算; 纯按量付费的 Provider 套餐没有任何窗口, 用积分余额展示.

use super::{
    AuthScheme, PlanFuture, PlanQuota, PlanQuotaSpec, get_json, number, ratio_window,
};
use crate::core::models::{QuotaWindow, QuotaWindowKind};
use crate::logging::network::HttpClient;
use serde_json::Value;

const HOST: &str = "https://api.commandcode.ai";

/// Command Code 各套餐的月度积分总额 (planId 前缀, 总额, 展示名).
///
/// 上游只在 `credits.monthlyCredits` 返回剩余积分, 总额要按套餐换算. 这份表与
/// `command-code` CLI 1.64.0 内置的套餐表一致; 上游新增套餐时月度窗口暂时缺失,
/// 只影响这一个分段, 5 小时与每周窗口照常展示.
const PLAN_CREDITS: &[(&str, f64, &str)] = &[
    ("individual-go", 10.0, "Go"),
    ("individual-goat", 70.0, "GOAT"),
    ("individual-pro-v1", 80.0, "Pro"),
    ("individual-pro", 30.0, "Pro"),
    ("individual-provider", 15.0, "Provider"),
    ("individual-max", 150.0, "Max"),
    ("individual-ultra", 300.0, "Ultra"),
    ("teams-pro", 40.0, "Teams Pro"),
];

pub(super) static SPEC: PlanQuotaSpec = PlanQuotaSpec {
    key: "commandcode",
    matchers: &["commandcode.ai"],
    money_fallback: false,
    query,
};

fn query<'a>(http: &'a HttpClient, api_key: &'a str, _base_url: &'a str) -> PlanFuture<'a> {
    Box::pin(async move {
        let credits_url = format!("{HOST}/alpha/billing/credits");
        let subscriptions_url = format!("{HOST}/alpha/billing/subscriptions");
        let (credits, subscriptions) = tokio::join!(
            get_json(http, &credits_url, api_key, AuthScheme::Bearer),
            get_json(http, &subscriptions_url, api_key, AuthScheme::Bearer),
        );
        let credits = credits?;
        // 订阅信息只用于月度窗口的套餐总额, 查不到时其余窗口照常展示.
        let subscription = subscriptions.ok();
        Ok(plan_quota(&credits, subscription.as_ref()))
    })
}

/// 把 credits 与 subscriptions 的响应组装成额度窗口.
fn plan_quota(credits: &Value, subscription: Option<&Value>) -> PlanQuota {
    let credit_object = credits.get("credits");
    let credit = |field: &str| {
        credit_object
            .and_then(|object| number(object, field))
            .unwrap_or(0.0)
            .max(0.0)
    };
    let monthly = credit("monthlyCredits");
    let purchased = credit("purchasedCredits");
    let free = credit("freeCredits");
    let total_remaining = monthly + purchased + free;

    let mut windows = Vec::new();
    if let Some(limits) = credits.get("windowLimits")
        && limits
            .get("limited")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        if let Some(window) = ratio_window(limits, "fiveHour", QuotaWindowKind::FiveHour) {
            windows.push(window);
        }
        if let Some(window) = ratio_window(limits, "weekly", QuotaWindowKind::Weekly) {
            windows.push(window);
        }
    }

    let subscription_data = subscription.and_then(|value| value.get("data"));
    let plan = subscription_data
        .and_then(|data| data.get("planId"))
        .and_then(Value::as_str)
        .and_then(plan_credits);
    if let Some(used_percent) = monthly_used_percent(
        subscription_data,
        plan,
        monthly,
        purchased,
        free,
        total_remaining,
    ) {
        windows.push(QuotaWindow::new(QuotaWindowKind::Monthly, used_percent));
    }

    let mut quota = PlanQuota {
        provider: SPEC.key,
        windows,
        message: plan.map(|(_, name)| format!("套餐 {name}")),
        ..PlanQuota::default()
    };
    // 没有窗口的套餐用积分余额展示.
    if !quota.has_windows() && total_remaining > 0.0 {
        quota.remaining = Some(total_remaining);
        quota.unit = Some("USD".to_string());
    }
    quota
}

/// 月度窗口的已用比例: 剩余积分除以套餐积分池.
fn monthly_used_percent(
    subscription_data: Option<&Value>,
    plan: Option<(f64, &'static str)>,
    monthly: f64,
    purchased: f64,
    free: f64,
    total_remaining: f64,
) -> Option<f64> {
    let data = subscription_data?;
    if data.get("status").and_then(Value::as_str) != Some("active") {
        return None;
    }
    let (plan_total, _) = plan?;
    let pool = plan_total.max(monthly) + purchased + free;
    if pool <= 0.0 {
        return None;
    }
    Some(((pool - total_remaining) / pool * 100.0).clamp(0.0, 100.0))
}

/// 按 planId 前缀匹配套餐表, 先匹配更长的键, 避免 `individual-pro` 抢先命中 `individual-pro-v1`.
fn plan_credits(plan_id: &str) -> Option<(f64, &'static str)> {
    let normalized = plan_id.trim().to_ascii_lowercase().replace('_', "-");
    PLAN_CREDITS
        .iter()
        .filter(|(id, _, _)| normalized.starts_with(id))
        .max_by_key(|(id, _, _)| id.len())
        .map(|(_, credits, name)| (*credits, *name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plan_credits_prefers_the_longest_prefix() {
        assert_eq!(plan_credits("individual-pro-v1"), Some((80.0, "Pro")));
        assert_eq!(plan_credits("individual_pro"), Some((30.0, "Pro")));
        assert_eq!(plan_credits("teams-pro"), Some((40.0, "Teams Pro")));
        assert_eq!(plan_credits("individual-unknown"), None);
    }

    #[test]
    fn windows_come_from_the_credit_ratio_and_the_monthly_pool() {
        let credits = json!({
            "credits": {"planId": "individual-goat", "monthlyCredits": 35.0},
            "windowLimits": {
                "limited": true,
                "fiveHour": {"used": 10.0, "cap": 40.0},
                "weekly": {"used": 5.0, "cap": 100.0}
            }
        });
        let subscription = json!({"data": {"planId": "individual-goat", "status": "active"}});
        let quota = plan_quota(&credits, Some(&subscription));

        let percents = quota
            .windows
            .iter()
            .map(|window| (window.kind, window.used_percent))
            .collect::<Vec<_>>();
        assert_eq!(
            percents,
            vec![
                (QuotaWindowKind::FiveHour, 25.0),
                (QuotaWindowKind::Weekly, 5.0),
                (QuotaWindowKind::Monthly, 50.0),
            ]
        );
        assert_eq!(quota.provider, "commandcode");
        assert_eq!(quota.message.as_deref(), Some("套餐 GOAT"));
        assert!(!quota.has_money());
    }

    #[test]
    fn unlimited_plans_show_credits_instead_of_windows() {
        let credits = json!({
            "credits": {"planId": "individual-provider", "purchasedCredits": 12.5, "freeCredits": 2.5},
            "windowLimits": {"limited": false}
        });
        let quota = plan_quota(&credits, None);
        assert!(!quota.has_windows());
        assert_eq!(quota.remaining, Some(15.0));
        assert_eq!(quota.unit.as_deref(), Some("USD"));
    }

    #[test]
    fn monthly_window_needs_an_active_subscription_and_known_plan() {
        let inactive = json!({"status": "canceled"});
        assert_eq!(
            monthly_used_percent(Some(&inactive), Some((70.0, "GOAT")), 35.0, 0.0, 0.0, 35.0),
            None
        );
        let active = json!({"status": "active"});
        assert_eq!(
            monthly_used_percent(Some(&active), None, 35.0, 0.0, 0.0, 35.0),
            None
        );
        assert_eq!(
            monthly_used_percent(None, Some((70.0, "GOAT")), 35.0, 0.0, 0.0, 35.0),
            None
        );
    }

    #[test]
    fn empty_credits_response_yields_nothing_usable() {
        let quota = plan_quota(&json!({}), None);
        assert!(!quota.has_windows());
        assert!(!quota.has_money());
    }
}

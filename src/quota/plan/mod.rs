//! 编码与令牌套餐 (coding plan / token plan) 的额度窗口查询.
//!
//! 这类上游的 API Key 除金额余额外还带滚动额度窗口, 例如:
//! - Command Code: 5 小时窗口, 每周窗口, 以及月度积分池.
//! - 智谱 GLM Coding Plan: 5 小时窗口与每周窗口.
//! - 后续可以按同一套结构接入 Kimi For Coding, MiniMax, OpenCode Zen,
//!   火山方舟 Agent / Coding Plan 这类套餐.
//!
//! 每个提供方只需要一个小模块: 声明 [`PlanQuotaSpec`], 实现一个查询函数,
//! 再把它登记进 [`PROVIDERS`]. 窗口种类, 百分比口径, 请求头风格和端点形状
//! 全部收在各自模块里, 公共层只负责注册, 识别, 请求与常用解析.
//!
//! 新增提供方的步骤:
//! 1. 新建 `plan/<name>.rs`, 写 `SPEC` 与 `query`.
//! 2. 在 [`PROVIDERS`] 里追加 `&<name>::SPEC`.
//! 3. 在 `docs/upstream-guide.md` 的套餐额度表里补一行.

mod command_code;
mod zhipu;

use crate::core::models::{QuotaWindow, QuotaWindowKind};
use crate::logging::network::HttpClient;
use anyhow::anyhow;
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::time::Duration;

/// 单次额度请求的超时.
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// 提供方查询函数的返回类型, 允许一次刷新发多个请求.
pub type PlanFuture<'a> = BoxFuture<'a, anyhow::Result<PlanQuota>>;

/// 一个套餐提供方的静态描述.
pub struct PlanQuotaSpec {
    /// 写入余额快照 `provider` 字段的键, 例如 `commandcode`, `zhipu_plan`.
    pub key: &'static str,
    /// Base URL 含任一子串即命中该提供方, 按 [`PROVIDERS`] 顺序优先.
    pub matchers: &'static [&'static str],
    /// 查询不到窗口时能否回落到原有的金额余额查询.
    pub money_fallback: bool,
    /// 查询入口, 负责把该提供方的响应归一化成额度窗口.
    pub query: for<'a> fn(&'a HttpClient, &'a str, &'a str) -> PlanFuture<'a>,
}

/// 已支持的套餐提供方, 顺序即识别优先级.
pub static PROVIDERS: &[&PlanQuotaSpec] = &[&command_code::SPEC, &zhipu::SPEC];

/// 一次套餐额度查询结果, 由 balance 模块组装成余额快照.
#[derive(Debug, Clone, Default)]
pub struct PlanQuota {
    /// 写入快照的 provider 键.
    pub provider: &'static str,
    pub windows: Vec<QuotaWindow>,
    pub remaining: Option<f64>,
    pub used: Option<f64>,
    pub total: Option<f64>,
    pub unit: Option<String>,
    /// 套餐名等附加说明, 展示为余额状态的悬浮提示.
    pub message: Option<String>,
}

impl PlanQuota {
    pub fn has_windows(&self) -> bool {
        !self.windows.is_empty()
    }

    pub fn has_money(&self) -> bool {
        self.remaining.is_some() || self.total.is_some() || self.used.is_some()
    }
}

/// 命中的套餐提供方.
#[derive(Debug, Clone, Copy)]
pub struct PlanQuotaProvider(&'static PlanQuotaSpec);

impl PlanQuotaProvider {
    /// 写入快照的 provider 键.
    pub fn key(self) -> &'static str {
        self.0.key
    }

    /// 没有窗口数据时能否回落到金额余额查询.
    pub fn money_fallback(self) -> bool {
        self.0.money_fallback
    }

    pub async fn query(
        self,
        http: &HttpClient,
        api_key: &str,
        base_url: &str,
    ) -> anyhow::Result<PlanQuota> {
        (self.0.query)(http, api_key, base_url).await
    }
}

/// 依据 Base URL 判断上游是否属于已支持的套餐, 与余额 provider 识别一样纯本地判断.
pub fn detect(base_url: &str) -> Option<PlanQuotaProvider> {
    let url = base_url.trim().to_ascii_lowercase();
    PROVIDERS
        .iter()
        .copied()
        .find(|spec| spec.matchers.iter().any(|matcher| url.contains(matcher)))
        .map(PlanQuotaProvider)
}

/// 请求头里 key 的放法, 不同上游风格不一.
#[derive(Debug, Clone, Copy)]
pub(super) enum AuthScheme {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// `Authorization: <key>`, 例如智谱.
    Raw,
}

/// 发一个 GET 并解析 JSON, HTTP 状态非成功时返回错误.
pub(super) async fn get_json(
    http: &HttpClient,
    url: &str,
    api_key: &str,
    scheme: AuthScheme,
) -> anyhow::Result<Value> {
    use anyhow::Context;

    let request = http
        .get(url)
        .header("Accept", "application/json")
        .header("Accept-Language", "en-US,en")
        .timeout(REQUEST_TIMEOUT);
    let request = match scheme {
        AuthScheme::Bearer => request.bearer_auth(api_key),
        AuthScheme::Raw => request.header("Authorization", api_key),
    };
    let response = request
        .send()
        .await
        .with_context(|| format!("failed to query plan quota at {url}"))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(anyhow!("HTTP {status}: {body}"));
    }
    Ok(body)
}

/// 读取数字字段, 同时接受数字和数字字符串.
pub(super) fn number(object: &Value, field: &str) -> Option<f64> {
    let value = object.get(field)?;
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

/// `{used, cap}` 形状的窗口: 已用量除以额度得到已用比例.
pub(super) fn ratio_window(
    object: &Value,
    field: &str,
    kind: QuotaWindowKind,
) -> Option<QuotaWindow> {
    let item = object.get(field)?;
    let used = number(item, "used")?;
    let cap = number(item, "cap")?;
    if cap <= 0.0 {
        return None;
    }
    Some(QuotaWindow::new(
        kind,
        (used / cap * 100.0).clamp(0.0, 100.0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_registered_providers_from_base_url() {
        assert_eq!(
            detect("https://api.commandcode.ai/provider/v1").map(|provider| provider.key()),
            Some("commandcode")
        );
        assert_eq!(
            detect("https://open.bigmodel.cn/api/coding/paas/v4").map(|provider| provider.key()),
            Some("zhipu_plan")
        );
        assert_eq!(
            detect("https://api.z.ai/api/paas/v4").map(|provider| provider.key()),
            Some("zhipu_plan")
        );
        assert!(detect("https://api.deepseek.com/v1").is_none());
    }

    #[test]
    fn provider_keys_and_matchers_are_unique() {
        let mut keys = PROVIDERS.iter().map(|spec| spec.key).collect::<Vec<_>>();
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count);

        let mut matchers = PROVIDERS
            .iter()
            .flat_map(|spec| spec.matchers.iter().copied())
            .collect::<Vec<_>>();
        matchers.sort_unstable();
        let count = matchers.len();
        matchers.dedup();
        assert_eq!(matchers.len(), count);
    }

    #[test]
    fn ratio_window_needs_a_positive_cap() {
        let object = serde_json::json!({"fiveHour": {"used": 10.0, "cap": 40.0}});
        let window = ratio_window(&object, "fiveHour", QuotaWindowKind::FiveHour).unwrap();
        assert!((window.used_percent - 25.0).abs() < 1e-9);

        let empty = serde_json::json!({"fiveHour": {"used": 10.0, "cap": 0.0}});
        assert!(ratio_window(&empty, "fiveHour", QuotaWindowKind::FiveHour).is_none());
    }
}

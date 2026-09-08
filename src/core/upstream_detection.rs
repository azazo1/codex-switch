//! 上游识别: 只依据 Base URL 在本地判断上游属于哪一类,
//! 并给出建议的 Wire API 和认证方式, 全程不发起任何网络请求.
//! 判断结果仅作建议, 用户可以随时覆盖.
//!
//! 识别出的模型列表形状会直接参与响应解析, 因此智谱 `/api/v1` 这种
//! 使用 `{"models":[{"slug": ...}]}` 的端点也能被正确读出.

use crate::core::models::{ApiKeyAuthScheme, Upstream, WireApi};
use serde_json::Value;

/// OpenAI 兼容形状的模型列表容器键.
const OPENAI_CONTAINER: &str = "data";

/// 智谱 `/api/v1` 的模型列表容器键.
const ZHIPU_SLUG_CONTAINER: &str = "models";

/// 识别出的上游类型, 决定模型列表的响应形状与默认协议.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedKind {
    /// OpenAI 标准兼容: `{"data":[{"id": ...}]}`.
    OpenAiCompatible,
    /// Anthropic 官方: `{"data":[{"id": ...}]}`, 请求需要 anthropic-version.
    Anthropic,
    /// 智谱 `/api/v1` (`open.bigmodel.cn/api/v1`, `api.z.ai/api/v1`):
    /// `{"models":[{"slug": ...}]}`, 鉴权失败也返回 HTTP 200.
    ZhipuApiV1,
    /// 智谱其他路径 (`.../api/paas/v4`): OpenAI 形状.
    ZhipuPaas,
    /// OpenCode Zen (`opencode.ai`): OpenAI 形状, 只接受 function 工具.
    OpenCode,
    /// 未识别, 按 OpenAI 兼容形状处理.
    Unknown,
}

impl DetectedKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "OpenAI 兼容",
            Self::Anthropic => "Anthropic",
            Self::ZhipuApiV1 => "智谱 /api/v1",
            Self::ZhipuPaas => "智谱 paas",
            Self::OpenCode => "OpenCode",
            Self::Unknown => "未识别",
        }
    }
}

/// 建议值, `None` 表示该维度无法从地址判断.
#[derive(Debug, Clone, Copy, Default)]
pub struct SuggestedSettings {
    pub wire_api: Option<WireApi>,
    pub api_key_auth_scheme: Option<ApiKeyAuthScheme>,
    pub supports_compact: Option<bool>,
    pub filter_chat_server_tools: Option<bool>,
}

impl SuggestedSettings {
    /// 把建议写入上游记录, 返回被改写的维度名称.
    pub fn apply_to(self, upstream: &mut Upstream) -> Vec<&'static str> {
        let mut changed = Vec::new();
        if let Some(wire_api) = self.wire_api
            && upstream.wire_api != wire_api
        {
            upstream.wire_api = wire_api;
            changed.push("Wire API");
        }
        if let Some(scheme) = self.api_key_auth_scheme
            && upstream.api_key_auth_scheme != scheme
        {
            upstream.api_key_auth_scheme = scheme;
            changed.push("API Key 认证");
        }
        if let Some(compact) = self.supports_compact
            && upstream.supports_compact != compact
        {
            upstream.supports_compact = compact;
            changed.push("支持 compact");
        }
        if let Some(filter) = self.filter_chat_server_tools
            && upstream.filter_chat_server_tools != filter
        {
            upstream.filter_chat_server_tools = filter;
            changed.push("过滤 server_tool");
        }
        // Anthropic 上游始终不支持 compact, 这里补齐与编辑器一致的约束.
        if upstream.wire_api == WireApi::AnthropicMessages && upstream.supports_compact {
            upstream.supports_compact = false;
            if !changed.contains(&"支持 compact") {
                changed.push("支持 compact");
            }
        }
        changed
    }
}

/// 上游识别结果.
#[derive(Debug, Clone, Copy)]
pub struct DetectedUpstream {
    pub kind: DetectedKind,
    pub suggestion: SuggestedSettings,
    /// 模型列表响应里条目所在容器的键.
    pub models_container: &'static str,
    /// 模型条目里表示模型 ID 的字段.
    pub models_id_field: &'static str,
    /// 错误是否用 HTTP 200 加信封结构表达.
    pub envelope_errors: bool,
}

impl DetectedUpstream {
    /// 判断一次模型列表查询是否失败, 返回可读错误.
    /// 智谱 `/api/v1` 鉴权失败也返回 200, 只能看信封里的 code 和 success.
    pub fn models_error(self, status_success: bool, value: &Value) -> Option<String> {
        if !status_success {
            return Some(error_text(value));
        }
        if !self.envelope_errors {
            return None;
        }
        let failed = value.get("success").and_then(Value::as_bool) == Some(false)
            || envelope_code_failed(value);
        failed.then(|| error_text(value))
    }

    /// 当前 Base URL 拼不出可用的模型列表地址时, 返回建议地址.
    /// 智谱裸域名下的 `/v1/models` 由 nginx 直接 404, 必须补上路径段.
    pub fn base_url_hint(self, base_url: &str) -> Option<&'static str> {
        if !matches!(
            self.kind,
            DetectedKind::ZhipuApiV1 | DetectedKind::ZhipuPaas
        ) {
            return None;
        }
        if !url_path(base_url).trim_end_matches('/').is_empty() {
            return None;
        }
        let url = base_url.trim().to_ascii_lowercase();
        if url.contains("bigmodel.cn") {
            Some("https://open.bigmodel.cn/api/v1")
        } else if url.contains("z.ai") {
            Some("https://api.z.ai/api/v1")
        } else {
            None
        }
    }
}

/// 未识别地址以及 Codex Switch 自身生成的响应使用的默认形状.
pub const DEFAULT_DETECTION: DetectedUpstream = DetectedUpstream {
    kind: DetectedKind::Unknown,
    suggestion: SuggestedSettings {
        wire_api: None,
        api_key_auth_scheme: None,
        supports_compact: None,
        filter_chat_server_tools: None,
    },
    models_container: OPENAI_CONTAINER,
    models_id_field: "id",
    envelope_errors: false,
};

/// 依据 Base URL 判断上游类型, 纯本地判断.
pub fn detect_upstream(base_url: &str) -> DetectedUpstream {
    let url = base_url.trim().to_ascii_lowercase();
    if url.is_empty() {
        return DEFAULT_DETECTION;
    }
    if url.contains("api.anthropic.com") {
        return anthropic_detection();
    }
    if url.contains("open.bigmodel.cn") || url.contains("api.z.ai") {
        return zhipu_detection(&url);
    }
    if url.contains("opencode.ai") {
        return opencode_detection();
    }
    if is_known_openai_compatible(&url) {
        return openai_compatible_detection();
    }
    DEFAULT_DETECTION
}

fn anthropic_detection() -> DetectedUpstream {
    DetectedUpstream {
        kind: DetectedKind::Anthropic,
        suggestion: SuggestedSettings {
            wire_api: Some(WireApi::AnthropicMessages),
            api_key_auth_scheme: Some(ApiKeyAuthScheme::XApiKey),
            supports_compact: Some(false),
            filter_chat_server_tools: None,
        },
        models_container: OPENAI_CONTAINER,
        models_id_field: "id",
        envelope_errors: false,
    }
}

fn openai_compatible_detection() -> DetectedUpstream {
    DetectedUpstream {
        kind: DetectedKind::OpenAiCompatible,
        suggestion: SuggestedSettings {
            wire_api: Some(WireApi::ChatCompletions),
            api_key_auth_scheme: Some(ApiKeyAuthScheme::Bearer),
            supports_compact: None,
            filter_chat_server_tools: None,
        },
        models_container: OPENAI_CONTAINER,
        models_id_field: "id",
        envelope_errors: false,
    }
}

/// OpenCode Zen (`opencode.ai`): OpenAI Chat Completions 形状,
/// 但只接受 function 工具, 需要过滤 server tool.
fn opencode_detection() -> DetectedUpstream {
    DetectedUpstream {
        kind: DetectedKind::OpenCode,
        suggestion: SuggestedSettings {
            wire_api: Some(WireApi::ChatCompletions),
            api_key_auth_scheme: Some(ApiKeyAuthScheme::Bearer),
            supports_compact: None,
            filter_chat_server_tools: Some(true),
        },
        models_container: OPENAI_CONTAINER,
        models_id_field: "id",
        envelope_errors: false,
    }
}

/// 已知原生以 OpenAI Chat Completions 为主接口的官方站点.
fn is_known_openai_compatible(url: &str) -> bool {
    [
        "api.deepseek.com",
        "api.siliconflow.cn",
        "api.siliconflow.com",
        "api.stepfun.com",
        "api.stepfun.ai",
        "openrouter.ai",
        "api.novita.ai",
    ]
    .iter()
    .any(|host| url.contains(host))
}

fn zhipu_detection(url: &str) -> DetectedUpstream {
    if is_zhipu_api_v1(&url_path(url)) {
        // 该端点同时暴露 /responses 与 /chat/completions, 但模型条目里的
        // apply_patch_tool_type, shell_type, truncation_policy 等字段是 Codex 侧概念,
        // 因此默认按 Responses 处理.
        return DetectedUpstream {
            kind: DetectedKind::ZhipuApiV1,
            suggestion: SuggestedSettings {
                wire_api: Some(WireApi::Responses),
                api_key_auth_scheme: Some(ApiKeyAuthScheme::Bearer),
                supports_compact: None,
                filter_chat_server_tools: None,
            },
            models_container: ZHIPU_SLUG_CONTAINER,
            models_id_field: "slug",
            envelope_errors: true,
        };
    }
    DetectedUpstream {
        kind: DetectedKind::ZhipuPaas,
        suggestion: SuggestedSettings {
            wire_api: Some(WireApi::ChatCompletions),
            api_key_auth_scheme: Some(ApiKeyAuthScheme::Bearer),
            supports_compact: None,
            filter_chat_server_tools: None,
        },
        models_container: OPENAI_CONTAINER,
        models_id_field: "id",
        envelope_errors: false,
    }
}

/// 智谱 `/api/v1` 以 `/api` 或 `/api/v1` 结尾, 默认拼接后落在 `/api/v1/models`.
/// 裸域名也按此处理, 并由 [`DetectedUpstream::base_url_hint`] 提示补全路径.
fn is_zhipu_api_v1(path: &str) -> bool {
    let trimmed = path.trim_end_matches('/');
    trimmed.is_empty() || trimmed.ends_with("/api") || trimmed.ends_with("/api/v1")
}

/// 取 URL 的路径部分; 缺少 scheme 等无法解析时退回原始字符串.
fn url_path(url: &str) -> String {
    url::Url::parse(url)
        .map(|parsed| parsed.path().to_string())
        .unwrap_or_else(|_| url.to_string())
}

fn envelope_code_failed(value: &Value) -> bool {
    match value.get("code") {
        Some(Value::Number(number)) => number.as_i64().is_some_and(|code| code != 200),
        Some(Value::String(text)) => text != "200",
        _ => false,
    }
}

fn error_text(value: &Value) -> String {
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| value.get("msg").and_then(Value::as_str))
        .unwrap_or("models endpoint returned an error")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_zhipu_paths() {
        let detected = detect_upstream("https://open.bigmodel.cn/api/v1");
        assert_eq!(detected.kind, DetectedKind::ZhipuApiV1);
        assert_eq!(detected.models_container, "models");
        assert_eq!(detected.models_id_field, "slug");

        let detected = detect_upstream("https://open.bigmodel.cn/api");
        assert_eq!(detected.kind, DetectedKind::ZhipuApiV1);

        let detected = detect_upstream("https://api.z.ai/api/v1");
        assert_eq!(detected.kind, DetectedKind::ZhipuApiV1);

        let detected = detect_upstream("https://open.bigmodel.cn/api/paas/v4");
        assert_eq!(detected.kind, DetectedKind::ZhipuPaas);
        assert_eq!(detected.models_id_field, "id");

        // 裸域名按 /api/v1 处理, 并提示补全路径.
        let detected = detect_upstream("https://open.bigmodel.cn");
        assert_eq!(detected.kind, DetectedKind::ZhipuApiV1);
        assert_eq!(detected.models_id_field, "slug");
    }

    #[test]
    fn detects_envelope_error() {
        let detected = detect_upstream("https://open.bigmodel.cn/api/v1");
        let message = detected.models_error(
            true,
            &json!({"code":401,"msg":"令牌已过期或验证不正确","success":false}),
        );
        assert_eq!(message.as_deref(), Some("令牌已过期或验证不正确"));
        assert!(detected.models_error(true, &json!({"models":[]})).is_none());
    }

    #[test]
    fn suggests_zhipu_base_url_when_path_missing() {
        let detected = detect_upstream("https://open.bigmodel.cn");
        assert_eq!(
            detected.base_url_hint("https://open.bigmodel.cn"),
            Some("https://open.bigmodel.cn/api/v1")
        );
        assert_eq!(
            detect_upstream("https://api.z.ai").base_url_hint("https://api.z.ai"),
            Some("https://api.z.ai/api/v1")
        );
        // 已经带了路径就不再提示.
        assert!(
            detect_upstream("https://open.bigmodel.cn/api/v1")
                .base_url_hint("https://open.bigmodel.cn/api/v1")
                .is_none()
        );
        assert!(
            detect_upstream("https://open.bigmodel.cn/api/paas/v4")
                .base_url_hint("https://open.bigmodel.cn/api/paas/v4")
                .is_none()
        );
    }

    #[test]
    fn detects_opencode() {
        let detected = detect_upstream("https://opencode.ai/zen/go/v1");
        assert_eq!(detected.kind, DetectedKind::OpenCode);
        assert_eq!(detected.suggestion.wire_api, Some(WireApi::ChatCompletions));
        assert_eq!(detected.suggestion.filter_chat_server_tools, Some(true));
        assert_eq!(detected.models_id_field, "id");

        let detected = detect_upstream("https://opencode.ai/zen/v1");
        assert_eq!(detected.kind, DetectedKind::OpenCode);
    }

    #[test]
    fn unknown_url_keeps_openai_shape() {
        let detected = detect_upstream("https://relay.example.com/v1");
        assert_eq!(detected.kind, DetectedKind::Unknown);
        assert_eq!(detected.models_id_field, "id");
        assert!(detected.suggestion.wire_api.is_none());
    }
}

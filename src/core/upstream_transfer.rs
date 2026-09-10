use crate::core::models::Upstream;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 当前导出格式的版本号, 导入时遇到更高版本直接拒绝.
pub const UPSTREAM_EXPORT_VERSION: u32 = 1;

/// 单个上游的导出条目, credentials 为凭据表中该上游的全部键值对.
/// 导出 JSON 不包含上游 id 和时间戳, 导入时由存储层生成新的 id 和时间戳.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamExportItem {
    #[serde(
        serialize_with = "serialize_upstream",
        deserialize_with = "deserialize_upstream"
    )]
    pub upstream: Upstream,
    pub credentials: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_script: Option<UpstreamPricingScriptExport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamPricingScriptExport {
    pub enabled: bool,
    pub source: String,
}

impl UpstreamPricingScriptExport {
    pub fn from_saved(enabled: bool, source: &str) -> Option<Self> {
        if !enabled && source.is_empty() {
            return None;
        }
        Some(Self {
            enabled,
            source: source.to_string(),
        })
    }
}

/// 上游导出/导入的 JSON 载荷: 顶层版本号 + 上游条目数组, 单条导出即长度为 1 的数组.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamExport {
    pub version: u32,
    pub upstreams: Vec<UpstreamExportItem>,
}

impl UpstreamExport {
    pub fn new(upstreams: Vec<UpstreamExportItem>) -> Self {
        Self {
            version: UPSTREAM_EXPORT_VERSION,
            upstreams,
        }
    }

    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        let export: Self = serde_json::from_str(text.trim())?;
        if export.version > UPSTREAM_EXPORT_VERSION {
            anyhow::bail!(
                "导出格式版本过高: v{}, 当前支持 v{UPSTREAM_EXPORT_VERSION}",
                export.version
            );
        }
        Ok(export)
    }
}

/// 导出时剥离 id 和时间戳, 这些字段由导入方的存储层生成.
fn serialize_upstream<S: serde::Serializer>(
    upstream: &Upstream,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut value = serde_json::to_value(upstream).map_err(serde::ser::Error::custom)?;
    if let Some(obj) = value.as_object_mut() {
        for field in ["id", "created_at", "updated_at"] {
            obj.remove(field);
        }
    }
    value.serialize(serializer)
}

fn deserialize_upstream<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Upstream, D::Error> {
    let mut value = serde_json::Value::deserialize(deserializer)?;
    if let Some(obj) = value.as_object_mut() {
        for field in ["id", "created_at", "updated_at"] {
            obj.remove(field);
        }
        // Upstream 的反序列化要求这些字段存在, 先填充占位值, 导入时会统一重置.
        obj.insert("id".to_string(), serde_json::Value::String(String::new()));
        for field in ["created_at", "updated_at"] {
            obj.insert(
                field.to_string(),
                serde_json::Value::String(UPSTREAM_IMPORT_EPOCH_RFC3339.to_string()),
            );
        }
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}

/// 反序列化占位时间戳, 仅用于让结构体校验通过, 不会写入数据库.
const UPSTREAM_IMPORT_EPOCH_RFC3339: &str = "1970-01-01T00:00:00Z";

use crate::core::models::Upstream;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 当前导出格式的版本号, 导入时遇到更高版本直接拒绝.
pub const UPSTREAM_EXPORT_VERSION: u32 = 1;

/// 单个上游的导出条目, credentials 为凭据表中该上游的全部键值对.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamExportItem {
    pub upstream: Upstream,
    pub credentials: BTreeMap<String, String>,
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

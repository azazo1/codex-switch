use crate::core::models::Upstream;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 当前导出格式的版本号, 导入时遇到更高版本直接拒绝.
pub const UPSTREAM_EXPORT_VERSION: u32 = 1;

/// 单个上游的导出/导入载荷, credentials 为凭据表中该上游的全部键值对.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamExport {
    pub version: u32,
    pub upstream: Upstream,
    pub credentials: BTreeMap<String, String>,
}

impl UpstreamExport {
    pub fn new(upstream: Upstream, credentials: BTreeMap<String, String>) -> Self {
        Self {
            version: UPSTREAM_EXPORT_VERSION,
            upstream,
            credentials,
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

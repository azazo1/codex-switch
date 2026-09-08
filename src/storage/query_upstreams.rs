use crate::core::models::{
    ApiKeyAuthScheme, BalanceProvider, ErrorRetryPolicy, UnknownModalityPolicy, Upstream,
    UpstreamKind, WireApi,
};
use crate::core::upstream_transfer::{
    UpstreamExport, UpstreamExportItem, UPSTREAM_EXPORT_VERSION,
};
use crate::storage::Store;
use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::Row;
use std::collections::BTreeMap;

pub(crate) struct SavedOAuthAccount {
    pub upstream: Upstream,
    pub created: bool,
    pub refreshable: bool,
}

impl Store {
    pub async fn list_upstreams(&self) -> anyhow::Result<Vec<Upstream>> {
        let rows = sqlx::query("SELECT * FROM upstreams ORDER BY priority DESC, created_at ASC")
            .fetch_all(self.pool())
            .await?;
        rows.into_iter().map(row_to_upstream).collect()
    }

    pub async fn enabled_upstreams(&self) -> anyhow::Result<Vec<Upstream>> {
        let rows = sqlx::query(
            "SELECT * FROM upstreams WHERE enabled = 1 ORDER BY priority DESC, created_at ASC",
        )
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(row_to_upstream).collect()
    }

    pub async fn get_upstream(&self, id: &str) -> anyhow::Result<Option<Upstream>> {
        let row = sqlx::query("SELECT * FROM upstreams WHERE id = ?1")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.map(row_to_upstream).transpose()
    }

    pub async fn save_upstream(&self, upstream: &Upstream) -> anyhow::Result<()> {
        let base_url = if upstream.kind == UpstreamKind::PeerNode {
            crate::peer::protocol::parse_peer_address(&upstream.base_url)?
        } else {
            upstream.base_url.clone()
        };
        sqlx::query(
            "INSERT INTO upstreams (
                id, kind, name, base_url, wire_api, api_key_auth_scheme, supports_compact, filter_chat_server_tools, strip_multimodal_for_text_models, unknown_modality_policy, error_retry_policy, price_multiplier,
                enabled, priority, weight, proxy_url, balance_provider, chatgpt_account_id, email,
                plan_type, token_expires_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
             ON CONFLICT(id) DO UPDATE SET
                kind = excluded.kind,
                name = excluded.name,
                base_url = excluded.base_url,
                wire_api = excluded.wire_api,
                api_key_auth_scheme = excluded.api_key_auth_scheme,
                supports_compact = excluded.supports_compact,
                filter_chat_server_tools = excluded.filter_chat_server_tools,
                strip_multimodal_for_text_models = excluded.strip_multimodal_for_text_models,
                unknown_modality_policy = excluded.unknown_modality_policy,
                error_retry_policy = excluded.error_retry_policy,
                price_multiplier = excluded.price_multiplier,
                enabled = excluded.enabled,
                priority = excluded.priority,
                weight = excluded.weight,
                proxy_url = excluded.proxy_url,
                balance_provider = excluded.balance_provider,
                chatgpt_account_id = excluded.chatgpt_account_id,
                email = excluded.email,
                plan_type = excluded.plan_type,
                token_expires_at = excluded.token_expires_at,
                updated_at = excluded.updated_at",
        )
        .bind(&upstream.id)
        .bind(upstream.kind.as_str())
        .bind(&upstream.name)
        .bind(&base_url)
        .bind(upstream.wire_api.as_str())
        .bind(upstream.api_key_auth_scheme.as_str())
        .bind(i64::from(upstream.supports_compact))
        .bind(i64::from(upstream.filter_chat_server_tools))
        .bind(i64::from(upstream.strip_multimodal_for_text_models))
        .bind(upstream.unknown_modality_policy.as_str())
        .bind(upstream.error_retry_policy.as_str())
        .bind(upstream.price_multiplier)
        .bind(i64::from(upstream.enabled))
        .bind(upstream.priority)
        .bind(upstream.weight)
        .bind(&upstream.proxy_url)
        .bind(upstream.balance_provider.as_str())
        .bind(&upstream.chatgpt_account_id)
        .bind(&upstream.email)
        .bind(&upstream.plan_type)
        .bind(upstream.token_expires_at)
        .bind(upstream.created_at.to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        if upstream.kind == UpstreamKind::PeerNode {
            self.sync_paired_peer_address(&upstream.id, &base_url)
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn save_oauth_account(
        &self,
        candidate: &Upstream,
        access_token: &str,
        refresh_token: Option<&str>,
        id_token: Option<&str>,
    ) -> anyhow::Result<SavedOAuthAccount> {
        let account_id = candidate
            .chatgpt_account_id
            .as_deref()
            .context("oauth upstream is missing chatgpt_account_id")?;
        let mut tx = self.pool().begin().await?;
        let existing = sqlx::query(
            "SELECT * FROM upstreams
             WHERE kind = 'codex_oauth' AND chatgpt_account_id = ?1
             ORDER BY created_at ASC, id ASC
             LIMIT 1",
        )
        .bind(account_id)
        .fetch_optional(&mut *tx)
        .await?
        .map(row_to_upstream)
        .transpose()?;
        let created = existing.is_none();
        let mut upstream = existing.unwrap_or_else(|| candidate.clone());
        if !created {
            if candidate.email.is_some() {
                upstream.email.clone_from(&candidate.email);
            }
            if candidate.plan_type.is_some() {
                upstream.plan_type.clone_from(&candidate.plan_type);
            }
            upstream.token_expires_at = candidate.token_expires_at;
            upstream.updated_at = Utc::now();
        }

        if created {
            insert_upstream(&mut tx, &upstream).await?;
        } else {
            sqlx::query(
                "UPDATE upstreams SET
                    email = ?2,
                    plan_type = ?3,
                    token_expires_at = ?4,
                    updated_at = ?5
                 WHERE id = ?1",
            )
            .bind(&upstream.id)
            .bind(&upstream.email)
            .bind(&upstream.plan_type)
            .bind(upstream.token_expires_at)
            .bind(upstream.updated_at.to_rfc3339())
            .execute(&mut *tx)
            .await?;
        }

        save_credential_in_tx(&mut tx, &upstream.id, "access_token", access_token).await?;
        if let Some(refresh_token) = refresh_token {
            save_credential_in_tx(&mut tx, &upstream.id, "refresh_token", refresh_token).await?;
        }
        if let Some(id_token) = id_token {
            save_credential_in_tx(&mut tx, &upstream.id, "id_token", id_token).await?;
        } else {
            sqlx::query("DELETE FROM credentials WHERE upstream_id = ?1 AND name = 'id_token'")
                .bind(&upstream.id)
                .execute(&mut *tx)
                .await?;
        }
        let refreshable = sqlx::query(
            "SELECT COUNT(*) AS count FROM credentials
             WHERE upstream_id = ?1 AND name = 'refresh_token'",
        )
        .bind(&upstream.id)
        .fetch_one(&mut *tx)
        .await?
        .get::<i64, _>("count")
            > 0;
        tx.commit().await?;
        Ok(SavedOAuthAccount {
            upstream,
            created,
            refreshable,
        })
    }

    /// 查询上游的价格倍率, 上游不存在或未指定时返回 1.0.
    pub async fn upstream_price_multiplier(
        &self,
        upstream_id: Option<&str>,
    ) -> anyhow::Result<f64> {
        let Some(id) = upstream_id.map(str::trim).filter(|id| !id.is_empty()) else {
            return Ok(1.0);
        };
        let row = sqlx::query("SELECT price_multiplier FROM upstreams WHERE id = ?1")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        Ok(row
            .map(|row| row.get::<f64, _>("price_multiplier"))
            .unwrap_or(1.0))
    }

    pub async fn set_upstream_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        sqlx::query("UPDATE upstreams SET enabled = ?2, updated_at = ?3 WHERE id = ?1")
            .bind(id)
            .bind(i64::from(enabled))
            .bind(Utc::now().to_rfc3339())
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn update_token_expiry(
        &self,
        id: &str,
        expires_at: Option<i64>,
    ) -> anyhow::Result<()> {
        sqlx::query("UPDATE upstreams SET token_expires_at = ?2, updated_at = ?3 WHERE id = ?1")
            .bind(id)
            .bind(expires_at)
            .bind(Utc::now().to_rfc3339())
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn delete_upstream(&self, id: &str) -> anyhow::Result<()> {
        if let Some(peer) = self.get_node_peer_by_upstream(id).await? {
            self.delete_node_peer(&peer.node_id).await?;
        }
        sqlx::query("DELETE FROM upstream_cache_keepalive_settings WHERE upstream_id = ?1")
            .bind(id)
            .execute(self.pool())
            .await?;
        sqlx::query("DELETE FROM schedule_route_rules WHERE target_upstream_id = ?1")
            .bind(id)
            .execute(self.pool())
            .await?;
        sqlx::query("DELETE FROM upstreams WHERE id = ?1")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn save_credential(
        &self,
        upstream_id: &str,
        name: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO credentials (upstream_id, name, value, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(upstream_id, name) DO UPDATE SET
                value = excluded.value,
                updated_at = excluded.updated_at",
        )
        .bind(upstream_id)
        .bind(name)
        .bind(value)
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn get_credential(
        &self,
        upstream_id: &str,
        name: &str,
    ) -> anyhow::Result<Option<String>> {
        let row = sqlx::query("SELECT value FROM credentials WHERE upstream_id = ?1 AND name = ?2")
            .bind(upstream_id)
            .bind(name)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.map(|r| r.get::<String, _>("value")))
    }

    /// 导出单个上游及其全部凭据, 上游不存在时返回 None.
    pub async fn export_upstream(&self, id: &str) -> anyhow::Result<Option<UpstreamExport>> {
        let Some(upstream) = self.get_upstream(id).await? else {
            return Ok(None);
        };
        tracing::info!(upstream_id = %id, "upstream exported");
        Ok(Some(self.upstream_export(vec![upstream]).await?))
    }

    /// 批量导出上游及其全部凭据, only_enabled 为 true 时仅导出已启用的上游.
    pub async fn export_upstreams(&self, only_enabled: bool) -> anyhow::Result<UpstreamExport> {
        let upstreams = if only_enabled {
            self.enabled_upstreams().await?
        } else {
            self.list_upstreams().await?
        };
        tracing::info!(
            count = upstreams.len(),
            only_enabled,
            "upstreams batch exported"
        );
        self.upstream_export(upstreams).await
    }

    async fn upstream_export(&self, upstreams: Vec<Upstream>) -> anyhow::Result<UpstreamExport> {
        let mut items = Vec::with_capacity(upstreams.len());
        for upstream in upstreams {
            let rows = sqlx::query(
                "SELECT name, value FROM credentials WHERE upstream_id = ?1 ORDER BY name",
            )
            .bind(&upstream.id)
            .fetch_all(self.pool())
            .await?;
            let credentials: BTreeMap<String, String> = rows
                .into_iter()
                .map(|row| {
                    (
                        row.get::<String, _>("name"),
                        row.get::<String, _>("value"),
                    )
                })
                .collect();
            items.push(UpstreamExportItem {
                upstream,
                credentials,
            });
        }
        Ok(UpstreamExport::new(items))
    }

    /// 批量导入上游, 单个事务完成, 任一条失败则整体回滚.
    pub async fn import_upstreams(
        &self,
        payload: &UpstreamExport,
    ) -> anyhow::Result<Vec<Upstream>> {
        if payload.version > UPSTREAM_EXPORT_VERSION {
            anyhow::bail!(
                "导出格式版本过高: v{}, 当前支持 v{UPSTREAM_EXPORT_VERSION}",
                payload.version
            );
        }
        let mut tx = self.pool().begin().await?;
        let mut imported = Vec::with_capacity(payload.upstreams.len());
        for item in &payload.upstreams {
            let mut upstream = item.upstream.clone();
            upstream.id = uuid::Uuid::new_v4().to_string();
            let now = Utc::now();
            upstream.created_at = now;
            upstream.updated_at = now;
            insert_upstream(&mut tx, &upstream).await?;
            for (name, value) in &item.credentials {
                save_credential_in_tx(&mut tx, &upstream.id, name, value).await?;
            }
            tracing::info!(
                upstream_id = %upstream.id,
                name = %upstream.name,
                credential_count = item.credentials.len(),
                "upstream imported"
            );
            imported.push(upstream);
        }
        tx.commit().await?;
        Ok(imported)
    }
}

async fn insert_upstream(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    upstream: &Upstream,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO upstreams (
            id, kind, name, base_url, wire_api, api_key_auth_scheme, supports_compact, filter_chat_server_tools, strip_multimodal_for_text_models, unknown_modality_policy, error_retry_policy, price_multiplier,
            enabled, priority, weight, proxy_url, balance_provider, chatgpt_account_id, email,
            plan_type, token_expires_at, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
    )
    .bind(&upstream.id)
    .bind(upstream.kind.as_str())
    .bind(&upstream.name)
    .bind(&upstream.base_url)
    .bind(upstream.wire_api.as_str())
    .bind(upstream.api_key_auth_scheme.as_str())
    .bind(i64::from(upstream.supports_compact))
    .bind(i64::from(upstream.filter_chat_server_tools))
    .bind(i64::from(upstream.strip_multimodal_for_text_models))
    .bind(upstream.unknown_modality_policy.as_str())
    .bind(upstream.error_retry_policy.as_str())
    .bind(upstream.price_multiplier)
    .bind(i64::from(upstream.enabled))
    .bind(upstream.priority)
    .bind(upstream.weight)
    .bind(&upstream.proxy_url)
    .bind(upstream.balance_provider.as_str())
    .bind(&upstream.chatgpt_account_id)
    .bind(&upstream.email)
    .bind(&upstream.plan_type)
    .bind(upstream.token_expires_at)
    .bind(upstream.created_at.to_rfc3339())
    .bind(upstream.updated_at.to_rfc3339())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn save_credential_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    upstream_id: &str,
    name: &str,
    value: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO credentials (upstream_id, name, value, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(upstream_id, name) DO UPDATE SET
            value = excluded.value,
            updated_at = excluded.updated_at",
    )
    .bind(upstream_id)
    .bind(name)
    .bind(value)
    .bind(Utc::now().to_rfc3339())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(super) fn row_to_upstream(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<Upstream> {
    let created_at: String = row.get("created_at");
    let updated_at: String = row.get("updated_at");
    Ok(Upstream {
        id: row.get("id"),
        kind: UpstreamKind::from_str(&row.get::<String, _>("kind")),
        name: row.get("name"),
        base_url: row.get("base_url"),
        wire_api: WireApi::from_str(&row.get::<String, _>("wire_api")),
        api_key_auth_scheme: ApiKeyAuthScheme::from_str(
            &row.get::<String, _>("api_key_auth_scheme"),
        ),
        supports_compact: row.get::<i64, _>("supports_compact") != 0,
        filter_chat_server_tools: row.get::<i64, _>("filter_chat_server_tools") != 0,
        strip_multimodal_for_text_models: row.get::<i64, _>("strip_multimodal_for_text_models")
            != 0,
        unknown_modality_policy: UnknownModalityPolicy::from_str(
            &row.get::<String, _>("unknown_modality_policy"),
        ),
        error_retry_policy: ErrorRetryPolicy::from_str(&row.get::<String, _>("error_retry_policy")),
        price_multiplier: row.get::<f64, _>("price_multiplier"),
        enabled: row.get::<i64, _>("enabled") != 0,
        priority: row.get("priority"),
        weight: row.get("weight"),
        proxy_url: row.get("proxy_url"),
        balance_provider: BalanceProvider::from_str(&row.get::<String, _>("balance_provider")),
        chatgpt_account_id: row.get("chatgpt_account_id"),
        email: row.get("email"),
        plan_type: row.get("plan_type"),
        token_expires_at: row.get("token_expires_at"),
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .context("invalid created_at")?
            .with_timezone(&Utc),
        updated_at: DateTime::parse_from_rfc3339(&updated_at)
            .context("invalid updated_at")?
            .with_timezone(&Utc),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn persists_error_retry_policy_and_auth_scheme() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-upstream-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(path).await.unwrap();
        let mut upstream = Upstream::new_relay(
            "relay".to_string(),
            "https://example.com/v1".to_string(),
            WireApi::Responses,
            false,
            BalanceProvider::Unsupported,
        );
        upstream.error_retry_policy = ErrorRetryPolicy::All;
        upstream.api_key_auth_scheme = ApiKeyAuthScheme::XApiKey;

        store.save_upstream(&upstream).await.unwrap();
        let saved = store.get_upstream(&upstream.id).await.unwrap().unwrap();

        assert_eq!(saved.error_retry_policy, ErrorRetryPolicy::All);
        assert_eq!(saved.api_key_auth_scheme, ApiKeyAuthScheme::XApiKey);
    }

    #[tokio::test]
    async fn persists_chat_server_tool_filter() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-upstream-filter-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(path).await.unwrap();
        let mut upstream = Upstream::new_relay(
            "opencode".to_string(),
            "https://opencode.ai/zen/go/v1".to_string(),
            WireApi::ChatCompletions,
            false,
            BalanceProvider::Unsupported,
        );
        upstream.filter_chat_server_tools = true;

        store.save_upstream(&upstream).await.unwrap();
        let saved = store.get_upstream(&upstream.id).await.unwrap().unwrap();

        assert!(saved.filter_chat_server_tools);
    }

    #[tokio::test]
    async fn persists_multimodal_stripping_setting() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-upstream-multimodal-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(path).await.unwrap();
        let mut upstream = Upstream::new_relay(
            "deepseek".to_string(),
            "https://example.com/v1".to_string(),
            WireApi::Responses,
            false,
            BalanceProvider::Unsupported,
        );
        upstream.strip_multimodal_for_text_models = true;

        store.save_upstream(&upstream).await.unwrap();
        let saved = store.get_upstream(&upstream.id).await.unwrap().unwrap();

        assert!(saved.strip_multimodal_for_text_models);
    }

    #[tokio::test]
    async fn persists_price_multiplier() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-upstream-multiplier-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(path).await.unwrap();
        let mut upstream = Upstream::new_relay(
            "relay".to_string(),
            "https://example.com/v1".to_string(),
            WireApi::Responses,
            false,
            BalanceProvider::Unsupported,
        );
        assert_eq!(upstream.price_multiplier, 1.0);
        upstream.price_multiplier = 2.5;

        store.save_upstream(&upstream).await.unwrap();
        let saved = store.get_upstream(&upstream.id).await.unwrap().unwrap();

        assert_eq!(saved.price_multiplier, 2.5);
        assert_eq!(
            store
                .upstream_price_multiplier(Some(&upstream.id))
                .await
                .unwrap(),
            2.5
        );
        assert_eq!(store.upstream_price_multiplier(None).await.unwrap(), 1.0);
        assert_eq!(
            store
                .upstream_price_multiplier(Some("missing"))
                .await
                .unwrap(),
            1.0
        );
    }

    #[tokio::test]
    async fn persists_unknown_modality_policy() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-upstream-modality-policy-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(path).await.unwrap();
        let mut upstream = Upstream::new_relay(
            "deepseek".to_string(),
            "https://example.com/v1".to_string(),
            WireApi::Responses,
            false,
            BalanceProvider::Unsupported,
        );
        upstream.unknown_modality_policy = UnknownModalityPolicy::Multimodal;

        store.save_upstream(&upstream).await.unwrap();
        let saved = store.get_upstream(&upstream.id).await.unwrap().unwrap();

        assert_eq!(
            saved.unknown_modality_policy,
            UnknownModalityPolicy::Multimodal
        );
    }

    #[tokio::test]
    async fn exports_and_imports_upstream_with_credentials() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-upstream-transfer-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(path).await.unwrap();
        let upstream = Upstream::new_relay(
            "relay".to_string(),
            "https://example.com/v1".to_string(),
            WireApi::Responses,
            false,
            BalanceProvider::Unsupported,
        );
        store.save_upstream(&upstream).await.unwrap();
        store
            .save_credential(&upstream.id, "api_key", "sk-test")
            .await
            .unwrap();

        let export = store
            .export_upstream(&upstream.id)
            .await
            .unwrap()
            .expect("upstream exists");
        let json = export.to_json().unwrap();
        assert!(!json.contains(&upstream.id), "导出 JSON 不应包含上游 id");
        assert!(
            !json.contains("created_at") && !json.contains("updated_at"),
            "导出 JSON 不应包含时间戳"
        );
        let parsed = UpstreamExport::from_json(&json).unwrap();
        assert_eq!(parsed.upstreams.len(), 1);
        let item = &parsed.upstreams[0];
        assert!(item.upstream.id.is_empty());
        assert_eq!(item.upstream.created_at.timestamp(), 0);
        assert_eq!(
            item.credentials.get("api_key").map(String::as_str),
            Some("sk-test")
        );

        let mut imported = store.import_upstreams(&parsed).await.unwrap();
        let imported = imported.pop().unwrap();
        assert_ne!(imported.id, upstream.id, "导入时应生成新 id");
        assert_eq!(imported.name, upstream.name);
        assert_eq!(imported.base_url, upstream.base_url);
        assert_eq!(
            store
                .get_credential(&imported.id, "api_key")
                .await
                .unwrap()
                .as_deref(),
            Some("sk-test")
        );

        assert!(store.export_upstream("missing-id").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn exports_and_imports_upstream_batches() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-upstream-batch-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(path).await.unwrap();
        for name in ["relay-a", "relay-b"] {
            let upstream = Upstream::new_relay(
                name.to_string(),
                format!("https://{name}.example.com/v1"),
                WireApi::Responses,
                false,
                BalanceProvider::Unsupported,
            );
            store.save_upstream(&upstream).await.unwrap();
            store
                .save_credential(&upstream.id, "api_key", &format!("sk-{name}"))
                .await
                .unwrap();
        }
        // 再添加一个禁用的上游, 验证 only_enabled 过滤.
        let mut disabled = Upstream::new_relay(
            "relay-off".to_string(),
            "https://off.example.com/v1".to_string(),
            WireApi::Responses,
            false,
            BalanceProvider::Unsupported,
        );
        disabled.enabled = false;
        store.save_upstream(&disabled).await.unwrap();

        let batch = store.export_upstreams(false).await.unwrap();
        assert_eq!(batch.upstreams.len(), 3);
        let json = batch.to_json().unwrap();

        let target = Store::open(
            std::env::temp_dir().join(format!(
                "codex-switch-upstream-batch-target-{}.sqlite",
                uuid::Uuid::new_v4()
            )),
        )
        .await
        .unwrap();
        let payloads = UpstreamExport::from_json(&json).unwrap();
        assert_eq!(payloads.upstreams.len(), 3);
        let imported = target.import_upstreams(&payloads).await.unwrap();
        assert_eq!(imported.len(), 3);
        assert_eq!(
            target
                .get_credential(&imported[0].id, "api_key")
                .await
                .unwrap()
                .as_deref(),
            Some("sk-relay-a")
        );

        let enabled = store.export_upstreams(true).await.unwrap();
        assert_eq!(enabled.upstreams.len(), 2);
    }
}

use crate::core::models::{BalanceProvider, Upstream, UpstreamKind, WireApi};
use crate::storage::Store;
use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::Row;

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
        sqlx::query(
            "INSERT INTO upstreams (
                id, kind, name, base_url, wire_api, supports_compact, enabled, priority, weight,
                balance_provider, chatgpt_account_id, email, plan_type, token_expires_at,
                created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT(id) DO UPDATE SET
                kind = excluded.kind,
                name = excluded.name,
                base_url = excluded.base_url,
                wire_api = excluded.wire_api,
                supports_compact = excluded.supports_compact,
                enabled = excluded.enabled,
                priority = excluded.priority,
                weight = excluded.weight,
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
        .bind(&upstream.base_url)
        .bind(upstream.wire_api.as_str())
        .bind(i64::from(upstream.supports_compact))
        .bind(i64::from(upstream.enabled))
        .bind(upstream.priority)
        .bind(upstream.weight)
        .bind(upstream.balance_provider.as_str())
        .bind(&upstream.chatgpt_account_id)
        .bind(&upstream.email)
        .bind(&upstream.plan_type)
        .bind(upstream.token_expires_at)
        .bind(upstream.created_at.to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
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
        supports_compact: row.get::<i64, _>("supports_compact") != 0,
        enabled: row.get::<i64, _>("enabled") != 0,
        priority: row.get("priority"),
        weight: row.get("weight"),
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

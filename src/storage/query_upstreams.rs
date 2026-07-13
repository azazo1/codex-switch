use crate::core::models::{BalanceProvider, Upstream, UpstreamKind, WireApi};
use crate::storage::Store;
use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::Row;

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
        sqlx::query(
            "INSERT INTO upstreams (
                id, kind, name, base_url, wire_api, supports_compact, enabled, priority, weight,
                proxy_url, balance_provider, chatgpt_account_id, email, plan_type, token_expires_at,
                created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(id) DO UPDATE SET
                kind = excluded.kind,
                name = excluded.name,
                base_url = excluded.base_url,
                wire_api = excluded.wire_api,
                supports_compact = excluded.supports_compact,
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
        .bind(&upstream.base_url)
        .bind(upstream.wire_api.as_str())
        .bind(i64::from(upstream.supports_compact))
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

async fn insert_upstream(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    upstream: &Upstream,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO upstreams (
            id, kind, name, base_url, wire_api, supports_compact, enabled, priority, weight,
            proxy_url, balance_provider, chatgpt_account_id, email, plan_type, token_expires_at,
            created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
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
        supports_compact: row.get::<i64, _>("supports_compact") != 0,
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

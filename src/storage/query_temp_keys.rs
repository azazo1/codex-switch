use crate::core::models::{TemporaryAccessKey, TokenUsage};
use crate::storage::Store;
use chrono::{DateTime, Utc};
use sqlx::Row;

impl Store {
    pub async fn create_temporary_access_key(
        &self,
        key: &TemporaryAccessKey,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO temporary_access_keys (
                id, name, key_value, enabled, request_limit, token_limit, expires_at,
                requests_used, tokens_used, last_used_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )
        .bind(&key.id)
        .bind(&key.name)
        .bind(&key.key_value)
        .bind(key.enabled)
        .bind(key.request_limit)
        .bind(key.token_limit)
        .bind(key.expires_at)
        .bind(key.requests_used)
        .bind(key.tokens_used)
        .bind(key.last_used_at)
        .bind(key.created_at.to_rfc3339())
        .bind(key.updated_at.to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn list_temporary_access_keys(&self) -> anyhow::Result<Vec<TemporaryAccessKey>> {
        let rows = sqlx::query("SELECT * FROM temporary_access_keys ORDER BY created_at DESC")
            .fetch_all(self.pool())
            .await?;
        Ok(rows.into_iter().map(temporary_access_key_from_row).collect())
    }

    pub async fn find_temporary_access_key(
        &self,
        key_value: &str,
    ) -> anyhow::Result<Option<TemporaryAccessKey>> {
        let row = sqlx::query("SELECT * FROM temporary_access_keys WHERE key_value = ?1")
            .bind(key_value)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.map(temporary_access_key_from_row))
    }

    pub async fn set_temporary_access_key_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE temporary_access_keys
             SET enabled = ?2, updated_at = ?3
             WHERE id = ?1",
        )
        .bind(id)
        .bind(enabled)
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn update_temporary_access_key(
        &self,
        id: &str,
        name: &str,
        key_value: &str,
        request_limit: Option<i64>,
        token_limit: Option<i64>,
        expires_at: Option<i64>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE temporary_access_keys
             SET name = ?2, key_value = ?3, request_limit = ?4, token_limit = ?5,
                 expires_at = ?6, updated_at = ?7
             WHERE id = ?1",
        )
        .bind(id)
        .bind(name)
        .bind(key_value)
        .bind(request_limit)
        .bind(token_limit)
        .bind(expires_at)
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn delete_temporary_access_key(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM temporary_access_keys WHERE id = ?1")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn record_temporary_access_key_success(
        &self,
        id: &str,
        usage: &TokenUsage,
    ) -> anyhow::Result<()> {
        let now = Utc::now().timestamp();
        sqlx::query(
            "UPDATE temporary_access_keys
             SET requests_used = requests_used + 1,
                 tokens_used = tokens_used + ?2,
                 last_used_at = ?3,
                 updated_at = ?4
             WHERE id = ?1",
        )
        .bind(id)
        .bind(usage.total_tokens)
        .bind(now)
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

fn temporary_access_key_from_row(row: sqlx::sqlite::SqliteRow) -> TemporaryAccessKey {
    let created_at: String = row.get("created_at");
    let updated_at: String = row.get("updated_at");
    TemporaryAccessKey {
        id: row.get("id"),
        name: row.get("name"),
        key_value: row.get("key_value"),
        enabled: row.get("enabled"),
        request_limit: row.get("request_limit"),
        token_limit: row.get("token_limit"),
        expires_at: row.get("expires_at"),
        requests_used: row.get("requests_used"),
        tokens_used: row.get("tokens_used"),
        last_used_at: row.get("last_used_at"),
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        updated_at: DateTime::parse_from_rfc3339(&updated_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stores_lists_and_finds_temporary_keys() {
        let path =
            std::env::temp_dir().join(format!("codex-switch-temp-key-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(path).await.unwrap();
        let key = TemporaryAccessKey::new(
            "key-one".to_string(),
            "shared".to_string(),
            "cs-tmp-test".to_string(),
            Some(3),
            Some(1000),
            Some(Utc::now().timestamp() + 60),
        );

        store.create_temporary_access_key(&key).await.unwrap();

        let list = store.list_temporary_access_keys().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "key-one");
        assert_eq!(list[0].key_value, "cs-tmp-test");
        let found = store
            .find_temporary_access_key("cs-tmp-test")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.name, "shared");
        assert!(found.enabled);
        assert_eq!(found.request_limit, Some(3));
        assert_eq!(found.token_limit, Some(1000));
    }

    #[tokio::test]
    async fn records_success_usage_and_updates_enabled_state() {
        let path =
            std::env::temp_dir().join(format!("codex-switch-temp-key-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(path).await.unwrap();
        let key = TemporaryAccessKey::new(
            "key-two".to_string(),
            String::new(),
            "cs-tmp-usage".to_string(),
            None,
            None,
            None,
        );
        store.create_temporary_access_key(&key).await.unwrap();

        store
            .record_temporary_access_key_success(
                "key-two",
                &TokenUsage {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: 20,
                    cache_creation_tokens: 10,
                    total_tokens: 180,
                },
            )
            .await
            .unwrap();
        let updated = store
            .find_temporary_access_key("cs-tmp-usage")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.requests_used, 1);
        assert_eq!(updated.tokens_used, 180);
        assert!(updated.last_used_at.is_some());

        store
            .set_temporary_access_key_enabled("key-two", false)
            .await
            .unwrap();
        assert!(
            !store
                .find_temporary_access_key("cs-tmp-usage")
                .await
                .unwrap()
                .unwrap()
                .enabled
        );

        store.delete_temporary_access_key("key-two").await.unwrap();
        assert!(
            store
                .find_temporary_access_key("cs-tmp-usage")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn updates_temporary_key_limits_and_key_value() {
        let path =
            std::env::temp_dir().join(format!("codex-switch-temp-key-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(path).await.unwrap();
        let key = TemporaryAccessKey::new(
            "key-three".to_string(),
            "old".to_string(),
            "cs-tmp-update".to_string(),
            Some(1),
            Some(100),
            Some(Utc::now().timestamp() + 60),
        );
        store.create_temporary_access_key(&key).await.unwrap();

        store
            .update_temporary_access_key(
                "key-three",
                "new-name",
                "cs-tmp-renamed",
                Some(5),
                Some(500),
                Some(Utc::now().timestamp() + 3600),
            )
            .await
            .unwrap();

        let updated = store
            .find_temporary_access_key("cs-tmp-renamed")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.name, "new-name");
        assert_eq!(updated.key_value, "cs-tmp-renamed");
        assert_eq!(updated.request_limit, Some(5));
        assert_eq!(updated.token_limit, Some(500));
        assert!(updated.enabled);

        let old = store
            .find_temporary_access_key("cs-tmp-update")
            .await
            .unwrap();
        assert!(old.is_none());
    }
}

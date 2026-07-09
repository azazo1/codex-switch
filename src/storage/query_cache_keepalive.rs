use crate::core::models::{CacheKeepaliveMode, UpstreamCacheKeepaliveSettings};
use crate::storage::Store;
use chrono::Utc;
use sqlx::Row;

impl Store {
    pub async fn cache_keepalive_settings(
        &self,
        upstream_id: &str,
    ) -> anyhow::Result<UpstreamCacheKeepaliveSettings> {
        let row =
            sqlx::query("SELECT * FROM upstream_cache_keepalive_settings WHERE upstream_id = ?1")
                .bind(upstream_id)
                .fetch_optional(self.pool())
                .await?;
        Ok(row
            .map(row_to_settings)
            .unwrap_or_else(|| UpstreamCacheKeepaliveSettings::new(upstream_id.to_string())))
    }

    pub async fn save_cache_keepalive_settings(
        &self,
        settings: &UpstreamCacheKeepaliveSettings,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO upstream_cache_keepalive_settings (
                upstream_id, enabled, mode, interval_seconds, max_idle_seconds,
                min_cacheable_tokens, max_cacheable_tokens, max_active_sessions,
                prefer_extended_retention, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(upstream_id) DO UPDATE SET
                enabled = excluded.enabled,
                mode = excluded.mode,
                interval_seconds = excluded.interval_seconds,
                max_idle_seconds = excluded.max_idle_seconds,
                min_cacheable_tokens = excluded.min_cacheable_tokens,
                max_cacheable_tokens = excluded.max_cacheable_tokens,
                max_active_sessions = excluded.max_active_sessions,
                prefer_extended_retention = excluded.prefer_extended_retention,
                updated_at = excluded.updated_at",
        )
        .bind(&settings.upstream_id)
        .bind(i64::from(settings.enabled))
        .bind(settings.mode.as_str())
        .bind(settings.interval_seconds.max(60))
        .bind(settings.max_idle_seconds.max(60))
        .bind(settings.min_cacheable_tokens.max(1024))
        .bind(normalized_max_cacheable_tokens(settings))
        .bind(settings.max_active_sessions.max(1))
        .bind(i64::from(settings.prefer_extended_retention))
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

fn row_to_settings(row: sqlx::sqlite::SqliteRow) -> UpstreamCacheKeepaliveSettings {
    UpstreamCacheKeepaliveSettings {
        upstream_id: row.get("upstream_id"),
        enabled: row.get::<i64, _>("enabled") != 0,
        mode: CacheKeepaliveMode::from_str(&row.get::<String, _>("mode")),
        interval_seconds: row.get("interval_seconds"),
        max_idle_seconds: row.get("max_idle_seconds"),
        min_cacheable_tokens: row.get("min_cacheable_tokens"),
        max_cacheable_tokens: row.get("max_cacheable_tokens"),
        max_active_sessions: row.get("max_active_sessions"),
        prefer_extended_retention: row.get::<i64, _>("prefer_extended_retention") != 0,
    }
}

fn normalized_max_cacheable_tokens(settings: &UpstreamCacheKeepaliveSettings) -> i64 {
    settings
        .max_cacheable_tokens
        .max(settings.min_cacheable_tokens.max(1024))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{BalanceProvider, Upstream, WireApi};

    #[tokio::test]
    async fn settings_are_scoped_per_upstream() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-cache-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(path).await.unwrap();
        let first = upstream("first");
        let second = upstream("second");
        store.save_upstream(&first).await.unwrap();
        store.save_upstream(&second).await.unwrap();

        let mut settings = UpstreamCacheKeepaliveSettings::new(first.id.clone());
        settings.enabled = true;
        settings.mode = CacheKeepaliveMode::Always;
        settings.max_cacheable_tokens = 230_000;
        store
            .save_cache_keepalive_settings(&settings)
            .await
            .unwrap();

        let first_settings = store.cache_keepalive_settings(&first.id).await.unwrap();
        let second_settings = store.cache_keepalive_settings(&second.id).await.unwrap();

        assert!(first_settings.enabled);
        assert_eq!(first_settings.mode, CacheKeepaliveMode::Always);
        assert_eq!(first_settings.max_cacheable_tokens, 230_000);
        assert!(!second_settings.enabled);
        assert_eq!(second_settings.mode, CacheKeepaliveMode::Smart);
    }

    fn upstream(id: &str) -> Upstream {
        let mut upstream = Upstream::new_relay(
            id.to_string(),
            "http://127.0.0.1".to_string(),
            WireApi::Responses,
            true,
            BalanceProvider::Unsupported,
        );
        upstream.id = id.to_string();
        upstream
    }
}

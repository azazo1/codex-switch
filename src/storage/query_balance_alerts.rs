use crate::core::models::UpstreamBalanceAlertSettings;
use crate::storage::Store;
use chrono::Utc;
use sqlx::Row;

impl Store {
    pub async fn balance_alert_settings(
        &self,
        upstream_id: &str,
    ) -> anyhow::Result<UpstreamBalanceAlertSettings> {
        let row = sqlx::query(
            "SELECT * FROM upstream_balance_alert_settings WHERE upstream_id = ?1",
        )
        .bind(upstream_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row
            .map(row_to_settings)
            .unwrap_or_else(|| UpstreamBalanceAlertSettings::new(upstream_id.to_string())))
    }

    pub async fn list_enabled_balance_alert_settings(
        &self,
    ) -> anyhow::Result<Vec<UpstreamBalanceAlertSettings>> {
        let rows = sqlx::query(
            "SELECT * FROM upstream_balance_alert_settings WHERE enabled = 1 ORDER BY upstream_id",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(row_to_settings).collect())
    }

    pub async fn save_balance_alert_settings(
        &self,
        settings: &UpstreamBalanceAlertSettings,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO upstream_balance_alert_settings (
                upstream_id, enabled, threshold, interval_seconds, last_checked_at,
                alert_active, updated_at
             ) VALUES (?1, ?2, ?3, ?4, NULL, 0, ?5)
             ON CONFLICT(upstream_id) DO UPDATE SET
                enabled = excluded.enabled,
                threshold = excluded.threshold,
                interval_seconds = excluded.interval_seconds,
                last_checked_at = CASE
                    WHEN upstream_balance_alert_settings.enabled != excluded.enabled
                      OR upstream_balance_alert_settings.threshold != excluded.threshold
                      OR upstream_balance_alert_settings.interval_seconds != excluded.interval_seconds
                    THEN NULL
                    ELSE upstream_balance_alert_settings.last_checked_at
                END,
                alert_active = CASE
                    WHEN upstream_balance_alert_settings.enabled != excluded.enabled
                      OR upstream_balance_alert_settings.threshold != excluded.threshold
                      OR upstream_balance_alert_settings.interval_seconds != excluded.interval_seconds
                    THEN 0
                    ELSE upstream_balance_alert_settings.alert_active
                END,
                updated_at = excluded.updated_at",
        )
        .bind(&settings.upstream_id)
        .bind(i64::from(settings.enabled))
        .bind(settings.threshold.max(0.0))
        .bind(settings.interval_seconds.max(60))
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn mark_balance_alert_checked(
        &self,
        upstream_id: &str,
        checked_at: i64,
        alert_active: bool,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE upstream_balance_alert_settings
             SET last_checked_at = ?2, alert_active = ?3, updated_at = ?4
             WHERE upstream_id = ?1",
        )
        .bind(upstream_id)
        .bind(checked_at)
        .bind(i64::from(alert_active))
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

fn row_to_settings(row: sqlx::sqlite::SqliteRow) -> UpstreamBalanceAlertSettings {
    UpstreamBalanceAlertSettings {
        upstream_id: row.get("upstream_id"),
        enabled: row.get::<i64, _>("enabled") != 0,
        threshold: row.get("threshold"),
        interval_seconds: row.get("interval_seconds"),
        last_checked_at: row.get("last_checked_at"),
        alert_active: row.get::<i64, _>("alert_active") != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{BalanceProvider, Upstream, WireApi};

    #[tokio::test]
    async fn settings_are_scoped_per_upstream_and_reset_alert_state() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-balance-alert-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(path).await.unwrap();
        let upstream = Upstream::new_relay(
            "first".to_string(),
            "https://example.com".to_string(),
            WireApi::Responses,
            true,
            BalanceProvider::Auto,
        );
        store.save_upstream(&upstream).await.unwrap();

        let mut settings = UpstreamBalanceAlertSettings::new(upstream.id.clone());
        settings.enabled = true;
        settings.threshold = 12.5;
        settings.interval_seconds = 600;
        store.save_balance_alert_settings(&settings).await.unwrap();
        store
            .mark_balance_alert_checked(&upstream.id, 1234, true)
            .await
            .unwrap();

        let saved = store.balance_alert_settings(&upstream.id).await.unwrap();
        assert!(saved.enabled);
        assert_eq!(saved.threshold, 12.5);
        assert_eq!(saved.interval_seconds, 600);
        assert_eq!(saved.last_checked_at, Some(1234));
        assert!(saved.alert_active);

        settings.threshold = 20.0;
        store.save_balance_alert_settings(&settings).await.unwrap();
        let reset = store.balance_alert_settings(&upstream.id).await.unwrap();
        assert_eq!(reset.last_checked_at, None);
        assert!(!reset.alert_active);
    }
}

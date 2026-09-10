use crate::storage::Store;
use chrono::Utc;
use sqlx::Row;

#[derive(Debug, Clone, Default)]
pub struct UpstreamPricingScript {
    pub upstream_id: String,
    pub enabled: bool,
    pub source: String,
}

impl Store {
    pub async fn list_upstream_pricing_scripts(
        &self,
    ) -> anyhow::Result<Vec<UpstreamPricingScript>> {
        let rows = sqlx::query(
            "SELECT upstream_id, enabled, source FROM upstream_pricing_scripts ORDER BY upstream_id",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(row_to_script).collect())
    }

    pub async fn get_upstream_pricing_script(
        &self,
        upstream_id: &str,
    ) -> anyhow::Result<Option<UpstreamPricingScript>> {
        let row = sqlx::query(
            "SELECT upstream_id, enabled, source FROM upstream_pricing_scripts WHERE upstream_id = ?1",
        )
        .bind(upstream_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(row_to_script))
    }

    pub async fn save_upstream_pricing_script(
        &self,
        script: &UpstreamPricingScript,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO upstream_pricing_scripts (upstream_id, enabled, source, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(upstream_id) DO UPDATE SET
                enabled = excluded.enabled,
                source = excluded.source,
                updated_at = excluded.updated_at",
        )
        .bind(&script.upstream_id)
        .bind(i64::from(script.enabled))
        .bind(&script.source)
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

pub(super) async fn save_upstream_pricing_script_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    script: &UpstreamPricingScript,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO upstream_pricing_scripts (upstream_id, enabled, source, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(upstream_id) DO UPDATE SET
            enabled = excluded.enabled,
            source = excluded.source,
            updated_at = excluded.updated_at",
    )
    .bind(&script.upstream_id)
    .bind(i64::from(script.enabled))
    .bind(&script.source)
    .bind(Utc::now().to_rfc3339())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn row_to_script(row: sqlx::sqlite::SqliteRow) -> UpstreamPricingScript {
    UpstreamPricingScript {
        upstream_id: row.get("upstream_id"),
        enabled: row.get::<i64, _>("enabled") != 0,
        source: row.get("source"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{BalanceProvider, Upstream, WireApi};

    #[tokio::test]
    async fn persists_upstream_pricing_script() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-upstream-pricing-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(&path).await.unwrap();
        let upstream = Upstream::new_relay(
            "relay".to_string(),
            "https://example.com/v1".to_string(),
            WireApi::Responses,
            false,
            BalanceProvider::Unsupported,
        );
        store.save_upstream(&upstream).await.unwrap();
        store
            .save_upstream_pricing_script(&UpstreamPricingScript {
                upstream_id: upstream.id.clone(),
                enabled: true,
                source: "fn estimate(ctx) { 1.0 }".to_string(),
            })
            .await
            .unwrap();

        let loaded = store
            .get_upstream_pricing_script(&upstream.id)
            .await
            .unwrap()
            .unwrap();
        assert!(loaded.enabled);
        assert_eq!(loaded.source, "fn estimate(ctx) { 1.0 }");

        store.delete_upstream(&upstream.id).await.unwrap();
        assert!(
            store
                .get_upstream_pricing_script(&upstream.id)
                .await
                .unwrap()
                .is_none()
        );
    }
}

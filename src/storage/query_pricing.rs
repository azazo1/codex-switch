use crate::core::models::ModelPrice;
use crate::storage::Store;
use sqlx::Row;

impl Store {
    pub async fn replace_model_prices(&self, prices: &[ModelPrice]) -> anyhow::Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM model_price_cache")
            .execute(&mut *tx)
            .await?;
        for price in prices {
            sqlx::query(
                "INSERT INTO model_price_cache (
                    provider_id, provider_name, model_id, model_name,
                    input_usd_per_million, cached_input_usd_per_million,
                    cache_write_usd_per_million, output_usd_per_million,
                    currency, source, official, fetched_at, raw_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )
            .bind(&price.provider_id)
            .bind(&price.provider_name)
            .bind(&price.model_id)
            .bind(&price.model_name)
            .bind(price.input_usd_per_million)
            .bind(price.cached_input_usd_per_million)
            .bind(price.cache_write_usd_per_million)
            .bind(price.output_usd_per_million)
            .bind(&price.currency)
            .bind(&price.source)
            .bind(price.official)
            .bind(price.fetched_at)
            .bind(&price.raw_json)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn model_price_cache_age_seconds(&self) -> anyhow::Result<Option<i64>> {
        let row = sqlx::query("SELECT MAX(fetched_at) AS fetched_at FROM model_price_cache")
            .fetch_one(self.pool())
            .await?;
        let Some(fetched_at) = row.get::<Option<i64>, _>("fetched_at") else {
            return Ok(None);
        };
        Ok(Some(chrono::Utc::now().timestamp() - fetched_at))
    }

    pub async fn model_price_count(&self) -> anyhow::Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS count FROM model_price_cache")
            .fetch_one(self.pool())
            .await?;
        Ok(row.get("count"))
    }

    pub async fn find_model_price(&self, model: &str) -> anyhow::Result<Option<ModelPrice>> {
        let candidates = model_price_candidates(model);
        for candidate in candidates {
            if let Some(price) = self.find_model_price_exact(&candidate, true).await? {
                return Ok(Some(price));
            }
        }
        for candidate in model_price_candidates(model) {
            if let Some(price) = self.find_model_price_exact(&candidate, false).await? {
                return Ok(Some(price));
            }
        }
        Ok(None)
    }

    async fn find_model_price_exact(
        &self,
        model: &str,
        official_only: bool,
    ) -> anyhow::Result<Option<ModelPrice>> {
        let row = if official_only {
            sqlx::query(
                "SELECT * FROM model_price_cache
                 WHERE model_id = ?1 AND official = 1
                 ORDER BY provider_id = 'openai' DESC, provider_id ASC
                 LIMIT 1",
            )
            .bind(model)
            .fetch_optional(self.pool())
            .await?
        } else {
            sqlx::query(
                "SELECT * FROM model_price_cache
                 WHERE model_id = ?1
                 ORDER BY official DESC, provider_id = 'openai' DESC, provider_id ASC
                 LIMIT 1",
            )
            .bind(model)
            .fetch_optional(self.pool())
            .await?
        };
        Ok(row.map(price_from_row))
    }
}

fn model_price_candidates(model: &str) -> Vec<String> {
    let model = model.trim();
    let mut candidates = Vec::new();
    push_unique(&mut candidates, model.to_string());
    if let Some(rest) = model.strip_prefix("openai/") {
        push_unique(&mut candidates, rest.to_string());
    } else {
        push_unique(&mut candidates, format!("openai/{model}"));
    }
    if let Some(rest) = model.rsplit('/').next()
        && rest != model
    {
        push_unique(&mut candidates, rest.to_string());
    }
    candidates
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !value.is_empty() && !values.iter().any(|item| item == &value) {
        values.push(value);
    }
}

fn price_from_row(row: sqlx::sqlite::SqliteRow) -> ModelPrice {
    ModelPrice {
        provider_id: row.get("provider_id"),
        provider_name: row.get("provider_name"),
        model_id: row.get("model_id"),
        model_name: row.get("model_name"),
        input_usd_per_million: row.get("input_usd_per_million"),
        cached_input_usd_per_million: row.get("cached_input_usd_per_million"),
        cache_write_usd_per_million: row.get("cache_write_usd_per_million"),
        output_usd_per_million: row.get("output_usd_per_million"),
        currency: row.get("currency"),
        source: row.get("source"),
        official: row.get("official"),
        fetched_at: row.get("fetched_at"),
        raw_json: row.get("raw_json"),
    }
}

#[cfg(test)]
mod tests {
    use super::model_price_candidates;

    #[test]
    fn builds_openai_model_candidates() {
        assert_eq!(
            model_price_candidates("openai/gpt-5-codex"),
            vec!["openai/gpt-5-codex", "gpt-5-codex"]
        );
        assert_eq!(
            model_price_candidates("gpt-5-codex"),
            vec!["gpt-5-codex", "openai/gpt-5-codex"]
        );
    }
}

use crate::core::models::{
    DashboardStats, ModelUsageStats, ProviderStats, RequestLog, TokenUsage,
};
use crate::storage::Store;
use chrono::{DateTime, Utc};
use sqlx::Row;

const UNASSIGNED_UPSTREAM_ID: &str = "none";

impl Store {
    pub async fn insert_request_log(&self, log: RequestLog) -> anyhow::Result<()> {
        let now = log.ts.unwrap_or_else(Utc::now);
        let day = now.format("%Y-%m-%d").to_string();
        let upstream_id = log
            .upstream_id
            .clone()
            .unwrap_or_else(|| UNASSIGNED_UPSTREAM_ID.to_string());
        let upstream_name = log
            .upstream_name
            .clone()
            .unwrap_or_else(|| "未选择".to_string());

        sqlx::query(
            "INSERT INTO request_logs (
                ts, upstream_id, upstream_name, endpoint, model, reasoning_effort, status, input_tokens,
                output_tokens, cache_read_tokens, cache_creation_tokens, total_tokens,
                duration_ms, first_token_ms, error
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )
        .bind(now.to_rfc3339())
        .bind(&log.upstream_id)
        .bind(&log.upstream_name)
        .bind(&log.endpoint)
        .bind(&log.model)
        .bind(&log.reasoning_effort)
        .bind(log.status)
        .bind(log.usage.input_tokens)
        .bind(log.usage.output_tokens)
        .bind(log.usage.cache_read_tokens)
        .bind(log.usage.cache_creation_tokens)
        .bind(log.usage.total_tokens)
        .bind(log.duration_ms)
        .bind(log.first_token_ms)
        .bind(&log.error)
        .execute(self.pool())
        .await?;

        sqlx::query(
            "INSERT INTO usage_rollups (
                day, upstream_id, upstream_name, requests, input_tokens, output_tokens,
                cache_read_tokens, cache_creation_tokens, total_tokens
             ) VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(day, upstream_id) DO UPDATE SET
                upstream_name = excluded.upstream_name,
                requests = requests + 1,
                input_tokens = input_tokens + excluded.input_tokens,
                output_tokens = output_tokens + excluded.output_tokens,
                cache_read_tokens = cache_read_tokens + excluded.cache_read_tokens,
                cache_creation_tokens = cache_creation_tokens + excluded.cache_creation_tokens,
                total_tokens = total_tokens + excluded.total_tokens",
        )
        .bind(day)
        .bind(upstream_id)
        .bind(upstream_name)
        .bind(log.usage.input_tokens)
        .bind(log.usage.output_tokens)
        .bind(log.usage.cache_read_tokens)
        .bind(log.usage.cache_creation_tokens)
        .bind(log.usage.total_tokens)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn dashboard_stats(&self) -> anyhow::Result<DashboardStats> {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let total = sqlx::query(
            "SELECT COALESCE(SUM(requests), 0) AS requests,
                    COALESCE(SUM(input_tokens), 0) AS input_tokens,
                    COALESCE(SUM(output_tokens), 0) AS output_tokens,
                    COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                    COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens,
                    COALESCE(SUM(total_tokens), 0) AS total_tokens
             FROM usage_rollups",
        )
        .fetch_one(self.pool())
        .await?;
        let today_row = sqlx::query(
            "SELECT COALESCE(SUM(requests), 0) AS requests,
                    COALESCE(SUM(input_tokens), 0) AS input_tokens,
                    COALESCE(SUM(output_tokens), 0) AS output_tokens,
                    COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                    COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens,
                    COALESCE(SUM(total_tokens), 0) AS total_tokens
             FROM usage_rollups WHERE day = ?1",
        )
        .bind(today)
        .fetch_one(self.pool())
        .await?;
        Ok(DashboardStats {
            total_requests: total.get::<i64, _>("requests"),
            total_usage: usage_from_rollup(&total),
            today_requests: today_row.get::<i64, _>("requests"),
            today_usage: usage_from_rollup(&today_row),
        })
    }

    pub async fn provider_stats(&self) -> anyhow::Result<Vec<ProviderStats>> {
        let rows = sqlx::query(
            "SELECT upstream_id, upstream_name,
                    COALESCE(SUM(requests), 0) AS requests,
                    COALESCE(SUM(input_tokens), 0) AS input_tokens,
                    COALESCE(SUM(output_tokens), 0) AS output_tokens,
                    COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                    COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens,
                    COALESCE(SUM(total_tokens), 0) AS total_tokens
             FROM usage_rollups
             WHERE upstream_id != ?1
             GROUP BY upstream_id, upstream_name
             ORDER BY total_tokens DESC",
        )
        .bind(UNASSIGNED_UPSTREAM_ID)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| ProviderStats {
                upstream_id: row.get("upstream_id"),
                upstream_name: row.get("upstream_name"),
                requests: row.get("requests"),
                usage: usage_from_rollup(&row),
            })
            .collect())
    }

    pub async fn model_usage_stats(&self, today_only: bool) -> anyhow::Result<Vec<ModelUsageStats>> {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let query = if today_only {
            "SELECT upstream_id, upstream_name, model,
                    COALESCE(SUM(input_tokens), 0) AS input_tokens,
                    COALESCE(SUM(output_tokens), 0) AS output_tokens,
                    COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                    COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens,
                    COALESCE(SUM(total_tokens), 0) AS total_tokens
             FROM request_logs
             WHERE substr(ts, 1, 10) = ?1
             GROUP BY upstream_id, upstream_name, model"
        } else {
            "SELECT upstream_id, upstream_name, model,
                    COALESCE(SUM(input_tokens), 0) AS input_tokens,
                    COALESCE(SUM(output_tokens), 0) AS output_tokens,
                    COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                    COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens,
                    COALESCE(SUM(total_tokens), 0) AS total_tokens
             FROM request_logs
             GROUP BY upstream_id, upstream_name, model"
        };
        let mut builder = sqlx::query(query);
        if today_only {
            builder = builder.bind(today);
        }
        let rows = builder.fetch_all(self.pool()).await?;
        Ok(rows
            .into_iter()
            .map(|row| ModelUsageStats {
                upstream_id: row.get("upstream_id"),
                model: row.get("model"),
                usage: usage_from_rollup(&row),
            })
            .collect())
    }

    #[cfg(test)]
    pub async fn recent_logs(&self, limit: i64) -> anyhow::Result<Vec<RequestLog>> {
        self.recent_logs_page(limit, 0).await
    }

    pub async fn recent_logs_page(
        &self,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<RequestLog>> {
        let rows = sqlx::query("SELECT * FROM request_logs ORDER BY id DESC LIMIT ?1 OFFSET ?2")
            .bind(limit)
            .bind(offset)
            .fetch_all(self.pool())
            .await?;
        Ok(rows.into_iter().map(request_log_from_row).collect())
    }

    pub async fn request_log_count(&self) -> anyhow::Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS count FROM request_logs")
            .fetch_one(self.pool())
            .await?;
        Ok(row.get("count"))
    }
}

fn request_log_from_row(row: sqlx::sqlite::SqliteRow) -> RequestLog {
    let ts: String = row.get("ts");
    RequestLog {
        ts: DateTime::parse_from_rfc3339(&ts)
            .ok()
            .map(|value| value.with_timezone(&Utc)),
        upstream_id: row.get("upstream_id"),
        upstream_name: row.get("upstream_name"),
        endpoint: row.get("endpoint"),
        model: row.get("model"),
        reasoning_effort: row.get("reasoning_effort"),
        status: row.get("status"),
        usage: TokenUsage {
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            cache_read_tokens: row.get("cache_read_tokens"),
            cache_creation_tokens: row.get("cache_creation_tokens"),
            total_tokens: row.get("total_tokens"),
        },
        duration_ms: row.get("duration_ms"),
        first_token_ms: row.get("first_token_ms"),
        error: row.get("error"),
    }
}

fn usage_from_rollup(row: &sqlx::sqlite::SqliteRow) -> TokenUsage {
    TokenUsage {
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cache_read_tokens: row.get("cache_read_tokens"),
        cache_creation_tokens: row.get("cache_creation_tokens"),
        total_tokens: row.get("total_tokens"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provider_stats_hides_unassigned_rollup() {
        let path = std::env::temp_dir()
            .join(format!("codex-switch-test-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(path).await.unwrap();
        store
            .insert_request_log(test_log(None, None, 5))
            .await
            .unwrap();
        store
            .insert_request_log(test_log(Some("upstream-a"), Some("relay-a"), 7))
            .await
            .unwrap();

        let stats = store.provider_stats().await.unwrap();

        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].upstream_id, "upstream-a");
        assert_eq!(stats[0].requests, 1);
        assert_eq!(stats[0].usage.total_tokens, 7);
    }

    fn test_log(
        upstream_id: Option<&str>,
        upstream_name: Option<&str>,
        total_tokens: i64,
    ) -> RequestLog {
        RequestLog {
            ts: None,
            upstream_id: upstream_id.map(str::to_string),
            upstream_name: upstream_name.map(str::to_string),
            endpoint: "/responses".to_string(),
            model: Some("gpt-test".to_string()),
            reasoning_effort: None,
            status: 200,
            usage: TokenUsage {
                input_tokens: total_tokens,
                total_tokens,
                ..Default::default()
            },
            duration_ms: 10,
            first_token_ms: None,
            error: None,
        }
    }
}

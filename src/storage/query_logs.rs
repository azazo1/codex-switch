use crate::core::models::{DashboardStats, ModelUsageStats, ProviderStats, RequestLog, TokenUsage};
use crate::pricing;
use crate::storage::Store;
use chrono::{DateTime, Utc};
use sqlx::{QueryBuilder, Row, Sqlite};

const UNASSIGNED_UPSTREAM_ID: &str = "none";

#[derive(Debug, Clone, Copy)]
pub enum RequestLogRetention {
    Since(DateTime<Utc>),
    Newest(i64),
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RequestLogFilter {
    pub model: Option<String>,
    pub upstream: Option<String>,
    pub reasoning_effort: Option<String>,
    pub endpoint: Option<String>,
    pub status_min: Option<i64>,
    pub status_max: Option<i64>,
    pub duration_ms_min: Option<i64>,
    pub duration_ms_max: Option<i64>,
    pub first_token_ms_min: Option<i64>,
    pub first_token_ms_max: Option<i64>,
    pub input_tokens_min: Option<i64>,
    pub input_tokens_max: Option<i64>,
    pub output_tokens_min: Option<i64>,
    pub output_tokens_max: Option<i64>,
    pub cache_read_tokens_min: Option<i64>,
    pub cache_read_tokens_max: Option<i64>,
    pub cache_creation_tokens_min: Option<i64>,
    pub cache_creation_tokens_max: Option<i64>,
    pub total_tokens_min: Option<i64>,
    pub total_tokens_max: Option<i64>,
    pub estimated_cost_usd_min: Option<f64>,
    pub estimated_cost_usd_max: Option<f64>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

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
        let estimated_cost_usd = match log.model.as_deref() {
            Some(model) => self
                .find_model_price(model)
                .await?
                .map(|price| pricing::estimate_usage_cost(&log.usage, &price).total_usd()),
            None => None,
        };

        sqlx::query(
            "INSERT INTO request_logs (
                ts, upstream_id, upstream_name, endpoint, model, reasoning_effort, status, input_tokens,
                output_tokens, cache_read_tokens, cache_creation_tokens, total_tokens,
                estimated_cost_usd, duration_ms, first_token_ms, error
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
        .bind(estimated_cost_usd)
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

    pub async fn model_usage_stats(
        &self,
        today_only: bool,
    ) -> anyhow::Result<Vec<ModelUsageStats>> {
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

    #[cfg(test)]
    pub async fn recent_logs_page(
        &self,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<RequestLog>> {
        self.recent_logs_page_filtered(limit, offset, &RequestLogFilter::default())
            .await
    }

    pub async fn recent_logs_page_filtered(
        &self,
        limit: i64,
        offset: i64,
        filter: &RequestLogFilter,
    ) -> anyhow::Result<Vec<RequestLog>> {
        let mut builder = QueryBuilder::<Sqlite>::new("SELECT * FROM request_logs");
        append_request_log_filters(&mut builder, filter);
        builder.push(" ORDER BY id DESC LIMIT ");
        builder.push_bind(limit);
        builder.push(" OFFSET ");
        builder.push_bind(offset);
        let rows = builder.build().fetch_all(self.pool()).await?;
        Ok(rows.into_iter().map(request_log_from_row).collect())
    }

    pub async fn request_log_count(&self) -> anyhow::Result<i64> {
        self.request_log_count_filtered(&RequestLogFilter::default())
            .await
    }

    pub async fn request_log_count_filtered(
        &self,
        filter: &RequestLogFilter,
    ) -> anyhow::Result<i64> {
        let mut builder = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) AS count FROM request_logs");
        append_request_log_filters(&mut builder, filter);
        let row = builder.build().fetch_one(self.pool()).await?;
        Ok(row.get("count"))
    }

    pub async fn cleanup_request_logs(
        &self,
        retention: RequestLogRetention,
    ) -> anyhow::Result<i64> {
        let mut tx = self.pool().begin().await?;
        let deleted = match retention {
            RequestLogRetention::Since(cutoff) => {
                sqlx::query("DELETE FROM request_logs WHERE ts < ?1")
                    .bind(cutoff.to_rfc3339())
                    .execute(&mut *tx)
                    .await?
                    .rows_affected()
            }
            RequestLogRetention::Newest(limit) => {
                let keep_count = limit.max(0);
                sqlx::query(
                    "DELETE FROM request_logs
                     WHERE id NOT IN (
                        SELECT id FROM request_logs ORDER BY id DESC LIMIT ?1
                     )",
                )
                .bind(keep_count)
                .execute(&mut *tx)
                .await?
                .rows_affected()
            }
            RequestLogRetention::Failed => {
                sqlx::query("DELETE FROM request_logs WHERE status >= 400")
                    .execute(&mut *tx)
                    .await?
                    .rows_affected()
            }
        };
        rebuild_usage_rollups(&mut tx).await?;
        tx.commit().await?;
        Ok(deleted as i64)
    }
}

fn append_request_log_filters(builder: &mut QueryBuilder<'_, Sqlite>, filter: &RequestLogFilter) {
    let mut has_where = false;
    macro_rules! begin_clause {
        () => {{
            if has_where {
                builder.push(" AND ");
            } else {
                builder.push(" WHERE ");
                has_where = true;
            }
        }};
    }

    if let Some(value) = filter_text(filter.model.as_deref()) {
        begin_clause!();
        builder.push("LOWER(COALESCE(model, '')) LIKE ");
        builder.push_bind(wildcard_like_pattern(value));
        builder.push(" ESCAPE '\\'");
    }
    if let Some(value) = filter_text(filter.upstream.as_deref()) {
        begin_clause!();
        builder.push("(LOWER(COALESCE(upstream_name, '')) LIKE ");
        builder.push_bind(like_pattern(value));
        builder.push(" ESCAPE '\\' OR LOWER(COALESCE(upstream_id, '')) LIKE ");
        builder.push_bind(like_pattern(value));
        builder.push(" ESCAPE '\\')");
    }
    if let Some(value) = filter_text(filter.reasoning_effort.as_deref()) {
        begin_clause!();
        builder.push("LOWER(COALESCE(reasoning_effort, '')) LIKE ");
        builder.push_bind(like_pattern(value));
        builder.push(" ESCAPE '\\'");
    }
    if let Some(value) = filter_text(filter.endpoint.as_deref()) {
        begin_clause!();
        builder.push("LOWER(endpoint) LIKE ");
        builder.push_bind(like_pattern(value));
        builder.push(" ESCAPE '\\'");
    }
    push_i64_min(builder, &mut has_where, "status", filter.status_min);
    push_i64_max(builder, &mut has_where, "status", filter.status_max);
    push_i64_min(
        builder,
        &mut has_where,
        "duration_ms",
        filter.duration_ms_min,
    );
    push_i64_max(
        builder,
        &mut has_where,
        "duration_ms",
        filter.duration_ms_max,
    );
    push_i64_min(
        builder,
        &mut has_where,
        "first_token_ms",
        filter.first_token_ms_min,
    );
    push_i64_max(
        builder,
        &mut has_where,
        "first_token_ms",
        filter.first_token_ms_max,
    );
    push_i64_min(
        builder,
        &mut has_where,
        "input_tokens",
        filter.input_tokens_min,
    );
    push_i64_max(
        builder,
        &mut has_where,
        "input_tokens",
        filter.input_tokens_max,
    );
    push_i64_min(
        builder,
        &mut has_where,
        "output_tokens",
        filter.output_tokens_min,
    );
    push_i64_max(
        builder,
        &mut has_where,
        "output_tokens",
        filter.output_tokens_max,
    );
    push_i64_min(
        builder,
        &mut has_where,
        "cache_read_tokens",
        filter.cache_read_tokens_min,
    );
    push_i64_max(
        builder,
        &mut has_where,
        "cache_read_tokens",
        filter.cache_read_tokens_max,
    );
    push_i64_min(
        builder,
        &mut has_where,
        "cache_creation_tokens",
        filter.cache_creation_tokens_min,
    );
    push_i64_max(
        builder,
        &mut has_where,
        "cache_creation_tokens",
        filter.cache_creation_tokens_max,
    );
    push_i64_min(
        builder,
        &mut has_where,
        "total_tokens",
        filter.total_tokens_min,
    );
    push_i64_max(
        builder,
        &mut has_where,
        "total_tokens",
        filter.total_tokens_max,
    );
    push_f64_min(
        builder,
        &mut has_where,
        "estimated_cost_usd",
        filter.estimated_cost_usd_min,
    );
    push_f64_max(
        builder,
        &mut has_where,
        "estimated_cost_usd",
        filter.estimated_cost_usd_max,
    );
    if let Some(value) = filter.started_at {
        begin_static_clause(builder, &mut has_where);
        builder.push("ts >= ");
        builder.push_bind(value.to_rfc3339());
    }
    if let Some(value) = filter.ended_at {
        begin_static_clause(builder, &mut has_where);
        builder.push("ts <= ");
        builder.push_bind(value.to_rfc3339());
    }
}

fn push_i64_min(
    builder: &mut QueryBuilder<'_, Sqlite>,
    has_where: &mut bool,
    column: &'static str,
    value: Option<i64>,
) {
    if let Some(value) = value {
        begin_static_clause(builder, has_where);
        builder.push(column);
        builder.push(" >= ");
        builder.push_bind(value);
    }
}

fn push_i64_max(
    builder: &mut QueryBuilder<'_, Sqlite>,
    has_where: &mut bool,
    column: &'static str,
    value: Option<i64>,
) {
    if let Some(value) = value {
        begin_static_clause(builder, has_where);
        builder.push(column);
        builder.push(" <= ");
        builder.push_bind(value);
    }
}

fn push_f64_min(
    builder: &mut QueryBuilder<'_, Sqlite>,
    has_where: &mut bool,
    column: &'static str,
    value: Option<f64>,
) {
    if let Some(value) = value {
        begin_static_clause(builder, has_where);
        builder.push(column);
        builder.push(" >= ");
        builder.push_bind(value);
    }
}

fn push_f64_max(
    builder: &mut QueryBuilder<'_, Sqlite>,
    has_where: &mut bool,
    column: &'static str,
    value: Option<f64>,
) {
    if let Some(value) = value {
        begin_static_clause(builder, has_where);
        builder.push(column);
        builder.push(" <= ");
        builder.push_bind(value);
    }
}

fn begin_static_clause(builder: &mut QueryBuilder<'_, Sqlite>, has_where: &mut bool) {
    if *has_where {
        builder.push(" AND ");
    } else {
        builder.push(" WHERE ");
        *has_where = true;
    }
}

fn filter_text(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn like_pattern(value: &str) -> String {
    let mut pattern = String::with_capacity(value.len() + 2);
    pattern.push('%');
    for ch in value.to_lowercase().chars() {
        match ch {
            '%' | '_' | '\\' => {
                pattern.push('\\');
                pattern.push(ch);
            }
            _ => pattern.push(ch),
        }
    }
    pattern.push('%');
    pattern
}

fn wildcard_like_pattern(value: &str) -> String {
    let mut pattern = String::with_capacity(value.len());
    for ch in value.to_lowercase().chars() {
        match ch {
            '*' => pattern.push('%'),
            '?' => pattern.push('_'),
            '%' | '_' | '\\' => {
                pattern.push('\\');
                pattern.push(ch);
            }
            _ => pattern.push(ch),
        }
    }
    pattern
}

async fn rebuild_usage_rollups(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM usage_rollups")
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO usage_rollups (
            day, upstream_id, upstream_name, requests, input_tokens, output_tokens,
            cache_read_tokens, cache_creation_tokens, total_tokens
         )
         SELECT
            substr(ts, 1, 10),
            COALESCE(upstream_id, ?1),
            COALESCE(MAX(upstream_name), ?2),
            COUNT(*),
            COALESCE(SUM(input_tokens), 0),
            COALESCE(SUM(output_tokens), 0),
            COALESCE(SUM(cache_read_tokens), 0),
            COALESCE(SUM(cache_creation_tokens), 0),
            COALESCE(SUM(total_tokens), 0)
         FROM request_logs
         GROUP BY substr(ts, 1, 10), COALESCE(upstream_id, ?1)",
    )
    .bind(UNASSIGNED_UPSTREAM_ID)
    .bind("未选择")
    .execute(&mut **tx)
    .await?;
    Ok(())
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
        estimated_cost_usd: row.get("estimated_cost_usd"),
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
    use crate::core::models::ModelPrice;
    use chrono::TimeZone;

    #[tokio::test]
    async fn provider_stats_hides_unassigned_rollup() {
        let path =
            std::env::temp_dir().join(format!("codex-switch-test-{}.sqlite", uuid::Uuid::new_v4()));
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

    #[tokio::test]
    async fn cleanup_request_logs_rebuilds_rollups() {
        let path =
            std::env::temp_dir().join(format!("codex-switch-test-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(path).await.unwrap();
        let old_ts = Utc.with_ymd_and_hms(2024, 1, 1, 12, 0, 0).unwrap();
        let new_ts = Utc.with_ymd_and_hms(2024, 1, 3, 12, 0, 0).unwrap();
        store
            .insert_request_log(test_log_at(old_ts, Some("upstream-a"), Some("relay-a"), 5))
            .await
            .unwrap();
        store
            .insert_request_log(test_log_at(new_ts, Some("upstream-a"), Some("relay-a"), 7))
            .await
            .unwrap();

        let deleted = store
            .cleanup_request_logs(RequestLogRetention::Since(
                Utc.with_ymd_and_hms(2024, 1, 2, 0, 0, 0).unwrap(),
            ))
            .await
            .unwrap();
        let stats = store.dashboard_stats().await.unwrap();
        let providers = store.provider_stats().await.unwrap();

        assert_eq!(deleted, 1);
        assert_eq!(store.request_log_count().await.unwrap(), 1);
        assert_eq!(stats.total_requests, 1);
        assert_eq!(stats.total_usage.total_tokens, 7);
        assert_eq!(providers[0].requests, 1);
        assert_eq!(providers[0].usage.total_tokens, 7);
    }

    #[tokio::test]
    async fn cleanup_request_logs_can_delete_failed_requests() {
        let path =
            std::env::temp_dir().join(format!("codex-switch-test-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(path).await.unwrap();
        store
            .insert_request_log(test_log_with_status(
                Some("upstream-a"),
                Some("relay-a"),
                5,
                200,
            ))
            .await
            .unwrap();
        store
            .insert_request_log(test_log_with_status(
                Some("upstream-a"),
                Some("relay-a"),
                7,
                500,
            ))
            .await
            .unwrap();

        let deleted = store
            .cleanup_request_logs(RequestLogRetention::Failed)
            .await
            .unwrap();
        let logs = store.recent_logs_page(10, 0).await.unwrap();
        let stats = store.dashboard_stats().await.unwrap();

        assert_eq!(deleted, 1);
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].status, 200);
        assert_eq!(stats.total_requests, 1);
        assert_eq!(stats.total_usage.total_tokens, 5);
    }

    #[tokio::test]
    async fn request_log_filter_uses_stored_estimated_cost() {
        let path =
            std::env::temp_dir().join(format!("codex-switch-test-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(path).await.unwrap();
        store
            .replace_model_prices(&[ModelPrice {
                provider_id: "openai".to_string(),
                provider_name: "OpenAI".to_string(),
                model_id: "gpt-test".to_string(),
                model_name: "GPT Test".to_string(),
                input_usd_per_million: Some(1.0),
                output_usd_per_million: Some(2.0),
                currency: "USD".to_string(),
                source: "test".to_string(),
                official: true,
                ..Default::default()
            }])
            .await
            .unwrap();
        store
            .insert_request_log(test_log(Some("upstream-a"), Some("relay-a"), 100))
            .await
            .unwrap();
        store
            .insert_request_log(test_log(Some("upstream-a"), Some("relay-a"), 2_000_000))
            .await
            .unwrap();

        let filter = RequestLogFilter {
            estimated_cost_usd_min: Some(1.0),
            ..Default::default()
        };
        let logs = store
            .recent_logs_page_filtered(10, 0, &filter)
            .await
            .unwrap();

        assert_eq!(store.request_log_count_filtered(&filter).await.unwrap(), 1);
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].usage.total_tokens, 2_000_000);
        assert!(logs[0].estimated_cost_usd.unwrap() >= 1.0);
    }

    #[tokio::test]
    async fn request_log_model_filter_supports_wildcards() {
        let path =
            std::env::temp_dir().join(format!("codex-switch-test-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(path).await.unwrap();
        let mut gpt_log = test_log(Some("upstream-a"), Some("relay-a"), 5);
        gpt_log.model = Some("gpt-5-codex".to_string());
        let mut claude_log = test_log(Some("upstream-a"), Some("relay-a"), 7);
        claude_log.model = Some("claude-sonnet".to_string());
        store.insert_request_log(gpt_log).await.unwrap();
        store.insert_request_log(claude_log).await.unwrap();

        let filter = RequestLogFilter {
            model: Some("gpt-*".to_string()),
            ..Default::default()
        };
        let logs = store
            .recent_logs_page_filtered(10, 0, &filter)
            .await
            .unwrap();

        assert_eq!(store.request_log_count_filtered(&filter).await.unwrap(), 1);
        assert_eq!(logs[0].model.as_deref(), Some("gpt-5-codex"));
    }

    fn test_log(
        upstream_id: Option<&str>,
        upstream_name: Option<&str>,
        total_tokens: i64,
    ) -> RequestLog {
        test_log_at(Utc::now(), upstream_id, upstream_name, total_tokens)
    }

    fn test_log_with_status(
        upstream_id: Option<&str>,
        upstream_name: Option<&str>,
        total_tokens: i64,
        status: i64,
    ) -> RequestLog {
        let mut log = test_log(upstream_id, upstream_name, total_tokens);
        log.status = status;
        log
    }

    fn test_log_at(
        ts: DateTime<Utc>,
        upstream_id: Option<&str>,
        upstream_name: Option<&str>,
        total_tokens: i64,
    ) -> RequestLog {
        RequestLog {
            ts: Some(ts),
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
            estimated_cost_usd: None,
            duration_ms: 10,
            first_token_ms: None,
            error: None,
        }
    }
}

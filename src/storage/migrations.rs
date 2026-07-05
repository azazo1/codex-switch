use crate::storage::Store;
use chrono::Utc;

impl Store {
    pub(super) async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at TEXT NOT NULL
            )",
        )
        .execute(self.pool())
        .await?;

        for migration in migrations() {
            if self.migration_applied(migration.version).await? {
                continue;
            }
            tracing::info!(
                version = migration.version,
                name = migration.name,
                "applying sqlite migration"
            );
            let mut tx = self.pool().begin().await?;
            for statement in migration.statements {
                sqlx::query(statement).execute(&mut *tx).await?;
            }
            sqlx::query(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            )
            .bind(migration.version)
            .bind(migration.name)
            .bind(Utc::now().to_rfc3339())
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
        Ok(())
    }

    async fn migration_applied(&self, version: i64) -> anyhow::Result<bool> {
        use sqlx::Row;

        let row = sqlx::query("SELECT COUNT(*) AS count FROM schema_migrations WHERE version = ?1")
            .bind(version)
            .fetch_one(self.pool())
            .await?;
        Ok(row.get::<i64, _>("count") > 0)
    }

    pub(super) async fn ensure_defaults(&self) -> anyhow::Result<()> {
        if self.get_setting("bind_addr").await?.is_none() {
            self.set_setting("bind_addr", "127.0.0.1:15721").await?;
        }
        if self.get_setting("local_access_key").await?.is_none() {
            self.set_setting("local_access_key", &format!("cs-{}", uuid::Uuid::new_v4()))
                .await?;
        }
        Ok(())
    }

}

struct Migration {
    version: i64,
    name: &'static str,
    statements: &'static [&'static str],
}

fn migrations() -> &'static [Migration] {
    &[Migration {
        version: 1,
        name: "initial_schema",
        statements: &[
            "CREATE TABLE IF NOT EXISTS upstreams (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                name TEXT NOT NULL,
                base_url TEXT NOT NULL DEFAULT '',
                wire_api TEXT NOT NULL DEFAULT 'responses',
                supports_compact INTEGER NOT NULL DEFAULT 0,
                enabled INTEGER NOT NULL DEFAULT 1,
                priority INTEGER NOT NULL DEFAULT 0,
                weight INTEGER NOT NULL DEFAULT 1,
                balance_provider TEXT NOT NULL DEFAULT 'auto',
                chatgpt_account_id TEXT,
                email TEXT,
                plan_type TEXT,
                token_expires_at INTEGER,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS secrets (
                upstream_id TEXT NOT NULL,
                name TEXT NOT NULL,
                encrypted_value TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (upstream_id, name),
                FOREIGN KEY (upstream_id) REFERENCES upstreams(id) ON DELETE CASCADE
            )",
            "CREATE TABLE IF NOT EXISTS request_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL,
                upstream_id TEXT,
                upstream_name TEXT,
                endpoint TEXT NOT NULL,
                model TEXT,
                status INTEGER NOT NULL,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                duration_ms INTEGER NOT NULL DEFAULT 0,
                first_token_ms INTEGER,
                error TEXT
            )",
            "CREATE INDEX IF NOT EXISTS idx_request_logs_ts ON request_logs(ts)",
            "CREATE INDEX IF NOT EXISTS idx_request_logs_upstream ON request_logs(upstream_id)",
            "CREATE TABLE IF NOT EXISTS usage_rollups (
                day TEXT NOT NULL,
                upstream_id TEXT NOT NULL,
                upstream_name TEXT NOT NULL,
                requests INTEGER NOT NULL DEFAULT 0,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (day, upstream_id)
            )",
            "CREATE TABLE IF NOT EXISTS quota_snapshots (
                upstream_id TEXT PRIMARY KEY,
                used_5h_percent REAL,
                reset_5h_seconds INTEGER,
                window_5h_minutes INTEGER,
                used_7d_percent REAL,
                reset_7d_seconds INTEGER,
                window_7d_minutes INTEGER,
                fetched_at INTEGER NOT NULL,
                FOREIGN KEY (upstream_id) REFERENCES upstreams(id) ON DELETE CASCADE
            )",
            "CREATE TABLE IF NOT EXISTS balance_snapshots (
                upstream_id TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                remaining REAL,
                total REAL,
                used REAL,
                unit TEXT,
                is_valid INTEGER NOT NULL DEFAULT 0,
                message TEXT,
                fetched_at INTEGER NOT NULL,
                FOREIGN KEY (upstream_id) REFERENCES upstreams(id) ON DELETE CASCADE
            )",
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        ],
    }]
}

#[cfg(test)]
mod tests {
    use crate::storage::Store;
    use sqlx::Row;

    #[tokio::test]
    async fn records_schema_version_and_keeps_migrations_idempotent() {
        let path = std::env::temp_dir()
            .join(format!("codex-switch-migrate-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(&path).await.unwrap();

        let rows = sqlx::query("SELECT version, name FROM schema_migrations ORDER BY version")
            .fetch_all(store.pool())
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<i64, _>("version"), 1);
        assert_eq!(rows[0].get::<String, _>("name"), "initial_schema");
        assert_eq!(
            store.get_setting("bind_addr").await.unwrap().as_deref(),
            Some("127.0.0.1:15721")
        );

        drop(store);
        let store = Store::open(&path).await.unwrap();
        let count = sqlx::query("SELECT COUNT(*) AS count FROM schema_migrations")
            .fetch_one(store.pool())
            .await
            .unwrap()
            .get::<i64, _>("count");
        assert_eq!(count, 1);
    }
}

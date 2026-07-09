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
        if self.get_setting("scheduler_route_max_hops").await?.is_none() {
            self.set_setting("scheduler_route_max_hops", "8").await?;
        }
        self.ensure_default_schedule_group().await?;
        Ok(())
    }

    async fn ensure_default_schedule_group(&self) -> anyhow::Result<()> {
        use sqlx::Row;

        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT OR IGNORE INTO schedule_groups (
                id, name, mode, use_all_upstreams, fixed_upstream_id,
                failure_threshold, failover_on_balance, failover_on_network, failover_on_5xx,
                affinity_ttl_seconds, created_at, updated_at
             ) VALUES (
                'default', 'Default', 'failover', 1, NULL, 1, 1, 1, 1, 1800, ?1, ?2
             )",
        )
        .bind(&now)
        .bind(&now)
        .execute(self.pool())
        .await?;

        let current = self.get_setting("current_schedule_group_id").await?;
        let current_is_valid = if let Some(id) = current {
            let row = sqlx::query("SELECT COUNT(*) AS count FROM schedule_groups WHERE id = ?1")
                .bind(id)
                .fetch_one(self.pool())
                .await?;
            row.get::<i64, _>("count") > 0
        } else {
            false
        };
        if !current_is_valid {
            self.set_setting("current_schedule_group_id", "default").await?;
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
    &[
        Migration {
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
                "CREATE TABLE IF NOT EXISTS credentials (
                upstream_id TEXT NOT NULL,
                name TEXT NOT NULL,
                value TEXT NOT NULL,
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
        },
        Migration {
            version: 2,
            name: "scheduler_groups",
            statements: &[
                "CREATE TABLE IF NOT EXISTS schedule_groups (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                mode TEXT NOT NULL DEFAULT 'failover',
                use_all_upstreams INTEGER NOT NULL DEFAULT 1,
                fixed_upstream_id TEXT,
                failure_threshold INTEGER NOT NULL DEFAULT 1,
                failover_on_balance INTEGER NOT NULL DEFAULT 1,
                failover_on_network INTEGER NOT NULL DEFAULT 1,
                failover_on_5xx INTEGER NOT NULL DEFAULT 1,
                affinity_ttl_seconds INTEGER NOT NULL DEFAULT 1800,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
                "CREATE TABLE IF NOT EXISTS schedule_group_members (
                group_id TEXT NOT NULL,
                upstream_id TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                priority INTEGER NOT NULL DEFAULT 0,
                weight INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (group_id, upstream_id),
                FOREIGN KEY (group_id) REFERENCES schedule_groups(id) ON DELETE CASCADE,
                FOREIGN KEY (upstream_id) REFERENCES upstreams(id) ON DELETE CASCADE
            )",
                "CREATE INDEX IF NOT EXISTS idx_schedule_group_members_group ON schedule_group_members(group_id)",
                "CREATE INDEX IF NOT EXISTS idx_schedule_group_members_upstream ON schedule_group_members(upstream_id)",
            ],
        },
        Migration {
            version: 3,
            name: "model_price_cache",
            statements: &[
                "CREATE TABLE IF NOT EXISTS model_price_cache (
                provider_id TEXT NOT NULL,
                provider_name TEXT NOT NULL,
                model_id TEXT NOT NULL,
                model_name TEXT NOT NULL,
                input_usd_per_million REAL,
                cached_input_usd_per_million REAL,
                cache_write_usd_per_million REAL,
                output_usd_per_million REAL,
                currency TEXT NOT NULL DEFAULT 'USD',
                source TEXT NOT NULL,
                official INTEGER NOT NULL DEFAULT 0,
                fetched_at INTEGER NOT NULL,
                raw_json TEXT,
                PRIMARY KEY (provider_id, model_id)
            )",
                "CREATE INDEX IF NOT EXISTS idx_model_price_cache_model ON model_price_cache(model_id)",
                "CREATE INDEX IF NOT EXISTS idx_model_price_cache_official ON model_price_cache(official, model_id)",
            ],
        },
        Migration {
            version: 4,
            name: "request_log_reasoning_effort",
            statements: &["ALTER TABLE request_logs ADD COLUMN reasoning_effort TEXT"],
        },
        Migration {
            version: 5,
            name: "schedule_route_rules",
            statements: &[
                "CREATE TABLE IF NOT EXISTS schedule_route_rules (
                id TEXT PRIMARY KEY,
                group_id TEXT NOT NULL,
                name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                pattern TEXT NOT NULL,
                target_kind TEXT NOT NULL,
                target_group_id TEXT,
                target_upstream_id TEXT,
                target_model TEXT,
                priority INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY (group_id) REFERENCES schedule_groups(id) ON DELETE CASCADE,
                FOREIGN KEY (target_group_id) REFERENCES schedule_groups(id) ON DELETE CASCADE,
                FOREIGN KEY (target_upstream_id) REFERENCES upstreams(id) ON DELETE CASCADE
            )",
                "CREATE INDEX IF NOT EXISTS idx_schedule_route_rules_group ON schedule_route_rules(group_id)",
                "CREATE INDEX IF NOT EXISTS idx_schedule_route_rules_target_group ON schedule_route_rules(target_group_id)",
                "CREATE INDEX IF NOT EXISTS idx_schedule_route_rules_target_upstream ON schedule_route_rules(target_upstream_id)",
            ],
        },
        Migration {
            version: 6,
            name: "nested_schedule_groups",
            statements: &[
                "ALTER TABLE schedule_groups ADD COLUMN fixed_target_kind TEXT NOT NULL DEFAULT 'upstream'",
                "ALTER TABLE schedule_groups ADD COLUMN fixed_group_id TEXT",
                "CREATE TABLE IF NOT EXISTS schedule_group_child_groups (
                group_id TEXT NOT NULL,
                target_group_id TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                priority INTEGER NOT NULL DEFAULT 0,
                weight INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (group_id, target_group_id),
                FOREIGN KEY (group_id) REFERENCES schedule_groups(id) ON DELETE CASCADE,
                FOREIGN KEY (target_group_id) REFERENCES schedule_groups(id) ON DELETE CASCADE
            )",
                "CREATE INDEX IF NOT EXISTS idx_schedule_group_child_groups_group ON schedule_group_child_groups(group_id)",
                "CREATE INDEX IF NOT EXISTS idx_schedule_group_child_groups_target ON schedule_group_child_groups(target_group_id)",
            ],
        },
        Migration {
            version: 7,
            name: "request_log_estimated_cost",
            statements: &[
                "ALTER TABLE request_logs ADD COLUMN estimated_cost_usd REAL",
                "CREATE INDEX IF NOT EXISTS idx_request_logs_estimated_cost ON request_logs(estimated_cost_usd)",
            ],
        },
        Migration {
            version: 8,
            name: "upstream_cache_keepalive_settings",
            statements: &[
                "CREATE TABLE IF NOT EXISTS upstream_cache_keepalive_settings (
                upstream_id TEXT PRIMARY KEY,
                enabled INTEGER NOT NULL DEFAULT 0,
                mode TEXT NOT NULL DEFAULT 'smart',
                interval_seconds INTEGER NOT NULL DEFAULT 300,
                max_idle_seconds INTEGER NOT NULL DEFAULT 3600,
                min_cacheable_tokens INTEGER NOT NULL DEFAULT 1024,
                max_active_sessions INTEGER NOT NULL DEFAULT 32,
                prefer_extended_retention INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL,
                FOREIGN KEY (upstream_id) REFERENCES upstreams(id) ON DELETE CASCADE
            )",
            ],
        },
        Migration {
            version: 9,
            name: "cache_keepalive_max_cacheable_tokens",
            statements: &[
                "ALTER TABLE upstream_cache_keepalive_settings ADD COLUMN max_cacheable_tokens INTEGER NOT NULL DEFAULT 128000",
            ],
        },
    ]
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
        assert_eq!(rows.len(), 9);
        assert_eq!(rows[0].get::<i64, _>("version"), 1);
        assert_eq!(rows[0].get::<String, _>("name"), "initial_schema");
        assert_eq!(rows[1].get::<i64, _>("version"), 2);
        assert_eq!(rows[1].get::<String, _>("name"), "scheduler_groups");
        assert_eq!(rows[2].get::<i64, _>("version"), 3);
        assert_eq!(rows[2].get::<String, _>("name"), "model_price_cache");
        assert_eq!(rows[3].get::<i64, _>("version"), 4);
        assert_eq!(
            rows[3].get::<String, _>("name"),
            "request_log_reasoning_effort"
        );
        assert_eq!(rows[4].get::<i64, _>("version"), 5);
        assert_eq!(
            rows[4].get::<String, _>("name"),
            "schedule_route_rules"
        );
        assert_eq!(rows[5].get::<i64, _>("version"), 6);
        assert_eq!(
            rows[5].get::<String, _>("name"),
            "nested_schedule_groups"
        );
        assert_eq!(rows[6].get::<i64, _>("version"), 7);
        assert_eq!(
            rows[6].get::<String, _>("name"),
            "request_log_estimated_cost"
        );
        assert_eq!(rows[7].get::<i64, _>("version"), 8);
        assert_eq!(
            rows[7].get::<String, _>("name"),
            "upstream_cache_keepalive_settings"
        );
        assert_eq!(rows[8].get::<i64, _>("version"), 9);
        assert_eq!(
            rows[8].get::<String, _>("name"),
            "cache_keepalive_max_cacheable_tokens"
        );
        assert_eq!(
            store.get_setting("bind_addr").await.unwrap().as_deref(),
            Some("127.0.0.1:15721")
        );
        assert_eq!(
            store
                .get_setting("current_schedule_group_id")
                .await
                .unwrap()
                .as_deref(),
            Some("default")
        );
        assert_eq!(
            store
                .get_setting("scheduler_route_max_hops")
                .await
                .unwrap()
                .as_deref(),
            Some("8")
        );

        drop(store);
        let store = Store::open(&path).await.unwrap();
        let count = sqlx::query("SELECT COUNT(*) AS count FROM schema_migrations")
            .fetch_one(store.pool())
            .await
            .unwrap()
            .get::<i64, _>("count");
        assert_eq!(count, 9);
    }
}

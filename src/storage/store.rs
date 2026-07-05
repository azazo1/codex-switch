use anyhow::Context;
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::path::Path;

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create data directory {}", parent.display()))?;
        }
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect(&url)
            .await
            .with_context(|| format!("failed to open sqlite database {}", path.display()))?;
        let store = Self { pool };
        store.migrate().await?;
        store.ensure_defaults().await?;
        Ok(store)
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

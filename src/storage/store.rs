use crate::core::models::DatabaseInfo;
use anyhow::Context;
use sqlx::Row;
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
    path: PathBuf,
}

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
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
        let store = Self { pool, path };
        store.migrate().await?;
        store.ensure_defaults().await?;
        Ok(store)
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn database_info(&self) -> anyhow::Result<DatabaseInfo> {
        let page_count = pragma_i64(self.pool(), "PRAGMA page_count").await?;
        let page_size = pragma_i64(self.pool(), "PRAGMA page_size").await?;
        let freelist_count = pragma_i64(self.pool(), "PRAGMA freelist_count").await?;
        Ok(DatabaseInfo {
            path: self.path.display().to_string(),
            main_file_bytes: file_size(&self.path).await?,
            wal_file_bytes: file_size(&sqlite_sidecar_path(&self.path, "-wal")).await?,
            shm_file_bytes: file_size(&sqlite_sidecar_path(&self.path, "-shm")).await?,
            page_count,
            page_size,
            freelist_count,
            request_log_count: self.request_log_count().await?,
        })
    }
}

async fn pragma_i64(pool: &SqlitePool, statement: &str) -> anyhow::Result<i64> {
    let row = sqlx::query(statement).fetch_one(pool).await?;
    Ok(row.get(0))
}

async fn file_size(path: &Path) -> anyhow::Result<u64> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.len()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(err).with_context(|| format!("failed to read file size {}", path.display())),
    }
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

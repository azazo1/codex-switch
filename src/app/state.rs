use crate::storage::{Store, credentials::CredentialStore};
use anyhow::Context;
use directories::ProjectDirs;
use std::path::PathBuf;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub credentials: CredentialStore,
    pub http: reqwest::Client,
}

impl AppState {
    pub async fn new() -> anyhow::Result<Self> {
        let data_dir = data_dir()?;
        let db_path = data_dir.join("codex-switch.sqlite");
        tracing::info!(path = %db_path.display(), "opening sqlite database");
        let store = Store::open(db_path).await?;
        let credentials = CredentialStore::new(store.clone()).await?;
        let http = reqwest::Client::builder()
            .user_agent("codex-switch/0.1.0")
            .build()
            .context("failed to build http client")?;
        Ok(Self {
            store,
            credentials,
            http,
        })
    }
}

fn data_dir() -> anyhow::Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "codex-switch")
        .context("failed to resolve system data directory")?;
    Ok(dirs.data_dir().to_path_buf())
}

use crate::storage::{Store, credentials::CredentialStore};
use crate::scheduler::SchedulerRuntime;
use anyhow::Context;
use directories::ProjectDirs;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub credentials: CredentialStore,
    pub http: reqwest::Client,
    pub events: AppEvents,
    pub scheduler: SchedulerRuntime,
}

#[derive(Clone, Default)]
pub struct AppEvents {
    request_log_version: Arc<AtomicU64>,
}

impl AppEvents {
    pub fn bump_request_logs(&self) {
        self.request_log_version.fetch_add(1, Ordering::Relaxed);
    }

    pub fn request_log_version(&self) -> u64 {
        self.request_log_version.load(Ordering::Relaxed)
    }
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
            events: AppEvents::default(),
            scheduler: SchedulerRuntime::default(),
        })
    }
}

fn data_dir() -> anyhow::Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "codex-switch")
        .context("failed to resolve system data directory")?;
    Ok(dirs.data_dir().to_path_buf())
}

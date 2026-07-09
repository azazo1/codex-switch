use crate::cache_keepalive::CacheKeepaliveRuntime;
use crate::live::LiveRequestStore;
use crate::scheduler::SchedulerRuntime;
use crate::storage::{Store, credentials::CredentialStore};
use anyhow::Context;
use directories::ProjectDirs;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

type RepaintRequester = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub credentials: CredentialStore,
    pub http: reqwest::Client,
    pub events: AppEvents,
    pub scheduler: SchedulerRuntime,
    pub live_requests: LiveRequestStore,
    pub cache_keepalive: CacheKeepaliveRuntime,
}

#[derive(Clone, Default)]
pub struct AppEvents {
    request_log_version: Arc<AtomicU64>,
    live_stream_version: Arc<AtomicU64>,
    cache_keepalive_version: Arc<AtomicU64>,
    repaint_requester: Arc<Mutex<Option<RepaintRequester>>>,
}

impl AppEvents {
    pub fn bump_request_logs(&self) {
        self.request_log_version.fetch_add(1, Ordering::Relaxed);
        self.request_repaint();
    }

    pub fn request_log_version(&self) -> u64 {
        self.request_log_version.load(Ordering::Relaxed)
    }

    pub fn bump_live_streams(&self) {
        self.live_stream_version.fetch_add(1, Ordering::Relaxed);
        self.request_repaint();
    }

    pub fn live_stream_version(&self) -> u64 {
        self.live_stream_version.load(Ordering::Relaxed)
    }

    pub fn bump_cache_keepalive(&self) {
        self.cache_keepalive_version.fetch_add(1, Ordering::Relaxed);
        self.request_repaint();
    }

    pub fn cache_keepalive_version(&self) -> u64 {
        self.cache_keepalive_version.load(Ordering::Relaxed)
    }

    pub fn set_repaint_requester<F>(&self, repaint: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        if let Ok(mut requester) = self.repaint_requester.lock() {
            *requester = Some(Arc::new(repaint));
        }
    }

    fn request_repaint(&self) {
        let requester = self
            .repaint_requester
            .lock()
            .ok()
            .and_then(|requester| requester.clone());
        if let Some(requester) = requester {
            requester();
        }
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
        let events = AppEvents::default();
        let cache_keepalive = CacheKeepaliveRuntime::new(
            store.clone(),
            credentials.clone(),
            http.clone(),
            events.clone(),
        );
        let state = Self {
            store,
            credentials,
            http,
            events,
            scheduler: SchedulerRuntime::default(),
            live_requests: LiveRequestStore::default(),
            cache_keepalive,
        };
        state.cache_keepalive.start();
        Ok(state)
    }
}

fn data_dir() -> anyhow::Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "codex-switch")
        .context("failed to resolve system data directory")?;
    Ok(dirs.data_dir().to_path_buf())
}

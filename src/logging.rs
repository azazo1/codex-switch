use crate::storage::Store;
use anyhow::Context;
use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_rolling_file::{RollingConditionBase, RollingFileAppenderBase};
use tracing_subscriber::{
    EnvFilter, Registry, fmt,
    prelude::*,
    reload,
};

const LOG_FILE_ENV: &str = "CODEX_SWITCH_LOG_FILE";
const LOG_BODIES_ENV: &str = "CODEX_SWITCH_LOG_BODIES";
const DEBUG_FILE_FILTER: &str = "info,codex_switch=trace,tower_http=debug";

const SETTING_DEBUG_LOG_ENABLED: &str = "debug_log_enabled";
const SETTING_LOG_ROTATION_SIZE_MB: &str = "log_rotation_size_mb";
const SETTING_LOG_MAX_FILES: &str = "log_max_files";

const DEFAULT_LOG_ROTATION_SIZE_MB: u64 = 20;
const DEFAULT_LOG_MAX_FILES: usize = 10;

type FileLayer = tracing_subscriber::filter::Filtered<
    fmt::Layer<
        Registry,
        fmt::format::DefaultFields,
        fmt::format::Format<fmt::format::Full>,
        NonBlocking,
    >,
    EnvFilter,
    Registry,
>;

#[derive(Debug, Clone, Copy)]
pub(crate) struct LogRotationConfig {
    pub enabled: bool,
    pub size_mb: u64,
    pub max_files: usize,
}

impl Default for LogRotationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            size_mb: DEFAULT_LOG_ROTATION_SIZE_MB,
            max_files: DEFAULT_LOG_MAX_FILES,
        }
    }
}

impl LogRotationConfig {
    pub(crate) async fn load(store: &Store) -> anyhow::Result<Self> {
        let enabled = store
            .get_setting(SETTING_DEBUG_LOG_ENABLED)
            .await?
            .as_deref()
            == Some("true");
        let size_mb = setting_u64(store, SETTING_LOG_ROTATION_SIZE_MB, DEFAULT_LOG_ROTATION_SIZE_MB)
            .await?
            .max(1);
        let max_files = setting_usize(store, SETTING_LOG_MAX_FILES, DEFAULT_LOG_MAX_FILES)
            .await?
            .max(1);
        Ok(Self {
            enabled,
            size_mb,
            max_files,
        })
    }
}

struct FileWriterState {
    non_blocking: NonBlocking,
    guard: WorkerGuard,
}

struct TracingControls {
    file_layer: reload::Handle<FileLayer, Registry>,
    file_writer: Mutex<FileWriterState>,
}

static CONTROLS: OnceLock<TracingControls> = OnceLock::new();
static BODY_LOGGING_ENABLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn init_tracing(config: LogRotationConfig) -> anyhow::Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .context("failed to create tracing env filter")?;

    if let Some(log_path) = std::env::var_os(LOG_FILE_ENV).filter(|path| !path.is_empty()) {
        set_body_logging_enabled(body_logging_env_enabled());
        return init_file_tracing(Path::new(&log_path), env_filter, false);
    }

    if env_override_active() {
        set_body_logging_enabled(body_logging_env_enabled());
        #[cfg(target_os = "windows")]
        {
            let log_path = log_file_path()?;
            return init_file_tracing(&log_path, env_filter, true);
        }
        #[cfg(not(target_os = "windows"))]
        {
            return init_stderr_tracing(env_filter);
        }
    }

    let log_path = log_file_path()?;
    let appender = build_rolling_appender(&log_path, config.size_mb, config.max_files)?;
    let (non_blocking, file_guard) = tracing_appender::non_blocking(appender);
    let initial_filter = if config.enabled {
        DEBUG_FILE_FILTER
    } else {
        "info"
    };
    let file_layer = fmt::layer()
        .with_ansi(false)
        .with_writer(non_blocking.clone())
        .with_filter(
            EnvFilter::try_new(initial_filter).context("failed to create file filter")?,
        );
    let (file_layer, file_layer_handle): (
        reload::Layer<FileLayer, Registry>,
        reload::Handle<FileLayer, Registry>,
    ) = reload::Layer::new(file_layer);

    let subscriber = tracing_subscriber::registry().with(file_layer);
    #[cfg(not(target_os = "windows"))]
    let subscriber = {
        let stderr = fmt::layer()
            .with_writer(std::io::stderr)
            .with_filter(
                EnvFilter::try_new("info").context("failed to create stderr filter")?,
            );
        subscriber.with(stderr)
    };
    subscriber
        .try_init()
        .context("failed to install tracing subscriber")?;

    let controls = TracingControls {
        file_layer: file_layer_handle,
        file_writer: Mutex::new(FileWriterState {
            non_blocking,
            guard: file_guard,
        }),
    };
    let _ = CONTROLS.set(controls);
    set_body_logging_enabled(config.enabled);
    Ok(())
}

pub(crate) fn set_debug_log_enabled(enabled: bool) -> anyhow::Result<()> {
    if env_override_active() {
        set_body_logging_enabled(body_logging_env_enabled());
        return Ok(());
    }
    let Some(controls) = CONTROLS.get() else {
        return Ok(());
    };
    let filter = if enabled {
        DEBUG_FILE_FILTER
    } else {
        "info"
    };
    let filter = EnvFilter::try_new(filter).context("failed to create debug log filter")?;
    let layer = {
        let writer = controls
            .file_writer
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock log writer"))?;
        fmt::layer()
            .with_ansi(false)
            .with_writer(writer.non_blocking.clone())
            .with_filter(filter)
    };
    controls
        .file_layer
        .reload(layer)
        .context("failed to switch debug log filter")?;
    set_body_logging_enabled(enabled);
    Ok(())
}

pub(crate) fn set_rotation_config(size_mb: u64, max_files: usize) -> anyhow::Result<()> {
    if env_override_active() {
        return Ok(());
    }
    let controls = CONTROLS
        .get()
        .context("tracing controls are not initialized")?;
    controls.reconfigure(size_mb, max_files)
}

pub(crate) fn body_logging_enabled() -> bool {
    BODY_LOGGING_ENABLED.load(Ordering::Relaxed)
}

pub(crate) fn env_override_active() -> bool {
    std::env::var_os(LOG_FILE_ENV).is_some_and(|path| !path.is_empty())
        || std::env::var_os(LOG_BODIES_ENV).is_some()
}

pub(crate) fn log_file_path() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os(LOG_FILE_ENV).filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    Ok(crate::app::data_dir()?.join("codex-switch.log"))
}

impl TracingControls {
    fn reconfigure(&self, size_mb: u64, max_files: usize) -> anyhow::Result<()> {
        let log_path = log_file_path()?;
        let appender = build_rolling_appender(&log_path, size_mb, max_files)?;
        let (non_blocking, guard) = tracing_appender::non_blocking(appender);
        let filter = if body_logging_enabled() {
            DEBUG_FILE_FILTER
        } else {
            "info"
        };
        let file_layer = fmt::layer()
            .with_ansi(false)
            .with_writer(non_blocking.clone())
            .with_filter(
                EnvFilter::try_new(filter).context("failed to create rotation log filter")?,
            );
        self.file_layer
            .reload(file_layer)
            .context("failed to reload rolling file layer")?;
        let mut writer = self
            .file_writer
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock log writer"))?;
        writer.non_blocking = non_blocking;
        writer.guard = guard;
        Ok(())
    }
}

fn set_body_logging_enabled(enabled: bool) {
    BODY_LOGGING_ENABLED.store(enabled, Ordering::Relaxed);
}

fn body_logging_env_enabled() -> bool {
    std::env::var(LOG_BODIES_ENV)
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
}

fn build_rolling_appender(
    log_path: &Path,
    size_mb: u64,
    max_files: usize,
) -> anyhow::Result<RollingFileAppenderBase> {
    let condition = RollingConditionBase::new()
        .daily()
        .max_size(normalized_size_bytes(size_mb));
    RollingFileAppenderBase::new(log_path, condition, max_files.max(1))
        .with_context(|| format!("failed to open rolling log file {}", log_path.display()))
}

fn normalized_size_bytes(size_mb: u64) -> u64 {
    size_mb.max(1).saturating_mul(1024).saturating_mul(1024)
}

fn init_file_tracing(log_path: &Path, env_filter: EnvFilter, append: bool) -> anyhow::Result<()> {
    if let Some(parent) = log_path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent).context("failed to create log directory")?;
    }
    let mut options = OpenOptions::new();
    options.create(true).write(true);
    if append {
        options.append(true);
    } else {
        options.truncate(true);
    }
    let log_file = options
        .open(log_path)
        .with_context(|| format!("failed to open log file: {}", log_path.display()))?;
    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            fmt::layer()
                .with_ansi(false)
                .with_writer(Mutex::new(log_file)),
        )
        .try_init()
        .context("failed to install tracing subscriber")?;
    Ok(())
}

fn init_stderr_tracing(env_filter: EnvFilter) -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().with_writer(std::io::stderr))
        .try_init()
        .context("failed to install tracing subscriber")?;
    Ok(())
}

async fn setting_u64(store: &Store, key: &str, default: u64) -> anyhow::Result<u64> {
    Ok(store
        .get_setting(key)
        .await?
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default))
}

async fn setting_usize(store: &Store, key: &str, default: usize) -> anyhow::Result<usize> {
    Ok(store
        .get_setting(key)
        .await?
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Store;
    use uuid::Uuid;

    #[tokio::test]
    async fn rotation_config_reads_persisted_values() {
        let path = std::env::temp_dir().join(format!("codex-switch-logging-{}.sqlite", Uuid::new_v4()));
        let store = Store::open(path).await.unwrap();
        store
            .set_setting(SETTING_DEBUG_LOG_ENABLED, "true")
            .await
            .unwrap();
        store
            .set_setting(SETTING_LOG_ROTATION_SIZE_MB, "42")
            .await
            .unwrap();
        store
            .set_setting(SETTING_LOG_MAX_FILES, "7")
            .await
            .unwrap();

        let config = LogRotationConfig::load(&store).await.unwrap();

        assert!(config.enabled);
        assert_eq!(config.size_mb, 42);
        assert_eq!(config.max_files, 7);
    }

    #[tokio::test]
    async fn rotation_config_normalizes_invalid_values() {
        let path = std::env::temp_dir().join(format!("codex-switch-logging-{}.sqlite", Uuid::new_v4()));
        let store = Store::open(path).await.unwrap();
        store
            .set_setting(SETTING_LOG_ROTATION_SIZE_MB, "0")
            .await
            .unwrap();
        store
            .set_setting(SETTING_LOG_MAX_FILES, "abc")
            .await
            .unwrap();

        let config = LogRotationConfig::load(&store).await.unwrap();

        assert_eq!(config.size_mb, 1);
        assert_eq!(config.max_files, DEFAULT_LOG_MAX_FILES);
    }

    #[test]
    fn body_logging_flag_can_be_toggled() {
        set_body_logging_enabled(true);
        assert!(body_logging_enabled());
        set_body_logging_enabled(false);
        assert!(!body_logging_enabled());
    }
}

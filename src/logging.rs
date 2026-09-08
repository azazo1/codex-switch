use crate::storage::Store;
use anyhow::Context;
use chrono::{DateTime, Local};
use std::{
    fs::{self, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_rolling_file::{RollingCondition, RollingConditionBase};
use tracing_subscriber::{EnvFilter, Registry, fmt, prelude::*, reload};

const LOG_FILE_ENV: &str = "CODEX_SWITCH_LOG_FILE";
const LOG_BODIES_ENV: &str = "CODEX_SWITCH_LOG_BODIES";
const MAIN_LOG_FILE_NAME: &str = "codex-switch.log";
const PROXY_LOG_FILE_NAME: &str = "codex-switch-proxy.log";

const MAIN_FILTER_BASE: &str = "info,codex_switch::proxy=off,tower_http=off";
const MAIN_FILTER_DEBUG: &str = "info,codex_switch=trace,codex_switch::proxy=off,tower_http=off";
const PROXY_FILTER_BASE: &str = "off,codex_switch::proxy=info";
const PROXY_FILTER_DEBUG: &str = "off,codex_switch::proxy=trace,tower_http=debug";

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
        let size_mb = setting_u64(
            store,
            SETTING_LOG_ROTATION_SIZE_MB,
            DEFAULT_LOG_ROTATION_SIZE_MB,
        )
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

struct RollingLogWriter {
    log_path: PathBuf,
    max_files: usize,
    condition: RollingConditionBase,
    current_size: u64,
    writer: Option<BufWriter<std::fs::File>>,
}

impl RollingLogWriter {
    fn new(log_path: &Path, size_mb: u64, max_files: usize) -> io::Result<Self> {
        let mut writer = Self {
            log_path: log_path.to_path_buf(),
            max_files: max_files.max(1),
            condition: RollingConditionBase::new()
                .daily()
                .max_size(normalized_size_bytes(size_mb)),
            current_size: 0,
            writer: None,
        };
        writer.open_current(false)?;
        Ok(writer)
    }

    fn new_append_only(log_path: &Path, truncate: bool) -> io::Result<Self> {
        let mut writer = Self {
            log_path: log_path.to_path_buf(),
            max_files: 1,
            condition: RollingConditionBase::new(),
            current_size: 0,
            writer: None,
        };
        writer.open_current(truncate)?;
        Ok(writer)
    }

    fn filename_for(&self, index: usize) -> PathBuf {
        if index == 0 {
            return self.log_path.clone();
        }
        let mut name = self.log_path.as_os_str().to_os_string();
        name.push(format!(".{index}"));
        PathBuf::from(name)
    }

    fn open_current(&mut self, truncate: bool) -> io::Result<()> {
        self.writer = None;
        let mut options = OpenOptions::new();
        options.create(true).write(true);
        if truncate {
            options.truncate(true);
        } else {
            options.append(true);
        }
        let file = open_log_file(&self.log_path, &options)?;
        self.current_size = self.log_path.metadata().map_or(0, |meta| meta.len());
        self.writer = Some(BufWriter::new(file));
        Ok(())
    }

    fn ensure_current_exists(&mut self) -> io::Result<()> {
        if self.writer.is_none() || !self.log_path.exists() {
            self.open_current(false)?;
        }
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(writer) = self.writer.as_mut() {
            writer.flush()?;
        }
        self.writer = None;
        let _ = fs::remove_file(self.filename_for(self.max_files));
        let mut result = Ok(());
        for index in (0..self.max_files).rev() {
            let from = self.filename_for(index);
            let to = self.filename_for(index + 1);
            if let Err(err) = fs::rename(&from, &to).or_else(|err| match err.kind() {
                io::ErrorKind::NotFound => Ok(()),
                _ => Err(err),
            }) && result.is_ok()
            {
                result = Err(err);
            }
        }
        result?;
        self.open_current(false)
    }

    fn write_with_datetime(&mut self, buf: &[u8], now: &DateTime<Local>) -> io::Result<usize> {
        if self.condition.should_rollover(now, self.current_size)
            && let Err(err) = self.rotate()
        {
            eprintln!(
                "WARNING: failed to rotate log file {}: {err}",
                self.log_path.display()
            );
        }
        self.ensure_current_exists()?;
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| io::Error::other("rolling log writer is missing"))?;
        writer.write_all(buf)?;
        // fmt 层写完事件不会 flush, 这里必须立即刷盘, 否则 GUI 进程不退出时日志会滞留在 BufWriter 中.
        writer.flush()?;
        self.current_size = self.current_size.saturating_add(buf.len() as u64);
        Ok(buf.len())
    }
}

impl io::Write for RollingLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_with_datetime(buf, &Local::now())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(writer) = self.writer.as_mut() {
            writer.flush()?;
        }
        Ok(())
    }
}

struct FileWriterState {
    non_blocking: NonBlocking,
    guard: WorkerGuard,
}

struct TracingControls {
    main_layer: reload::Handle<FileLayer, Registry>,
    main_writer: Mutex<FileWriterState>,
    proxy_layer: reload::Handle<FileLayer, Registry>,
    proxy_writer: Mutex<FileWriterState>,
}

static CONTROLS: OnceLock<TracingControls> = OnceLock::new();
static BODY_LOGGING_ENABLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn init_tracing(config: LogRotationConfig) -> anyhow::Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .context("failed to create tracing env filter")?;

    if let Some(log_path) = std::env::var_os(LOG_FILE_ENV).filter(|path| !path.is_empty()) {
        set_body_logging_enabled(body_logging_env_enabled());
        let proxy_log_path = proxy_log_file_path()?;
        return init_file_tracing(Path::new(&log_path), &proxy_log_path, env_filter, false);
    }

    if env_override_active() {
        set_body_logging_enabled(body_logging_env_enabled());
        #[cfg(target_os = "windows")]
        {
            let log_path = main_log_file_path()?;
            let proxy_log_path = proxy_log_file_path()?;
            return init_file_tracing(&log_path, &proxy_log_path, env_filter, true);
        }
        #[cfg(not(target_os = "windows"))]
        {
            return init_stderr_tracing(env_filter);
        }
    }

    let log_path = main_log_file_path()?;
    let proxy_log_path = proxy_log_file_path()?;
    let main_state = build_writer_state(&log_path, config.size_mb, config.max_files)?;
    let proxy_state = build_writer_state(&proxy_log_path, config.size_mb, config.max_files)?;
    let main_layer = build_file_layer(main_state.non_blocking.clone(), main_log_filter(config.enabled))
        .context("failed to create main log filter")?;
    let proxy_layer =
        build_file_layer(proxy_state.non_blocking.clone(), proxy_log_filter(config.enabled))
            .context("failed to create proxy log filter")?;
    let (main_layer, main_handle): (
        reload::Layer<FileLayer, Registry>,
        reload::Handle<FileLayer, Registry>,
    ) = reload::Layer::new(main_layer);
    let (proxy_layer, proxy_handle): (
        reload::Layer<FileLayer, Registry>,
        reload::Handle<FileLayer, Registry>,
    ) = reload::Layer::new(proxy_layer);

    let subscriber =
        tracing_subscriber::registry().with(main_layer.and_then(proxy_layer));
    #[cfg(not(target_os = "windows"))]
    let subscriber = {
        let stderr = fmt::layer()
            .with_writer(std::io::stderr)
            .with_filter(EnvFilter::try_new("info").context("failed to create stderr filter")?);
        subscriber.with(stderr)
    };
    subscriber
        .try_init()
        .context("failed to install tracing subscriber")?;

    let controls = TracingControls {
        main_layer: main_handle,
        main_writer: Mutex::new(main_state),
        proxy_layer: proxy_handle,
        proxy_writer: Mutex::new(proxy_state),
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
    reload_file_layer(
        &controls.main_layer,
        &controls.main_writer,
        main_log_filter(enabled),
        "main",
    )?;
    reload_file_layer(
        &controls.proxy_layer,
        &controls.proxy_writer,
        proxy_log_filter(enabled),
        "proxy",
    )?;
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
    let enabled = body_logging_enabled();
    replace_writer_state(
        &controls.main_layer,
        &controls.main_writer,
        &main_log_file_path()?,
        size_mb,
        max_files,
        main_log_filter(enabled),
        "main",
    )?;
    replace_writer_state(
        &controls.proxy_layer,
        &controls.proxy_writer,
        &proxy_log_file_path()?,
        size_mb,
        max_files,
        proxy_log_filter(enabled),
        "proxy",
    )?;
    Ok(())
}

pub(crate) fn body_logging_enabled() -> bool {
    BODY_LOGGING_ENABLED.load(Ordering::Relaxed)
}

pub(crate) fn env_override_active() -> bool {
    std::env::var_os(LOG_FILE_ENV).is_some_and(|path| !path.is_empty())
        || std::env::var_os(LOG_BODIES_ENV).is_some()
}

pub(crate) fn main_log_file_path() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os(LOG_FILE_ENV).filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    Ok(crate::app::data_dir()?.join(MAIN_LOG_FILE_NAME))
}

pub(crate) fn proxy_log_file_path() -> anyhow::Result<PathBuf> {
    if let Some(main_path) = std::env::var_os(LOG_FILE_ENV).filter(|path| !path.is_empty()) {
        let main_path = PathBuf::from(main_path);
        let proxy_name = main_path
            .file_stem()
            .map(|stem| {
                let mut name = stem.to_os_string();
                name.push("-proxy.log");
                name
            })
            .unwrap_or_else(|| PROXY_LOG_FILE_NAME.into());
        return Ok(main_path.with_file_name(proxy_name));
    }
    Ok(crate::app::data_dir()?.join(PROXY_LOG_FILE_NAME))
}

fn main_log_filter(debug: bool) -> &'static str {
    if debug {
        MAIN_FILTER_DEBUG
    } else {
        MAIN_FILTER_BASE
    }
}

fn proxy_log_filter(debug: bool) -> &'static str {
    if debug {
        PROXY_FILTER_DEBUG
    } else {
        PROXY_FILTER_BASE
    }
}

fn build_file_layer(writer: NonBlocking, filter: &'static str) -> anyhow::Result<FileLayer> {
    Ok(fmt::layer()
        .with_ansi(false)
        .with_writer(writer)
        .with_filter(EnvFilter::try_new(filter)?))
}

fn reload_file_layer(
    handle: &reload::Handle<FileLayer, Registry>,
    state: &Mutex<FileWriterState>,
    filter: &'static str,
    label: &'static str,
) -> anyhow::Result<()> {
    let layer = {
        let writer = state
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock {label} log writer"))?;
        build_file_layer(writer.non_blocking.clone(), filter)
            .with_context(|| format!("failed to create {label} log filter"))?
    };
    handle
        .reload(layer)
        .with_context(|| format!("failed to reload {label} log layer"))
}

fn build_writer_state(
    log_path: &Path,
    size_mb: u64,
    max_files: usize,
) -> anyhow::Result<FileWriterState> {
    let writer = build_rolling_writer(log_path, size_mb, max_files)?;
    let (non_blocking, guard) = tracing_appender::non_blocking(writer);
    Ok(FileWriterState { non_blocking, guard })
}

fn replace_writer_state(
    handle: &reload::Handle<FileLayer, Registry>,
    state: &Mutex<FileWriterState>,
    log_path: &Path,
    size_mb: u64,
    max_files: usize,
    filter: &'static str,
    label: &'static str,
) -> anyhow::Result<()> {
    let new_state = build_writer_state(log_path, size_mb, max_files)?;
    reload_file_layer(handle, state, filter, label)?;
    let mut writer = state
        .lock()
        .map_err(|_| anyhow::anyhow!("failed to lock {label} log writer"))?;
    writer.non_blocking = new_state.non_blocking;
    writer.guard = new_state.guard;
    Ok(())
}

fn set_body_logging_enabled(enabled: bool) {
    BODY_LOGGING_ENABLED.store(enabled, Ordering::Relaxed);
}

fn body_logging_env_enabled() -> bool {
    std::env::var(LOG_BODIES_ENV)
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
}

fn build_rolling_writer(
    log_path: &Path,
    size_mb: u64,
    max_files: usize,
) -> anyhow::Result<RollingLogWriter> {
    RollingLogWriter::new(log_path, size_mb, max_files)
        .with_context(|| format!("failed to open rolling log file {}", log_path.display()))
}

fn normalized_size_bytes(size_mb: u64) -> u64 {
    size_mb.max(1).saturating_mul(1024).saturating_mul(1024)
}

fn open_log_file(path: &Path, options: &OpenOptions) -> io::Result<std::fs::File> {
    match options.open(path) {
        Ok(file) => Ok(file),
        Err(err) => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                fs::create_dir_all(parent)?;
                options.open(path)
            } else {
                Err(err)
            }
        }
    }
}

fn init_file_tracing(
    log_path: &Path,
    proxy_log_path: &Path,
    env_filter: EnvFilter,
    append: bool,
) -> anyhow::Result<()> {
    let log_writer = RollingLogWriter::new_append_only(log_path, !append)
        .with_context(|| format!("failed to open log file: {}", log_path.display()))?;
    let proxy_writer = RollingLogWriter::new_append_only(proxy_log_path, !append).with_context(
        || format!("failed to open proxy log file: {}", proxy_log_path.display()),
    )?;
    let proxy_filter =
        EnvFilter::try_new(PROXY_FILTER_DEBUG).context("failed to create proxy env log filter")?;
    let main_filter = env_filter
        .add_directive(
            "codex_switch::proxy=off"
                .parse()
                .context("failed to add proxy off directive to main env filter")?,
        )
        .add_directive(
            "tower_http=off"
                .parse()
                .context("failed to add tower_http off directive to main env filter")?,
        );
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_ansi(false)
                .with_writer(Mutex::new(log_writer))
                .with_filter(main_filter),
        )
        .with(
            fmt::layer()
                .with_ansi(false)
                .with_writer(Mutex::new(proxy_writer))
                .with_filter(proxy_filter),
        )
        .try_init()
        .context("failed to install tracing subscriber")?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn init_stderr_tracing(env_filter: EnvFilter) -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(env_filter),
        )
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
    use chrono::TimeZone;
    use std::io::Write;
    use uuid::Uuid;

    fn temp_log_path() -> PathBuf {
        std::env::temp_dir().join(format!("codex-switch-log-{}", Uuid::new_v4()))
    }

    #[test]
    fn rolling_writer_recreates_file_after_external_delete() {
        let log_path = temp_log_path();
        let mut writer = build_rolling_writer(&log_path, 1, 2).unwrap();
        writer.write_all(b"first line\n").unwrap();
        writer.flush().unwrap();

        fs::remove_file(&log_path).unwrap();
        writer.write_all(b"second line\n").unwrap();
        writer.flush().unwrap();

        let content = fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("second line"));
        assert!(!content.contains("first line"));
    }

    #[test]
    fn append_only_writer_recreates_file_after_external_delete() {
        let log_path = temp_log_path();
        let mut writer = RollingLogWriter::new_append_only(&log_path, true).unwrap();
        writer.write_all(b"first line\n").unwrap();
        writer.flush().unwrap();

        fs::remove_file(&log_path).unwrap();
        writer.write_all(b"second line\n").unwrap();
        writer.flush().unwrap();

        let content = fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("second line"));
        assert!(!content.contains("first line"));
    }

    #[test]
    fn rolling_writer_keeps_daily_rotated_file() {
        let log_path = temp_log_path();
        let mut writer = RollingLogWriter::new(&log_path, 1, 2).unwrap();
        let first = Local
            .with_ymd_and_hms(2026, 8, 4, 12, 0, 0)
            .single()
            .unwrap();
        let second = Local
            .with_ymd_and_hms(2026, 8, 5, 12, 0, 0)
            .single()
            .unwrap();

        writer.write_with_datetime(b"first line\n", &first).unwrap();
        writer
            .write_with_datetime(b"second line\n", &second)
            .unwrap();
        writer.flush().unwrap();

        let current = fs::read_to_string(&log_path).unwrap();
        let rotated = fs::read_to_string(writer.filename_for(1)).unwrap();
        assert!(current.contains("second line"));
        assert!(rotated.contains("first line"));
    }

    #[tokio::test]
    async fn rotation_config_reads_persisted_values() {
        let path =
            std::env::temp_dir().join(format!("codex-switch-logging-{}.sqlite", Uuid::new_v4()));
        let store = Store::open(path).await.unwrap();
        store
            .set_setting(SETTING_DEBUG_LOG_ENABLED, "true")
            .await
            .unwrap();
        store
            .set_setting(SETTING_LOG_ROTATION_SIZE_MB, "42")
            .await
            .unwrap();
        store.set_setting(SETTING_LOG_MAX_FILES, "7").await.unwrap();

        let config = LogRotationConfig::load(&store).await.unwrap();

        assert!(config.enabled);
        assert_eq!(config.size_mb, 42);
        assert_eq!(config.max_files, 7);
    }

    #[tokio::test]
    async fn rotation_config_normalizes_invalid_values() {
        let path =
            std::env::temp_dir().join(format!("codex-switch-logging-{}.sqlite", Uuid::new_v4()));
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

    #[test]
    fn log_filter_constants_are_parseable() {
        for filter in [
            MAIN_FILTER_BASE,
            MAIN_FILTER_DEBUG,
            PROXY_FILTER_BASE,
            PROXY_FILTER_DEBUG,
        ] {
            EnvFilter::try_new(filter).unwrap();
        }
    }

    #[test]
    fn env_override_file_tracing_writes_events() {
        let log_path = temp_log_path();
        let proxy_log_path = temp_log_path();
        let filter = EnvFilter::try_new("codex_switch=trace").unwrap();
        init_file_tracing(&log_path, &proxy_log_path, filter, false).unwrap();
        tracing::info!("env override repro event");
        tracing::trace!(target: "codex_switch::proxy::body", "proxy repro event");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let content = fs::read_to_string(&log_path).unwrap();
        assert!(
            content.contains("env override repro event"),
            "log file is empty or missing the event: {content:?}"
        );
        assert!(
            !content.contains("proxy repro event"),
            "proxy event leaked into the main log file: {content:?}"
        );
        let proxy_content = fs::read_to_string(&proxy_log_path).unwrap();
        assert!(
            proxy_content.contains("proxy repro event"),
            "proxy log file is empty or missing the event: {proxy_content:?}"
        );
    }
}

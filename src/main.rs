#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod balance;
mod balance_alert;
mod cache_keepalive;
mod core;
mod live;
mod oauth;
mod notification;
mod pricing;
mod proxy;
mod quota;
mod scheduler;
mod storage;
mod usage;

use anyhow::Context;
use std::{
    fs::{self, OpenOptions},
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::runtime::Runtime;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

const LOG_FILE_ENV: &str = "CODEX_SWITCH_LOG_FILE";

fn main() -> eframe::Result<()> {
    if let Err(err) = init_tracing() {
        #[cfg(target_os = "windows")]
        let _ = err;
        #[cfg(not(target_os = "windows"))]
        eprintln!("failed to initialize tracing: {err}");
    }

    let runtime =
        Arc::new(Runtime::new().expect("failed to create tokio runtime for codex switch"));
    let app_state = runtime
        .block_on(app::AppState::new())
        .expect("failed to initialize application state");

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Codex Switch")
            .with_app_id("codex-switch")
            .with_icon(app::app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Codex Switch",
        native_options,
        Box::new(move |cc| {
            app::install_fonts(&cc.egui_ctx);
            Ok(Box::new(app::CodexSwitchApp::new(
                runtime,
                app_state,
                cc.egui_ctx.clone(),
            )))
        }),
    )
}

fn init_tracing() -> anyhow::Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .context("failed to create tracing env filter")?;

    if let Some(log_path) = std::env::var_os(LOG_FILE_ENV).filter(|path| !path.is_empty()) {
        return init_file_tracing(Path::new(&log_path), env_filter, false);
    }

    #[cfg(target_os = "windows")]
    {
        let data_dir = app::data_dir()?;
        let log_path = data_dir.join("codex-switch.log");
        init_file_tracing(&log_path, env_filter, true)
    }

    #[cfg(not(target_os = "windows"))]
    {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt::layer())
            .try_init()
            .context("failed to install tracing subscriber")?;
        Ok(())
    }
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

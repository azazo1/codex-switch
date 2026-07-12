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
use std::sync::Arc;
#[cfg(target_os = "windows")]
use std::{
    fs::{self, OpenOptions},
    sync::Mutex,
};
use tokio::runtime::Runtime;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

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

    #[cfg(target_os = "windows")]
    {
        let data_dir = app::data_dir()?;
        fs::create_dir_all(&data_dir).context("failed to create application data directory")?;
        let log_path = data_dir.join("codex-switch.log");
        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("failed to open log file: {}", log_path.display()))?;
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt::layer().with_writer(Mutex::new(log_file)))
            .try_init()
            .context("failed to install tracing subscriber")?;
        Ok(())
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

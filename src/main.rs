mod app;
mod balance;
mod core;
mod oauth;
mod pricing;
mod proxy;
mod quota;
mod scheduler;
mod storage;
mod usage;

use anyhow::Context;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

fn main() -> eframe::Result<()> {
    if let Err(err) = init_tracing() {
        eprintln!("failed to initialize tracing: {err}");
    }

    let runtime = Arc::new(
        Runtime::new().expect("failed to create tokio runtime for codex switch"),
    );
    let app_state = runtime
        .block_on(app::AppState::new())
        .expect("failed to initialize application state");

    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Codex Switch",
        native_options,
        Box::new(move |cc| {
            app::install_fonts(&cc.egui_ctx);
            Ok(Box::new(app::CodexSwitchApp::new(runtime, app_state)))
        }),
    )
}

fn init_tracing() -> anyhow::Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .context("failed to create tracing env filter")?;
    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer())
        .try_init()
        .context("failed to install tracing subscriber")?;
    Ok(())
}

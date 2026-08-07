// Debug 构建保留控制台, 方便终端里 Ctrl+C; release 使用无控制台 GUI 子系统.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod app;
mod balance;
mod balance_alert;
mod cache_keepalive;
mod core;
mod live;
mod logging;
mod oauth;
mod notification;
mod pricing;
mod proxy;
mod quota;
mod scheduler;
mod storage;
mod usage;

use std::sync::Arc;
use tokio::runtime::Runtime;

fn main() -> eframe::Result<()> {
    let runtime =
        Arc::new(Runtime::new().expect("failed to create tokio runtime for codex switch"));
    let default_rotation = logging::LogRotationConfig::default();
    if let Err(err) = logging::init_tracing(default_rotation) {
        #[cfg(target_os = "windows")]
        let _ = err;
        #[cfg(not(target_os = "windows"))]
        eprintln!("failed to initialize tracing: {err}");
    }
    let app_state = runtime
        .block_on(app::AppState::new())
        .expect("failed to initialize application state");
    let rotation_config = runtime
        .block_on(logging::LogRotationConfig::load(&app_state.store))
        .unwrap_or_default();
    if rotation_config.size_mb != default_rotation.size_mb
        || rotation_config.max_files != default_rotation.max_files
    {
        let _ = logging::set_rotation_config(rotation_config.size_mb, rotation_config.max_files);
    }
    let _ = logging::set_debug_log_enabled(rotation_config.enabled);

    let persistence_path = app::data_dir()
        .expect("failed to resolve application data directory")
        .join("window-state.ron");
    app::window_state::sanitize_file(&persistence_path);
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Codex Switch")
            .with_app_id("codex-switch")
            .with_inner_size(app::window_state::DEFAULT_WINDOW_SIZE)
            .with_icon(app::app_icon()),
        persistence_path: Some(persistence_path),
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
                cc.storage,
            )))
        }),
    )
}

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
mod notification;
mod oauth;
mod peer;
mod pricing;
mod proxy;
mod quota;
mod scheduler;
mod storage;
mod update;
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

    // 同一数据目录只允许一个实例: 二次启动把启动参数转发给已有实例后退出.
    let single_instance = match runtime.block_on(async {
        let data_dir = app::data_dir()?;
        app::single_instance::acquire(&data_dir).await
    }) {
        Ok(app::single_instance::AcquireOutcome::Primary(listener)) => Some(Arc::new(listener)),
        Ok(app::single_instance::AcquireOutcome::Duplicate) => {
            tracing::info!("another codex switch instance is running, exiting");
            return Ok(());
        }
        Err(err) => {
            tracing::warn!(error = %err, "single instance lock unavailable, continuing without it");
            None
        }
    };

    let app_state = runtime
        .block_on(app::AppState::new(single_instance))
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
    let hide_on_launch = runtime
        .block_on(app_state.store.get_setting(app::SETTING_HIDE_ON_LAUNCH))
        .ok()
        .flatten()
        .as_deref()
        == Some("true");

    // 从终端启动的实例收到 Ctrl+C 后转成退出请求, 由统一退出入口优雅收尾.
    {
        let events = app_state.events.clone();
        runtime.spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::info!("received Ctrl+C, shutting down gracefully");
                events.request_exit();
            }
        });
    }

    let persistence_path = app::data_dir()
        .expect("failed to resolve application data directory")
        .join("window-state.ron");
    app::window_state::sanitize_file(&persistence_path);
    // macOS 上不能带着 fullscreen 创建窗口, 详见 take_initial_fullscreen 的说明.
    #[cfg(target_os = "macos")]
    let defer_fullscreen = app::window_state::take_initial_fullscreen(&persistence_path);
    #[cfg(not(target_os = "macos"))]
    let defer_fullscreen = false;
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Codex Switch")
            .with_app_id("codex-switch")
            .with_inner_size(app::window_state::DEFAULT_WINDOW_SIZE)
            .with_icon(app::app_icon())
            .with_visible(!hide_on_launch),
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
                hide_on_launch,
                defer_fullscreen,
            )))
        }),
    )
}

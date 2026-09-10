mod build_info;
mod fonts;
pub(crate) mod http;
mod icon;
mod platform;
pub(crate) mod single_instance;
mod state;
mod tray;
mod ui;
pub(crate) mod window_state;

pub use build_info::display_version;
pub use fonts::install_fonts;
pub use icon::app_icon;
pub use state::{AppEvents, AppState};
pub(crate) use state::{
    SETTING_HIDE_ON_LAUNCH, SETTING_START_PEER_ON_LAUNCH, SETTING_START_SERVER_ON_LAUNCH, data_dir,
};
#[cfg(target_os = "macos")]
pub(crate) use state::SETTING_DOCK_ICON_FOLLOWS_WINDOW;
pub use ui::CodexSwitchApp;

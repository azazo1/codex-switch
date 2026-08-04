mod build_info;
mod fonts;
pub(crate) mod http;
mod icon;
mod platform;
mod state;
mod tray;
mod ui;

pub use build_info::display_version;
pub use fonts::install_fonts;
pub use icon::app_icon;
pub(crate) use state::data_dir;
pub use state::{AppEvents, AppState};
pub use ui::CodexSwitchApp;

mod fonts;
mod icon;
mod platform;
mod state;
mod tray;
mod ui;

pub use fonts::install_fonts;
pub use icon::app_icon;
pub use state::{AppEvents, AppState};
pub use ui::CodexSwitchApp;

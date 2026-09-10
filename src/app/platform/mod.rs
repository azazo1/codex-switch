#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "macos")]
pub(crate) use macos::tray_title;

#[cfg(target_os = "linux")]
pub use linux::{
    BackgroundReopenMonitor, hide_from_dock, open_file_location, open_url, show_in_dock,
};
#[cfg(target_os = "macos")]
pub use macos::{
    BackgroundReopenMonitor, hide_from_dock, open_file_location, open_url, show_in_dock,
};
#[cfg(target_os = "macos")]
pub(crate) use macos::install_window_shortcuts;
#[cfg(not(target_os = "macos"))]
pub(crate) fn install_window_shortcuts() {}
#[cfg(target_os = "windows")]
pub use windows::{
    BackgroundReopenMonitor, hide_from_dock, open_file_location, open_url, show_in_dock,
};

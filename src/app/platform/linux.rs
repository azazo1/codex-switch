use anyhow::{Context, bail};
use std::{path::Path, process::Command};

#[derive(Debug, Default)]
pub struct BackgroundReopenMonitor {
    _private: (),
}

impl BackgroundReopenMonitor {
    pub fn mark_hidden(&mut self) {}

    pub fn mark_shown(&mut self) {}

    pub fn should_show_hidden_window(&mut self) -> bool {
        false
    }
}

pub fn hide_from_dock() {}

pub fn show_in_dock() {}

pub fn open_file_location(path: impl AsRef<Path>) -> anyhow::Result<()> {
    let path = path.as_ref();
    let status = Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .status()
        .context("failed to open file location")?;
    if !status.success() {
        bail!("file browser returned {status}");
    }
    Ok(())
}

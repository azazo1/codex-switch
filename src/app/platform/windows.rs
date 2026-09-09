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
    let status = Command::new("explorer")
        .arg(format!("/select,{}", path.as_ref().display()))
        .status()
        .context("failed to open file location")?;
    if !status.success() {
        bail!("file browser returned {status}");
    }
    Ok(())
}

pub fn open_url(url: &str) -> anyhow::Result<()> {
    // start 是 cmd 内建命令, 第一个空参数是窗口标题占位.
    let status = Command::new("cmd")
        .args(["/C", "start", "", url])
        .status()
        .context("failed to open url")?;
    if !status.success() {
        bail!("start returned {status}");
    }
    Ok(())
}

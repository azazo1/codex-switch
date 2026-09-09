//! 平台相关的自更新安装策略.
//!
//! Linux/Windows: 解包新二进制后通过 rename 让位并替换自身, 随后可重启生效;
//! macOS: 产物是 dmg, 程序内无法静默替换已安装的 .app, 只能打开镜像引导拖拽.

use anyhow::Context;
use std::path::{Path, PathBuf};

/// 单次安装的结果, 决定 UI 的后续引导.
/// 各平台只构造其中部分变体.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum InstallOutcome {
    /// 二进制已替换完成, 等待重启生效.
    Replaced,
    /// dmg 已在系统中打开, 需要用户手动拖拽安装.
    DmgOpened,
}

#[cfg(target_os = "macos")]
pub(crate) fn apply(archive: &Path) -> anyhow::Result<InstallOutcome> {
    std::process::Command::new("open")
        .arg(archive)
        .spawn()
        .with_context(|| format!("failed to open {}", archive.display()))?;
    tracing::info!(path = %archive.display(), "dmg opened for manual install");
    Ok(InstallOutcome::DmgOpened)
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(crate) fn apply(archive: &Path) -> anyhow::Result<InstallOutcome> {
    let dest_dir = archive
        .parent()
        .context("archive has no parent directory")?
        .to_path_buf();
    extract_archive(archive, &dest_dir)?;
    let binary_name = if cfg!(target_os = "windows") {
        "codex-switch.exe"
    } else {
        "codex-switch"
    };
    let extracted = dest_dir.join(binary_name);
    anyhow::ensure!(
        extracted.is_file(),
        "downloaded archive does not contain {binary_name}"
    );
    replace_running_binary(&extracted)?;
    tracing::info!(path = %archive.display(), "binary replaced, restart required");
    Ok(InstallOutcome::Replaced)
}

/// 清理上次更新遗留的旧二进制备份, 被占用时静默留待下次.
pub(crate) fn cleanup_stale_backup() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let backup = backup_path(&exe);
    if !backup.exists() {
        return;
    }
    match std::fs::remove_file(&backup) {
        Ok(()) => tracing::info!(path = %backup.display(), "removed stale update backup"),
        Err(err) => {
            tracing::debug!(error = %err, "failed to remove stale update backup")
        }
    }
}

/// 启动一个新的应用进程用于接管服务, 调用方随后退出当前进程.
pub(crate) fn spawn_restart() -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("failed to locate current executable")?;
    std::process::Command::new(exe)
        .spawn()
        .context("failed to spawn updated binary")?;
    Ok(())
}

fn backup_path(exe: &Path) -> PathBuf {
    let mut name = exe
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".old");
    exe.with_file_name(name)
}

/// 以 rename 让位的方式替换正在运行的二进制, 失败时回滚保证可用性.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn replace_running_binary(new_binary: &Path) -> anyhow::Result<()> {
    let current = std::env::current_exe().context("failed to locate current executable")?;
    let backup = backup_path(&current);
    // 运行中的二进制允许 rename, 不允许覆盖删除, 因此先让位.
    if backup.exists() {
        let _ = std::fs::remove_file(&backup);
    }
    move_file(&current, &backup)
        .with_context(|| format!("failed to move {}", current.display()))?;
    if let Err(err) = move_file(new_binary, &current) {
        let _ = move_file(&backup, &current);
        return Err(err).context("failed to install new binary, rolled back");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o755))
            .context("failed to mark binary executable")?;
    }
    Ok(())
}

/// rename 失败 (例如跨文件系统) 时退回复制后删除.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn move_file(from: &Path, to: &Path) -> anyhow::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)
        .with_context(|| format!("failed to copy {}", from.display()))?;
    std::fs::remove_file(from)
        .with_context(|| format!("failed to remove {}", from.display()))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn extract_archive(archive: &Path, dest_dir: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(archive)
        .with_context(|| format!("failed to open {}", archive.display()))?;
    let decoder = flate2::read::GzDecoder::new(file);
    tar::Archive::new(decoder)
        .unpack(dest_dir)
        .with_context(|| format!("failed to unpack {}", archive.display()))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn extract_archive(archive: &Path, dest_dir: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(archive)
        .with_context(|| format!("failed to open {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .with_context(|| format!("failed to read {}", archive.display()))?;
    zip.extract(dest_dir)
        .with_context(|| format!("failed to unpack {}", archive.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_name_appends_old_suffix() {
        let exe = Path::new("/opt/codex-switch/bin/codex-switch");
        assert_eq!(
            backup_path(exe),
            PathBuf::from("/opt/codex-switch/bin/codex-switch.old")
        );
        let exe = Path::new("C:\\app\\codex-switch.exe");
        assert_eq!(
            backup_path(exe),
            PathBuf::from("C:\\app\\codex-switch.exe.old")
        );
    }
}

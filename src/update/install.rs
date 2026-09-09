//! 平台相关的自更新安装策略.
//!
//! Linux/Windows: 解包新二进制后通过 rename 让位并替换自身, 随后可重启生效;
//! macOS: 已安装的 .app 在进程存活时不允许覆盖, 因此释放一个脱离父进程的
//! 替换脚本, 由它等旧进程退出后挂载 dmg 并替换 bundle, 最后重新拉起应用.

use anyhow::Context;
use std::path::{Path, PathBuf};

/// 单次安装的结果, 决定 UI 的后续引导.
/// 各平台只构造其中部分变体.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum InstallOutcome {
    /// 二进制已替换完成, 等待重启生效.
    Replaced,
    /// 替换脚本已接管: 当前进程退出后由它替换 bundle 并重新拉起应用.
    HandedOff,
    /// dmg 已在系统中打开, 需要用户手动拖拽安装.
    DmgOpened,
}

#[cfg(target_os = "macos")]
pub(crate) fn apply(archive: &Path) -> anyhow::Result<InstallOutcome> {
    macos::hand_off(archive)
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

/// 清理上次 macOS 自更新遗留的 bundle 备份, 替换脚本与残留挂载点.
///
/// bundle 备份是目录, 与二进制备份分开处理; 脚本已被执行完毕, 直接删除;
/// 日志保留, 便于用户排查替换失败的原因.
#[cfg(target_os = "macos")]
pub(crate) fn cleanup_stale_macos_artifacts() {
    if let Ok(Some(bundle)) = macos::installed_bundle() {
        let backup = backup_path(&bundle);
        if backup.exists() {
            match std::fs::remove_dir_all(&backup) {
                Ok(()) => tracing::info!(path = %backup.display(), "removed stale bundle backup"),
                Err(err) => {
                    tracing::debug!(error = %err, "failed to remove stale bundle backup")
                }
            }
        }
    }
    let Ok(dir) = crate::update::update_dir() else {
        return;
    };
    let script = dir.join(macos::HELPER_NAME);
    if script.exists() {
        let _ = std::fs::remove_file(&script);
    }
    // 上次替换若被强杀, 挂载点目录可能残留, 重新挂载前必须先清掉.
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("mount-"))
        {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn cleanup_stale_macos_artifacts() {}

/// 读取并清除替换脚本留下的结果文件, 返回失败原因.
///
/// 脚本在应用退出后才执行, 失败信息只能落盘; 新进程启动时取出来展示给用户.
#[cfg(target_os = "macos")]
pub(crate) fn take_handoff_result() -> Option<String> {
    let path = crate::update::update_dir().ok()?.join(macos::RESULT_NAME);
    let content = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let message = content.trim();
    (!message.is_empty()).then(|| message.to_string())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn take_handoff_result() -> Option<String> {
    None
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
    std::fs::copy(from, to).with_context(|| format!("failed to copy {}", from.display()))?;
    std::fs::remove_file(from).with_context(|| format!("failed to remove {}", from.display()))?;
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

/// macOS 的 bundle 自替换.
///
/// 进程存活时 LaunchServices 会占用已安装的 .app, 覆盖会被系统拒绝, 因此
/// 替换工作交给一个脱离父进程的 shell 脚本: 当前进程立即退出, 脚本等旧 PID
/// 消失后挂载 dmg, 把新 bundle 复制到同卷临时目录, 再以 rename 让位的方式
/// 换掉旧 bundle, 最后重新拉起应用. 任一步失败都回滚并退回打开 dmg 的手动路径.
#[cfg(target_os = "macos")]
mod macos {
    use super::{InstallOutcome, backup_path};
    use anyhow::Context;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    pub(super) const HELPER_NAME: &str = "apply-update.sh";
    pub(super) const HELPER_LOG_NAME: &str = "apply-update.log";
    pub(super) const RESULT_NAME: &str = "apply-update-result.txt";

    pub(super) fn hand_off(archive: &Path) -> anyhow::Result<InstallOutcome> {
        let Some(target) = installed_bundle()? else {
            // 便携运行 (直接跑二进制) 没有可替换的 bundle, 退回手动引导.
            return open_dmg(archive, "no installed app bundle found");
        };
        let work_dir = archive
            .parent()
            .context("archive has no parent directory")?
            .to_path_buf();
        std::fs::create_dir_all(&work_dir)
            .with_context(|| format!("failed to create {}", work_dir.display()))?;
        let helper = work_dir.join(HELPER_NAME);
        let script = helper_script(archive, &target, &work_dir);
        std::fs::write(&helper, script)
            .with_context(|| format!("failed to write {}", helper.display()))?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755))
                .with_context(|| format!("failed to mark {} executable", helper.display()))?;
        }
        std::process::Command::new(&helper)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("failed to launch {}", helper.display()))?;
        tracing::info!(
            helper = %helper.display(),
            bundle = %target.display(),
            "macos update helper started, waiting for this process to exit"
        );
        Ok(InstallOutcome::HandedOff)
    }

    /// 定位当前运行的应用 bundle, 不在 bundle 中运行时返回 None.
    pub(super) fn installed_bundle() -> anyhow::Result<Option<PathBuf>> {
        let exe = std::env::current_exe().context("failed to locate current executable")?;
        // 期望结构: <bundle>/Contents/MacOS/<exe>.
        let Some(macos_dir) = exe.parent() else {
            return Ok(None);
        };
        if macos_dir.file_name() != Some(OsStr::new("MacOS")) {
            return Ok(None);
        }
        let Some(contents_dir) = macos_dir.parent() else {
            return Ok(None);
        };
        if contents_dir.file_name() != Some(OsStr::new("Contents")) {
            return Ok(None);
        }
        let Some(bundle) = contents_dir.parent() else {
            return Ok(None);
        };
        if bundle.extension() != Some(OsStr::new("app")) {
            return Ok(None);
        }
        Ok(Some(bundle.to_path_buf()))
    }

    fn open_dmg(archive: &Path, reason: &str) -> anyhow::Result<InstallOutcome> {
        std::process::Command::new("open")
            .arg(archive)
            .spawn()
            .with_context(|| format!("failed to open {}", archive.display()))?;
        tracing::info!(
            path = %archive.display(),
            reason,
            "dmg opened for manual install"
        );
        Ok(InstallOutcome::DmgOpened)
    }

    /// 生成替换脚本. 路径以占位符注入, 避免脚本里的 `${...}` 和 `{` 与格式化语法冲突.
    pub(super) fn helper_script(archive: &Path, bundle: &Path, work_dir: &Path) -> String {
        let pid = std::process::id();
        let mount_point = work_dir.join(format!("mount-{pid}"));
        let replacements = [
            ("__ARCHIVE__", archive.to_string_lossy().into_owned()),
            ("__BUNDLE__", bundle.to_string_lossy().into_owned()),
            (
                "__BACKUP__",
                backup_path(bundle).to_string_lossy().into_owned(),
            ),
            (
                "__LOG__",
                work_dir.join(HELPER_LOG_NAME).to_string_lossy().into_owned(),
            ),
            (
                "__MOUNT_POINT__",
                mount_point.to_string_lossy().into_owned(),
            ),
            (
                "__RESULT__",
                work_dir.join(RESULT_NAME).to_string_lossy().into_owned(),
            ),
            ("__APP_PID__", pid.to_string()),
        ];
        let mut script = SCRIPT_TEMPLATE.to_string();
        for (placeholder, value) in replacements {
            script = script.replace(placeholder, &shell_quote(&value));
        }
        script
    }

    /// 以 shell 单引号包裹, 内部单引号转义为 '\''.
    pub(super) fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', r"'\''"))
    }

    const SCRIPT_TEMPLATE: &str = r#"#!/bin/bash
# Codex Switch 自更新替换脚本, 由应用在下载校验完成后写出并立即启动.
# 不接收参数, 路径在生成时已内联, 保证脱离父进程后仍可独立执行.
set -uo pipefail

archive=__ARCHIVE__
bundle=__BUNDLE__
backup=__BACKUP__
log=__LOG__
mount_point=__MOUNT_POINT__
result=__RESULT__
app_pid=__APP_PID__
app_name="Codex Switch"

exec >>"$log" 2>&1
# 应用退出时父会话可能发出 SIGHUP, 替换必须活到结束.
trap '' HUP
echo "[$(date '+%Y-%m-%d %H:%M:%S')] helper started, waiting for pid $app_pid to exit"

# 暂存目录放在 bundle 同目录, 保证最后一步 rename 不跨卷.
bundle_dir="$(dirname "$bundle")"
stage_dir="$bundle_dir/.${app_name##*/}.new-$app_pid"
rm -rf "$stage_dir"
rm -f "$result"

# 应用已退出, 失败时必须把原因落盘并重新拉起, 否则用户面对的是"应用不见了".
fail() {
    echo "$1"
    printf '%s\n' "$1" > "$result"
    open "$bundle" >/dev/null 2>&1
    exit 1
}

attached=false
cleanup() {
    if [[ "$attached" == true ]]; then
        hdiutil detach "$mount_point" -quiet -force >/dev/null 2>&1
    fi
    rm -rf "$stage_dir"
    rm -rf "$mount_point"
}
trap cleanup EXIT

# 等旧进程退出, 否则 bundle 仍被 LaunchServices 占用.
for _ in $(seq 1 300); do
    if ! kill -0 "$app_pid" 2>/dev/null; then
        break
    fi
    sleep 0.2
done
if kill -0 "$app_pid" 2>/dev/null; then
    # 旧进程仍在, 此时替换会被系统拒绝, 放弃并让用户稍后重试.
    fail "更新已取消: 应用在 60 秒内没有退出, 请重新点击立即更新."
fi

# 提前确认 bundle 所在目录可写, 否则不打断用户.
if ! mkdir -p "$stage_dir"; then
    fail "更新失败: 无法写入 $bundle_dir, 请手动安装."
fi

# 挂载 dmg 并定位其中的 app bundle.
if ! hdiutil attach "$archive" -nobrowse -quiet -mountpoint "$mount_point"; then
    fail "更新失败: 无法挂载安装镜像, 请手动安装."
fi
attached=true

src_app=""
for candidate in "$mount_point"/*.app; do
    if [[ -d "$candidate" ]]; then
        src_app="$candidate"
        break
    fi
done
if [[ -z "$src_app" ]]; then
    fail "更新失败: 安装镜像中没有找到 .app, 请手动安装."
fi

if ! ditto "$src_app" "$stage_dir"; then
    fail "更新失败: 复制新版本到本地失败, 请手动安装."
fi
hdiutil detach "$mount_point" -quiet -force >/dev/null 2>&1
attached=false

# 清除隔离属性, 避免替换后首次启动被 Gatekeeper 拦截.
xattr -dr com.apple.quarantine "$stage_dir" >/dev/null 2>&1

# 旧 bundle 让位; 此时旧进程已退出, 失败通常意味着磁盘权限或空间问题.
rm -rf "$backup"
if ! mv "$bundle" "$backup"; then
    fail "更新失败: 无法替换已安装的应用, 请手动安装."
fi
if ! mv "$stage_dir" "$bundle"; then
    mv "$backup" "$bundle"
    fail "更新失败: 安装新版本失败, 已回滚到原版本."
fi
rm -rf "$backup"

# 重新拉起应用; 单实例锁已随旧进程退出释放.
if ! open "$bundle"; then
    fail "更新已安装, 但自动重启失败, 请手动打开应用."
fi
echo "[$(date '+%Y-%m-%d %H:%M:%S')] update applied, $app_name relaunched"
exit 0
"#;
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::macos;
    use std::path::Path;

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(macos::shell_quote("/tmp/a b"), "'/tmp/a b'");
        assert_eq!(macos::shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn helper_script_keeps_paths_intact() {
        let script = macos::helper_script(
            Path::new("/Users/me/Library/Application Support/Codex Switch/update/a.dmg"),
            Path::new("/Applications/Codex Switch.app"),
            Path::new("/Users/me/Library/Application Support/Codex Switch/update"),
        );
        assert!(script.starts_with("#!/bin/bash\n"));
        // 所有占位符都应被替换.
        assert!(!script.contains("__ARCHIVE__"));
        assert!(!script.contains("__BUNDLE__"));
        assert!(script.contains("'/Applications/Codex Switch.app'"));
        assert!(script.contains("'/Applications/Codex Switch.app.old'"));
        assert!(script.contains("'/Users/me/Library/Application Support/Codex Switch/update/a.dmg'"));
    }

    #[test]
    fn helper_script_is_valid_bash() {
        let script = macos::helper_script(
            Path::new("/tmp/update/a.dmg"),
            Path::new("/Applications/Codex Switch.app"),
            Path::new("/tmp/update"),
        );
        let dir = std::env::temp_dir().join(format!("codex-switch-helper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("apply-update.sh");
        std::fs::write(&path, &script).unwrap();
        let status = std::process::Command::new("bash")
            .arg("-n")
            .arg(&path)
            .status()
            .expect("failed to run bash -n");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(status.success(), "generated helper script has syntax errors");
    }
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

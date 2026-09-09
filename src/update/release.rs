//! GitHub Release 查询, 版本比较与当前平台的发布资产匹配.

use crate::logging::network::HttpClient;
use anyhow::Context;
use serde::Deserialize;

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/azazo1/codex-switch/releases/latest";

/// 当前平台对应的最新稳定 release 信息, 已选定归档和校验和资产.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReleaseInfo {
    pub tag: String,
    pub html_url: String,
    pub body: String,
    pub archive: ReleaseAsset,
    pub checksums: ReleaseAsset,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
}

#[derive(Debug, Deserialize)]
struct LatestReleaseResponse {
    tag_name: String,
    html_url: String,
    body: Option<String>,
    assets: Vec<LatestAssetResponse>,
}

#[derive(Debug, Deserialize)]
struct LatestAssetResponse {
    name: String,
    browser_download_url: String,
}

/// 请求 GitHub latest release (自动排除 draft 和 prerelease), 匹配当前平台资产.
pub(crate) async fn fetch_latest(client: &HttpClient) -> anyhow::Result<ReleaseInfo> {
    let response = client
        .get(LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("failed to request latest release")?;
    let status = response.status();
    if !status.is_success() {
        if status.as_u16() == 403 || status.as_u16() == 429 {
            anyhow::bail!("GitHub API rate limited (status {status})");
        }
        anyhow::bail!("latest release request failed with status {status}");
    }
    let release: LatestReleaseResponse = response
        .json()
        .await
        .context("failed to decode latest release response")?;

    // 发布资产名使用剥离 v 前缀的版本号 (见 build-version.sh 的 artifact 输出).
    let tag_version = release.tag_name.trim_start_matches('v');
    let platform = platform_id()?;
    let arch = arch_id()?;
    let archive_name = archive_name_for(tag_version, platform, arch);
    let archive = select_asset(&release.assets, &archive_name)?;
    let checksums = select_asset(&release.assets, "SHA256SUMS")?;

    Ok(ReleaseInfo {
        tag: release.tag_name,
        html_url: release.html_url,
        body: release.body.unwrap_or_default(),
        archive: ReleaseAsset {
            name: archive.name.clone(),
            download_url: archive.browser_download_url.clone(),
        },
        checksums: ReleaseAsset {
            name: checksums.name.clone(),
            download_url: checksums.browser_download_url.clone(),
        },
    })
}

/// 若 `tag` 比当前构建版本新, 返回其 semver; 当前为开发构建 (无 semver) 时始终返回 None.
pub(crate) fn is_newer_version(tag: &str) -> Option<semver::Version> {
    let current = parse_semver(crate::app::display_version())?;
    is_newer_than(current, tag)
}

fn is_newer_than(current: semver::Version, tag: &str) -> Option<semver::Version> {
    let candidate = parse_semver(tag)?;
    (candidate > current).then_some(candidate)
}

/// 从构建版本字符串中提取 semver, 兼容 `v0.13.0` / `v0.13.0-abc1234` / `v0.13.0^abc1234`.
fn parse_semver(raw: &str) -> Option<semver::Version> {
    let core = raw.trim().strip_prefix('v')?;
    let core = core.split(['-', '^']).next()?;
    semver::Version::parse(core).ok()
}

fn platform_id() -> anyhow::Result<&'static str> {
    if cfg!(target_os = "linux") {
        Ok("linux")
    } else if cfg!(target_os = "macos") {
        Ok("macos")
    } else if cfg!(target_os = "windows") {
        Ok("windows")
    } else {
        anyhow::bail!("auto update is not supported on this platform")
    }
}

fn arch_id() -> anyhow::Result<&'static str> {
    if cfg!(target_arch = "x86_64") {
        Ok("x86_64")
    } else if cfg!(target_arch = "aarch64") {
        Ok("aarch64")
    } else {
        anyhow::bail!("auto update is not supported on this CPU architecture")
    }
}

fn archive_ext(platform: &str) -> &'static str {
    match platform {
        "linux" => "tar.gz",
        "windows" => "zip",
        "macos" => "dmg",
        _ => "",
    }
}

fn archive_name_for(tag: &str, platform: &str, arch: &str) -> String {
    format!(
        "codex-switch-{tag}-{platform}-{arch}.{}",
        archive_ext(platform)
    )
}

fn select_asset<'a>(
    assets: &'a [LatestAssetResponse],
    name: &str,
) -> anyhow::Result<&'a LatestAssetResponse> {
    assets
        .iter()
        .find(|asset| asset.name == name)
        .ok_or_else(|| anyhow::anyhow!("release asset not found: {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_semver_strips_build_suffixes() {
        assert_eq!(
            parse_semver("v0.13.0").map(|v| v.to_string()),
            Some("0.13.0".to_string())
        );
        assert_eq!(
            parse_semver("v0.13.0-8e56cd0").map(|v| v.to_string()),
            Some("0.13.0".to_string())
        );
        assert_eq!(
            parse_semver("v0.13.0^8e56cd0").map(|v| v.to_string()),
            Some("0.13.0".to_string())
        );
        assert_eq!(parse_semver("garbage"), None);
    }

    #[test]
    fn newer_version_requires_semantic_greater() {
        let current = semver::Version::new(0, 13, 0);
        // 同版本视为不更新, 覆盖本地 dev 构建带 commit 后缀的场景.
        assert_eq!(is_newer_than(current.clone(), "v0.13.0"), None);
        assert_eq!(is_newer_than(current.clone(), "v0.13.0-abc"), None);
        assert!(is_newer_than(current.clone(), "v0.14.0").is_some());
        assert_eq!(is_newer_than(current, "v0.12.9"), None);
    }

    #[test]
    fn archive_names_follow_ci_layout() {
        // CI 产物名不带 v 前缀, 例如 codex-switch-0.14.0-macos-aarch64.dmg.
        assert_eq!(
            archive_name_for("0.14.0", "linux", "x86_64"),
            "codex-switch-0.14.0-linux-x86_64.tar.gz"
        );
        assert_eq!(
            archive_name_for("0.14.0", "windows", "aarch64"),
            "codex-switch-0.14.0-windows-aarch64.zip"
        );
        assert_eq!(
            archive_name_for("0.14.0", "macos", "aarch64"),
            "codex-switch-0.14.0-macos-aarch64.dmg"
        );
    }

    #[test]
    fn select_asset_matches_exact_name() {
        let assets = vec![
            LatestAssetResponse {
                name: "codex-switch-v0.14.0-macos-aarch64.dmg".to_string(),
                browser_download_url: "https://example.com/dmg".to_string(),
            },
            LatestAssetResponse {
                name: "SHA256SUMS".to_string(),
                browser_download_url: "https://example.com/sums".to_string(),
            },
        ];
        assert_eq!(
            select_asset(&assets, "SHA256SUMS")
                .unwrap()
                .browser_download_url,
            "https://example.com/sums"
        );
        assert!(select_asset(&assets, "codex-switch-v0.14.0-linux-x86_64.tar.gz").is_err());
    }
}

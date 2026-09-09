//! 发布归档下载与 SHA256 校验.

use super::release::ReleaseAsset;
use crate::logging::network::HttpClient;
use anyhow::Context;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// 解析 SHA256SUMS 内容, 返回指定归档名的十六进制摘要.
pub(crate) fn checksum_line_for(content: &str, archive_name: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        // sha256sum 的二进制模式会在文件名前带 `*` 标记.
        let file = parts.next()?.trim().trim_start_matches('*');
        (file == archive_name).then(|| hash.to_string())
    })
}

/// 下载 SHA256SUMS 并提取目标归档的期望摘要.
pub(crate) async fn fetch_checksum_for(
    client: &HttpClient,
    checksums: &ReleaseAsset,
    archive_name: &str,
) -> anyhow::Result<String> {
    let response = client
        .get(&checksums.download_url)
        .send()
        .await
        .with_context(|| format!("failed to download {}", checksums.name))?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("download of {} failed with status {status}", checksums.name);
    }
    let text = response
        .text()
        .await
        .with_context(|| format!("failed to read {}", checksums.name))?;
    checksum_line_for(&text, archive_name)
        .ok_or_else(|| anyhow::anyhow!("SHA256SUMS has no entry for {archive_name}"))
}

/// 流式下载归档到 `dest_dir`, 校验通过后以正式文件名落盘, 返回其路径.
///
/// 进度回调按收到的数据块触发, `total` 来自 Content-Length, 可能未知.
pub(crate) async fn download_archive(
    client: &HttpClient,
    asset: &ReleaseAsset,
    dest_dir: &Path,
    expected_sha256: &str,
    mut on_progress: impl FnMut(u64, Option<u64>) + Send,
) -> anyhow::Result<PathBuf> {
    tokio::fs::create_dir_all(dest_dir)
        .await
        .with_context(|| format!("failed to create update dir {}", dest_dir.display()))?;
    let dest = dest_dir.join(&asset.name);
    let temp = dest_dir.join(format!("{}.part", asset.name));

    let response = client
        .get(&asset.download_url)
        .send()
        .await
        .with_context(|| format!("failed to download {}", asset.name))?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("download of {} failed with status {status}", asset.name);
    }
    let total = response.content_length();

    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(&temp)
        .await
        .with_context(|| format!("failed to create {}", temp.display()))?;
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("download stream interrupted")?;
        file.write_all(&chunk)
            .await
            .with_context(|| format!("failed to write {}", temp.display()))?;
        hasher.update(&chunk);
        received += chunk.len() as u64;
        on_progress(received, total);
    }
    file.flush().await.context("failed to flush download")?;
    drop(file);

    let actual: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        let _ = tokio::fs::remove_file(&temp).await;
        anyhow::bail!("checksum mismatch for {}", asset.name);
    }
    tokio::fs::rename(&temp, &dest)
        .await
        .with_context(|| format!("failed to finalize {}", dest.display()))?;
    tracing::info!(path = %dest.display(), bytes = received, "release archive downloaded");
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_line_matches_by_filename() {
        let content = "aaa111  codex-switch-v0.14.0-linux-x86_64.tar.gz\n\
                        bbb222  codex-switch-v0.14.0-macos-aarch64.dmg\n";
        assert_eq!(
            checksum_line_for(content, "codex-switch-v0.14.0-macos-aarch64.dmg"),
            Some("bbb222".to_string())
        );
        assert_eq!(checksum_line_for(content, "missing.zip"), None);
    }

    #[test]
    fn checksum_line_tolerates_binary_marker_and_crlf() {
        let content = "ccc333 *codex-switch-v0.14.0-windows-x86_64.zip\r\n";
        assert_eq!(
            checksum_line_for(content, "codex-switch-v0.14.0-windows-x86_64.zip"),
            Some("ccc333".to_string())
        );
    }
}

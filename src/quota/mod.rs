mod codex;
pub mod plan;

pub use codex::{query_and_store, snapshot_from_headers};

use crate::core::models::{QuotaSnapshot, QuotaWindow, QuotaWindowKind, Upstream, UpstreamKind};

/// 该上游是否可能带额度窗口: 已支持的套餐 Base URL, 或 Codex OAuth 的 5h / 7d 额度.
///
/// 识别不出来时上游编辑器会把开关置灰, 这里也是运行时回落金额余额的依据.
pub fn upstream_supports_windows(upstream: &Upstream) -> bool {
    upstream.kind == UpstreamKind::CodexOauth || plan::detect(&upstream.base_url).is_some()
}

/// 该上游当前是否展示额度窗口.
///
/// 数据库里被强行勾上但识别不出接口时返回 false, 调用方因此显示金额余额而不是崩溃.
pub fn shows_quota_windows(upstream: &Upstream) -> bool {
    upstream.show_quota_windows && upstream_supports_windows(upstream)
}

/// 勾选了额度窗口但没有窗口数据时的占位文字.
pub const NO_QUOTA_WINDOWS: &str = "无额度窗口";

/// Codex OAuth 的 5h / 7d 额度快照, 转成通用的窗口表示.
pub fn codex_windows(snapshot: &QuotaSnapshot) -> Vec<QuotaWindow> {
    let mut windows = Vec::new();
    if let Some(used_percent) = snapshot.used_5h_percent {
        windows.push(QuotaWindow::new(QuotaWindowKind::FiveHour, used_percent));
    }
    if let Some(used_percent) = snapshot.used_7d_percent {
        windows.push(QuotaWindow::new(QuotaWindowKind::Weekly, used_percent));
    }
    windows
}

/// 一个窗口的剩余百分比, 越界值裁剪到 0-100.
pub fn remaining_percent(window: &QuotaWindow) -> i64 {
    (100.0 - window.used_percent).clamp(0.0, 100.0).round() as i64
}

/// 按固定窗口顺序输出剩余百分比, 缺失的窗口直接省略.
/// `with_percent` 为 false 时输出紧凑形式, 用于托盘的两行标题.
pub fn windows_title(windows: &[QuotaWindow], with_percent: bool) -> Option<String> {
    let values = ordered_windows(windows)
        .map(|window| {
            let percent = remaining_percent(window);
            if with_percent {
                format!("{percent}%")
            } else {
                percent.to_string()
            }
        })
        .collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.join("/"))
}

/// 逐窗口明细, 每行一个窗口, 用于悬浮提示.
pub fn windows_detail(windows: &[QuotaWindow]) -> String {
    window_lines(windows).collect::<Vec<_>>().join("\n")
}

/// 单行明细, 用于托盘悬浮提示.
pub fn windows_inline(windows: &[QuotaWindow]) -> Option<String> {
    let lines = window_lines(windows).collect::<Vec<_>>();
    (!lines.is_empty()).then(|| lines.join(" / "))
}

fn window_lines(windows: &[QuotaWindow]) -> impl Iterator<Item = String> + '_ {
    ordered_windows(windows).map(|window| {
        format!(
            "{} 剩余 {}%",
            window.kind.label(),
            remaining_percent(window)
        )
    })
}

/// 按 5h / 1w / 1month 固定顺序取窗口, 跳过没有有效百分比的分段.
fn ordered_windows(windows: &[QuotaWindow]) -> impl Iterator<Item = &QuotaWindow> {
    QuotaWindowKind::ORDER.into_iter().filter_map(move |kind| {
        windows
            .iter()
            .find(|window| window.kind == kind && window.used_percent.is_finite())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn windows() -> Vec<QuotaWindow> {
        vec![
            QuotaWindow::new(QuotaWindowKind::Monthly, 40.0),
            QuotaWindow::new(QuotaWindowKind::FiveHour, 58.4),
            QuotaWindow::new(QuotaWindowKind::Weekly, 51.0),
        ]
    }

    #[test]
    fn title_keeps_the_window_order_and_both_forms() {
        assert_eq!(
            windows_title(&windows(), true).as_deref(),
            Some("42%/49%/60%")
        );
        assert_eq!(windows_title(&windows(), false).as_deref(), Some("42/49/60"));
    }

    #[test]
    fn title_omits_missing_windows() {
        let only_five_hour = vec![QuotaWindow::new(QuotaWindowKind::FiveHour, 10.0)];
        assert_eq!(windows_title(&only_five_hour, true).as_deref(), Some("90%"));
        assert_eq!(windows_title(&[], true), None);
    }

    #[test]
    fn remaining_percent_clamps_out_of_range_values() {
        assert_eq!(
            remaining_percent(&QuotaWindow::new(QuotaWindowKind::Weekly, 140.0)),
            0
        );
        assert_eq!(
            remaining_percent(&QuotaWindow::new(QuotaWindowKind::Weekly, -5.0)),
            100
        );
    }

    #[test]
    fn codex_quota_snapshot_converts_to_windows() {
        let snapshot = QuotaSnapshot {
            used_5h_percent: Some(12.0),
            used_7d_percent: Some(34.0),
            ..QuotaSnapshot::default()
        };
        assert_eq!(
            codex_windows(&snapshot),
            vec![
                QuotaWindow::new(QuotaWindowKind::FiveHour, 12.0),
                QuotaWindow::new(QuotaWindowKind::Weekly, 34.0),
            ]
        );
        assert!(codex_windows(&QuotaSnapshot::default()).is_empty());
    }

    #[test]
    fn detail_lists_every_window() {
        assert_eq!(
            windows_detail(&windows()),
            "5h 剩余 42%\n1w 剩余 49%\n1mo 剩余 60%"
        );
        assert_eq!(
            windows_inline(&windows()).as_deref(),
            Some("5h 剩余 42% / 1w 剩余 49% / 1mo 剩余 60%")
        );
        assert_eq!(windows_inline(&[]), None);
    }
}

use super::icon;
#[cfg(target_os = "macos")]
use crate::app::platform::tray_title;
#[cfg(target_os = "macos")]
use crate::app::platform::tray_title::TrayTitleView;
#[cfg(target_os = "windows")]
use anyhow::Context;
use eframe::egui;
#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2::MainThreadMarker;
use std::sync::Arc;
#[cfg(target_os = "windows")]
use std::time::Duration;
#[cfg(not(target_os = "windows"))]
use tray_icon::menu::{CheckMenuItem, IsMenuItem, Submenu};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::live::LiveRequestSnapshot;

const OPEN_MENU_ID: &str = "codex-switch-open-window";
const TOGGLE_SERVICE_MENU_ID: &str = "codex-switch-toggle-service";
const QUIT_MENU_ID: &str = "codex-switch-quit";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrayBadgeMetric {
    None,
    #[default]
    Connections,
    TotalTps,
    TotalCps,
    TodayRequests,
    KeepaliveSessions,
}

impl TrayBadgeMetric {
    #[cfg(not(target_os = "windows"))]
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Connections,
        Self::TotalTps,
        Self::TotalCps,
        Self::TodayRequests,
        Self::KeepaliveSessions,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Connections => "connections",
            Self::TotalTps => "total_tps",
            Self::TotalCps => "total_cps",
            Self::TodayRequests => "today_requests",
            Self::KeepaliveSessions => "keepalive_sessions",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "关闭",
            Self::Connections => "连接数",
            Self::TotalTps => "总 TPS",
            Self::TotalCps => "总字符速率",
            Self::TodayRequests => "今日请求数",
            Self::KeepaliveSessions => "缓存保持会话数",
        }
    }

}

impl std::fmt::Display for TrayBadgeMetric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TrayBadgeMetric {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "none" => Ok(Self::None),
            "connections" => Ok(Self::Connections),
            "total_tps" => Ok(Self::TotalTps),
            "total_cps" => Ok(Self::TotalCps),
            "today_requests" => Ok(Self::TodayRequests),
            "keepalive_sessions" => Ok(Self::KeepaliveSessions),
            _ => Err(format!("unknown tray badge metric: {value}")),
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn metric_menu_id(metric: TrayBadgeMetric, secondary: bool) -> MenuId {
    let suffix = if secondary { "-2" } else { "" };
    MenuId::new(format!("codex-switch-badge-{}{}", metric.as_str(), suffix))
}

#[cfg(not(target_os = "windows"))]
fn metric_from_menu_id(id: &str) -> Option<(TrayBadgeMetric, bool)> {
    let (base, secondary) = match id.strip_suffix("-2") {
        Some(base) => (base, true),
        None => (id, false),
    };
    let metric = base.strip_prefix("codex-switch-badge-")?.parse().ok()?;
    Some((metric, secondary))
}

#[cfg(not(target_os = "windows"))]
fn metric_items(
    current: TrayBadgeMetric,
    secondary: bool,
) -> Vec<(TrayBadgeMetric, CheckMenuItem)> {
    TrayBadgeMetric::ALL
        .into_iter()
        .map(|metric| {
            let item = CheckMenuItem::with_id(
                metric_menu_id(metric, secondary),
                metric.label(),
                true,
                metric == current,
                None,
            );
            (metric, item)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrayStats {
    pub server_running: bool,
    pub active_connections: usize,
    pub total_tps: f64,
    pub total_cps: f64,
    pub today_requests: i64,
    pub keepalive_sessions: usize,
}

impl TrayStats {
    pub fn from_live(
        snapshots: &[LiveRequestSnapshot],
        today_requests: i64,
        keepalive_sessions: usize,
        server_running: bool,
    ) -> Self {
        let mut stats = Self {
            server_running,
            active_connections: 0,
            total_tps: 0.0,
            total_cps: 0.0,
            today_requests,
            keepalive_sessions,
        };
        for item in snapshots.iter().filter(|item| item.finished_at.is_none()) {
            stats.active_connections += 1;
            if let Some(rate) = item.output_rate {
                stats.total_tps += rate.estimated_tokens_per_second;
                stats.total_cps += rate.chars_per_second;
            }
        }
        stats
    }

    #[cfg(not(target_os = "windows"))]
    pub fn badge_text(self, metric: TrayBadgeMetric) -> Option<String> {
        match metric {
            TrayBadgeMetric::None => None,
            TrayBadgeMetric::Connections => {
                Some(format_badge_count(self.active_connections as u64))
            }
            TrayBadgeMetric::TotalTps => {
                Some(format_badge_count(self.total_tps.round().max(0.0) as u64))
            }
            TrayBadgeMetric::TotalCps => {
                Some(format_badge_count(self.total_cps.round().max(0.0) as u64))
            }
            TrayBadgeMetric::TodayRequests => {
                Some(format_compact_count(self.today_requests.max(0) as u64))
            }
            TrayBadgeMetric::KeepaliveSessions => {
                Some(format_badge_count(self.keepalive_sessions as u64))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    ShowWindow,
    ToggleService,
    Quit,
    ThemeChanged(bool),
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    SetBadgeMetric(TrayBadgeMetric),
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    SetBadgeMetricSecondary(TrayBadgeMetric),
}

pub struct TrayController {
    tray_icon: TrayIcon,
    toggle_service_item: MenuItem,
    #[cfg(not(target_os = "windows"))]
    first_badge_items: Vec<(TrayBadgeMetric, CheckMenuItem)>,
    #[cfg(not(target_os = "windows"))]
    second_badge_items: Vec<(TrayBadgeMetric, CheckMenuItem)>,
    badge_metric: TrayBadgeMetric,
    secondary_badge_metric: TrayBadgeMetric,
    dark: bool,
    last_tooltip: String,
    #[cfg(target_os = "linux")]
    last_title: String,
    last_stats: Option<TrayStats>,
    #[cfg(target_os = "macos")]
    tray_title_view: Option<Retained<TrayTitleView>>,
}

impl TrayController {
    pub fn new<F>(
        server_running: bool,
        badge_metric: TrayBadgeMetric,
        secondary_badge_metric: TrayBadgeMetric,
        egui_ctx: egui::Context,
        send_command: F,
    ) -> anyhow::Result<Self>
    where
        F: Fn(TrayCommand) + Send + Sync + 'static,
    {
        let send_command: Arc<dyn Fn(TrayCommand) + Send + Sync> = Arc::new(send_command);
        install_handlers(egui_ctx, Arc::clone(&send_command));

        let open_item = MenuItem::with_id(MenuId::new(OPEN_MENU_ID), "打开主界面", true, None);
        let toggle_service_item = MenuItem::with_id(
            MenuId::new(TOGGLE_SERVICE_MENU_ID),
            service_menu_text(server_running),
            true,
            None,
        );
        let quit_item = MenuItem::with_id(MenuId::new(QUIT_MENU_ID), "退出", true, None);
        let first_separator = PredefinedMenuItem::separator();
        let second_separator = PredefinedMenuItem::separator();

        #[cfg(not(target_os = "windows"))]
        let first_badge_items = metric_items(badge_metric, false);
        #[cfg(not(target_os = "windows"))]
        let first_badge_item_refs = first_badge_items
            .iter()
            .map(|(_, item)| item as &dyn IsMenuItem)
            .collect::<Vec<_>>();
        #[cfg(not(target_os = "windows"))]
        let first_title_submenu = Submenu::with_items("标题行 1", true, &first_badge_item_refs)?;

        #[cfg(not(target_os = "windows"))]
        let second_badge_items = metric_items(secondary_badge_metric, true);
        #[cfg(not(target_os = "windows"))]
        let second_badge_item_refs = second_badge_items
            .iter()
            .map(|(_, item)| item as &dyn IsMenuItem)
            .collect::<Vec<_>>();
        #[cfg(not(target_os = "windows"))]
        let second_title_submenu = Submenu::with_items("标题行 2", true, &second_badge_item_refs)?;

        let menu = Menu::new();
        menu.append(&open_item)?;
        menu.append(&first_separator)?;
        menu.append(&toggle_service_item)?;
        menu.append(&second_separator)?;
        #[cfg(not(target_os = "windows"))]
        menu.append(&first_title_submenu)?;
        #[cfg(not(target_os = "windows"))]
        menu.append(&second_title_submenu)?;
        menu.append(&quit_item)?;

        #[cfg(target_os = "windows")]
        let dark = tray_theme_is_dark();
        #[cfg(not(target_os = "windows"))]
        let dark = false;
        let initial_icon = icon::tray_icon_for_theme(dark)?;

        let builder = TrayIconBuilder::new()
            .with_tooltip("Codex Switch")
            .with_icon(initial_icon)
            .with_icon_as_template(cfg!(target_os = "macos"));
        let tray_icon = builder
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(true)
            .build()?;

        spawn_theme_watcher(send_command)?;

        #[cfg(target_os = "macos")]
        let tray_title_view = {
            let mtm =
                MainThreadMarker::new().expect("tray initialization must run on main thread");
            tray_icon
                .ns_status_item()
                .map(|status_item| tray_title::install(&status_item, mtm))
        };

        tracing::info!("system tray initialized");
        Ok(Self {
            tray_icon,
            toggle_service_item,
            #[cfg(not(target_os = "windows"))]
            first_badge_items,
            #[cfg(not(target_os = "windows"))]
            second_badge_items,
            badge_metric,
            secondary_badge_metric,
            dark,
            last_tooltip: String::new(),
            #[cfg(target_os = "linux")]
            last_title: String::new(),
            last_stats: None,
            #[cfg(target_os = "macos")]
            tray_title_view,
        })
    }

    pub fn set_server_running(&self, running: bool) {
        self.toggle_service_item
            .set_text(service_menu_text(running));
    }

    pub fn set_theme(&mut self, dark: bool) -> anyhow::Result<()> {
        if self.dark == dark {
            return Ok(());
        }
        self.dark = dark;
        self.update_icon()
    }

    pub fn set_badge_metric(&mut self, metric: TrayBadgeMetric) {
        self.badge_metric = metric;
        #[cfg(not(target_os = "windows"))]
        {
            for (item_metric, item) in &self.first_badge_items {
                item.set_checked(*item_metric == metric);
            }
            self.refresh_title_if_needed();
        }
    }

    pub fn set_badge_metric_secondary(&mut self, metric: TrayBadgeMetric) {
        self.secondary_badge_metric = metric;
        #[cfg(not(target_os = "windows"))]
        {
            for (item_metric, item) in &self.second_badge_items {
                item.set_checked(*item_metric == metric);
            }
            self.refresh_title_if_needed();
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn refresh_title_if_needed(&mut self) {
        if let Some(stats) = self.last_stats {
            self.update_title(&stats);
        }
    }

    pub fn set_stats(&mut self, stats: TrayStats) {
        self.last_stats = Some(stats);
        let tooltip = format_tooltip(&stats);
        if tooltip != self.last_tooltip {
            self.last_tooltip = tooltip.clone();
            if let Err(err) = self.tray_icon.set_tooltip(Some(tooltip)) {
                tracing::warn!(error = %err, "failed to update tray tooltip");
            }
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        self.update_title(&stats);
    }

    fn update_icon(&self) -> anyhow::Result<()> {
        let tray_icon = icon::tray_icon_for_theme(self.dark)?;
        #[cfg(target_os = "macos")]
        {
            self.tray_icon
                .set_icon_with_as_template(Some(tray_icon), true)?;
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.tray_icon.set_icon(Some(tray_icon))?;
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn update_title(&mut self, stats: &TrayStats) {
        let first = metric_line(stats, self.badge_metric);
        let second = metric_line(stats, self.secondary_badge_metric);
        if let Some(view) = &self.tray_title_view {
            view.update(first.as_deref(), second.as_deref());
        }
    }

    #[cfg(target_os = "linux")]
    fn update_title(&mut self, stats: &TrayStats) {
        let title =
            format_title(stats, self.badge_metric, self.secondary_badge_metric).unwrap_or_default();
        if title != self.last_title {
            self.last_title = title.clone();
            self.tray_icon.set_title(Some(title));
        }
    }
}

#[cfg(target_os = "windows")]
fn tray_theme_is_dark() -> bool {
    use windows_registry::CURRENT_USER;

    let key = CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .ok();
    let Some(key) = key else {
        return false;
    };
    match key.get_u32("SystemUsesLightTheme") {
        Ok(0) => true,
        Ok(_) => false,
        Err(err) => {
            tracing::debug!(error = %err, "failed to read system tray theme");
            false
        }
    }
}

#[cfg(target_os = "windows")]
fn spawn_theme_watcher(send_command: Arc<dyn Fn(TrayCommand) + Send + Sync>) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("tray-theme-watcher".to_string())
        .spawn(move || {
            let mut current = tray_theme_is_dark();
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let next = tray_theme_is_dark();
                if next != current {
                    current = next;
                    send_command(TrayCommand::ThemeChanged(next));
                }
            }
        })
        .context("failed to spawn tray theme watcher")?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn spawn_theme_watcher(
    _send_command: Arc<dyn Fn(TrayCommand) + Send + Sync>,
) -> anyhow::Result<()> {
    Ok(())
}

fn install_handlers(egui_ctx: egui::Context, send_command: Arc<dyn Fn(TrayCommand) + Send + Sync>) {
    let menu_ctx = egui_ctx.clone();
    let menu_sender = Arc::clone(&send_command);
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let command = match event.id.as_ref() {
            OPEN_MENU_ID => Some(TrayCommand::ShowWindow),
            TOGGLE_SERVICE_MENU_ID => Some(TrayCommand::ToggleService),
            QUIT_MENU_ID => Some(TrayCommand::Quit),
            #[cfg(not(target_os = "windows"))]
            id => metric_from_menu_id(id).map(|(metric, secondary)| {
                if secondary {
                    TrayCommand::SetBadgeMetricSecondary(metric)
                } else {
                    TrayCommand::SetBadgeMetric(metric)
                }
            }),
            #[cfg(target_os = "windows")]
            _ => None,
        };
        if let Some(command) = command {
            menu_sender(command);
            menu_ctx.request_repaint();
        }
    }));

    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            send_command(TrayCommand::ShowWindow);
            egui_ctx.request_repaint();
        }
    }));
}

fn service_menu_text(running: bool) -> &'static str {
    if running {
        "关闭服务"
    } else {
        "启动服务"
    }
}

fn format_tooltip(stats: &TrayStats) -> String {
    let service = if stats.server_running {
        "运行中"
    } else {
        "已停止"
    };
    format!(
        "Codex Switch\n服务: {service}\n活跃连接: {}\n总 TPS: {}\n总字符速率: {}\n今日请求: {}\n缓存保持会话: {}",
        stats.active_connections,
        format_tps(stats.total_tps),
        format_cps(stats.total_cps),
        stats.today_requests,
        stats.keepalive_sessions,
    )
}

fn format_tps(value: f64) -> String {
    if value >= 100.0 {
        format!("~{value:.0}")
    } else {
        format!("~{value:.1}")
    }
}

fn format_cps(value: f64) -> String {
    format!("~{}", value.round())
}

#[cfg(not(target_os = "windows"))]
fn format_title(
    stats: &TrayStats,
    first: TrayBadgeMetric,
    second: TrayBadgeMetric,
) -> Option<String> {
    let lines = [first, second]
        .into_iter()
        .filter_map(|metric| metric_line(stats, metric))
        .collect::<Vec<_>>();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(not(target_os = "windows"))]
fn metric_line(stats: &TrayStats, metric: TrayBadgeMetric) -> Option<String> {
    let text = stats.badge_text(metric)?;
    Some(format!("{text} {}", metric_unit(metric)))
}

#[cfg(not(target_os = "windows"))]
fn metric_unit(metric: TrayBadgeMetric) -> &'static str {
    match metric {
        TrayBadgeMetric::Connections => "连接",
        TrayBadgeMetric::TotalTps => "tps",
        TrayBadgeMetric::TotalCps => "cps",
        TrayBadgeMetric::TodayRequests => "请求",
        TrayBadgeMetric::KeepaliveSessions => "会话",
        TrayBadgeMetric::None => "",
    }
}

#[cfg(not(target_os = "windows"))]
fn format_badge_count(value: u64) -> String {
    value.to_string()
}

#[cfg(not(target_os = "windows"))]
fn format_compact_count(value: u64) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    let (unit, scale) = if value < 1_000_000 {
        ("K", 1_000.0)
    } else if value < 1_000_000_000 {
        ("M", 1_000_000.0)
    } else {
        ("B", 1_000_000_000.0)
    };
    let number = value as f64 / scale;
    let digits = if number < 10.0 { 1 } else { 0 };
    format!("{number:.digits$}{unit}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::{LiveOutputRate, LiveResponseState};
    use chrono::Utc;

    fn snapshot(id: &str, finished: bool, rate: Option<LiveOutputRate>) -> LiveRequestSnapshot {
        LiveRequestSnapshot {
            id: id.to_string(),
            upstream_name: None,
            endpoint: "/responses".to_string(),
            model: None,
            target_model: None,
            reasoning_effort: None,
            response_state: LiveResponseState::Streaming,
            tail: String::new(),
            tail_start_char_index: 0,
            tail_end_char_index: 0,
            hover_output: String::new(),
            output_rate: rate,
            started_at: Utc::now(),
            finished_at: finished.then(Utc::now),
            terminating: false,
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn badge_metric_roundtrips_through_text() {
        for metric in TrayBadgeMetric::ALL {
            assert_eq!(metric.as_str().parse::<TrayBadgeMetric>().unwrap(), metric);
            assert_eq!(metric.to_string().as_str(), metric.as_str());
        }
        assert!("unknown".parse::<TrayBadgeMetric>().is_err());
    }

    #[test]
    fn tray_stats_aggregates_active_connection_rates() {
        let rate_a = LiveOutputRate {
            estimated_tokens_per_second: 12.5,
            chars_per_second: 40.0,
        };
        let rate_b = LiveOutputRate {
            estimated_tokens_per_second: 3.0,
            chars_per_second: 10.0,
        };
        let snapshots = vec![
            snapshot("a", false, Some(rate_a)),
            snapshot("b", false, Some(rate_b)),
            snapshot("c", false, None),
            snapshot(
                "d",
                true,
                Some(LiveOutputRate {
                    estimated_tokens_per_second: 99.0,
                    chars_per_second: 999.0,
                }),
            ),
        ];

        let stats = TrayStats::from_live(&snapshots, 42, 3, true);

        assert_eq!(stats.active_connections, 3);
        assert!((stats.total_tps - 15.5).abs() < f64::EPSILON);
        assert!((stats.total_cps - 50.0).abs() < f64::EPSILON);
        assert_eq!(stats.today_requests, 42);
        assert_eq!(stats.keepalive_sessions, 3);
        assert!(stats.server_running);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn badge_text_uses_metric_specific_formatting() {
        let stats = TrayStats {
            server_running: true,
            active_connections: 0,
            total_tps: 12.4,
            total_cps: 45.6,
            today_requests: 1_234,
            keepalive_sessions: 5,
        };

        assert_eq!(stats.badge_text(TrayBadgeMetric::None), None);
        assert_eq!(
            stats.badge_text(TrayBadgeMetric::Connections).as_deref(),
            Some("0")
        );
        assert_eq!(
            stats.badge_text(TrayBadgeMetric::TotalTps).as_deref(),
            Some("12")
        );
        assert_eq!(
            stats.badge_text(TrayBadgeMetric::TotalCps).as_deref(),
            Some("46")
        );
        assert_eq!(
            stats.badge_text(TrayBadgeMetric::TodayRequests).as_deref(),
            Some("1.2K")
        );
        assert_eq!(
            stats
                .badge_text(TrayBadgeMetric::KeepaliveSessions)
                .as_deref(),
            Some("5")
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn title_supports_zero_one_or_two_metrics() {
        let stats = TrayStats {
            server_running: true,
            active_connections: 3,
            total_tps: 12.4,
            total_cps: 45.6,
            today_requests: 1_234,
            keepalive_sessions: 5,
        };

        assert_eq!(format_title(&stats, TrayBadgeMetric::None, TrayBadgeMetric::None), None);
        assert_eq!(
            format_title(&stats, TrayBadgeMetric::Connections, TrayBadgeMetric::None)
                .as_deref(),
            Some("3 连接")
        );
        assert_eq!(
            format_title(&stats, TrayBadgeMetric::None, TrayBadgeMetric::KeepaliveSessions)
                .as_deref(),
            Some("5 会话")
        );
        assert_eq!(
            format_title(
                &stats,
                TrayBadgeMetric::TotalTps,
                TrayBadgeMetric::TotalCps
            )
            .as_deref(),
            Some("12 tps\n46 cps")
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn metric_menu_ids_roundtrip() {
        for metric in TrayBadgeMetric::ALL {
            for secondary in [false, true] {
                let id = metric_menu_id(metric, secondary);
                assert_eq!(metric_from_menu_id(id.as_ref()), Some((metric, secondary)));
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn badge_count_caps_at_ninety_nine() {
        assert_eq!(format_badge_count(0), "0");
        assert_eq!(format_badge_count(99), "99");
        assert_eq!(format_badge_count(100), "100");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn compact_count_uses_short_units() {
        assert_eq!(format_compact_count(999), "999");
        assert_eq!(format_compact_count(1_234), "1.2K");
        assert_eq!(format_compact_count(12_345), "12K");
        assert_eq!(format_compact_count(1_234_567), "1.2M");
    }
}

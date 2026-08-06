use super::icon;
#[cfg(target_os = "windows")]
use anyhow::Context;
use eframe::egui;
use std::sync::Arc;
#[cfg(target_os = "windows")]
use std::time::Duration;
use tray_icon::menu::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::live::LiveRequestSnapshot;

const OPEN_MENU_ID: &str = "codex-switch-open-window";
const TOGGLE_SERVICE_MENU_ID: &str = "codex-switch-toggle-service";
const QUIT_MENU_ID: &str = "codex-switch-quit";
const BADGE_NONE_MENU_ID: &str = "codex-switch-badge-none";
const BADGE_CONNECTIONS_MENU_ID: &str = "codex-switch-badge-connections";
const BADGE_TOTAL_TPS_MENU_ID: &str = "codex-switch-badge-total-tps";
const BADGE_TOTAL_CPS_MENU_ID: &str = "codex-switch-badge-total-cps";
const BADGE_TODAY_REQUESTS_MENU_ID: &str = "codex-switch-badge-today-requests";
const BADGE_KEEPALIVE_MENU_ID: &str = "codex-switch-badge-keepalive-sessions";

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

    fn menu_id(self) -> MenuId {
        MenuId::new(match self {
            Self::None => BADGE_NONE_MENU_ID,
            Self::Connections => BADGE_CONNECTIONS_MENU_ID,
            Self::TotalTps => BADGE_TOTAL_TPS_MENU_ID,
            Self::TotalCps => BADGE_TOTAL_CPS_MENU_ID,
            Self::TodayRequests => BADGE_TODAY_REQUESTS_MENU_ID,
            Self::KeepaliveSessions => BADGE_KEEPALIVE_MENU_ID,
        })
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
        for item in snapshots
            .iter()
            .filter(|item| item.finished_at.is_none())
        {
            stats.active_connections += 1;
            if let Some(rate) = item.output_rate {
                stats.total_tps += rate.estimated_tokens_per_second;
                stats.total_cps += rate.chars_per_second;
            }
        }
        stats
    }

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
    SetBadgeMetric(TrayBadgeMetric),
}

pub struct TrayController {
    tray_icon: TrayIcon,
    toggle_service_item: MenuItem,
    badge_items: Vec<(TrayBadgeMetric, CheckMenuItem)>,
    badge_metric: TrayBadgeMetric,
    dark: bool,
    last_tooltip: String,
    #[cfg(target_os = "linux")]
    last_title: Option<String>,
    last_badge_text: Option<String>,
    last_stats: Option<TrayStats>,
}

impl TrayController {
    pub fn new<F>(
        server_running: bool,
        badge_metric: TrayBadgeMetric,
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

        let mut badge_items = Vec::with_capacity(TrayBadgeMetric::ALL.len());
        for metric in TrayBadgeMetric::ALL {
            badge_items.push((
                metric,
                CheckMenuItem::with_id(
                metric.menu_id(),
                metric.label(),
                true,
                metric == badge_metric,
                None,
                ),
            ));
        }
        let badge_item_refs = badge_items
            .iter()
            .map(|(_, item)| item as &dyn IsMenuItem)
            .collect::<Vec<_>>();
        let badge_submenu = Submenu::with_items("角标显示", true, &badge_item_refs)?;

        let menu = Menu::new();
        menu.append(&open_item)?;
        menu.append(&first_separator)?;
        menu.append(&toggle_service_item)?;
        menu.append(&second_separator)?;
        menu.append(&badge_submenu)?;
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

        tracing::info!("system tray initialized");
        Ok(Self {
            tray_icon,
            toggle_service_item,
            badge_items,
            badge_metric,
            dark,
            last_tooltip: String::new(),
            #[cfg(target_os = "linux")]
            last_title: None,
            last_badge_text: None,
            last_stats: None,
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
        for (item_metric, item) in &self.badge_items {
            item.set_checked(*item_metric == metric);
        }
        if let Some(stats) = self.last_stats {
            #[cfg(target_os = "linux")]
            self.update_title(&stats);
            let badge_text = stats.badge_text(metric);
            if badge_text != self.last_badge_text {
                self.last_badge_text = badge_text.clone();
                if let Err(err) = self.update_icon() {
                    tracing::warn!(error = %err, "failed to update tray icon badge");
                }
            }
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
        #[cfg(target_os = "linux")]
        self.update_title(&stats);
        let badge_text = stats.badge_text(self.badge_metric);
        if badge_text != self.last_badge_text {
            self.last_badge_text = badge_text.clone();
            if let Err(err) = self.update_icon() {
                tracing::warn!(error = %err, "failed to update tray icon badge");
            }
        }
    }

    fn update_icon(&self) -> anyhow::Result<()> {
        let tray_icon = icon::tray_icon_with_badge(
            self.dark,
            self.last_badge_text.as_deref(),
            cfg!(target_os = "macos"),
        )?;
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

    #[cfg(target_os = "linux")]
    fn update_title(&mut self, stats: &TrayStats) {
        let title = format_title(stats, self.badge_metric);
        if title != self.last_title {
            self.last_title = title.clone();
            self.tray_icon.set_title(title);
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
            BADGE_NONE_MENU_ID => Some(TrayCommand::SetBadgeMetric(TrayBadgeMetric::None)),
            BADGE_CONNECTIONS_MENU_ID => {
                Some(TrayCommand::SetBadgeMetric(TrayBadgeMetric::Connections))
            }
            BADGE_TOTAL_TPS_MENU_ID => {
                Some(TrayCommand::SetBadgeMetric(TrayBadgeMetric::TotalTps))
            }
            BADGE_TOTAL_CPS_MENU_ID => {
                Some(TrayCommand::SetBadgeMetric(TrayBadgeMetric::TotalCps))
            }
            BADGE_TODAY_REQUESTS_MENU_ID => {
                Some(TrayCommand::SetBadgeMetric(TrayBadgeMetric::TodayRequests))
            }
            BADGE_KEEPALIVE_MENU_ID => {
                Some(TrayCommand::SetBadgeMetric(TrayBadgeMetric::KeepaliveSessions))
            }
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
    let service = if stats.server_running { "运行中" } else { "已停止" };
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

#[cfg(target_os = "linux")]
fn format_title(stats: &TrayStats, metric: TrayBadgeMetric) -> Option<String> {
    let text = stats.badge_text(metric)?;
    let unit = match metric {
        TrayBadgeMetric::Connections => "连接",
        TrayBadgeMetric::TotalTps => "tps",
        TrayBadgeMetric::TotalCps => "cps",
        TrayBadgeMetric::TodayRequests => "请求",
        TrayBadgeMetric::KeepaliveSessions => "会话",
        TrayBadgeMetric::None => return None,
    };
    Some(format!("{text} {unit}"))
}

fn format_badge_count(value: u64) -> String {
    if value > 99 {
        "99+".to_string()
    } else {
        value.to_string()
    }
}

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

    #[test]
    fn badge_metric_roundtrips_through_text() {
        for metric in TrayBadgeMetric::ALL {
            assert_eq!(
                metric.as_str().parse::<TrayBadgeMetric>().unwrap(),
                metric
            );
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
            stats
                .badge_text(TrayBadgeMetric::TodayRequests)
                .as_deref(),
            Some("1.2K")
        );
        assert_eq!(
            stats
                .badge_text(TrayBadgeMetric::KeepaliveSessions)
                .as_deref(),
            Some("5")
        );
    }

    #[test]
    fn badge_count_caps_at_ninety_nine() {
        assert_eq!(format_badge_count(0), "0");
        assert_eq!(format_badge_count(99), "99");
        assert_eq!(format_badge_count(100), "99+");
    }

    #[test]
    fn compact_count_uses_short_units() {
        assert_eq!(format_compact_count(999), "999");
        assert_eq!(format_compact_count(1_234), "1.2K");
        assert_eq!(format_compact_count(12_345), "12K");
        assert_eq!(format_compact_count(1_234_567), "1.2M");
    }
}

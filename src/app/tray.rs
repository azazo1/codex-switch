use super::icon;
#[cfg(target_os = "windows")]
use anyhow::Context;
use eframe::egui;
use std::sync::Arc;
#[cfg(target_os = "windows")]
use std::time::Duration;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const OPEN_MENU_ID: &str = "codex-switch-open-window";
const TOGGLE_SERVICE_MENU_ID: &str = "codex-switch-toggle-service";
const QUIT_MENU_ID: &str = "codex-switch-quit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    ShowWindow,
    ToggleService,
    Quit,
    ThemeChanged(bool),
}

pub struct TrayController {
    tray_icon: TrayIcon,
    toggle_service_item: MenuItem,
}

impl TrayController {
    pub fn new<F>(
        server_running: bool,
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

        let menu = Menu::new();
        menu.append(&open_item)?;
        menu.append(&first_separator)?;
        menu.append(&toggle_service_item)?;
        menu.append(&second_separator)?;
        menu.append(&quit_item)?;

        #[cfg(target_os = "windows")]
        let initial_icon = icon::tray_icon_for_theme(tray_theme_is_dark())?;
        #[cfg(not(target_os = "windows"))]
        let initial_icon = icon::tray_icon_for_theme(false)?;

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
        })
    }

    pub fn set_server_running(&self, running: bool) {
        self.toggle_service_item
            .set_text(service_menu_text(running));
    }

    pub fn set_theme(&mut self, dark: bool) -> anyhow::Result<()> {
        self.tray_icon
            .set_icon(Some(icon::tray_icon_for_theme(dark)?))?;
        Ok(())
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

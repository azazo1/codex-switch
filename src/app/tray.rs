use super::icon;
use eframe::egui;
use std::sync::Arc;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

const OPEN_MENU_ID: &str = "codex-switch-open-window";
const TOGGLE_SERVICE_MENU_ID: &str = "codex-switch-toggle-service";
const QUIT_MENU_ID: &str = "codex-switch-quit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    ShowWindow,
    ToggleService,
    Quit,
}

pub struct TrayController {
    _tray_icon: TrayIcon,
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
        install_handlers(egui_ctx, send_command);

        let open_item =
            MenuItem::with_id(MenuId::new(OPEN_MENU_ID), "打开主界面", true, None);
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

        let tray_icon = TrayIconBuilder::new()
            .with_tooltip("Codex Switch")
            .with_icon(icon::tray_icon()?)
            .with_icon_as_template(true)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(true)
            .build()?;

        tracing::info!("system tray initialized");
        Ok(Self {
            _tray_icon: tray_icon,
            toggle_service_item,
        })
    }

    pub fn set_server_running(&self, running: bool) {
        self.toggle_service_item
            .set_text(service_menu_text(running));
    }
}

fn install_handlers(
    egui_ctx: egui::Context,
    send_command: Arc<dyn Fn(TrayCommand) + Send + Sync>,
) {
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

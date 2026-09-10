use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{MainThreadMarker, sel};
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSMenu, NSMenuItem,
    NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::NSString;

/// ANSI F 的硬件键码, 对应 macOS 的 Globe/Fn+F 全屏快捷键.
const KEY_CODE_ANSI_F: u16 = 0x03;

static MENU_INSTALLED: AtomicBool = AtomicBool::new(false);
static MONITOR_INSTALLED: AtomicBool = AtomicBool::new(false);

/// 安装 macOS 窗口级快捷键, 可重复调用.
///
/// winit 默认菜单只有应用项, 没有 Window 菜单, 因此 `Ctrl+Cmd+F` 不会进入原生全屏.
/// 这里补上 Close / Minimize / Zoom / Enter Full Screen, 并为 `Fn+F` 加本地按键监听.
pub fn install() {
    enable_native_fullscreen_on_windows();
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if !MENU_INSTALLED.load(Ordering::Relaxed) && install_window_menu(mtm) {
        MENU_INSTALLED.store(true, Ordering::Relaxed);
    }
    if !MONITOR_INSTALLED.swap(true, Ordering::Relaxed) {
        install_fn_f_monitor();
    }
}

fn install_window_menu(mtm: MainThreadMarker) -> bool {
    let app = NSApplication::sharedApplication(mtm);
    let Some(menubar) = app.mainMenu() else {
        return false;
    };
    if menubar
        .itemWithTitle(&NSString::from_str("Window"))
        .is_some()
    {
        return true;
    }

    let window_item = NSMenuItem::new(mtm);
    window_item.setTitle(&NSString::from_str("Window"));
    let window_menu = NSMenu::new(mtm);
    window_menu.setTitle(&NSString::from_str("Window"));
    window_item.setSubmenu(Some(&window_menu));

    window_menu.addItem(&menu_item(
        mtm,
        "Close",
        Some(sel!(performClose:)),
        Some("w"),
        None,
    ));
    window_menu.addItem(&NSMenuItem::separatorItem(mtm));
    window_menu.addItem(&menu_item(
        mtm,
        "Minimize",
        Some(sel!(performMiniaturize:)),
        Some("m"),
        None,
    ));
    window_menu.addItem(&menu_item(
        mtm,
        "Zoom",
        Some(sel!(performZoom:)),
        None,
        None,
    ));
    window_menu.addItem(&NSMenuItem::separatorItem(mtm));
    window_menu.addItem(&menu_item(
        mtm,
        "Enter Full Screen",
        Some(sel!(toggleFullScreen:)),
        Some("f"),
        Some(NSEventModifierFlags::Control | NSEventModifierFlags::Command),
    ));

    menubar.addItem(&window_item);
    tracing::info!("installed macOS Window menu shortcuts");
    true
}

fn menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<Sel>,
    key: Option<&str>,
    modifiers: Option<NSEventModifierFlags>,
) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key.unwrap_or("")),
        )
    };
    if let Some(modifiers) = modifiers {
        item.setKeyEquivalentModifierMask(modifiers);
    }
    item
}

fn install_fn_f_monitor() {
    let block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        let event_ref = unsafe { event.as_ref() };
        if is_fn_plus_f(event_ref) {
            toggle_key_window_fullscreen();
            std::ptr::null_mut()
        } else {
            event.as_ptr()
        }
    });
    let Some(monitor) = (unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block)
    }) else {
        tracing::warn!("failed to install Fn+F fullscreen monitor");
        return;
    };
    // 监听器和 block 需要活到进程结束.
    std::mem::forget(block);
    std::mem::forget(monitor);
    tracing::info!("installed Fn+F native fullscreen shortcut");
}

fn is_fn_plus_f(event: &NSEvent) -> bool {
    if event.isARepeat() || event.keyCode() != KEY_CODE_ANSI_F {
        return false;
    }
    let flags = event.modifierFlags();
    let interesting = NSEventModifierFlags::Function
        | NSEventModifierFlags::Command
        | NSEventModifierFlags::Control
        | NSEventModifierFlags::Option
        | NSEventModifierFlags::Shift;
    flags.contains(NSEventModifierFlags::Function)
        && (flags & interesting) == NSEventModifierFlags::Function
}

fn toggle_key_window_fullscreen() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(window) = app.keyWindow().or_else(|| app.mainWindow()) else {
        return;
    };
    if !window.styleMask().contains(NSWindowStyleMask::Titled) {
        return;
    }
    enable_native_fullscreen(&window);
    tracing::info!("toggling native fullscreen via Fn+F");
    window.toggleFullScreen(None);
}

fn enable_native_fullscreen_on_windows() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    for window in app.windows().to_vec() {
        if window.styleMask().contains(NSWindowStyleMask::Titled) {
            enable_native_fullscreen(&window);
        }
    }
}

fn enable_native_fullscreen(window: &NSWindow) {
    let behavior = window.collectionBehavior();
    if behavior.contains(NSWindowCollectionBehavior::FullScreenPrimary) {
        return;
    }
    window.setCollectionBehavior(behavior | NSWindowCollectionBehavior::FullScreenPrimary);
    tracing::info!("enabled NSWindowCollectionBehaviorFullScreenPrimary");
}

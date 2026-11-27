//! System tray icon and close-to-tray lifecycle helpers.
//!
//! Keeps OmniDoppelganger resident in the menu bar / notification area when the main window is closed,
//! so background scheduled tasks (`功课`) wake up reliably without cluttering the desktop.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime};

pub const TRAY_ID: &str = "main-tray";
pub const OPEN_SCHEDULES_EVENT: &str = "open-schedules";

const MENU_SHOW: &str = "tray-show";
const MENU_SCHEDULES: &str = "tray-schedules";
const MENU_QUIT: &str = "tray-quit";

static QUITTING: AtomicBool = AtomicBool::new(false);

pub fn is_quitting() -> bool {
    QUITTING.load(Ordering::SeqCst)
}

pub fn mark_quitting() {
    QUITTING.store(true, Ordering::SeqCst);
}

/// Restore and focus the main desktop window from the system tray or Dock.
pub fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Build the system tray icon with Show / Schedules / Quit menu items and left-click restore.
pub fn setup_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, MENU_SHOW, "显示主窗口", true, None::<&str>)?;
    let schedules = MenuItem::with_id(app, MENU_SCHEDULES, "查看功课任务", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "完全退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &schedules, &sep, &quit])?;

    #[cfg(target_os = "macos")]
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray-template.png"))?;
    #[cfg(not(target_os = "macos"))]
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("OmniDoppelganger — 后台驻留（功课守护中）")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            MENU_SHOW => show_main_window(app),
            MENU_SCHEDULES => {
                show_main_window(app);
                let _ = app.emit(OPEN_SCHEDULES_EVENT, ());
            }
            MENU_QUIT => {
                mark_quitting();
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

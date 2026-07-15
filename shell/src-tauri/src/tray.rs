//! System tray: closing the main window hides it instead of quitting (a
//! call must survive window close), with a tray menu to bring it back or
//! quit for real.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Manager};

const TRAY_ICON_BYTES: &[u8] = include_bytes!("../icons/tray-icon@2x.png");

fn show_and_focus(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub fn setup(app: &App) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show_item, &separator, &quit_item])?;

    let icon = tauri::image::Image::from_bytes(TRAY_ICON_BYTES)?;

    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Centinelo Phone")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_and_focus(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_and_focus(tray.app_handle());
            }
        })
        .build(app)?;

    // Close = hide to tray, not quit - an active call must survive it.
    if let Some(window) = app.get_webview_window("main") {
        let window_clone = window.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window_clone.hide();
            }
        });
    }

    Ok(())
}

//! System tray: closing the main window hides it instead of quitting (a
//! call must survive window close), with a tray menu to bring it back or
//! quit for real.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Manager};

use crate::premium::PremiumHandle;

const TRAY_ICON_BYTES: &[u8] = include_bytes!("../icons/tray-icon@2x.png");

/// Also used by bridge.rs and deeplink.rs - any external dial trigger
/// (click-to-call, centinelo:// or tel: link) surfaces the app the same way
/// v1's `showMainWindow()` did (src/main/main.js).
pub(crate) fn show_and_focus(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub fn setup(app: &App, premium: &PremiumHandle) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;

    // "Console…" only ever appears on the menu at all when the premium
    // license gate is cleared (see console::is_unlocked's doc for why
    // that's "Available or NotImplemented", not a literal Available
    // check) - decided once here at startup, not toggled at runtime,
    // because v0 has no way for a license to change mid-session (no
    // activate-license flow yet; see premium.rs's "Never fails startup"
    // doc) and tray-icon's MenuItem has no visibility toggle on all
    // platforms, only enabled/disabled - so "absent" (task e2e scenario
    // (a): "console entry absent") has to mean "never added", not
    // "added but disabled".
    let menu = if crate::console::is_unlocked(premium) {
        let console_item = MenuItem::with_id(app, "console", "Console…", true, None::<&str>)?;
        let console_separator = PredefinedMenuItem::separator(app)?;
        Menu::with_items(
            app,
            &[
                &show_item,
                &console_separator,
                &console_item,
                &separator,
                &quit_item,
            ],
        )?
    } else {
        Menu::with_items(app, &[&show_item, &separator, &quit_item])?
    };

    let icon = tauri::image::Image::from_bytes(TRAY_ICON_BYTES)?;

    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Centinelo Phone")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_and_focus(app),
            "console" => {
                if let Err(e) = crate::console::open_or_focus(app) {
                    log::warn!("tray: open console failed: {e}");
                }
            }
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

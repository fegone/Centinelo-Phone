//! System tray: closing the main window hides it instead of quitting (a
//! call must survive window close), with a tray menu to bring it back or
//! quit for real.

use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Emitter, Manager, Wry};

use std::sync::Arc;

use crate::premium::PremiumHandle;
use crate::settings::SettingsStore;
use crate::sidecar::SidecarHandle;

const TRAY_ICON_BYTES: &[u8] = include_bytes!("../icons/tray-icon@2x.png");

// ---- availability / auto-answer tray toggles (shell task) -----------------
//
// Plain hardcoded English labels, deliberately NOT run through ui/js/i18n.js
// - this is a native OS menu (muda/tray-icon), not the webview, and every
// existing tray item here ("Show", "Console…", "Quit") is already hardcoded
// English with no i18n plumbing at all; matching that existing convention
// beats introducing a one-off localization path for two new items. The
// webview-side titlebar indicator (app.js) IS localized via
// availability.titlebarAvailableTitle/titlebarDndTitle - that's the surface
// the shell task's i18n requirement targets.
//
// Kept as a managed `AvailabilityTrayHandles` (not just built once and
// forgotten) so a settings-pane change to the same preference - which
// doesn't go through this menu's own click handler - can still push the
// tray's checkmark back in sync via `sync_availability_menu` (called from
// `commands::set_available`/`set_auto_answer`). Both `CheckMenuItem`s are
// `Clone` (Arc-wrapped, tauri::menu's own `gen_wrappers!` macro), so cloning
// into the click closure and into this struct is cheap and doesn't need any
// extra locking - `set_checked` itself hops to the main thread internally.
#[derive(Clone)]
pub struct AvailabilityTrayHandles {
    available_item: CheckMenuItem<Wry>,
    auto_answer_item: CheckMenuItem<Wry>,
}

impl AvailabilityTrayHandles {
    /// Pushes the CURRENT persisted values onto both checkmarks. Called
    /// after every settings change to this preference, whichever surface
    /// (tray itself, titlebar button, Settings pane) made it - so all three
    /// controls can never visibly disagree with each other for longer than
    /// one round-trip. `set_checked` failing (e.g. the tray icon has been
    /// torn down) is swallowed - a stale checkmark on a tray that's about
    /// to disappear anyway isn't worth surfacing as an error to the caller.
    fn sync(&self, available: bool, auto_answer: bool) {
        let _ = self.available_item.set_checked(available);
        let _ = self.auto_answer_item.set_checked(auto_answer);
    }
}

/// Best-effort push of the current availability/auto-answer values onto the
/// tray's own checkmarks, AND onto the webview via an `availability-changed`
/// event - the SINGLE common point every route that changes this preference
/// funnels through (`commands::set_available`/`set_auto_answer` AND this
/// file's own `toggle_available`/`toggle_auto_answer`), so all 3 surfaces
/// (tray checkmarks, titlebar dot, Settings pane bool rows) can never
/// disagree with each other for longer than one round-trip.
///
/// 4R RELIABILITY (2026-07-18 re-review): before this fix, a change made
/// from the TRAY only updated the tray's own checkmarks - there was no
/// `emit` reaching the webview at all, so app.js had nothing to `listen`
/// for. The titlebar dot and Settings pane bool rows kept showing whatever
/// `state.availability` happened to be from the last boot/command-invoke
/// round-trip, silently diverging from the engine's real, already-changed
/// behavior (e.g. tray -> Do Not Disturb really rejects calls with 486,
/// while the titlebar dot kept showing "Available" until the next reload).
/// app.js's `listen("availability-changed", ...)` (attachTauriListeners)
/// is the other half of this fix.
///
/// A no-op tray sync if the tray hasn't been built yet (or at all, e.g.
/// under `cfg(test)`/e2e harnesses that never call `tray::setup`),
/// matching this file's existing "never fails app startup" tolerance for
/// anything tray-related - the `emit` still fires regardless (harmless,
/// swallowed by Tauri, if no window is listening yet).
pub fn sync_availability_menu(app: &AppHandle, available: bool, auto_answer: bool) {
    if let Some(handles) = app.try_state::<AvailabilityTrayHandles>() {
        handles.sync(available, auto_answer);
    }
    let _ = app.emit(
        "availability-changed",
        serde_json::json!({"available": available, "auto_answer": auto_answer}),
    );
}

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

/// Flips `availability.available`, reapplies the effective answer mode, and
/// pushes both checkmarks back in sync - the tray-menu-click counterpart to
/// `commands::set_available` (that command handles the same flow for the
/// titlebar button/Settings pane, this handles the tray's own click).
/// Best-effort: a failed persist/reapply is logged, not panicked - same
/// tolerance every other tray click handler here already has (`console`'s
/// `open_or_focus` failure is likewise just a `log::warn!`).
fn toggle_available(app: &AppHandle) {
    let settings = app.state::<Arc<SettingsStore>>();
    let current = settings.snapshot().availability;
    let new_available = !current.available;
    if let Err(e) = settings.update_available(new_available) {
        log::warn!("tray: update_available failed: {e}");
        return;
    }
    if let Err(e) = app.state::<SidecarHandle>().apply_answer_mode() {
        log::warn!("tray: apply_answer_mode after toggling available failed: {e}");
    }
    sync_availability_menu(app, new_available, current.auto_answer);
}

/// Tray-menu-click counterpart to `commands::set_auto_answer` - see
/// `toggle_available`'s doc for the shared shape.
fn toggle_auto_answer(app: &AppHandle) {
    let settings = app.state::<Arc<SettingsStore>>();
    let current = settings.snapshot().availability;
    let new_auto_answer = !current.auto_answer;
    if let Err(e) = settings.update_auto_answer(new_auto_answer) {
        log::warn!("tray: update_auto_answer failed: {e}");
        return;
    }
    if let Err(e) = app.state::<SidecarHandle>().apply_answer_mode() {
        log::warn!("tray: apply_answer_mode after toggling auto_answer failed: {e}");
    }
    sync_availability_menu(app, current.available, new_auto_answer);
}

pub fn setup(app: &App, premium: &PremiumHandle) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let avail_separator = PredefinedMenuItem::separator(app)?;

    // Availability/auto-answer toggles (shell task) - initial checked state
    // read from whatever's already persisted (a relaunch must not silently
    // reset either preference back to its default). Registered as managed
    // state right after construction so `sync_availability_menu` (called by
    // commands::set_available/set_auto_answer) can reach these checkmarks
    // even though this function has no return value to hand them back
    // through.
    let initial_availability = app.state::<Arc<SettingsStore>>().snapshot().availability;
    let available_item = CheckMenuItem::with_id(
        app,
        "toggle_available",
        "Available",
        true,
        initial_availability.available,
        None::<&str>,
    )?;
    let auto_answer_item = CheckMenuItem::with_id(
        app,
        "toggle_auto_answer",
        "Auto-answer",
        true,
        initial_availability.auto_answer,
        None::<&str>,
    )?;
    app.manage(AvailabilityTrayHandles {
        available_item: available_item.clone(),
        auto_answer_item: auto_answer_item.clone(),
    });

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
    let mut items: Vec<&dyn IsMenuItem<Wry>> = vec![&show_item, &avail_separator, &available_item, &auto_answer_item, &separator];
    let console_item;
    let console_separator;
    if crate::console::is_unlocked(premium) {
        console_item = MenuItem::with_id(app, "console", "Console…", true, None::<&str>)?;
        console_separator = PredefinedMenuItem::separator(app)?;
        items.push(&console_item);
        items.push(&console_separator);
    }
    items.push(&quit_item);
    let menu = Menu::with_items(app, &items)?;

    let icon = tauri::image::Image::from_bytes(TRAY_ICON_BYTES)?;

    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Centinelo Phone")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_and_focus(app),
            "toggle_available" => toggle_available(app),
            "toggle_auto_answer" => toggle_auto_answer(app),
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

mod bridge;
mod commands;
mod deeplink;
#[cfg(debug_assertions)]
mod e2e;
mod settings;
mod sidecar;
mod tray;

use settings::{AdminSession, SettingsStore};
use sidecar::SidecarHandle;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Single-instance MUST be registered before the deep-link plugin
        // (per tauri-plugin-deep-link's own docs) - with its "deep-link"
        // feature enabled (Cargo.toml), it forwards a second launch's argv
        // into the deep-link plugin automatically (Windows/Linux
        // centinelo:// or tel: activation while already running), and this
        // callback additionally surfaces the window on ANY second-launch
        // attempt, matching v1's `app.on('second-instance', ...)`
        // (src/main/main.js).
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            log::info!("second instance launched with args: {argv:?}");
            tray::show_and_focus(app);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_log::Builder::default().build())
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            let settings = Arc::new(SettingsStore::load(&app_data_dir)?);
            app.manage(settings.clone());
            app.manage(AdminSession::default());

            let sidecar = SidecarHandle::new(app.handle().clone(), settings.clone());
            app.manage(sidecar.clone());
            if settings.snapshot().account.is_configured() {
                sidecar.start();
            }

            bridge::start(app.handle().clone(), settings.clone(), sidecar.clone());
            deeplink::setup(app, settings.clone());

            tray::setup(app)?;

            #[cfg(debug_assertions)]
            {
                // Opt-in devtools (CENTINELO_OPEN_DEVTOOLS=1) - lets a human
                // drive commands via window.__TAURI__.core.invoke(...) from
                // the console instead of OS-level click automation. Off by
                // default so a plain `cargo tauri dev` stays uncluttered.
                if std::env::var("CENTINELO_OPEN_DEVTOOLS").as_deref() == Ok("1") {
                    if let Some(window) = app.get_webview_window("main") {
                        window.open_devtools();
                    }
                }
                e2e::maybe_run_e2e_script(app.handle());
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::sidecar_dial,
            commands::sidecar_answer,
            commands::sidecar_hangup,
            commands::sidecar_restart,
            commands::sidecar_status,
            commands::get_account_settings,
            commands::save_account_settings,
            commands::get_core_binary_path,
            commands::set_core_binary_path,
            commands::get_favorites,
            commands::save_favorites,
            commands::get_blf_states,
            commands::get_theme,
            commands::set_theme,
            commands::admin_status,
            commands::admin_set_password,
            commands::admin_unlock,
            commands::admin_lock,
            commands::get_recents,
            commands::add_recent,
            commands::get_bridge_settings,
            commands::set_auto_dial,
            commands::set_register_tel_handler,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(sidecar) = app_handle.try_state::<SidecarHandle>() {
                    sidecar.stop();
                    // Give ctrl_json a brief moment to exit cleanly (stdin
                    // EOF -> quit, core/PROTOCOL.md) before the process
                    // table disappears out from under it.
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
            }
        });
}

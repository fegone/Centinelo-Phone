mod activation;
mod bridge;
mod commands;
mod console;
mod deeplink;
#[cfg(debug_assertions)]
mod e2e;
mod hid;
mod premium;
mod provisioning;
mod settings;
mod sidecar;
mod sync_ext;
mod tray;
mod transcription;
mod updater;
mod url_policy;

use premium::PremiumHandle;
use settings::{AdminSession, SettingsStore};
use sidecar::SidecarHandle;
use std::sync::Arc;
use tauri::Manager;
use transcription::TranscriptionHandle;

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
        // HIPAA remote-install hardening (2026-08-13): `sidecar.rs`'s
        // `log::info!("sidecar event: {value}")` (module `app_lib::sidecar`)
        // deliberately logs every ctrl_json event verbatim - README.md's own
        // "Architecture: shell <-> sidecar" section and E2E.md document this
        // as the intentional e2e/dev evidence trail (`RUST_LOG=info` +
        // terminal capture). Those events include `call_state`'s `peer`
        // field - the caller's real SIP URI/phone number
        // (`core/PROTOCOL.md` "Events") - and, if transcription is ever
        // turned on, `transcription.rs`'s `log::debug!("transcribe[{call_id}]:
        // {line}")` (module `app_lib::transcription`) relays the transcribe
        // sidecar's raw stdout, including `segment` events carrying the
        // actual transcribed speech text.
        //
        // `tauri_plugin_log::Builder::default()` (the previous code here)
        // targets BOTH stdout AND `TargetKind::LogDir` - the OS-specific
        // logs folder (`%LOCALAPPDATA%\...\logs` on Windows) - meaning a
        // plain production install silently accumulates every caller's
        // phone number (and, if transcription is on, their words) in a
        // plaintext file on disk forever, independent of any Settings
        // toggle. That's exactly the kind of "PHI in a path nobody looks
        // at" this task's audit called out - worth closing regardless of
        // whether this particular install ever turns transcription on,
        // since it's a structural default, not a per-install choice.
        //
        // Fix: keep stdout logging exactly as documented (unfiltered - the
        // e2e/dev capture workflow in E2E.md/README.md depends on it
        // unchanged), but exclude these two call-content-bearing modules
        // from the persistent `LogDir` file. Every other module (sidecar
        // crash/restart diagnostics, updater, activation, HID, ...) still
        // gets written there for real troubleshooting - only the two log
        // sites that can carry a caller's identity or their words are
        // scoped out of the durable, on-disk copy.
        .plugin(
            tauri_plugin_log::Builder::default()
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir { file_name: None })
                        .filter(|metadata| !is_call_content_log_target(metadata.target())),
                ])
                .build(),
        )
        // Auto-updater (roadmap debt fix, see shell/README.md
        // "Auto-updater") - endpoint/pubkey come from tauri.conf.json's
        // own `plugins.updater` block, nothing to configure here.
        // tauri-plugin-process supplies relaunch() for the one step after
        // a successful install (ui/js/updater.js calls it directly via
        // @tauri-apps/plugin-process, no Rust-side glue needed for either
        // plugin beyond registering them).
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        // Serves the premium console UI (console-ui package) to the
        // "console" webview window - see console.rs's module doc for why
        // a custom protocol instead of a bundled frontendDist path (short
        // version: the console-ui source is premium and must never ship
        // in this public repo, so it can't live under `ui/`, the one
        // directory tauri.conf.json's `frontendDist` bundles). Registered
        // unconditionally on the builder (Tauri requires protocol
        // registration before `.build()`) - harmless when no premium
        // assets directory exists, since the "console" window itself is
        // only ever created when PremiumHandle reports the capability
        // licensed (see commands::open_console / tray.rs).
        .register_uri_scheme_protocol(console::ASSET_SCHEME, console::asset_protocol_handler)
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            let settings = Arc::new(SettingsStore::load(&app_data_dir)?);
            app.manage(settings.clone());
            app.manage(AdminSession::default());
            // Managed before deeplink::setup() below - a `centinelo://
            // provision` link handled during that call's own
            // `get_current()` branch (app launched *by* the link) spawns
            // a background thread that reaches for this state as soon as
            // it resolves (provisioning.rs `handle_deep_link`); for the
            // embedded (`config=`) form that resolution is instant, no
            // network wait to cover the ordering gap.
            app.manage(provisioning::ProvisioningPending::default());

            let sidecar = SidecarHandle::new(app.handle().clone(), settings.clone());
            app.manage(sidecar.clone());
            if settings.snapshot().account.is_configured() {
                sidecar.start();
            }

            bridge::start(app.handle().clone(), settings.clone(), sidecar.clone());
            deeplink::setup(app, settings.clone());

            // HID headset support (F4 ola 2, spec §5) - independent of the
            // premium loader/transcription below, so it only needs
            // settings + a way to send answer/hangup/mute commands, both
            // already available here. Never fails app startup (no headset
            // plugged in - or hidapi itself unavailable on this machine -
            // just means the background thread stays in a "searching"/
            // "disabled" state forever, see src/hid/mod.rs).
            app.manage(hid::HidHandle::new(app.handle().clone(), settings.clone(), sidecar.clone()));

            // Looks for centinelo_premium next to this executable, verifies
            // + loads it if present, silently stays in free mode if not -
            // never fails app startup either way. See premium.rs and
            // docs/loader-integration.md (private premium repo) for the
            // full design.
            let premium = PremiumHandle::load(app.handle().clone());
            app.manage(premium.clone());

            // Wired in after both PremiumHandle and SidecarHandle exist
            // (transcription needs the license gate + a way to send
            // tap_start/tap_stop) - see SidecarHandle::attach_transcription's
            // doc for why this is a post-construction attach rather than a
            // constructor argument.
            let transcription = TranscriptionHandle::new(
                app.handle().clone(),
                settings.clone(),
                premium.clone(),
                sidecar.clone(),
            );
            sidecar.attach_transcription(transcription.clone());
            app.manage(transcription);

            tray::setup(app, &premium)?;

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
            commands::get_blf_enabled,
            commands::set_blf_enabled,
            commands::get_audio_settings,
            commands::save_audio_settings,
            commands::sidecar_list_devices,
            commands::get_theme,
            commands::set_theme,
            commands::get_locale,
            commands::set_locale,
            commands::get_updater_settings,
            commands::set_updater_check_on_startup,
            commands::get_availability_settings,
            commands::set_available,
            commands::set_auto_answer,
            updater::updater_download,
            updater::updater_install,
            commands::admin_status,
            commands::admin_set_password,
            commands::admin_unlock,
            commands::admin_lock,
            commands::get_recents,
            commands::add_recent,
            commands::get_bridge_settings,
            commands::set_auto_dial,
            commands::set_register_tel_handler,
            commands::premium_info,
            commands::premium_capability_status,
            commands::premium_diagnostic,
            commands::open_console,
            commands::sidecar_hold,
            commands::sidecar_resume,
            commands::sidecar_mute,
            commands::sidecar_blind_transfer,
            commands::sidecar_attended_transfer,
            commands::sidecar_complete_transfer,
            commands::sidecar_abort_transfer,
            commands::sidecar_blf_subscribe,
            commands::sidecar_blf_unsubscribe,
            commands::get_transcription_settings,
            commands::save_transcription_settings,
            commands::test_remote_stt_connection,
            commands::transcription_manual_start,
            commands::transcription_manual_stop,
            commands::transcription_pending_retries,
            commands::transcription_retry,
            commands::transcription_model_status,
            commands::download_transcription_model,
            commands::reveal_in_file_manager,
            commands::provisioning_resolve,
            commands::provisioning_pending_preview,
            commands::provisioning_apply,
            commands::provisioning_cancel,
            commands::get_license_settings,
            commands::activate_license,
            hid::commands::hid_status,
            hid::commands::hid_list_devices,
            hid::commands::get_hid_settings,
            hid::commands::save_hid_settings,
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

/// `true` for the two Rust module paths that log a caller's phone number
/// (`sidecar`'s `call_state`/`reg_state`/... event trace) or their
/// transcribed words (`transcription`'s raw sidecar-stdout relay) - see
/// this file's own log-plugin setup comment for why those two, and only
/// those two, are kept out of the persistent `LogDir` file. `log::Metadata::target()`
/// defaults to the logging module's own path (`app_lib::sidecar`,
/// `app_lib::transcription` - confirmed against `shell/E2E.md`'s own
/// captured `[app_lib::sidecar][INFO] ...` lines), so a prefix match is
/// exact and stable regardless of which function inside either module
/// logged.
fn is_call_content_log_target(target: &str) -> bool {
    const MODULES: [&str; 2] = ["app_lib::sidecar", "app_lib::transcription"];
    MODULES.iter().any(|m| target == *m || target.starts_with(&format!("{m}::")))
}

#[cfg(test)]
mod log_target_tests {
    use super::*;

    #[test]
    fn sidecar_and_transcription_targets_are_excluded() {
        assert!(is_call_content_log_target("app_lib::sidecar"));
        assert!(is_call_content_log_target("app_lib::transcription"));
    }

    #[test]
    fn unrelated_targets_are_kept() {
        assert!(!is_call_content_log_target("app_lib::updater"));
        assert!(!is_call_content_log_target("app_lib::activation"));
        assert!(!is_call_content_log_target("app_lib::hid::device"));
        assert!(!is_call_content_log_target("tauri"));
    }

    #[test]
    fn a_target_that_merely_contains_the_word_is_not_a_false_positive() {
        // Prefix match, not substring match - a hypothetical future module
        // like `app_lib::sidecar_metrics` (unrelated to the call-content
        // logging this exists to scope out) must not be swept in by
        // accident just because it shares a prefix word.
        assert!(!is_call_content_log_target("app_lib::sidecar_metrics"));
    }
}

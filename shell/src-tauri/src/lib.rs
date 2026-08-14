mod activation;
mod bridge;
mod commands;
mod console;
mod deeplink;
#[cfg(debug_assertions)]
mod e2e;
mod frontend_log;
mod hid;
mod premium;
mod profile_cleanup;
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
        // HIPAA remote-install hardening (2026-08-13, revised same day after
        // two independent 4R findings on the first version of this fix -
        // see git blame / PR #20 history for the full story, kept here so
        // the *current* reasoning doesn't get re-litigated from scratch):
        //
        // `tauri_plugin_log::Builder::default()` (the previous code, before
        // any of this) targets BOTH stdout AND `TargetKind::LogDir` - the
        // OS-specific logs folder (`%LOCALAPPDATA%\...\logs` on Windows) -
        // and several call sites across this crate log content that
        // qualifies as PHI in the Neola Dental deployment this shell ships
        // to: a caller's phone number/SIP URI (`sidecar.rs`'s verbatim
        // `call_state`/`reg_state`/... event trace, `core/PROTOCOL.md`
        // "Events") or their transcribed words, if transcription is ever
        // turned on. A plain production install would otherwise
        // accumulate that in a plaintext file on disk forever, independent
        // of any Settings toggle - "PHI in a path nobody looks at."
        //
        // The *first* version of this fix excluded the two whole modules
        // that contain those log sites (`app_lib::sidecar`,
        // `app_lib::transcription`) from `LogDir` via `Target::filter`.
        // Two 4R lenses (RESILIENCE, then RELIABILITY independently) found
        // the same problem with that: `Target::filter` operates on
        // `log::Metadata::target()`, which defaults to the *module path*,
        // not the individual call site - so excluding a module excludes
        // EVERY line it logs, PHI-bearing or not. Those two modules are
        // exactly the ones that also carry this app's crash/restart/
        // transport-fallback/device-enumeration diagnostics (see e.g.
        // `sidecar.rs`'s `spawn_supervisor` retry-with-backoff loop and
        // `choose_transport`'s wss->udp fallback) - on a release build,
        // `main.rs`'s `windows_subsystem = "windows"` means there is no
        // console for stdout to go to, so `LogDir` losing those lines
        // means a remote Windows machine with no technical user in front
        // of it has ZERO on-disk trail of "why did the sidecar restart" or
        // "why did calls stop connecting" - worse than the PHI leak this
        // was meant to fix, for a support team that can't otherwise see
        // that machine.
        //
        // Fix (current): stop filtering by module. Only the specific log
        // *lines* that actually carry a caller's identity or their words
        // get an explicit `target: "app_lib::phi"` at their own call site
        // (`sidecar.rs`'s `log::info!(target: "app_lib::phi", "sidecar
        // event: {value}")` for the ctrl_json event trace, one more
        // `sidecar.rs` site for a `call_state` event logged on a *missing*
        // call_id - the event `Value` is still logged whole there, `peer`
        // and all - and `transcription.rs`'s raw relay of the
        // `centinelo-transcribe` child process's own stderr) - see each
        // call site's own comment for why it, specifically, needs this.
        // Every other line in those same modules keeps its normal
        // `app_lib::sidecar`/`app_lib::transcription` target and reaches
        // `LogDir` same as any other module - `is_call_content_log_target`
        // below is now a single-target check, not a module prefix check.
        //
        // Two more call sites turned out to log a caller's dialed number
        // directly - `bridge.rs`'s click-to-call HTTP handler and
        // `deeplink.rs`'s `tel:`/`centinelo:` handler, both compiled into
        // every release build (no `debug_assertions` gate, unlike
        // `e2e.rs`'s driver, which logs raw SIP URIs but never ships).
        // Those aren't excluded here at all - excluding either module
        // would cost real diagnostics for a bridge/deep-link bug report
        // ("is a request even arriving on that machine?") for no reason,
        // since unlike a ctrl_json event or a transcript segment, a single
        // dialed number is cheap to redact in place instead: see
        // `bridge::redacted_log_number`'s doc, used at both call sites.
        //
        // Stdout keeps logging every line above unfiltered, exactly as
        // before - the e2e/dev capture workflow in E2E.md/README.md
        // (`RUST_LOG=info` + terminal capture) depends on that and isn't
        // this task's concern; only the persistent `LogDir` copy is
        // filtered.
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

            // Windows-only cleanup of a stale `%APPDATA%\.baresip\` profile
            // left behind by a now-fixed engine bug that made the engine
            // ignore the `-f <scratch_dir>` path this app passes it and
            // fall back to baresip's own default profile location instead
            // - which, during diagnosis, ended up with a real SIP
            // `accounts` file (plaintext `auth_pass`) hand-copied into it.
            // Runs on every startup, not just after an installer, because
            // a machine that already hit the bug reaches this fix through
            // the auto-updater, never the installer again. See
            // profile_cleanup.rs's module doc for the full reasoning
            // (what gets deleted, why the whole directory, why not
            // unconditionally, and the safety line against ever touching
            // `app_data_dir` itself). macOS never had this bug (baresip's
            // Windows-only `fs_gethome()` fallback is what created the
            // stale directory in the first place), so this is Windows-only
            // by construction, not by omission.
            #[cfg(target_os = "windows")]
            {
                match std::env::var("APPDATA") {
                    Ok(appdata) => profile_cleanup::cleanup_stale_baresip_profile(
                        std::path::Path::new(&appdata),
                        &app_data_dir,
                    ),
                    Err(e) => log::warn!("stale-profile cleanup: APPDATA not set ({e}), skipping"),
                }
            }

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
            commands::get_codec_settings,
            commands::save_codec_settings,
            commands::sidecar_list_codecs,
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
            frontend_log::log_frontend_error,
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

/// The one `log::Metadata::target()` value that marks a line as carrying
/// call content (a caller's identity or their transcribed words) - see
/// this file's own log-plugin setup comment for the full "why per-line, not
/// per-module" story. A handful of `log::info!`/`log::warn!`/`log::debug!`
/// call sites in `sidecar.rs`/`transcription.rs` pass `target:
/// "app_lib::phi"` explicitly (overriding the macro's own default of the
/// calling module's path) precisely so this function can single them out
/// without taking every other diagnostic line in the same module down with
/// them. **Any future log site that logs a caller's number, SIP URI, or
/// transcribed speech text must do the same** (`log::info!(target:
/// "app_lib::phi", ...)`) - nothing here can discover a new PHI-bearing
/// call site automatically; see `phi_target_smoke_test` below for how far
/// automated coverage of that actually goes (a canary on the target name
/// itself, not on which sites use it).
fn is_call_content_log_target(target: &str) -> bool {
    target == "app_lib::phi"
}

#[cfg(test)]
mod log_target_tests {
    use super::*;

    #[test]
    fn the_phi_target_is_excluded() {
        assert!(is_call_content_log_target("app_lib::phi"));
    }

    #[test]
    fn a_diagnostic_line_from_the_same_module_is_kept() {
        // The actual guarantee this whole mechanism exists for (RELIABILITY
        // 4R fix-pass finding on PR #20's first version, which excluded by
        // module and silently took the sidecar crash/restart/transport-
        // fallback diagnostics down with the one PHI-bearing line): a
        // `sidecar.rs` line logged at its module's own default target -
        // e.g. `sidecar exited unexpectedly (...); attempt N/MAX` at
        // `app_lib::sidecar`, or a `transcription.rs` line at
        // `app_lib::transcription` - is NOT the phi target and must reach
        // the persistent log file, even though it lives in the same file
        // right next to a line that IS `app_lib::phi`.
        assert!(!is_call_content_log_target("app_lib::sidecar"));
        assert!(!is_call_content_log_target("app_lib::transcription"));
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
        // Exact match, not substring/prefix match - `app_lib::phi_metrics`
        // or `app_lib::graphify` must not be swept in by accident just
        // because they share a prefix/substring with the real sentinel.
        assert!(!is_call_content_log_target("app_lib::phi_metrics"));
        assert!(!is_call_content_log_target("app_lib::graphify"));
    }

    /// Not a test of `is_call_content_log_target` itself (already covered
    /// above) - a grep-based canary so that if a future edit ever
    /// misspells the sentinel target string at one of the three real call
    /// sites (`sidecar.rs`'s two, `transcription.rs`'s one), `cargo test`
    /// fails instead of the mismatch silently reintroducing PHI-on-disk.
    /// This is the honest answer to "a test that fails if a new PHI-
    /// bearing log site skips registration": no such test is possible in
    /// general (nothing distinguishes "call content" from any other string
    /// interpolated into a `log::` call short of a human reading it) - what
    /// IS checkable is that the sentinel string used here and the sentinel
    /// string used at each known real call site stay byte-identical.
    #[test]
    fn phi_target_string_matches_the_known_call_sites() {
        const SIDECAR_SRC: &str = include_str!("sidecar.rs");
        const TRANSCRIPTION_SRC: &str = include_str!("transcription.rs");
        let needle = "target: \"app_lib::phi\"";
        assert!(
            SIDECAR_SRC.matches(needle).count() >= 2,
            "expected at least 2 target: \"app_lib::phi\" call sites in sidecar.rs \
             (the ctrl_json event trace + the missing-call_id branch) - found {}",
            SIDECAR_SRC.matches(needle).count()
        );
        assert!(
            TRANSCRIPTION_SRC.matches(needle).count() >= 1,
            "expected at least 1 target: \"app_lib::phi\" call site in transcription.rs \
             (the centinelo-transcribe stderr relay) - found {}",
            TRANSCRIPTION_SRC.matches(needle).count()
        );
    }
}

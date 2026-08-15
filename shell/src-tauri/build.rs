use tauri_build::{AppManifest, Attributes};

/// Every command wired into `generate_handler!` in `src/lib.rs`, kept in
/// the exact same order so a `git diff` on both files stays easy to
/// eyeball. This is the ONLY reason ACL exists for these commands at all:
/// without an `AppManifest`, `has_app_acl_manifest` is false and Tauri
/// lets any local webview invoke any of them with zero permission check
/// (see tauri-2.11.5 src/webview/mod.rs's invoke-resolution gate). Listing
/// a command here makes tauri-build autogenerate `allow-<slug>` /
/// `deny-<slug>` permissions for it (identifier is the literal string
/// below, `_` slugified to `-`) — whether any window is actually GRANTED
/// `allow-<slug>` is decided per-window in `capabilities/*.json`, not
/// here. A command left out of this list gets no permission at all and
/// can never be invoked by any webview, ACL or not.
///
/// Keep this in sync with `generate_handler!` by hand: there is no way to
/// introspect the macro's command list at build-script time (it runs
/// before the `commands` module is even parsed for that purpose), so a
/// command added to one and not the other either won't compile (missing
/// from `generate_handler!`) or will silently have no ACL (missing from
/// here) — the second failure mode is exactly the bug this file exists to
/// close, so double-check both lists on every command addition.
const APP_COMMANDS: &[&str] = &[
    "sidecar_dial",
    "sidecar_answer",
    "sidecar_hangup",
    "sidecar_restart",
    "sidecar_status",
    "get_account_settings",
    "save_account_settings",
    "get_core_binary_path",
    "set_core_binary_path",
    "get_favorites",
    "save_favorites",
    "get_blf_states",
    "get_blf_enabled",
    "set_blf_enabled",
    "get_audio_settings",
    "save_audio_settings",
    "sidecar_list_devices",
    "get_codec_settings",
    "save_codec_settings",
    "sidecar_list_codecs",
    "get_theme",
    "set_theme",
    "get_locale",
    "set_locale",
    "get_updater_settings",
    "set_updater_check_on_startup",
    "get_availability_settings",
    "set_available",
    "set_auto_answer",
    "updater_download",
    "updater_install",
    "admin_status",
    "admin_set_password",
    "admin_unlock",
    "admin_lock",
    "get_recents",
    "add_recent",
    "get_bridge_settings",
    "set_auto_dial",
    "set_register_tel_handler",
    "premium_info",
    "premium_capability_status",
    "premium_diagnostic",
    "open_console",
    "console_panel_closed",
    "console_frontend_fatal",
    "console_frontend_ready",
    "sidecar_hold",
    "sidecar_resume",
    "sidecar_mute",
    "sidecar_blind_transfer",
    "sidecar_attended_transfer",
    "sidecar_complete_transfer",
    "sidecar_abort_transfer",
    "sidecar_blf_subscribe",
    "sidecar_blf_unsubscribe",
    "get_transcription_settings",
    "save_transcription_settings",
    "test_remote_stt_connection",
    "transcription_manual_start",
    "transcription_manual_stop",
    "transcription_pending_retries",
    "transcription_retry",
    "transcription_model_status",
    "download_transcription_model",
    "reveal_in_file_manager",
    "provisioning_resolve",
    "provisioning_pending_preview",
    "provisioning_apply",
    "provisioning_cancel",
    "get_license_settings",
    "activate_license",
    "hid_status",
    "hid_list_devices",
    "get_hid_settings",
    "save_hid_settings",
    "log_frontend_error",
];

fn main() {
    let attributes = Attributes::new().app_manifest(AppManifest::new().commands(APP_COMMANDS));

    if let Err(error) = tauri_build::try_build(attributes) {
        // Mirrors tauri_build::build()'s own error handling (it isn't
        // reusable directly since it always builds with default
        // Attributes) so a broken ACL still fails the build with the same
        // "unknown field -> stale tauri-build" hint instead of a bare
        // panic.
        let error = format!("{error:#}");
        println!("{error}");
        if error.starts_with("unknown field") {
            print!("found an unknown configuration field. This usually means that you are using a CLI version that is newer than `tauri-build` and is incompatible. ");
            println!(
                "Please try updating the Rust crates by running `cargo update` in the Tauri app folder."
            );
        }
        std::process::exit(1);
    }
}

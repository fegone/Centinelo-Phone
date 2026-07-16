//! Debug-only e2e driver: reads `CENTINELO_E2E_SCRIPT` and, if set, calls
//! the exact same `#[tauri::command]` functions the frontend's
//! `invoke()` calls would reach - via `AppHandle::state()` to get the same
//! `State<T>` extractors Tauri's IPC dispatch would construct - so a
//! scripted run exercises the identical sidecar-supervisor/settings code
//! path as a human clicking the UI (see PROTOCOL.md/README "e2e
//! verification"). Never compiled into a release build (`#[cfg(debug_assertions)]`
//! in lib.rs), and only active when the env var is explicitly set - a plain
//! `cargo tauri dev` / bundled app is unaffected.
//!
//! Script grammar: `|`-separated steps, e.g.
//!   "wait:5|dial:sip:*43@192.0.2.10|wait:8|hangup|wait:2"
//! Steps: `wait:<secs>`, `dial:<uri>`, `answer`, `hangup`,
//! `hold`, `resume`, `mute:on`/`mute:off`,
//! `blind_transfer:<uri>`, `attended_transfer:<uri>`,
//! `complete_transfer`, `abort_transfer`,
//! `blf_subscribe:<ext>`, `blf_unsubscribe:<ext>`,
//! `open_console`, `premium_diagnostic`,
//! `transcription_manual_start:<call_id>`, `transcription_manual_stop:<call_id>`,
//! `transcription_pending_retries`, `transcription_model_status:accurate|light`,
//! `reveal_in_file_manager:<path>` (panel ola-2 - "Show in folder"/"Show
//! local copy"; expected to `Err` on a plain scripted run since it
//! validates `path` against the configured `storage_dir`/temp tap-dir
//! roots - see `commands::reveal_in_file_manager`'s doc - the point of
//! this step is confirming the command dispatches and rejects an
//! out-of-scope path exactly like it would from the real UI, not that it
//! succeeds without a real transcript on disk),
//! `provisioning_resolve:<link>` (auto-provisioning, spec §5 - see
//! provisioning.rs; `<link>` is passed through unencoded, so a
//! `centinelo://provision?config=...` link's base64url payload is fine
//! as-is, but avoid a link containing a literal `|` - this driver's own
//! step separator), `provisioning_apply`, `provisioning_cancel`,
//! `admin_set_password:<password>` (sets/changes the admin password and
//! leaves the session unlocked on success, same as the real
//! `admin_set_password` command - lets a script reach an admin-gated step
//! like `provisioning_apply` on an already-configured account without a
//! GUI; added 2026-07-16 4R re-review to verify R4's "refuse to restart
//! mid-call" check end to end, which needs an unlocked session on an
//! already-configured account to even reach that check).
//!
//! Every step targets "the current call" (no `call_id`) - matching the
//! frontend's own single-call-at-a-time UI; there's no scripted way here
//! to address a specific consultation leg mid-attended-transfer. That's a
//! deliberate scope limit of this driver, not of the underlying commands
//! (which all accept an optional `call_id` - see `commands.rs`).

use crate::commands;
use crate::sidecar::SidecarHandle;
use tauri::{AppHandle, Manager};

pub fn maybe_run_e2e_script(app: &AppHandle) {
    let Ok(script) = std::env::var("CENTINELO_E2E_SCRIPT") else {
        return;
    };
    let app = app.clone();
    std::thread::spawn(move || {
        // Redact before logging the whole script (2026-07-16 qa-e2e finding
        // #3): a `provisioning_resolve:<link>` step's `config=`/`url=` query
        // value can carry a SIP secret (base64 is not encryption - trivially
        // reversible) straight from a `centinelo://provision` deep link.
        // Logging `script` verbatim put that secret in this debug log the
        // moment the script *started*, before `provisioning_resolve`'s own
        // existing secret-free preview ever ran. See `redact_script_for_log`.
        log::info!("e2e: script starting: {}", redact_script_for_log(&script));
        for raw_step in script.split('|') {
            let step = raw_step.trim();
            if step.is_empty() {
                continue;
            }
            if let Some(rest) = step.strip_prefix("wait:") {
                if let Ok(secs) = rest.parse::<u64>() {
                    log::info!("e2e: waiting {secs}s");
                    std::thread::sleep(std::time::Duration::from_secs(secs));
                }
                continue;
            }
            let sidecar: tauri::State<SidecarHandle> = app.state();
            if let Some(uri) = step.strip_prefix("dial:") {
                match commands::sidecar_dial(sidecar, uri.to_string()) {
                    Ok(()) => log::info!("e2e: dial({uri}) -> ok"),
                    Err(e) => log::error!("e2e: dial({uri}) -> err: {e}"),
                }
            } else if step == "answer" {
                match commands::sidecar_answer(sidecar) {
                    Ok(()) => log::info!("e2e: answer -> ok"),
                    Err(e) => log::error!("e2e: answer -> err: {e}"),
                }
            } else if step == "hangup" {
                match commands::sidecar_hangup(sidecar, None) {
                    Ok(()) => log::info!("e2e: hangup -> ok"),
                    Err(e) => log::error!("e2e: hangup -> err: {e}"),
                }
            } else if step == "hold" {
                match commands::sidecar_hold(sidecar, None) {
                    Ok(()) => log::info!("e2e: hold -> ok"),
                    Err(e) => log::error!("e2e: hold -> err: {e}"),
                }
            } else if step == "resume" {
                match commands::sidecar_resume(sidecar, None) {
                    Ok(()) => log::info!("e2e: resume -> ok"),
                    Err(e) => log::error!("e2e: resume -> err: {e}"),
                }
            } else if let Some(rest) = step.strip_prefix("mute:") {
                let on = rest == "on";
                match commands::sidecar_mute(sidecar, on, None) {
                    Ok(()) => log::info!("e2e: mute({on}) -> ok"),
                    Err(e) => log::error!("e2e: mute({on}) -> err: {e}"),
                }
            } else if let Some(uri) = step.strip_prefix("blind_transfer:") {
                match commands::sidecar_blind_transfer(sidecar, uri.to_string(), None) {
                    Ok(()) => log::info!("e2e: blind_transfer({uri}) -> ok"),
                    Err(e) => log::error!("e2e: blind_transfer({uri}) -> err: {e}"),
                }
            } else if let Some(uri) = step.strip_prefix("attended_transfer:") {
                match commands::sidecar_attended_transfer(sidecar, uri.to_string(), None) {
                    Ok(()) => log::info!("e2e: attended_transfer({uri}) -> ok"),
                    Err(e) => log::error!("e2e: attended_transfer({uri}) -> err: {e}"),
                }
            } else if step == "complete_transfer" {
                match commands::sidecar_complete_transfer(sidecar, None) {
                    Ok(()) => log::info!("e2e: complete_transfer -> ok"),
                    Err(e) => log::error!("e2e: complete_transfer -> err: {e}"),
                }
            } else if step == "abort_transfer" {
                match commands::sidecar_abort_transfer(sidecar) {
                    Ok(()) => log::info!("e2e: abort_transfer -> ok"),
                    Err(e) => log::error!("e2e: abort_transfer -> err: {e}"),
                }
            } else if let Some(ext) = step.strip_prefix("blf_subscribe:") {
                match commands::sidecar_blf_subscribe(sidecar, ext.to_string()) {
                    Ok(()) => log::info!("e2e: blf_subscribe({ext}) -> ok"),
                    Err(e) => log::error!("e2e: blf_subscribe({ext}) -> err: {e}"),
                }
            } else if let Some(ext) = step.strip_prefix("blf_unsubscribe:") {
                match commands::sidecar_blf_unsubscribe(sidecar, ext.to_string()) {
                    Ok(()) => log::info!("e2e: blf_unsubscribe({ext}) -> ok"),
                    Err(e) => log::error!("e2e: blf_unsubscribe({ext}) -> err: {e}"),
                }
            } else if step == "open_console" {
                match commands::open_console(app.clone()) {
                    Ok(()) => log::info!("e2e: open_console -> ok"),
                    Err(e) => log::info!("e2e: open_console -> err: {e}"),
                }
            } else if step == "premium_diagnostic" {
                let premium: tauri::State<crate::premium::PremiumHandle> = app.state();
                log::info!("e2e: premium_diagnostic = {}", premium.diagnostic());
                let status = premium.capability_status("blf_console");
                log::info!("e2e: premium_capability_status(blf_console) = {status:?}");
                let transcription_status = premium.capability_status("transcription");
                log::info!("e2e: premium_capability_status(transcription) = {transcription_status:?}");
            } else if let Some(call_id) = step.strip_prefix("transcription_manual_start:") {
                let transcription: tauri::State<crate::transcription::TranscriptionHandle> = app.state();
                match commands::transcription_manual_start(transcription, call_id.to_string(), "sip:e2e-test@example.invalid".to_string()) {
                    Ok(()) => log::info!("e2e: transcription_manual_start({call_id}) -> ok"),
                    Err(e) => log::info!("e2e: transcription_manual_start({call_id}) -> err: {e}"),
                }
            } else if let Some(call_id) = step.strip_prefix("transcription_manual_stop:") {
                let transcription: tauri::State<crate::transcription::TranscriptionHandle> = app.state();
                match commands::transcription_manual_stop(transcription, call_id.to_string()) {
                    Ok(()) => log::info!("e2e: transcription_manual_stop({call_id}) -> ok"),
                    Err(e) => log::info!("e2e: transcription_manual_stop({call_id}) -> err: {e}"),
                }
            } else if step == "transcription_pending_retries" {
                let transcription: tauri::State<crate::transcription::TranscriptionHandle> = app.state();
                let pending = commands::transcription_pending_retries(transcription);
                log::info!("e2e: transcription_pending_retries = {pending:?}");
            } else if let Some(tier) = step.strip_prefix("transcription_model_status:") {
                let parsed_tier = match tier {
                    "light" => crate::settings::ModelTier::Light,
                    _ => crate::settings::ModelTier::Accurate,
                };
                let status = commands::transcription_model_status(app.clone(), parsed_tier);
                log::info!("e2e: transcription_model_status({tier}) present={} path={}", status.present, status.path);
            } else if let Some(path) = step.strip_prefix("reveal_in_file_manager:") {
                let settings: tauri::State<std::sync::Arc<crate::settings::SettingsStore>> = app.state();
                match commands::reveal_in_file_manager(settings, path.to_string()) {
                    Ok(()) => log::info!("e2e: reveal_in_file_manager({path}) -> ok"),
                    Err(e) => log::info!("e2e: reveal_in_file_manager({path}) -> err: {e}"),
                }
            } else if let Some(link) = step.strip_prefix("provisioning_resolve:") {
                let provisioning: tauri::State<crate::provisioning::ProvisioningPending> = app.state();
                match commands::provisioning_resolve(provisioning, link.to_string()) {
                    Ok(preview) => log::info!("e2e: provisioning_resolve -> ok, preview={preview:?}"),
                    Err(e) => log::info!("e2e: provisioning_resolve -> err: {e}"),
                }
            } else if step == "provisioning_apply" {
                let settings: tauri::State<std::sync::Arc<crate::settings::SettingsStore>> = app.state();
                let admin: tauri::State<crate::settings::AdminSession> = app.state();
                let sidecar: tauri::State<SidecarHandle> = app.state();
                let provisioning: tauri::State<crate::provisioning::ProvisioningPending> = app.state();
                match commands::provisioning_apply(settings, admin, sidecar, provisioning) {
                    Ok(()) => log::info!("e2e: provisioning_apply -> ok"),
                    Err(e) => log::info!("e2e: provisioning_apply -> err: {e}"),
                }
            } else if step == "provisioning_cancel" {
                let provisioning: tauri::State<crate::provisioning::ProvisioningPending> = app.state();
                commands::provisioning_cancel(provisioning);
                log::info!("e2e: provisioning_cancel -> ok");
            } else if let Some(pw) = step.strip_prefix("admin_set_password:") {
                let settings: tauri::State<std::sync::Arc<crate::settings::SettingsStore>> = app.state();
                let admin: tauri::State<crate::settings::AdminSession> = app.state();
                match commands::admin_set_password(settings, admin, pw.to_string()) {
                    Ok(()) => log::info!("e2e: admin_set_password -> ok"),
                    Err(e) => log::info!("e2e: admin_set_password -> err: {e}"),
                }
            } else {
                log::warn!("e2e: unknown step '{step}'");
            }
        }
        // Genuine backend-tracked "app state" (sidecar.rs `Shared::blf_states`,
        // fed by real `blf` events - the same data the frontend's own
        // `state.blf` derives from), logged as part of the evidence trail so
        // BLF verification doesn't need any GUI/devtools introspection - see
        // shell/E2E.md "F3".
        let sidecar: tauri::State<SidecarHandle> = app.state();
        log::info!("e2e: final blf_states = {:?}", sidecar.blf_states());
        log::info!("e2e: script complete");
    });
}

/// Redacts any embedded provisioning secret before the full script is
/// logged (see the doc on the `log::info!` call above). Only a
/// `provisioning_resolve:<link>` step ever carries a link with a
/// `config=`/`url=` query value - every other step (`dial`, `mute`, ...)
/// has nothing sensitive to redact and passes through byte-for-byte.
fn redact_script_for_log(script: &str) -> String {
    script
        .split('|')
        .map(|raw_step| match raw_step.strip_prefix("provisioning_resolve:") {
            Some(link) => format!("provisioning_resolve:{}", redact_provisioning_link(link)),
            None => raw_step.to_string(),
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// Redacts the value of a `config=`/`url=` query parameter in a
/// `centinelo://provision?...` deep link, leaving the scheme/host/other
/// params visible (still useful for debugging which link shape a script
/// exercised, just not the secret-bearing payload itself).
fn redact_provisioning_link(link: &str) -> String {
    let Some((base, query)) = link.split_once('?') else {
        return link.to_string(); // no query string at all - nothing to redact
    };
    let redacted_query = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, _)) if key == "config" || key == "url" => format!("{key}=<redacted>"),
            _ => pair.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("{base}?{redacted_query}")
}

#[cfg(test)]
mod redact_tests {
    use super::*;

    #[test]
    fn embedded_config_payload_is_redacted() {
        let script = "wait:2|provisioning_resolve:centinelo://provision?config=eyJzZWNyZXQiOiJzM2NyZXQifQ|wait:1";
        let redacted = redact_script_for_log(script);
        assert!(!redacted.contains("eyJzZWNyZXQiOiJzM2NyZXQifQ"), "secret payload leaked: {redacted}");
        assert!(redacted.contains("provisioning_resolve:centinelo://provision?config=<redacted>"));
        // Untouched steps still readable, unredacted.
        assert!(redacted.contains("wait:2"));
        assert!(redacted.contains("wait:1"));
    }

    #[test]
    fn fetch_url_param_is_redacted_too() {
        let script = "provisioning_resolve:centinelo://provision?url=https://example.invalid/cfg?token=abc123";
        let redacted = redact_script_for_log(script);
        assert!(!redacted.contains("https://example.invalid/cfg?token=abc123"), "fetch URL leaked: {redacted}");
        assert!(redacted.contains("url=<redacted>"));
    }

    #[test]
    fn other_params_alongside_config_are_preserved() {
        let script = "provisioning_resolve:centinelo://provision?tls_pin=abcd&config=SECRETPAYLOAD";
        let redacted = redact_script_for_log(script);
        assert!(!redacted.contains("SECRETPAYLOAD"));
        assert!(redacted.contains("tls_pin=abcd"), "non-secret param should survive: {redacted}");
        assert!(redacted.contains("config=<redacted>"));
    }

    #[test]
    fn steps_without_provisioning_resolve_are_unchanged() {
        let script = "wait:5|dial:sip:*43@192.0.2.10|hangup";
        assert_eq!(redact_script_for_log(script), script);
    }

    #[test]
    fn link_with_no_query_string_passes_through() {
        let script = "provisioning_resolve:not-a-real-link-no-query";
        assert_eq!(redact_script_for_log(script), script);
    }
}

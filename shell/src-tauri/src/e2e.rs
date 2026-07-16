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
//!   "wait:5|dial:sip:*43@100.119.230.80|wait:8|hangup|wait:2"
//! Steps: `wait:<secs>`, `dial:<uri>`, `answer`, `hangup`,
//! `hold`, `resume`, `mute:on`/`mute:off`,
//! `blind_transfer:<uri>`, `attended_transfer:<uri>`,
//! `complete_transfer`, `abort_transfer`,
//! `blf_subscribe:<ext>`, `blf_unsubscribe:<ext>`,
//! `open_console`, `premium_diagnostic`,
//! `transcription_manual_start:<call_id>`, `transcription_manual_stop:<call_id>`,
//! `transcription_pending_retries`, `transcription_model_status:accurate|light`.
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
        log::info!("e2e: script starting: {script}");
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

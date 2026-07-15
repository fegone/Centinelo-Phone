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
//! Steps: `wait:<secs>`, `dial:<uri>`, `answer`, `hangup`.

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
                match commands::sidecar_hangup(sidecar) {
                    Ok(()) => log::info!("e2e: hangup -> ok"),
                    Err(e) => log::error!("e2e: hangup -> err: {e}"),
                }
            } else {
                log::warn!("e2e: unknown step '{step}'");
            }
        }
        log::info!("e2e: script complete");
    });
}

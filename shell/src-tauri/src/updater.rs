//! Auto-updater install path — the ONE place `has_active_call()` (the
//! authoritative, call_id-tracked source `sidecar.rs` hardened after the
//! R4 provisioning-cuts-active-calls bug, see its own doc on that method)
//! gates the disruptive step. See `shell/README.md` "Auto-updater" for the
//! full feature design; this module's own doc covers only why it exists
//! as dedicated Rust commands instead of the frontend calling
//! `tauri-plugin-updater`'s own `plugin:updater|download`/`|install`
//! directly (2026-07-17 4R re-review, RESILIENCE blocker).
//!
//! # Why not the plugin's own `plugin:updater|install` command
//!
//! The obvious shape for closing this gap would be a thin wrapper around
//! the plugin's existing `download`/`install` IPC commands that just adds
//! a call check in front. That doesn't work: `tauri-plugin-updater`
//! declares `mod commands` (not `pub mod`), and `lib.rs`'s `pub use
//! updater::*` only re-exports the `updater` module — `commands::
//! DownloadEvent` and `commands::DownloadedBytes` (the private resource
//! type a `bytes_rid` from `plugin:updater|download` actually points at)
//! are not reachable from outside the plugin crate at all. There is no
//! type this crate could name to resolve that resource id even if it
//! wanted to merely forward to `install()` with an extra check bolted on.
//!
//! What IS public (`tauri_plugin_updater::Update`, confirmed via its own
//! `updater.rs`: `#[derive(Clone)] pub struct Update`, `impl Resource for
//! Update {}`, and `pub async fn download`/`pub fn install`/`pub async fn
//! download_and_install`, all on `&self`) is the same underlying object
//! the plugin's own commands call these exact methods on internally. This
//! module calls them directly instead, storing the downloaded bytes under
//! **this crate's own** resource type (`DownloadedUpdateBytes`) so both
//! halves of the flow (`updater_download` / `updater_install`) agree on a
//! type this crate actually owns end to end — no dependency on anything
//! private to the plugin. `Update::download()` verifies the update's
//! signature before ever returning bytes (the plugin's own
//! `verify_signature` call, inside `download()`) — not something this
//! module re-implements or could accidentally skip.
//!
//! `updater_download` needs no call-safety check — downloading bytes over
//! HTTP never touches the running process or an active call. Only
//! `updater_install` (which calls `Update::install`, replacing the running
//! app / spawning the platform installer) does, and it re-checks
//! `has_active_call()` at the LAST possible moment before that happens —
//! `ui/js/updater.js`'s own `canStartInstall` mirror is UX only (disables
//! the button before the round trip even starts); this is the check that
//! actually decides.

use crate::sidecar::SidecarHandle;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{Manager, Resource, ResourceId, Runtime, State, Webview};
use tauri_plugin_updater::Update;

/// This crate's own resource wrapper for the bytes `updater_download`
/// fetched — see this module's header comment for why it can't reuse the
/// plugin's own (private) `DownloadedBytes` type.
struct DownloadedUpdateBytes(Vec<u8>);
impl Resource for DownloadedUpdateBytes {}

/// Same wire shape `tauri-plugin-updater`'s own (private, see header
/// comment) `DownloadEvent` uses — `ui/js/app.js`'s `startUpdateDownload`
/// already expects exactly `{event:"Started"|"Progress"|"Finished",
/// data:{contentLength|chunkLength}}` from when it called the plugin's own
/// command directly; keeping the same shape here means that JS-side
/// handling didn't need to change, only the invoke target.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum DownloadEvent {
    #[serde(rename_all = "camelCase")]
    Started { content_length: Option<u64> },
    #[serde(rename_all = "camelCase")]
    Progress { chunk_length: usize },
    Finished,
}

/// Downloads and signature-verifies the update `update_rid` refers to
/// (the resource id `plugin:updater|check` — still called directly, it's
/// read-only and carries no call-safety concern — handed back), reporting
/// progress on `on_event`. Returns a NEW resource id (this crate's own
/// `DownloadedUpdateBytes`, not the plugin's), which `updater_install`
/// below is the only thing that ever reads.
#[tauri::command(rename_all = "snake_case")]
pub async fn updater_download<R: Runtime>(
    webview: Webview<R>,
    update_rid: ResourceId,
    on_event: Channel<DownloadEvent>,
) -> Result<ResourceId, String> {
    let update = webview.resources_table().get::<Update>(update_rid).map_err(|e| e.to_string())?;

    let mut first_chunk = true;
    let bytes = update
        .download(
            |chunk_length, content_length| {
                if first_chunk {
                    first_chunk = false;
                    let _ = on_event.send(DownloadEvent::Started { content_length });
                }
                let _ = on_event.send(DownloadEvent::Progress { chunk_length });
            },
            || {
                let _ = on_event.send(DownloadEvent::Finished);
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    Ok(webview.resources_table().add(DownloadedUpdateBytes(bytes)))
}

/// Pure decision function, extracted specifically so the ordering
/// guarantee ("refuse before touching any resource, not merely before
/// `install()` itself") has a real, unit-testable name — same reasoning
/// `ui/js/updater.js`'s `canStartInstall` was extracted for on the
/// frontend side. `sidecar.rs`'s own `call_phase_tests` already cover
/// `has_active_call()`'s state machine exhaustively; this function is the
/// one line of NEW decision logic this module adds on top of it.
fn refuse_install_while_on_a_call(has_active_call: bool) -> Result<(), String> {
    if has_active_call {
        return Err("You're on a call — finish or hang up before installing this update.".to_string());
    }
    Ok(())
}

/// The disruptive step — replaces the running app / hands off to the
/// platform installer. Refuses while `sidecar.has_active_call()` reports
/// true (see this module's header comment and `sidecar.rs`'s own doc on
/// that method for why it, not a frontend-mirrored `state.call`, is the
/// authoritative source) — checked and returned on BEFORE either resource
/// is even looked up, so a refusal never has the side effect of consuming/
/// closing anything. Synchronous like every other command in this crate
/// (`Update::install` itself is `fn`, not `async fn` — no await needed
/// here at all) — blocking I/O in a sync command runs on Tauri's own
/// command thread pool, same as every other command in this codebase that
/// touches disk (see `settings.rs`'s `write_private_file`, called from
/// several sync commands the same way).
#[tauri::command(rename_all = "snake_case")]
pub fn updater_install<R: Runtime>(
    webview: Webview<R>,
    sidecar: State<SidecarHandle>,
    update_rid: ResourceId,
    bytes_rid: ResourceId,
) -> Result<(), String> {
    refuse_install_while_on_a_call(sidecar.has_active_call())?;

    let update = webview.resources_table().get::<Update>(update_rid).map_err(|e| e.to_string())?;
    let bytes = webview.resources_table().get::<DownloadedUpdateBytes>(bytes_rid).map_err(|e| e.to_string())?;

    update.install(&bytes.0).map_err(|e| e.to_string())?;
    let _ = webview.resources_table().close(bytes_rid);
    Ok(())
}

#[cfg(test)]
mod refuse_install_while_on_a_call_tests {
    use super::*;

    #[test]
    fn refuses_with_a_clear_message_while_a_call_is_active() {
        let err = refuse_install_while_on_a_call(true).unwrap_err();
        assert!(err.contains("on a call"), "unexpected message: {err}");
    }

    #[test]
    fn allows_installing_when_no_call_is_active() {
        assert!(refuse_install_while_on_a_call(false).is_ok());
    }
}

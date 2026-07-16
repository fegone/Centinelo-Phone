//! Local call transcription orchestration (F4 "plomeria" sprint).
//!
//! This module wires the pieces the task spec calls out: gating behind the
//! `transcription` premium capability (same mechanism `console.rs` uses for
//! `blf_console`), admin-locked settings (`settings.rs`'s
//! `TranscriptionSettings`), automatic `tap_start`/`tap_stop` on the engine
//! (`core/PROTOCOL.md` v1.2) driven by real `call_state`/`tap_state`
//! events, spawning the `centinelo-transcribe` sidecar binary against the
//! two tapped WAVs, and moving the finished transcript (+ optionally the
//! raw audio) into `storage_dir/YYYY/MM/DD/`.
//!
//! # What this sprint does NOT build
//!
//! The live-updating transcript panel is ola 2 (mockups landed
//! 2026-07-16, see creative-vigilia's report) - this module only ever
//! emits Tauri events (`transcription://segment`/`done`/`error`/
//! `model-download-*`) for a future frontend to subscribe to; nothing
//! here renders UI.
//!
//! # Where the license check happens
//!
//! Never here in a way that could be forked around - same discipline as
//! `premium.rs`/`console.rs`: [`is_unlocked`] only ever asks the loaded
//! premium dylib "is `transcription` licensed" and relays the answer.
//!
//! # Contract with `centinelo-transcribe` (the sidecar binary)
//!
//! **Confirmed against transcribe-engine's real implementation**
//! (`premium` repo, `feature/transcribe-e2e` @ `962754e`, read read-only -
//! not this module's scope to edit) as of 2026-07-16, superseding this
//! module's earlier guess at the contract (which used an `"event"` JSON
//! key and `txt_path`/`json_path` done-event field names - the real
//! binary uses `"type"` and `txt`/`json`; fixed here to match):
//!
//! ```text
//! centinelo-transcribe run --rx <rx.wav> --tx <tx.wav> --model <path> \
//!   --lang <lang> --mode live|post --out-dir <dir> [--meta <json-or-path>]
//! ```
//!
//! Stdout, one JSON object per line: `{"type":"segment","speaker":"agent"|
//! "caller","t0_ms":N,"t1_ms":N,"text":"..."}`, then exactly one
//! `{"type":"done","txt":"...","json":"..."}` (or `{"type":"error",
//! "message":"..."}` - no `done` follows an `error`). `--mode live` polls
//! the still-growing WAVs until it reads a `stop` line on stdin (or EOF),
//! flushes, then emits `done`. `rx` is always the **Caller** (remote
//! RTP), `tx` is always the **Agent** (local mic) - this module passes
//! `core/PROTOCOL.md`'s own `<call_id>-rx.wav`/`-tx.wav` straight through
//! unmodified, so that pairing is inherited from `tap_start`'s own
//! naming, never re-derived here.
//!
//! `--meta` accepts **inline JSON or a file path** (the binary tries
//! parsing it as JSON first, then falls back to reading it as a path) -
//! this module always passes a path (`write_meta_file`), never inline
//! JSON, so call metadata (the caller's number, in particular) never
//! appears in this process's own argv, visible to any other local user
//! via `ps` - see `write_meta_file`'s doc (2026-07-16 review finding M5).
//!
//! A separate `centinelo-transcribe ensure-model --tier <tier> --models-dir
//! <dir>` subcommand (not `run`) handles on-demand, checksum-verified
//! model download - `{"type":"progress","asset":...,"downloaded":N,
//! "total":N}` / `{"type":"ready","model":"...","vad_model":"..."}` /
//! `{"type":"error","message":"..."}`. This module shells out to it
//! (`ensure_model`) rather than re-implementing its own download+checksum
//! logic (an earlier version of this file did exactly that, with no
//! pinned checksum and no `Content-Length` validation - 2026-07-16 review
//! finding M4) - transcribe-engine's copy is the one, real source of
//! pinned SHA256s (pulled from HuggingFace's own LFS metadata), so
//! shelling out to it is the actual "consolidate into one source" fix,
//! not hand-copying their hashes into a second, driftable copy here.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use crate::premium::{CapabilityStatusView, PremiumHandle};
use crate::settings::{
    ModelTier, SettingsStore, TranscriptionActivation, TranscriptionMode, TranscriptionSettings,
};
use crate::sidecar::SidecarHandle;

/// The [`centinelo_premium_abi::Capability::feature_name`] this whole
/// module is gated behind - see `console.rs`'s `CAPABILITY`/`is_unlocked`
/// for the sibling pattern this mirrors.
const CAPABILITY: &str = "transcription";

/// Whether the transcription feature (settings + auto-tap + sidecar
/// orchestration) should be offered at all right now. Accepts both
/// `Available` (the dylib itself now reports `Transcription` as
/// `Available` once the license includes it - see
/// `licensing-2026-07-16-transcription-flag` report) and `NotImplemented`
/// (the earlier, still-valid-in-general "licensed but this dylib build's
/// own FFI stub has nothing behind it yet" case - see `console.rs`'s
/// sibling `unlocks_console` for the fuller reasoning, which applies
/// unchanged here) - either way, the *shell's* implementation of the
/// feature (this module) is what actually runs; the dylib call is only
/// ever the license probe. `NotLicensed`/`Unavailable` (no dylib,
/// tampered signature, FFI error) both still hide the feature entirely.
pub fn is_unlocked(premium: &PremiumHandle) -> bool {
    matches!(
        premium.capability_status(CAPABILITY),
        CapabilityStatusView::Available | CapabilityStatusView::NotImplemented
    )
}

// ---------------------------------------------------------------------
// call_id validation (2026-07-16 review, S1 - RISK VETO)
// ---------------------------------------------------------------------

/// Longest `call_id` this shell will ever act on. Generous for any
/// realistic engine-generated id (observed real ids are short hex
/// strings - `core/E2E-F1.md`) while still bounding worst-case path
/// component length.
const MAX_CALL_ID_LEN: usize = 128;

/// Validates a `call_id` **before it is ever used to build a filesystem
/// path** - the tap directory, the WAV filenames, and the final
/// transcript's base filename under `storage_dir`
/// (`finalize_artifacts`'s `base`). See the 2026-07-16 4R review, finding
/// S1 (RISK VETO): `call_id` reaches this module from two places - a
/// Tauri command argument (`commands::transcription_manual_start`, which
/// is **not** admin-gated, so any webview content can supply an
/// arbitrary string) and the core engine's own `call_state`/`tap_state`
/// events. Neither is trusted. A `call_id` of e.g. `"../../../etc/x"`
/// reaching `format!("{call_id}-rx.wav")`/`.join(...)` unchecked would let
/// an attacker write or read outside every directory this module ever
/// touches (`std::path::Path::join` does not sandbox `..` or embedded
/// `/`/`\` - the OS resolves them as real path separators the moment a
/// `std::fs` call actually touches disk).
///
/// [`TranscriptionHandle::start_tap`] is the single choke point every
/// `call_id` passes through before any path is built from it (both the
/// automatic path from `on_call_state` and the manual path from
/// `manual_start` call it) - see that function's own doc. Every other
/// place a `call_id` is used in this module (`on_tap_started`/
/// `on_tap_stopped`/`manual_stop`/`retry`) only ever looks it up as a
/// `HashMap` key against `active`/`pending_retries`, whose keys are
/// themselves only ever populated through `start_tap` - an
/// attacker-supplied `call_id` that never passed validation simply never
/// matches anything and those lookups no-op, so a second validation call
/// at each of those sites would be redundant, not defense-in-depth.
///
/// Rejects: empty, longer than [`MAX_CALL_ID_LEN`], containing `/`, `\`,
/// or the two-character sequence `..` anywhere, or any byte outside
/// `[A-Za-z0-9._-]`.
fn valid_call_id(call_id: &str) -> bool {
    if call_id.is_empty() || call_id.len() > MAX_CALL_ID_LEN {
        return false;
    }
    if call_id.contains("..") || call_id.contains('/') || call_id.contains('\\') {
        return false;
    }
    call_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

// ---------------------------------------------------------------------
// Orchestration handle
// ---------------------------------------------------------------------

/// Per-call bookkeeping needed to finalize a tap once transcription
/// finishes - captured once at tap-start time so the reader/finalize
/// thread never has to re-read settings or re-look-up the call, and a
/// mid-call settings change (e.g. admin edits `storage_dir` while a call
/// is in progress) can't produce inconsistent behavior partway through a
/// single call's pipeline - matches how account-settings changes
/// elsewhere in this crate only take effect on the *next* sidecar
/// respawn, never retroactively.
#[derive(Clone)]
struct FinalizeContext {
    call_id: String,
    peer: String,
    tap_dir: PathBuf,
    rx_path: PathBuf,
    tx_path: PathBuf,
    started_at: DateTime<Local>,
    settings: TranscriptionSettings,
}

/// Coordinates the `on_tap_started`/`on_tap_stopped` handshake for a
/// `Live`-mode tap - split out from `ActiveEntry` specifically so this
/// coordination logic is unit-testable under a bare struct, with no
/// `Mutex`/`Inner`/`AppHandle`/thread-spawning involved (2026-07-16
/// review, finding A2: "maquina de estados SIN tests").
///
/// # The race this closes
///
/// `tap_state:"started"` always precedes `tap_state:"stopped"` on the
/// wire (a tap must exist before it can stop), but this shell's own
/// reaction to each is asynchronous (`on_tap_started`/`on_tap_stopped`
/// each spawn a fresh thread) - so it's possible for `on_tap_stopped`'s
/// thread to finish running *before* `on_tap_started`'s thread has
/// finished spawning the live `centinelo-transcribe` process on a very
/// short call. Whichever of "the process is spawned" and "stop was
/// requested" happens *second* is the one that must actually write the
/// `stop` line - this struct's two `mark_*` methods each return whether
/// **this call** is the second one, i.e. whether the caller should send
/// stop right now.
#[derive(Debug, Default)]
struct TapCoordination {
    live_spawned: bool,
    stop_requested: bool,
}

impl TapCoordination {
    fn new() -> Self {
        Self::default()
    }

    /// Call once `on_tap_started` has a live child's stdin in hand.
    /// Returns `true` if stop should be signaled immediately (a
    /// `mark_stop_requested` call already landed first).
    fn mark_live_spawned(&mut self) -> bool {
        self.live_spawned = true;
        self.stop_requested
    }

    /// Call from `on_tap_stopped`. Returns `true` if the live process is
    /// already spawned (the caller should write `stop` to its stdin
    /// right now) - `false` means `on_tap_started` hasn't finished
    /// spawning yet, and `mark_live_spawned`'s return value above is what
    /// will eventually signal it instead.
    fn mark_stop_requested(&mut self) -> bool {
        self.stop_requested = true;
        self.live_spawned
    }

    fn stop_requested(&self) -> bool {
        self.stop_requested
    }
}

struct ActiveEntry {
    ctx: FinalizeContext,
    /// Only ever `Some` in `Live` mode, and only after the transcribe
    /// process has actually spawned (`on_tap_started`).
    live_stdin: Option<std::process::ChildStdin>,
    coordination: TapCoordination,
    /// Set by `spawn_reader_and_finalize` if a `Live`-mode transcribe
    /// process exits **before** this call's tap was ever told to stop
    /// (`coordination.stop_requested()` was still `false` at exit) - i.e.
    /// it crashed, or exited on its own, while the call may still be
    /// active (2026-07-16 review, finding A3). When this is `true`,
    /// `on_tap_stopped`'s real, eventual stop event falls back to running
    /// a fresh **post-call** pass against the WAVs (which keep being
    /// written by the still-running core tap regardless of what happened
    /// to the transcribe process) instead of assuming a long-dead reader
    /// thread still owns finalize.
    live_died_early: bool,
    /// Set by `on_tap_started` the moment it confirms this call's tap
    /// really started (`tap_state:"started"` actually arrived) - read by
    /// `arm_tap_start_watchdog` (2026-07-16 review, finding M1) to detect
    /// a `tap_start` that silently never landed (e.g. the call already
    /// ended before the fire-and-forget wire write reached the engine,
    /// which then responds with a generic, uncorrelated `error` event
    /// this module has no way to route back to this specific call - see
    /// that function's own doc for why a timeout, not protocol
    /// correlation, is this version's fix).
    tap_confirmed: bool,
}

struct Inner {
    app: AppHandle,
    settings: Arc<SettingsStore>,
    premium: PremiumHandle,
    sidecar: SidecarHandle,
    active: Mutex<HashMap<String, ActiveEntry>>,
    /// Calls whose final `storage_dir` write failed (NAS unreachable, disk
    /// full, engine binary missing, ...) - see `finalize_artifacts`'s doc
    /// and the task spec's "no perder los WAVs (reintento manual
    /// posible)". The original WAVs (and transcript, if the engine got
    /// that far) stay on disk in `PendingRetry::tap_dir` until
    /// [`TranscriptionHandle::retry`] succeeds or the process restarts
    /// (a fresh launch does NOT re-scan for orphaned tap dirs this
    /// version - see this file's report, "known limitations").
    pending_retries: Mutex<HashMap<String, PendingRetry>>,
}

/// Handle stashed in Tauri's managed state (`app.manage(...)`, see
/// `lib.rs`) and additionally handed to `SidecarHandle` (see
/// `sidecar::SidecarHandle::attach_transcription`) so the stdout-reader
/// thread can forward `call_state`/`tap_state` events into it. `Clone` is
/// cheap (`Arc`), matching `PremiumHandle`/`SidecarHandle`'s own
/// newtype-over-`Arc` pattern.
#[derive(Clone)]
pub struct TranscriptionHandle(Arc<Inner>);

impl TranscriptionHandle {
    pub fn new(
        app: AppHandle,
        settings: Arc<SettingsStore>,
        premium: PremiumHandle,
        sidecar: SidecarHandle,
    ) -> Self {
        Self(Arc::new(Inner {
            app,
            settings,
            premium,
            sidecar,
            active: Mutex::new(HashMap::new()),
            pending_retries: Mutex::new(HashMap::new()),
        }))
    }

    /// Called from `sidecar.rs`'s stdout-reader thread on every
    /// `call_state` event. Deliberately cheap and non-blocking (only a
    /// mutex check + maybe inserting a map entry) - the actual
    /// `tap_start` wire write happens on a fresh thread (`start_tap`) so
    /// this never risks delaying that same reader thread's delivery of
    /// this call's own upcoming `tap_state:"started"` event.
    pub fn on_call_state(&self, event: &Value) {
        let Some((state, call_id)) = event_state_and_call_id(event) else {
            return;
        };
        if state != "established" {
            return; // "closed" is handled entirely via tap_state:"stopped" - see on_tap_stopped's doc.
        }
        let already_active = self.0.active.lock().expect("poisoned").contains_key(call_id);
        let settings = self.0.settings.snapshot().transcription;
        if !should_auto_tap(&settings, already_active) {
            return;
        }
        if !is_unlocked(&self.0.premium) {
            return;
        }
        let peer = event.get("peer").and_then(Value::as_str).unwrap_or("").to_string();
        // start_tap re-checks "already active" atomically (M2) and
        // validates call_id (S1) - the checks above are a cheap
        // fast-path, not the correctness guarantee.
        if let Err(e) = self.start_tap(call_id, &peer, settings) {
            log::debug!("transcription: auto-tap for {call_id} not started: {e}");
        }
    }

    /// Called from `sidecar.rs`'s stdout-reader thread on every
    /// `tap_state` event (`core/PROTOCOL.md` v1.2).
    pub fn on_tap_state(&self, event: &Value) {
        let Some((state, call_id)) = event_state_and_call_id(event) else {
            return;
        };
        match state {
            "started" => self.on_tap_started(call_id),
            "stopped" => self.on_tap_stopped(call_id, event),
            _ => {}
        }
    }

    /// Manual per-call start (`transcription.activation == Manual`) - the
    /// ola-2 panel's per-call button calls this via
    /// `commands::transcription_manual_start`. `peer` is supplied by the
    /// caller (the in-call UI already has it) rather than re-derived here.
    pub fn manual_start(&self, call_id: &str, peer: &str) -> Result<(), String> {
        if !is_unlocked(&self.0.premium) {
            return Err("Transcription is not licensed on this installation.".to_string());
        }
        let settings = self.0.settings.snapshot().transcription;
        if settings.mode == TranscriptionMode::Off {
            return Err("Transcription is turned off in Settings.".to_string());
        }
        self.start_tap(call_id, peer, settings)
    }

    /// Manual per-call stop - sends `tap_stop`; the rest of the pipeline
    /// (finalize/move/cleanup) runs the same way it would for a natural
    /// hangup, off the resulting `tap_state:"stopped"` event.
    pub fn manual_stop(&self, call_id: &str) -> Result<(), String> {
        if !self.0.active.lock().expect("poisoned").contains_key(call_id) {
            return Err("Not currently transcribing this call.".to_string());
        }
        self.0
            .sidecar
            .send_cmd(serde_json::json!({"cmd": "tap_stop", "call_id": call_id}))
    }

    pub fn pending_retries(&self) -> Vec<PendingRetryView> {
        self.0
            .pending_retries
            .lock()
            .expect("poisoned")
            .values()
            .map(PendingRetryView::from)
            .collect()
    }

    /// Re-runs the transcribe pass for a call whose finalize step failed
    /// earlier (NAS down, disk full, engine binary missing) - see
    /// `Inner::pending_retries`'s doc. If the transcript itself already
    /// exists (only the final move into `storage_dir` failed - e.g. a
    /// transient NAS blip after the engine had already finished), this
    /// just retries that move rather than re-running the whole engine
    /// against the WAVs again (2026-07-16 review, finding B1: the earlier
    /// version threw the already-known `txt_path`/`json_path` away on
    /// every failure and always re-transcribed from scratch).
    pub fn retry(&self, call_id: &str) -> Result<(), String> {
        let retry = {
            let mut guard = self.0.pending_retries.lock().expect("poisoned");
            guard
                .remove(call_id)
                .ok_or_else(|| "No pending retry for this call.".to_string())?
        };
        let ctx = FinalizeContext {
            call_id: retry.call_id.clone(),
            peer: retry.peer.clone(),
            tap_dir: retry.tap_dir.clone(),
            rx_path: retry.rx_path.clone(),
            tx_path: retry.tx_path.clone(),
            started_at: retry.started_at,
            settings: retry.settings.clone(),
        };

        if let (Some(txt), Some(json)) = (&retry.txt_path, &retry.json_path) {
            if txt.is_file() && json.is_file() {
                return match finalize_artifacts(&ctx, txt, json) {
                    Ok(result) => {
                        let _ = self.0.app.emit(
                            "transcription://done",
                            serde_json::json!({
                                "call_id": ctx.call_id,
                                "txt_path": result.txt.display().to_string(),
                                "json_path": result.json.display().to_string(),
                                "audio_kept": result.audio_kept,
                            }),
                        );
                        Ok(())
                    }
                    Err(e) => {
                        stash_pending_retry(&self.0, &ctx, Some(txt.clone()), Some(json.clone()), e.clone());
                        Err(e)
                    }
                };
            }
        }

        if !retry.rx_path.is_file() && !retry.tx_path.is_file() {
            let call_id_owned = retry.call_id.clone();
            self.0.pending_retries.lock().expect("poisoned").insert(call_id_owned, retry);
            return Err("The original audio for this call is no longer on disk - nothing to retry.".to_string());
        }

        let Some(child) = spawn_transcribe_for(&self.0, &ctx, "post") else {
            // spawn_transcribe_for already logged, emitted
            // transcription://error, and re-stashed a fresh
            // pending_retries entry for this call_id with the new failure
            // reason - nothing left for this fn to restore.
            return Err("Retry failed to start - see the transcription://error event for why.".to_string());
        };
        spawn_reader_and_finalize(self.0.clone(), ctx, child);
        Ok(())
    }

    /// The single entry point every `call_id` this module acts on passes
    /// through before any filesystem path is built from it - see
    /// [`valid_call_id`]'s doc (S1) for why this specific function is
    /// that choke point. Also the single place a tap is atomically
    /// reserved in `active` (2026-07-16 review, finding M2: the earlier
    /// version's separate `contains_key` check + later `insert` in two
    /// different function calls left a TOCTOU window a double-click on
    /// the not-admin-gated `transcription_manual_start` command could
    /// hit).
    fn start_tap(&self, call_id: &str, peer: &str, settings: TranscriptionSettings) -> Result<(), String> {
        if !valid_call_id(call_id) {
            log::warn!("transcription: refusing to tap call with an invalid call_id ({} bytes)", call_id.len());
            return Err("Invalid call id.".to_string());
        }

        let tap_dir = std::env::temp_dir().join(format!("centinelo-transcribe-tap.{call_id}"));
        let rx_path = tap_dir.join(format!("{call_id}-rx.wav"));
        let tx_path = tap_dir.join(format!("{call_id}-tx.wav"));
        let ctx = FinalizeContext {
            call_id: call_id.to_string(),
            peer: peer.to_string(),
            tap_dir: tap_dir.clone(),
            rx_path,
            tx_path,
            started_at: Local::now(),
            settings,
        };

        // Atomic check-and-reserve: a single lock acquisition via the
        // Entry API, so two concurrent callers (e.g. a double-click on
        // the manual-start button, which isn't admin-gated) can never
        // both observe "not active yet" and both proceed - the second
        // one's `Entry::Occupied` always wins the race, deterministically.
        {
            let mut active = self.0.active.lock().expect("poisoned");
            match active.entry(call_id.to_string()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    return Err("Already transcribing this call.".to_string());
                }
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(ActiveEntry {
                        ctx,
                        live_stdin: None,
                        coordination: TapCoordination::new(),
                        live_died_early: false,
                        tap_confirmed: false,
                    });
                }
            }
        }

        if let Err(e) = std::fs::create_dir_all(&tap_dir) {
            log::warn!("transcription: could not create tap dir for {call_id}: {e}");
            self.0.active.lock().expect("poisoned").remove(call_id);
            return Err(format!("Could not create a working directory for this call: {e}"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tap_dir, std::fs::Permissions::from_mode(0o700));
        }

        let sidecar = self.0.sidecar.clone();
        let call_id_owned = call_id.to_string();
        let dir_str = tap_dir.display().to_string();
        // Fire-and-forget stdin write (sub-millisecond) - still off the
        // caller's thread on principle, matching every other command
        // dispatch in this crate (`commands.rs`/`bridge.rs`).
        std::thread::spawn(move || {
            if let Err(e) = sidecar.send_cmd(serde_json::json!({
                "cmd": "tap_start",
                "dir": dir_str,
                "call_id": call_id_owned,
            })) {
                log::warn!("transcription: tap_start({call_id_owned}) failed: {e}");
            }
        });

        self.arm_tap_start_watchdog(call_id);
        Ok(())
    }

    /// Cleans up an `active` entry (and its tap directory) whose
    /// `tap_start` was never confirmed by a real `tap_state:"started"`
    /// event within [`tap_start_confirm_timeout`] - see
    /// `ActiveEntry::tap_confirmed`'s doc (2026-07-16 review, finding M1).
    /// `core/PROTOCOL.md`'s `tap_start` can fail with a generic, call-id-
    /// less `error` event (e.g. the call already ended before this
    /// fire-and-forget command reached the engine) that this module has
    /// no way to correlate back to the specific call that sent it -
    /// full request/response correlation (the protocol's own `id`/
    /// `result` mechanism, v1.1+) would close this properly but isn't
    /// wired up anywhere in `sidecar.rs` yet for any command, and adding
    /// it is a larger, shared-infrastructure change out of scope for this
    /// fix - a bounded timeout is the pragmatic version-scoped fix: it
    /// can't distinguish "the command genuinely failed" from "it's just
    /// slow", but the failure mode of falsely cleaning up a real,
    /// slow-to-confirm tap is `on_tap_started` finding nothing in
    /// `active` and silently no-op'ing (never a crash, never a corrupted
    /// tap) - see that function's own `None => return` arm.
    fn arm_tap_start_watchdog(&self, call_id: &str) {
        let inner = self.0.clone();
        let call_id = call_id.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(tap_start_confirm_timeout());
            let mut active = inner.active.lock().expect("poisoned");
            let Some(entry) = active.get(&call_id) else {
                return; // already finished/cleaned up through the normal path
            };
            if entry.tap_confirmed {
                return;
            }
            log::warn!(
                "transcription: tap_start for call {call_id} was never confirmed (no \
                 tap_state:\"started\" within {:?}) - the call likely ended before the tap \
                 could attach; cleaning up the orphaned tap directory",
                tap_start_confirm_timeout()
            );
            let tap_dir = entry.ctx.tap_dir.clone();
            active.remove(&call_id);
            drop(active);
            let _ = std::fs::remove_dir_all(&tap_dir);
        });
    }

    fn on_tap_started(&self, call_id: &str) {
        let inner = self.0.clone();
        let call_id = call_id.to_string();
        std::thread::spawn(move || {
            let ctx = {
                let mut active = inner.active.lock().expect("poisoned");
                match active.get_mut(&call_id) {
                    Some(e) => {
                        e.tap_confirmed = true;
                        e.ctx.clone()
                    }
                    None => return, // not a call this handle armed (or already cleaned up - M1)
                }
            };
            if !on_tap_started_should_spawn(ctx.settings.mode) {
                return; // post-call mode starts its process at tap_stop, not tap_start
            }
            let Some(mut child) = spawn_transcribe_for(&inner, &ctx, "live") else {
                inner.active.lock().expect("poisoned").remove(&ctx.call_id);
                return;
            };
            let stdin = child.stdin.take();
            let mut send_stop_now = false;
            {
                let mut active = inner.active.lock().expect("poisoned");
                if let Some(entry) = active.get_mut(&call_id) {
                    entry.live_stdin = stdin;
                    send_stop_now = entry.coordination.mark_live_spawned();
                }
            }
            if send_stop_now {
                // tap_state:"stopped" already raced ahead of this spawn (a
                // very short call) - see TapCoordination's doc.
                let mut active = inner.active.lock().expect("poisoned");
                if let Some(entry) = active.get_mut(&call_id) {
                    if let Some(stdin) = entry.live_stdin.as_mut() {
                        let _ = writeln!(stdin, "stop");
                    }
                }
            }
            spawn_reader_and_finalize(inner.clone(), ctx, child);
        });
    }

    fn on_tap_stopped(&self, call_id: &str, event: &Value) {
        let inner = self.0.clone();
        let call_id = call_id.to_string();
        let rx_bytes = event.get("rx_bytes").and_then(Value::as_u64);
        let tx_bytes = event.get("tx_bytes").and_then(Value::as_u64);
        std::thread::spawn(move || {
            let run_post_call_ctx: Option<FinalizeContext> = {
                let mut active = inner.active.lock().expect("poisoned");
                let Some(entry) = active.get_mut(&call_id) else {
                    return; // not a call this handle armed
                };
                let live_already_spawned = entry.coordination.mark_stop_requested();
                if on_tap_stopped_should_run_post_call(entry.ctx.settings.mode, entry.live_died_early) {
                    active.remove(&call_id).map(|e| e.ctx)
                } else {
                    if live_already_spawned {
                        if let Some(stdin) = entry.live_stdin.as_mut() {
                            let _ = writeln!(stdin, "stop");
                        }
                    }
                    None
                }
            };
            log::info!("transcription: tap stopped for {call_id} (rx_bytes={rx_bytes:?} tx_bytes={tx_bytes:?})");
            let Some(ctx) = run_post_call_ctx else {
                // Live process still spawning/running - its own reader
                // thread (from on_tap_started) owns finalize, including
                // the A3 fallback if it turns out to have already died.
                return;
            };
            let Some(child) = spawn_transcribe_for(&inner, &ctx, "post") else {
                return;
            };
            spawn_reader_and_finalize(inner.clone(), ctx, child);
        });
    }
}

/// Pure extraction of `state`/`call_id` from a `call_state`/`tap_state`
/// event `Value` - shared by `on_call_state`/`on_tap_state`, and
/// unit-tested directly against malformed/partial event shapes without
/// needing a `TranscriptionHandle` at all (2026-07-16 review, finding A2).
fn event_state_and_call_id(event: &Value) -> Option<(&str, &str)> {
    let state = event.get("state").and_then(Value::as_str)?;
    let call_id = event.get("call_id").and_then(Value::as_str)?;
    Some((state, call_id))
}

/// Pure decision: should `on_call_state`'s `"established"` transition
/// start an automatic tap? Unit-tested directly (2026-07-16 review,
/// finding A2) - `mode == Off` always suppresses it; `Manual` activation
/// never auto-taps (only an explicit `transcription_manual_start` call
/// does); `AllCalls` does, unless a tap is already active for this call.
fn should_auto_tap(settings: &TranscriptionSettings, already_active: bool) -> bool {
    if already_active || settings.mode == TranscriptionMode::Off {
        return false;
    }
    settings.activation == TranscriptionActivation::AllCalls
}

/// Pure decision: does `on_tap_started` need to spawn a transcribe
/// process right now? Only `Live` mode does - `PostCall` waits for
/// `tap_state:"stopped"` (both WAVs finalized) before running its single
/// pass. Unit-tested directly (2026-07-16 review, finding A2).
fn on_tap_started_should_spawn(mode: TranscriptionMode) -> bool {
    mode == TranscriptionMode::Live
}

/// Pure decision: does `on_tap_stopped` need to run (or re-run) a
/// post-call transcribe pass right now? True for `PostCall` mode always
/// (that's its only trigger), and true for `Live` mode **only** if its
/// process already died early (`live_died_early` - see `ActiveEntry`'s
/// doc, A3 fix) - an on-track `Live` tap's own reader thread owns
/// finalize instead. Unit-tested directly (2026-07-16 review, finding
/// A2).
fn on_tap_stopped_should_run_post_call(mode: TranscriptionMode, live_died_early: bool) -> bool {
    mode != TranscriptionMode::Live || live_died_early
}

fn tap_start_confirm_timeout() -> Duration {
    std::env::var("CENTINELO_TAP_CONFIRM_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(8))
}

#[derive(Clone)]
struct PendingRetry {
    call_id: String,
    peer: String,
    tap_dir: PathBuf,
    rx_path: PathBuf,
    tx_path: PathBuf,
    /// Present when the engine finished (`done` was seen) but the final
    /// move into `storage_dir` failed - lets `retry` skip straight to
    /// re-attempting the move instead of re-running the whole engine
    /// (2026-07-16 review, finding B1).
    txt_path: Option<PathBuf>,
    json_path: Option<PathBuf>,
    started_at: DateTime<Local>,
    settings: TranscriptionSettings,
    last_error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingRetryView {
    pub call_id: String,
    pub peer: String,
    pub started_at: String,
    pub last_error: String,
}

impl From<&PendingRetry> for PendingRetryView {
    fn from(r: &PendingRetry) -> Self {
        Self {
            call_id: r.call_id.clone(),
            peer: r.peer.clone(),
            started_at: r.started_at.to_rfc3339(),
            last_error: r.last_error.clone(),
        }
    }
}

fn stash_pending_retry(
    inner: &Arc<Inner>,
    ctx: &FinalizeContext,
    txt_path: Option<PathBuf>,
    json_path: Option<PathBuf>,
    reason: String,
) {
    let retry = PendingRetry {
        call_id: ctx.call_id.clone(),
        peer: ctx.peer.clone(),
        tap_dir: ctx.tap_dir.clone(),
        rx_path: ctx.rx_path.clone(),
        tx_path: ctx.tx_path.clone(),
        txt_path,
        json_path,
        started_at: ctx.started_at,
        settings: ctx.settings.clone(),
        last_error: reason.clone(),
    };
    inner
        .pending_retries
        .lock()
        .expect("poisoned")
        .insert(ctx.call_id.clone(), retry);
    let _ = inner.app.emit(
        "transcription://error",
        serde_json::json!({"call_id": ctx.call_id, "message": reason, "retryable": true}),
    );
}

// ---------------------------------------------------------------------
// centinelo-transcribe process: spawn, parse, finalize
// ---------------------------------------------------------------------

struct TranscribeArgs {
    bin: PathBuf,
    rx: PathBuf,
    tx: PathBuf,
    model: PathBuf,
    lang: String,
    mode: &'static str, // "live" | "post"
    out_dir: PathBuf,
    /// A filesystem path, never inline JSON - see `write_meta_file`'s doc
    /// (2026-07-16 review, finding M5).
    meta_path: PathBuf,
}

fn spawn_transcribe(args: &TranscribeArgs) -> std::io::Result<Child> {
    Command::new(&args.bin)
        .arg("run")
        .arg("--rx")
        .arg(&args.rx)
        .arg("--tx")
        .arg(&args.tx)
        .arg("--model")
        .arg(&args.model)
        .arg("--lang")
        .arg(&args.lang)
        .arg("--mode")
        .arg(args.mode)
        .arg("--out-dir")
        .arg(&args.out_dir)
        .arg("--meta")
        .arg(&args.meta_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Writes this call's metadata to `<tap_dir>/meta.json` and returns its
/// path, to be passed as `--meta <path>` - **never** inline JSON on the
/// command line. 2026-07-16 review, finding M5: an earlier version of
/// this module passed `--meta '{"call_id":...,"peer":...}'` directly as
/// an argv value, which (unlike the SIP secret, confirmed never exposed
/// this way) put the *called/calling number* in argv - visible to any
/// other local user via `ps`/`/proc` for as long as the process runs.
/// `centinelo-transcribe run --meta` accepts a file path as a documented
/// fallback (it tries JSON-parsing the value first, then treats it as a
/// path) so this costs nothing on the receiving end. `tap_dir` is
/// already mode `0700` (owner-only - see `start_tap`), so this is no
/// less protected than the WAV files sitting right next to it.
///
/// Only `call_id`/`number`/`started_at` are populated - `extension`/
/// `direction`/`transport` (also accepted by `centinelo-transcribe`'s
/// `CallMeta`) aren't threaded through from `core/PROTOCOL.md`'s
/// `call_state` event to this module yet; all fields are `Option` on
/// their side so omitting them is a supported, not a degraded, shape.
fn write_meta_file(ctx: &FinalizeContext) -> Result<PathBuf, String> {
    let meta = serde_json::json!({
        "call_id": ctx.call_id,
        "number": ctx.peer,
        "started_at": ctx.started_at.to_rfc3339(),
    });
    let path = ctx.tap_dir.join("meta.json");
    std::fs::write(&path, meta.to_string()).map_err(|e| format!("could not write call metadata file: {e}"))?;
    Ok(path)
}

/// Resolves the binary, builds args (including writing the meta file),
/// and spawns the `run` process for `ctx` in `mode` (`"live"`/`"post"`).
/// On **any** failure (binary not found, meta file couldn't be written,
/// `spawn()` itself fails) this function itself logs, emits
/// `transcription://error`, and stashes a pending retry before returning
/// `None` - every caller only needs `let Some(child) = ... else { return
/// };`, no separate failure-handling branch of their own.
///
/// Extracted from what used to be three near-identical ~35-line blocks
/// (`on_tap_started`'s live spawn, `on_tap_stopped`'s post spawn,
/// `retry`'s respawn) - the triplication is exactly what let one of the
/// three (the live spawn's `resolve_transcribe_binary()` failure branch)
/// skip `stash_pending_retry` while its two siblings didn't (2026-07-16
/// review, finding A1 - caught independently by both the readability and
/// reliability lenses). A single shared function makes that class of
/// drift structurally impossible instead of a linting/review concern.
fn spawn_transcribe_for(inner: &Arc<Inner>, ctx: &FinalizeContext, mode: &'static str) -> Option<Child> {
    let bin = match resolve_transcribe_binary() {
        Ok(b) => b,
        Err(e) => {
            log::warn!("transcription: {e}");
            let _ = inner.app.emit(
                "transcription://error",
                serde_json::json!({"call_id": ctx.call_id, "message": e}),
            );
            stash_pending_retry(inner, ctx, None, None, e);
            return None;
        }
    };
    let meta_path = match write_meta_file(ctx) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("transcription: {e}");
            let _ = inner.app.emit(
                "transcription://error",
                serde_json::json!({"call_id": ctx.call_id, "message": e}),
            );
            stash_pending_retry(inner, ctx, None, None, e);
            return None;
        }
    };
    let model = model_path(&inner.app, ctx.settings.model_tier);
    let args = TranscribeArgs {
        bin,
        rx: ctx.rx_path.clone(),
        tx: ctx.tx_path.clone(),
        model,
        lang: ctx.settings.language.clone(),
        mode,
        out_dir: ctx.tap_dir.clone(),
        meta_path,
    };
    match spawn_transcribe(&args) {
        Ok(child) => Some(child),
        Err(e) => {
            log::warn!("transcription: failed to start {mode} transcribe for {}: {e}", ctx.call_id);
            let _ = inner.app.emit(
                "transcription://error",
                serde_json::json!({"call_id": ctx.call_id, "message": e.to_string()}),
            );
            stash_pending_retry(inner, ctx, None, None, e.to_string());
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum TranscribeLine {
    Segment { speaker: String, t0_ms: i64, t1_ms: i64, text: String },
    Done { txt_path: Option<String>, json_path: Option<String> },
    Error { message: String },
    Unknown,
}

/// Pure parser for one line of `run`'s stdout - see this module's doc for
/// the exact JSON shape (confirmed against transcribe-engine's real
/// implementation, 2026-07-16: `"type"` discriminator, `done`'s fields
/// are `txt`/`json`). Unit-tested without spawning any process (`mod
/// tests` below).
fn parse_transcribe_line(line: &str) -> Option<TranscribeLine> {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        return None; // matches sidecar.rs's own "ignore non-JSON noise" convention
    }
    let v: Value = serde_json::from_str(trimmed).ok()?;
    let ty = v.get("type").and_then(Value::as_str)?;
    Some(match ty {
        "segment" => TranscribeLine::Segment {
            speaker: v.get("speaker").and_then(Value::as_str).unwrap_or("").to_string(),
            t0_ms: v.get("t0_ms").and_then(Value::as_i64).unwrap_or(0),
            t1_ms: v.get("t1_ms").and_then(Value::as_i64).unwrap_or(0),
            text: v.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
        },
        "done" => TranscribeLine::Done {
            txt_path: v.get("txt").and_then(Value::as_str).map(str::to_string),
            json_path: v.get("json").and_then(Value::as_str).map(str::to_string),
        },
        "error" => TranscribeLine::Error {
            message: v.get("message").and_then(Value::as_str).unwrap_or("unknown error").to_string(),
        },
        _ => TranscribeLine::Unknown,
    })
}

/// Reads `child`'s stdout to completion, classifying each line, emitting
/// `transcription://segment`/`transcription://error` as they arrive, then
/// (on EOF) waits the process and decides what happened - see the A3
/// handling below for why "the process exited" isn't always "this call
/// is done". Runs on its own thread - the only thing that blocks on this
/// child's lifetime, matching `sidecar.rs`'s own
/// stdout-reader-thread-per-process style.
fn spawn_reader_and_finalize(inner: Arc<Inner>, ctx: FinalizeContext, mut child: Child) {
    std::thread::spawn(move || {
        if let Some(stderr) = child.stderr.take() {
            let call_id = ctx.call_id.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    log::debug!("transcribe[{call_id}]: {line}");
                }
            });
        }

        let mut done_txt: Option<PathBuf> = None;
        let mut done_json: Option<PathBuf> = None;
        let mut last_error: Option<String> = None;
        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                match parse_transcribe_line(&line) {
                    Some(TranscribeLine::Segment { speaker, t0_ms, t1_ms, text }) => {
                        let _ = inner.app.emit(
                            "transcription://segment",
                            serde_json::json!({
                                "call_id": ctx.call_id,
                                "speaker": speaker,
                                "t0_ms": t0_ms,
                                "t1_ms": t1_ms,
                                "text": text,
                            }),
                        );
                    }
                    Some(TranscribeLine::Done { txt_path, json_path }) => {
                        done_txt = txt_path.map(PathBuf::from);
                        done_json = json_path.map(PathBuf::from);
                    }
                    Some(TranscribeLine::Error { message }) => {
                        last_error = Some(message.clone());
                        let _ = inner.app.emit(
                            "transcription://error",
                            serde_json::json!({"call_id": ctx.call_id, "message": message}),
                        );
                    }
                    Some(TranscribeLine::Unknown) | None => {}
                }
            }
        }
        let _ = child.wait();

        // A3 (2026-07-16 review): distinguish "this exit is expected"
        // (post-call: always expected - it only ever runs once, after
        // tap_stop; live: only expected once tap_state:"stopped" told
        // this call to stop) from "a live process died on its own while
        // the call may still be active". An entry missing from `active`
        // entirely only happens on the post-call path (on_tap_stopped
        // already removed it before spawning this reader) - treat that
        // as "yes, expected" so post-call finalize behaves exactly as an
        // always-expected exit.
        let stop_was_requested = {
            let active = inner.active.lock().expect("poisoned");
            active
                .get(&ctx.call_id)
                .map(|e| e.coordination.stop_requested())
                .unwrap_or(true)
        };

        if !stop_was_requested {
            log::warn!(
                "transcription: live engine for call {} exited before the call ended - \
                 will run a full pass once the call actually hangs up",
                ctx.call_id
            );
            let mut active = inner.active.lock().expect("poisoned");
            if let Some(entry) = active.get_mut(&ctx.call_id) {
                entry.live_stdin = None;
                entry.live_died_early = true;
            }
            drop(active);
            let _ = inner.app.emit(
                "transcription://error",
                serde_json::json!({
                    "call_id": ctx.call_id,
                    "message": "Live transcription stopped unexpectedly; will retry once the call ends.",
                    "retryable": false,
                }),
            );
            return; // do NOT remove from active, do NOT stash a terminal retry
        }

        inner.active.lock().expect("poisoned").remove(&ctx.call_id);

        match (done_txt, done_json) {
            (Some(txt), Some(json)) => match finalize_artifacts(&ctx, &txt, &json) {
                Ok(result) => {
                    let _ = inner.app.emit(
                        "transcription://done",
                        serde_json::json!({
                            "call_id": ctx.call_id,
                            "txt_path": result.txt.display().to_string(),
                            "json_path": result.json.display().to_string(),
                            "audio_kept": result.audio_kept,
                        }),
                    );
                }
                Err(e) => {
                    log::warn!("transcription: could not finalize artifacts for {}: {e}", ctx.call_id);
                    stash_pending_retry(&inner, &ctx, Some(txt), Some(json), e);
                }
            },
            _ => {
                let reason = last_error.unwrap_or_else(|| {
                    "transcription engine exited without a done event".to_string()
                });
                log::warn!("transcription: {reason} (call {})", ctx.call_id);
                stash_pending_retry(&inner, &ctx, None, None, reason);
            }
        }
    });
}

struct FinalizedPaths {
    txt: PathBuf,
    json: PathBuf,
    audio_kept: bool,
}

/// Moves the finished transcript (`txt_path`/`json_path`, as reported by
/// the sidecar's `done` event) into `storage_dir/YYYY/MM/DD/`, moves or
/// deletes the tap WAVs per `keep_audio`, and removes the temp tap
/// directory. Pure filesystem logic (no `AppHandle`) - unit-tested
/// directly (`mod tests` below) with real temp directories and synthetic
/// files, no PHI, no real call audio.
///
/// # `view_only`
///
/// `storage_dir` is this shell's one configured destination whether it's
/// a local path or an SMB-mounted NAS share (`TranscriptionSettings::
/// storage_dir`'s own doc) - both `view_only` on and off write there and
/// nowhere else; this function doesn't branch on it. What `view_only`
/// changes (per the task spec, "persiste SOLO al storage_dir remoto, nada
/// local") is the *operational contract* enforced one layer up: the temp
/// tap directory this function always removes on success is the only
/// "local" artifact that ever exists, and it's already always cleaned up
/// here regardless of `view_only` - so the guarantee holds by
/// construction rather than needing a separate code path. The one
/// deliberate exception is the failure path (`stash_pending_retry`),
/// which keeps the temp WAVs on purpose even in `view_only` mode, per the
/// task spec's own "no perder los WAVs (reintento manual posible)" -
/// losing audio on a transient NAS blip is worse than a temp file
/// surviving until a manual retry. Flagged in the shell report for
/// ola-2/consolidation to confirm this reading matches product intent.
fn finalize_artifacts(ctx: &FinalizeContext, txt_path: &Path, json_path: &Path) -> Result<FinalizedPaths, String> {
    let storage_dir = ctx.settings.storage_dir.trim();
    if storage_dir.is_empty() {
        return Err("storage_dir is not configured".to_string());
    }
    let dest_dir = dated_dest_dir(Path::new(storage_dir), ctx.started_at);
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("could not write to storage_dir ({}): {e}", dest_dir.display()))?;

    let base = format!("{}-{}", ctx.started_at.format("%H%M%S"), ctx.call_id);
    let dest_txt = dest_dir.join(format!("{base}.txt"));
    let dest_json = dest_dir.join(format!("{base}.json"));
    move_file(txt_path, &dest_txt)?;
    move_file(json_path, &dest_json)?;

    let audio_kept = ctx.settings.keep_audio;
    if audio_kept {
        if ctx.rx_path.is_file() {
            move_file(&ctx.rx_path, &dest_dir.join(format!("{base}-rx.wav")))?;
        }
        if ctx.tx_path.is_file() {
            move_file(&ctx.tx_path, &dest_dir.join(format!("{base}-tx.wav")))?;
        }
    } else {
        let _ = std::fs::remove_file(&ctx.rx_path);
        let _ = std::fs::remove_file(&ctx.tx_path);
    }

    // Should be empty now (both WAVs moved/deleted, txt/json moved out) -
    // remove_dir_all rather than the strict-empty remove_dir so a stray
    // file from a future engine version doesn't leave permanent litter in
    // the OS temp dir.
    let _ = std::fs::remove_dir_all(&ctx.tap_dir);

    Ok(FinalizedPaths { txt: dest_txt, json: dest_json, audio_kept })
}

fn dated_dest_dir(storage_dir: &Path, started_at: DateTime<Local>) -> PathBuf {
    storage_dir
        .join(started_at.format("%Y").to_string())
        .join(started_at.format("%m").to_string())
        .join(started_at.format("%d").to_string())
}

/// Appends `suffix` to a path's file name (not its extension - `.part`
/// stays a literal suffix on the whole name, e.g. `foo.txt` ->
/// `foo.txt.part`, never `foo.part`).
fn append_to_filename(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// `rename()` fails across filesystems/mount points (temp dir on one
/// volume, a mounted NAS share on another) - falls back to copy+rename,
/// **never copy-directly-to-`dest`** (2026-07-16 review, finding M3): the
/// earlier version copied straight to the final `dest` path, which would
/// leave a corrupt, indistinguishable-from-good file at that exact name
/// if the copy was interrupted partway (NAS blip, disk full mid-write).
/// Copies to `<dest>.part` in the same directory first, and only
/// `rename()`s it to `dest` on full success - the same atomic-publish
/// pattern `centinelo-transcribe`'s own `model_manager.rs` uses for
/// model downloads, and `rename()` within one directory is atomic on
/// every filesystem this shell targets.
fn move_file(src: &Path, dest: &Path) -> Result<(), String> {
    if std::fs::rename(src, dest).is_ok() {
        return Ok(());
    }
    let tmp_dest = append_to_filename(dest, ".part");
    std::fs::copy(src, &tmp_dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_dest);
        format!("could not copy {} -> {}: {e}", src.display(), tmp_dest.display())
    })?;
    std::fs::rename(&tmp_dest, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_dest);
        format!("copied but could not publish {} -> {}: {e}", tmp_dest.display(), dest.display())
    })?;
    std::fs::remove_file(src)
        .map_err(|e| format!("published but could not remove source {}: {e}", src.display()))?;
    Ok(())
}

/// Resolves the `centinelo-transcribe` binary: `CENTINELO_TRANSCRIBE_BIN`
/// override first (also how tests point this at the mocked script), then
/// next to this executable - same two-step shape as `sidecar.rs`'s
/// `resolve_core_binary`, minus that function's dev-convenience walk-up
/// search (the transcribe crate lives in the private `premium/` repo, a
/// sibling of `phone/` rather than nested under it, so there's no single
/// relative path to walk up to across every dev machine's checkout).
fn resolve_transcribe_binary() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("CENTINELO_TRANSCRIBE_BIN") {
        let pb = PathBuf::from(&p);
        return if pb.is_file() {
            Ok(pb)
        } else {
            Err(format!("CENTINELO_TRANSCRIBE_BIN is set but not a file: {p}"))
        };
    }
    let bin_name = if cfg!(windows) {
        "centinelo-transcribe.exe"
    } else {
        "centinelo-transcribe"
    };
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        let candidate = dir.join(bin_name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(
        "Transcription engine binary not found. Set CENTINELO_TRANSCRIBE_BIN, or place \
         centinelo-transcribe next to the app executable."
            .to_string(),
    )
}

// ---------------------------------------------------------------------
// Model tiers + on-demand download (F4 item 5)
// ---------------------------------------------------------------------

/// ggml filename per tier - matches `centinelo-transcribe`'s own
/// `model_manager::ModelTier::default_path` naming exactly (confirmed by
/// reading that module, `premium` repo `feature/transcribe-e2e`,
/// read-only), so `transcription_model_status`'s file-presence check
/// looks at the same path `ensure_model`/`run --model` actually use.
pub fn model_filename(tier: ModelTier) -> &'static str {
    match tier {
        ModelTier::Accurate => "ggml-large-v3-turbo-q5_0.bin",
        ModelTier::Light => "ggml-small-q5_1.bin",
    }
}

/// The `--tier` value `centinelo-transcribe ensure-model` expects -
/// distinct from [`model_filename`] (a `.bin` file name), confirmed
/// against that binary's real `ModelTier::as_str`/`parse`.
fn tier_cli_name(tier: ModelTier) -> &'static str {
    match tier {
        ModelTier::Accurate => "large-v3-turbo-q5_0",
        ModelTier::Light => "small-q5_1",
    }
}

fn model_dir(app: &AppHandle) -> PathBuf {
    if let Ok(p) = std::env::var("CENTINELO_MODEL_DIR") {
        return PathBuf::from(p);
    }
    app.path()
        .app_data_dir()
        .map(|d| d.join("models"))
        .unwrap_or_else(|_| std::env::temp_dir().join("centinelo-models"))
}

pub fn model_path(app: &AppHandle, tier: ModelTier) -> PathBuf {
    model_dir(app).join(model_filename(tier))
}

#[derive(Debug, Clone, PartialEq)]
enum EnsureModelLine {
    Progress { asset: String, downloaded: u64, total: u64 },
    Ready { model: String },
    Error { message: String },
    Unknown,
}

/// Pure parser for `ensure-model`'s stdout, mirroring
/// [`parse_transcribe_line`] for `run` - same `"type"` discriminator
/// convention, different variant set (confirmed against
/// `centinelo-transcribe`'s real `main.rs`, `premium` repo
/// `feature/transcribe-e2e`, read-only).
fn parse_ensure_model_line(line: &str) -> Option<EnsureModelLine> {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        return None;
    }
    let v: Value = serde_json::from_str(trimmed).ok()?;
    let ty = v.get("type").and_then(Value::as_str)?;
    Some(match ty {
        "progress" => EnsureModelLine::Progress {
            asset: v.get("asset").and_then(Value::as_str).unwrap_or("").to_string(),
            downloaded: v.get("downloaded").and_then(Value::as_u64).unwrap_or(0),
            total: v.get("total").and_then(Value::as_u64).unwrap_or(0),
        },
        "ready" => EnsureModelLine::Ready {
            model: v.get("model").and_then(Value::as_str).unwrap_or("").to_string(),
        },
        "error" => EnsureModelLine::Error {
            message: v.get("message").and_then(Value::as_str).unwrap_or("unknown error").to_string(),
        },
        _ => EnsureModelLine::Unknown,
    })
}

fn spawn_ensure_model(bin: &Path, tier: ModelTier, models_dir: &Path) -> std::io::Result<Child> {
    Command::new(bin)
        .arg("ensure-model")
        .arg("--tier")
        .arg(tier_cli_name(tier))
        .arg("--models-dir")
        .arg(models_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Ensures `tier`'s model (+ its VAD companion) is present and
/// checksum-verified, by shelling out to `centinelo-transcribe
/// ensure-model` and streaming its progress - per task item 5, "descarga
/// con progreso + checksum".
///
/// # Consolidation, not reimplementation (2026-07-16 review, finding M4)
///
/// An earlier version of this function downloaded the model file
/// directly (`ureq` + `sha2`, streamed to a `.part` file) with an
/// **unpinned** checksum (`model_expected_sha256` always returned `None`,
/// making the rejection branch unreachable in practice) and no
/// validation that the full `Content-Length` was actually received
/// before accepting the file. `centinelo-transcribe`'s own
/// `model_manager.rs` already does this correctly (pinned SHA256s pulled
/// from HuggingFace's own LFS metadata, `lfs.oid`, atomic
/// `.part`-then-rename publish, no silent partial-download acceptance),
/// so duplicating that logic here, even with real hashes copied in,
/// would create a second copy that drifts the moment either side updates
/// a pinned hash. Shelling out to the one real implementation, the way
/// `run` already does for transcription itself, is what "consolidate
/// into one source" actually means here.
pub fn ensure_model(app: &AppHandle, tier: ModelTier) -> Result<PathBuf, String> {
    let bin = resolve_transcribe_binary()?;
    let models_dir = model_dir(app);
    std::fs::create_dir_all(&models_dir).map_err(|e| e.to_string())?;

    let mut child = spawn_ensure_model(&bin, tier, &models_dir).map_err(|e| e.to_string())?;
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                log::debug!("ensure-model: {line}");
            }
        });
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ensure-model: no stdout pipe".to_string())?;
    let reader = BufReader::new(stdout);
    let mut ready_model: Option<PathBuf> = None;
    let mut last_error: Option<String> = None;
    let tier_name = tier_cli_name(tier);
    for line in reader.lines().map_while(Result::ok) {
        match parse_ensure_model_line(&line) {
            Some(EnsureModelLine::Progress { asset, downloaded, total }) => {
                let _ = app.emit(
                    "transcription://model-download-progress",
                    serde_json::json!({
                        "tier": tier_name,
                        "asset": asset,
                        "downloaded_bytes": downloaded,
                        "total_bytes": total,
                    }),
                );
            }
            Some(EnsureModelLine::Ready { model }) => {
                ready_model = Some(PathBuf::from(model));
            }
            Some(EnsureModelLine::Error { message }) => {
                last_error = Some(message);
            }
            Some(EnsureModelLine::Unknown) | None => {}
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    match ready_model {
        Some(path) if status.success() => Ok(path),
        _ => Err(last_error.unwrap_or_else(|| "ensure-model exited without a ready event".to_string())),
    }
}

/// Spawns [`ensure_model`] on its own thread and emits
/// `transcription://model-download-done`/`-error` when it finishes - see
/// `commands::download_transcription_model`. Blocking work
/// (`ensure_model` waits on a whole child process) always happens off
/// the Tauri command's own calling thread, matching this crate's
/// existing thread-per-blocking-operation style (`bridge.rs`'s HTTP
/// server, `sidecar.rs`'s supervisor).
pub fn spawn_model_download(app: AppHandle, tier: ModelTier) {
    std::thread::spawn(move || match ensure_model(&app, tier) {
        Ok(path) => {
            let _ = app.emit(
                "transcription://model-download-done",
                serde_json::json!({"tier": tier_cli_name(tier), "path": path.display().to_string()}),
            );
        }
        Err(e) => {
            let _ = app.emit(
                "transcription://model-download-error",
                serde_json::json!({"tier": tier_cli_name(tier), "message": e}),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- valid_call_id (S1) ----

    #[test]
    fn valid_call_id_accepts_realistic_engine_ids() {
        assert!(valid_call_id("8832d603f43c4fd3"));
        assert!(valid_call_id("2845d09c"));
        assert!(valid_call_id("a-b_c.d123"));
    }

    #[test]
    fn valid_call_id_rejects_path_traversal_attempts() {
        assert!(!valid_call_id("../../../etc/passwd"));
        assert!(!valid_call_id("..\\..\\windows"));
        assert!(!valid_call_id(".."));
        assert!(!valid_call_id("a/b"));
        assert!(!valid_call_id("a\\b"));
        assert!(!valid_call_id("foo/../bar"));
    }

    #[test]
    fn valid_call_id_rejects_empty_and_oversized() {
        assert!(!valid_call_id(""));
        assert!(!valid_call_id(&"a".repeat(MAX_CALL_ID_LEN + 1)));
        assert!(valid_call_id(&"a".repeat(MAX_CALL_ID_LEN)));
    }

    #[test]
    fn valid_call_id_rejects_other_special_characters() {
        assert!(!valid_call_id("a b")); // space
        assert!(!valid_call_id("a;rm -rf /"));
        assert!(!valid_call_id("a\0b")); // NUL
        assert!(!valid_call_id("a\nb"));
    }

    // ---- event_state_and_call_id (A2) ----

    #[test]
    fn event_state_and_call_id_extracts_both_fields() {
        let event = serde_json::json!({"event": "call_state", "state": "established", "call_id": "abc123"});
        assert_eq!(event_state_and_call_id(&event), Some(("established", "abc123")));
    }

    #[test]
    fn event_state_and_call_id_none_when_state_missing() {
        let event = serde_json::json!({"call_id": "abc123"});
        assert_eq!(event_state_and_call_id(&event), None);
    }

    #[test]
    fn event_state_and_call_id_none_when_call_id_missing() {
        let event = serde_json::json!({"state": "established"});
        assert_eq!(event_state_and_call_id(&event), None);
    }

    // ---- should_auto_tap (A2) ----

    fn settings_with(mode: TranscriptionMode, activation: TranscriptionActivation) -> TranscriptionSettings {
        TranscriptionSettings {
            mode,
            activation,
            storage_dir: "/tmp/whatever".to_string(),
            ..TranscriptionSettings::default()
        }
    }

    #[test]
    fn should_auto_tap_false_when_mode_off_regardless_of_activation() {
        assert!(!should_auto_tap(
            &settings_with(TranscriptionMode::Off, TranscriptionActivation::AllCalls),
            false
        ));
        assert!(!should_auto_tap(
            &settings_with(TranscriptionMode::Off, TranscriptionActivation::Manual),
            false
        ));
    }

    #[test]
    fn should_auto_tap_false_for_manual_activation_even_when_mode_on() {
        assert!(!should_auto_tap(
            &settings_with(TranscriptionMode::Live, TranscriptionActivation::Manual),
            false
        ));
        assert!(!should_auto_tap(
            &settings_with(TranscriptionMode::PostCall, TranscriptionActivation::Manual),
            false
        ));
    }

    #[test]
    fn should_auto_tap_true_for_all_calls_activation_when_mode_on() {
        assert!(should_auto_tap(
            &settings_with(TranscriptionMode::Live, TranscriptionActivation::AllCalls),
            false
        ));
        assert!(should_auto_tap(
            &settings_with(TranscriptionMode::PostCall, TranscriptionActivation::AllCalls),
            false
        ));
    }

    #[test]
    fn should_auto_tap_false_when_already_active() {
        assert!(!should_auto_tap(
            &settings_with(TranscriptionMode::Live, TranscriptionActivation::AllCalls),
            true
        ));
    }

    // ---- on_tap_started/on_tap_stopped decisions (A2) ----

    #[test]
    fn on_tap_started_spawns_only_for_live() {
        assert!(on_tap_started_should_spawn(TranscriptionMode::Live));
        assert!(!on_tap_started_should_spawn(TranscriptionMode::PostCall));
        assert!(!on_tap_started_should_spawn(TranscriptionMode::Off));
    }

    #[test]
    fn on_tap_stopped_runs_post_call_for_post_call_mode() {
        assert!(on_tap_stopped_should_run_post_call(TranscriptionMode::PostCall, false));
        assert!(on_tap_stopped_should_run_post_call(TranscriptionMode::PostCall, true));
    }

    #[test]
    fn on_tap_stopped_defers_to_live_reader_when_still_alive() {
        assert!(!on_tap_stopped_should_run_post_call(TranscriptionMode::Live, false));
    }

    #[test]
    fn on_tap_stopped_falls_back_to_post_call_when_live_died_early() {
        // A3 fix: a live process that already crashed before this stop
        // event must still get a full pass once the call really ends.
        assert!(on_tap_stopped_should_run_post_call(TranscriptionMode::Live, true));
    }

    // ---- TapCoordination race (A2) - both possible orderings ----

    #[test]
    fn coordination_started_then_stopped_signals_stop_on_the_stop_call() {
        let mut coord = TapCoordination::new();
        let send_stop_on_attach = coord.mark_live_spawned();
        assert!(!send_stop_on_attach, "no stop requested yet - nothing to send immediately");
        let live_already_spawned = coord.mark_stop_requested();
        assert!(live_already_spawned, "live was already spawned - caller should write stop now");
    }

    #[test]
    fn coordination_stopped_then_started_signals_stop_on_the_start_call() {
        let mut coord = TapCoordination::new();
        let live_already_spawned = coord.mark_stop_requested();
        assert!(!live_already_spawned, "live not spawned yet - nothing to write to yet");
        let send_stop_on_attach = coord.mark_live_spawned();
        assert!(send_stop_on_attach, "stop was already requested - caller should send it now on attach");
    }

    #[test]
    fn coordination_started_without_stop_never_signals() {
        let mut coord = TapCoordination::new();
        let send_stop_on_attach = coord.mark_live_spawned();
        assert!(!send_stop_on_attach);
        assert!(!coord.stop_requested());
    }

    // ---- parse_transcribe_line ----

    #[test]
    fn parses_segment_line() {
        let line = r#"{"type":"segment","speaker":"agent","t0_ms":100,"t1_ms":900,"text":"hola"}"#;
        assert_eq!(
            parse_transcribe_line(line),
            Some(TranscribeLine::Segment {
                speaker: "agent".to_string(),
                t0_ms: 100,
                t1_ms: 900,
                text: "hola".to_string(),
            })
        );
    }

    #[test]
    fn parses_done_line() {
        let line = r#"{"type":"done","txt":"/tmp/a.txt","json":"/tmp/a.json"}"#;
        assert_eq!(
            parse_transcribe_line(line),
            Some(TranscribeLine::Done {
                txt_path: Some("/tmp/a.txt".to_string()),
                json_path: Some("/tmp/a.json".to_string()),
            })
        );
    }

    #[test]
    fn parses_error_line() {
        let line = r#"{"type":"error","message":"model not found"}"#;
        assert_eq!(
            parse_transcribe_line(line),
            Some(TranscribeLine::Error { message: "model not found".to_string() })
        );
    }

    #[test]
    fn ignores_non_json_noise() {
        assert_eq!(parse_transcribe_line("loading model..."), None);
        assert_eq!(parse_transcribe_line(""), None);
        assert_eq!(parse_transcribe_line("   "), None);
    }

    #[test]
    fn unknown_type_is_unknown_not_none() {
        assert_eq!(parse_transcribe_line(r#"{"type":"progress","pct":50}"#), Some(TranscribeLine::Unknown));
    }

    #[test]
    fn malformed_json_is_none() {
        assert_eq!(parse_transcribe_line("{not json"), None);
    }

    #[test]
    fn old_event_key_is_no_longer_recognized() {
        // Guards against silently regressing back to the earlier, wrong
        // guessed contract (`"event"` key, `txt_path`/`json_path` fields)
        // now that it's confirmed against the real binary.
        assert_eq!(parse_transcribe_line(r#"{"event":"segment","speaker":"agent","t0_ms":0,"t1_ms":1,"text":"x"}"#), None);
    }

    // ---- parse_ensure_model_line ----

    #[test]
    fn parses_ensure_model_progress_line() {
        let line = r#"{"type":"progress","asset":"ggml-large-v3-turbo-q5_0.bin","downloaded":1024,"total":574041195}"#;
        assert_eq!(
            parse_ensure_model_line(line),
            Some(EnsureModelLine::Progress {
                asset: "ggml-large-v3-turbo-q5_0.bin".to_string(),
                downloaded: 1024,
                total: 574041195,
            })
        );
    }

    #[test]
    fn parses_ensure_model_ready_line() {
        let line = r#"{"type":"ready","model":"/models/ggml-large-v3-turbo-q5_0.bin","vad_model":"/models/ggml-silero-v5.1.2.bin"}"#;
        assert_eq!(
            parse_ensure_model_line(line),
            Some(EnsureModelLine::Ready { model: "/models/ggml-large-v3-turbo-q5_0.bin".to_string() })
        );
    }

    #[test]
    fn parses_ensure_model_error_line() {
        let line = r#"{"type":"error","message":"checksum mismatch"}"#;
        assert_eq!(
            parse_ensure_model_line(line),
            Some(EnsureModelLine::Error { message: "checksum mismatch".to_string() })
        );
    }

    // ---- dated_dest_dir ----

    #[test]
    fn dated_dest_dir_builds_ymd_layout() {
        use chrono::TimeZone;
        let dt = Local.with_ymd_and_hms(2026, 7, 16, 14, 32, 10).unwrap();
        let dir = dated_dest_dir(Path::new("/storage"), dt);
        assert_eq!(dir, PathBuf::from("/storage/2026/07/16"));
    }

    // ---- append_to_filename / move_file atomicity (M3) ----

    #[test]
    fn append_to_filename_suffixes_whole_name() {
        assert_eq!(append_to_filename(Path::new("/a/b/foo.txt"), ".part"), PathBuf::from("/a/b/foo.txt.part"));
    }

    #[test]
    fn move_file_publishes_atomically_via_part_file() {
        let dir = scratch_dir("move-file-atomic");
        let src = dir.join("src.txt");
        let dest_dir = dir.join("cross-device-sim");
        std::fs::create_dir_all(&dest_dir).unwrap();
        std::fs::write(&src, b"hello").unwrap();
        let dest = dest_dir.join("dest.txt");

        move_file(&src, &dest).unwrap();

        assert!(dest.is_file());
        assert!(!src.exists());
        // No leftover .part file after a successful move.
        assert!(!dest_dir.join("dest.txt.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_file_never_leaves_a_partial_file_at_the_final_name() {
        // Simulate a copy failure (source path with no read permission is
        // hard to construct portably in a test - instead point `dest` at
        // a directory that doesn't exist and won't be created, so the
        // copy step itself fails cleanly) and confirm nothing appears at
        // the exact final `dest` path.
        let dir = scratch_dir("move-file-failure");
        let src = dir.join("src.txt");
        std::fs::write(&src, b"hello").unwrap();
        let dest = dir.join("does-not-exist").join("dest.txt");

        let result = move_file(&src, &dest);
        assert!(result.is_err());
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- resolve_transcribe_binary ----

    #[test]
    fn resolve_binary_honors_env_override() {
        let tmp = std::env::temp_dir().join(format!("centinelo-test-bin-{}", std::process::id()));
        std::fs::write(&tmp, b"#!/bin/sh\n").unwrap();
        std::env::set_var("CENTINELO_TRANSCRIBE_BIN", &tmp);
        let resolved = resolve_transcribe_binary();
        std::env::remove_var("CENTINELO_TRANSCRIBE_BIN");
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(resolved.unwrap(), tmp);
    }

    #[test]
    fn resolve_binary_env_override_missing_file_errors() {
        std::env::set_var("CENTINELO_TRANSCRIBE_BIN", "/no/such/binary/here");
        let resolved = resolve_transcribe_binary();
        std::env::remove_var("CENTINELO_TRANSCRIBE_BIN");
        assert!(resolved.is_err());
    }

    // ---- finalize_artifacts (real filesystem, synthetic data, no PHI) ----

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("centinelo-transcription-test.{name}.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_ctx(tap_dir: &Path, storage_dir: &Path, keep_audio: bool) -> FinalizeContext {
        use chrono::TimeZone;
        FinalizeContext {
            call_id: "call-abc123".to_string(),
            // Synthetic, never the real PBX test extension (repo is
            // public - see the 2026-07-16 review, finding B3).
            peer: "sip:9999@example.test".to_string(),
            tap_dir: tap_dir.to_path_buf(),
            rx_path: tap_dir.join("call-abc123-rx.wav"),
            tx_path: tap_dir.join("call-abc123-tx.wav"),
            started_at: Local.with_ymd_and_hms(2026, 7, 16, 9, 5, 0).unwrap(),
            settings: TranscriptionSettings {
                storage_dir: storage_dir.display().to_string(),
                keep_audio,
                ..TranscriptionSettings::default()
            },
        }
    }

    #[test]
    fn finalize_moves_transcript_into_dated_dir_and_deletes_wavs_by_default() {
        let tap_dir = scratch_dir("finalize-default");
        let storage_dir = scratch_dir("finalize-default-storage");
        let ctx = make_ctx(&tap_dir, &storage_dir, false);
        std::fs::write(&ctx.rx_path, b"RIFF....WAVEfmt fake pcm").unwrap();
        std::fs::write(&ctx.tx_path, b"RIFF....WAVEfmt fake pcm").unwrap();
        let txt_path = tap_dir.join("transcript.txt");
        let json_path = tap_dir.join("transcript.json");
        std::fs::write(&txt_path, "[00:00] Agent: hello\n").unwrap();
        std::fs::write(&json_path, "{}").unwrap();

        let result = finalize_artifacts(&ctx, &txt_path, &json_path).expect("finalize should succeed");

        assert!(result.txt.exists());
        assert!(result.json.exists());
        assert!(!result.audio_kept);
        assert!(result.txt.starts_with(storage_dir.join("2026/07/16")));
        assert!(!ctx.rx_path.exists());
        assert!(!ctx.tx_path.exists());
        assert!(!tap_dir.exists(), "temp tap dir should be fully cleaned up");

        let _ = std::fs::remove_dir_all(&storage_dir);
    }

    #[test]
    fn finalize_keeps_audio_when_keep_audio_is_set() {
        let tap_dir = scratch_dir("finalize-keep-audio");
        let storage_dir = scratch_dir("finalize-keep-audio-storage");
        let ctx = make_ctx(&tap_dir, &storage_dir, true);
        std::fs::write(&ctx.rx_path, b"fake-rx").unwrap();
        std::fs::write(&ctx.tx_path, b"fake-tx").unwrap();
        let txt_path = tap_dir.join("transcript.txt");
        let json_path = tap_dir.join("transcript.json");
        std::fs::write(&txt_path, "text").unwrap();
        std::fs::write(&json_path, "{}").unwrap();

        let result = finalize_artifacts(&ctx, &txt_path, &json_path).expect("finalize should succeed");
        assert!(result.audio_kept);
        let dest_dir = storage_dir.join("2026/07/16");
        let rx_entries: Vec<_> = std::fs::read_dir(&dest_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with("-rx.wav"))
            .collect();
        assert_eq!(rx_entries.len(), 1, "rx WAV should have been moved into the dated dir");

        let _ = std::fs::remove_dir_all(&storage_dir);
    }

    #[test]
    fn finalize_fails_and_keeps_wavs_when_storage_dir_unconfigured() {
        let tap_dir = scratch_dir("finalize-no-storage");
        let ctx = make_ctx(&tap_dir, Path::new(""), false);
        std::fs::write(&ctx.rx_path, b"fake").unwrap();
        std::fs::write(&ctx.tx_path, b"fake").unwrap();
        let txt_path = tap_dir.join("transcript.txt");
        let json_path = tap_dir.join("transcript.json");
        std::fs::write(&txt_path, "text").unwrap();
        std::fs::write(&json_path, "{}").unwrap();

        let result = finalize_artifacts(&ctx, &txt_path, &json_path);
        assert!(result.is_err());
        // Per this fn's own doc: a failure never deletes the WAVs -
        // they're the thing a manual retry needs.
        assert!(ctx.rx_path.exists());
        assert!(ctx.tx_path.exists());

        let _ = std::fs::remove_dir_all(&tap_dir);
    }

    #[test]
    fn finalize_fails_when_storage_dir_is_unwritable() {
        // A file (not a directory) as the storage_dir - create_dir_all
        // underneath it must fail, simulating an unreachable/misconfigured
        // NAS path without needing an actual network mount in CI.
        let parent = scratch_dir("finalize-unwritable-parent");
        let blocking_file = parent.join("not-a-directory");
        std::fs::write(&blocking_file, b"x").unwrap();
        let tap_dir = scratch_dir("finalize-unwritable-tap");
        let ctx = make_ctx(&tap_dir, &blocking_file, false);
        std::fs::write(&ctx.rx_path, b"fake").unwrap();
        std::fs::write(&ctx.tx_path, b"fake").unwrap();
        let txt_path = tap_dir.join("transcript.txt");
        let json_path = tap_dir.join("transcript.json");
        std::fs::write(&txt_path, "text").unwrap();
        std::fs::write(&json_path, "{}").unwrap();

        let result = finalize_artifacts(&ctx, &txt_path, &json_path);
        assert!(result.is_err());
        assert!(ctx.rx_path.exists());

        let _ = std::fs::remove_dir_all(&parent);
        let _ = std::fs::remove_dir_all(&tap_dir);
    }

    // ---- write_meta_file (M5) ----

    #[test]
    fn write_meta_file_writes_json_under_tap_dir_not_argv() {
        let tap_dir = scratch_dir("meta-file");
        let ctx = make_ctx(&tap_dir, Path::new("/tmp/storage"), false);
        let path = write_meta_file(&ctx).expect("should write meta file");
        assert_eq!(path, tap_dir.join("meta.json"));
        let contents = std::fs::read_to_string(&path).unwrap();
        let v: Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(v["call_id"], "call-abc123");
        assert_eq!(v["number"], "sip:9999@example.test");
        assert!(v["started_at"].is_string());
        let _ = std::fs::remove_dir_all(&tap_dir);
    }

    // ---- model tiers ----

    #[test]
    fn model_filenames_are_distinct() {
        assert_ne!(model_filename(ModelTier::Accurate), model_filename(ModelTier::Light));
    }

    #[test]
    fn tier_cli_names_match_the_real_binarys_parse_function() {
        // Confirmed against `centinelo-transcribe`'s real
        // `ModelTier::as_str`/`parse` (premium repo, read-only) - this
        // test pins the exact strings so a future drift is a loud test
        // failure, not a silent "unknown --tier" error from the sidecar.
        assert_eq!(tier_cli_name(ModelTier::Accurate), "large-v3-turbo-q5_0");
        assert_eq!(tier_cli_name(ModelTier::Light), "small-q5_1");
    }

    // ---- mocked-sidecar process integration tests -----------------------
    //
    // These spawn the real `tests/fixtures/mock-transcribe.sh` script as a
    // real child process through the same `spawn_transcribe`/
    // `parse_transcribe_line` code the live app uses - the "e2e scripted
    // flujo con sidecar mockeado" the task asked for, without needing a
    // running Tauri app (no AppHandle involved - only the process
    // spawn/pipe/parse layer). Unix-only: the fixture is a bash script
    // (see its own header) and this repo's Windows CI is already
    // best-effort/continue-on-error (shell/README.md "Known limitations").

    #[cfg(unix)]
    fn mock_binary_path() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/mock-transcribe.sh"))
    }

    #[cfg(unix)]
    fn write_test_meta(out_dir: &Path) -> PathBuf {
        let path = out_dir.join("meta.json");
        std::fs::write(&path, r#"{"call_id":"call-abc123","number":"sip:9999@example.test"}"#).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn mock_binary_post_mode_emits_segments_then_done_with_real_files() {
        let out_dir = scratch_dir("mock-post");
        let meta_path = write_test_meta(&out_dir);
        let args = TranscribeArgs {
            bin: mock_binary_path(),
            rx: out_dir.join("call-rx.wav"),
            tx: out_dir.join("call-tx.wav"),
            model: PathBuf::from("/dev/null"),
            lang: "es".to_string(),
            mode: "post",
            out_dir: out_dir.clone(),
            meta_path,
        };
        let mut child = spawn_transcribe(&args).expect("mock binary should spawn");
        let stdout = child.stdout.take().expect("piped");
        let reader = BufReader::new(stdout);

        let mut segments = 0;
        let mut done: Option<TranscribeLine> = None;
        for line in reader.lines().map_while(Result::ok) {
            match parse_transcribe_line(&line) {
                Some(seg @ TranscribeLine::Segment { .. }) => {
                    segments += 1;
                    let _ = seg;
                }
                Some(d @ TranscribeLine::Done { .. }) => done = Some(d),
                _ => {}
            }
        }
        let status = child.wait().expect("mock binary should exit cleanly");
        assert!(status.success());
        assert_eq!(segments, 2, "mock binary should emit exactly 2 segment events");

        let TranscribeLine::Done { txt_path, json_path } = done.expect("mock binary should emit a done event") else {
            unreachable!()
        };
        let txt_path = PathBuf::from(txt_path.expect("done event should carry txt path"));
        let json_path = PathBuf::from(json_path.expect("done event should carry json path"));
        assert!(txt_path.is_file(), "mock binary should have actually written the txt file");
        assert!(json_path.is_file(), "mock binary should have actually written the json file");

        // Full round trip: feed the mock's real output into finalize_artifacts,
        // same as spawn_reader_and_finalize would.
        let storage_dir = scratch_dir("mock-post-storage");
        let ctx = make_ctx(&out_dir, &storage_dir, false);
        std::fs::write(&ctx.rx_path, b"fake-rx").unwrap();
        std::fs::write(&ctx.tx_path, b"fake-tx").unwrap();
        let result = finalize_artifacts(&ctx, &txt_path, &json_path).expect("finalize should succeed on mock output");
        assert!(result.txt.is_file());
        assert!(result.json.is_file());

        let _ = std::fs::remove_dir_all(&storage_dir);
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    #[cfg(unix)]
    #[test]
    fn mock_binary_live_mode_blocks_until_stop_then_emits_done() {
        let out_dir = scratch_dir("mock-live");
        let meta_path = write_test_meta(&out_dir);
        let args = TranscribeArgs {
            bin: mock_binary_path(),
            rx: out_dir.join("call-rx.wav"),
            tx: out_dir.join("call-tx.wav"),
            model: PathBuf::from("/dev/null"),
            lang: "es".to_string(),
            mode: "live",
            out_dir: out_dir.clone(),
            meta_path,
        };
        let mut child = spawn_transcribe(&args).expect("mock binary should spawn");
        let mut stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let mut lines = BufReader::new(stdout).lines();

        let seg1 = lines.next().expect("first segment").expect("readable");
        let seg2 = lines.next().expect("second segment").expect("readable");
        assert!(matches!(parse_transcribe_line(&seg1), Some(TranscribeLine::Segment { .. })));
        assert!(matches!(parse_transcribe_line(&seg2), Some(TranscribeLine::Segment { .. })));

        // Live mode must not finish on its own - it's still waiting on
        // stdin at this point. Signal stop exactly like on_tap_stopped does.
        writeln!(stdin, "stop").expect("write stop to mock stdin");

        let done_line = lines.next().expect("done event after stop").expect("readable");
        assert!(matches!(parse_transcribe_line(&done_line), Some(TranscribeLine::Done { .. })));

        let status = child.wait().expect("mock binary should exit after stop");
        assert!(status.success());

        let _ = std::fs::remove_dir_all(&out_dir);
    }

    #[cfg(unix)]
    #[test]
    fn mock_binary_ensure_model_emits_progress_then_ready() {
        let models_dir = scratch_dir("mock-ensure-model");
        let mut child = spawn_ensure_model(&mock_binary_path(), ModelTier::Light, &models_dir)
            .expect("mock binary should spawn for ensure-model");
        let stdout = child.stdout.take().expect("piped");
        let reader = BufReader::new(stdout);
        let mut saw_progress = false;
        let mut ready: Option<EnsureModelLine> = None;
        for line in reader.lines().map_while(Result::ok) {
            match parse_ensure_model_line(&line) {
                Some(p @ EnsureModelLine::Progress { .. }) => {
                    saw_progress = true;
                    let _ = p;
                }
                Some(r @ EnsureModelLine::Ready { .. }) => ready = Some(r),
                _ => {}
            }
        }
        let status = child.wait().expect("mock binary should exit cleanly");
        assert!(status.success());
        assert!(saw_progress, "mock ensure-model should emit at least one progress event");
        assert!(matches!(ready, Some(EnsureModelLine::Ready { .. })));

        let _ = std::fs::remove_dir_all(&models_dir);
    }
}

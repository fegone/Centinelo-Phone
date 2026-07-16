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
//! The live-updating transcript panel is ola 2 (a mockup is "en camino"
//! per the task) - this module only ever emits Tauri events
//! (`transcription://segment`/`done`/`error`/`model-download-*`) for a
//! future frontend to subscribe to; nothing here renders UI. See the shell
//! task report for the full list of what's plumbing today vs. ola 2 scope.
//!
//! # Where the license check happens
//!
//! Never here in a way that could be forked around - same discipline as
//! `premium.rs`/`console.rs`: [`is_unlocked`] only ever asks the loaded
//! premium dylib "is `transcription` licensed" and relays the answer.
//!
//! # Contract with `centinelo-transcribe` (the sidecar binary)
//!
//! This module integrates against the CLI/event contract specified in the
//! F4-cierre shell task (Mario, 2026-07-16), **not** the crate's current
//! `centinelo-transcribe/src/main.rs` CLI (which predates this task and
//! uses a different `Label=path` positional-args shape with no JSON
//! event stream) - see this file's own report for the exact mismatch and
//! why: the binary may not exist yet on this machine, so this shell talks
//! to whatever `centinelo-transcribe run ...` becomes, and tests mock it
//! with a small script (`tests/fixtures/mock-transcribe.sh`) that speaks
//! the target contract. Reconciling the crate's real CLI with this
//! contract is `transcribe-engine`'s side of the integration - flagged in
//! the shell report as a cross-agent follow-up, not guessed at here.
//!
//! Invocation: `centinelo-transcribe run --rx <path> --tx <path> --model
//! <path> --lang <lang> --mode live|post --out-dir <dir> --meta <json>`.
//! Stdout: one JSON object per line - `{"event":"segment","speaker":...,
//! "t0_ms":...,"t1_ms":...,"text":...}`, `{"event":"done","txt_path":...,
//! "json_path":...}`, `{"event":"error","message":...}`. Live mode: a
//! `stop\n` line on stdin signals "wrap up now" (matching
//! `core/PROTOCOL.md`'s own "the engine reads commands, not just runs to
//! completion" shape, so this shell's spawn/pipe/read code below looks
//! deliberately similar to `sidecar.rs`'s).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};

use crate::premium::{CapabilityStatusView, PremiumHandle};
use crate::settings::{ModelTier, SettingsStore, TranscriptionMode, TranscriptionSettings};
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
// Orchestration handle
// ---------------------------------------------------------------------

/// Per-call bookkeeping needed to finalize a tap once transcription
/// finishes - captured once at tap-start time (see `FinalizeContext`'s own
/// doc) so the reader/finalize thread never has to re-read settings or
/// re-look-up the call, and a mid-call settings change (e.g. admin edits
/// `storage_dir` while a call is in progress) can't produce inconsistent
/// behavior partway through a single call's pipeline - matches how
/// account-settings changes elsewhere in this crate only take effect on
/// the *next* sidecar respawn, never retroactively.
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

struct ActiveEntry {
    ctx: FinalizeContext,
    /// Only ever `Some` in `Live` mode, and only after the transcribe
    /// process has actually spawned (`on_tap_started`) - used to deliver
    /// the `stop\n` signal from `on_tap_stopped`. See those two methods'
    /// docs for the started/stopped race this (plus `stop_requested`)
    /// exists to close.
    live_stdin: Option<std::process::ChildStdin>,
    /// Set by `on_tap_stopped` the moment it runs, *before* it knows
    /// whether `live_stdin` is populated yet - `on_tap_started`, once it
    /// finishes spawning, checks this and sends the stop signal itself if
    /// it's already true, rather than losing it to a race on a very short
    /// call (tap started and stopped before the transcribe process
    /// finished spawning).
    stop_requested: bool,
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
        let Some(state) = event.get("state").and_then(Value::as_str) else {
            return;
        };
        let Some(call_id) = event.get("call_id").and_then(Value::as_str) else {
            return;
        };
        if state != "established" {
            return; // "closed" is handled entirely via tap_state:"stopped" - see on_tap_stopped's doc.
        }
        if self.0.active.lock().expect("poisoned").contains_key(call_id) {
            return; // already tapping (e.g. a manual start already armed it)
        }
        let settings = self.0.settings.snapshot().transcription;
        if settings.mode == TranscriptionMode::Off {
            return;
        }
        if settings.activation != crate::settings::TranscriptionActivation::AllCalls {
            return; // Manual: only an explicit transcription_manual_start call arms a tap.
        }
        if !is_unlocked(&self.0.premium) {
            return;
        }
        let peer = event.get("peer").and_then(Value::as_str).unwrap_or("").to_string();
        self.start_tap(call_id, &peer, settings);
    }

    /// Called from `sidecar.rs`'s stdout-reader thread on every
    /// `tap_state` event (`core/PROTOCOL.md` v1.2).
    pub fn on_tap_state(&self, event: &Value) {
        let Some(state) = event.get("state").and_then(Value::as_str) else {
            return;
        };
        let Some(call_id) = event.get("call_id").and_then(Value::as_str) else {
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
        if self.0.active.lock().expect("poisoned").contains_key(call_id) {
            return Err("Already transcribing this call.".to_string());
        }
        self.start_tap(call_id, peer, settings);
        Ok(())
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
    /// `Inner::pending_retries`'s doc. Always re-runs a full `post` pass
    /// against the still-on-disk WAVs (simpler and just as correct as
    /// trying to distinguish "only the move failed" from "transcription
    /// itself never finished" - re-transcribing the same WAVs is
    /// idempotent, see `finalize_artifacts`).
    pub fn retry(&self, call_id: &str) -> Result<(), String> {
        let retry = {
            let mut guard = self.0.pending_retries.lock().expect("poisoned");
            guard
                .remove(call_id)
                .ok_or_else(|| "No pending retry for this call.".to_string())?
        };
        if !retry.rx_path.is_file() && !retry.tx_path.is_file() {
            let call_id_owned = retry.call_id.clone();
            self.0
                .pending_retries
                .lock()
                .expect("poisoned")
                .insert(call_id_owned, retry);
            return Err("The original audio for this call is no longer on disk - nothing to retry.".to_string());
        }
        let ctx = FinalizeContext {
            call_id: retry.call_id.clone(),
            peer: retry.peer.clone(),
            tap_dir: retry.tap_dir.clone(),
            rx_path: retry.rx_path.clone(),
            tx_path: retry.tx_path.clone(),
            started_at: retry.started_at,
            settings: retry.settings.clone(),
        };
        let bin = match resolve_transcribe_binary() {
            Ok(b) => b,
            Err(e) => {
                let call_id_owned = retry.call_id.clone();
                self.0
                    .pending_retries
                    .lock()
                    .expect("poisoned")
                    .insert(call_id_owned, retry);
                return Err(e);
            }
        };
        let model = model_path(&self.0.app, ctx.settings.model_tier);
        let meta = call_meta_json(&ctx.call_id, &ctx.peer, "post");
        let args = TranscribeArgs {
            bin,
            rx: ctx.rx_path.clone(),
            tx: ctx.tx_path.clone(),
            model,
            lang: ctx.settings.language.clone(),
            mode: "post",
            out_dir: ctx.tap_dir.clone(),
            meta_json: meta,
        };
        match spawn_transcribe(&args) {
            Ok(child) => {
                spawn_reader_and_finalize(self.0.clone(), ctx, child);
                Ok(())
            }
            Err(e) => {
                let call_id_owned = retry.call_id.clone();
                self.0
                    .pending_retries
                    .lock()
                    .expect("poisoned")
                    .insert(call_id_owned, retry);
                Err(e.to_string())
            }
        }
    }

    fn start_tap(&self, call_id: &str, peer: &str, settings: TranscriptionSettings) {
        let tap_dir = std::env::temp_dir().join(format!("centinelo-transcribe-tap.{call_id}"));
        if let Err(e) = std::fs::create_dir_all(&tap_dir) {
            log::warn!("transcription: could not create tap dir for {call_id}: {e}");
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tap_dir, std::fs::Permissions::from_mode(0o700));
        }
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
        self.0.active.lock().expect("poisoned").insert(
            call_id.to_string(),
            ActiveEntry {
                ctx,
                live_stdin: None,
                stop_requested: false,
            },
        );

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
    }

    fn on_tap_started(&self, call_id: &str) {
        let inner = self.0.clone();
        let call_id = call_id.to_string();
        std::thread::spawn(move || {
            let ctx = {
                let active = inner.active.lock().expect("poisoned");
                match active.get(&call_id) {
                    Some(e) => e.ctx.clone(),
                    None => return, // not a call this handle armed
                }
            };
            if ctx.settings.mode != TranscriptionMode::Live {
                return; // post-call mode starts its process at tap_stop, not tap_start
            }
            let bin = match resolve_transcribe_binary() {
                Ok(b) => b,
                Err(e) => {
                    log::warn!("transcription: {e}");
                    let _ = inner.app.emit(
                        "transcription://error",
                        serde_json::json!({"call_id": ctx.call_id, "message": e}),
                    );
                    inner.active.lock().expect("poisoned").remove(&ctx.call_id);
                    return;
                }
            };
            let model = model_path(&inner.app, ctx.settings.model_tier);
            let meta = call_meta_json(&ctx.call_id, &ctx.peer, "live");
            let args = TranscribeArgs {
                bin,
                rx: ctx.rx_path.clone(),
                tx: ctx.tx_path.clone(),
                model,
                lang: ctx.settings.language.clone(),
                mode: "live",
                out_dir: ctx.tap_dir.clone(),
                meta_json: meta,
            };
            match spawn_transcribe(&args) {
                Ok(mut child) => {
                    let stdin = child.stdin.take();
                    let mut send_stop_now = false;
                    {
                        let mut active = inner.active.lock().expect("poisoned");
                        if let Some(entry) = active.get_mut(&call_id) {
                            entry.live_stdin = stdin;
                            if entry.stop_requested {
                                send_stop_now = true;
                            }
                        }
                    }
                    if send_stop_now {
                        // tap_state:"stopped" already raced ahead of this
                        // spawn (a very short call) - see ActiveEntry's
                        // `stop_requested` doc.
                        let mut active = inner.active.lock().expect("poisoned");
                        if let Some(entry) = active.get_mut(&call_id) {
                            if let Some(stdin) = entry.live_stdin.as_mut() {
                                let _ = writeln!(stdin, "stop");
                            }
                        }
                    }
                    spawn_reader_and_finalize(inner.clone(), ctx, child);
                }
                Err(e) => {
                    log::warn!("transcription: failed to start live transcribe for {}: {e}", ctx.call_id);
                    let _ = inner.app.emit(
                        "transcription://error",
                        serde_json::json!({"call_id": ctx.call_id, "message": e.to_string()}),
                    );
                    inner.active.lock().expect("poisoned").remove(&ctx.call_id);
                    stash_pending_retry(&inner, &ctx, None, None, e.to_string());
                }
            }
        });
    }

    fn on_tap_stopped(&self, call_id: &str, event: &Value) {
        let inner = self.0.clone();
        let call_id = call_id.to_string();
        let rx_bytes = event.get("rx_bytes").and_then(Value::as_u64);
        let tx_bytes = event.get("tx_bytes").and_then(Value::as_u64);
        std::thread::spawn(move || {
            let is_live = {
                let mut active = inner.active.lock().expect("poisoned");
                let Some(entry) = active.get_mut(&call_id) else {
                    return; // not a call this handle armed (or already finalized)
                };
                entry.stop_requested = true;
                let is_live = entry.ctx.settings.mode == TranscriptionMode::Live;
                if is_live {
                    if let Some(stdin) = entry.live_stdin.as_mut() {
                        let _ = writeln!(stdin, "stop");
                    }
                }
                is_live
            };
            log::info!("transcription: tap stopped for {call_id} (rx_bytes={rx_bytes:?} tx_bytes={tx_bytes:?})");
            if is_live {
                // The reader thread spawned from on_tap_started already
                // owns this call's finalize step - nothing else to do.
                return;
            }
            // Post-call: this IS the trigger to run the single transcribe
            // pass, now that both WAVs are fully finalized on disk.
            let ctx = {
                let mut active = inner.active.lock().expect("poisoned");
                match active.remove(&call_id) {
                    Some(entry) => entry.ctx,
                    None => return,
                }
            };
            let bin = match resolve_transcribe_binary() {
                Ok(b) => b,
                Err(e) => {
                    log::warn!("transcription: {e}");
                    let _ = inner.app.emit(
                        "transcription://error",
                        serde_json::json!({"call_id": ctx.call_id, "message": e}),
                    );
                    stash_pending_retry(&inner, &ctx, None, None, e);
                    return;
                }
            };
            let model = model_path(&inner.app, ctx.settings.model_tier);
            let meta = call_meta_json(&ctx.call_id, &ctx.peer, "post");
            let args = TranscribeArgs {
                bin,
                rx: ctx.rx_path.clone(),
                tx: ctx.tx_path.clone(),
                model,
                lang: ctx.settings.language.clone(),
                mode: "post",
                out_dir: ctx.tap_dir.clone(),
                meta_json: meta,
            };
            match spawn_transcribe(&args) {
                Ok(child) => spawn_reader_and_finalize(inner.clone(), ctx, child),
                Err(e) => {
                    log::warn!("transcription: failed to start post-call transcribe for {}: {e}", ctx.call_id);
                    let _ = inner.app.emit(
                        "transcription://error",
                        serde_json::json!({"call_id": ctx.call_id, "message": e.to_string()}),
                    );
                    stash_pending_retry(&inner, &ctx, None, None, e.to_string());
                }
            }
        });
    }
}

#[derive(Clone)]
struct PendingRetry {
    call_id: String,
    peer: String,
    tap_dir: PathBuf,
    rx_path: PathBuf,
    tx_path: PathBuf,
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
    _txt_path: Option<PathBuf>,
    _json_path: Option<PathBuf>,
    reason: String,
) {
    let retry = PendingRetry {
        call_id: ctx.call_id.clone(),
        peer: ctx.peer.clone(),
        tap_dir: ctx.tap_dir.clone(),
        rx_path: ctx.rx_path.clone(),
        tx_path: ctx.tx_path.clone(),
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
    meta_json: String,
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
        .arg(&args.meta_json)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

#[derive(Debug, Clone, PartialEq)]
enum TranscribeLine {
    Segment { speaker: String, t0_ms: u64, t1_ms: u64, text: String },
    Done { txt_path: Option<String>, json_path: Option<String> },
    Error { message: String },
    Unknown,
}

/// Pure parser for one line of the sidecar's stdout - see this module's
/// doc for the exact JSON shapes. Unit-tested without spawning any
/// process (`mod tests` below).
fn parse_transcribe_line(line: &str) -> Option<TranscribeLine> {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        return None; // matches sidecar.rs's own "ignore non-JSON noise" convention
    }
    let v: Value = serde_json::from_str(trimmed).ok()?;
    let event = v.get("event").and_then(Value::as_str)?;
    Some(match event {
        "segment" => TranscribeLine::Segment {
            speaker: v.get("speaker").and_then(Value::as_str).unwrap_or("").to_string(),
            t0_ms: v.get("t0_ms").and_then(Value::as_u64).unwrap_or(0),
            t1_ms: v.get("t1_ms").and_then(Value::as_u64).unwrap_or(0),
            text: v.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
        },
        "done" => TranscribeLine::Done {
            txt_path: v.get("txt_path").and_then(Value::as_str).map(str::to_string),
            json_path: v.get("json_path").and_then(Value::as_str).map(str::to_string),
        },
        "error" => TranscribeLine::Error {
            message: v.get("message").and_then(Value::as_str).unwrap_or("unknown error").to_string(),
        },
        _ => TranscribeLine::Unknown,
    })
}

/// Reads `child`'s stdout to completion, classifying each line, emitting
/// `transcription://segment`/`transcription://error` as they arrive, then
/// (on EOF) waits the process and either finalizes (`done` was seen) or
/// stashes a pending retry. Runs on its own thread - the only thing that
/// blocks on this child's lifetime, matching `sidecar.rs`'s own
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
        // The one place a tap's lifecycle in `active` actually ends - see
        // `ActiveEntry`/`on_tap_started`/`on_tap_stopped` docs for why
        // live-mode removal doesn't happen at tap_stop time.
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

/// `rename()` fails across filesystems/mount points (temp dir on one
/// volume, a mounted NAS share on another) - falls back to copy+remove.
fn move_file(src: &Path, dest: &Path) -> Result<(), String> {
    if std::fs::rename(src, dest).is_ok() {
        return Ok(());
    }
    std::fs::copy(src, dest).map_err(|e| format!("could not copy {} -> {}: {e}", src.display(), dest.display()))?;
    std::fs::remove_file(src)
        .map_err(|e| format!("copied but could not remove source {}: {e}", src.display()))?;
    Ok(())
}

fn call_meta_json(call_id: &str, peer: &str, mode: &str) -> String {
    serde_json::json!({"call_id": call_id, "peer": peer, "mode": mode}).to_string()
}

/// Resolves the `centinelo-transcribe` binary: `CENTINELO_TRANSCRIBE_BIN`
/// override first (also how tests point this at the mocked script), then
/// next to this executable - same two-step shape as `sidecar.rs`'s
/// `resolve_core_binary`, minus that function's dev-convenience walk-up
/// search (the transcribe crate lives in the private `premium/` repo, a
/// sibling of `phone/` rather than nested under it, so there's no single
/// relative path to walk up to across every dev machine's checkout - see
/// this file's own module doc, "may not exist yet on this machine").
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
// Model tiers + download (F4 item 5)
// ---------------------------------------------------------------------

/// ggml filename per tier - `transcribe` skill's model research
/// (2026-07-16): large-v3-turbo-q5_0 default "accurate", small-q5_1
/// "light" (medium is obsolete per that research; turbo is both more
/// accurate and faster).
pub fn model_filename(tier: ModelTier) -> &'static str {
    match tier {
        ModelTier::Accurate => "ggml-large-v3-turbo-q5_0.bin",
        ModelTier::Light => "ggml-small-q5_1.bin",
    }
}

fn model_download_url(tier: ModelTier) -> &'static str {
    match tier {
        ModelTier::Accurate => {
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin"
        }
        ModelTier::Light => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin",
    }
}

/// Pinned SHA256 of each tier's canonical file, when known. **Tech debt
/// (2026-07-16, shell task report): both are `None` today** - this shell
/// has no verified-safe way to obtain the real published checksums inside
/// this sprint's environment, and shipping a *guessed* hash would be
/// actively worse than none (a wrong pin fails every legitimate download
/// closed, permanently, until someone notices). [`download_model`] always
/// computes the real SHA256 of what it downloaded and logs it either way.
/// Once Felix/`transcribe-engine` confirm the canonical values (e.g. from
/// the model card on huggingface.co/ggerganov/whisper.cpp), pinning them
/// here is a one-line change with no other code affected.
fn model_expected_sha256(_tier: ModelTier) -> Option<&'static str> {
    None
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

/// Downloads `tier`'s model file with progress + checksum, per task item
/// 5. Blocking (`ureq`) - always called from a background thread (see
/// `commands::download_transcription_model`), matching this crate's
/// existing thread-per-blocking-operation style (`bridge.rs`'s HTTP
/// server, `sidecar.rs`'s supervisor) rather than pulling in an async
/// runtime for what's ultimately one big sequential download.
///
/// # Tech debt: lives here, not in `centinelo-transcribe`
///
/// Per the task spec: "la logica real de descarga vive en el crate
/// transcribe si su API esta lista; si no, implementala en shell". As of
/// this sprint the crate (`premium/crates/centinelo-transcribe`) has no
/// download/model-manager API at all (checked directly - only
/// `default_model_path()`, no HTTP/checksum code) - so this is the shell
/// implementation, flagged for consolidation into the crate later so a
/// future CLI-only/headless use of `centinelo-transcribe` doesn't need to
/// reimplement it.
pub fn download_model(app: &AppHandle, tier: ModelTier) -> Result<PathBuf, String> {
    let dir = model_dir(app);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dest_path = dir.join(model_filename(tier));
    let tmp_path = dir.join(format!("{}.part", model_filename(tier)));
    let url = model_download_url(tier);

    let resp = ureq::get(url).call().map_err(|e| format!("download failed: {e}"))?;
    let total: Option<u64> = resp.header("Content-Length").and_then(|s| s.parse().ok());
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(&tmp_path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    let tier_name = model_filename(tier);
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("read failed: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        hasher.update(&buf[..n]);
        downloaded += n as u64;
        let _ = app.emit(
            "transcription://model-download-progress",
            serde_json::json!({
                "tier": tier_name,
                "downloaded_bytes": downloaded,
                "total_bytes": total,
            }),
        );
    }
    drop(file);

    let digest = format!("{:x}", hasher.finalize());
    if let Some(expected) = model_expected_sha256(tier) {
        if !digest.eq_ignore_ascii_case(expected) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(format!(
                "checksum mismatch for {tier_name}: expected {expected}, got {digest} - download discarded"
            ));
        }
    } else {
        log::warn!(
            "transcription: no pinned checksum for {tier_name} yet (see transcription.rs \
             model_expected_sha256's doc) - downloaded sha256={digest}, not verified against a known-good value"
        );
    }
    std::fs::rename(&tmp_path, &dest_path).map_err(|e| e.to_string())?;
    Ok(dest_path)
}

/// Spawns [`download_model`] on its own thread and emits
/// `transcription://model-download-done`/`-error` when it finishes - see
/// `commands::download_transcription_model`.
pub fn spawn_model_download(app: AppHandle, tier: ModelTier) {
    std::thread::spawn(move || match download_model(&app, tier) {
        Ok(path) => {
            let _ = app.emit(
                "transcription://model-download-done",
                serde_json::json!({"tier": model_filename(tier), "path": path.display().to_string()}),
            );
        }
        Err(e) => {
            let _ = app.emit(
                "transcription://model-download-error",
                serde_json::json!({"tier": model_filename(tier), "message": e}),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_transcribe_line ----

    #[test]
    fn parses_segment_line() {
        let line = r#"{"event":"segment","speaker":"agent","t0_ms":100,"t1_ms":900,"text":"hola"}"#;
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
        let line = r#"{"event":"done","txt_path":"/tmp/a.txt","json_path":"/tmp/a.json"}"#;
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
        let line = r#"{"event":"error","message":"model not found"}"#;
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
    fn unknown_event_name_is_unknown_not_none() {
        assert_eq!(parse_transcribe_line(r#"{"event":"progress","pct":50}"#), Some(TranscribeLine::Unknown));
    }

    #[test]
    fn malformed_json_is_none() {
        assert_eq!(parse_transcribe_line("{not json"), None);
    }

    // ---- dated_dest_dir ----

    #[test]
    fn dated_dest_dir_builds_ymd_layout() {
        use chrono::TimeZone;
        let dt = Local.with_ymd_and_hms(2026, 7, 16, 14, 32, 10).unwrap();
        let dir = dated_dest_dir(Path::new("/storage"), dt);
        assert_eq!(dir, PathBuf::from("/storage/2026/07/16"));
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
            peer: "sip:1100@example.test".to_string(),
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

    // ---- model tiers ----

    #[test]
    fn model_filenames_are_distinct() {
        assert_ne!(model_filename(ModelTier::Accurate), model_filename(ModelTier::Light));
    }

    #[test]
    fn model_expected_sha256_is_documented_tech_debt() {
        // See model_expected_sha256's doc: intentionally None until a real
        // checksum is confirmed - this test pins the *current* state so a
        // future fill-in is a deliberate, reviewed change, not a silent
        // drop of verification.
        assert_eq!(model_expected_sha256(ModelTier::Accurate), None);
        assert_eq!(model_expected_sha256(ModelTier::Light), None);
    }

    // ---- mocked-sidecar process integration tests -----------------------
    //
    // These spawn the real `tests/fixtures/mock-transcribe.sh` script as a
    // real child process through the same `spawn_transcribe`/
    // `parse_transcribe_line` code the live app uses - the "e2e scripted
    // flujo con sidecar mockeado" the task asked for, without needing a
    // running Tauri app (no AppHandle involved - only the process
    // spawn/pipe/parse layer, which is exactly what's untested by the
    // pure `finalize_artifacts`/`parse_transcribe_line` unit tests above).
    // Unix-only: the fixture is a bash script (see its own header) and
    // this repo's Windows CI is already best-effort/continue-on-error
    // (shell/README.md "Known limitations").

    #[cfg(unix)]
    fn mock_binary_path() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/mock-transcribe.sh"))
    }

    #[cfg(unix)]
    #[test]
    fn mock_binary_post_mode_emits_segments_then_done_with_real_files() {
        let out_dir = scratch_dir("mock-post");
        let args = TranscribeArgs {
            bin: mock_binary_path(),
            rx: out_dir.join("call-rx.wav"),
            tx: out_dir.join("call-tx.wav"),
            model: PathBuf::from("/dev/null"),
            lang: "es".to_string(),
            mode: "post",
            out_dir: out_dir.clone(),
            meta_json: "{}".to_string(),
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
        let txt_path = PathBuf::from(txt_path.expect("done event should carry txt_path"));
        let json_path = PathBuf::from(json_path.expect("done event should carry json_path"));
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
        let args = TranscribeArgs {
            bin: mock_binary_path(),
            rx: out_dir.join("call-rx.wav"),
            tx: out_dir.join("call-tx.wav"),
            model: PathBuf::from("/dev/null"),
            lang: "es".to_string(),
            mode: "live",
            out_dir: out_dir.clone(),
            meta_json: "{}".to_string(),
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
}

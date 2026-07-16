//! Sidecar process supervisor: spawns `core/`'s baresip+ctrl_json binary,
//! speaks its newline-delimited-JSON protocol (see core/PROTOCOL.md) over
//! stdio, forwards events to the frontend as Tauri events, and restarts the
//! process with exponential backoff (capped at 5 tries) if it dies
//! unexpectedly.
//!
//! Config generation (the `accounts`/`config` scratch files) mirrors
//! `core/run-spike.sh` exactly - see that script's comments for *why* each
//! line is there (module order, `outbound=`, `mediaenc=dtls_srtp`, etc).
//! The one deliberate difference: `run-spike.sh` is a human-facing dev
//! tool that reads CENT_* env vars; here the equivalent values come from
//! `SettingsStore` (the account the operator configured in Settings), and
//! the SIP secret is written *only* into that ephemeral, mode-0600,
//! delete-on-stop scratch `accounts` file - the same sanctioned exception
//! `run-spike.sh` itself documents - never anywhere else.

use crate::settings::{AccountSettings, SettingsStore, TransportPriority};
use crate::transcription::TranscriptionHandle;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

pub const MAX_ATTEMPTS: u32 = 5;
const POLL_TICK: Duration = Duration::from_millis(120);
const STOP_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StatusPayload {
    /// No account configured yet, or explicitly stopped and never restarted.
    Idle,
    Starting,
    Running,
    Restarting {
        attempt: u32,
        max_attempts: u32,
        delay_secs: u64,
    },
    Stopped,
    Failed {
        message: String,
    },
}

const EVENT_STATUS: &str = "sidecar-status";
const EVENT_LINE: &str = "sidecar-event";

enum ControlSignal {
    None,
    Stop,
    RestartNow,
}

/// Coarse "what's happening on the line right now" - tracked here (not just
/// left to the frontend) so the click-to-call bridge's `/ping` (bridge.rs)
/// can report an honest state without duplicating protocol parsing. Mirrors
/// the vocabulary v1's `currentCallState` used (src/main/main.js), minus
/// `held` - F2/F3's shell UI has no hold control wired to the v1 protocol's
/// `hold` command yet (see shell/README.md "Known limitations"), so that
/// state can't actually be reached here and isn't fabricated.
///
/// No `None` variant on purpose (2026-07-16, fixing the qa-e2e-reported R4
/// bug below): this used to be a single `Mutex<CallPhase>` with a `None`
/// resting state, which meant *any* call_id's `closed` event reset the
/// whole thing to `None` - including an unrelated, already-cancelled leg
/// (e.g. dual-contact's own auto-ring cancelling itself) closing while a
/// real call on a *different* `call_id` was still established/on hold/
/// muted. `has_active_call()` would then report `false` mid-call and
/// `provisioning_apply` would restart the sidecar and drop the real call
/// with no warning - exactly what R4 was supposed to prevent. Now phases
/// are tracked per `call_id` in `Shared::call_phases`, and "no active
/// call" is simply "the map is empty", not a variant a stray event can
/// force onto an unrelated call_id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallPhase {
    /// `call_state:"incoming"` - someone is calling us.
    Incoming,
    /// `call_state:"ringing"` with no prior `incoming` for this call_id -
    /// an outbound call we placed is ringing out.
    Calling,
    InCall,
}

/// Applies one `call_state` transition to `phases`, scoped to `call_id` -
/// pulled out as a free function (not inlined in the stdout-reader match)
/// specifically so it's unit-testable without spinning up a `Shared`/
/// `AppHandle` (see the `call_phase_tests` module below, and `CallPhase`'s
/// doc for why this must be scoped per call_id).
fn apply_call_state_transition(
    phases: &mut HashMap<String, CallPhase>,
    call_id: &str,
    call_state: &str,
) {
    match call_state {
        "incoming" => {
            phases.insert(call_id.to_string(), CallPhase::Incoming);
        }
        "ringing" => {
            // Don't downgrade an already-incoming call_id to Calling - a
            // call we're being rung for stays "ringing" (Incoming) in the
            // vocabulary even if the far end's own signaling also fires a
            // `ringing` transition for the same call_id.
            if phases.get(call_id) != Some(&CallPhase::Incoming) {
                phases.insert(call_id.to_string(), CallPhase::Calling);
            }
        }
        "established" => {
            phases.insert(call_id.to_string(), CallPhase::InCall);
        }
        "closed" => {
            phases.remove(call_id);
        }
        // hold/resumed/muted/unmuted: attributes of an already-established
        // call, not a lifecycle change - see core/PROTOCOL.md "Events".
        // Deliberately a no-op, same as before this fix.
        _ => {}
    }
}

/// Picks the phase [`SidecarHandle::ping_state`] should report when more
/// than one call_id is tracked at once: `InCall` wins over `Incoming`
/// over `Calling`, so a genuine ongoing conversation (e.g. the original
/// call during an attended-transfer consultation) is never masked by some
/// other leg still ringing or dialing out. Returns `None` when `phases` is
/// empty (no active call at all).
fn dominant_call_phase(phases: &HashMap<String, CallPhase>) -> Option<CallPhase> {
    if phases.values().any(|p| *p == CallPhase::InCall) {
        Some(CallPhase::InCall)
    } else if phases.values().any(|p| *p == CallPhase::Incoming) {
        Some(CallPhase::Incoming)
    } else if phases.values().any(|p| *p == CallPhase::Calling) {
        Some(CallPhase::Calling)
    } else {
        None
    }
}

#[cfg(test)]
mod call_phase_tests {
    use super::*;

    /// The exact bug this fix addresses, reproduced twice against a real
    /// PBX: dual-contact's own auto-ring cancels itself (a `closed` on a
    /// call_id that was never the real call) while the real call, on a
    /// different call_id, is still established - `has_active_call()` must
    /// keep reporting `true` throughout.
    #[test]
    fn unrelated_leg_closing_does_not_clear_a_different_active_call() {
        let mut phases = HashMap::new();
        apply_call_state_transition(&mut phases, "real-call", "ringing");
        apply_call_state_transition(&mut phases, "real-call", "established");
        apply_call_state_transition(&mut phases, "ghost-leg", "incoming");
        apply_call_state_transition(&mut phases, "ghost-leg", "closed");

        assert!(!phases.is_empty(), "the real call must still be tracked");
        assert_eq!(phases.get("real-call"), Some(&CallPhase::InCall));
        assert!(!phases.contains_key("ghost-leg"));
    }

    /// A subsequent hold/mute on the real call must not be undone by
    /// (or need) the ghost leg's own teardown - full lifecycle sanity
    /// check matching qa-e2e's log timeline (established -> hold ->
    /// resumed -> muted -> unmuted, all on the same call_id, closed
    /// event from the other call_id in between).
    #[test]
    fn hold_mute_after_an_unrelated_close_still_leave_the_real_call_active() {
        let mut phases = HashMap::new();
        apply_call_state_transition(&mut phases, "real-call", "established");
        apply_call_state_transition(&mut phases, "ghost-leg", "incoming");
        apply_call_state_transition(&mut phases, "ghost-leg", "closed");
        apply_call_state_transition(&mut phases, "real-call", "hold");
        apply_call_state_transition(&mut phases, "real-call", "resumed");
        apply_call_state_transition(&mut phases, "real-call", "muted");
        apply_call_state_transition(&mut phases, "real-call", "unmuted");

        assert!(!phases.is_empty());
        assert_eq!(phases.get("real-call"), Some(&CallPhase::InCall));
    }

    /// The normal, single-call case: closing the only tracked call_id
    /// must still clear the active-call state (no regression from the
    /// per-call_id scoping).
    #[test]
    fn single_call_closing_clears_active_state() {
        let mut phases = HashMap::new();
        apply_call_state_transition(&mut phases, "only-call", "incoming");
        apply_call_state_transition(&mut phases, "only-call", "established");
        apply_call_state_transition(&mut phases, "only-call", "closed");

        assert!(phases.is_empty());
    }

    #[test]
    fn incoming_ring_is_not_downgraded_to_calling() {
        let mut phases = HashMap::new();
        apply_call_state_transition(&mut phases, "inbound", "incoming");
        apply_call_state_transition(&mut phases, "inbound", "ringing");

        assert_eq!(phases.get("inbound"), Some(&CallPhase::Incoming));
    }

    #[test]
    fn outbound_ringing_with_no_prior_incoming_is_calling() {
        let mut phases = HashMap::new();
        apply_call_state_transition(&mut phases, "outbound", "ringing");

        assert_eq!(phases.get("outbound"), Some(&CallPhase::Calling));
    }

    #[test]
    fn dominant_phase_prefers_in_call_over_other_legs() {
        let mut phases = HashMap::new();
        apply_call_state_transition(&mut phases, "original", "established");
        apply_call_state_transition(&mut phases, "consult", "ringing"); // attended-transfer consultation leg

        assert_eq!(dominant_call_phase(&phases), Some(CallPhase::InCall));
    }

    #[test]
    fn dominant_phase_is_none_when_no_calls_tracked() {
        let phases: HashMap<String, CallPhase> = HashMap::new();
        assert_eq!(dominant_call_phase(&phases), None);
    }
}

struct Shared {
    app: AppHandle,
    settings: Arc<SettingsStore>,
    stdin: Mutex<Option<std::process::ChildStdin>>,
    control: Mutex<ControlSignal>,
    thread_alive: Mutex<bool>,
    attempts: AtomicU32,
    exited_flag: AtomicBool,
    current_pid: Mutex<Option<u32>>,
    /// Set once per "session" (from start()/restart() to the next explicit
    /// restart) so the wss->udp auto fallback only fires once, not in a loop.
    auto_fallback_used: AtomicBool,
    pending_transport_override: Mutex<Option<&'static str>>,
    last_status: Mutex<StatusPayload>,
    /// True from a `reg_state:"registered"` event until the next
    /// `"failed"`/`"unregistered"` transition or process exit. Read by
    /// `ping_state()` (bridge.rs `/ping`) and used to gate the one-time BLF
    /// auto-subscribe below.
    registered: AtomicBool,
    /// Per-`call_id` phase - see [`CallPhase`]'s doc for why this is a map
    /// keyed by `call_id` and not a single global flag. An entry exists
    /// only while its call_id is live; `"closed"` removes it rather than
    /// resetting a shared value, so one leg's teardown can never affect
    /// another's tracked state.
    call_phases: Mutex<HashMap<String, CallPhase>>,
    /// Last-known state per watched extension (ext -> "idle"|"ringing"|
    /// "busy"|"offline"), from `blf` events - the same data the frontend's
    /// own `state.blf` derives from, kept here too so (a) a devtools
    /// reload doesn't lose it (`commands::get_blf_states`, fetched at
    /// `boot()`) and (b) it's real, backend-tracked "app state" a scripted
    /// e2e driver can read without any GUI - see e2e.rs.
    blf_states: Mutex<HashMap<String, String>>,
    /// Extensions this process has already sent `blf_subscribe` for -
    /// tracked explicitly (not inferred from `blf_states`, which only
    /// gains an entry once the *first* NOTIFY arrives) so
    /// `blf_subscribe`/`blf_unsubscribe` (see those methods below) can be
    /// idempotent. Needed because two independent callers now legitimately
    /// want "make sure this extension is watched": the favorites
    /// auto-subscribe below (on every `reg_state:"registered"`) and the
    /// premium console mounting with a roster that can - and in the F3/F4
    /// e2e setup, does - overlap favorites. `core/PROTOCOL.md`'s
    /// `blf_subscribe` errors on a literal duplicate subscribe; without
    /// this, opening the console after favorites already subscribed the
    /// same extension would surface a spurious `error` event instead of
    /// the idempotent "already watching it, nothing to do" this file's
    /// callers actually want.
    subscribed_exts: Mutex<HashSet<String>>,
    /// Set once at startup via `SidecarHandle::attach_transcription`
    /// (`lib.rs`'s `.setup()`, after both handles exist - see that
    /// method's doc for why this can't be a constructor argument). `None`
    /// until then, which the stdout reader below treats as "nothing to
    /// forward to" - never a panic, matching this file's existing
    /// tolerate-missing-state style elsewhere (e.g. `blf_states` starting
    /// empty).
    transcription: Mutex<Option<TranscriptionHandle>>,
}

#[derive(Clone)]
pub struct SidecarHandle(Arc<Shared>);

impl SidecarHandle {
    pub fn new(app: AppHandle, settings: Arc<SettingsStore>) -> Self {
        Self(Arc::new(Shared {
            app,
            settings,
            stdin: Mutex::new(None),
            control: Mutex::new(ControlSignal::None),
            thread_alive: Mutex::new(false),
            attempts: AtomicU32::new(0),
            exited_flag: AtomicBool::new(true),
            current_pid: Mutex::new(None),
            auto_fallback_used: AtomicBool::new(false),
            pending_transport_override: Mutex::new(None),
            last_status: Mutex::new(StatusPayload::Idle),
            registered: AtomicBool::new(false),
            call_phases: Mutex::new(HashMap::new()),
            blf_states: Mutex::new(HashMap::new()),
            subscribed_exts: Mutex::new(HashSet::new()),
            transcription: Mutex::new(None),
        }))
    }

    /// Wires a [`TranscriptionHandle`] in after construction - `lib.rs`'s
    /// `.setup()` builds `SidecarHandle` first (transcription needs a
    /// clone of it to send `tap_start`/`tap_stop`), so this is a
    /// post-construction attach rather than a constructor parameter, the
    /// same shape `bridge::start`'s `sidecar: SidecarHandle` argument
    /// avoids needing by being a free function instead - see `premium.rs`'s
    /// `PremiumHandle::load` for the sibling "handles get wired together
    /// in `.setup()`, not all at once" pattern this follows.
    pub fn attach_transcription(&self, transcription: TranscriptionHandle) {
        *self.0.transcription.lock().expect("poisoned") = Some(transcription);
    }

    /// Snapshot of every extension's last-known BLF state (see `Shared::blf_states`).
    pub fn blf_states(&self) -> HashMap<String, String> {
        self.0.blf_states.lock().expect("poisoned").clone()
    }

    /// Last known status, for the frontend's initial paint (before its event
    /// listener would otherwise catch the next transition).
    pub fn status(&self) -> StatusPayload {
        self.0.last_status.lock().expect("poisoned").clone()
    }

    /// Coarse call/registration state for the click-to-call bridge's
    /// `/ping` and for `commands::provisioning_apply`'s pre-restart check
    /// (2026-07-16 4R re-review, R4): applying a provisioning config
    /// restarts this sidecar unconditionally, which drops whatever call is
    /// in progress with no warning - a real risk specifically because a
    /// provisioning request can arrive via a `centinelo://provision` deep
    /// link (email/IM) at any time, not just from a deliberate "I'm
    /// between calls, let's reconfigure" moment the way opening Settings
    /// usually is. A fresh install (no account configured yet, the common
    /// first-run case) can never be mid-call - the supervisor loop never
    /// even starts before an account exists - so this is a safe, always-
    /// accurate check to run unconditionally rather than only when
    /// re-provisioning an already-configured install.
    ///
    /// True iff *any* call_id currently has a tracked phase - see
    /// `CallPhase`'s doc for the per-call_id rationale.
    pub fn has_active_call(&self) -> bool {
        !self.0.call_phases.lock().expect("poisoned").is_empty()
    }

    /// `/ping` (bridge.rs) - see `CallPhase`'s doc comment for the
    /// vocabulary and why `held` is deliberately absent, and
    /// `dominant_call_phase`'s doc for how this picks a single phase to
    /// report when more than one call_id is live at once.
    pub fn ping_state(&self) -> &'static str {
        match self.status() {
            StatusPayload::Idle | StatusPayload::Stopped | StatusPayload::Failed { .. } => {
                "disconnected"
            }
            StatusPayload::Starting | StatusPayload::Restarting { .. } => "connecting",
            StatusPayload::Running => {
                if !self.0.registered.load(Ordering::SeqCst) {
                    "connecting"
                } else {
                    match dominant_call_phase(&self.0.call_phases.lock().expect("poisoned")) {
                        None => "registered",
                        Some(CallPhase::Incoming) => "ringing",
                        Some(CallPhase::Calling) => "calling",
                        Some(CallPhase::InCall) => "in-call",
                    }
                }
            }
        }
    }

    /// Start supervision if it isn't already running. No-op if a supervisor
    /// thread is already alive (use `restart_now` to force a respawn).
    pub fn start(&self) {
        let mut alive = self.0.thread_alive.lock().expect("poisoned");
        if *alive {
            return;
        }
        *alive = true;
        drop(alive);

        self.0.auto_fallback_used.store(false, Ordering::SeqCst);
        *self.0.pending_transport_override.lock().expect("poisoned") = None;
        self.0.attempts.store(0, Ordering::SeqCst);
        *self.0.control.lock().expect("poisoned") = ControlSignal::None;

        let shared = self.0.clone();
        std::thread::spawn(move || supervisor_loop(shared));
    }

    /// Force an immediate respawn: used after saving new account settings,
    /// the UI's manual "retry" action, and internally by the wss->udp auto
    /// fallback. Does not count against the crash-backoff budget.
    pub fn restart_now(&self) {
        *self.0.control.lock().expect("poisoned") = ControlSignal::RestartNow;
        self.close_stdin();
        if *self.0.thread_alive.lock().expect("poisoned") {
            self.arm_force_kill_watchdog();
        } else {
            // Supervisor thread already exited (terminal Failed/Stopped) -
            // nothing to interrupt, just start a fresh one.
            self.start();
        }
    }

    /// Graceful shutdown - used when the whole app is exiting. Closing
    /// stdin is enough in the common case: ctrl_json treats stdin EOF as an
    /// implicit `quit` (core/PROTOCOL.md), so the child exits on its own
    /// and the supervisor thread sees `stop_requested` and does not
    /// respawn. The watchdog force-kills after a grace period if the child
    /// doesn't cooperate (e.g. truly hung).
    pub fn stop(&self) {
        *self.0.control.lock().expect("poisoned") = ControlSignal::Stop;
        self.close_stdin();
        self.arm_force_kill_watchdog();
    }

    fn close_stdin(&self) {
        // Dropping the ChildStdin closes the write end of the pipe.
        let _ = self.0.stdin.lock().expect("poisoned").take();
    }

    fn arm_force_kill_watchdog(&self) {
        self.0.exited_flag.store(false, Ordering::SeqCst);
        let shared = self.0.clone();
        std::thread::spawn(move || {
            std::thread::sleep(STOP_GRACE);
            if !shared.exited_flag.load(Ordering::SeqCst) {
                if let Some(pid) = *shared.current_pid.lock().expect("poisoned") {
                    log::warn!("sidecar: pid {pid} did not exit within {STOP_GRACE:?}, force-killing");
                    force_kill(pid);
                }
            }
        });
    }

    /// Send a command line to the running sidecar's stdin. Returns Err if
    /// the sidecar isn't currently running.
    pub fn send_cmd(&self, value: Value) -> Result<(), String> {
        send_cmd_raw(&self.0, value)
    }

    /// Idempotent `blf_subscribe` - see `Shared::subscribed_exts`'s doc
    /// for why this needs to be safe to call more than once for the same
    /// extension (favorites auto-subscribe + the console's own
    /// subscribe-on-mount can both reach the same ext). A second call for
    /// an already-watched extension is `Ok(())` without touching the
    /// wire at all, not a duplicate `blf_subscribe` command.
    pub fn blf_subscribe(&self, ext: &str) -> Result<(), String> {
        blf_subscribe_raw(&self.0, ext)
    }

    /// Idempotent counterpart to [`Self::blf_subscribe`] - unsubscribing
    /// an extension nothing currently has watched is `Ok(())`, matching
    /// `core/PROTOCOL.md`'s own "errors if not currently subscribed"
    /// caveat being something callers here shouldn't need to track by
    /// hand.
    pub fn blf_unsubscribe(&self, ext: &str) -> Result<(), String> {
        blf_unsubscribe_raw(&self.0, ext)
    }
}

/// Shared implementation of [`SidecarHandle::blf_subscribe`], also used
/// directly (via `&Arc<Shared>`) by the favorites auto-subscribe loop in
/// [`spawn_stdout_reader`], which only ever holds the inner `Shared`, not
/// a `SidecarHandle` wrapper (same reason `send_cmd_raw` is a free
/// function - see that function's own doc).
fn blf_subscribe_raw(shared: &Shared, ext: &str) -> Result<(), String> {
    let mut subscribed = shared.subscribed_exts.lock().expect("poisoned");
    if !subscribed.insert(ext.to_string()) {
        return Ok(()); // already watching it - nothing to do
    }
    drop(subscribed);
    let result = send_cmd_raw(shared, serde_json::json!({"cmd": "blf_subscribe", "ext": ext}));
    if result.is_err() {
        // Didn't actually reach the wire (sidecar not running) - don't
        // leave it marked subscribed, or a later real subscribe attempt
        // (e.g. once the sidecar comes back up) would be silently
        // swallowed by this same idempotency check.
        shared.subscribed_exts.lock().expect("poisoned").remove(ext);
    }
    result
}

fn blf_unsubscribe_raw(shared: &Shared, ext: &str) -> Result<(), String> {
    let mut subscribed = shared.subscribed_exts.lock().expect("poisoned");
    if !subscribed.remove(ext) {
        return Ok(()); // not currently watching it - nothing to do
    }
    drop(subscribed);
    send_cmd_raw(shared, serde_json::json!({"cmd": "blf_unsubscribe", "ext": ext}))
}

/// Writes one `ctrl_json` command line (core/PROTOCOL.md framing: one JSON
/// object, `\n`-terminated) to the sidecar's stdin. Free function (not a
/// `SidecarHandle` method) so the stdout-reader thread - which only ever
/// holds the inner `Arc<Shared>`, not a `SidecarHandle` - can also issue
/// commands (used for the BLF auto-subscribe below) without constructing a
/// throwaway wrapper.
fn send_cmd_raw(shared: &Shared, value: Value) -> Result<(), String> {
    let mut guard = shared.stdin.lock().expect("poisoned");
    match guard.as_mut() {
        Some(stdin) => {
            let mut line = serde_json::to_string(&value).map_err(|e| e.to_string())?;
            line.push('\n');
            stdin.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
            stdin.flush().map_err(|e| e.to_string())
        }
        None => Err("Not connected to the phone system yet.".to_string()),
    }
}

#[cfg(unix)]
fn force_kill(pid: u32) {
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
}
#[cfg(windows)]
fn force_kill(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status();
}

fn supervisor_loop(shared: Arc<Shared>) {
    loop {
        // --- resolve config for this attempt -------------------------------
        let account = shared.settings.snapshot().account;
        if !account.is_configured() {
            shared.emit_status_from_thread(StatusPayload::Idle);
            *shared.thread_alive.lock().expect("poisoned") = false;
            return;
        }

        let transport = choose_transport(&account, &shared);
        let plan = match SpawnPlan::build(&shared.settings, &account, transport) {
            Ok(p) => p,
            Err(e) => {
                let attempt = shared.attempts.fetch_add(1, Ordering::SeqCst) + 1;
                if !wait_out_backoff_or_stop(&shared, attempt) {
                    return; // stop requested during backoff
                }
                shared.emit_status_from_thread(StatusPayload::Failed {
                    message: e.clone(),
                });
                if attempt >= MAX_ATTEMPTS {
                    *shared.thread_alive.lock().expect("poisoned") = false;
                    return;
                }
                continue;
            }
        };

        shared.emit_status_from_thread(StatusPayload::Starting);

        // CENT_TLS_PIN: core/PROTOCOL.md's own documented env var ("one
        // flat env var - single pin, checked for every TLS/WSS
        // connection", see that file's TLS verification section) - only
        // set when the account actually has one (provisioning.rs is the
        // only writer today, see settings.rs AccountSettings doc). Built
        // as a plain `Command` rather than one long builder chain so this
        // one env var can be conditional without duplicating every other
        // `.arg()`/`.env()`/`.stdio()` call across two branches.
        let mut cmd = Command::new(&plan.binary);
        cmd.arg("-f")
            .arg(&plan.scratch_dir)
            .env("CENT_WS_PATH", &plan.ws_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(pin) = account.tls_pin_sha256.as_deref().filter(|p| !p.is_empty()) {
            cmd.env("CENT_TLS_PIN", pin);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&plan.scratch_dir);
                let attempt = shared.attempts.fetch_add(1, Ordering::SeqCst) + 1;
                shared.emit_status_from_thread(StatusPayload::Failed {
                    message: format!("Couldn't start the core engine ({e}). Check the binary path in Settings > Advanced."),
                });
                if attempt >= MAX_ATTEMPTS || !wait_out_backoff_or_stop(&shared, attempt) {
                    *shared.thread_alive.lock().expect("poisoned") = false;
                    return;
                }
                continue;
            }
        };

        *shared.current_pid.lock().expect("poisoned") = child.id().into();
        shared.exited_flag.store(false, Ordering::SeqCst);

        let stdin = child.stdin.take();
        *shared.stdin.lock().expect("poisoned") = stdin;
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");

        let recent_stderr = Arc::new(Mutex::new(Vec::<String>::new()));
        spawn_stderr_drain(stderr, recent_stderr.clone());
        spawn_stdout_reader(shared.clone(), stdout, transport, account.transport_priority);

        // Blocking wait - unblocks on natural exit/crash, or promptly after
        // stop()/restart_now() close stdin (ctrl_json quits on stdin EOF).
        let exit = wait_child(&mut child);
        shared.exited_flag.store(true, Ordering::SeqCst);
        *shared.stdin.lock().expect("poisoned") = None;
        *shared.current_pid.lock().expect("poisoned") = None;
        // The engine (and every subscription/call it held) is gone with the
        // process - a stale "registered"/"in-call" ping_state() would be a
        // straightforward lie to the click-to-call bridge.
        shared.registered.store(false, Ordering::SeqCst);
        shared.call_phases.lock().expect("poisoned").clear();
        shared.blf_states.lock().expect("poisoned").clear();
        // A fresh process starts with no subscriptions either - matches
        // blf_states being cleared above, and lets a respawned process's
        // favorites auto-subscribe (and any console still open across the
        // respawn) re-subscribe for real instead of blf_subscribe_raw's
        // idempotency check silently swallowing it as "already watching".
        shared.subscribed_exts.lock().expect("poisoned").clear();
        let _ = std::fs::remove_dir_all(&plan.scratch_dir);

        let signal = std::mem::replace(&mut *shared.control.lock().expect("poisoned"), ControlSignal::None);
        match signal {
            ControlSignal::Stop => {
                shared.emit_status_from_thread(StatusPayload::Stopped);
                *shared.thread_alive.lock().expect("poisoned") = false;
                return;
            }
            ControlSignal::RestartNow => {
                // Intentional respawn (settings change / manual retry /
                // auto transport fallback) - not a crash, no backoff.
                continue;
            }
            ControlSignal::None => {
                // Unexpected exit.
                let attempt = shared.attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let tail = recent_stderr
                    .lock()
                    .map(|v| v.join(" | "))
                    .unwrap_or_default();
                let exit_desc = exit
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "unknown".into());
                log::warn!("sidecar exited unexpectedly ({exit_desc}); attempt {attempt}/{MAX_ATTEMPTS}; stderr tail: {tail}");
                if attempt >= MAX_ATTEMPTS {
                    shared.emit_status_from_thread(StatusPayload::Failed {
                        message: format!(
                            "The core engine crashed {MAX_ATTEMPTS} times in a row and Centinelo stopped retrying. Last exit: {exit_desc}."
                        ),
                    });
                    *shared.thread_alive.lock().expect("poisoned") = false;
                    return;
                }
                if !wait_out_backoff_or_stop(&shared, attempt) {
                    return;
                }
                continue;
            }
        }
    }
}

impl Shared {
    fn emit_status_from_thread(self: &Arc<Self>, payload: StatusPayload) {
        *self.last_status.lock().expect("poisoned") = payload.clone();
        let _ = self.app.emit(EVENT_STATUS, payload);
    }
}

/// Sleeps in small increments up to the exponential backoff delay for
/// `attempt` (1s, 2s, 4s, 8s, 16s), checking every tick whether a manual
/// stop/restart came in so a human doesn't have to wait out a long delay.
/// Returns false if a Stop was observed (caller should terminate).
fn wait_out_backoff_or_stop(shared: &Arc<Shared>, attempt: u32) -> bool {
    let delay_secs: u64 = 1 << (attempt.saturating_sub(1)).min(4); // 1,2,4,8,16
    shared.emit_status_from_thread(StatusPayload::Restarting {
        attempt,
        max_attempts: MAX_ATTEMPTS,
        delay_secs,
    });
    let ticks = (Duration::from_secs(delay_secs).as_millis() / POLL_TICK.as_millis()).max(1);
    for _ in 0..ticks {
        std::thread::sleep(POLL_TICK);
        match &*shared.control.lock().expect("poisoned") {
            ControlSignal::Stop => return false,
            ControlSignal::RestartNow => return true, // stop waiting, respawn now
            ControlSignal::None => {}
        }
    }
    true
}

fn wait_child(child: &mut Child) -> Option<std::process::ExitStatus> {
    child.wait().ok()
}

fn choose_transport(account: &AccountSettings, shared: &Arc<Shared>) -> &'static str {
    if let Some(t) = *shared.pending_transport_override.lock().expect("poisoned") {
        return t;
    }
    match account.transport_priority {
        TransportPriority::Wss => "wss",
        TransportPriority::Classic => "udp",
        TransportPriority::Auto => "wss",
    }
}

fn spawn_stderr_drain(stderr: std::process::ChildStderr, sink: Arc<Mutex<Vec<String>>>) {
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            log::debug!("core: {line}");
            if let Ok(mut buf) = sink.lock() {
                buf.push(line);
                if buf.len() > 20 {
                    buf.remove(0);
                }
            }
        }
    });
}

fn spawn_stdout_reader(
    shared: Arc<Shared>,
    stdout: std::process::ChildStdout,
    transport_this_attempt: &'static str,
    priority: TransportPriority,
) {
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut registered_once = false;
        for line in reader.lines().map_while(Result::ok) {
            let trimmed = line.trim_start();
            if !trimmed.starts_with('{') {
                continue; // baresip's own human-readable log noise, see PROTOCOL.md "Framing"
            }
            let value: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // Evidence trail: every ctrl_json event line, verbatim, at INFO
            // so `shell/E2E.md` can cite real captured output (see PROTOCOL.md
            // "v0 events" - this mirrors how core/BUILD.md's own testing
            // narrative captured stdout with `grep '^{'`).
            log::info!("sidecar event: {value}");
            let event_name = value.get("event").and_then(Value::as_str).unwrap_or("");

            if event_name == "ready" {
                shared.attempts.store(0, Ordering::SeqCst);
                shared.emit_status_from_thread(StatusPayload::Running);
            } else if event_name == "reg_state" {
                let state = value.get("state").and_then(Value::as_str).unwrap_or("");
                if state == "registered" {
                    registered_once = true;
                    shared.registered.store(true, Ordering::SeqCst);
                    // blf_subscribe_raw is idempotent (see Shared::subscribed_exts's
                    // doc) - safe to run on every "registered" transition,
                    // including regint=120's periodic re-REGISTER within
                    // the same process, and safe to overlap with the
                    // premium console's own subscribe-on-mount for the
                    // same extensions.
                    let favorites = shared.settings.snapshot().favorites;
                    for fav in favorites {
                        let ext = fav.ext.trim();
                        if ext.is_empty() {
                            continue; // unconfigured slot - nothing to watch
                        }
                        if let Err(e) = blf_subscribe_raw(&shared, ext) {
                            log::warn!("sidecar: blf_subscribe({ext}) failed: {e}");
                        }
                    }
                } else {
                    if state == "failed" || state == "unregistered" {
                        shared.registered.store(false, Ordering::SeqCst);
                    }
                    if state == "failed"
                        && !registered_once
                        && priority == TransportPriority::Auto
                        && transport_this_attempt == "wss"
                        && !shared.auto_fallback_used.swap(true, Ordering::SeqCst)
                    {
                        log::info!("sidecar: wss registration failed, falling back to classic udp (auto transport)");
                        *shared.pending_transport_override.lock().expect("poisoned") = Some("udp");
                        *shared.control.lock().expect("poisoned") = ControlSignal::RestartNow;
                        // Close stdin from here too, in case the owning thread's
                        // wait() is what's blocking (same trick as restart_now()).
                        let _ = shared.stdin.lock().expect("poisoned").take();
                    }
                }
            } else if event_name == "call_state" {
                // Coarse phase for ping_state() only - the frontend gets the
                // full event (with call_id/peer/...) via the emit() below
                // regardless and does its own richer state machine.
                //
                // Scoped to this event's own call_id (2026-07-16, fixing
                // the qa-e2e R4 finding - see `CallPhase`'s doc): every
                // other in-flight call_id's tracked phase is left alone,
                // so an unrelated leg closing can never stomp on a real,
                // still-established call's state.
                let call_state = value.get("state").and_then(Value::as_str).unwrap_or("");
                if let Some(call_id) = value.get("call_id").and_then(Value::as_str) {
                    let mut phases = shared.call_phases.lock().expect("poisoned");
                    apply_call_state_transition(&mut phases, call_id, call_state);
                } else {
                    // Silently dropping this would be R4 all over again via
                    // a different trigger: a tracked call_id's own
                    // eventual "closed" arriving without a call_id would
                    // never remove its entry from `call_phases`, and
                    // has_active_call() would stay stuck reporting `true`
                    // forever. Loud (not silent) so a future engine-side
                    // protocol regression is visible instead of quietly
                    // reintroducing this class of bug.
                    log::warn!("sidecar: call_state event missing call_id, phase not updated: {value}");
                }
                if let Some(t) = shared.transcription.lock().expect("poisoned").as_ref() {
                    t.on_call_state(&value);
                }
            } else if event_name == "tap_state" {
                // F4 audio tap (core/PROTOCOL.md v1.2) - forwarded straight
                // to transcription orchestration, nothing tracked here.
                if let Some(t) = shared.transcription.lock().expect("poisoned").as_ref() {
                    t.on_tap_state(&value);
                }
            } else if event_name == "blf" {
                if let (Some(ext), Some(blf_state)) = (
                    value.get("ext").and_then(Value::as_str),
                    value.get("state").and_then(Value::as_str),
                ) {
                    shared
                        .blf_states
                        .lock()
                        .expect("poisoned")
                        .insert(ext.to_string(), blf_state.to_string());
                }
            }

            let _ = shared.app.emit(EVENT_LINE, value);
        }
    });
}

// ---------------------------------------------------------------------------
// Spawn plan / scratch config generation (Rust port of core/run-spike.sh)
// ---------------------------------------------------------------------------

struct SpawnPlan {
    binary: PathBuf,
    scratch_dir: PathBuf,
    ws_path: String,
}

impl SpawnPlan {
    fn build(
        settings: &Arc<SettingsStore>,
        account: &AccountSettings,
        transport: &str,
    ) -> Result<Self, String> {
        let binary = resolve_core_binary(settings)?;
        let module_path = binary
            .parent()
            .ok_or_else(|| "Core binary path has no parent directory".to_string())?
            .to_path_buf();

        let scratch_dir = std::env::temp_dir().join(format!(
            "centinelo-shell.{}.{}",
            std::process::id(),
            nanos_suffix()
        ));
        std::fs::create_dir_all(&scratch_dir).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&scratch_dir, std::fs::Permissions::from_mode(0o700));
        }

        let port = default_port_for(transport);
        let ws_path = "/ws".to_string();

        write_accounts_file(&scratch_dir, account, transport, port)?;
        write_config_file(&scratch_dir, &module_path, transport)?;

        Ok(Self {
            binary,
            scratch_dir,
            ws_path,
        })
    }
}

fn default_port_for(transport: &str) -> u16 {
    match transport {
        "wss" => 8089,
        "tls" => 5061,
        _ => 5060, // tcp | udp
    }
}

fn nanos_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn write_accounts_file(
    scratch_dir: &Path,
    account: &AccountSettings,
    transport: &str,
    port: u16,
) -> Result<(), String> {
    // Defense at the sink (2026-07-16 4R re-review, A1): both callers that
    // can produce an AccountSettings this function ends up spawning with
    // (commands::save_account_settings, provisioning::validate) already
    // run this same check before they persist - this second call is
    // deliberate belt-and-suspenders, not redundant: it holds regardless
    // of whether some future third caller remembers to validate before
    // writing to `AccountSettings`, since this is the one place that
    // actually builds the unescaped accounts-file line those characters
    // could break out of.
    crate::settings::validate_account_fields(&account.host, &account.ext, &account.secret, &account.display_name)?;

    // Mirrors run-spike.sh's ACCOUNT_URI/ACCOUNT_PARAMS exactly - see that
    // script for why each param is required (webrtc=yes on the endpoint
    // forces dtls_srtp/ice regardless of signaling transport; `outbound=`
    // pins the route so a bare `dial sip:ext@host` reuses the registered
    // transport instead of re-resolving to the wss/tls well-known port).
    let host = &account.host;
    let ext = &account.ext;
    let secret = &account.secret;
    let uri = format!("<sip:{ext}@{host}:{port};transport={transport}>");
    let params = format!(
        ";auth_pass={secret};mediaenc=dtls_srtp;medianat=ice;rtcp_mux=yes;audio_codecs=pcmu,pcma;regint=120;outbound=\"sip:{host}:{port};transport={transport}\""
    );
    let contents = format!("{uri}{params}\n");
    write_private_file(&scratch_dir.join("accounts"), contents.as_bytes())
}

fn write_config_file(scratch_dir: &Path, module_path: &Path, transport: &str) -> Result<(), String> {
    let _ = transport; // media requirements are unconditional, see run-spike.sh
    let module_path_str = module_path.display();
    let scratch_str = scratch_dir.display();
    let contents = format!(
        "# Generated by Centinelo Phone shell - do not edit by hand, do not commit.\n\n\
module_path\t\t{module_path_str}\n\n\
module\t\t\tg711.so\n\
module\t\t\tauconv.so\n\
module\t\t\tauresamp.so\n\
module\t\t\tausine.so\n\
module\t\t\taufile.so\n\
module\t\t\tice.so\n\
module\t\t\tdtls_srtp.so\n\
module\t\t\tmenu.so\n\
module\t\t\taccount.so\n\
module_app\t\tctrl_json.so\n\n\
sip_verify_server\tno\n\n\
audio_source\t\tausine,440\n\
audio_player\t\taufile,{scratch_str}/rx.wav\n\
audio_alert\t\taufile,{scratch_str}/rx.wav\n\n\
rtp_timeout\t\t0\n"
    );
    std::fs::write(scratch_dir.join("config"), contents).map_err(|e| e.to_string())
}

#[cfg(unix)]
fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.write_all(contents).map_err(|e| e.to_string())
}
#[cfg(not(unix))]
fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| e.to_string())
}

/// Resolves the core binary: explicit setting override first, then
/// `CENTINELO_CORE_BIN` env var, then a walk-up search from the current
/// working directory and the running executable's directory for
/// `core/deps/baresip/build/baresip` - matches this repo's dev layout
/// (`shell/` next to `core/`) without hardcoding an absolute path.
pub fn resolve_core_binary(settings: &Arc<SettingsStore>) -> Result<PathBuf, String> {
    if let Some(p) = settings.snapshot().core_binary_path {
        let pb = PathBuf::from(&p);
        return if pb.is_file() {
            Ok(pb)
        } else {
            Err(format!("Configured core binary path does not exist: {p}"))
        };
    }
    if let Some(p) = default_core_binary_path() {
        return Ok(p);
    }
    Err("Core engine binary not found. Build core/ per core/BUILD.md, or set its path in Settings > Advanced.".to_string())
}

pub fn default_core_binary_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CENTINELO_CORE_BIN") {
        let pb = PathBuf::from(&p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    let bin_name = if cfg!(windows) { "baresip.exe" } else { "baresip" };
    let roots = [
        std::env::current_dir().ok(),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf())),
    ];
    for root in roots.into_iter().flatten() {
        let mut dir = root.as_path();
        for _ in 0..10 {
            let candidate = dir.join("core/deps/baresip/build").join(bin_name);
            if candidate.is_file() {
                return Some(candidate);
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => break,
            }
        }
    }
    None
}

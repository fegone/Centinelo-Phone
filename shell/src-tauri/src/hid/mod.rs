//! HID headset support (spec §5, F4 ola 2): enumerates HID devices, finds
//! ones whose report descriptor declares Telephony Device Page (`0x0B`)
//! controls, reads their button presses, maps Hook Switch / Phone Mute
//! transitions onto this shell's *existing* `answer`/`hangup`/`mute`
//! sidecar commands (`crate::sidecar::SidecarHandle::send_cmd`,
//! `core/PROTOCOL.md`) - this module never reimplements call-control logic,
//! only translates HID bytes into the same commands the UI already sends -
//! and best-effort mirrors ring/off-hook/mute state back to the device's
//! LEDs.
//!
//! ```text
//! device.rs       - the only file touching the `hidapi` crate
//! descriptor.rs   - pure USB HID report-descriptor parser
//! mapping.rs      - pure usage-code -> action mapping + edge detection
//! led.rs          - pure LED-state -> output-report-bytes encoder
//! mod.rs (here)   - orchestration: background thread, hot-plug, wiring
//! commands.rs     - #[tauri::command]s exposed to the frontend
//! ```
//!
//! **Resilience is the point of this module, not an afterthought**: a call
//! center's headsets are optional peripherals that get plugged in, unplugged
//! mid-shift, denied OS permission, or are simply never present on a given
//! machine - none of that may ever crash the app, block startup, or wedge
//! a call. Every fallible step below (`HidApi::new()`, enumeration, open,
//! descriptor read/parse, report read, LED write) is a `Result` handled
//! locally; nothing in this module's background thread ever `unwrap()`s or
//! `panic!()`s on data that came from outside the process. See
//! `crate::hid::device`'s module doc for why the `hidapi`-touching surface
//! is kept as small as possible, and this feature's task report for exactly
//! what still needs real hardware to verify (`qa-e2e`).
//!
//! **Not covered by this session's testing** (see task report): every unit
//! test here exercises pure logic against hand-built fixtures - no real
//! telephony headset was available. What's *not* independently verified
//! against a real device: the exact descriptor shape a specific vendor
//! ships (the parser handles the general USB HID spec, but a specific
//! device could exercise a corner this session's fixtures didn't), the
//! macOS Input Monitoring permission-prompt flow end-to-end, and whether a
//! specific device's LED output report actually lights up (vs. just
//! accepting the write silently).

pub mod commands;
mod descriptor;
mod device;
mod led;
mod mapping;

use crate::settings::SettingsStore;
use crate::sidecar::SidecarHandle;
use crate::sync_ext::PoisonRecover;
use descriptor::ParsedDescriptor;
use device::DeviceSummary;
use hidapi::{HidApi, HidDevice};
use led::LedState;
use mapping::{CallPhase, HidAction, TelephonyInputState};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Listener};

/// How long to sleep between enumeration attempts while no matching device
/// is open (no headset plugged in, or the operator hasn't enabled this
/// feature). Deliberately not fast - this is a "nothing to do" idle poll,
/// not a latency-sensitive path.
const IDLE_POLL: Duration = Duration::from_millis(1500);
/// `HidDevice::read_timeout`'s own timeout while a device *is* open -
/// short enough to keep hot-unplug/settings-change latency low, long
/// enough not to busy-spin.
const READ_TIMEOUT_MS: i32 = 300;
/// Even while happily reading from an open device, re-confirm via a fresh
/// enumeration that it's still actually present, roughly this often. Belt
/// and suspenders alongside reacting to a `read` error: on at least one
/// hidapi backend a disconnected device's `read_timeout` has been known to
/// keep returning `Ok(0)` (plain timeout) rather than surfacing an error
/// immediately, which would otherwise leave a stale "Connected" status
/// showing for an unplugged headset indefinitely.
const PRESENCE_RECHECK: Duration = Duration::from_secs(2);
/// Minimum time between two dispatched HID actions - 4R review finding M1
/// (2026-07-16): edge-triggered dispatch alone doesn't stop a device
/// sending reports faster than a human could physically press a button
/// (buggy firmware re-sending the same report on a timer, or a
/// deliberately hostile USB device) from replaying answer/hangup/mute
/// against real call-center calls as fast as the bus allows.
/// `READ_TIMEOUT_MS` is a *read* timeout, not a rate limit - it doesn't
/// help here. 400ms is comfortably above any real human button-press
/// cadence (even a fast double-tap) while still feeling instant.
const MIN_ACTION_INTERVAL: Duration = Duration::from_millis(400);

/// Debounce bucket for `dispatch_action`'s rate limit - deliberately
/// coarser than `HidAction` (`MuteOn`/`MuteOff` share one bucket) so a
/// genuine toggle still debounces against itself, but distinct per
/// call-control primitive so a *global* single timestamp can't make one
/// action starve an unrelated one. Fixes a real deuda: an operator
/// answering and immediately muting inside 400ms (a completely normal
/// "pick up, go quiet" motion at a call center) used to have the `mute`
/// silently dropped because `answer` had just reset the one shared
/// timestamp - see this fix's task report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ActionKind {
    Answer,
    Hangup,
    Mute,
}

impl From<HidAction> for ActionKind {
    fn from(action: HidAction) -> Self {
        match action {
            HidAction::Answer => ActionKind::Answer,
            HidAction::Hangup => ActionKind::Hangup,
            HidAction::MuteOn | HidAction::MuteOff => ActionKind::Mute,
        }
    }
}

/// What the frontend (`hid_status` command) sees. Deliberately does not
/// distinguish "no device plugged in" from "feature not enabled but would
/// otherwise search" beyond the `Disabled`/`Searching` split - anything
/// finer belongs in `HidSettings`, which the frontend already reads
/// separately via `get_hid_settings`.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HidStatus {
    /// `HidSettings::enabled` is off.
    #[default]
    Disabled,
    /// Enabled, no matching device currently found.
    Searching,
    /// Found a matching device but couldn't open it, and the failure looks
    /// permission-shaped (e.g. macOS Input Monitoring not granted). Kept
    /// distinct from `OpenFailed` so the frontend can point the operator at
    /// System Settings specifically instead of a generic error.
    PermissionDenied { detail: String },
    /// Found a matching device but opening or reading its descriptor failed
    /// for some other reason (already open elsewhere, I/O error, ...).
    OpenFailed { detail: String },
    Connected { vendor_id: u16, product_id: u16, product_string: Option<String> },
}

struct Shared {
    app: AppHandle,
    settings: Arc<SettingsStore>,
    sidecar: SidecarHandle,
    status: Mutex<HidStatus>,
    /// Authoritative call/mute state for LED mirroring, kept independent of
    /// *who* changed it (the headset's own button, or the main window's
    /// UI) - see `listen_call_events` below. Read by the poll thread every
    /// tick, not just when device input arrives (4R review finding A2 -
    /// see `poll_once`'s doc).
    led: Mutex<LedState>,
    /// When each `ActionKind` last actually dispatched - rate-limit state
    /// for `dispatch_action`'s debounce (4R review finding M1, see
    /// `MIN_ACTION_INTERVAL`'s doc), keyed per-kind (see `ActionKind`'s doc)
    /// rather than one shared timestamp so unrelated actions never debounce
    /// each other.
    last_dispatch: Mutex<HashMap<ActionKind, Instant>>,
}

/// Cloneable handle, managed as Tauri state - same shape as
/// `SidecarHandle`/`PremiumHandle`/`TranscriptionHandle`.
#[derive(Clone)]
pub struct HidHandle(Arc<Shared>);

impl HidHandle {
    /// Spawns the background poll thread and wires up call-state listening.
    /// Never fails - even if `hidapi` itself can't initialize on this
    /// machine (unlikely, but see `HidApi::new`'s own error cases), that
    /// surfaces later as an `OpenFailed` status from the poll loop, not a
    /// constructor error that would need `lib.rs`'s `.setup()` to handle a
    /// new failure mode.
    pub fn new(app: AppHandle, settings: Arc<SettingsStore>, sidecar: SidecarHandle) -> Self {
        let shared = Arc::new(Shared {
            app: app.clone(),
            settings,
            sidecar,
            status: Mutex::new(HidStatus::Disabled),
            led: Mutex::new(LedState::default()),
            last_dispatch: Mutex::new(HashMap::new()),
        });
        let handle = Self(shared.clone());
        handle.listen_call_events();
        std::thread::spawn(move || poll_loop(shared));
        handle
    }

    /// Reacts to the same events the frontend's own call-state UI already
    /// consumes (`sidecar.rs`'s `EVENT_LINE`, `"sidecar-event"` - that
    /// constant isn't `pub`, so this is a documented string-literal
    /// coupling rather than an import; chosen over teaching `sidecar.rs`
    /// itself about HID, which would mean editing a file already shared
    /// with other in-flight work - see this feature's task report) so LED
    /// state reflects reality regardless of whether a call was
    /// answered/muted from the headset button or the on-screen UI.
    fn listen_call_events(&self) {
        let shared = self.0.clone();
        self.0.app.listen("sidecar-event", move |event| {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(event.payload()) else {
                return; // not JSON - ignore, never a panic on a malformed/foreign payload
            };
            let Some(name) = value.get("event").and_then(|v| v.as_str()) else { return };
            if name != "call_state" {
                return; // tap_state / blf / stats / ... - no LED-relevant meaning here
            }
            let mut led = shared.led.lock_or_recover();
            match value.get("state").and_then(|v| v.as_str()) {
                Some("incoming") => led.ring = true,
                Some("established") => {
                    led.ring = false;
                    led.off_hook = true;
                }
                Some("closed") => {
                    led.ring = false;
                    led.off_hook = false;
                    led.mute = false;
                }
                Some("muted") => led.mute = true,
                Some("unmuted") => led.mute = false,
                // "ringing" (outbound) / "hold" / "resumed" / anything
                // else - no LED-relevant meaning here.
                _ => {}
            }
        });
    }

    pub fn status(&self) -> HidStatus {
        self.0.status.lock_or_recover().clone()
    }

    /// Fresh enumeration for the frontend's device picker. Never panics -
    /// an `HidApi::new()` failure (no real hardware backend available,
    /// permissions, ...) just yields an empty list, same as "nothing
    /// plugged in" from this caller's perspective.
    pub fn list_candidates(&self) -> Vec<DeviceSummary> {
        match HidApi::new() {
            Ok(api) => device::enumerate(&api).iter().map(DeviceSummary::from).collect(),
            Err(e) => {
                log::warn!("hid: enumeration failed: {e}");
                Vec::new()
            }
        }
    }
}

fn set_status(shared: &Shared, status: HidStatus) {
    let mut guard = shared.status.lock_or_recover();
    if *guard != status {
        log::info!("hid: status -> {status:?}");
        *guard = status;
    }
}

/// Thin seam over the two `HidDevice` operations `poll_once` (below) needs,
/// so that function - the scheduling logic deciding *when* to read and
/// *when* to write LEDs - is unit testable with a fake implementation
/// instead of real HID hardware, the same "keep the hidapi-touching
/// surface small and isolated" principle `device.rs`'s own module doc
/// describes, applied one level up. Real HID devices (`HidDevice`) and
/// tests' fake ones both implement it; `OpenDevice` only ever holds a
/// `Box<dyn ReportPort>`, never a concrete `HidDevice`, past `try_open`.
trait ReportPort {
    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize, String>;
    fn send_output_report(&self, data: &[u8]) -> Result<(), String>;
}

impl ReportPort for HidDevice {
    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize, String> {
        HidDevice::read_timeout(self, buf, timeout_ms).map_err(|e| e.to_string())
    }
    fn send_output_report(&self, data: &[u8]) -> Result<(), String> {
        HidDevice::send_output_report(self, data).map_err(|e| e.to_string())
    }
}

/// Lets a test hand `poll_once` an `Arc<FakePort>` (so the test can keep a
/// handle to assert against after the `Box<dyn ReportPort>` has taken
/// ownership of a clone) without a second, near-identical trait impl.
impl<T: ReportPort + ?Sized> ReportPort for Arc<T> {
    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize, String> {
        (**self).read_timeout(buf, timeout_ms)
    }
    fn send_output_report(&self, data: &[u8]) -> Result<(), String> {
        (**self).send_output_report(data)
    }
}

struct OpenDevice {
    device: Box<dyn ReportPort>,
    parsed: ParsedDescriptor,
    identity: crate::settings::HidDeviceIdentity,
    product_string: Option<String>,
    max_input_report_len: usize,
    /// The settings this device was selected under - if the operator
    /// changes `selected`/`auto_detect` while this device is open, that's
    /// detected cheaply (no re-enumeration needed) by comparing against
    /// this rather than the live config every single loop tick.
    selected_at_open: Option<crate::settings::HidDeviceIdentity>,
    auto_detect_at_open: bool,
}

fn poll_loop(shared: Arc<Shared>) {
    let mut current: Option<OpenDevice> = None;
    let mut prev_state = TelephonyInputState::default();
    let mut last_presence_check = Instant::now();

    loop {
        let cfg = shared.settings.snapshot().hid;

        if !cfg.enabled {
            current = None; // dropping HidDevice closes the OS handle
            set_status(&shared, HidStatus::Disabled);
            std::thread::sleep(IDLE_POLL);
            continue;
        }

        let config_changed_under_us = current
            .as_ref()
            .map(|d| d.selected_at_open != cfg.selected || d.auto_detect_at_open != cfg.auto_detect)
            .unwrap_or(false);
        if config_changed_under_us {
            current = None;
        }

        if current.is_none() {
            match try_open(&cfg) {
                Ok(Some(opened)) => {
                    log::info!(
                        "hid: opened {:04x}:{:04x} ({:?})",
                        opened.identity.vendor_id,
                        opened.identity.product_id,
                        opened.product_string
                    );
                    set_status(
                        &shared,
                        HidStatus::Connected {
                            vendor_id: opened.identity.vendor_id,
                            product_id: opened.identity.product_id,
                            product_string: opened.product_string.clone(),
                        },
                    );
                    prev_state = TelephonyInputState::default();
                    last_presence_check = Instant::now();
                    current = Some(opened);
                }
                Ok(None) => {
                    set_status(&shared, HidStatus::Searching);
                    std::thread::sleep(IDLE_POLL);
                    continue;
                }
                Err(status) => {
                    set_status(&shared, status);
                    std::thread::sleep(IDLE_POLL);
                    continue;
                }
            }
        }

        let Some(dev) = current.as_mut() else { continue };

        if last_presence_check.elapsed() >= PRESENCE_RECHECK {
            last_presence_check = Instant::now();
            if !still_present(&dev.identity) {
                log::info!(
                    "hid: {:04x}:{:04x} no longer enumerates - treating as unplugged",
                    dev.identity.vendor_id,
                    dev.identity.product_id
                );
                current = None;
                set_status(&shared, HidStatus::Searching);
                continue;
            }
        }

        let led_now = *shared.led.lock_or_recover();
        let phase = CallPhase::from_ping_state(shared.sidecar.ping_state());
        match poll_once(dev, prev_state, led_now, phase, |action| dispatch_action(&shared, action)) {
            PollOutcome::Continue(new_state) => prev_state = new_state,
            PollOutcome::Unplugged => {
                current = None;
                set_status(&shared, HidStatus::Searching);
            }
        }
    }
}

enum PollOutcome {
    Continue(TelephonyInputState),
    Unplugged,
}

/// One loop iteration's worth of read -> (maybe dispatch) -> LED sync.
/// Entirely decoupled from `Shared`/`SidecarHandle`/`AppHandle` - takes
/// plain values (`led`, `phase`) and a `dispatch` closure instead - so it's
/// unit testable with a fake `ReportPort` and no Tauri runtime (see the
/// `poll_once_tests` module below). `poll_loop` (the real caller) supplies
/// `led`/`phase` freshly read from `Shared` every tick.
///
/// **LED sync happens on every successful read, timeout (`Ok(0)`) or real
/// data (`Ok(n)`) alike** — not just when a button was pressed. 4R review
/// finding A2 (2026-07-16): a call answered/muted from the UI, click-to-
/// call, or the console never touches this device's own Input report;
/// gating the LED write on "only after an Input report arrived" left it
/// showing stale ring/off-hook/mute state until the next physical button
/// press, which could be the entire rest of the call (or never, for calls
/// answered/handled without touching the headset at all).
fn poll_once(
    dev: &mut OpenDevice,
    prev_state: TelephonyInputState,
    led: LedState,
    phase: CallPhase,
    mut dispatch: impl FnMut(HidAction),
) -> PollOutcome {
    let mut buf = vec![0u8; dev.max_input_report_len];
    match dev.device.read_timeout(&mut buf, READ_TIMEOUT_MS) {
        Ok(0) => {
            sync_led(dev, led);
            PollOutcome::Continue(prev_state)
        }
        Ok(n) => {
            let cur_state = mapping::extract_state(&dev.parsed.fields, &buf[..n]);
            for action in mapping::diff_actions(prev_state, cur_state, phase) {
                dispatch(action);
            }
            // Merge rather than overwrite: a multi-Report-ID device's
            // single `read()` only ever returns *one* report, so a field
            // this report didn't carry must keep its last known value, not
            // silently reset to "unknown" (see
            // `mapping::TelephonyInputState`'s doc).
            let merged = TelephonyInputState { hook: cur_state.hook.or(prev_state.hook), mute: cur_state.mute.or(prev_state.mute) };
            sync_led(dev, led);
            PollOutcome::Continue(merged)
        }
        Err(e) => {
            log::warn!("hid: read failed, treating as unplugged: {e}");
            PollOutcome::Unplugged
        }
    }
}

fn still_present(identity: &crate::settings::HidDeviceIdentity) -> bool {
    match HidApi::new() {
        // No `is_plausible()`/`is_telephony()` filter here on purpose: this
        // only needs to confirm the *same physical device* (by VID/PID/
        // serial) is still enumerated *somewhere*, under any interface -
        // `dev.identity` was already validated to have real telephony
        // controls once, at open time (`try_open`'s `has_telephony_controls`
        // check); re-deriving that from scratch every 2s would mean an
        // extra open()+get_report_descriptor() per check for no benefit.
        Ok(api) => device::enumerate(&api).iter().any(|c| identity == &c.identity()),
        Err(_) => true, // couldn't ask - don't spuriously drop a working device over a transient enumeration hiccup
    }
}

/// Tries every plausible candidate, in `device::select_candidates_to_try`'s
/// priority order, until one's *real, parsed* descriptor actually has a
/// Hook Switch or Phone Mute Input field (`mapping::has_telephony_controls`).
/// See `device.rs`'s module doc for why the enumeration-level usage_page
/// pre-filter alone isn't a reliable enough final answer (a real finding
/// from this session's own dev machine). A candidate that opens fine but
/// turns out irrelevant (e.g. a plain volume-only Consumer-page cluster) is
/// silently skipped, not treated as an error - only genuine open/descriptor
/// failures accumulate into the status this function ultimately returns
/// when nothing usable was found.
fn try_open(cfg: &crate::settings::HidSettings) -> Result<Option<OpenDevice>, HidStatus> {
    let api = HidApi::new().map_err(|e| HidStatus::OpenFailed { detail: format!("hidapi init: {e}") })?;
    let candidates = device::enumerate(&api);
    let ordered = device::select_candidates_to_try(&candidates, cfg.selected.as_ref(), cfg.auto_detect);
    if ordered.is_empty() {
        return Ok(None);
    }

    let mut last_error: Option<HidStatus> = None;
    for target in ordered {
        match device::open_and_parse(&api, target) {
            Ok((device, parsed)) => {
                if !mapping::has_telephony_controls(&parsed.fields) {
                    // Opened fine, but no Hook Switch/Phone Mute anywhere -
                    // e.g. a Consumer-page volume-only cluster. Not our
                    // device; try the next candidate rather than settling.
                    continue;
                }
                let max_len = parsed
                    .report_bit_lengths
                    .iter()
                    .filter(|((_, kind), _)| *kind == descriptor::MainKind::Input)
                    .map(|(_, bits)| (*bits as usize).div_ceil(8) + 1 /* room for a leading report-id byte */)
                    .max()
                    .unwrap_or(64)
                    .max(64);
                return Ok(Some(OpenDevice {
                    identity: target.identity(),
                    product_string: target.product_string.clone(),
                    device: Box::new(device),
                    parsed,
                    max_input_report_len: max_len,
                    selected_at_open: cfg.selected.clone(),
                    auto_detect_at_open: cfg.auto_detect,
                }));
            }
            Err(detail) => {
                let lower = detail.to_lowercase();
                let looks_like_permission = lower.contains("permission") || lower.contains("access") || lower.contains("denied");
                last_error = Some(if looks_like_permission {
                    HidStatus::PermissionDenied { detail }
                } else {
                    HidStatus::OpenFailed { detail }
                });
                // Keep trying the rest - one interface being exclusively
                // locked by another driver (very common: a mouse/keyboard's
                // other HID interfaces routinely are) shouldn't stop this
                // loop from reaching an actual telephony candidate further
                // down the list.
            }
        }
    }

    match last_error {
        // Every plausible candidate failed to even open - surface the last
        // failure so the operator/qa-e2e has something actionable (e.g. a
        // macOS permission prompt to grant).
        Some(status) => Err(status),
        // Every plausible candidate opened fine, none had telephony
        // controls - genuinely nothing to connect to, not an error.
        None => Ok(None),
    }
}

/// Pure debounce decision, factored out of `dispatch_action` specifically
/// so it's unit-testable without real clock timing (`Duration`s in, `bool`
/// out - no `Instant`). `elapsed_since_last` is `None` on the very first
/// action this session (always allowed) or `Some(d)` for how long it's
/// been since the last one actually dispatched.
fn should_dispatch(elapsed_since_last: Option<Duration>) -> bool {
    match elapsed_since_last {
        Some(d) => d >= MIN_ACTION_INTERVAL,
        None => true,
    }
}

/// Per-`ActionKind` debounce gate: looks up (and, if allowed, stamps) only
/// *this* action's bucket in `last_dispatch`, so `dispatch_action`'s lock
/// holder never has to reason about other kinds. Takes the map directly
/// (rather than the whole `Shared`) so a test can exercise the real
/// `HashMap`/`Instant` bookkeeping without needing a `Shared` (which needs
/// a live `AppHandle`/`SidecarHandle` this module has no fake for) - only
/// `should_dispatch` above needed that treatment for `dispatch_action`
/// itself, this is the same idea one level up.
fn gate(last_dispatch: &mut HashMap<ActionKind, Instant>, action: HidAction) -> bool {
    let kind = ActionKind::from(action);
    let elapsed = last_dispatch.get(&kind).map(|t: &Instant| t.elapsed());
    if !should_dispatch(elapsed) {
        return false;
    }
    last_dispatch.insert(kind, Instant::now());
    true
}

fn dispatch_action(shared: &Shared, action: HidAction) {
    {
        let mut last = shared.last_dispatch.lock_or_recover();
        if !gate(&mut last, action) {
            // 4R review finding M1: rate-limit, not just edge-trigger - a
            // device (buggy firmware or a deliberately hostile USB
            // peripheral) resending reports faster than any human could
            // press a button must not be able to replay answer/hangup/mute
            // against a real call at bus speed. Per-`ActionKind` (see its
            // doc) so a genuine answer-then-mute inside 400ms isn't dropped.
            log::warn!(
                "hid: rate-limiting {action:?} - another {:?} action dispatched less than {MIN_ACTION_INTERVAL:?} ago",
                ActionKind::from(action)
            );
            return;
        }
    }
    log::info!("hid: dispatching {action:?}");
    let cmd = match action {
        HidAction::Answer => serde_json::json!({ "cmd": "answer" }),
        HidAction::Hangup => serde_json::json!({ "cmd": "hangup" }),
        HidAction::MuteOn => serde_json::json!({ "cmd": "mute", "on": true }),
        HidAction::MuteOff => serde_json::json!({ "cmd": "mute", "on": false }),
    };
    // Same `SidecarHandle::send_cmd` the UI's own `#[tauri::command]`
    // wrappers call (`commands.rs` sidecar_answer/sidecar_hangup/
    // sidecar_mute) - this module never talks to the sidecar any other
    // way, so there is exactly one place that knows how to answer/hang up/
    // mute a call, regardless of whether a button or the headset triggered
    // it.
    if let Err(e) = shared.sidecar.send_cmd(cmd) {
        // Not a call-center emergency - e.g. the button was pressed right
        // as the call ended on the far end. Logged, not surfaced to the
        // operator as an error toast; core/PROTOCOL.md's own `error` event
        // (if the sidecar is actually up) already covers the "no current
        // call" case for anything watching the normal event stream.
        log::info!("hid: {action:?} not delivered: {e}");
    }
}

fn sync_led(dev: &OpenDevice, led: LedState) {
    if let Some(report) = led::build_output_report(&dev.parsed, led) {
        if let Err(e) = dev.device.send_output_report(&report) {
            // Best-effort by design (module doc) - most cheap USB
            // telephony adapters have no LED at all, and hidapi's
            // send_output_report can fail for entirely benign reasons
            // (device asleep, transient USB hiccup) that shouldn't spam
            // the log every ~300ms. One-line, not per-attempt noise.
            log::debug!("hid: LED write failed (non-fatal): {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_status_default_is_disabled() {
        assert_eq!(HidStatus::default(), HidStatus::Disabled);
    }

    #[test]
    fn hid_status_serializes_with_a_state_tag() {
        let json = serde_json::to_value(HidStatus::Connected { vendor_id: 1, product_id: 2, product_string: None }).unwrap();
        assert_eq!(json["state"], "connected");
        assert_eq!(json["vendor_id"], 1);
    }

    // ---- should_dispatch (4R finding M1: debounce) -----------------------

    #[test]
    fn should_dispatch_true_for_the_first_action_this_session() {
        assert!(should_dispatch(None));
    }

    #[test]
    fn should_dispatch_false_within_the_minimum_interval() {
        assert!(!should_dispatch(Some(Duration::from_millis(0))));
        assert!(!should_dispatch(Some(MIN_ACTION_INTERVAL - Duration::from_millis(1))));
    }

    #[test]
    fn should_dispatch_true_at_or_beyond_the_minimum_interval() {
        assert!(should_dispatch(Some(MIN_ACTION_INTERVAL)));
        assert!(should_dispatch(Some(Duration::from_secs(5))));
    }

    // ---- gate (per-ActionKind debounce, fixes global-debounce deuda) ------

    #[test]
    fn gate_allows_a_different_action_kind_within_the_debounce_window() {
        // The bug this fixes: answer, then mute <400ms later - a completely
        // normal "pick up, go quiet" motion - used to have the mute
        // silently dropped because `answer` had just reset the one shared
        // timestamp. Answer and Mute are different `ActionKind`s, so both
        // must go through even back-to-back.
        let mut last_dispatch = HashMap::new();
        assert!(gate(&mut last_dispatch, HidAction::Answer));
        assert!(gate(&mut last_dispatch, HidAction::MuteOn));
    }

    #[test]
    fn gate_still_rate_limits_the_same_action_kind() {
        let mut last_dispatch = HashMap::new();
        assert!(gate(&mut last_dispatch, HidAction::Answer));
        // Immediately repeating the *same* kind must still be rejected -
        // per-kind debounce isn't a way to disable the M1 rate limit.
        assert!(!gate(&mut last_dispatch, HidAction::Answer));
    }

    #[test]
    fn gate_treats_mute_on_and_mute_off_as_the_same_bucket() {
        // MuteOn/MuteOff share one ActionKind::Mute bucket (see its doc) -
        // a genuine on/off toggle inside the debounce window is exactly
        // the "resending faster than a human could" shape M1 guards
        // against, so it debounces same as two Answers in a row would.
        let mut last_dispatch = HashMap::new();
        assert!(gate(&mut last_dispatch, HidAction::MuteOn));
        assert!(!gate(&mut last_dispatch, HidAction::MuteOff));
    }

    #[test]
    fn gate_allows_the_same_kind_again_once_the_window_has_passed() {
        let mut last_dispatch = HashMap::new();
        // Backdate as if the last Answer dispatched just past the window -
        // Instant subtraction is the standard way to simulate elapsed time
        // in these tests without a mockable clock.
        last_dispatch.insert(ActionKind::Answer, Instant::now() - MIN_ACTION_INTERVAL - Duration::from_millis(1));
        assert!(gate(&mut last_dispatch, HidAction::Answer));
    }
}

#[cfg(test)]
mod poll_once_tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex as StdMutex;

    /// A `ReportPort` test double: `read_timeout` replays a canned queue of
    /// outcomes (an exhausted queue behaves like a real idle device -
    /// `Ok(0)`, a plain timeout, forever after); `send_output_report`
    /// records every write for assertions. `Arc`-wrapped so the test can
    /// keep its own handle after a clone goes into `OpenDevice` as the
    /// trait object (see `impl<T: ReportPort> ReportPort for Arc<T>` above).
    struct FakePort {
        reads: StdMutex<VecDeque<Result<Vec<u8>, String>>>,
        writes: StdMutex<Vec<Vec<u8>>>,
    }
    impl FakePort {
        fn new(reads: Vec<Result<Vec<u8>, String>>) -> Self {
            Self { reads: StdMutex::new(reads.into()), writes: StdMutex::new(Vec::new()) }
        }
        fn write_count(&self) -> usize {
            self.writes.lock().unwrap().len()
        }
    }
    impl ReportPort for FakePort {
        fn read_timeout(&self, buf: &mut [u8], _timeout_ms: i32) -> Result<usize, String> {
            match self.reads.lock().unwrap().pop_front() {
                Some(Ok(bytes)) => {
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                Some(Err(e)) => Err(e),
                None => Ok(0), // exhausted queue = idle device, plain timeout
            }
        }
        fn send_output_report(&self, data: &[u8]) -> Result<(), String> {
            self.writes.lock().unwrap().push(data.to_vec());
            Ok(())
        }
    }

    /// A minimal `OpenDevice` whose descriptor has exactly one Output LED
    /// field (Off-Hook) - enough for `led::build_output_report` to always
    /// have something to write, so these tests can assert on write count
    /// without depending on `led.rs`'s own field-selection details.
    fn open_device_with(port: Arc<FakePort>) -> OpenDevice {
        let mut report_bit_lengths = HashMap::new();
        report_bit_lengths.insert((None, descriptor::MainKind::Output), 8u32);
        let parsed = ParsedDescriptor {
            fields: vec![descriptor::FieldLocation {
                kind: descriptor::MainKind::Output,
                report_id: None,
                usage_page: mapping::USAGE_PAGE_LED,
                usage: mapping::LED_OFF_HOOK,
                bit_offset: 0,
                bit_length: 1,
            }],
            report_bit_lengths,
        };
        OpenDevice {
            device: Box::new(port),
            parsed,
            identity: crate::settings::HidDeviceIdentity { vendor_id: 1, product_id: 2, serial_number: None },
            product_string: None,
            max_input_report_len: 8,
            selected_at_open: None,
            auto_detect_at_open: true,
        }
    }

    #[test]
    fn led_syncs_on_a_timeout_tick_with_no_device_input_at_all() {
        // 4R review finding A2: no queued reads at all - every
        // read_timeout() call is a plain Ok(0) timeout, exactly like an
        // idle device the operator never touched, which is exactly the
        // "call answered/muted from the UI, not the headset" case the
        // finding was about. LED must still be written.
        let port = Arc::new(FakePort::new(vec![]));
        let mut dev = open_device_with(Arc::clone(&port));

        let outcome = poll_once(&mut dev, TelephonyInputState::default(), LedState { off_hook: true, ..Default::default() }, CallPhase::Active, |_| {
            panic!("no device input was queued - nothing should have dispatched")
        });

        assert!(matches!(outcome, PollOutcome::Continue(_)));
        assert_eq!(port.write_count(), 1, "LED must sync even on a pure timeout tick");
    }

    #[test]
    fn led_syncs_again_on_every_subsequent_timeout_tick() {
        // Not just once - every idle tick keeps the LED synced, since the
        // call-state driving `led` can change between any two ticks.
        let port = Arc::new(FakePort::new(vec![]));
        let mut dev = open_device_with(Arc::clone(&port));
        let mut state = TelephonyInputState::default();
        for _ in 0..3 {
            match poll_once(&mut dev, state, LedState::default(), CallPhase::Idle, |_| {}) {
                PollOutcome::Continue(s) => state = s,
                PollOutcome::Unplugged => panic!("fake port never errors"),
            }
        }
        assert_eq!(port.write_count(), 3);
    }

    #[test]
    fn led_also_syncs_when_real_device_input_arrives() {
        // Regression coverage for the Ok(n) path (worked before this 4R
        // pass too, but now goes through the same shared sync_led call as
        // the Ok(0) path - worth asserting explicitly).
        let port = Arc::new(FakePort::new(vec![Ok(vec![0b0000_0000])]));
        let mut dev = open_device_with(Arc::clone(&port));
        let _ = poll_once(&mut dev, TelephonyInputState::default(), LedState::default(), CallPhase::Idle, |_| {});
        assert_eq!(port.write_count(), 1);
    }

    #[test]
    fn a_read_error_reports_unplugged_and_never_syncs_led() {
        let port = Arc::new(FakePort::new(vec![Err("device disconnected".to_string())]));
        let mut dev = open_device_with(Arc::clone(&port));
        let outcome = poll_once(&mut dev, TelephonyInputState::default(), LedState::default(), CallPhase::Idle, |_| {});
        assert!(matches!(outcome, PollOutcome::Unplugged));
        assert_eq!(port.write_count(), 0);
    }

    #[test]
    fn device_input_dispatches_the_expected_action_via_the_closure() {
        // Hook Switch (usage 0x20) at bit 0, no Report ID - matches
        // mapping.rs's own fixture shape. Going off-hook while ringing
        // must dispatch exactly one Answer through the closure.
        let mut report_bit_lengths = HashMap::new();
        report_bit_lengths.insert((None, descriptor::MainKind::Input), 1u32);
        let parsed = ParsedDescriptor {
            fields: vec![descriptor::FieldLocation {
                kind: descriptor::MainKind::Input,
                report_id: None,
                usage_page: mapping::USAGE_PAGE_TELEPHONY,
                usage: mapping::USAGE_HOOK_SWITCH,
                bit_offset: 0,
                bit_length: 1,
            }],
            report_bit_lengths,
        };
        let port = Arc::new(FakePort::new(vec![Ok(vec![0b0000_0001])]));
        let mut dev = OpenDevice {
            device: Box::new(port),
            parsed,
            identity: crate::settings::HidDeviceIdentity { vendor_id: 1, product_id: 2, serial_number: None },
            product_string: None,
            max_input_report_len: 8,
            selected_at_open: None,
            auto_detect_at_open: true,
        };
        let mut dispatched = Vec::new();
        let _ = poll_once(&mut dev, TelephonyInputState { hook: Some(false), mute: None }, LedState::default(), CallPhase::Ringing, |a| dispatched.push(a));
        assert_eq!(dispatched, vec![HidAction::Answer]);
    }
}

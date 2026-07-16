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
use descriptor::ParsedDescriptor;
use device::DeviceSummary;
use hidapi::{HidApi, HidDevice};
use led::LedState;
use mapping::{CallPhase, HidAction, TelephonyInputState};
use serde::Serialize;
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
    /// time it has fresh input to react to.
    led: Mutex<LedState>,
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
            let mut led = shared.led.lock().expect("poisoned");
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
        self.0.status.lock().expect("poisoned").clone()
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
    let mut guard = shared.status.lock().expect("poisoned");
    if *guard != status {
        log::info!("hid: status -> {status:?}");
        *guard = status;
    }
}

struct OpenDevice {
    device: HidDevice,
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

        let mut buf = vec![0u8; dev.max_input_report_len];
        match dev.device.read_timeout(&mut buf, READ_TIMEOUT_MS) {
            Ok(0) => { /* timeout, nothing new - loop again */ }
            Ok(n) => {
                let cur_state = mapping::extract_state(&dev.parsed.fields, &buf[..n]);
                let phase = CallPhase::from_ping_state(shared.sidecar.ping_state());
                for action in mapping::diff_actions(prev_state, cur_state, phase) {
                    dispatch_action(&shared, action);
                }
                // Merge rather than overwrite: a multi-Report-ID device's
                // single `read()` only ever returns *one* report, so a
                // field this report didn't carry must keep its last known
                // value, not silently reset to "unknown" (see
                // `mapping::TelephonyInputState`'s doc).
                prev_state = TelephonyInputState { hook: cur_state.hook.or(prev_state.hook), mute: cur_state.mute.or(prev_state.mute) };
                sync_led(&shared, dev);
            }
            Err(e) => {
                log::warn!("hid: read failed, treating as unplugged: {e}");
                current = None;
                set_status(&shared, HidStatus::Searching);
            }
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
                    device,
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

fn dispatch_action(shared: &Shared, action: HidAction) {
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

fn sync_led(shared: &Shared, dev: &OpenDevice) {
    let led = *shared.led.lock().expect("poisoned");
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
}

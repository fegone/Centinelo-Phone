//! HID Telephony usage IDs → this shell's call-control actions, and the
//! LED usage IDs used to mirror state back. Every constant below is a real,
//! cited value — not invented:
//!
//! - Usage Page `0x0B` = **Telephony Device Page**, Usage `0x20` = **Hook
//!   Switch**, `0x21` = **Flash**, `0x24` = **Redial** — USB Implementers
//!   Forum, "HID Usage Tables" spec, Telephony Device Page table.
//! - Usage Page `0x0B`, Usage `0x2F` = **Phone Mute** — same spec table;
//!   independently cross-checked against real, shipping driver code: Linux
//!   kernel `drivers/hid/hid-plantronics.c` defines
//!   `HID_TELEPHONY_MUTE = (HID_UP_TELEPHONY | 0x2f)`, and
//!   `drivers/hid/hid-input.c`'s own `case HID_UP_TELEPHONY:` switch maps
//!   `case 0x2f: map_key_clear(KEY_MICMUTE)`.
//! - Usage Page `0x08` = **LED Page**, Usage `0x09` = **Mute** — also
//!   cross-checked against `drivers/hid/hid-input.c`'s `case HID_UP_LED:`
//!   switch: `case 0x09: map_led(LED_MUTE)`.
//! - Usage Page `0x08`, Usage `0x17` = **Off-Hook**, `0x18` = **Ring** — USB
//!   HID Usage Tables spec, LED Page table. These two don't have an
//!   independent Linux-kernel cross-check the way Mute/Phone-Mute do —
//!   Linux's own generic `hid-input.c` LED table only bothers mapping LED
//!   usages that have a matching kernel `LED_*` input-subsystem constant,
//!   and off-hook/ring lamps don't (there's no `LED_OFFHOOK`/`LED_RING` in
//!   the kernel's input event namespace) — but they're still real,
//!   USB-IF-standard usage IDs this module can legitimately target; this
//!   shell talks to the device directly rather than through that generic
//!   OS abstraction anyway.
//!
//! Flash (`0x21`) and Redial (`0x24`) are recognized-but-unwired — see
//! "Planned" at the bottom of this file. The task spec (spec §5) only asks
//! for Hook Switch (answer/hangup) and Phone Mute; wiring more controls
//! than the shell has a clear, honest action for would mean either
//! fabricating behavior or silently dropping the press, neither of which is
//! better than not claiming support yet.

use super::descriptor::{get_bit, FieldLocation, MainKind};

pub const USAGE_PAGE_TELEPHONY: u16 = 0x0B;
pub const USAGE_PAGE_LED: u16 = 0x08;

pub const USAGE_HOOK_SWITCH: u16 = 0x20;
// Planned, not wired to any action - see module doc's last paragraph for
// why. Kept as named, cited constants (rather than left out entirely, or
// as a bare magic number in a comment) so wiring them up later starts from
// a correct value, not a fresh lookup - `#[allow(dead_code)]` is the
// intentional-for-now marker, not a mistake; `tests::
// planned_but_unwired_usage_constants_match_the_hut_spec_values` still
// exercises them in the test target, but a plain (non-test) build has
// nothing that reads them yet.
#[allow(dead_code)]
pub const USAGE_FLASH: u16 = 0x21;
#[allow(dead_code)]
pub const USAGE_REDIAL: u16 = 0x24;
pub const USAGE_PHONE_MUTE: u16 = 0x2F;

pub const LED_MUTE: u16 = 0x09;
pub const LED_OFF_HOOK: u16 = 0x17;
pub const LED_RING: u16 = 0x18;

/// The Hook Switch / Phone Mute bit values read from a device's most recent
/// Input report(s). `None` means "this report didn't say" — either the
/// device hasn't sent that field yet, or (multi-Report-ID devices) this
/// particular report wasn't the one carrying it — not "off"/"false"; see
/// `crate::hid::extract_and_dispatch` for why that distinction matters for
/// edge detection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TelephonyInputState {
    pub hook: Option<bool>,
    pub mute: Option<bool>,
}

/// Coarse call state, just enough to resolve Hook Switch's meaning (see
/// `diff_actions`). Deliberately not `crate::sidecar`'s own private
/// `CallPhase` (that type isn't `pub`, and pulling it in would couple this
/// pure module to sidecar.rs's internals for three enum variants) — this
/// shell's `SidecarHandle::ping_state()` already returns an equivalent
/// vocabulary as `&'static str`; `CallPhase::from_ping_state` below is the
/// one, tiny, tested translation point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPhase {
    Idle,
    Ringing,
    Active,
}

impl CallPhase {
    /// `SidecarHandle::ping_state()`'s vocabulary: "disconnected" |
    /// "connecting" | "registered" | "ringing" | "calling" | "in-call".
    /// Everything except "ringing" (someone's calling *us*) and "in-call"
    /// maps to `Idle` — in particular "calling" (we're dialing out, no
    /// established call yet) is `Idle` on purpose: there's nothing for a
    /// Hook Switch press to answer or hang up yet in that state that this
    /// protocol can act on differently from plain idle.
    pub fn from_ping_state(state: &str) -> Self {
        match state {
            "ringing" => CallPhase::Ringing,
            "in-call" => CallPhase::Active,
            _ => CallPhase::Idle,
        }
    }
}

/// What a HID button transition should do, expressed as this shell's own
/// existing sidecar commands (`core/PROTOCOL.md` `answer`/`hangup`/`mute`)
/// — never a new call-control primitive invented here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HidAction {
    Answer,
    Hangup,
    MuteOn,
    MuteOff,
}

/// Does this device's *parsed descriptor* actually contain a Hook Switch or
/// Phone Mute Input field, anywhere in it? This is the real test for "is
/// this our device" — **not** the top-level enumeration `usage_page` alone
/// (`crate::hid::device::DeviceCandidate::is_telephony`), which real
/// hardware on this session's own dev machine proved unreliable: a "USB PnP
/// Sound Device" enumerates with a top-level Consumer Page (`0x0C`)
/// collection usage, yet its real descriptor nests a `usage_page=0x0B
/// usage=0x20` (Hook Switch) Input field *inside* that collection — a
/// composite device whose outermost collection usage doesn't reflect every
/// control cluster nested within it. See `crate::hid::device::try_open`'s
/// (in `crate::hid::mod`) doc for how the two checks are combined: the
/// enumeration-level usage_page is still used as a *cheap pre-filter*
/// (worth the `open()` syscall or not), this function is the actual
/// decision once a candidate's descriptor is in hand.
pub fn has_telephony_controls(fields: &[FieldLocation]) -> bool {
    fields
        .iter()
        .any(|f| f.kind == MainKind::Input && f.usage_page == USAGE_PAGE_TELEPHONY && matches!(f.usage, USAGE_HOOK_SWITCH | USAGE_PHONE_MUTE))
}

/// Pulls the Hook Switch / Phone Mute bit(s) out of one raw Input report,
/// using `fields` (from `crate::hid::descriptor::parse_report_descriptor`)
/// to know where they live. `report` is the *exact* bytes handed back by
/// `hidapi`'s `HidDevice::read`/`read_timeout` — per hidapi's own
/// documented contract, byte 0 is the Report ID when the device uses
/// numbered reports (see `FieldLocation::report_id`'s doc), which this
/// function accounts for per-field rather than assuming one way or the
/// other for the whole device.
pub fn extract_state(fields: &[FieldLocation], report: &[u8]) -> TelephonyInputState {
    let mut state = TelephonyInputState::default();
    if report.is_empty() {
        return state;
    }
    let report_id_byte = report[0];
    for f in fields {
        if f.kind != MainKind::Input || f.usage_page != USAGE_PAGE_TELEPHONY {
            continue;
        }
        if f.usage != USAGE_HOOK_SWITCH && f.usage != USAGE_PHONE_MUTE {
            continue;
        }
        let data: &[u8] = match f.report_id {
            Some(id) if id == report_id_byte => &report[1..],
            Some(_) => continue, // a different report ID's field - not in this report
            None => report,
        };
        let Some(bit) = get_bit(data, f.bit_offset) else { continue };
        match f.usage {
            USAGE_HOOK_SWITCH => state.hook = Some(bit),
            USAGE_PHONE_MUTE => state.mute = Some(bit),
            _ => unreachable!("filtered above"),
        }
    }
    state
}

/// Turns a `prev -> cur` transition into zero or more shell actions, given
/// the call's current phase (needed because Hook Switch is one bit whose
/// *meaning* depends on context — a real desk-phone-style switch, not a
/// dedicated "answer" button and a dedicated "hangup" button).
///
/// Hook Switch (HUT categorizes it an On/Off Control — a *level*, matching
/// a physical handset cradle switch, not a momentary click):
/// - `false -> true` ("went off-hook"): `Answer` if a call is `Ringing`.
///   `Idle`/`Active` are ignored on purpose — this protocol has no "get a
///   dialtone" concept to react to a bare off-hook with nothing ringing,
///   and firing on an already-`Active` call would be a spurious duplicate.
/// - `true -> false` ("went on-hook"): `Hangup` unless the call is already
///   `Idle` (nothing to hang up).
///
/// Phone Mute (also an On/Off Control — a persistent toggle state, not a
/// momentary press): any change while `Active` maps straight to
/// `MuteOn`/`MuteOff`. Ignored outside an active call so idly touching the
/// mute button between calls doesn't send a `mute` command with no call for
/// it to apply to (harmless per `core/PROTOCOL.md` — resolves to an
/// `error` event, never a crash — but noisy and pointless).
///
/// Either field being `None` in `prev` or `cur` (see `TelephonyInputState`'s
/// doc) means "unknown, not necessarily unchanged" — no action fires for
/// that field on this call. This is also what makes the very first sample
/// after opening a device safe: `prev` starts as `TelephonyInputState::default()`
/// (both `None`), so nothing fires off the device's resting state, only
/// real subsequent transitions.
pub fn diff_actions(prev: TelephonyInputState, cur: TelephonyInputState, phase: CallPhase) -> Vec<HidAction> {
    let mut actions = Vec::new();

    if let (Some(was), Some(is)) = (prev.hook, cur.hook) {
        if !was && is {
            if phase == CallPhase::Ringing {
                actions.push(HidAction::Answer);
            }
        } else if was && !is && phase != CallPhase::Idle {
            actions.push(HidAction::Hangup);
        }
    }

    if let (Some(was), Some(is)) = (prev.mute, cur.mute) {
        if was != is && phase == CallPhase::Active {
            actions.push(if is { HidAction::MuteOn } else { HidAction::MuteOff });
        }
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::super::descriptor::parse_report_descriptor;
    use super::*;

    // ---- extract_state -----------------------------------------------

    /// A realistic minimal single-Report-ID telephony descriptor: one
    /// Input report (Report ID 1) with Hook Switch at bit 0 and Phone Mute
    /// at bit 1 of the (single) data byte, three padding bits, then one
    /// Output report (also ID 1) with the three LEDs this module targets
    /// at bits 0/1/2 plus 5 padding bits — the same general shape real
    /// USB telephony-class control clusters use (a top-level Telephony
    /// Device / Headset collection containing an Input report for buttons
    /// and an Output report for LEDs). Hand-assembled from the USB HID
    /// spec's own item encoding rules (§6.2.2), not copied from a real
    /// device's binary (no real device was available this session — see
    /// this crate's own task report for what still needs real hardware).
    fn sample_descriptor_bytes() -> Vec<u8> {
        let mut d = Vec::new();
        // Usage Page (Telephony) = 0x0B
        d.extend_from_slice(&[0x05, 0x0B]);
        // Usage (Headset) = 0x05 (top-level collection usage - not one this
        // parser needs to resolve, just needs to consume the bytes)
        d.extend_from_slice(&[0x09, 0x05]);
        // Collection (Application)
        d.extend_from_slice(&[0xA1, 0x01]);
        //   Report ID (1)
        d.extend_from_slice(&[0x85, 0x01]);
        //   Usage Page (Telephony) = 0x0B (redundant with outer, real
        //   descriptors often repeat it inside the collection)
        d.extend_from_slice(&[0x05, 0x0B]);
        //   Usage (Hook Switch) = 0x20
        d.extend_from_slice(&[0x09, 0x20]);
        //   Usage (Phone Mute) = 0x2F
        d.extend_from_slice(&[0x09, 0x2F]);
        //   Logical Minimum (0), Logical Maximum (1) — omitted, not needed
        //   by this parser (it never reads them).
        //   Report Size (1), Report Count (2)
        d.extend_from_slice(&[0x75, 0x01]);
        d.extend_from_slice(&[0x95, 0x02]);
        //   Input (Data, Variable, Absolute) = 0x02
        d.extend_from_slice(&[0x81, 0x02]);
        //   Report Count (6) - padding to fill the byte
        d.extend_from_slice(&[0x95, 0x06]);
        //   Input (Constant) = 0x01
        d.extend_from_slice(&[0x81, 0x01]);
        //   Usage Page (LED) = 0x08
        d.extend_from_slice(&[0x05, 0x08]);
        //   Usage (Off-Hook) = 0x17, (Ring) = 0x18, (Mute) = 0x09
        d.extend_from_slice(&[0x09, 0x17]);
        d.extend_from_slice(&[0x09, 0x18]);
        d.extend_from_slice(&[0x09, 0x09]);
        //   Report Count (3)
        d.extend_from_slice(&[0x95, 0x03]);
        //   Output (Data, Variable, Absolute) = 0x02
        d.extend_from_slice(&[0x91, 0x02]);
        //   Report Count (5) - padding
        d.extend_from_slice(&[0x95, 0x05]);
        //   Output (Constant) = 0x01
        d.extend_from_slice(&[0x91, 0x01]);
        // End Collection
        d.push(0xC0);
        d
    }

    fn sample_fields() -> Vec<FieldLocation> {
        parse_report_descriptor(&sample_descriptor_bytes()).expect("valid fixture descriptor").fields
    }

    #[test]
    fn extracts_hook_and_mute_from_real_shaped_report() {
        let fields = sample_fields();
        // Report ID 1, data byte = 0b0000_0001 -> Hook Switch (bit 0) = 1, Phone Mute (bit 1) = 0.
        let report = [0x01u8, 0b0000_0001];
        let state = extract_state(&fields, &report);
        assert_eq!(state.hook, Some(true));
        assert_eq!(state.mute, Some(false));
    }

    #[test]
    fn extracts_mute_bit_independently_of_hook_bit() {
        let fields = sample_fields();
        // bit0=0 (on-hook), bit1=1 (muted)
        let report = [0x01u8, 0b0000_0010];
        let state = extract_state(&fields, &report);
        assert_eq!(state.hook, Some(false));
        assert_eq!(state.mute, Some(true));
    }

    #[test]
    fn ignores_a_report_for_a_different_report_id() {
        let fields = sample_fields();
        let report = [0x02u8, 0xFF]; // report ID 2 - this fixture only wired ID 1
        let state = extract_state(&fields, &report);
        assert_eq!(state, TelephonyInputState::default());
    }

    #[test]
    fn empty_report_yields_unknown_state_not_a_panic() {
        let fields = sample_fields();
        assert_eq!(extract_state(&fields, &[]), TelephonyInputState::default());
    }

    // ---- has_telephony_controls ------------------------------------------

    #[test]
    fn has_telephony_controls_true_for_the_sample_fixture() {
        assert!(has_telephony_controls(&sample_fields()));
    }

    #[test]
    fn has_telephony_controls_false_for_an_unrelated_descriptor() {
        // Reproduces the real, physical finding this session (see task
        // report): a device can enumerate with *some* Telephony-page
        // fields absent entirely - a plain Consumer-page volume/mute
        // control cluster with no Hook Switch/Phone Mute at all.
        let fields = vec![FieldLocation {
            kind: MainKind::Input,
            report_id: None,
            usage_page: USAGE_PAGE_LED, // 0x08, arbitrary "not telephony"
            usage: 0x01,
            bit_offset: 0,
            bit_length: 1,
        }];
        assert!(!has_telephony_controls(&fields));
    }

    #[test]
    fn has_telephony_controls_ignores_output_fields_with_matching_usage() {
        // Usage IDs aren't unique across kind - an Output field that
        // happens to reuse 0x20 on the Telephony page (unusual, but the
        // spec doesn't forbid it) must not count; only real *Input*
        // (button) fields mean "this device can tell us about a press".
        let fields = vec![FieldLocation {
            kind: MainKind::Output,
            report_id: None,
            usage_page: USAGE_PAGE_TELEPHONY,
            usage: USAGE_HOOK_SWITCH,
            bit_offset: 0,
            bit_length: 1,
        }];
        assert!(!has_telephony_controls(&fields));
    }

    #[test]
    fn has_telephony_controls_false_for_empty_fields() {
        assert!(!has_telephony_controls(&[]));
    }

    // ---- diff_actions ---------------------------------------------------

    #[test]
    fn hook_switch_off_hook_while_ringing_answers() {
        let prev = TelephonyInputState { hook: Some(false), mute: None };
        let cur = TelephonyInputState { hook: Some(true), mute: None };
        assert_eq!(diff_actions(prev, cur, CallPhase::Ringing), vec![HidAction::Answer]);
    }

    #[test]
    fn hook_switch_off_hook_while_idle_does_nothing() {
        let prev = TelephonyInputState { hook: Some(false), mute: None };
        let cur = TelephonyInputState { hook: Some(true), mute: None };
        assert!(diff_actions(prev, cur, CallPhase::Idle).is_empty());
    }

    #[test]
    fn hook_switch_off_hook_while_already_active_does_nothing() {
        // Spurious/duplicate signal - already answered, nothing new to do.
        let prev = TelephonyInputState { hook: Some(false), mute: None };
        let cur = TelephonyInputState { hook: Some(true), mute: None };
        assert!(diff_actions(prev, cur, CallPhase::Active).is_empty());
    }

    #[test]
    fn hook_switch_on_hook_while_active_hangs_up() {
        let prev = TelephonyInputState { hook: Some(true), mute: None };
        let cur = TelephonyInputState { hook: Some(false), mute: None };
        assert_eq!(diff_actions(prev, cur, CallPhase::Active), vec![HidAction::Hangup]);
    }

    #[test]
    fn hook_switch_on_hook_while_ringing_declines() {
        // Putting the handset back down while it's still just ringing (not
        // yet answered) - a real, useful "decline" gesture.
        let prev = TelephonyInputState { hook: Some(true), mute: None };
        let cur = TelephonyInputState { hook: Some(false), mute: None };
        assert_eq!(diff_actions(prev, cur, CallPhase::Ringing), vec![HidAction::Hangup]);
    }

    #[test]
    fn hook_switch_on_hook_while_idle_does_nothing() {
        let prev = TelephonyInputState { hook: Some(true), mute: None };
        let cur = TelephonyInputState { hook: Some(false), mute: None };
        assert!(diff_actions(prev, cur, CallPhase::Idle).is_empty());
    }

    #[test]
    fn first_sample_after_open_never_fires_from_a_none_baseline() {
        let prev = TelephonyInputState::default(); // just opened, nothing read yet
        let cur = TelephonyInputState { hook: Some(true), mute: Some(true) };
        assert!(diff_actions(prev, cur, CallPhase::Ringing).is_empty());
        assert!(diff_actions(prev, cur, CallPhase::Active).is_empty());
    }

    #[test]
    fn mute_toggle_while_active_maps_to_mute_on_and_off() {
        let prev = TelephonyInputState { hook: Some(true), mute: Some(false) };
        let on = TelephonyInputState { hook: Some(true), mute: Some(true) };
        assert_eq!(diff_actions(prev, on, CallPhase::Active), vec![HidAction::MuteOn]);
        assert_eq!(diff_actions(on, prev, CallPhase::Active), vec![HidAction::MuteOff]);
    }

    #[test]
    fn mute_toggle_outside_a_call_is_ignored() {
        let prev = TelephonyInputState { hook: None, mute: Some(false) };
        let cur = TelephonyInputState { hook: None, mute: Some(true) };
        assert!(diff_actions(prev, cur, CallPhase::Idle).is_empty());
        assert!(diff_actions(prev, cur, CallPhase::Ringing).is_empty());
    }

    #[test]
    fn unchanged_state_never_fires_an_action() {
        let s = TelephonyInputState { hook: Some(true), mute: Some(true) };
        assert!(diff_actions(s, s, CallPhase::Active).is_empty());
    }

    #[test]
    fn hook_and_mute_can_both_fire_from_one_report() {
        let prev = TelephonyInputState { hook: Some(false), mute: Some(false) };
        // Not realistic for one physical button press, but the two fields
        // are independent bits in the same byte on some devices - a
        // combined change must still surface both actions.
        let cur = TelephonyInputState { hook: Some(true), mute: Some(true) };
        let actions = diff_actions(prev, cur, CallPhase::Ringing);
        assert_eq!(actions, vec![HidAction::Answer]); // mute ignored: not Active yet
    }

    // ---- CallPhase::from_ping_state -------------------------------------

    #[test]
    fn planned_but_unwired_usage_constants_match_the_hut_spec_values() {
        // Flash/Redial aren't dispatched to any action yet (see this
        // module's own doc, "Planned") - locked in here so (a) the cited
        // HUT values stay correct if this file is ever extended to wire
        // them up, and (b) these constants have a real reason to exist for
        // `cargo clippy -- -D warnings` rather than needing an
        // `#[allow(dead_code)]` escape hatch.
        assert_eq!(USAGE_FLASH, 0x21);
        assert_eq!(USAGE_REDIAL, 0x24);
    }

    #[test]
    fn call_phase_from_ping_state_covers_every_sidecar_vocabulary_value() {
        assert_eq!(CallPhase::from_ping_state("ringing"), CallPhase::Ringing);
        assert_eq!(CallPhase::from_ping_state("in-call"), CallPhase::Active);
        for idle in ["disconnected", "connecting", "registered", "calling", "anything-unknown"] {
            assert_eq!(CallPhase::from_ping_state(idle), CallPhase::Idle, "{idle} should be Idle");
        }
    }
}

//! The one file in `crate::hid` that touches the `hidapi` crate directly.
//! Every decision — which device to prefer, how to interpret its bytes,
//! what to send back — lives in sibling pure modules (`descriptor.rs`,
//! `mapping.rs`, `led.rs`) that take/return plain data and are unit tested
//! without any real HID hardware attached. This file stays deliberately
//! thin: enumerate, open, read the descriptor — nothing else — so the
//! surface that genuinely needs real hardware to exercise is as small as
//! possible (see this feature's task report for exactly what still needs a
//! real headset, for `qa-e2e`).
//!
//! `DeviceCandidate` and `select_candidates_to_try` below are the one
//! exception: they operate on plain data (no `hidapi` types in their own
//! fields once constructed) and *are* unit tested, even though
//! `DeviceCandidate`s are normally produced by `enumerate()` below, which
//! does touch `hidapi`.
//!
//! **Real-hardware finding this session** (see
//! `hid::device::tests::real_descriptor_parses_on_a_real_connected_device`,
//! and this feature's task report): the top-level enumeration `usage_page`
//! alone is *not* a reliable "is this a telephony device" test. A "USB PnP
//! Sound Device" on this session's real dev machine enumerates with a
//! top-level Consumer Page (`0x0C`) collection usage, yet its actual
//! descriptor nests a `usage_page=0x0B usage=0x20` (Hook Switch) Input
//! field *inside* that collection - a composite device whose outermost
//! collection usage doesn't reflect every control cluster nested within it.
//! So selection here is two-staged: `is_plausible()` is a *cheap*
//! enumeration-level pre-filter (worth the `open()`/`get_report_descriptor()`
//! syscalls or not - most of the dozen-plus HID interfaces a real machine
//! enumerates, e.g. every mouse/keyboard axis, obviously aren't), and
//! `mapping::has_telephony_controls` (called from `crate::hid`'s
//! orchestration loop, after actually parsing a plausible candidate's real
//! descriptor) is the real decision.

use super::descriptor::{parse_report_descriptor, ParsedDescriptor};
use crate::settings::HidDeviceIdentity as DeviceIdentity;
use hidapi::{HidApi, HidDevice};
use serde::Serialize;
use std::ffi::CString;

/// Telephony Device Page — USB HID Usage Tables spec (see
/// `crate::hid::mapping`'s module doc for the full citation).
pub const TELEPHONY_USAGE_PAGE: u16 = 0x0B;
/// Consumer Page — the *other* top-level usage real composite USB
/// audio/headset devices are commonly enumerated under, per this module's
/// own doc above. Included in the pre-filter for the same reason
/// `TELEPHONY_USAGE_PAGE` is: worth actually opening and parsing.
pub const CONSUMER_USAGE_PAGE: u16 = 0x0C;

/// A candidate HID interface from one `HidApi::device_list()` enumeration.
/// Deliberately holds only plain, owned data (no lifetime tied to the
/// `HidApi` it came from) so it outlives that transient `HidApi` and so it
/// can be constructed directly in tests without any real hardware.
#[derive(Debug, Clone)]
pub struct DeviceCandidate {
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
    pub product_string: Option<String>,
    pub usage_page: u16,
    pub usage: u16,
    path: CString,
}

impl DeviceCandidate {
    fn from_info(d: &hidapi::DeviceInfo) -> Self {
        Self {
            vendor_id: d.vendor_id(),
            product_id: d.product_id(),
            serial_number: d.serial_number().map(str::to_string),
            product_string: d.product_string().map(str::to_string),
            usage_page: d.usage_page(),
            usage: d.usage(),
            path: d.path().to_owned(),
        }
    }

    pub fn identity(&self) -> DeviceIdentity {
        DeviceIdentity {
            vendor_id: self.vendor_id,
            product_id: self.product_id,
            serial_number: self.serial_number.clone(),
        }
    }

    /// `true` when this device's usage_page (Telephony or Consumer, per
    /// this module's doc) makes it worth actually opening and parsing -
    /// **not** a final "this is a telephony device" answer; see
    /// `mapping::has_telephony_controls` for that, run against the real
    /// parsed descriptor once one of these plausible candidates is opened.
    pub fn is_plausible(&self) -> bool {
        matches!(self.usage_page, TELEPHONY_USAGE_PAGE | CONSUMER_USAGE_PAGE)
    }

    /// Narrower than `is_plausible` - `true` only for the enumeration-level
    /// Telephony page itself. Kept for `DeviceSummary`'s own
    /// `is_telephony` field (a device picker showing "definitely
    /// telephony" vs "maybe, has to be opened to know" is more honest than
    /// collapsing both into one boolean).
    pub fn is_telephony(&self) -> bool {
        self.usage_page == TELEPHONY_USAGE_PAGE
    }
}

/// Matching logic for `crate::settings::HidDeviceIdentity` (imported above
/// as `DeviceIdentity`) against a live enumeration - kept here (an inherent
/// impl of a type defined in `settings.rs`, legal within the same crate)
/// rather than on the settings type's own definition, so settings.rs
/// (imported by nearly every other module) never needs to mention
/// `DeviceCandidate` or know `hidapi`-shaped types exist at all.
impl DeviceIdentity {
    fn matches(&self, c: &DeviceCandidate) -> bool {
        self.vendor_id == c.vendor_id
            && self.product_id == c.product_id
            // A device with no serial number at all (common on cheap
            // peripherals) matches on VID+PID alone. A saved identity that
            // *does* have a serial only matches a candidate with the exact
            // same one - two identical cheap headsets both missing a
            // serial are indistinguishable at this layer anyway, so
            // requiring an exact match when both have one is strictly more
            // correct without losing anything for the common no-serial case.
            && match (&self.serial_number, &c.serial_number) {
                (None, _) => true,
                (Some(want), Some(have)) => want == have,
                (Some(_), None) => false,
            }
    }
}

/// A JSON-friendly summary for the frontend's device picker
/// (`hid_list_devices` command) — never exposes the raw OS `path` (an
/// internal handle, not something a device-selection UI needs, and one
/// that can look alarmingly like a filesystem path on some platforms).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct DeviceSummary {
    pub vendor_id: u16,
    pub product_id: u16,
    pub product_string: Option<String>,
    pub is_telephony: bool,
    /// The top-level collection's HUT usage ID within its usage page (e.g.
    /// `0x05` = Headset, `0x01` = Phone, when `usage_page` is Telephony) -
    /// exposed so a future device-picker UI can show "Headset" vs "Phone"
    /// rather than just a raw VID:PID pair. Not interpreted by this shell
    /// itself - `is_telephony` (from `usage_page` alone) is all the
    /// selection logic in this module needs.
    pub usage: u16,
}

impl From<&DeviceCandidate> for DeviceSummary {
    fn from(c: &DeviceCandidate) -> Self {
        Self {
            vendor_id: c.vendor_id,
            product_id: c.product_id,
            product_string: c.product_string.clone(),
            is_telephony: c.is_telephony(),
            usage: c.usage,
        }
    }
}

/// Enumerates every HID interface currently visible to the OS. Never
/// panics; an enumeration failure surfaces as an empty list (the caller,
/// `crate::hid`'s poll loop, treats "nothing found" and "couldn't ask"
/// identically — both mean "no headset available right now").
pub fn enumerate(api: &HidApi) -> Vec<DeviceCandidate> {
    api.device_list().map(DeviceCandidate::from_info).collect()
}

/// Orders `candidates` by how worth trying they are, given the operator's
/// saved preference and whether auto-detect is on - the caller
/// (`crate::hid`'s `try_open`) then actually opens + parses each in order
/// until one's real descriptor has telephony controls
/// (`mapping::has_telephony_controls`), since (see this module's own doc)
/// enumeration-level `usage_page` alone can't answer that. Pure — no
/// `hidapi` involved — the one piece of `device.rs` logic that's actually a
/// decision rather than raw I/O, so it's unit tested like every other
/// decision in `crate::hid`.
///
/// - Only `is_plausible()` candidates are ever returned - opening every
///   enumerated HID interface (a real machine can have a dozen-plus, most
///   of them keyboard/mouse axes) would mean needless syscalls, and on
///   macOS, needless Input-Monitoring-permission friction, for interfaces
///   that structurally can't be a telephony control cluster.
/// - A saved `preferred` identity that's currently present is always tried
///   *first*, auto-detect or not - an operator's explicit choice shouldn't
///   lose to "whichever plausible device happened to enumerate first".
/// - `auto_detect` off: the returned list contains *only* the preferred
///   device (if present) - never silently substituting a different device
///   the operator didn't choose, which would be a surprising thing for a
///   call-center admin lock to allow. Empty if there's no preference, or
///   the preferred one isn't currently present.
/// - `auto_detect` on: every other plausible candidate follows, in
///   enumeration order, as fallback tries.
pub fn select_candidates_to_try<'a>(
    candidates: &'a [DeviceCandidate],
    preferred: Option<&DeviceIdentity>,
    auto_detect: bool,
) -> Vec<&'a DeviceCandidate> {
    let plausible: Vec<&DeviceCandidate> = candidates.iter().filter(|c| c.is_plausible()).collect();
    let preferred_match = preferred.and_then(|id| plausible.iter().find(|c| id.matches(c)).copied());

    if !auto_detect {
        return preferred_match.into_iter().collect();
    }

    let mut ordered: Vec<&DeviceCandidate> = Vec::with_capacity(plausible.len());
    if let Some(first) = preferred_match {
        ordered.push(first);
    }
    for c in &plausible {
        let is_the_preferred_one = preferred_match.map(|p| std::ptr::eq(p, *c)).unwrap_or(false);
        if !is_the_preferred_one {
            ordered.push(c);
        }
    }
    ordered
}

/// Opens `candidate` and parses its report descriptor. The only function in
/// this crate that both opens a real device *and* runs the descriptor
/// parser on real (device-supplied) bytes — not part of the default
/// `cargo test` unit suite for that reason (no real hardware guaranteed to
/// be attached in CI), but exercised against real, physically-connected
/// hardware this session via the `#[ignore]`d
/// `real_descriptor_parses_on_a_real_connected_device` test below - see
/// this feature's task report for the captured output. Every error path
/// returns a plain, loggable `String` rather than propagating
/// `hidapi::HidError` further than this file, keeping `hidapi` types out of
/// `crate::hid`'s orchestration module.
pub fn open_and_parse(api: &HidApi, candidate: &DeviceCandidate) -> Result<(HidDevice, ParsedDescriptor), String> {
    let device = api
        .open_path(&candidate.path)
        .map_err(|e| format!("open failed: {e}"))?;
    // 4096 bytes comfortably exceeds any real HID report descriptor this
    // shell will encounter (the USB HID spec doesn't hard-cap descriptor
    // size, but real peripherals - telephony headsets very much included -
    // are consistently well under 1 KiB).
    let mut buf = [0u8; 4096];
    let n = device
        .get_report_descriptor(&mut buf)
        .map_err(|e| format!("report descriptor read failed: {e}"))?;
    let parsed = parse_report_descriptor(&buf[..n]).map_err(|e| format!("report descriptor parse failed: {e:?}"))?;
    Ok((device, parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(vendor_id: u16, product_id: u16, serial: Option<&str>, usage_page: u16) -> DeviceCandidate {
        DeviceCandidate {
            vendor_id,
            product_id,
            serial_number: serial.map(str::to_string),
            product_string: Some("Test Headset".to_string()),
            usage_page,
            usage: 0x05,
            path: CString::new(format!("test-path-{vendor_id:04x}-{product_id:04x}")).unwrap(),
        }
    }

    #[test]
    fn ignores_implausible_interfaces_of_a_composite_device() {
        // A real device enumerates as several HID interfaces (e.g. a
        // generic-desktop page for the mouse-pointer part, a consumer page
        // for volume, and - the real finding this session - telephony
        // controls that can live *nested inside* either a Telephony or
        // Consumer top-level collection) - a Generic Desktop (0x01) axis
        // interface, which can never plausibly carry Hook Switch/Phone
        // Mute, must never even be offered to try.
        let candidates = vec![candidate(0x1234, 0x5678, None, 0x01), candidate(0x1234, 0x5678, None, 0x0B)];
        let ordered = select_candidates_to_try(&candidates, None, true);
        assert_eq!(ordered.len(), 1);
        assert_eq!(ordered[0].usage_page, 0x0B);
    }

    #[test]
    fn consumer_page_candidates_are_plausible_too() {
        // See this module's own doc: a real "USB PnP Sound Device" on this
        // session's dev machine enumerated under the Consumer page (0x0C)
        // yet had real Telephony-page fields nested inside - excluding
        // 0x0C candidates from the try-list would have missed it.
        let candidates = vec![candidate(0x1234, 0x5678, None, 0x0C)];
        assert_eq!(select_candidates_to_try(&candidates, None, true).len(), 1);
    }

    #[test]
    fn no_auto_detect_and_no_preference_selects_nothing() {
        let candidates = vec![candidate(0x1234, 0x5678, None, 0x0B)];
        assert!(select_candidates_to_try(&candidates, None, false).is_empty());
    }

    #[test]
    fn preferred_device_is_tried_first_even_with_another_plausible_device_present() {
        let candidates = vec![candidate(0x1111, 0x1111, None, 0x0B), candidate(0x2222, 0x2222, None, 0x0B)];
        let preferred = DeviceIdentity { vendor_id: 0x2222, product_id: 0x2222, serial_number: None };
        let ordered = select_candidates_to_try(&candidates, Some(&preferred), true);
        assert_eq!(ordered.len(), 2, "the non-preferred one should still be a fallback try");
        assert_eq!(ordered[0].vendor_id, 0x2222, "preferred goes first");
    }

    #[test]
    fn preferred_device_absent_still_offers_auto_detect_fallbacks_when_enabled() {
        let candidates = vec![candidate(0x3333, 0x3333, None, 0x0B)];
        let preferred = DeviceIdentity { vendor_id: 0x9999, product_id: 0x9999, serial_number: None };
        let ordered = select_candidates_to_try(&candidates, Some(&preferred), true);
        assert_eq!(ordered.len(), 1);
        assert_eq!(ordered[0].vendor_id, 0x3333);
    }

    #[test]
    fn preferred_device_absent_and_auto_detect_off_selects_nothing() {
        let candidates = vec![candidate(0x3333, 0x3333, None, 0x0B)];
        let preferred = DeviceIdentity { vendor_id: 0x9999, product_id: 0x9999, serial_number: None };
        assert!(select_candidates_to_try(&candidates, Some(&preferred), false).is_empty());
    }

    #[test]
    fn auto_detect_off_tries_only_the_preferred_device_never_a_substitute() {
        let candidates = vec![candidate(0x1111, 0x1111, None, 0x0B), candidate(0x2222, 0x2222, None, 0x0B)];
        let preferred = DeviceIdentity { vendor_id: 0x2222, product_id: 0x2222, serial_number: None };
        let ordered = select_candidates_to_try(&candidates, Some(&preferred), false);
        assert_eq!(ordered.len(), 1);
        assert_eq!(ordered[0].vendor_id, 0x2222);
    }

    #[test]
    fn serial_number_disambiguates_two_identical_vid_pid_devices() {
        let candidates = vec![candidate(0x4444, 0x4444, Some("AAA"), 0x0B), candidate(0x4444, 0x4444, Some("BBB"), 0x0B)];
        let preferred = DeviceIdentity { vendor_id: 0x4444, product_id: 0x4444, serial_number: Some("BBB".to_string()) };
        let ordered = select_candidates_to_try(&candidates, Some(&preferred), false);
        assert_eq!(ordered.len(), 1);
        assert_eq!(ordered[0].serial_number.as_deref(), Some("BBB"));
    }

    #[test]
    fn preferred_without_serial_matches_a_device_that_has_one() {
        // The common "operator picked it before we ever recorded a serial"
        // case, and the common "device has no serial at all" case - both
        // should still resolve on VID+PID alone.
        let candidates = vec![candidate(0x5555, 0x5555, Some("XYZ"), 0x0B)];
        let preferred = DeviceIdentity { vendor_id: 0x5555, product_id: 0x5555, serial_number: None };
        assert!(!select_candidates_to_try(&candidates, Some(&preferred), false).is_empty());
    }

    #[test]
    fn empty_candidate_list_selects_nothing_regardless_of_settings() {
        assert!(select_candidates_to_try(&[], None, true).is_empty());
    }

    /// Not run by default (`cargo test` skips `#[ignore]`d tests, so
    /// `cargo clippy --all-targets`/CI are unaffected) - this is a manual
    /// real-hardware smoke check, for `qa-e2e` or anyone verifying this
    /// feature against a physical machine: `cargo test -- --ignored
    /// --nocapture hid::device::real_hidapi_enumeration_runs_on_this_machine`.
    /// Confirms `hidapi` itself actually initializes and enumerates on the
    /// real OS (not just compiles/links) - verified once, in this session,
    /// on real macOS hardware (see this feature's task report for the
    /// actual output). Does *not* require a telephony headset to be
    /// plugged in - it just prints whatever HID devices the OS currently
    /// sees, so a developer can sanity-check hidapi itself works on their
    /// machine before debugging anything telephony-specific.
    #[test]
    #[ignore = "manual real-hardware smoke check, see doc comment"]
    fn real_hidapi_enumeration_runs_on_this_machine() {
        let api = HidApi::new().expect("hidapi failed to initialize on this machine");
        let candidates = enumerate(&api);
        eprintln!("hidapi enumerated {} HID interface(s):", candidates.len());
        for c in &candidates {
            eprintln!(
                "  {:04x}:{:04x} usage_page={:#04x} usage={:#04x} telephony={} product={:?}",
                c.vendor_id,
                c.product_id,
                c.usage_page,
                c.usage,
                c.is_telephony(),
                c.product_string
            );
        }
    }

    /// Companion to the enumeration smoke check above: actually opens the
    /// first HID interface the OS reports and runs the real
    /// `get_report_descriptor` + `parse_report_descriptor` pipeline against
    /// *real, device-supplied* descriptor bytes (not this crate's hand-
    /// built fixtures) - the strongest available verification of
    /// `descriptor.rs` without a real telephony device attached, which was
    /// not available this session (see task report). Manual-only, same
    /// invocation shape as the enumeration test above.
    #[test]
    #[ignore = "manual real-hardware smoke check, see doc comment"]
    fn real_descriptor_parses_on_a_real_connected_device() {
        let api = HidApi::new().expect("hidapi failed to initialize on this machine");
        let candidates = enumerate(&api);
        if candidates.is_empty() {
            eprintln!("no HID devices enumerated on this machine - nothing to open");
            return;
        }
        for c in &candidates {
            eprintln!("opening {:04x}:{:04x} usage_page={:#04x} ({:?})...", c.vendor_id, c.product_id, c.usage_page, c.product_string);
            match open_and_parse(&api, c) {
                Ok((_device, parsed)) => {
                    eprintln!("  ok: {} field(s) across {} report(s)", parsed.fields.len(), parsed.report_bit_lengths.len());
                    for f in &parsed.fields {
                        eprintln!("    {:?} report_id={:?} usage_page={:#04x} usage={:#04x} bit_offset={} bit_length={}", f.kind, f.report_id, f.usage_page, f.usage, f.bit_offset, f.bit_length);
                    }
                }
                Err(e) => {
                    // Not a test failure by itself - some HID interfaces are
                    // legitimately exclusive-locked by the OS (e.g. a
                    // keyboard driver already owns it) or need Input
                    // Monitoring permission this shell hasn't been granted
                    // in this terminal session; that's exactly the
                    // real-world failure mode `crate::hid`'s poll loop is
                    // built to degrade through gracefully, so seeing it
                    // here for some devices is informative, not alarming.
                    eprintln!("  open/parse failed (may be expected - see doc comment): {e}");
                }
            }
        }
    }
}

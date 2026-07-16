//! Best-effort headset LED mirroring (spec §5: "reflejar off-hook/mute/ring
//! en el LED del headset"). Pure — builds the exact output-report bytes to
//! send; `crate::hid` (the orchestration loop) is the only caller, and it's
//! the one that actually calls `HidDevice::write`/`send_output_report`,
//! wrapped so any failure here degrades silently (see that module's doc) —
//! not every telephony headset exposes LED usages on its Output report at
//! all (many wired/analog-hook USB adapters are buttons-only, no LED), and
//! that's a normal, expected shape, not an error.

use super::descriptor::{set_bit, MainKind, ParsedDescriptor};
use super::mapping::{LED_MUTE, LED_OFF_HOOK, LED_RING, USAGE_PAGE_LED};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LedState {
    pub off_hook: bool,
    pub ring: bool,
    pub mute: bool,
}

/// Builds a complete Output report (including the leading Report ID byte,
/// if the device uses one, and every padding bit the descriptor declares)
/// reflecting `led`. Returns `None` if the descriptor has no Output field
/// for any of the three LED usages this shell knows about — a real,
/// expected outcome for a buttons-only headset, not a bug.
///
/// Sized from the descriptor's own declared total Output-report bit length
/// for that report ID (`ParsedDescriptor::report_bit_lengths`), not just
/// "however many bits our fields need" — writing a short report to a real
/// device is a good way to have it silently ignore the write or reject it,
/// since HID output reports are fixed-size per Report ID by construction.
pub fn build_output_report(parsed: &ParsedDescriptor, led: LedState) -> Option<Vec<u8>> {
    let led_fields: Vec<_> = parsed
        .fields
        .iter()
        .filter(|f| f.kind == MainKind::Output && f.usage_page == USAGE_PAGE_LED)
        .filter(|f| matches!(f.usage, LED_OFF_HOOK | LED_RING | LED_MUTE))
        .collect();
    let report_id = led_fields.first()?.report_id;
    let total_bits = *parsed.report_bit_lengths.get(&(report_id, MainKind::Output))?;
    if total_bits == 0 {
        return None;
    }
    let total_bytes = total_bits.div_ceil(8) as usize;
    let has_id_byte = report_id.is_some();
    let mut buf = vec![0u8; total_bytes + usize::from(has_id_byte)];
    if let Some(id) = report_id {
        buf[0] = id;
    }
    let data = if has_id_byte { &mut buf[1..] } else { &mut buf[..] };

    let mut wrote_any = false;
    for f in led_fields {
        // Only ever act on fields belonging to the one report_id we picked
        // above - a descriptor that (unusually) split these three LEDs
        // across more than one Output report ID would need a second
        // write() call, which this shell doesn't attempt; every real
        // telephony headset descriptor observed in HUT's own worked
        // examples keeps a device's whole LED cluster on one report.
        if f.report_id != report_id {
            continue;
        }
        let value = match f.usage {
            LED_OFF_HOOK => led.off_hook,
            LED_RING => led.ring,
            LED_MUTE => led.mute,
            _ => continue,
        };
        set_bit(data, f.bit_offset, value);
        wrote_any = true;
    }
    wrote_any.then_some(buf)
}

#[cfg(test)]
mod tests {
    use super::super::descriptor::{FieldLocation, MainKind};
    use super::*;
    use std::collections::HashMap;

    fn parsed_with_led_fields(report_id: Option<u8>, total_output_bits: u32) -> ParsedDescriptor {
        let mut fields = vec![
            FieldLocation { kind: MainKind::Output, report_id, usage_page: USAGE_PAGE_LED, usage: LED_OFF_HOOK, bit_offset: 0, bit_length: 1 },
            FieldLocation { kind: MainKind::Output, report_id, usage_page: USAGE_PAGE_LED, usage: LED_RING, bit_offset: 1, bit_length: 1 },
            FieldLocation { kind: MainKind::Output, report_id, usage_page: USAGE_PAGE_LED, usage: LED_MUTE, bit_offset: 2, bit_length: 1 },
        ];
        // An unrelated field the LED builder must ignore (e.g. some other
        // vendor-page output control sharing the same report).
        fields.push(FieldLocation { kind: MainKind::Output, report_id, usage_page: 0xFF00, usage: 0x01, bit_offset: 3, bit_length: 1 });
        let mut report_bit_lengths = HashMap::new();
        report_bit_lengths.insert((report_id, MainKind::Output), total_output_bits);
        ParsedDescriptor { fields, report_bit_lengths }
    }

    #[test]
    fn builds_report_with_id_byte_and_correct_bits() {
        let parsed = parsed_with_led_fields(Some(3), 8);
        let led = LedState { off_hook: true, ring: false, mute: true };
        let buf = build_output_report(&parsed, led).expect("has LED fields");
        assert_eq!(buf.len(), 2); // 1 ID byte + 1 data byte (8 bits)
        assert_eq!(buf[0], 3);
        assert_eq!(buf[1] & 0b0000_0111, 0b0000_0101); // off_hook=1(bit0), ring=0(bit1), mute=1(bit2)
    }

    #[test]
    fn builds_report_without_id_byte_when_device_has_no_report_ids() {
        let parsed = parsed_with_led_fields(None, 8);
        let led = LedState { off_hook: false, ring: true, mute: false };
        let buf = build_output_report(&parsed, led).expect("has LED fields");
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0] & 0b0000_0111, 0b0000_0010); // ring bit set only
    }

    #[test]
    fn all_off_produces_a_zeroed_data_region() {
        let parsed = parsed_with_led_fields(Some(1), 8);
        let buf = build_output_report(&parsed, LedState::default()).unwrap();
        assert_eq!(buf[1], 0);
    }

    #[test]
    fn no_led_fields_at_all_returns_none() {
        let parsed = ParsedDescriptor::default();
        assert!(build_output_report(&parsed, LedState { off_hook: true, ..Default::default() }).is_none());
    }

    #[test]
    fn report_bit_length_rounds_up_to_a_whole_byte() {
        // 3 real LED bits declared as a 3-bit-total output report (an
        // unusually tight descriptor, but legal) - must still round up to
        // one full byte of buffer, never a partial byte.
        let parsed = parsed_with_led_fields(None, 3);
        let buf = build_output_report(&parsed, LedState { mute: true, ..Default::default() }).unwrap();
        assert_eq!(buf.len(), 1);
    }
}

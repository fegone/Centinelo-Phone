//! Minimal USB HID Report Descriptor parser (USB HID spec 1.11, "Device
//! Class Definition for HID 1.11" §6.2.2, "short items" only — long items,
//! §6.2.2.2, are a rare escape hatch essentially never used by real HID
//! peripherals; this parser reports them as an error rather than guessing
//! at their meaning).
//!
//! **Why this exists at all**: `hidapi` (`crate::hid::device`) only ever
//! hands back raw report *bytes* — `HidDevice::read`/`write` — it does not
//! decode which bit of those bytes means what. Hard-coding "byte 0, bit 0 =
//! Hook Switch" would only be correct for one specific vendor/model's own
//! report layout (there is no universal one — the whole point of a
//! descriptor is that each device declares its own). So instead this module
//! walks the device's own report descriptor
//! (`HidDevice::get_report_descriptor`) the same way the OS's real HID
//! stack does, to find out *for this specific connected device* which bit
//! of which report carries the Telephony page's Hook Switch (usage `0x20`)
//! and Phone Mute (usage `0x2F`) controls, and the LED page's Off-Hook
//! (`0x17`)/Ring (`0x18`)/Mute (`0x09`) output indicators — see
//! `crate::hid::mapping` for where those usage IDs come from and why they're
//! not invented.
//!
//! Pure, no I/O, no `hidapi` dependency — takes descriptor bytes in,
//! returns a parsed field list out. Fully unit-testable without real
//! hardware (see the `tests` module below, fixtures included).

use std::collections::HashMap;

/// Which of the three independent HID "report streams" a field belongs to.
/// Input = device→host (buttons), Output = host→device (LEDs), Feature =
/// bidirectional config (not used by this shell today, parsed anyway since
/// skipping it would throw off byte offsets for the reports that come
/// after it in the descriptor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MainKind {
    Input,
    Output,
    Feature,
}

/// One resolved bit-field: "usage U on page P lives at bit N of report R".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldLocation {
    pub kind: MainKind,
    /// `None` when the device declares no Report IDs at all (a single-report
    /// device) — the raw report then has no leading ID byte, and
    /// `bit_offset` counts from bit 0 of byte 0 of the report as-is. `Some`
    /// means the report's first *byte* is the Report ID and real data
    /// starts after it (`bit_offset` is still relative to the start of the
    /// data, not the ID byte — see `crate::hid::mapping::extract_state`).
    pub report_id: Option<u8>,
    pub usage_page: u16,
    pub usage: u16,
    /// Bit offset from the start of this report's *data* (after the Report
    /// ID byte, if any). Bit 0 is the LSB of the first data byte.
    pub bit_offset: u32,
    pub bit_length: u32,
}

/// Everything this module extracts from a descriptor: the individual fields
/// we can look up by usage, plus the *total* bit length of each
/// (report_id, kind) stream — needed separately because building a valid
/// Output report (`crate::hid::led`) requires sending the device's full
/// declared report size, not just the bits we personally care about (the
/// device's own firmware expects every field, including padding, to be
/// present).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedDescriptor {
    pub fields: Vec<FieldLocation>,
    pub report_bit_lengths: HashMap<(Option<u8>, MainKind), u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescriptorError {
    /// An item's declared data size ran past the end of the buffer.
    UnexpectedEnd,
    /// Hit a "long item" (prefix byte `0xFE`) — not supported, see module
    /// doc. Real telephony/audio HID descriptors observed in the wild
    /// (and every one the USB HID spec itself gives as an example) only
    /// ever use short items.
    LongItemUnsupported,
    /// A Push without a matching Pop, or vice versa, more than
    /// `MAX_GLOBAL_STACK` deep — defensive cap against a malformed or
    /// hostile descriptor spinning this parser forever; no real device
    /// nests anywhere close to this deep.
    GlobalStackOverflow,
    /// Report Count (Global item tag `0x9`) exceeded `MAX_REPORT_COUNT` —
    /// see that constant's doc. 4R review finding A1 (2026-07-16): a
    /// Report Count is a 1/2/4-byte Global item read straight into a `u32`
    /// (up to `u32::MAX`) and was used directly as `for idx in 0..count`'s
    /// bound with no cap at all - a device with buggy firmware (no
    /// malicious intent required, a cheap call-center headset is enough)
    /// declaring a huge count could balloon this parser's field list into
    /// the billions, hanging the HID thread or OOM-killing the whole app,
    /// reachable just by opening the device.
    ReportCountTooLarge,
    /// Report Size (Global item tag `0x7`, bits per field) exceeded
    /// `MAX_REPORT_SIZE` — see that constant's doc. Same finding as
    /// `ReportCountTooLarge`, the other half of the `count * size`
    /// multiplication that fed the same DoS.
    ReportSizeTooLarge,
    /// A running bit cursor (or a `Report Count * Report Size`
    /// multiplication) would have overflowed `u32`. Defense in depth
    /// alongside the two caps above — should be unreachable through a
    /// *single* Main item once both caps hold, but a descriptor with many
    /// chained large-but-individually-capped Input/Output/Feature items
    /// can still, in principle, accumulate a cursor past `u32::MAX`;
    /// `checked_mul`/`checked_add` catch that explicitly rather than
    /// silently wrapping (release) or panicking (debug) on external
    /// device data, per this crate's "never panic/wrap on untrusted input"
    /// discipline.
    ReportArithmeticOverflow,
}

const MAX_GLOBAL_STACK: usize = 16;
/// Caps a Usage Minimum..Usage Maximum expansion — a malformed descriptor
/// could declare a huge range; no real telephony control cluster needs more
/// than a couple dozen usages in one field group.
const MAX_USAGE_RANGE_EXPANSION: usize = 256;
/// Report Count (fields per Input/Output/Feature item) cap — see
/// `DescriptorError::ReportCountTooLarge`'s doc. Real HID peripherals
/// (including large 100+-key keyboards, which pack far more fields into a
/// single descriptor than any telephony headset) never come close to this;
/// it exists purely to bound a malformed/buggy-firmware descriptor.
const MAX_REPORT_COUNT: u32 = 8_192;
/// Report Size (bits per field) cap — see
/// `DescriptorError::ReportSizeTooLarge`'s doc. Real fields this parser
/// cares about are 1 bit (every button/LED usage in `crate::hid::mapping`);
/// 128 bits (16 bytes) is already far more generous than any real HID
/// field this shell will ever encounter needs.
const MAX_REPORT_SIZE: u32 = 128;

#[derive(Debug, Clone, Copy, Default)]
struct GlobalState {
    usage_page: u16,
    report_size: u32,
    report_count: u32,
    report_id: Option<u8>,
}

/// A single `Usage` local item, with the extended-usage form (`Usage(page,
/// id)`, a 4-byte item per §6.2.2.8) tracked separately from the common
/// 1/2-byte form that only carries an id and inherits the current global
/// Usage Page.
#[derive(Debug, Clone, Copy)]
struct LocalUsage {
    page_override: Option<u16>,
    id: u16,
}

#[derive(Debug, Clone, Default)]
struct LocalState {
    usages: Vec<LocalUsage>,
}

pub fn parse_report_descriptor(bytes: &[u8]) -> Result<ParsedDescriptor, DescriptorError> {
    let mut out = ParsedDescriptor::default();
    let mut global = GlobalState::default();
    let mut local = LocalState::default();
    let mut stack: Vec<GlobalState> = Vec::new();
    // Running bit cursor per (report_id, kind) — a report_id can carry
    // Input, Output *and* Feature data, each its own independent bit
    // stream, so this is keyed on both.
    let mut cursors: HashMap<(Option<u8>, MainKind), u32> = HashMap::new();
    // Usage Minimum/Maximum arrive as two separate Local items that only
    // mean something once both halves of the pair are in — held here across
    // loop iterations until that happens (or until the next Main item
    // clears them, same as every other Local item).
    let mut pending_usage_min: Option<u16> = None;
    let mut pending_usage_max: Option<u16> = None;

    let mut i = 0usize;
    while i < bytes.len() {
        let prefix = bytes[i];
        if prefix == 0xFE {
            return Err(DescriptorError::LongItemUnsupported);
        }
        let size_code = prefix & 0b0000_0011;
        let item_type = (prefix >> 2) & 0b0000_0011;
        let tag = (prefix >> 4) & 0b0000_1111;
        let data_len = match size_code {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 4, // size_code == 3 means 4 bytes per spec table
        };
        if i + 1 + data_len > bytes.len() {
            return Err(DescriptorError::UnexpectedEnd);
        }
        let data_bytes = &bytes[i + 1..i + 1 + data_len];
        let data = read_unsigned(data_bytes);
        i += 1 + data_len;

        match item_type {
            1 => {
                // Global item.
                match tag {
                    0x0 => global.usage_page = data as u16,       // Usage Page
                    0x7 => {
                        // Report Size - capped, see MAX_REPORT_SIZE's doc
                        // (4R finding A1: this is one half of the
                        // count*size DoS).
                        if data > MAX_REPORT_SIZE {
                            return Err(DescriptorError::ReportSizeTooLarge);
                        }
                        global.report_size = data;
                    }
                    0x8 => {
                        // Report ID — also (re)starts that report's own bit
                        // cursors at 0 the first time we see it, matching
                        // real HID semantics (each report ID is its own
                        // independently-addressed report).
                        global.report_id = Some(data as u8);
                    }
                    0x9 => {
                        // Report Count - capped, see MAX_REPORT_COUNT's doc
                        // (4R finding A1: this is the other half - and the
                        // direct loop bound in the Main-item handler below).
                        if data > MAX_REPORT_COUNT {
                            return Err(DescriptorError::ReportCountTooLarge);
                        }
                        global.report_count = data;
                    }
                    0xA => {
                        // Push
                        if stack.len() >= MAX_GLOBAL_STACK {
                            return Err(DescriptorError::GlobalStackOverflow);
                        }
                        stack.push(global);
                    }
                    0xB => {
                        // Pop
                        if let Some(prev) = stack.pop() {
                            global = prev;
                        }
                        // A Pop with nothing pushed is technically malformed;
                        // tolerated as a no-op rather than erroring, same
                        // "never crash on a weird device" spirit as the rest
                        // of this crate.
                    }
                    _ => {} // Logical Min/Max, Physical Min/Max, Unit*, ... — not needed here.
                }
            }
            2 => {
                // Local item.
                match tag {
                    0x0 => {
                        // Usage. 4-byte data size = extended usage
                        // (page<<16 | id), per §6.2.2.8; 1/2-byte size =
                        // plain id on the current global Usage Page.
                        if data_len == 4 {
                            local.usages.push(LocalUsage {
                                page_override: Some((data >> 16) as u16),
                                id: (data & 0xFFFF) as u16,
                            });
                        } else {
                            local.usages.push(LocalUsage { page_override: None, id: data as u16 });
                        }
                    }
                    0x1 | 0x2 => {
                        // Usage Minimum (0x1) / Usage Maximum (0x2). Only
                        // acted on once both of a pair have arrived; stashed
                        // via a tiny piece of extra state below.
                        if tag == 0x1 {
                            pending_usage_min = Some(data as u16);
                        } else {
                            pending_usage_max = Some(data as u16);
                        }
                        if let (Some(min), Some(max)) = (pending_usage_min, pending_usage_max) {
                            let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
                            let expanded = (lo..=hi).take(MAX_USAGE_RANGE_EXPANSION);
                            for id in expanded {
                                local.usages.push(LocalUsage { page_override: None, id });
                            }
                            pending_usage_min = None;
                            pending_usage_max = None;
                        }
                    }
                    _ => {} // Designator*, String*, Delimiter — not needed here.
                }
            }
            0 => {
                // Main item.
                match tag {
                    0x8 | 0x9 | 0xB => {
                        // Input (0x8) / Output (0x9) / Feature (0xB).
                        let kind = match tag {
                            0x8 => MainKind::Input,
                            0x9 => MainKind::Output,
                            _ => MainKind::Feature,
                        };
                        let is_constant = data & 0b1 == 1; // bit 0 of the Main item's flags: Data(0)/Constant(1)
                        let key = (global.report_id, kind);
                        let cursor = cursors.entry(key).or_insert(0);
                        let count = global.report_count;
                        let size = global.report_size;

                        if is_constant || local.usages.is_empty() {
                            // Padding, or a field with no Usage() at all
                            // (both real and harmless — e.g. a reserved
                            // byte) — still occupies report space, but
                            // there's no usage to resolve it to.
                            let total = count.checked_mul(size).ok_or(DescriptorError::ReportArithmeticOverflow)?;
                            *cursor = cursor.checked_add(total).ok_or(DescriptorError::ReportArithmeticOverflow)?;
                        } else {
                            for idx in 0..count {
                                // Fewer explicit usages than the field count
                                // repeats the *last* usage for the remainder
                                // — the exact rule §6.2.2.8 documents for
                                // "more fields than usages".
                                let u = local
                                    .usages
                                    .get(idx as usize)
                                    .or_else(|| local.usages.last())
                                    .copied();
                                if let Some(u) = u {
                                    out.fields.push(FieldLocation {
                                        kind,
                                        report_id: global.report_id,
                                        usage_page: u.page_override.unwrap_or(global.usage_page),
                                        usage: u.id,
                                        bit_offset: *cursor,
                                        bit_length: size,
                                    });
                                }
                                *cursor = cursor.checked_add(size).ok_or(DescriptorError::ReportArithmeticOverflow)?;
                            }
                        }
                        out.report_bit_lengths.insert(key, *cursor);
                        local = LocalState::default(); // Local items never carry over past a Main item.
                        pending_usage_min = None;
                        pending_usage_max = None;
                    }
                    0xA | 0xC => {
                        // Collection / End Collection — no report-layout
                        // effect for our purposes (bit offsets are
                        // report-relative, not collection-relative), but
                        // Local state is still cleared per spec.
                        local = LocalState::default();
                    }
                    _ => {}
                }
            }
            _ => {} // item_type == 3 is reserved; nothing defined uses it.
        }
    }

    Ok(out)
}

fn read_unsigned(bytes: &[u8]) -> u32 {
    let mut v = 0u32;
    for (idx, b) in bytes.iter().enumerate() {
        v |= (*b as u32) << (8 * idx);
    }
    v
}

/// Reads a single bit from `data` (already sliced to skip any leading
/// Report ID byte — see `FieldLocation::report_id`'s doc). `None` if
/// `bit_offset` falls outside `data`, which happens if a device's actual
/// report turned out shorter than its own descriptor promised (seen in
/// practice with flaky/cheap peripherals) — treated as "don't know", never
/// a panic or an out-of-bounds read.
pub fn get_bit(data: &[u8], bit_offset: u32) -> Option<bool> {
    let byte_idx = (bit_offset / 8) as usize;
    let bit_idx = bit_offset % 8;
    data.get(byte_idx).map(|b| (b >> bit_idx) & 1 == 1)
}

/// Sets a single bit in `data` (already sliced past any leading Report ID
/// byte). Silently does nothing if `bit_offset` is out of range — building
/// an output report is always sized from the descriptor's own declared
/// length first (see `crate::hid::led::build_output_report`), so this
/// should never actually happen; kept a no-op rather than a panic anyway,
/// consistent with "the HID thread never crashes the app" throughout this
/// module.
pub fn set_bit(data: &mut [u8], bit_offset: u32, value: bool) {
    let byte_idx = (bit_offset / 8) as usize;
    let bit_idx = bit_offset % 8;
    if let Some(b) = data.get_mut(byte_idx) {
        if value {
            *b |= 1 << bit_idx;
        } else {
            *b &= !(1 << bit_idx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- fixture builders -------------------------------------------
    //
    // Hand-encodes short HID items per USB HID spec §6.2.2 (this module's
    // own subject matter) rather than copying real device bytes - these
    // exist to exercise specific parser paths in isolation (4R review
    // finding M2: this file itself had zero direct tests before this
    // pass, only indirect coverage via mapping.rs's single fixture).

    /// `item_type`: 0=Main, 1=Global, 2=Local. `width` (bytes of `value` to
    /// encode, little-endian): 0, 1, 2, or 4 - matches the four short-item
    /// size codes exactly.
    fn short_item(tag: u8, item_type: u8, value: u32, width: usize) -> Vec<u8> {
        let size_code = match width {
            0 => 0,
            1 => 1,
            2 => 2,
            4 => 3,
            other => panic!("test fixture bug: unsupported item width {other}"),
        };
        let prefix = (tag << 4) | (item_type << 2) | size_code;
        let mut v = vec![prefix];
        v.extend_from_slice(&value.to_le_bytes()[..width]);
        v
    }
    fn global(tag: u8, value: u32, width: usize) -> Vec<u8> {
        short_item(tag, 1, value, width)
    }
    fn local(tag: u8, value: u32, width: usize) -> Vec<u8> {
        short_item(tag, 2, value, width)
    }
    fn main_item(tag: u8, flags: u32) -> Vec<u8> {
        short_item(tag, 0, flags, 1)
    }
    fn usage_page(page: u16) -> Vec<u8> {
        global(0x0, page as u32, 2)
    }

    // ---- Push / Pop ----------------------------------------------------

    #[test]
    fn push_pop_restores_previous_global_state() {
        let mut d = Vec::new();
        d.extend(usage_page(0x0B));
        d.extend(global(0x7, 1, 1)); // Report Size = 1
        d.extend(global(0x9, 1, 1)); // Report Count = 1
        d.extend(global(0xA, 0, 0)); // Push (saves report_size=1)
        d.extend(global(0x7, 8, 1)); // Report Size = 8, only until Pop
        d.extend(global(0xB, 0, 0)); // Pop -> report_size back to 1
        d.extend(local(0x0, 0x20, 1)); // Usage (Hook Switch)
        d.extend(main_item(0x8, 0x02)); // Input, Data/Variable/Absolute

        let parsed = parse_report_descriptor(&d).unwrap();
        assert_eq!(parsed.fields.len(), 1);
        assert_eq!(parsed.fields[0].bit_length, 1, "report_size must have reverted to 1 after Pop, not stayed at 8");
    }

    #[test]
    fn push_beyond_max_stack_depth_returns_overflow_error() {
        let mut d = Vec::new();
        for _ in 0..=MAX_GLOBAL_STACK {
            d.extend(global(0xA, 0, 0)); // Push, one more than the cap allows
        }
        assert_eq!(parse_report_descriptor(&d), Err(DescriptorError::GlobalStackOverflow));
    }

    #[test]
    fn pop_with_nothing_pushed_is_tolerated_as_a_no_op() {
        let d = global(0xB, 0, 0); // bare Pop, no matching Push
        assert!(parse_report_descriptor(&d).is_ok());
    }

    // ---- long items / truncation ---------------------------------------

    #[test]
    fn long_item_prefix_is_rejected_not_misparsed() {
        let d = vec![0xFEu8, 0x02, 0x00, 0xAA, 0xBB]; // long-item header + 2 data bytes
        assert_eq!(parse_report_descriptor(&d), Err(DescriptorError::LongItemUnsupported));
    }

    #[test]
    fn truncated_item_returns_unexpected_end_not_a_panic() {
        // Report Count (Global, tag 0x9) claiming a 2-byte payload with
        // only 1 byte actually present.
        let d = vec![0x96u8, 0x01];
        assert_eq!(parse_report_descriptor(&d), Err(DescriptorError::UnexpectedEnd));
    }

    // ---- extended (4-byte) usage -----------------------------------------

    #[test]
    fn extended_four_byte_usage_overrides_the_global_usage_page() {
        let mut d = Vec::new();
        d.extend(usage_page(0x0C)); // Consumer - deliberately NOT Telephony
        d.extend(global(0x7, 1, 1));
        d.extend(global(0x9, 1, 1));
        // Usage(page=0x0B, id=0x20) as one 4-byte extended-usage item
        // (§6.2.2.8) - overrides the surrounding Consumer-page collection's
        // usage page for just this one field, the real-world shape this
        // session's own hardware finding needed (see device.rs's module
        // doc: a Consumer-page device with a nested Telephony field).
        let extended: u32 = (0x000B_u32 << 16) | 0x0020_u32;
        d.extend(local(0x0, extended, 4));
        d.extend(main_item(0x8, 0x02));

        let parsed = parse_report_descriptor(&d).unwrap();
        assert_eq!(parsed.fields.len(), 1);
        assert_eq!(parsed.fields[0].usage_page, 0x0B);
        assert_eq!(parsed.fields[0].usage, 0x20);
    }

    // ---- Usage Minimum/Maximum range expansion --------------------------

    #[test]
    fn usage_minimum_maximum_expands_into_one_field_per_usage_in_order() {
        let mut d = Vec::new();
        d.extend(usage_page(0x09)); // Button page - arbitrary, not telephony-specific
        d.extend(global(0x7, 1, 1));
        d.extend(global(0x9, 3, 1)); // Report Count = 3, matching the 3-usage range below
        d.extend(local(0x1, 1, 1)); // Usage Minimum = 1
        d.extend(local(0x2, 3, 1)); // Usage Maximum = 3
        d.extend(main_item(0x8, 0x02));

        let parsed = parse_report_descriptor(&d).unwrap();
        let usages: Vec<u16> = parsed.fields.iter().map(|f| f.usage).collect();
        assert_eq!(usages, vec![1, 2, 3]);
        assert_eq!(parsed.fields[0].bit_offset, 0);
        assert_eq!(parsed.fields[1].bit_offset, 1);
        assert_eq!(parsed.fields[2].bit_offset, 2);
    }

    #[test]
    fn usage_minimum_maximum_reversed_order_still_expands_correctly() {
        // Maximum item arriving before Minimum, and min > max as written -
        // §6.2.2.8 doesn't forbid either order; this parser sorts them.
        let mut d = Vec::new();
        d.extend(usage_page(0x09));
        d.extend(global(0x7, 1, 1));
        d.extend(global(0x9, 2, 1));
        d.extend(local(0x2, 5, 1)); // Usage Maximum = 5 (declared first)
        d.extend(local(0x1, 4, 1)); // Usage Minimum = 4
        d.extend(main_item(0x8, 0x02));

        let parsed = parse_report_descriptor(&d).unwrap();
        let usages: Vec<u16> = parsed.fields.iter().map(|f| f.usage).collect();
        assert_eq!(usages, vec![4, 5]);
    }

    // ---- multiple, interleaved Report IDs --------------------------------

    #[test]
    fn multiple_report_ids_have_independent_bit_cursors() {
        let mut d = Vec::new();
        d.extend(usage_page(0x0B));
        d.extend(global(0x7, 1, 1));
        d.extend(global(0x9, 1, 1));
        d.extend(global(0x8, 1, 1)); // Report ID = 1
        d.extend(local(0x0, 0x20, 1)); // Hook Switch
        d.extend(main_item(0x8, 0x02)); // Input for report 1, bit_offset 0
        d.extend(global(0x8, 2, 1)); // Report ID = 2 (a *different* report)
        d.extend(local(0x0, 0x2F, 1)); // Phone Mute
        d.extend(main_item(0x8, 0x02)); // Input for report 2 - independent cursor

        let parsed = parse_report_descriptor(&d).unwrap();
        assert_eq!(parsed.fields.len(), 2);
        assert_eq!(parsed.fields[0].report_id, Some(1));
        assert_eq!(parsed.fields[0].bit_offset, 0);
        assert_eq!(parsed.fields[1].report_id, Some(2));
        assert_eq!(parsed.fields[1].bit_offset, 0, "report 2's cursor must start fresh at 0, not continue from report 1's");
    }

    // ---- A1 (4R risk/resilience finding): Report Count/Size DoS ----------

    #[test]
    fn report_count_at_u32_max_is_rejected_in_microseconds_not_hung_on() {
        let d = global(0x9, u32::MAX, 4); // Report Count = u32::MAX, one Global item
        let start = std::time::Instant::now();
        let result = parse_report_descriptor(&d);
        let elapsed = start.elapsed();
        assert_eq!(result, Err(DescriptorError::ReportCountTooLarge));
        assert!(elapsed < std::time::Duration::from_millis(50), "must reject immediately, took {elapsed:?}");
    }

    #[test]
    fn report_size_at_u32_max_is_rejected() {
        let d = global(0x7, u32::MAX, 4); // Report Size = u32::MAX
        assert_eq!(parse_report_descriptor(&d), Err(DescriptorError::ReportSizeTooLarge));
    }

    #[test]
    fn report_count_just_over_the_cap_is_rejected() {
        let d = global(0x9, MAX_REPORT_COUNT + 1, 4);
        assert_eq!(parse_report_descriptor(&d), Err(DescriptorError::ReportCountTooLarge));
    }

    #[test]
    fn report_size_just_over_the_cap_is_rejected() {
        let d = global(0x7, MAX_REPORT_SIZE + 1, 4);
        assert_eq!(parse_report_descriptor(&d), Err(DescriptorError::ReportSizeTooLarge));
    }

    #[test]
    fn report_count_and_size_exactly_at_the_cap_are_accepted() {
        let mut d = Vec::new();
        d.extend(global(0x7, MAX_REPORT_SIZE, 1));
        d.extend(global(0x9, MAX_REPORT_COUNT, 2));
        // Constant/padding (no Usage()) - a real, legitimate field group at
        // exactly the cap, exercised via the bulk (non-per-field-push) path
        // so this test itself stays fast and light.
        d.extend(main_item(0x8, 0x01));
        let parsed = parse_report_descriptor(&d).unwrap();
        let total_bits = *parsed.report_bit_lengths.get(&(None, MainKind::Input)).unwrap();
        assert_eq!(total_bits, MAX_REPORT_COUNT * MAX_REPORT_SIZE);
    }

    #[test]
    fn cumulative_bit_cursor_overflow_across_many_capped_items_is_rejected_not_wrapped() {
        // Each individual Input item here stays within the (now-capped)
        // MAX_REPORT_COUNT/MAX_REPORT_SIZE limits - this is the *other*
        // half of finding A1, the "checked_add on the running cursor"
        // defense-in-depth, not the per-item cap. MAX_REPORT_COUNT (8192)
        // * MAX_REPORT_SIZE (128) = 2^20 bits per item; 2^32 / 2^20 = 4096,
        // so the 4096th item's addition takes the cursor to exactly 2^32,
        // which doesn't fit in a u32 (max is 2^32 - 1) - must be caught by
        // checked_add, not silently wrapped (release) or panicked on
        // (debug), since this is untrusted device data.
        let mut d = Vec::new();
        d.extend(global(0x7, MAX_REPORT_SIZE, 1));
        d.extend(global(0x9, MAX_REPORT_COUNT, 2));
        for _ in 0..4096 {
            d.extend(main_item(0x8, 0x01)); // Constant - bulk cursor advance only, no per-field allocation
        }
        assert_eq!(parse_report_descriptor(&d), Err(DescriptorError::ReportArithmeticOverflow));
    }
}

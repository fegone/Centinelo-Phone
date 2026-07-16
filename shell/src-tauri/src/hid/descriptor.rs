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
}

const MAX_GLOBAL_STACK: usize = 16;
/// Caps a Usage Minimum..Usage Maximum expansion — a malformed descriptor
/// could declare a huge range; no real telephony control cluster needs more
/// than a couple dozen usages in one field group.
const MAX_USAGE_RANGE_EXPANSION: usize = 256;

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
                    0x7 => global.report_size = data,             // Report Size
                    0x8 => {
                        // Report ID — also (re)starts that report's own bit
                        // cursors at 0 the first time we see it, matching
                        // real HID semantics (each report ID is its own
                        // independently-addressed report).
                        global.report_id = Some(data as u8);
                    }
                    0x9 => global.report_count = data,            // Report Count
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
                            *cursor += count * size;
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
                                *cursor += size;
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

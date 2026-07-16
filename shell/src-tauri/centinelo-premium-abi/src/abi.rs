//! The versioned entry point and vtable shape.

use std::os::raw::c_char;

/// Struct-shape version of [`PremiumAbiV1`]. Doubles as the value that
/// struct's own `abi_version` field must hold — a loader that reads a
/// different value back is looking at a table it doesn't understand the
/// layout of and must not touch any field of it beyond `abi_version`
/// itself.
///
/// This is *not* meant to climb with every small addition — see
/// [`PremiumAbiV1`]'s doc for the actual versioning policy (short version:
/// any shape change gets a new entry symbol, this constant only exists so
/// the struct can self-describe which version it is).
pub const ABI_VERSION: u32 = 1;

/// The symbol `libloading::Library::get` looks up. Must be nul-terminated
/// for `Library::get`'s C-string API — the trailing `\0` is part of the
/// byte string, not decorative.
///
/// # Why a versioned symbol name, not just a versioned struct field
///
/// A struct field can only be *read* after you already know how to
/// interpret the bytes at its offset — which requires already knowing the
/// struct's layout, which is exactly what's in question across an ABI
/// boundary. Encoding the version in the *symbol name* sidesteps that
/// chicken-and-egg problem: `dlsym`/`GetProcAddress` either finds
/// `centinelo_premium_abi_v1` or it doesn't, and that answer doesn't
/// require agreeing on any struct layout first. A future breaking change
/// to [`PremiumAbiV1`] ships as a new function under a new symbol
/// (`centinelo_premium_abi_v2`) that coexists with this one — an old
/// dylib simply doesn't export the new symbol, and an old shell simply
/// never looks for it, so mismatched pairs degrade to "loader doesn't find
/// the entry point it's looking for" (see `loader-poc`'s `LoadOutcome`),
/// which is already a handled, non-crashing outcome.
pub const ENTRY_SYMBOL_NAME: &[u8] = b"centinelo_premium_abi_v1\0";

/// Per-call FFI status code — the return value of every function in
/// [`PremiumAbiV1`]. Distinct from [`crate::CapabilityStatus`]: this describes
/// whether *the call itself* succeeded (valid pointers, no panic, a
/// recognized capability id); `CapabilityStatus` is the *answer* written
/// into a call's out-parameter once the call did succeed. A caller must
/// check `FfiResult::Ok` before trusting anything written to an
/// out-parameter — see `loader-poc`'s `PremiumRuntime::capability_status`
/// for the reference caller-side pattern.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiResult {
    /// The call completed normally; any out-parameter is valid.
    Ok = 0,
    /// A required out-parameter pointer was null. The callee must check
    /// this *before* writing through the pointer — see [`crate::ffi_guard`]
    /// and `centinelo-premium`'s FFI wrapper functions for the pattern.
    NullPointer = -1,
    /// The callee's implementation panicked; [`crate::ffi_guard`] caught
    /// it before it could unwind across the FFI boundary. Treat exactly
    /// like any other failure — degrade, don't retry in a loop.
    Panic = -2,
    /// The `u32` capability id didn't decode to a known [`crate::Capability`]
    /// (see that type's "Why `u32` at the boundary" note) — most likely an
    /// ABI/version-skew pairing rather than a bug in either side.
    UnknownCapability = -3,
}

impl FfiResult {
    /// Checked conversion from the raw `i32` a call actually returns.
    /// `None` means neither side's copy of this crate recognizes the code —
    /// callers should treat that exactly like [`FfiResult::Panic`]: don't
    /// trust anything else about the call.
    pub const fn from_i32(raw: i32) -> Option<FfiResult> {
        match raw {
            0 => Some(FfiResult::Ok),
            -1 => Some(FfiResult::NullPointer),
            -2 => Some(FfiResult::Panic),
            -3 => Some(FfiResult::UnknownCapability),
            _ => None,
        }
    }

    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

/// Static metadata about the loaded premium dylib. Every pointer field
/// points at data owned by the dylib for as long as it stays loaded — see
/// the crate doc's "Pointer lifetime contract". Written through an
/// out-parameter by [`PremiumInfoFn`] rather than returned by value,
/// deliberately: returning a `#[repr(C)]` struct by value across an FFI
/// boundary is calling-convention-sensitive (register vs. hidden-pointer
/// return differs by platform/struct size); writing through a
/// caller-owned `*mut PremiumInfo` has exactly one behavior everywhere.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PremiumInfo {
    /// Human-readable edition string, e.g. `"Centinelo Premium"`. Nul-terminated UTF-8.
    pub edition: *const c_char,
    /// `centinelo-premium`'s own crate version (`CARGO_PKG_VERSION`), e.g. `"0.1.0"`. Nul-terminated UTF-8.
    pub build_version: *const c_char,
    /// Always equal to [`ABI_VERSION`] for a table obtained through
    /// [`ENTRY_SYMBOL_NAME`] — carried here too so a caller that stashed a
    /// `PremiumInfo` doesn't need to keep the originating table pointer
    /// around just to remember which ABI version it came from.
    pub abi_version: u32,
}

/// `extern "C" fn(out_info: *mut PremiumInfo) -> FfiResult`
///
/// Writes dylib metadata into `*out_info` on [`FfiResult::Ok`]. Never
/// gated behind a license — "what edition is this binary" is metadata, not
/// a premium capability (contrast [`CapabilityStatusFn`], which is gated).
pub type PremiumInfoFn = unsafe extern "C" fn(out_info: *mut PremiumInfo) -> i32;

/// `extern "C" fn(capability: u32, out_status: *mut u32) -> FfiResult`
///
/// `capability` is a [`crate::Capability`] discriminant (see that type for
/// why the parameter type is `u32`, not `Capability`). On
/// [`FfiResult::Ok`], `*out_status` holds a [`crate::CapabilityStatus`]
/// discriminant (see that type for why it's `u32`, not `CapabilityStatus`,
/// on this side of the boundary too).
pub type CapabilityStatusFn = unsafe extern "C" fn(capability: u32, out_status: *mut u32) -> i32;

/// The vtable [`ENTRY_SYMBOL_NAME`] hands back: everything the shell needs
/// to talk to a loaded premium dylib.
///
/// # Reading this struct safely
///
/// Check `abi_version == ABI_VERSION` **before** calling either function
/// pointer. A table with a different `abi_version` was not produced by
/// this version of this crate and this crate's function pointer *types*
/// (calling convention, parameter shapes) are not guaranteed to match
/// whatever actually lives at those offsets — the only field safe to read
/// on a version mismatch is `abi_version` itself. In practice this should
/// never happen (the loader only got here via [`ENTRY_SYMBOL_NAME`], which
/// already pins the struct shape by construction — see that constant's
/// "why a versioned symbol name" note) but the check costs nothing and
/// catches a corrupted/hand-edited dylib.
///
/// # Extending this struct
///
/// Once shipped, this exact struct shape is frozen. A field addition,
/// removal, reorder, or type change is a **new struct** exported under a
/// **new entry symbol** (`centinelo_premium_abi_v2` returning
/// `*const PremiumAbiV2`), never an in-place edit of this one — see
/// [`ENTRY_SYMBOL_NAME`]'s doc for why versioning at the symbol level
/// (rather than trying to make this struct forward/backward compatible via
/// trailing optional fields) is the simple, unambiguous rule this crate
/// commits to.
#[repr(C)]
pub struct PremiumAbiV1 {
    /// Always [`ABI_VERSION`] for a table reached through
    /// [`ENTRY_SYMBOL_NAME`]. Read this field first, always.
    pub abi_version: u32,
    /// Dylib identity/version metadata. Never license-gated.
    pub premium_info: PremiumInfoFn,
    /// Per-capability status probe. Internally license-gated inside
    /// `centinelo-premium` — see that crate's `capability_status_for` and
    /// this crate's [`crate::CapabilityStatus`] doc for the ordering guarantee.
    pub capability_status: CapabilityStatusFn,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_result_round_trips_through_i32() {
        for r in [
            FfiResult::Ok,
            FfiResult::NullPointer,
            FfiResult::Panic,
            FfiResult::UnknownCapability,
        ] {
            assert_eq!(FfiResult::from_i32(r.as_i32()), Some(r));
        }
    }

    #[test]
    fn ffi_result_from_i32_rejects_unknown_codes() {
        assert_eq!(FfiResult::from_i32(1), None);
        assert_eq!(FfiResult::from_i32(-4), None);
        assert_eq!(FfiResult::from_i32(i32::MAX), None);
    }

    #[test]
    fn entry_symbol_name_is_nul_terminated_with_no_interior_nul() {
        assert_eq!(
            *ENTRY_SYMBOL_NAME.last().unwrap(),
            0,
            "must end in a nul byte for libloading::Library::get"
        );
        assert!(
            ENTRY_SYMBOL_NAME[..ENTRY_SYMBOL_NAME.len() - 1]
                .iter()
                .all(|&b| b != 0),
            "must have no interior nul bytes"
        );
        // Also must be valid, printable ASCII - it's a linker symbol name.
        assert!(ENTRY_SYMBOL_NAME[..ENTRY_SYMBOL_NAME.len() - 1]
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b'_'));
    }

    #[test]
    fn entry_symbol_name_embeds_the_abi_version() {
        let s = std::str::from_utf8(&ENTRY_SYMBOL_NAME[..ENTRY_SYMBOL_NAME.len() - 1]).unwrap();
        assert_eq!(
            s,
            format!("centinelo_premium_abi_v{ABI_VERSION}"),
            "symbol name and ABI_VERSION drifted apart"
        );
    }

    /// `PremiumAbiV1` is handed across an FFI boundary as `*const
    /// PremiumAbiV1`; both sides must agree on its size/layout, which
    /// `#[repr(C)]` guarantees are stable *within a given field list*, but
    /// this pins the actual pointer-width-dependent size so a silent
    /// field-size change (e.g. a field accidentally becoming a fat
    /// pointer) fails CI loudly instead of shipping.
    #[test]
    fn premium_abi_v1_has_the_expected_size_on_64_bit() {
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<PremiumAbiV1>(), 24);
    }
}

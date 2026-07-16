//! Premium module loader: looks for `centinelo_premium` next to this
//! executable at startup, verifies its integrity signature, loads it, and
//! exposes a small handle the frontend can query through `commands.rs`.
//!
//! The load/verify/query logic below is adapted from the private premium
//! repo's `loader-poc` crate (`crates/loader-poc/src/loader.rs`) - that
//! crate is the tested reference implementation this is kept in sync
//! with; see `centinelo-premium-abi/README.md` (vendored alongside this
//! file - see `Cargo.toml`) for the full ABI contract this speaks, and
//! the premium repo's `docs/loader-integration.md` for the complete
//! design writeup (threat model, side-car signature rationale, etc).
//!
//! # Where the license check actually happens
//!
//! Not here. `capability_status` below never decides whether a feature is
//! licensed - it only ever relays what the loaded (closed-source) dylib
//! says. This file has no concept of a license at all, and depends on
//! nothing from the private `centinelo-license` crate - see
//! `Cargo.toml`'s dependency list for this crate: `centinelo-premium-abi`
//! (vendored, public), `libloading`, `ed25519-dalek`. That's it. See
//! `centinelo-premium-abi`'s crate doc, "Why the split is a dylib", for
//! why gating logic living in this file instead would defeat the whole
//! point - this file is public, forkable source, and a fork could just
//! delete an `if license.has(...)` if this file were the one deciding.
//!
//! # Never fails startup
//!
//! [`PremiumHandle::load`] cannot fail in a way that stops the app from
//! starting - a missing, corrupt, or tampered premium module all resolve
//! to ordinary free-mode operation, logged once at `info`/`warn` level for
//! diagnostics and never surfaced to the user as an error.

use std::ffi::CStr;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use centinelo_premium_abi::{
    expected_library_path, expected_signature_path, CapabilityStatus as AbiCapabilityStatus,
    EntryFn, FfiResult, PremiumAbiV1, PremiumInfo, ABI_VERSION, ENTRY_SYMBOL_NAME,
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use libloading::Library;
use serde::Serialize;
use tauri::AppHandle;

/// # DEV/TEST PLACEHOLDER — replace before shipping a real Pro build
///
/// This is `SigningKey::from_bytes(&[0x24; 32]).verifying_key().to_bytes()`,
/// the exact same fixed, publicly-documented dev/test seed `loader-poc`'s
/// own test fixtures and demo binary use (see that crate's
/// `tests/flow.rs` and `src/main.rs`), chosen so a locally-built
/// `centinelo-premium` signed via `premium/scripts/build-and-sign-premium.sh`
/// against that same test seed's private half will load correctly against
/// *this* placeholder during development, without needing Felix's real
/// key on hand.
///
/// **Before an official release build**: run
/// `premium-sign keygen --out-dir <offline location>` for real, replace
/// the bytes below with that run's `centinelo_libsign.pub` contents, and
/// re-sign the shipped `centinelo-premium` dylib with the matching real
/// private key. Until that swap happens, this shell will only ever accept
/// a `centinelo-premium` signed by the well-known dev/test key above,
/// which is a safe failure mode (it just means official installers built
/// before the swap silently run in free mode), not a security hole (the
/// dev/test private key being public doesn't let anyone bypass licensing;
/// it only lets them make officially-signed-*looking* files that this
/// placeholder pubkey, and only this placeholder, accepts).
const LIB_PUBKEY_BYTES: [u8; 32] = [
    0x58, 0x93, 0x66, 0x04, 0xab, 0xda, 0x11, 0x2b, 0xc9, 0x49, 0x33, 0x56, 0x9c, 0x82, 0xf8, 0xd0,
    0xcc, 0x0d, 0xdf, 0x92, 0xa3, 0xf8, 0x32, 0x9f, 0x2f, 0x44, 0x8f, 0x7f, 0x48, 0x4a, 0x59, 0x4c,
];

/// Handle stashed in Tauri's managed state (`app.manage(...)`) at startup;
/// see `lib.rs`'s `.setup()`. `Clone` is cheap (`Arc`), matching
/// `SidecarHandle`'s newtype-over-`Arc` pattern elsewhere in this crate.
#[derive(Clone)]
pub struct PremiumHandle(Arc<Inner>);

enum Inner {
    Loaded(PremiumRuntime),
    /// Anything other than a clean load - carries a short reason for the
    /// startup log line only, never shown to the user.
    Unavailable(&'static str),
}

impl PremiumHandle {
    /// Runs the full find/verify/load flow against the directory this
    /// executable lives in. Call once, at startup (see `lib.rs`).
    ///
    /// Takes `app` by value (an owned `AppHandle`), matching the
    /// `SidecarHandle::new(app.handle().clone(), ...)` call convention
    /// already used a few lines above this call site in `lib.rs` -
    /// `AppHandle` is Tauri's cheap-clone handle type, so callers pass
    /// `app.handle().clone()`, not a borrow.
    pub fn load(app: AppHandle) -> Self {
        let exe_dir = match std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
        {
            Some(dir) => dir,
            None => {
                log::warn!(
                    "premium: could not determine executable directory, staying in free mode"
                );
                return Self(Arc::new(Inner::Unavailable("no exe dir")));
            }
        };
        let pubkey = VerifyingKey::from_bytes(&LIB_PUBKEY_BYTES)
            .expect("LIB_PUBKEY_BYTES must be a valid Ed25519 public key - see its doc comment");

        match load_premium(&exe_dir, &pubkey) {
            Ok(runtime) => {
                let info = runtime.info();
                log::info!(
                    "premium: loaded {} (build {})",
                    info.as_ref().map(|i| i.edition.as_str()).unwrap_or("?"),
                    info.as_ref()
                        .map(|i| i.build_version.as_str())
                        .unwrap_or("?"),
                );
                let _ = app; // reserved: future use (e.g. emitting a "premium-ready" event)
                Self(Arc::new(Inner::Loaded(runtime)))
            }
            Err(reason) => {
                // NotFound is the ordinary Community-edition/not-yet-Pro
                // case - info, not a warning. Everything else (tampered,
                // ABI mismatch, load failure) is worth a warn-level line
                // for support/diagnostics, still never user-facing.
                if reason == "not found" {
                    log::info!("premium: no module found next to the executable, running free");
                } else {
                    log::warn!("premium: not loading module ({reason}), running free");
                }
                Self(Arc::new(Inner::Unavailable(reason)))
            }
        }
    }

    pub fn info(&self) -> Option<PremiumInfoView> {
        match &*self.0 {
            Inner::Loaded(runtime) => runtime.info(),
            Inner::Unavailable(_) => None,
        }
    }

    /// Short diagnostic string - `"loaded"`, or a short reason why not
    /// (`"not found"`, `"signature does not verify"`, ...). Not
    /// user-facing copy; intended for a support/about pane or a
    /// `--verbose` startup log, so a stuck "why doesn't Pro show up"
    /// report has an actual answer instead of a silent shrug.
    pub fn diagnostic(&self) -> &'static str {
        match &*self.0 {
            Inner::Loaded(_) => "loaded",
            Inner::Unavailable(reason) => reason,
        }
    }

    /// `capability` is a canonical feature name (e.g. `"blf_console"` -
    /// see `centinelo_premium_abi::Capability::feature_name`). An
    /// unrecognized name resolves to `Unavailable`, same as the module
    /// not being loaded at all - the frontend doesn't need to distinguish
    /// "typo'd the capability name" from "premium isn't here".
    pub fn capability_status(&self, capability: &str) -> CapabilityStatusView {
        let Some(cap) = centinelo_premium_abi::Capability::ALL
            .iter()
            .find(|c| c.feature_name() == capability)
        else {
            return CapabilityStatusView::Unavailable;
        };
        match &*self.0 {
            Inner::Loaded(runtime) => runtime.capability_status(*cap).into(),
            Inner::Unavailable(_) => CapabilityStatusView::Unavailable,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PremiumInfoView {
    pub edition: String,
    pub build_version: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatusView {
    Available,
    NotLicensed,
    NotImplemented,
    /// Premium module not loaded, an internal FFI failure occurred, or an
    /// unrecognized capability name was asked about - the frontend should
    /// treat this identically to `NotLicensed` for display purposes (show
    /// the free-tier UI), the distinction exists only for logs.
    Unavailable,
}

impl From<AbiCapabilityStatus> for CapabilityStatusView {
    fn from(status: AbiCapabilityStatus) -> Self {
        match status {
            AbiCapabilityStatus::Available => CapabilityStatusView::Available,
            AbiCapabilityStatus::NotLicensed => CapabilityStatusView::NotLicensed,
            AbiCapabilityStatus::NotImplemented => CapabilityStatusView::NotImplemented,
            AbiCapabilityStatus::Error => CapabilityStatusView::Unavailable,
        }
    }
}

// ---------------------------------------------------------------------
// The load/verify/query flow itself - see loader-poc's loader.rs (private
// premium repo) for the exhaustively-commented reference version this
// mirrors; comments here focus on what a shell maintainer needs to know,
// not the full design rationale (which lives there and in
// docs/loader-integration.md so it isn't duplicated across two repos).
// ---------------------------------------------------------------------

struct PremiumRuntime {
    _lib: Library,
    table: *const PremiumAbiV1,
}

// SAFETY: see loader-poc's PremiumRuntime for the full justification this
// mirrors - `table` points at 'static-for-the-dylib's-lifetime read-only
// data (a version tag + extern "C" fn pointers), never mutated after load,
// and Tauri's `app.manage(...)` requires Send + Sync for managed state.
unsafe impl Send for PremiumRuntime {}
unsafe impl Sync for PremiumRuntime {}

impl PremiumRuntime {
    fn info(&self) -> Option<PremiumInfoView> {
        let table = unsafe { &*self.table };
        let mut out = std::mem::MaybeUninit::<PremiumInfo>::uninit();
        let rc = unsafe { (table.premium_info)(out.as_mut_ptr()) };
        if FfiResult::from_i32(rc) != Some(FfiResult::Ok) {
            return None;
        }
        let info = unsafe { out.assume_init() };
        Some(PremiumInfoView {
            edition: unsafe { CStr::from_ptr(info.edition) }
                .to_string_lossy()
                .into_owned(),
            build_version: unsafe { CStr::from_ptr(info.build_version) }
                .to_string_lossy()
                .into_owned(),
        })
    }

    fn capability_status(&self, cap: centinelo_premium_abi::Capability) -> AbiCapabilityStatus {
        let table = unsafe { &*self.table };
        let mut out: u32 = 0;
        let rc = unsafe { (table.capability_status)(cap.as_u32(), &mut out) };
        if FfiResult::from_i32(rc) != Some(FfiResult::Ok) {
            return AbiCapabilityStatus::NotLicensed;
        }
        AbiCapabilityStatus::from_u32(out).unwrap_or(AbiCapabilityStatus::NotLicensed)
    }
}

/// `Err` carries a short, static reason string for logging only - never
/// shown to the user (see this module's doc, "Never fails startup").
fn load_premium(exe_dir: &Path, lib_pubkey: &VerifyingKey) -> Result<PremiumRuntime, &'static str> {
    let lib_path = expected_library_path(exe_dir);
    let sig_path = expected_signature_path(exe_dir);

    if !lib_path.is_file() {
        return Err("not found");
    }

    // Verify from bytes on disk BEFORE any library loading - Library::new
    // executes the target's load-time init code the moment it succeeds,
    // so a tampered file's code must never get that far. See
    // docs/loader-integration.md, "verify before load, not after".
    let lib_bytes = fs::read(&lib_path).map_err(|_| "could not read library file")?;
    let sig_bytes = fs::read(&sig_path).map_err(|_| "could not read signature file")?;
    let sig_array: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "signature file is the wrong length")?;
    let signature = Signature::from_bytes(&sig_array);
    if lib_pubkey.verify(&lib_bytes, &signature).is_err() {
        return Err("signature does not verify");
    }

    // SAFETY: runs the dylib's load-time init code - only reached after
    // the signature check above succeeded.
    let lib = unsafe { Library::new(&lib_path) }.map_err(|_| "failed to load library")?;

    // SAFETY: ENTRY_SYMBOL_NAME names the expected `EntryFn` signature;
    // Symbol::get validates the symbol exists before we call through it.
    let entry: libloading::Symbol<EntryFn> =
        unsafe { lib.get(ENTRY_SYMBOL_NAME) }.map_err(|_| "entry point not found")?;
    // SAFETY: EntryFn takes no arguments and returns either null or a
    // pointer valid for as long as `lib` stays loaded.
    let table_ptr: *const PremiumAbiV1 = unsafe { entry() };
    if table_ptr.is_null() {
        return Err("entry point returned null");
    }
    // SAFETY: non-null per the check above, valid per EntryFn's contract
    // for as long as `lib` (about to move into the returned PremiumRuntime)
    // stays loaded. Only `abi_version` is read here - the one field safe
    // to read regardless of version, per PremiumAbiV1's own doc.
    if unsafe { (*table_ptr).abi_version } != ABI_VERSION {
        return Err("unsupported ABI version");
    }

    Ok(PremiumRuntime {
        _lib: lib,
        table: table_ptr,
    })
}

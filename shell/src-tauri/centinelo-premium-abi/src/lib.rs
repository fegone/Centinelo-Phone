//! C ABI contract between `centinelo-shell` (public) and `centinelo-premium`
//! (private, this crate's sibling in `centinelo-premium`) — the wire format
//! two independently-compiled binaries use to talk to each other across a
//! `dlopen`/`LoadLibrary` boundary.
//!
//! # Dual-homed: this is the socket, not the plug
//!
//! This crate contains **no secrets and no feature logic** — no license
//! checking, no capability implementations, nothing that needs to stay
//! private. It is exactly the calling convention: symbol names, a version
//! tag, a capability enum, and a couple of `#[repr(C)]` structs. Because of
//! that, its source is meant to be **vendored verbatim into the public
//! `fegone/Centinelo-Phone` repo** (see `premium/docs/loader-integration.md`
//! for the exact steps), so the public shell can compile a real loader
//! against the real ABI instead of a hand-copied approximation that drifts.
//!
//! Consequences of being dual-homed, enforced by convention here:
//!
//! - **Zero dependencies beyond `std`** (see `Cargo.toml`'s comment) — every
//!   dependency here is one the public repo's Community build would have to
//!   accept too.
//! - **Nothing in this crate ever reads a license, a key, or a config
//!   file.** It only describes shapes and calling conventions. The private
//!   half (`centinelo-premium`) decides *what* a capability call returns;
//!   this crate only fixes *how* that answer is encoded on the wire.
//! - Changes here are changes to a contract both repos compile against.
//!   Treat every public item as append-only once shipped — see
//!   [`PremiumAbiV1`] and [`Capability`]'s docs for the specific rules.
//!
//! # Why the split is a dylib, not a static link
//!
//! The product spec (`docs/SPEC-2026-07-15-centinelo-2.0-design.md` §2)
//! draws `centinelo-core` "linking" to `centinelo-premium` in official
//! builds. This crate (and its sibling `centinelo-premium`) implements that
//! link as a **runtime-loaded dynamic library**, not a compile-time static
//! link, specifically so:
//!
//! 1. The public repo can build a complete, working "Community edition"
//!    with `cargo build`/`tauri build` alone — no private crate on the
//!    dependency graph, ever (a path dependency on a private repo would
//!    break that build for anyone who isn't Felix or Edgar).
//! 2. Feature gating lives in *compiled, closed-source* code
//!    (`centinelo-premium`'s cdylib), not in an `if license.has(...)` that
//!    a fork of the public repo could just delete. See
//!    `premium/docs/loader-integration.md`'s "Where the gating actually
//!    happens" section for the full reasoning — the short version is: the
//!    shell never decides whether a capability is licensed, it only ever
//!    *asks* the dylib, and the dylib is the only thing that isn't
//!    forkable.
//!
//! # Panic safety at the FFI boundary
//!
//! Unwinding a Rust panic across an `extern "C"` boundary is undefined
//! behavior (and with `panic = "abort"`, it's an instant process abort
//! either way — see the warning on [`ffi_guard`]). Every `extern "C"`
//! function `centinelo-premium` exports **must** run its body through
//! [`ffi_guard`], which converts a caught panic into
//! [`FfiResult::Panic`] instead of letting it unwind into the shell's stack.
//! This is a hard rule, not a suggestion: a premium capability panicking
//! must never take the whole softphone down mid-call. See [`ffi_guard`]'s
//! doc comment for the mechanics and the `panic = "abort"` gotcha.
//!
//! # Pointer lifetime contract
//!
//! Every pointer this ABI hands the caller (the [`PremiumAbiV1`] table
//! itself, and the `*const c_char` fields inside [`PremiumInfo`]) points at
//! `'static`-within-the-dylib data: valid for as long as the originating
//! library stays loaded, invalid the instant it's unloaded. A caller that
//! needs data to outlive the current call (or the library's lifetime) must
//! copy it out (e.g. `CStr::to_string_lossy().into_owned()`) immediately —
//! see `centinelo-premium-abi`'s consumer, `loader-poc`'s `PremiumRuntime`,
//! for the reference pattern. Nothing here is reference-counted or
//! garbage-collected; ownership is "the dylib owns it, you borrow it".

mod abi;
mod capability;
mod panic_guard;
mod paths;

pub use abi::{
    CapabilityStatusFn, FfiResult, PremiumAbiV1, PremiumInfo, PremiumInfoFn, ABI_VERSION,
    ENTRY_SYMBOL_NAME,
};
pub use capability::{Capability, CapabilityStatus};
pub use panic_guard::ffi_guard;
pub use paths::{
    expected_library_filename, expected_library_path, expected_signature_path,
    SIGNATURE_FILE_SUFFIX,
};

/// The FFI entry point's function type: `extern "C" fn() -> *const PremiumAbiV1`,
/// looked up by [`ENTRY_SYMBOL_NAME`] via `libloading::Library::get`.
///
/// Returns a null pointer (never a dangling/invalid one) if the dylib
/// cannot hand back a valid table for any reason — see [`abi`]'s docs.
/// Callers must null-check before dereferencing; see `loader-poc`'s
/// `load_premium` for the reference check.
pub type EntryFn = unsafe extern "C" fn() -> *const PremiumAbiV1;

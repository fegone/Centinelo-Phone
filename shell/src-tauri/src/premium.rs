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
use std::path::{Path, PathBuf};
use std::sync::Arc;

use centinelo_premium_abi::{
    expected_library_filename, expected_library_path, expected_signature_path,
    CapabilityStatus as AbiCapabilityStatus, EntryFn, FfiResult, PremiumAbiV1, PremiumInfo,
    ABI_VERSION, ENTRY_SYMBOL_NAME,
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use libloading::Library;
use serde::Serialize;
use tauri::{AppHandle, Manager};

/// # DEV/TEST PLACEHOLDER — replace before shipping a real Pro build
///
/// This is the verifying key derived from a fixed, well-known dev/test
/// seed shared with `loader-poc`'s own test fixtures and demo binary
/// (see that crate's `tests/flow.rs` and `src/main.rs`), chosen so a
/// locally-built `centinelo-premium` signed via
/// `premium/scripts/build-and-sign-premium.sh` against that same test
/// seed's private half will load correctly against *this* placeholder
/// during development, without needing Felix's real key on hand. The
/// seed's literal value is intentionally not written here — it lives in
/// the private `centinelo-premium` repo (and in `loader-poc`'s test
/// fixtures) for anyone with access who needs to reproduce a matching
/// signature.
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
        // Needed for `stage_verified_copy` (see `load_premium`'s TOCTOU
        // note for the full reasoning, including why this only narrows
        // the exposure on macOS and is a no-op on Windows) - the verified
        // library bytes get staged here, never reloaded from `exe_dir`.
        let app_data_dir = match app.path().app_data_dir() {
            Ok(dir) => dir,
            Err(_) => {
                log::warn!(
                    "premium: could not determine app data directory, staying in free mode"
                );
                return Self(Arc::new(Inner::Unavailable("no app data dir")));
            }
        };
        let pubkey = VerifyingKey::from_bytes(&LIB_PUBKEY_BYTES)
            .expect("LIB_PUBKEY_BYTES must be a valid Ed25519 public key - see its doc comment");

        match load_premium(&exe_dir, &app_data_dir, &pubkey) {
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
fn load_premium(
    exe_dir: &Path,
    app_data_dir: &Path,
    lib_pubkey: &VerifyingKey,
) -> Result<PremiumRuntime, &'static str> {
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

    // TOCTOU (2026-08-16 security review, RISK: high; narrowed-not-closed
    // wording fixed 2026-08-16 round 2 - see the shell-tauri report for
    // both rounds). `Library::new` below must load *exactly* the bytes
    // just verified, not reopen `lib_path` by path - a
    // `Library::new(&lib_path)` here would re-read `lib_path` from disk a
    // second time, after the signature check above, so the bytes that
    // actually get executed would never be guaranteed to be the bytes
    // that were verified. The race doesn't need microsecond timing either:
    // every app *launch* is another attempt, so a background loop
    // alternating a legitimate and a malicious dylib on disk eventually
    // wins regardless of how narrow the window is.
    //
    // What staging into `app_data_dir` actually buys - be precise here,
    // it's easy to overclaim: this does NOT eliminate check-then-use.
    // `Library::new` below still loads `staged.path` *by path*, a second
    // filesystem operation after the write, so in the abstract, whoever
    // can write to `app_data_dir` between that write and this load still
    // has a race window (the staged filename isn't secret either - the
    // pid is visible via `ps`, and `next_stage_nonce` starts from 0). What
    // this change actually does is swap *who* that "whoever" can be:
    //   - `exe_dir` (the old, buggy target) is not a single-writer
    //     location on macOS - it's commonly `/Applications/...`, writable
    //     by any local admin account, not just this app's own user. That
    //     was the exploitable case: an attacker without this user's
    //     session, via another admin account, tampering with a dylib this
    //     user's app would then load and run.
    //   - `app_data_dir` is writable only by the user running this app -
    //     the same trust boundary `settings.rs` already depends on for
    //     the plaintext SIP secret. An attacker who can already write
    //     there as that user has strictly cheaper, race-free ways in
    //     (`DYLD_INSERT_LIBRARIES`, reading `settings.json`'s secret
    //     directly, straight memory injection into this process) - none
    //     of which need to win a race against `stage_verified_copy`.
    // So: this narrows the window from "any local admin, no race skill
    // needed in practice" to "the same user this process already runs
    // as, who has no reason to bother racing it." That's what closes the
    // *practically exploitable* case, not TOCTOU as an abstract pattern.
    //
    // Windows: `tauri.conf.json`'s `bundle.windows.nsis.installMode` is
    // `"currentUser"` (see that file, line ~55) - the installer places
    // this app under `%LOCALAPPDATA%\Programs\...`, already writable by
    // the same user without admin rights. So on Windows `exe_dir` and
    // `app_data_dir` sit in the *same* trust boundary already - moving
    // the load off `exe_dir` buys nothing there. This block is done on
    // Windows too only for one code path across both OSes, not because
    // it changes Windows's exposure.
    let staged = stage_verified_copy(app_data_dir, &lib_bytes)?;

    // SAFETY: runs the dylib's load-time init code - only reached after
    // the signature check above succeeded. Loading `staged.path` (not
    // `lib_path`) is what narrows the TOCTOU window documented above from
    // "any local admin on the machine" down to "the same user this
    // process already runs as" (macOS; a no-op on Windows - see that same
    // comment) - it does not remove the check-then-use shape itself.
    let lib = unsafe { Library::new(&staged.path) }.map_err(|_| "failed to load library")?;

    // Best-effort cleanup, not required for correctness: on unix,
    // unlinking a file with an active dlopen mapping is safe (the inode
    // stays alive until the mapping is dropped) and leaves nothing on
    // disk afterward. On Windows this is expected to fail while the
    // module stays loaded (`LoadLibrary` doesn't request
    // `FILE_SHARE_DELETE`) - harmless; `stage_verified_copy`'s own sweep
    // picks it up on the next launch instead. Either way this runs after
    // `staged.path` has already been consumed by `Library::new` above.
    staged.remove_best_effort();

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

/// Subdirectory of `app_data_dir` that verified premium-dylib copies get
/// staged into - see `load_premium`'s TOCTOU note for why this exists.
/// Deliberately a subdirectory rather than `app_data_dir` itself, so the
/// sweep in `stage_verified_copy` can safely `remove_file` every entry it
/// finds without needing to know the names of unrelated files (settings,
/// license cache, ...) this app also keeps in `app_data_dir`.
const PREMIUM_RUNTIME_SUBDIR: &str = "premium-runtime";

/// A premium dylib copy staged inside `app_data_dir`, holding exactly the
/// bytes that were signature-verified moments earlier in `load_premium`.
struct StagedCopy {
    path: PathBuf,
}

impl StagedCopy {
    /// Consumes `self` - callers pass `staged.path` to `Library::new`
    /// first, then call this. See the call site in `load_premium` for why
    /// a failure here is expected (and harmless) on Windows.
    fn remove_best_effort(self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Writes `verified_bytes` (already signature-checked by the caller) to a
/// fresh, process-unique file under `app_data_dir/premium-runtime/`, and
/// returns its path. Never reads `verified_bytes` back from disk to
/// double-check them - the point is that the caller's in-memory buffer
/// *is* what ends up on disk, with no read of an attacker-reachable path
/// in between.
fn stage_verified_copy(
    app_data_dir: &Path,
    verified_bytes: &[u8],
) -> Result<StagedCopy, &'static str> {
    let dir = app_data_dir.join(PREMIUM_RUNTIME_SUBDIR);
    fs::create_dir_all(&dir).map_err(|_| "could not create premium runtime dir")?;

    // Best-effort sweep of anything a previous run left behind (crashed
    // before reaching `remove_best_effort`, or hit the expected-on-Windows
    // failure documented there). Failures here (e.g. a file still locked
    // by another running instance of this app) are ignored - same
    // best-effort posture as `settings.rs`'s `write_private_file` cleanup,
    // and harmless: a stray file left in this directory can only ever be
    // loaded by a *future* verified-copy write choosing the same name,
    // which `private_copy_filename`'s pid suffix already avoids for any
    // process still alive.
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let _ = fs::remove_file(entry.path());
        }
    }

    let path = dir.join(private_copy_filename(std::process::id(), next_stage_nonce()));
    // `write_private_file` (mode 0600 on unix; relies on the containing
    // directory's ACLs on Windows, same as every other file this app
    // keeps in `app_data_dir`) writes to a `.tmp.<pid>` sibling and
    // renames into place - the rename is same-directory and therefore
    // atomic, so `path` is never observable half-written.
    crate::settings::write_private_file(&path, verified_bytes)
        .map_err(|_| "could not stage verified library copy")?;
    Ok(StagedCopy { path })
}

/// Per-process call counter folded into [`private_copy_filename`] alongside
/// the pid. `load_premium` only ever runs once per process today (see
/// `lib.rs`'s single `PremiumHandle::load` call in `.setup()`), so the pid
/// alone would be enough for the shipped product - the counter exists so
/// that stays true by construction rather than by convention: any future
/// caller invoking `load_premium` more than once in the same process (a
/// hot-reload command, a retry path, this module's own test suite calling
/// it in a loop) still gets a distinct staging path every time, instead of
/// silently reusing one and depending on `remove_best_effort` having
/// already run.
fn next_stage_nonce() -> u64 {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// `<library filename>.<pid>.<nonce>.<original extension>`, e.g.
/// `libcentinelo_premium.4821.3.dylib` - pid+nonce-suffixed so two
/// instances of this app running at once, or two calls within the same
/// process (see [`next_stage_nonce`]), never contend for the same file.
fn private_copy_filename(pid: u32, nonce: u64) -> String {
    let name = expected_library_filename();
    match name.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}.{pid}.{nonce}.{ext}"),
        None => format!("{name}.{pid}.{nonce}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    /// Fixed test-only seed - same pattern as `activation.rs`'s
    /// `TEST_ACTIVATION_SEED`. Not a real key, never used to sign a real
    /// build; `load_premium` takes `lib_pubkey` as a parameter precisely
    /// so tests don't need to touch `LIB_PUBKEY_BYTES`.
    const TEST_SEED: [u8; 32] = [0x42; 32];

    fn test_signing_key() -> SigningKey {
        SigningKey::from_bytes(&TEST_SEED)
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "centinelo-premium-toctou-test.{name}.{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---- stage_verified_copy: the piece the TOCTOU fix actually adds ---

    #[test]
    fn stage_verified_copy_writes_the_exact_bytes_it_was_given() {
        let app_data_dir = scratch_dir("stage-exact");
        let bytes = b"not a real dylib - just a buffer to round-trip through staging";
        let staged = stage_verified_copy(&app_data_dir, bytes).unwrap();
        assert_eq!(fs::read(&staged.path).unwrap(), bytes);
        assert!(staged
            .path
            .starts_with(app_data_dir.join(PREMIUM_RUNTIME_SUBDIR)));
        let _ = fs::remove_dir_all(&app_data_dir);
    }

    #[cfg(unix)]
    #[test]
    fn stage_verified_copy_is_owner_only_not_group_or_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let app_data_dir = scratch_dir("stage-perms");
        let staged = stage_verified_copy(&app_data_dir, b"contents don't matter here").unwrap();
        let mode = fs::metadata(&staged.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the staged copy lives in the same trust boundary as settings.json (SIP secret) \
             and must carry the same 0600 permissions"
        );
        let _ = fs::remove_dir_all(&app_data_dir);
    }

    #[test]
    fn stage_verified_copy_sweeps_stale_leftovers_from_a_previous_run() {
        let app_data_dir = scratch_dir("stage-sweep");
        let runtime_dir = app_data_dir.join(PREMIUM_RUNTIME_SUBDIR);
        fs::create_dir_all(&runtime_dir).unwrap();
        fs::write(
            runtime_dir.join("libcentinelo_premium.999999.0.dylib"),
            b"stale leftover from a crashed or Windows-locked previous run",
        )
        .unwrap();
        assert_eq!(fs::read_dir(&runtime_dir).unwrap().count(), 1);

        let staged = stage_verified_copy(&app_data_dir, b"fresh verified bytes").unwrap();

        let remaining: Vec<_> = fs::read_dir(&runtime_dir).unwrap().collect();
        assert_eq!(
            remaining.len(),
            1,
            "the sweep must remove the stale file, leaving only the freshly staged one"
        );
        assert_eq!(fs::read(&staged.path).unwrap(), b"fresh verified bytes");
        let _ = fs::remove_dir_all(&app_data_dir);
    }

    #[test]
    fn private_copy_filename_is_unique_per_pid_and_nonce_and_keeps_the_extension() {
        let a = private_copy_filename(111, 0);
        let b = private_copy_filename(111, 1);
        let c = private_copy_filename(222, 0);
        assert_ne!(a, b, "same pid, different nonce must not collide");
        assert_ne!(a, c, "same nonce, different pid must not collide");
        let ext = expected_library_filename().rsplit('.').next().unwrap();
        assert!(a.ends_with(&format!(".{ext}")));
    }

    // ---- load_premium: the TOCTOU invariant itself -----------------------
    //
    // These build a real (tiny) loadable dylib with `cc` so the test can
    // tell "Library::new got a real shared library" (fails later, at
    // `lib.get(ENTRY_SYMBOL_NAME)`, with "entry point not found" - our
    // fixture doesn't export the premium ABI symbol) apart from "Library::new
    // got garbage" (fails immediately with "failed to load library"). That
    // distinction is what lets the race test below prove which bytes
    // actually got executed, not just which bytes got *verified*.

    #[cfg(target_os = "macos")]
    fn compile_fixture_dylib(out_path: &Path) -> bool {
        let src_path = out_path.with_extension("c");
        if fs::write(
            &src_path,
            "int centinelo_toctou_fixture_marker(void) { return 42; }\n",
        )
        .is_err()
        {
            return false;
        }
        std::process::Command::new("cc")
            .arg("-dynamiclib")
            .arg("-o")
            .arg(out_path)
            .arg(&src_path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn load_premium_stages_the_verified_bytes_before_loading_them() {
        // Baseline (non-racy) proof that the staged copy - not `lib_path` -
        // is what gets loaded: sanity check for the race test below, which
        // depends on this same fixture/signing setup working at all.
        let exe_dir = scratch_dir("stage-then-load-exe");
        let app_data_dir = scratch_dir("stage-then-load-appdata");
        let fixture_path = exe_dir.join("fixture-source.dylib");
        if !compile_fixture_dylib(&fixture_path) {
            eprintln!("skipping: `cc -dynamiclib` unavailable in this environment");
            return;
        }
        let good_bytes = fs::read(&fixture_path).unwrap();
        let signing_key = test_signing_key();
        let verifying_key = signing_key.verifying_key();
        let signature = signing_key.sign(&good_bytes);

        fs::write(expected_library_path(&exe_dir), &good_bytes).unwrap();
        fs::write(expected_signature_path(&exe_dir), signature.to_bytes()).unwrap();

        // `match` on a `&'static str` reason rather than `assert_eq!` on
        // the whole `Result` - `PremiumRuntime` (the `Ok` payload) wraps a
        // raw pointer and a `Library` and intentionally implements neither
        // `Debug` nor `PartialEq`.
        match load_premium(&exe_dir, &app_data_dir, &verifying_key) {
            Err("entry point not found") => {}
            Err(other) => panic!(
                "expected \"entry point not found\" (a real dylib loaded, then rejected for \
                 lacking the premium ABI symbol), got {other:?} instead - staging/loading is broken"
            ),
            Ok(_) => panic!(
                "expected the load to fail at the entry-point-symbol step (the fixture dylib \
                 doesn't export it) - it fully succeeded instead, which would mean the ABI \
                 check no longer runs"
            ),
        }
        // The best-effort post-load cleanup must have removed the staged
        // copy (unix: safe to unlink a mapped file).
        let runtime_dir = app_data_dir.join(PREMIUM_RUNTIME_SUBDIR);
        assert_eq!(
            fs::read_dir(&runtime_dir).unwrap().count(),
            0,
            "the staged copy should be cleaned up after a successful load on unix"
        );

        let _ = fs::remove_dir_all(&exe_dir);
        let _ = fs::remove_dir_all(&app_data_dir);
    }

    /// The actual TOCTOU simulation: a background thread continuously
    /// overwrites `lib_path` (alternating a real signed dylib and garbage)
    /// for the whole duration of a loop of `load_premium` calls - i.e. the
    /// exact window `load_premium`'s TOCTOU doc comment describes, forced
    /// to fire far more often than an attacker would ever need to, since a
    /// real attacker gets one attempt per app *launch*, not per loop
    /// iteration.
    ///
    /// Mutation check (2026-08-16): reintroducing the bug -
    /// `Library::new(&lib_path)` instead of `Library::new(&staged.path)` -
    /// makes this test fail reliably (see the shell-tauri report for the
    /// captured before/after output). That line is the only thing this
    /// test depends on; everything else in `load_premium` is incidental to
    /// it.
    #[cfg(target_os = "macos")]
    #[test]
    fn concurrent_swap_of_lib_path_never_lets_load_premium_execute_unverified_bytes() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::thread;

        let exe_dir = scratch_dir("race-exe");
        let app_data_dir = scratch_dir("race-appdata");
        let fixture_path = exe_dir.join("fixture-source.dylib");
        if !compile_fixture_dylib(&fixture_path) {
            eprintln!("skipping: `cc -dynamiclib` unavailable in this environment");
            return;
        }
        let good_bytes = fs::read(&fixture_path).unwrap();
        // Never a valid Mach-O (wrong magic number) - if `Library::new`
        // ever gets handed this, it must fail with "failed to load
        // library", never quietly succeed.
        let bad_bytes = vec![0x41u8; good_bytes.len().max(4096)];

        let signing_key = test_signing_key();
        let verifying_key = signing_key.verifying_key();
        let signature = signing_key.sign(&good_bytes);

        let lib_path = expected_library_path(&exe_dir);
        let sig_path = expected_signature_path(&exe_dir);
        // The signature is computed once, over `good_bytes`, and never
        // rewritten - only `lib_path`'s *content* gets swapped below, so a
        // successful signature check always means the reader happened to
        // see `good_bytes` at that instant.
        fs::write(&sig_path, signature.to_bytes()).unwrap();
        fs::write(&lib_path, &good_bytes).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let swapper = {
            let stop = stop.clone();
            let lib_path = lib_path.clone();
            let good_bytes = good_bytes.clone();
            let bad_bytes = bad_bytes.clone();
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = fs::write(&lib_path, &bad_bytes);
                    let _ = fs::write(&lib_path, &good_bytes);
                }
            })
        };

        let saw_real_load = AtomicUsize::new(0);
        let saw_bad_library_load = AtomicUsize::new(0);
        for _ in 0..200 {
            match load_premium(&exe_dir, &app_data_dir, &verifying_key) {
                Err("entry point not found") => {
                    saw_real_load.fetch_add(1, Ordering::Relaxed);
                }
                Err("failed to load library") => {
                    saw_bad_library_load.fetch_add(1, Ordering::Relaxed);
                }
                // "signature does not verify" / "could not read library
                // file" - the reader caught `lib_path` mid-swap or holding
                // `bad_bytes`, before ever passing the signature check.
                // Expected under a race like this, and not the property
                // under test.
                _ => {}
            }
        }

        stop.store(true, Ordering::Relaxed);
        swapper.join().unwrap();

        assert_eq!(
            saw_bad_library_load.load(Ordering::Relaxed),
            0,
            "a signature-verified load must never end up executing bytes that were swapped in \
             after the verify step - this is exactly the TOCTOU this test exists to catch"
        );
        assert!(
            saw_real_load.load(Ordering::Relaxed) > 0,
            "the race never landed a single verified load in 200 attempts on this machine - \
             the test isn't exercising the intended window, its result doesn't mean anything"
        );

        let _ = fs::remove_dir_all(&exe_dir);
        let _ = fs::remove_dir_all(&app_data_dir);
    }
}

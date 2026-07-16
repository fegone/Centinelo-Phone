//! The one required pattern every `extern "C"` function in
//! `centinelo-premium` must use.

use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::abi::FfiResult;

/// Runs `f`, catching any panic and converting it to [`FfiResult::Panic`]
/// instead of letting it unwind out of the `extern "C"` function that
/// called this. Every FFI-exported function in `centinelo-premium` wraps
/// its entire body in this — see the crate doc's "Panic safety at the FFI
/// boundary" section for why that's a hard rule, not a suggestion.
///
/// # The `panic = "abort"` gotcha
///
/// `catch_unwind` can only catch a panic if the active panic strategy is
/// `"unwind"` (Rust's default). If a crate (or, worse, the workspace's
/// `[profile.release]`) sets `panic = "abort"`, every panic becomes an
/// immediate process abort *before* `catch_unwind` ever gets a chance to
/// run — silently defeating this entire function and turning "a premium
/// capability had a bug" into "the whole softphone just vanished
/// mid-call", which directly violates the design constraint that a
/// missing/broken premium module must degrade, never crash. Neither this
/// workspace's root `Cargo.toml` nor `centinelo-premium/Cargo.toml` set
/// `panic = "abort"` anywhere, and `centinelo-premium`'s own test suite
/// (`panic_is_caught_not_propagated`) proves `catch_unwind` is actually
/// live. **Do not add a `[profile.*] panic = "abort"` to any profile this
/// cdylib builds under** — if a future contributor is tempted to (it's a
/// common size/perf optimization for *binaries* that don't need to catch
/// anything), this crate needs a different mechanism first.
///
/// # Why `AssertUnwindSafe`
///
/// `f`'s closure environment is asserted unwind-safe rather than required
/// to prove it via `UnwindSafe`. This is the standard, accepted trade-off
/// for FFI boundary guards: we're not relying on any state the closure
/// captures being *logically* consistent after a caught panic (we're about
/// to report [`FfiResult::Panic`] and the caller is expected to treat the
/// whole call as failed, not to inspect partially-mutated state) — we only
/// need the process not to crash. `AssertUnwindSafe` says exactly that:
/// "I know a caught panic might leave captured state torn, and this
/// caller doesn't care because it discards that state either way".
pub fn ffi_guard<F>(f: F) -> FfiResult
where
    F: FnOnce() -> FfiResult,
{
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(FfiResult::Panic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_the_normal_result() {
        assert_eq!(ffi_guard(|| FfiResult::Ok), FfiResult::Ok);
        assert_eq!(
            ffi_guard(|| FfiResult::UnknownCapability),
            FfiResult::UnknownCapability
        );
    }

    #[test]
    fn catches_a_panic_instead_of_propagating_it() {
        // Silence the default panic-hook printing a backtrace to stderr for
        // this *expected* panic, so `cargo test` output for this test isn't
        // alarming; restore it afterwards regardless of outcome.
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ffi_guard(|| panic!("simulated capability bug"))
        }));
        std::panic::set_hook(previous_hook);

        // The key assertion: ffi_guard itself did NOT let the panic
        // unwind past it - the outer catch_unwind here is only a safety
        // net for the test harness, and finds nothing to catch.
        assert_eq!(result.unwrap(), FfiResult::Panic);
    }
}

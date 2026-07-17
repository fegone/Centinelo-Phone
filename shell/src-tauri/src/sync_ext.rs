//! Poison-recovering `Mutex` access (deuda fix, 2026-07-16).
//!
//! Every `Shared`-style struct in this crate (`sidecar::Shared`,
//! `hid::Shared`, `transcription`'s internal state, `settings::SettingsStore`)
//! holds several independent `std::sync::Mutex` fields and, until this fix,
//! locked every one of them with `.lock().expect("poisoned")`. A `Mutex`
//! only actually poisons when a thread panics *while holding the lock* -
//! but when that happens on any one of these fields (a bug in a completely
//! unrelated code path touching a completely unrelated field), the
//! `.expect()` on every *other* lock in the app that happens to be reached
//! next turns that one thread's panic into an app-wide crash, on the very
//! next line of code that merely wants to read, say, cached BLF state or
//! HID LED state - state that has nothing to do with whatever originally
//! panicked and is still perfectly readable.
//!
//! `lock_or_recover` instead does what `provisioning::ProvisioningPending::lock`
//! already established as this crate's precedent for exactly this
//! situation: `unwrap_or_else(|e| e.into_inner())` recovers the
//! last-known-good value inside a poisoned `Mutex` and carries on. The
//! recovered value may be mid-mutation (whatever the panicking thread left
//! it as), same caveat any poison-recovery strategy has - preferable to a
//! guaranteed full-app crash for state this app already treats as
//! best-effort/eventually-consistent (call phase tracking, LED mirroring,
//! sidecar handle bookkeeping - none of it is a financial ledger).
use std::sync::{Mutex, MutexGuard};

pub(crate) trait PoisonRecover<T> {
    /// Locks `self`, recovering the guard from a poisoned lock instead of
    /// panicking - see this module's doc for why that's the right default
    /// here.
    fn lock_or_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> PoisonRecover<T> for Mutex<T> {
    fn lock_or_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn locks_normally_when_never_poisoned() {
        let m = Mutex::new(5);
        assert_eq!(*m.lock_or_recover(), 5);
    }

    #[test]
    fn recovers_the_last_known_value_instead_of_panicking_after_poisoning() {
        let m = Arc::new(Mutex::new(1));
        let m2 = Arc::clone(&m);
        // Poison it: panic while holding the lock, on another thread, same
        // as an unrelated bug elsewhere in the app touching this field
        // would.
        let _ = std::thread::spawn(move || {
            let mut guard = m2.lock().unwrap();
            *guard = 2;
            panic!("simulated unrelated panic while holding the lock");
        })
        .join();
        assert!(m.is_poisoned());
        // The whole point: this must return the last value, not panic.
        assert_eq!(*m.lock_or_recover(), 2);
    }
}

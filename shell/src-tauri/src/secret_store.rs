//! Where the SIP secret actually lives at rest.
//!
//! Before this module, `settings.rs` wrote the SIP `secret` field straight
//! into `settings.json` in cleartext (0600-mode file, atomic write - see
//! `write_private_file` - but cleartext all the same). That's fine against
//! *other users* of the same machine, but not against anything that reads
//! this one user's own profile wholesale: a backup tool, a sync client
//! pointed at the whole home directory by a well-meaning office IT setup,
//! or any other process already running as this user. The admin-unlock
//! password gets an Argon2 hash; the SIP credential that actually dials
//! out to a real PBX got nothing. This module is the fix: hand the secret
//! to the OS's own per-user credential store instead - macOS Keychain,
//! Windows Credential Manager (DPAPI-backed) - and keep `settings.json` as
//! a *fallback*, not the primary copy, once that succeeds.
//!
//! ## Why a trait, not a direct `keyring::Entry` call
//!
//! Two reasons, both load-bearing:
//!
//! 1. **Unit tests never touch a real OS credential store.** `cargo test`
//!    runs on CI runners (this project's own `release-ci` GitHub Actions
//!    workflows, macOS *and* Windows) where the login keychain / Credential
//!    Manager is either absent, locked, or would pop an interactive
//!    "allow keychain access?" prompt with nothing there to click it - the
//!    exact kind of environment `qa-e2e`'s own doc warns about for GUI
//!    automation, just one layer lower. `settings.rs`'s own tests
//!    (`#[cfg(test)]` modules already in that file) construct a
//!    `MemorySecretStore` instead - deterministic, no I/O, no platform
//!    dependency, matches the pattern `write_private_file_tests` already
//!    uses for the settings file itself (a real temp dir, never the real
//!    app-data dir).
//! 2. **The `CENTINELO_E2E_SCRIPT` driver (`e2e.rs`) needs the same
//!    escape hatch at runtime**, not just in `cargo test`. It runs a real,
//!    bundled app process against a real PBX (see that module's own doc),
//!    and that process may itself run in a CI runner or a fresh VM
//!    without a usable keychain session. `CENTINELO_E2E_SECRET_STORE=memory`
//!    swaps the backing store the exact same way `sidecar.rs`'s
//!    `CENTINELO_E2E_AUDIO=synthetic` already swaps the audio path for a
//!    scripted run: same convention, same shape, no new pattern
//!    introduced. Unset (the default for every real install) always
//!    means "the real OS credential store."
//!
//! ## The one entry this app ever stores
//!
//! There's exactly one `AccountSettings` today (see that struct's own
//! doc) - no per-account keying needed. `SERVICE`/`USERNAME` below are
//! fixed constants, not derived from the account's `host`/`ext`, so
//! changing the PBX host or extension doesn't orphan a stale keychain
//! entry under the old identity; `SettingsStore::update_account` simply
//! overwrites (or deletes) the one entry in place.

use std::sync::Mutex;

/// Fixed identity for the one credential this app ever asks the OS store
/// for. `SERVICE` matches `tauri.conf.json`'s `identifier` - the same
/// namespace this app's app-data directory, deep-link scheme, and updater
/// pubkey already live under, so a `Keychain Access.app`/Credential
/// Manager listing reads as unambiguously "Centinelo Phone" to anyone who
/// goes looking, the same way every other per-user resource this app owns
/// already does.
const SERVICE: &str = "com.centinelo.phone";
const USERNAME: &str = "sip-account-secret";

/// One-line, un-nested error type: every caller of this trait only ever
/// needs to know "did it work, and if not, what do I tell the log" - see
/// `settings.rs`'s `SettingsStore::load`/`update_account` for the two call
/// sites, both of which log-and-degrade rather than propagate this
/// upward (a credential-store hiccup must never be the reason the whole
/// app fails to start or a settings save fails outright - see this
/// module's own doc, point 1 of "why a trait").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretStoreError(pub String);

impl std::fmt::Display for SecretStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for SecretStoreError {}

/// `set()` then read it straight back and compare, before ever trusting
/// the store enough to let a caller scrub the on-disk fallback copy.
///
/// The reason this exists at all (2026-08-16, round 2 of review): a
/// backend that returns `Ok(())` from `set()` is NOT proof the value can
/// be read back later on this machine - a Windows Credential Manager
/// profile that behaves oddly, a credential scoped to expire with the
/// session, a backend that silently no-ops the write under some policy -
/// any of those would still return `Ok(())` from `set()` while leaving
/// nothing actually retrievable. Without this check, `load_with_store`'s
/// migration would trust that `Ok(())` and scrub `settings.json` in the
/// same breath, on a machine where the secret only ever existed in
/// memory for the rest of that one process's lifetime - a working
/// install today, silently unable to register on its very next launch,
/// with no error anywhere pointing at why. That regression (works today,
/// silently broken later) is strictly worse than the plaintext-file
/// finding this whole module exists to fix, and is exactly the failure
/// mode `settings.rs`'s `secret_synced` flag was built to keep off the
/// table - this function is what makes the flag's guarantee actually
/// hold instead of merely trusting `set()`'s return value.
///
/// Every caller of this function must treat its `Err` exactly like a
/// `set()` failure: `secret_synced = false`, leave the on-disk fallback
/// alone, retry next launch/save. See `settings.rs`'s
/// `SettingsStore::load_with_store` and `update_account`, the two
/// callers.
pub fn verified_set(store: &dyn SecretStore, secret: &str) -> Result<(), SecretStoreError> {
    store.set(secret)?;
    match store.get() {
        Ok(Some(readback)) if readback == secret => Ok(()),
        Ok(Some(_)) => Err(SecretStoreError(
            "write verification failed: read-back value did not match what was written".to_string(),
        )),
        Ok(None) => Err(SecretStoreError(
            "write verification failed: entry not found immediately after writing it".to_string(),
        )),
        Err(e) => Err(SecretStoreError(format!(
            "write verification failed: could not read back the value just written ({e})"
        ))),
    }
}

/// Where the SIP secret is read from / written to, independent of
/// `settings.json`. `Send + Sync` because `SettingsStore` (this trait's
/// only holder) is Tauri-managed state, shared across the IPC dispatch
/// threads and the sidecar supervisor's own background thread exactly
/// like every other field on that struct.
pub trait SecretStore: Send + Sync {
    /// `Ok(None)` = no entry (never configured, or already migrated away:
    /// genuinely nothing stored, not a failure). `Err` = the store
    /// itself couldn't be reached this call (locked, unavailable,
    /// permission denied, ...): callers must NOT treat this the same as
    /// `Ok(None)`; see `settings.rs` `SettingsStore::load`'s doc comment
    /// on why that distinction is the whole point.
    fn get(&self) -> Result<Option<String>, SecretStoreError>;

    /// Overwrites (or creates) the one entry. Never called with an empty
    /// `secret` - callers route that case to `delete` instead (see
    /// `settings.rs` `SettingsStore::update_account`).
    fn set(&self, secret: &str) -> Result<(), SecretStoreError>;

    /// Removes the entry. Idempotent: deleting an already-absent entry is
    /// `Ok(())`, not an error - mirrors `keyring::Entry::delete_credential`'s
    /// own "not found" case being folded in by each implementation below,
    /// so callers never need to check "does it exist first."
    fn delete(&self) -> Result<(), SecretStoreError>;
}

/// The real thing: macOS Keychain / Windows Credential Manager (DPAPI) /
/// Linux Secret Service, via the `keyring` crate's per-platform backends.
/// No fields - `keyring::Entry::new` is cheap enough (no I/O, just binds
/// the service/username pair) to call fresh on every method rather than
/// cache one, which sidesteps ever needing `unsafe impl Sync` reasoning
/// about the underlying platform handle.
pub struct OsKeystore;

impl OsKeystore {
    fn entry() -> Result<keyring::Entry, SecretStoreError> {
        keyring::Entry::new(SERVICE, USERNAME).map_err(|e| SecretStoreError(e.to_string()))
    }
}

impl SecretStore for OsKeystore {
    fn get(&self) -> Result<Option<String>, SecretStoreError> {
        let entry = Self::entry()?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretStoreError(e.to_string())),
        }
    }

    fn set(&self, secret: &str) -> Result<(), SecretStoreError> {
        let entry = Self::entry()?;
        entry.set_password(secret).map_err(|e| SecretStoreError(e.to_string()))
    }

    fn delete(&self) -> Result<(), SecretStoreError> {
        let entry = Self::entry()?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretStoreError(e.to_string())),
        }
    }
}

/// Test / `CENTINELO_E2E_SECRET_STORE=memory` double - see this module's
/// doc, "why a trait", point 1 and 2. Process-lifetime only, by design:
/// nothing here is meant to survive a restart, matching how
/// `CENTINELO_E2E_AUDIO=synthetic` never touches a real device either.
#[derive(Default)]
pub struct MemorySecretStore {
    value: Mutex<Option<String>>,
}

impl SecretStore for MemorySecretStore {
    fn get(&self) -> Result<Option<String>, SecretStoreError> {
        Ok(self.value.lock().unwrap_or_else(|p| p.into_inner()).clone())
    }

    fn set(&self, secret: &str) -> Result<(), SecretStoreError> {
        *self.value.lock().unwrap_or_else(|p| p.into_inner()) = Some(secret.to_string());
        Ok(())
    }

    fn delete(&self) -> Result<(), SecretStoreError> {
        *self.value.lock().unwrap_or_else(|p| p.into_inner()) = None;
        Ok(())
    }
}

/// Test-only double for the "the OS credential store itself is
/// unreachable" branch - `settings.rs`'s `SettingsStore::load_with_store`
/// / `update_account` tests inject this to exercise that path
/// deterministically. `MemorySecretStore` above can't stand in for it: it
/// never fails, by construction, so it only ever covers the happy path.
/// `pub(crate)`, not `#[cfg(test)]`-gated: `settings.rs`'s tests live in a
/// different module of the same crate and need to name this type; gating
/// it behind `#[cfg(test)]` here would work too (test builds compile both
/// modules' `#[cfg(test)]` code together) but this crate's existing test
/// doubles (e.g. `transcription.rs`'s mock binary helpers) aren't gated
/// that way either - matching that precedent.
#[cfg(test)]
pub(crate) struct FailingSecretStore;

#[cfg(test)]
impl SecretStore for FailingSecretStore {
    fn get(&self) -> Result<Option<String>, SecretStoreError> {
        Err(SecretStoreError("simulated: credential store unreachable".to_string()))
    }

    fn set(&self, _secret: &str) -> Result<(), SecretStoreError> {
        Err(SecretStoreError("simulated: credential store unreachable".to_string()))
    }

    fn delete(&self) -> Result<(), SecretStoreError> {
        Err(SecretStoreError("simulated: credential store unreachable".to_string()))
    }
}

/// Test-only double for the exact gap `verified_set` closes: a backend
/// that lies about a successful write. `set()` always returns `Ok(())`
/// (the outward behavior of the misbehaving Windows profile/expiring-
/// credential/policy-blocked-write scenarios `verified_set`'s own doc
/// describes), but `get()` never returns what was just written - here,
/// always `Ok(None)`, the simplest of the several shapes that failure
/// could actually take (a wrong value or a hard `Err` from `get()` would
/// be caught the same way, by the same `!= secret` / `Err` arms in
/// `verified_set`). Same `pub(crate)`, same not-`#[cfg(test)]`-gated
/// reasoning as `FailingSecretStore` above.
#[cfg(test)]
pub(crate) struct SucceedsWriteButCannotReadBackStore;

#[cfg(test)]
impl SecretStore for SucceedsWriteButCannotReadBackStore {
    fn get(&self) -> Result<Option<String>, SecretStoreError> {
        Ok(None)
    }

    fn set(&self, _secret: &str) -> Result<(), SecretStoreError> {
        Ok(())
    }

    fn delete(&self) -> Result<(), SecretStoreError> {
        Ok(())
    }
}

/// `CENTINELO_E2E_SECRET_STORE=memory` opts a real running app instance out
/// of the OS credential store - same escape hatch shape as
/// `sidecar.rs`'s `CENTINELO_E2E_AUDIO`. Unset (every real install, and
/// `cargo tauri dev` by default) always resolves to the real store.
const E2E_STORE_ENV: &str = "CENTINELO_E2E_SECRET_STORE";

pub fn default_secret_store() -> Box<dyn SecretStore> {
    if std::env::var(E2E_STORE_ENV).as_deref() == Ok("memory") {
        Box::new(MemorySecretStore::default())
    } else {
        Box::new(OsKeystore)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trips() {
        let store = MemorySecretStore::default();
        assert_eq!(store.get().unwrap(), None);
        store.set("s3cret").unwrap();
        assert_eq!(store.get().unwrap(), Some("s3cret".to_string()));
    }

    #[test]
    fn verified_set_succeeds_when_the_store_actually_holds_what_was_written() {
        let store = MemorySecretStore::default();
        assert!(verified_set(&store, "s3cret").is_ok());
        assert_eq!(store.get().unwrap(), Some("s3cret".to_string()));
    }

    #[test]
    fn verified_set_fails_when_set_ok_but_get_cannot_read_it_back() {
        // The exact scenario round 2 of review named: a backend whose
        // `set()` lies about success. `verified_set` must catch it, not
        // just propagate `set()`'s own `Ok(())`.
        let store = SucceedsWriteButCannotReadBackStore;
        let result = verified_set(&store, "s3cret");
        assert!(result.is_err(), "a set() that can't be read back must not be trusted");
    }

    #[test]
    fn verified_set_propagates_a_set_failure_unchanged() {
        let store = FailingSecretStore;
        assert!(verified_set(&store, "s3cret").is_err());
    }

    #[test]
    fn memory_store_delete_is_idempotent() {
        let store = MemorySecretStore::default();
        store.delete().unwrap(); // never set - must not error
        store.set("s3cret").unwrap();
        store.delete().unwrap();
        assert_eq!(store.get().unwrap(), None);
        store.delete().unwrap(); // already gone - still must not error
    }

    #[test]
    fn env_var_selects_memory_backend() {
        // SAFETY: test-only env mutation, same convention as
        // `sidecar.rs`'s `default_core_binary_path_finds_packaged_layout_beside_current_exe`
        // test - `E2E_STORE_ENV` is grep-confirmed read/written by no other
        // test in this crate, and the prior value (expected: unset) is
        // restored below regardless of outcome.
        let prior = std::env::var(E2E_STORE_ENV).ok();
        unsafe { std::env::set_var(E2E_STORE_ENV, "memory") };

        let store = default_secret_store();
        store.set("probe").unwrap();
        let result = store.get().unwrap();

        match prior {
            Some(v) => unsafe { std::env::set_var(E2E_STORE_ENV, v) },
            None => unsafe { std::env::remove_var(E2E_STORE_ENV) },
        }
        assert_eq!(result, Some("probe".to_string()));
    }
}

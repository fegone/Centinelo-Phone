//! Startup cleanup of a stale `%APPDATA%\.baresip\` profile left behind by a
//! now-fixed Windows engine bug — Windows only, see `lib.rs`'s call site.
//!
//! Background (the reason this module exists at all): until recently, the
//! engine's own argument parsing on MSVC builds silently ignored the `-f
//! <scratch_dir>` flag `sidecar.rs` passes it (see that fix's own PR for the
//! parsing bug itself). With no config path it could actually use, baresip
//! fell back to *its own* default profile location — `conf_path_get()` in
//! `core/deps/baresip/src/conf.c`, which on Windows resolves through
//! `fs_gethome()` (`core/deps/re/src/sys/fs.c`) to
//! `SHGetFolderPath(CSIDL_APPDATA)` + `\.baresip` — i.e. exactly
//! `%APPDATA%\.baresip`, a *sibling* of this app's own data directory
//! (`%APPDATA%\com.centinelo.phone`, Tauri's `app_data_dir()`), not a
//! subdirectory of it.
//!
//! During diagnosis on a real machine, a technician hand-copied a real SIP
//! `accounts` file into that directory to unblock the user while the root
//! cause was still unknown — that file carries `auth_pass` in plaintext.
//! Now that the parsing bug is fixed and the engine reads the shell's own
//! ephemeral scratch dir (`std::env::temp_dir()`, see `sidecar.rs`'s
//! `SpawnPlan`) again, `%APPDATA%\.baresip` is permanently orphaned: nothing
//! reads it anymore, but nothing ever deletes it either, so a plaintext SIP
//! password sits on disk indefinitely on every machine that hit the bug.
//! Anyone who never hit the bug never had this directory in the first place
//! (or has an empty/harmless one — see `looks_like_baresip_profile` below),
//! so this cleanup is a no-op for them.
//!
//! This has to run on **shell startup**, not the installer: a user updating
//! from the currently-shipped build goes through the auto-updater, never
//! the installer, so an installer-only fix would never reach the machines
//! that actually have the leftover file.
//!
//! ## What gets removed, and why the whole directory
//!
//! The whole `.baresip` directory, not just `accounts`/`config`. Two
//! reasons: (1) `accounts` is the file we know for certain carries a
//! password, but baresip's default profile can also grow `contacts` or a
//! call history — call metadata is its own kind of sensitive data we have
//! no business leaving around either, so a narrower delete just trades one
//! leftover for another; (2) once the fix in `sidecar.rs` is in place this
//! whole directory is dead weight — nothing this app does today or in any
//! planned future reads it — so there's no "the rest of the directory is
//! still useful" case to preserve.
//!
//! ## Why not delete unconditionally
//!
//! `%APPDATA%\.baresip` is baresip's own well-known default profile
//! location, not something this app invented — in the (acknowledged
//! unlikely, per the bug report this was written from) case that a
//! technician installed a standalone baresip build on the same Windows
//! account for unrelated reasons, blowing away their config on every launch
//! of this app would be hostile. Matching the *contents* of that profile
//! against this app's own configured SIP account (the other option
//! considered) was rejected: a mismatch there proves nothing — the leaked
//! `accounts` file could easily predate whatever's in this app's
//! `settings.json` today (rotated credentials, a technician who typed the
//! unblocking account by hand without also saving it in Settings) — so
//! trusting a match/mismatch either way risks the one outcome that actually
//! matters here, leaving a real password on disk. Instead this only checks
//! that the directory *looks like a baresip profile at all* (holds at least
//! one of the filenames baresip itself is known to write there) before
//! touching it — cheap, doesn't depend on this app's own state, and still
//! leaves alone a directory that isn't baresip-shaped in the first place.
//!
//! ## The one hard line
//!
//! `is_disjoint_from` below refuses to act if the computed target is the
//! app's own `app_data_dir` (`settings.json`, the admin password hash,
//! `license.json`, ...) or anywhere inside/above it. This should be
//! geometrically impossible in real usage (`.baresip` and
//! `com.centinelo.phone` are always siblings under `%APPDATA%`), but the
//! guard costs nothing and turns "impossible" into "checked", which is the
//! whole point of `never_touches_the_real_app_data_dir` below.

// Only ever called from lib.rs's `#[cfg(target_os = "windows")]` setup
// block (this bug, and thus its cleanup, is Windows-only — see the module
// doc above). On every other target the whole module compiles (so its
// tests still run everywhere, including this macOS dev machine) but has no
// caller, which `-D warnings` would otherwise flag as dead code.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use std::path::{Path, PathBuf};

/// Filenames baresip's own default profile (`config_write_template` in
/// `core/deps/baresip/src/config.c`, plus whatever it accumulates in normal
/// use) is known to write into `conf_path_get()`'s directory. Used only to
/// decide "does this look like one of baresip's own profiles" before
/// deleting it — see the module doc's "Why not delete unconditionally".
const BARESIP_PROFILE_MARKERS: &[&str] = &["accounts", "config", "contacts", "call_history.csv"];

/// `%APPDATA%\.baresip` given `%APPDATA%` itself — mirrors
/// `conf_path_get()` + `fs_gethome()`'s `CSIDL_APPDATA` lookup exactly (see
/// module doc), so this always points at the same directory the engine
/// itself fell back to under the now-fixed bug.
fn baresip_profile_dir(appdata_root: &Path) -> PathBuf {
    appdata_root.join(".baresip")
}

/// True if `dir` contains at least one file baresip's own default profile
/// is known to write. Deliberately loose (any one marker is enough) — the
/// goal is only to rule out "this directory has nothing to do with
/// baresip", not to fingerprint the exact bug scenario.
fn looks_like_baresip_profile(dir: &Path) -> bool {
    BARESIP_PROFILE_MARKERS.iter().any(|name| dir.join(name).is_file())
}

/// The hard safety line: refuses to treat `target` as disjoint from
/// `app_data_dir` if either contains the other (including equality). Both
/// directions matter — `target` inside `app_data_dir` would risk deleting
/// this app's own settings; `app_data_dir` inside `target` would mean
/// removing `target` takes the app's own settings with it.
fn is_disjoint_from(target: &Path, app_data_dir: &Path) -> bool {
    !target.starts_with(app_data_dir) && !app_data_dir.starts_with(target)
}

/// Removes the stale `%APPDATA%\.baresip` profile if — and only if — it
/// exists, looks like a baresip profile, and sits nowhere near this app's
/// own `app_data_dir`. Never panics and never propagates an error: this
/// runs during `lib.rs`'s `setup()`, and a locked file or a permissions
/// problem on some machine must not be the reason the app fails to open
/// (see module doc). Every branch just logs and returns.
///
/// `appdata_root` is `%APPDATA%` itself (the *parent* of both this app's
/// own data directory and baresip's `.baresip` default) — passed in rather
/// than read from the environment here so this stays plain, unit-testable
/// path logic; the one call site (`lib.rs`, Windows-only) is what actually
/// reads `APPDATA`.
pub fn cleanup_stale_baresip_profile(appdata_root: &Path, app_data_dir: &Path) {
    let target = baresip_profile_dir(appdata_root);

    if !is_disjoint_from(&target, app_data_dir) {
        log::warn!(
            "stale-profile cleanup: refusing — target {} overlaps app data dir {}",
            target.display(),
            app_data_dir.display()
        );
        return;
    }

    if !target.is_dir() {
        // The overwhelmingly common case: this machine never hit the
        // engine bug, so baresip never fell back to its own default
        // profile in the first place. Nothing to do.
        return;
    }

    if !looks_like_baresip_profile(&target) {
        log::info!(
            "stale-profile cleanup: {} exists but doesn't look like a baresip profile, leaving it alone",
            target.display()
        );
        return;
    }

    match std::fs::remove_dir_all(&target) {
        Ok(()) => log::info!(
            "stale-profile cleanup: removed stale baresip profile at {} (see profile_cleanup.rs module doc)",
            target.display()
        ),
        Err(e) => log::warn!(
            "stale-profile cleanup: failed to remove {}: {e} — leaving it in place, app startup continues",
            target.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("centinelo-profile-cleanup-test.{name}.{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn removes_stale_profile_containing_accounts_file() {
        let base = scratch_dir("removes-accounts");
        let appdata_root = base.join("AppData");
        let app_data_dir = appdata_root.join("com.centinelo.phone");
        fs::create_dir_all(&app_data_dir).unwrap();
        fs::write(app_data_dir.join("settings.json"), b"{}").unwrap();

        let stale = baresip_profile_dir(&appdata_root);
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("accounts"), b"<sip:1100@10.0.0.1:5060>;auth_pass=plaintext-secret\n").unwrap();

        cleanup_stale_baresip_profile(&appdata_root, &app_data_dir);

        assert!(!stale.exists(), "stale .baresip profile should have been removed");
        // The real app data dir and its settings.json must survive untouched.
        assert!(app_data_dir.is_dir());
        assert_eq!(fs::read(app_data_dir.join("settings.json")).unwrap(), b"{}");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn removes_stale_profile_containing_only_config_file() {
        // config-only (no accounts) is still a real baresip profile —
        // covers the "user reproduced the bug but a technician never got
        // to hand-copy an accounts file in" case.
        let base = scratch_dir("removes-config-only");
        let appdata_root = base.join("AppData");
        let app_data_dir = appdata_root.join("com.centinelo.phone");
        fs::create_dir_all(&app_data_dir).unwrap();

        let stale = baresip_profile_dir(&appdata_root);
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("config"), b"#\n# baresip configuration\n#\n").unwrap();

        cleanup_stale_baresip_profile(&appdata_root, &app_data_dir);

        assert!(!stale.exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_directory_is_a_silent_no_op() {
        // The common case — a machine that never hit the engine bug.
        let base = scratch_dir("missing-dir");
        let appdata_root = base.join("AppData");
        let app_data_dir = appdata_root.join("com.centinelo.phone");
        fs::create_dir_all(&app_data_dir).unwrap();

        cleanup_stale_baresip_profile(&appdata_root, &app_data_dir);

        assert!(!baresip_profile_dir(&appdata_root).exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn leaves_alone_a_directory_that_does_not_look_like_a_baresip_profile() {
        // Guards the "someone else's unrelated .baresip-named directory"
        // case from the module doc — no marker files, so this must not
        // touch it even though the name matches.
        let base = scratch_dir("unrelated-dir");
        let appdata_root = base.join("AppData");
        let app_data_dir = appdata_root.join("com.centinelo.phone");
        fs::create_dir_all(&app_data_dir).unwrap();

        let unrelated = baresip_profile_dir(&appdata_root);
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(unrelated.join("readme.txt"), b"not a baresip file").unwrap();

        cleanup_stale_baresip_profile(&appdata_root, &app_data_dir);

        assert!(unrelated.is_dir(), "unrelated directory must survive");
        assert!(unrelated.join("readme.txt").is_file());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn never_touches_the_real_app_data_dir_even_if_paths_collapse() {
        // The hard safety line: if some future refactor ever made the
        // computed target equal to (or an ancestor/descendant of) the
        // real app_data_dir, this must refuse outright rather than ever
        // delete anything under it — settings.json, the admin password
        // hash, license.json all live there.
        let base = scratch_dir("collapsed-paths");
        // Force an artificial collision: pretend the "baresip profile" IS
        // the app's own data dir (as if `appdata_root` were misconfigured
        // to already point at `.baresip`'s parent one level too deep).
        let app_data_dir = base.join(".baresip");
        fs::create_dir_all(&app_data_dir).unwrap();
        fs::write(app_data_dir.join("settings.json"), b"{\"real\":true}").unwrap();
        fs::write(app_data_dir.join("accounts"), b"<sip:1100@10.0.0.1>;auth_pass=leaked\n").unwrap();

        cleanup_stale_baresip_profile(&base, &app_data_dir);

        assert!(app_data_dir.is_dir(), "app_data_dir must survive");
        assert_eq!(fs::read(app_data_dir.join("settings.json")).unwrap(), b"{\"real\":true}");
        assert!(app_data_dir.join("accounts").is_file());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn refuses_when_app_data_dir_is_nested_inside_the_target() {
        // The other direction of the same guard: app_data_dir living
        // *under* the computed .baresip target.
        let base = scratch_dir("nested-app-data");
        let appdata_root = base.join("AppData");
        let stale = baresip_profile_dir(&appdata_root);
        let app_data_dir = stale.join("com.centinelo.phone");
        fs::create_dir_all(&app_data_dir).unwrap();
        fs::write(app_data_dir.join("settings.json"), b"{\"real\":true}").unwrap();
        fs::write(stale.join("accounts"), b"<sip:1100@10.0.0.1>;auth_pass=leaked\n").unwrap();

        cleanup_stale_baresip_profile(&appdata_root, &app_data_dir);

        assert!(stale.is_dir());
        assert!(app_data_dir.is_dir());
        assert_eq!(fs::read(app_data_dir.join("settings.json")).unwrap(), b"{\"real\":true}");
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn removal_failure_is_logged_and_does_not_panic() {
        use std::os::unix::fs::PermissionsExt;

        let base = scratch_dir("removal-failure");
        let appdata_root = base.join("AppData");
        let app_data_dir = appdata_root.join("com.centinelo.phone");
        fs::create_dir_all(&app_data_dir).unwrap();

        let stale = baresip_profile_dir(&appdata_root);
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("accounts"), b"<sip:1100@10.0.0.1>;auth_pass=leaked\n").unwrap();
        // Strip write+execute from the parent so remove_dir_all can't
        // unlink entries inside `stale` — simulates a locked-file /
        // permissions failure on a real Windows machine without needing
        // one. The call below must return normally, not panic.
        fs::set_permissions(&appdata_root, fs::Permissions::from_mode(0o500)).unwrap();

        cleanup_stale_baresip_profile(&appdata_root, &app_data_dir);

        // Restore permissions so the scratch dir can be cleaned up.
        fs::set_permissions(&appdata_root, fs::Permissions::from_mode(0o700)).unwrap();
        let _ = fs::remove_dir_all(&base);
    }
}

//! Settings persistence for Centinelo Phone.
//!
//! Everything lives in one JSON file in the Tauri app-data directory
//! (`settings.json`). The SIP `secret` field is the only sensitive value we
//! ever hold, and it is stored the same way the v1 Electron app stored it
//! (plaintext, in a settings file that lives under the OS's per-user
//! application-data directory, never in the repo, never logged). It is
//! *never* written anywhere else: not to logs, not to a second file, not
//! into the child process's environment (see sidecar.rs for why the scratch
//! `accounts` file baresip itself reads is the one documented exception,
//! matching core/run-spike.sh's own security note).
//!
//! The admin password is never stored in any recoverable form: only its
//! Argon2 hash (see `hash_password`/`verify_password`).

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const SETTINGS_FILE: &str = "settings.json";
pub const RECENTS_FILE: &str = "recents.json";
pub const MAX_RECENTS: usize = 200;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum TransportPriority {
    /// Try WSS first; fall back to classic UDP once if the initial
    /// registration attempt fails. See sidecar.rs `SidecarSupervisor` for
    /// the (intentionally simple, v0-scoped) fallback logic.
    #[default]
    Auto,
    Wss,
    Classic,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FavoriteSlot {
    #[serde(default)]
    pub ext: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountSettings {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub ext: String,
    #[serde(default)]
    pub secret: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub transport_priority: TransportPriority,
}

impl AccountSettings {
    pub fn is_configured(&self) -> bool {
        !self.host.trim().is_empty() && !self.ext.trim().is_empty() && !self.secret.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdminSettings {
    /// Argon2 PHC hash string, e.g. "$argon2id$v=19$...". `None` until the
    /// operator sets an admin password on first run.
    #[serde(default)]
    pub password_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemePref {
    #[default]
    Auto,
    Light,
    Dark,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub account: AccountSettings,
    #[serde(default)]
    pub admin: AdminSettings,
    #[serde(default)]
    pub favorites: Vec<FavoriteSlot>,
    #[serde(default)]
    pub theme: ThemePref,
    /// Explicit override for the core binary path. `None` = auto-resolve
    /// (see sidecar.rs `default_core_binary_path`).
    #[serde(default)]
    pub core_binary_path: Option<String>,
}

fn default_favorites() -> Vec<FavoriteSlot> {
    (1..=4)
        .map(|n| FavoriteSlot {
            ext: String::new(),
            label: format!("Favorite {n}"),
        })
        .collect()
}

/// Thread-safe settings handle managed as Tauri state. Holds the settings
/// file path (resolved once at startup) plus the in-memory copy, guarded by
/// a mutex since both Tauri commands (frontend-triggered) and the sidecar
/// supervisor (background thread) read account settings.
pub struct SettingsStore {
    path: PathBuf,
    recents_path: PathBuf,
    inner: Mutex<AppSettings>,
}

impl SettingsStore {
    pub fn load(app_data_dir: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(app_data_dir)?;
        let path = app_data_dir.join(SETTINGS_FILE);
        let recents_path = app_data_dir.join(RECENTS_FILE);

        let mut settings = if path.exists() {
            let raw = fs::read_to_string(&path)?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            AppSettings::default()
        };
        if settings.favorites.is_empty() {
            settings.favorites = default_favorites();
        }

        Ok(Self {
            path,
            recents_path,
            inner: Mutex::new(settings),
        })
    }

    pub fn snapshot(&self) -> AppSettings {
        self.inner.lock().expect("settings mutex poisoned").clone()
    }

    pub fn recents_path(&self) -> &Path {
        &self.recents_path
    }

    fn persist(&self, settings: &AppSettings) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(settings)?;
        // Settings file contains the SIP secret - keep it user-readable only.
        write_private_file(&self.path, json.as_bytes())
    }

    pub fn update_account(&self, account: AccountSettings) -> std::io::Result<()> {
        let mut guard = self.inner.lock().expect("settings mutex poisoned");
        guard.account = account;
        self.persist(&guard)
    }

    pub fn update_core_binary_path(&self, path: Option<String>) -> std::io::Result<()> {
        let mut guard = self.inner.lock().expect("settings mutex poisoned");
        guard.core_binary_path = path;
        self.persist(&guard)
    }

    pub fn update_theme(&self, theme: ThemePref) -> std::io::Result<()> {
        let mut guard = self.inner.lock().expect("settings mutex poisoned");
        guard.theme = theme;
        self.persist(&guard)
    }

    pub fn set_admin_password_hash(&self, hash: String) -> std::io::Result<()> {
        let mut guard = self.inner.lock().expect("settings mutex poisoned");
        guard.admin.password_hash = Some(hash);
        self.persist(&guard)
    }

    pub fn admin_password_hash(&self) -> Option<String> {
        self.inner
            .lock()
            .expect("settings mutex poisoned")
            .admin
            .password_hash
            .clone()
    }
}

#[cfg(unix)]
fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    use std::io::Write;
    f.write_all(contents)
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    fs::write(path, contents)
}

/// Session-only admin unlock flag. Deliberately NOT persisted - a fresh app
/// launch always starts locked, matching the task's "session unlock flag in
/// memory only" requirement.
#[derive(Default)]
pub struct AdminSession {
    unlocked: std::sync::atomic::AtomicBool,
}

impl AdminSession {
    pub fn is_unlocked(&self) -> bool {
        self.unlocked.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn set_unlocked(&self, v: bool) {
        self.unlocked.store(v, std::sync::atomic::Ordering::SeqCst);
    }
}

pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let parsed = match PasswordHash::new(hash) {
        Ok(p) => p,
        Err(_) => return false,
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

// ---- Recents ------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallDirection {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentCall {
    pub peer: String,
    pub direction: CallDirection,
    /// Epoch milliseconds.
    pub started_at: u64,
    /// Call leg duration in whole seconds (0 for missed/never-established).
    pub duration_secs: u64,
    pub missed: bool,
}

pub fn load_recents(path: &Path) -> Vec<RecentCall> {
    match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

pub fn add_recent(path: &Path, entry: RecentCall) -> std::io::Result<Vec<RecentCall>> {
    let mut list = load_recents(path);
    list.insert(0, entry);
    list.truncate(MAX_RECENTS);
    let json = serde_json::to_string_pretty(&list)?;
    fs::write(path, json)?;
    Ok(list)
}

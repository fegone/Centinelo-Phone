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

/// Free tier: 4 live BLF favorites (see core/PROTOCOL.md `blf_subscribe`).
pub const MAX_FAVORITES: usize = 4;

/// Always exactly `MAX_FAVORITES` slots (padding with empty ones), each
/// trimmed - keeps the favorites grid's shape stable regardless of how many
/// the operator actually filled in, matching `default_favorites()`.
fn normalize_favorites(input: Vec<FavoriteSlot>) -> Vec<FavoriteSlot> {
    let mut out: Vec<FavoriteSlot> = input
        .into_iter()
        .take(MAX_FAVORITES)
        .map(|f| FavoriteSlot {
            ext: f.ext.trim().to_string(),
            label: f.label.trim().to_string(),
        })
        .collect();
    while out.len() < MAX_FAVORITES {
        let n = out.len() + 1;
        out.push(FavoriteSlot {
            ext: String::new(),
            label: format!("Favorite {n}"),
        });
    }
    out
}

/// Click-to-call bridge (localhost HTTP, see bridge.rs) + deep-link settings.
/// `token` guards the bridge - minted once on first run (see
/// `SettingsStore::load`), same shape as v1's `clickToCallToken`
/// (`crypto.randomBytes(16).toString('hex')`, see src/main/main.js) so
/// there's nothing new for a paired Chrome extension to learn.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BridgeSettings {
    #[serde(default)]
    pub token: String,
    /// Skip the "call this number?" confirmation for both the click-to-call
    /// bridge and centinelo:// or tel: deep links (same flow, one flag - see
    /// bridge.rs/deeplink.rs). Off by default: an external dial request
    /// always needs a human's yes first unless explicitly opted into.
    #[serde(default)]
    pub auto_dial: bool,
    /// Opt-in OS-level `tel:` handler registration (`centinelo://` is always
    /// claimed - it's this app's own scheme, no conflict risk). See
    /// deeplink.rs for the macOS/Windows/Linux platform split.
    #[serde(default)]
    pub register_tel_handler: bool,
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

// ---- transcription (F4) --------------------------------------------------
//
// All fields here are admin-gated, same as account/favorites - see
// commands.rs `get_transcription_settings`/`save_transcription_settings`.
// Additionally gated behind the `transcription` premium capability
// (src/transcription.rs `is_unlocked`) - a Community/unlicensed build
// never even reports these settings to the frontend, per the task spec's
// "sin dylib/licencia -> settings de transcripcion ni aparecen".

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionMode {
    #[default]
    Off,
    Live,
    PostCall,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionActivation {
    #[default]
    AllCalls,
    Manual,
}

/// Which whisper.cpp ggml model tier to use - see
/// `transcription::model_filename`/`transcription::model_download_url` for
/// the concrete file this maps to (`transcribe` skill's model research,
/// 2026-07-16: large-v3-turbo-q5_0 default "accurate", small-q5_1 "light").
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    #[default]
    Accurate,
    Light,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptionSettings {
    #[serde(default)]
    pub mode: TranscriptionMode,
    #[serde(default)]
    pub activation: TranscriptionActivation,
    /// Delete the tap WAVs once transcription finishes (default) or keep
    /// them alongside the transcript in `storage_dir` (= the recording
    /// feature, gated separately behind the `recording` capability -
    /// this flag is honored here regardless, since keeping raw audio
    /// around is the operator's explicit choice either way).
    #[serde(default)]
    pub keep_audio: bool,
    /// Local path or NAS (SMB-mounted) path. Empty = not configured yet -
    /// `save_transcription_settings` requires a non-empty value once
    /// `mode != Off`.
    #[serde(default)]
    pub storage_dir: String,
    /// See `transcription.rs`'s `finalize_artifacts` doc for exactly what
    /// this changes (and doesn't) about where files land.
    #[serde(default)]
    pub view_only: bool,
    #[serde(default)]
    pub model_tier: ModelTier,
    #[serde(default = "default_transcription_language")]
    pub language: String,
}

fn default_transcription_language() -> String {
    "es".to_string()
}

impl Default for TranscriptionSettings {
    fn default() -> Self {
        Self {
            mode: TranscriptionMode::default(),
            activation: TranscriptionActivation::default(),
            keep_audio: false,
            storage_dir: String::new(),
            view_only: false,
            model_tier: ModelTier::default(),
            language: default_transcription_language(),
        }
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
    #[serde(default)]
    pub bridge: BridgeSettings,
    #[serde(default)]
    pub transcription: TranscriptionSettings,
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

        let mut settings: AppSettings = if path.exists() {
            let raw = fs::read_to_string(&path)?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            AppSettings::default()
        };
        if settings.favorites.is_empty() {
            settings.favorites = default_favorites();
        }
        let mut dirty = false;
        // First run (or an existing settings.json predating the bridge):
        // mint a token once and persist it immediately, matching v1's own
        // "mint on first read" behavior for `clickToCallToken` (see
        // src/main/main.js `getSettings()`).
        if settings.bridge.token.is_empty() {
            settings.bridge.token = generate_bridge_token();
            dirty = true;
        }

        let store = Self {
            path,
            recents_path,
            inner: Mutex::new(settings),
        };
        if dirty {
            let snapshot = store.snapshot();
            store.persist(&snapshot)?;
        }
        Ok(store)
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

    pub fn update_favorites(&self, favorites: Vec<FavoriteSlot>) -> std::io::Result<()> {
        let mut guard = self.inner.lock().expect("settings mutex poisoned");
        guard.favorites = normalize_favorites(favorites);
        self.persist(&guard)
    }

    pub fn update_bridge_auto_dial(&self, auto_dial: bool) -> std::io::Result<()> {
        let mut guard = self.inner.lock().expect("settings mutex poisoned");
        guard.bridge.auto_dial = auto_dial;
        self.persist(&guard)
    }

    pub fn update_bridge_register_tel(&self, register: bool) -> std::io::Result<()> {
        let mut guard = self.inner.lock().expect("settings mutex poisoned");
        guard.bridge.register_tel_handler = register;
        self.persist(&guard)
    }

    pub fn update_transcription(&self, transcription: TranscriptionSettings) -> std::io::Result<()> {
        let mut guard = self.inner.lock().expect("settings mutex poisoned");
        guard.transcription = transcription;
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

/// 16 random bytes, hex-encoded (32 chars) - same shape as v1's
/// `crypto.randomBytes(16).toString('hex')` (src/main/main.js
/// `getSettings()`), just via `getrandom` (OS CSPRNG) instead of Node's.
fn generate_bridge_token() -> String {
    let mut buf = [0u8; 16];
    // getrandom only fails if the OS entropy source itself is unavailable
    // (never observed on macOS/Windows/Linux in practice) - falling back to
    // an all-zero token would be a *worse* failure mode (a guessable bridge
    // secret) than making the crash visible, so this stays a hard panic
    // rather than a silent degrade.
    getrandom::getrandom(&mut buf).expect("OS RNG unavailable - can't mint a bridge token");
    hex::encode(buf)
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

#[cfg(test)]
mod transcription_settings_tests {
    use super::*;

    #[test]
    fn defaults_are_off_all_calls_accurate_spanish() {
        let t = TranscriptionSettings::default();
        assert_eq!(t.mode, TranscriptionMode::Off);
        assert_eq!(t.activation, TranscriptionActivation::AllCalls);
        assert_eq!(t.model_tier, ModelTier::Accurate);
        assert_eq!(t.language, "es");
        assert!(!t.keep_audio);
        assert!(!t.view_only);
        assert!(t.storage_dir.is_empty());
    }

    #[test]
    fn round_trips_through_json() {
        let t = TranscriptionSettings {
            mode: TranscriptionMode::Live,
            activation: TranscriptionActivation::Manual,
            keep_audio: true,
            storage_dir: "/mnt/nas/transcripts".to_string(),
            view_only: true,
            model_tier: ModelTier::Light,
            language: "auto".to_string(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: TranscriptionSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn deserializing_missing_field_falls_back_to_off() {
        // An older settings.json predating this field (or a hand-edited
        // one missing it) must not fail to load the rest of the file -
        // matches every other #[serde(default)] field in AppSettings.
        let json = r#"{}"#;
        let t: TranscriptionSettings = serde_json::from_str(json).unwrap();
        assert_eq!(t.mode, TranscriptionMode::Off);
    }

    #[test]
    fn app_settings_without_transcription_key_defaults_gracefully() {
        // Simulates loading a pre-F4 settings.json - SettingsStore::load's
        // `unwrap_or_default()` path plus #[serde(default)] on the field
        // must produce a usable AppSettings, not an error.
        let json = r#"{"account":{"host":"pbx.example.test","ext":"1100","secret":"x"}}"#;
        let app: AppSettings = serde_json::from_str(json).unwrap();
        assert_eq!(app.transcription.mode, TranscriptionMode::Off);
        assert_eq!(app.account.host, "pbx.example.test");
    }

    #[test]
    fn mode_and_activation_serialize_as_expected_wire_values() {
        // Locks in the exact snake_case wire shape the frontend will match
        // against (ola-2 panel) - a serde rename change here is a breaking
        // change for that consumer.
        assert_eq!(serde_json::to_string(&TranscriptionMode::PostCall).unwrap(), "\"post_call\"");
        assert_eq!(serde_json::to_string(&TranscriptionMode::Live).unwrap(), "\"live\"");
        assert_eq!(serde_json::to_string(&TranscriptionActivation::AllCalls).unwrap(), "\"all_calls\"");
        assert_eq!(serde_json::to_string(&ModelTier::Accurate).unwrap(), "\"accurate\"");
    }
}

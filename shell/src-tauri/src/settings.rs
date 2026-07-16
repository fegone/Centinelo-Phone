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
    /// Hex SHA-256 leaf-certificate pin (colons optional), applied to the
    /// sidecar as the `CENT_TLS_PIN` env var it already documents
    /// (`core/PROTOCOL.md` "CENT_TLS_PIN is one flat env var" - see
    /// sidecar.rs's `spawn()`). `None` = no pinning beyond whatever the
    /// OS trust store does for the negotiated transport, same as before
    /// this field existed. Set via provisioning (provisioning.rs) today -
    /// there's no manual-entry field for it in Settings yet (advanced/
    /// security value, provisioning is the intended path - see
    /// shell/PROVISIONING.md).
    #[serde(default)]
    pub tls_pin_sha256: Option<String>,
}

impl AccountSettings {
    pub fn is_configured(&self) -> bool {
        !self.host.trim().is_empty() && !self.ext.trim().is_empty() && !self.secret.is_empty()
    }
}

// ---- shared account-field validation (2026-07-16 4R re-review, A1) -------
//
// `sidecar.rs`'s `write_accounts_file` interpolates `host`/`ext`/`secret`
// unquoted, unescaped, straight into a single-line baresip accounts entry
// (`<sip:{ext}@{host}:{port};transport=...>` + `;auth_pass={secret};...`)
// - a `;` inside `secret` would prematurely close `auth_pass=` and let the
// rest of the string inject arbitrary account params; a newline anywhere
// would inject an entirely separate account line.
//
// The first version of this check lived only in `provisioning.rs`
// (provisioning-sourced accounts are more exposed - a URL merely pasted,
// or a link someone else sent). That left the *other* writer of
// `AccountSettings`, `commands::save_account_settings` (manual entry in
// Settings), checking only for empty host/ext - still reachable by the
// same injection, just gated behind admin-unlock instead of being
// impossible. This function is now the single source of truth, called
// from BOTH callers before they persist, **and** defensively again inside
// `write_accounts_file` itself right before it builds the line - so the
// check holds even if some future third caller forgets to call it.
pub const MAX_HOST_LEN: usize = 253; // RFC 1035 full-name limit
pub const MAX_EXT_LEN: usize = 64;
pub const MAX_SECRET_LEN: usize = 256;
pub const MAX_DISPLAY_NAME_LEN: usize = 128;

/// Character/length safety for the four fields that flow into that one
/// accounts-file line. Deliberately does **not** enforce "non-empty" -
/// that's each caller's own business rule: provisioning requires all
/// three (see `provisioning::validate`); manual Settings entry already
/// enforces host/ext non-empty separately (`commands::save_account_settings`)
/// and allows an unspecified secret to mean "keep the existing one".
pub fn validate_account_fields(host: &str, ext: &str, secret: &str, display_name: &str) -> Result<(), String> {
    if host.chars().count() > MAX_HOST_LEN {
        return Err("\"host\" is too long.".to_string());
    }
    if !host.is_empty()
        && !host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
    {
        return Err("\"host\" contains characters that aren't allowed in a hostname or IP address.".to_string());
    }

    if ext.chars().count() > MAX_EXT_LEN {
        return Err("\"ext\" is too long.".to_string());
    }
    if !ext.is_empty()
        && !ext.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '*' | '#' | '+'))
    {
        return Err("\"ext\" contains characters that aren't allowed in an extension.".to_string());
    }

    if secret.chars().count() > MAX_SECRET_LEN {
        return Err("\"secret\" is too long.".to_string());
    }
    if secret.chars().any(|c| c.is_control() || c == ';') {
        return Err(
            "\"secret\" contains characters that aren't allowed (control characters or \";\").".to_string(),
        );
    }

    if display_name.chars().count() > MAX_DISPLAY_NAME_LEN {
        return Err("\"display_name\" is too long.".to_string());
    }
    if display_name.chars().any(|c| c.is_control()) {
        return Err("\"display_name\" contains control characters.".to_string());
    }

    Ok(())
}

#[cfg(test)]
mod validate_account_fields_tests {
    use super::*;

    #[test]
    fn ordinary_values_pass() {
        assert!(validate_account_fields("pbx.example.test", "9999", "s3cret", "Front Desk").is_ok());
    }

    #[test]
    fn empty_values_pass_requiredness_is_the_callers_job() {
        assert!(validate_account_fields("", "", "", "").is_ok());
    }

    #[test]
    fn secret_semicolon_rejected_account_line_injection() {
        let err =
            validate_account_fields("h", "1", "x;outbound=\"sip:evil.example.test\"", "").unwrap_err();
        assert!(err.contains("secret"), "unexpected message: {err}");
    }

    #[test]
    fn secret_newline_rejected_account_line_injection() {
        assert!(validate_account_fields("h", "1", "x\n<sip:evil@evil.example.test>;auth_pass=y", "").is_err());
    }

    #[test]
    fn host_semicolon_rejected() {
        assert!(validate_account_fields("pbx.example.test;evilparam=1", "1", "x", "").is_err());
    }

    #[test]
    fn ext_at_sign_rejected() {
        assert!(validate_account_fields("h", "1001@evil.example.test", "x", "").is_err());
    }

    #[test]
    fn host_too_long_rejected() {
        assert!(validate_account_fields(&"a".repeat(MAX_HOST_LEN + 1), "1", "x", "").is_err());
    }

    #[test]
    fn display_name_control_char_rejected() {
        assert!(validate_account_fields("h", "1", "x", "Front\nDesk").is_err());
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

// ---- HID headsets (F4 ola 2, spec §5) ------------------------------------
//
// Admin-gated like account/favorites/transcription - a call-center agent
// can use whichever headset is plugged in, but can't repoint the app at a
// device via settings (see commands in `crate::hid::commands`).

/// A device's stable identity - VID+PID, optionally narrowed by serial
/// number - not a raw OS device path. Paths can and do change across
/// replug on some platforms (notably macOS's IOHID registry-entry-based
/// paths), so persisting one could silently stop resolving to anything by
/// the next launch; this is what a device actually advertises about
/// itself. `crate::hid::device::DeviceIdentity::matches` (not this file -
/// keeps `hidapi`-shaped matching logic out of settings.rs) is where this
/// gets compared against a live enumeration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidDeviceIdentity {
    pub vendor_id: u16,
    pub product_id: u16,
    #[serde(default)]
    pub serial_number: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HidSettings {
    /// Off by default - matches this file's existing conservative-default
    /// convention for anything that reaches outside this app on its own
    /// (`bridge.auto_dial`, `bridge.register_tel_handler`, both also
    /// off-by-default) - opening a HID device can prompt for OS-level
    /// Input Monitoring permission on macOS the first time it happens, and
    /// an operator with no headset shouldn't see that unprompted.
    #[serde(default)]
    pub enabled: bool,
    /// When on, an unrecognized (never-selected) telephony-page HID device
    /// is used automatically the moment it's plugged in - the "just works"
    /// default once `enabled` is on. When off, only `selected` (if it's
    /// actually present) is ever used - see
    /// `crate::hid::device::select_candidates_to_try`'s doc for the exact
    /// precedence.
    #[serde(default = "default_true")]
    pub auto_detect: bool,
    #[serde(default)]
    pub selected: Option<HidDeviceIdentity>,
}

fn default_true() -> bool {
    true
}

impl Default for HidSettings {
    fn default() -> Self {
        Self { enabled: false, auto_detect: true, selected: None }
    }
}

// ---- audio devices (real-audio-devices fix) ------------------------------
//
// Selects which real input/output device `sidecar.rs`'s `write_config_file`
// wires up for the engine (`coreaudio` on macOS, `wasapi` on Windows - see
// `sidecar::platform_audio_driver`), instead of the `ausine`/`aufile`
// synthetic pair the config generator hardcoded unconditionally before this
// fix. `None` on either field = that platform's driver at its own `default`
// pseudo-device, which is what a fresh install gets with zero settings
// changes - the whole point of this feature (a beta tester who never opens
// Settings still hears/is heard). Admin-gated
// (`commands::save_audio_settings`), same rationale as `HidSettings`: an
// agent shouldn't be able to silently repoint the app at a different mic/
// speaker than the one an admin verified.
//
// A `CENTINELO_E2E_AUDIO=synthetic` env var overrides both fields at spawn
// time regardless of what's persisted here - qa-e2e's driver depends on
// deterministic synthetic audio and must never be silently switched to a
// real device by whatever happens to be persisted in a given test profile's
// `settings.json`. See `sidecar::audio_config_lines`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioSettings {
    /// `"<module>[,<device>]"` - `core/PROTOCOL.md`'s own `devices` event
    /// "name" shape, round-tripped verbatim from that event via
    /// `commands::save_audio_settings`. Never hand-typed by a human.
    #[serde(default)]
    pub input_device: Option<String>,
    #[serde(default)]
    pub output_device: Option<String>,
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

/// The shell's language, mirroring `ThemePref`'s own "Auto" semantic
/// exactly: `Auto` (default) means "follow this computer's language" and
/// is resolved client-side (ui/js/i18n.js `detectSystemLocale`, from
/// `navigator.language`) rather than written back to settings - the same
/// reason `ThemePref::Auto` doesn't get rewritten to `Light`/`Dark` the
/// first time the OS theme is read. An explicit choice (`En`/`PtBr`/`Es`)
/// always wins over the OS language and is what actually gets persisted
/// when someone picks a language in Settings. `PtBr` only (not `PtPt`) -
/// Brazilian Portuguese is the only Portuguese variant this product ships
/// (task brief: "PT-BR real, não português de Portugal").
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum LocalePref {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "en")]
    En,
    #[serde(rename = "pt-BR")]
    PtBr,
    #[serde(rename = "es")]
    Es,
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
    #[serde(default)]
    pub locale: LocalePref,
    /// Explicit override for the core binary path. `None` = auto-resolve
    /// (see sidecar.rs `default_core_binary_path`).
    #[serde(default)]
    pub core_binary_path: Option<String>,
    #[serde(default)]
    pub bridge: BridgeSettings,
    #[serde(default)]
    pub transcription: TranscriptionSettings,
    #[serde(default)]
    pub hid: HidSettings,
    #[serde(default)]
    pub audio: AudioSettings,
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

    /// Rolls the in-memory copy back to `account`'s previous value if
    /// `persist()` fails (disk full, NAS-mounted app-data dir gone, ...) -
    /// every sibling `update_*` method below has the same
    /// mutate-then-persist shape and the same latent memory/disk
    /// divergence on a failed write; `update_account` gets the fix here
    /// because `provisioning_apply` (commands.rs) depends on it directly
    /// (2026-07-16 4R re-review, R1) - a failed provisioning apply should
    /// leave the account exactly as it was, not silently switch the
    /// *in-memory* account to the new (never-persisted, and therefore
    /// never what the next sidecar spawn's scratch `accounts` file - or
    /// the next successful `snapshot()` - actually reflects) one. The
    /// other `update_*` methods sharing this shape are pre-existing and
    /// out of this diff's scope; flagged as a follow-up.
    pub fn update_account(&self, account: AccountSettings) -> std::io::Result<()> {
        let mut guard = self.inner.lock().expect("settings mutex poisoned");
        let previous = guard.account.clone();
        guard.account = account;
        if let Err(e) = self.persist(&guard) {
            guard.account = previous;
            return Err(e);
        }
        Ok(())
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

    pub fn update_locale(&self, locale: LocalePref) -> std::io::Result<()> {
        let mut guard = self.inner.lock().expect("settings mutex poisoned");
        guard.locale = locale;
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

    pub fn update_hid(&self, hid: HidSettings) -> std::io::Result<()> {
        let mut guard = self.inner.lock().expect("settings mutex poisoned");
        guard.hid = hid;
        self.persist(&guard)
    }

    pub fn update_audio(&self, audio: AudioSettings) -> std::io::Result<()> {
        let mut guard = self.inner.lock().expect("settings mutex poisoned");
        guard.audio = audio;
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

/// `path.tmp.<pid>` in the same directory as `path` - same directory so
/// the final `fs::rename` below is a same-filesystem rename (atomic on
/// every OS this app targets), and pid-suffixed so two processes racing
/// to write the same settings file (shouldn't happen in practice - one
/// `SettingsStore` per running app - but cheap to make impossible rather
/// than merely unlikely) can't clobber each other's temp file mid-write.
fn tmp_sibling_path(path: &Path) -> PathBuf {
    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "settings".to_string());
    path.with_file_name(format!("{file_name}.tmp.{}", std::process::id()))
}

/// Write-then-rename instead of truncate-in-place (2026-07-16 4R
/// re-review, R2): the previous `OpenOptions::truncate(true)` + one
/// `write_all` left a real window where a crash or a full disk mid-write
/// truncated `settings.json` to a partial/invalid file - `SettingsStore::load`'s
/// `unwrap_or_default()` on a parse failure would then silently reset
/// *every* setting (account, admin password hash, bridge token,
/// favorites) on the next launch. `fs::rename` within one directory is
/// atomic on macOS/Linux/Windows - the file at `path` is always either
/// the old complete contents or the new complete contents, never a
/// partial write, regardless of when a crash/power-loss happens.
#[cfg(unix)]
fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp_path = tmp_sibling_path(path);
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?;
        use std::io::Write;
        f.write_all(contents)?;
        f.sync_all()?; // durable on disk before the rename makes it visible
    }
    fs::rename(&tmp_path, path)
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let tmp_path = tmp_sibling_path(path);
    fs::write(&tmp_path, contents)?;
    fs::rename(&tmp_path, path)
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
        // "9999" here, never the real test PBX extension (private -
        // see this workspace's CLAUDE.md) - this file is in a public repo
        // (2026-07-16 review, finding B3).
        let json = r#"{"account":{"host":"pbx.example.test","ext":"9999","secret":"x"}}"#;
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

#[cfg(test)]
mod hid_settings_tests {
    use super::*;

    #[test]
    fn defaults_are_disabled_but_auto_detect_ready() {
        // Off by default (see HidSettings's own doc - avoids a surprise
        // macOS Input Monitoring permission prompt for an operator with no
        // headset) but auto_detect stays on, so flipping `enabled` alone is
        // enough to "just work" for the common case.
        let hid = HidSettings::default();
        assert!(!hid.enabled);
        assert!(hid.auto_detect);
        assert!(hid.selected.is_none());
    }

    #[test]
    fn app_settings_without_hid_key_defaults_gracefully() {
        // Pre-this-feature settings.json (or a hand-edited one missing the
        // key) must still load - same #[serde(default)] discipline as
        // every other AppSettings field.
        let json = r#"{"account":{"host":"pbx.example.test","ext":"9999","secret":"x"}}"#;
        let app: AppSettings = serde_json::from_str(json).unwrap();
        assert!(!app.hid.enabled);
        assert!(app.hid.auto_detect);
    }

    #[test]
    fn selected_identity_round_trips_through_json() {
        let hid = HidSettings {
            enabled: true,
            auto_detect: false,
            selected: Some(HidDeviceIdentity { vendor_id: 0x1234, product_id: 0x5678, serial_number: Some("SN1".to_string()) }),
        };
        let json = serde_json::to_string(&hid).unwrap();
        let back: HidSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(hid, back);
    }

    #[test]
    fn selected_without_serial_number_key_defaults_to_none() {
        // An older/hand-edited selected identity missing serial_number must
        // still deserialize.
        let json = r#"{"vendor_id":1,"product_id":2}"#;
        let id: HidDeviceIdentity = serde_json::from_str(json).unwrap();
        assert_eq!(id.serial_number, None);
    }
}

// 2026-07-16 4R re-review (RELIABILITY A2): LocalePref used a hand-written
// #[serde(rename = "...")] per variant (not #[serde(rename_all = ...)] like
// ThemePref) BECAUSE "pt-BR" isn't expressible by any of serde's built-in
// case conventions (not snake/kebab/camel/etc.) - a typo in one of those
// four rename strings would silently break persistence (a saved "pt-BR"
// preference would fail to round-trip, or fall back to Auto without any
// visible error) with zero coverage catching it. These tests pin the exact
// wire representation for all four variants plus the two AppSettings-level
// behaviors (missing key, unknown variant) shared with every other
// #[serde(default)] field in this file.
#[cfg(test)]
mod locale_pref_tests {
    use super::*;

    #[test]
    fn default_is_auto() {
        assert_eq!(LocalePref::default(), LocalePref::Auto);
    }

    #[test]
    fn wire_representation_matches_the_frontends_locale_codes() {
        // ui/js/i18n.js's SUPPORTED_LOCALES + "auto" send/expect exactly
        // these four strings - a mismatch here breaks get_locale/set_locale
        // silently (the command still "succeeds", it just never resolves to
        // the locale the operator picked).
        assert_eq!(serde_json::to_string(&LocalePref::Auto).unwrap(), r#""auto""#);
        assert_eq!(serde_json::to_string(&LocalePref::En).unwrap(), r#""en""#);
        assert_eq!(serde_json::to_string(&LocalePref::PtBr).unwrap(), r#""pt-BR""#);
        assert_eq!(serde_json::to_string(&LocalePref::Es).unwrap(), r#""es""#);
    }

    #[test]
    fn deserializes_from_the_same_four_wire_strings() {
        assert_eq!(serde_json::from_str::<LocalePref>(r#""auto""#).unwrap(), LocalePref::Auto);
        assert_eq!(serde_json::from_str::<LocalePref>(r#""en""#).unwrap(), LocalePref::En);
        assert_eq!(serde_json::from_str::<LocalePref>(r#""pt-BR""#).unwrap(), LocalePref::PtBr);
        assert_eq!(serde_json::from_str::<LocalePref>(r#""es""#).unwrap(), LocalePref::Es);
    }

    #[test]
    fn rejects_pt_pt_and_other_lookalikes_rather_than_silently_aliasing() {
        // This product only ships Brazilian Portuguese (task brief: "PT-BR
        // real, não português de Portugal") - "pt-PT", "pt", or a wrong-case
        // "PT-BR" must fail loudly (serde error), not silently collapse to
        // some default that would mask a frontend/backend drift.
        assert!(serde_json::from_str::<LocalePref>(r#""pt-PT""#).is_err());
        assert!(serde_json::from_str::<LocalePref>(r#""pt""#).is_err());
        assert!(serde_json::from_str::<LocalePref>(r#""PT-BR""#).is_err());
    }

    #[test]
    fn round_trips_through_json_for_every_variant() {
        for locale in [LocalePref::Auto, LocalePref::En, LocalePref::PtBr, LocalePref::Es] {
            let json = serde_json::to_string(&locale).unwrap();
            let back: LocalePref = serde_json::from_str(&json).unwrap();
            assert_eq!(locale, back);
        }
    }

    #[test]
    fn app_settings_without_locale_key_defaults_to_auto() {
        // Same #[serde(default)] discipline as every other AppSettings
        // field (see hid_settings_tests/transcription_settings_tests above)
        // - an existing settings.json predating this sprint must still load
        // and resolve to "follow the OS language" rather than failing.
        let json = r#"{"account":{"host":"pbx.example.test","ext":"9999","secret":"x"}}"#;
        let app: AppSettings = serde_json::from_str(json).unwrap();
        assert_eq!(app.locale, LocalePref::Auto);
    }

    #[test]
    fn app_settings_locale_round_trips_an_explicit_choice() {
        let app = AppSettings { locale: LocalePref::PtBr, ..Default::default() };
        let json = serde_json::to_string(&app).unwrap();
        assert!(json.contains(r#""locale":"pt-BR""#), "expected a literal pt-BR in the wire JSON, got: {json}");
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.locale, LocalePref::PtBr);
    }
}

#[cfg(test)]
mod audio_settings_tests {
    use super::*;

    #[test]
    fn defaults_to_no_explicit_device_either_side() {
        // No settings.json entry yet (fresh install) = the platform's real
        // driver default device, per sidecar::audio_config_lines - nothing
        // persisted here forces that, it's just "no override".
        let audio = AudioSettings::default();
        assert_eq!(audio.input_device, None);
        assert_eq!(audio.output_device, None);
    }

    #[test]
    fn app_settings_without_audio_key_defaults_gracefully() {
        // Pre-this-feature settings.json (or hand-edited, missing the key)
        // must still load - same #[serde(default)] discipline as every
        // other AppSettings field (transcription/hid above).
        let json = r#"{"account":{"host":"pbx.example.test","ext":"9999","secret":"x"}}"#;
        let app: AppSettings = serde_json::from_str(json).unwrap();
        assert_eq!(app.audio.input_device, None);
        assert_eq!(app.audio.output_device, None);
    }

    #[test]
    fn selected_devices_round_trip_through_json() {
        let audio = AudioSettings {
            input_device: Some("coreaudio,MacBook Pro Microphone".to_string()),
            output_device: Some("coreaudio,MacBook Pro Speakers".to_string()),
        };
        let json = serde_json::to_string(&audio).unwrap();
        let back: AudioSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(audio, back);
    }
}

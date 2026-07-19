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

use crate::sync_ext::PoisonRecover;
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const SETTINGS_FILE: &str = "settings.json";
pub const RECENTS_FILE: &str = "recents.json";
pub const MAX_RECENTS: usize = 200;
/// The license activation writes to (see `activation.rs`'s module doc,
/// "The real gap this piece leaves open" - nothing reads this file back
/// yet, but this is the established, documented path a future real
/// consumer should read from). Sibling of `settings.json`, same app-data
/// directory, same "0600, never logged" handling via `write_private_file`.
pub const LICENSE_FILE: &str = "license.json";

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

// ---- audio device name validation (2026-07-16 4R review, S1 VETO) -------
//
// `sidecar.rs`'s `write_config_file` interpolates a device name unescaped
// into baresip's `audio_source`/`audio_player`/`audio_alert` config lines
// (`"<line>\t\t{name}\n"`) - same injection shape `validate_account_fields`
// above already guards against for the SIP account fields: a `\n` embedded
// in `name` would inject an arbitrary extra config line (rewriting
// `sip_verify_server`/`rtp_timeout`, or loading an unauthenticated control
// module like `cons`/`httpd`). The attacker surface here isn't only a human
// typing into a settings field either - a device name round-trips from
// `core/PROTOCOL.md`'s own `devices` event, which in turn comes from
// whatever a USB/Bluetooth peripheral advertises as its own name over
// CoreAudio/WASAPI. A crafted device name (`"Mic\nmodule cons.so"`) would
// show up as a normal, selectable entry in that enumeration.
//
// Called at BOTH the persist site (`commands::save_audio_settings`'s
// `merge_device_choice`) AND again at the sink
// (`sidecar::resolve_device`, right before interpolation) - the same
// defense-in-depth shape `write_accounts_file` already uses for
// `validate_account_fields`, so the check holds even if some future third
// writer of `AudioSettings` forgets to validate before persisting.
pub const MAX_DEVICE_LEN: usize = 256;

pub fn validate_device_name(name: &str) -> Result<(), String> {
    if name.chars().count() > MAX_DEVICE_LEN {
        return Err("Device name is too long.".to_string());
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(
            "Device name contains characters that aren't allowed (control characters, including newlines).".to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod validate_device_name_tests {
    use super::*;

    #[test]
    fn ordinary_device_names_pass() {
        assert!(validate_device_name("coreaudio,MacBook Pro Microphone").is_ok());
        assert!(validate_device_name("wasapi,default").is_ok());
        // Real hardware names seen in this fix's own e2e verification -
        // punctuation/parens/commas from a manufacturer string must not
        // trip this up.
        assert!(validate_device_name("coreaudio,USB PnP Sound Device (2.0)").is_ok());
    }

    #[test]
    fn embedded_newline_rejected_config_line_injection() {
        let err = validate_device_name("coreaudio,Mic\nmodule cons.so").unwrap_err();
        assert!(err.contains("control"), "unexpected message: {err}");
    }

    #[test]
    fn embedded_carriage_return_rejected() {
        assert!(validate_device_name("coreaudio,Mic\rrtp_timeout\t99999").is_err());
    }

    #[test]
    fn embedded_tab_rejected() {
        // A literal tab could still shift baresip's own whitespace-based
        // config parsing even without a full newline - reject any control
        // character, not just \n/\r.
        assert!(validate_device_name("coreaudio,Mic\tsip_verify_server\tno").is_err());
    }

    #[test]
    fn too_long_rejected() {
        assert!(validate_device_name(&"a".repeat(MAX_DEVICE_LEN + 1)).is_err());
    }

    #[test]
    fn empty_string_passes_requiredness_is_the_callers_job() {
        // Mirrors validate_account_fields's own convention - "is this
        // field required at all" is each caller's business rule
        // (sidecar::resolve_device treats an empty/absent device as "use
        // the platform default", not an error).
        assert!(validate_device_name("").is_ok());
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

// ---- remote STT (P6, "STT remoto" plan) -----------------------------------
//
// Extends TranscriptionSettings with the optional remote-speech-to-text
// backend (Centinelo-hosted by default; an OpenAI-compatible HTTP endpoint
// as the escape hatch). All remote fields default to EMPTY/off - this public
// repo ships zero internal hostnames or API keys, the same convention the
// license/provisioning endpoints already follow. The `remote_url` is
// validated through `url_policy::validate_https_or_localhost` (the same
// shared rule the activation server URL uses) at the command layer, never
// persisted unvalidated.

/// Local whisper.cpp (in-process) vs. a remote HTTP backend. Defaults to
/// `Local` so a fresh install keeps the proven v1 behavior unless an admin
/// explicitly opts into the remote path.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SttMode {
    #[default]
    Local,
    Remote,
}

/// Which remote protocol to speak. `Centinelo` = the Centinelo-hosted
/// `/health` + `/transcribe` service; `OpenaiCompat` = any OpenAI-compatible
/// `/v1/audio/transcriptions` endpoint, reached best-effort (no `/health`
/// contract assumed).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RemoteBackend {
    #[default]
    Centinelo,
    OpenaiCompat,
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
    /// Local whisper.cpp vs. remote HTTP backend (P6). Defaults to `Local`.
    #[serde(default)]
    pub stt_mode: SttMode,
    /// Which remote protocol `remote_url` speaks (P6). Defaults to
    /// `Centinelo`.
    #[serde(default)]
    pub remote_backend: RemoteBackend,
    /// Base URL of the remote STT service, e.g.
    /// `https://stt.example.test`. Empty by default - validated through
    /// `url_policy::validate_https_or_localhost` before it's ever persisted.
    #[serde(default)]
    pub remote_url: String,
    /// Bearer/API key for the remote backend. Empty by default; stored the
    /// same way the SIP `secret` is (plaintext in the 0600 settings file,
    /// never logged).
    #[serde(default)]
    pub remote_api_key: String,
    /// Model name passed to the remote backend (e.g. `centinelo-es`,
    /// `whisper-large-v3`). Empty = the backend's own default.
    #[serde(default)]
    pub remote_model: String,
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
            stt_mode: SttMode::default(),
            remote_backend: RemoteBackend::default(),
            remote_url: String::new(),
            remote_api_key: String::new(),
            remote_model: String::new(),
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

// ---- auto-updater (roadmap debt fix) -------------------------------------
//
// The only backend-persisted preference for the updater - everything else
// (checking, downloading, installing, the version/notes/progress it shows)
// is transient, session-only state owned entirely by `ui/js/updater.js` /
// `app.js`, same split theme/locale already use (a stored *preference*
// here, the *resolved*/*live* value lives client-side). Not admin-gated -
// same reasoning `set_theme`/`set_locale` already document (settings.rs's
// own comment on those): the ONE admin-lock enforcement point for
// low-risk Settings controls is visual (`#lock-overlay` covering the
// whole `#settings-body`), and this is exactly that class of control -
// "did the operator want an automatic background network check" is not a
// sensitive account/transport/advanced-path value the front desk
// shouldn't casually flip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdaterSettings {
    /// Whether `app.js`'s `boot()` fires a background `check()` on launch.
    /// On by default - matches this task's own spec ("check_on_startup,
    /// default on"). Turning it off only stops the *automatic* check; the
    /// Settings > About "Check for updates" button always works regardless
    /// (same "the manual path never depends on the automatic one's
    /// setting" shape `settings.hid.enabled` has for HID auto-detect vs.
    /// the plugged-in-device answer/hangup path).
    #[serde(default = "default_true")]
    pub check_on_startup: bool,
}

impl Default for UpdaterSettings {
    fn default() -> Self {
        Self { check_on_startup: true }
    }
}

// ---- BLF admin toggle (P4, "BLF favorites admin toggle" feature) ----------
//
// The single admin-gated switch that turns BLF (the free 4-favorite grid AND
// the premium receptionist console) fully OFF, engine-level - see
// `docs/SPEC-2026-07-17-blf-admin-toggle-design.md` §2 ("real engine-level
// off, not a UI hide"). Default ON: BLF is a flagship differentiator and this
// is opt-OUT, so a missing field on an older settings.json must resolve to
// `true` (the `app_settings_without_blf_key_defaults_to_true` migration test
// locks that in).
//
// Lives in its OWN struct (not folded into `AdminSettings`) to mirror the
// shape of every other feature-area settings struct here
// (`TranscriptionSettings`/`HidSettings`/`UpdaterSettings`/...): one struct
// per concern, composed into `AppSettings`, while `AdminSettings` stays the
// pure admin-auth-credential holder (`password_hash`) it already is. The
// admin-lock is an enforcement point in the command
// (`set_blf_enabled`'s `require_unlocked()`), not a structural property of
// the persisted struct - same split `HidSettings` (admin-gated via
// `save_audio_settings`/`update_hid`) already uses.
//
// The subscribe-loop gating that actually CONSUMES `enabled` (P5) reads it as
// `settings.snapshot().blf.enabled`; this piece only adds the field plus the
// `get_blf_enabled`/`set_blf_enabled` commands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlfSettings {
    /// `false` = this install ships no BLF at all: the shell skips every
    /// `blf_subscribe` call site (P5) and hides the favorites grid + premium
    /// console entry point (P5 UI), and a `true -> false` transition (handled
    /// in `commands::set_blf_enabled`) issues `blf_unsubscribe` for every
    /// currently-tracked extension. `#[serde(default = "default_true")]` so a
    /// pre-this-field settings.json loads as the shipped default (ON) - same
    /// shape `HidSettings::auto_detect`/`UpdaterSettings::check_on_startup`
    /// already use.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for BlfSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

// ---- availability / auto-answer (shell task "disponibilidad + auto-
// answer") ------------------------------------------------------------
//
// Two independent, NOT admin-gated preferences (same "operational, not
// sensitive" bucket as `UpdaterSettings::check_on_startup` above - an
// agent flipping "I'm away from my desk" or "answer for me" is routine
// day-to-day use, not an account/transport/advanced-path change that
// needs an admin's blessing). The actual engine-facing decision
// (`set_answer_mode` "auto"/"manual", plus whether an incoming call gets
// auto-hung-up instead of ringing at all) is a pure function of BOTH
// fields together - see `sidecar::effective_answer_mode`/
// `sidecar::should_auto_reject_incoming` (mirrored in
// `ui/js/call-availability.js`'s `computeCallHandling` for the frontend's
// own rendering) - deliberately not stored here, so there is exactly one
// place that combines them and `available` can never be bypassed by a
// stale `auto_answer` read.
//
// `available` defaults to `true` (a fresh install rings normally, same
// "opt-out not opt-in" shape `BlfSettings::enabled` uses) and
// `auto_answer` defaults to `false` (auto-answering every call is an
// explicit opt-in, not a surprise a first-run user should hit) - hence
// two different `#[serde(default...)]` strategies below, both needed for
// a pre-this-field settings.json to migrate to the shipped defaults
// rather than failing to load.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AvailabilitySettings {
    /// `false` = "do not disturb": every incoming call is auto-rejected
    /// (486 Busy Here via `hangup` before the frontend ever sees it ring -
    /// see `sidecar.rs`'s `call_state:"incoming"` handling) and the PBX
    /// routes it to voicemail. Always wins over `auto_answer` - see this
    /// struct's own doc.
    #[serde(default = "default_true")]
    pub available: bool,
    /// `true` = while `available`, the engine answers every incoming call
    /// itself (`set_answer_mode` "auto") instead of waiting for a manual
    /// `answer`. Ignored entirely while `available` is `false` (no calls
    /// ever reach ringing in that state to auto-answer in the first
    /// place).
    #[serde(default)]
    pub auto_answer: bool,
}

impl Default for AvailabilitySettings {
    fn default() -> Self {
        Self { available: true, auto_answer: false }
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

// ---- license activation (P3 of the activation-server plan) --------------
//
// Only the activation server URL is persisted here - the serial itself is
// NEVER written to settings.json or anywhere else (see activation.rs's
// module doc). Admin-gated in commands::activate_license, same "licencia"
// entry the shell-tauri skill's own rule lists alongside account/
// transport/transcription as sensitive. Default empty: no internal
// hostname ships in this public repo, same rule the STT/provisioning
// endpoints already follow.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LicenseSettings {
    #[serde(default)]
    pub activation_server_url: String,
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
    #[serde(default)]
    pub updater: UpdaterSettings,
    #[serde(default)]
    pub license: LicenseSettings,
    /// BLF master switch (P4) - see `BlfSettings`. Composed last so the
    /// surrounding field list is untouched; `#[serde(default)]` makes the
    /// field's on-disk absence resolve to `BlfSettings::default()` (ON).
    #[serde(default)]
    pub blf: BlfSettings,
    /// Availability + auto-answer - see `AvailabilitySettings`. Composed
    /// last for the same "untouched surrounding list" reason as `blf`
    /// above; `#[serde(default)]` resolves a pre-this-field settings.json
    /// to `AvailabilitySettings::default()` (available, manual answer).
    #[serde(default)]
    pub availability: AvailabilitySettings,
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
        self.inner.lock_or_recover().clone()
    }

    pub fn recents_path(&self) -> &Path {
        &self.recents_path
    }

    /// `license.json`, sibling of `settings.json` - see `LICENSE_FILE`'s
    /// own doc comment and `activation.rs`'s module doc for why nothing
    /// reads this back yet.
    pub fn license_path(&self) -> PathBuf {
        self.path.with_file_name(LICENSE_FILE)
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
        let mut guard = self.inner.lock_or_recover();
        let previous = guard.account.clone();
        guard.account = account;
        if let Err(e) = self.persist(&guard) {
            guard.account = previous;
            return Err(e);
        }
        Ok(())
    }

    pub fn update_core_binary_path(&self, path: Option<String>) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.core_binary_path = path;
        self.persist(&guard)
    }

    pub fn update_theme(&self, theme: ThemePref) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.theme = theme;
        self.persist(&guard)
    }

    pub fn update_locale(&self, locale: LocalePref) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.locale = locale;
        self.persist(&guard)
    }

    pub fn update_favorites(&self, favorites: Vec<FavoriteSlot>) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.favorites = normalize_favorites(favorites);
        self.persist(&guard)
    }

    /// Sets `blf.enabled` and persists - the write half of
    /// `commands::set_blf_enabled` (P5 consumes the field at
    /// `snapshot().blf.enabled`; the true->false unsubscribe round-trip lives
    /// in the command, not here). Same mutate-then-persist shape as every
    /// sibling `update_*` method (`update_favorites` just above, etc.).
    pub fn update_blf_enabled(&self, enabled: bool) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.blf.enabled = enabled;
        self.persist(&guard)
    }

    /// Sets `availability.available` and persists - the write half of
    /// `commands::set_available`. The engine-facing follow-up (reapplying
    /// the effective answer mode, auto-rejecting whatever's already
    /// ringing) is the command's job, not this store's - same split
    /// `update_blf_enabled` uses (this method only owns the persisted
    /// value).
    pub fn update_available(&self, available: bool) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.availability.available = available;
        self.persist(&guard)
    }

    /// Sets `availability.auto_answer` and persists - the write half of
    /// `commands::set_auto_answer`. See `update_available`'s doc for why
    /// the engine-facing reapply lives in the command instead.
    pub fn update_auto_answer(&self, auto_answer: bool) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.availability.auto_answer = auto_answer;
        self.persist(&guard)
    }

    pub fn update_bridge_auto_dial(&self, auto_dial: bool) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.bridge.auto_dial = auto_dial;
        self.persist(&guard)
    }

    pub fn update_bridge_register_tel(&self, register: bool) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.bridge.register_tel_handler = register;
        self.persist(&guard)
    }

    pub fn update_transcription(&self, transcription: TranscriptionSettings) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.transcription = transcription;
        self.persist(&guard)
    }

    pub fn update_hid(&self, hid: HidSettings) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.hid = hid;
        self.persist(&guard)
    }

    pub fn update_audio(&self, audio: AudioSettings) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.audio = audio;
        self.persist(&guard)
    }

    pub fn update_updater_check_on_startup(&self, check_on_startup: bool) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.updater.check_on_startup = check_on_startup;
        self.persist(&guard)
    }

    /// Persists the activation server URL an operator typed - called
    /// whenever `commands::activate_license` receives a URL that passes
    /// `activation::validate_server_url`, regardless of whether that
    /// particular activation attempt then succeeds (a network hiccup
    /// shouldn't force retyping a real endpoint - same "don't lose what
    /// was typed" discipline `provisioning_apply`'s own `peek()`-not-
    /// `take()` comment documents elsewhere in this codebase).
    pub fn update_license_server_url(&self, activation_server_url: String) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.license.activation_server_url = activation_server_url;
        self.persist(&guard)
    }

    pub fn set_admin_password_hash(&self, hash: String) -> std::io::Result<()> {
        let mut guard = self.inner.lock_or_recover();
        guard.admin.password_hash = Some(hash);
        self.persist(&guard)
    }

    pub fn admin_password_hash(&self) -> Option<String> {
        self.inner.lock_or_recover().admin.password_hash.clone()
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
pub(crate) fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp_path = tmp_sibling_path(path);
    let write_result: std::io::Result<()> = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?;
        use std::io::Write;
        f.write_all(contents)?;
        f.sync_all() // durable on disk before the rename makes it visible
    })();
    if let Err(e) = write_result {
        // Best-effort cleanup (2026-07-16 4R re-review, RESILIENCE minor):
        // if `write_all` or `sync_all` itself fails partway - disk full
        // mid-write, a flaky filesystem choking on the fsync, ... - the
        // tmp file would otherwise be left orphaned next to the real one
        // forever (never reached by the rename below, and nothing else
        // ever cleans up a `.tmp.<pid>` sibling). Never touched on the
        // success path, where the rename consumes the tmp file instead.
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    fs::rename(&tmp_path, path)?;
    // Best-effort fsync of the containing directory too (deuda fix,
    // 2026-07-16): the file's own `sync_all()` above only guarantees its
    // *contents* survive a crash - on ext4/APFS/etc the directory-entry
    // change the rename itself made is a separate piece of durable state
    // (some filesystems can lose a rename, leaving either the old file or
    // neither visible, across a crash right after `rename` returns unless
    // that's flushed too). Best-effort, not `?`: this directory is always
    // openable in practice (it's the app-data dir this process just wrote
    // into), but if some future caller ever points `path` somewhere
    // stranger, a failure to open/sync *the directory* shouldn't turn an
    // already-successful, already-renamed file write into an error.
    if let Some(dir) = path.parent() {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Windows sibling of the unix path above - same write-then-rename shape,
/// same `sync_all()` before the rename (deuda fix: the atomic rename alone
/// only protects against a *torn* write, not against the new file's bytes
/// still sitting in the OS page cache, unflushed to disk, when a power cut
/// hits right after `rename` returns but before the OS gets around to
/// flushing - `sync_all()` forces that flush first, so the rename is only
/// ever visible once the new contents are actually durable). No `.mode()`
/// call - Windows has no unix permission bits; this file's contents (the
/// SIP secret) rely on the app-data directory's own OS-level ACLs instead,
/// same as every other file this app writes there. A `File::sync_all()` on
/// the *directory* (to also fsync the rename itself) isn't reachable from
/// safe std on Windows - flushing the file's own contents before the
/// rename is the durability guarantee actually available on this OS.
#[cfg(not(unix))]
pub(crate) fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let tmp_path = tmp_sibling_path(path);
    let write_result: std::io::Result<()> = (|| {
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).open(&tmp_path)?;
        use std::io::Write;
        f.write_all(contents)?;
        f.sync_all()
    })();
    if let Err(e) = write_result {
        // Same orphaned-tmp-file cleanup as the unix branch above - see
        // its comment.
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    fs::rename(&tmp_path, path)
}

#[cfg(test)]
mod write_private_file_tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("centinelo-settings-test.{name}.{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writes_the_exact_contents_and_leaves_no_tmp_sibling_behind() {
        let dir = scratch_dir("roundtrip");
        let path = dir.join("settings.json");
        write_private_file(&path, b"{\"a\":1}").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{\"a\":1}");
        // tmp_sibling_path's own file must not survive a successful write -
        // the rename either consumes it or the write never leaves a stray
        // partial file lying around next to the real settings.json.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn overwrites_existing_contents_atomically() {
        let dir = scratch_dir("overwrite");
        let path = dir.join("settings.json");
        write_private_file(&path, b"old").unwrap();
        write_private_file(&path, b"much longer new contents").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"much longer new contents");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn unix_file_is_mode_0600_not_world_or_group_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("mode");
        let path = dir.join("settings.json");
        write_private_file(&path, b"secret").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "SIP secret file must not be group/world readable");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn removes_the_orphaned_tmp_file_when_the_write_step_fails() {
        // 4R re-review, RESILIENCE minor: if opening/writing/fsyncing the
        // tmp file fails partway, it must not linger forever next to the
        // real settings file. Pre-create the tmp sibling read-only so
        // `write_private_file`'s own `OpenOptions::write(true)...open()`
        // deterministically fails with EACCES - a portable, no-full-disk-
        // needed way to exercise the same "the write step failed" cleanup
        // path a real disk-full/flaky-fsync failure would take.
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("cleanup-on-write-failure");
        let path = dir.join("settings.json");
        let tmp_path = tmp_sibling_path(&path);
        fs::write(&tmp_path, b"stale").unwrap();
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o400)).unwrap();

        let result = write_private_file(&path, b"new contents");

        assert!(result.is_err());
        assert!(!tmp_path.exists(), "the orphaned tmp file must be cleaned up on a failed write, not left behind forever");
        assert!(!path.exists(), "a failed write must never leave the real settings file created either");
        let _ = fs::remove_dir_all(&dir);
    }
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
            stt_mode: SttMode::Remote,
            remote_backend: RemoteBackend::OpenaiCompat,
            remote_url: "https://stt.example.test".to_string(),
            remote_api_key: "sk-test".to_string(),
            remote_model: "whisper-large-v3".to_string(),
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

    // ---- remote STT (P6) ------------------------------------------------

    #[test]
    fn remote_defaults_are_local_centinelo_and_empty() {
        // No internal hostname/key ships in this public repo - same
        // convention the license/provisioning endpoints already follow.
        let t = TranscriptionSettings::default();
        assert_eq!(t.stt_mode, SttMode::Local);
        assert_eq!(t.remote_backend, RemoteBackend::Centinelo);
        assert!(t.remote_url.is_empty());
        assert!(t.remote_api_key.is_empty());
        assert!(t.remote_model.is_empty());
    }

    #[test]
    fn remote_settings_round_trip_through_json() {
        let t = TranscriptionSettings {
            mode: TranscriptionMode::Live,
            activation: TranscriptionActivation::Manual,
            keep_audio: false,
            storage_dir: "/tmp/x".to_string(),
            view_only: false,
            model_tier: ModelTier::Accurate,
            language: "es".to_string(),
            stt_mode: SttMode::Remote,
            remote_backend: RemoteBackend::Centinelo,
            remote_url: "https://stt.example.test".to_string(),
            remote_api_key: "key-123".to_string(),
            remote_model: "centinelo-es".to_string(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: TranscriptionSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn older_settings_json_without_remote_keys_defaults_gracefully() {
        // A pre-P6 settings.json has no stt_mode/remote_* fields - they must
        // resolve to Local/Centinelo/empty, not fail to deserialize.
        let json = r#"{"mode":"off","activation":"all_calls"}"#;
        let t: TranscriptionSettings = serde_json::from_str(json).unwrap();
        assert_eq!(t.stt_mode, SttMode::Local);
        assert_eq!(t.remote_backend, RemoteBackend::Centinelo);
        assert!(t.remote_url.is_empty());
    }

    #[test]
    fn stt_mode_and_backend_serialize_as_lowercase_snake_case() {
        assert_eq!(serde_json::to_string(&SttMode::Local).unwrap(), "\"local\"");
        assert_eq!(serde_json::to_string(&SttMode::Remote).unwrap(), "\"remote\"");
        assert_eq!(serde_json::to_string(&RemoteBackend::Centinelo).unwrap(), "\"centinelo\"");
        assert_eq!(serde_json::to_string(&RemoteBackend::OpenaiCompat).unwrap(), "\"openai_compat\"");
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

#[cfg(test)]
mod updater_settings_tests {
    use super::*;

    #[test]
    fn defaults_to_check_on_startup_enabled() {
        // Task spec: "check_on_startup, default on" - same default-true
        // shape as HidSettings::auto_detect (see that struct's own doc for
        // why an opt-OUT default is right here: a beta tester who never
        // opens Settings should still get notified of a new build).
        assert!(UpdaterSettings::default().check_on_startup);
    }

    #[test]
    fn app_settings_without_updater_key_defaults_gracefully() {
        // Pre-this-feature settings.json (or hand-edited, missing the key)
        // must still load - same #[serde(default)] discipline as every
        // other AppSettings field (transcription/hid/audio above).
        let json = r#"{"account":{"host":"pbx.example.test","ext":"9999","secret":"x"}}"#;
        let app: AppSettings = serde_json::from_str(json).unwrap();
        assert!(app.updater.check_on_startup);
    }

    #[test]
    fn an_explicit_false_round_trips_through_json() {
        let updater = UpdaterSettings { check_on_startup: false };
        let json = serde_json::to_string(&updater).unwrap();
        let back: UpdaterSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(updater, back);
        assert!(!back.check_on_startup);
    }
}

#[cfg(test)]
mod blf_settings_tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("centinelo-blf-settings-test.{name}.{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_to_enabled() {
        // Default ON - BLF is a flagship differentiator, opt-OUT not opt-in
        // (SPEC §2 "Default ON"). P4 test #1 (struct/default level): both the
        // struct default and AppSettings' composed default must read true.
        assert!(BlfSettings::default().enabled);
        assert!(AppSettings::default().blf.enabled);
    }

    #[test]
    fn app_settings_without_blf_key_defaults_to_true() {
        // P4 test #5 (migration): a pre-this-field settings.json (or a
        // hand-edited one missing the key) must NOT fail to load, and the
        // absent `blf` field must resolve to the shipped default (ON) - same
        // #[serde(default)] discipline every other AppSettings field
        // (transcription/hid/audio/updater above) already follows. "9999" is
        // a never-real test extension (this is a public repo).
        let json = r#"{"account":{"host":"pbx.example.test","ext":"9999","secret":"x"}}"#;
        let app: AppSettings = serde_json::from_str(json).unwrap();
        assert!(app.blf.enabled);
    }

    #[test]
    fn explicit_false_round_trips_through_json() {
        // Round-trips the struct by itself (P4 test #4, struct half) - an
        // explicit `false` must survive a serialize/deserialize cycle, not
        // silently snap back to the default `true`.
        let blf = BlfSettings { enabled: false };
        let json = serde_json::to_string(&blf).unwrap();
        let back: BlfSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(blf, back);
        assert!(!back.enabled);
    }

    #[test]
    fn fresh_settings_store_starts_blf_enabled_true() {
        // P4 test #1 (SettingsStore level): a fresh store with no persisted
        // file must expose blf_enabled == true end-to-end (what
        // `commands::get_blf_enabled` returns on a brand-new install).
        let dir = scratch_dir("fresh");
        let store = SettingsStore::load(&dir).unwrap();
        assert!(store.snapshot().blf.enabled);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_blf_enabled_persists_and_survives_reload() {
        // P4 test #4 (disk round-trip): write false through
        // `update_blf_enabled`, then load a BRAND-NEW `SettingsStore` from the
        // same dir - the value must have hit disk (not just the in-memory
        // copy), matching how `update_favorites`/`update_theme` already
        // behave. A reload that lost it would mean the command silently lied
        // to the user about a disabled feature.
        let dir = scratch_dir("roundtrip");
        let store = SettingsStore::load(&dir).unwrap();
        assert!(store.snapshot().blf.enabled);
        store.update_blf_enabled(false).unwrap();
        assert!(!store.snapshot().blf.enabled);

        let reloaded = SettingsStore::load(&dir).unwrap();
        assert!(
            !reloaded.snapshot().blf.enabled,
            "blf_enabled=false must survive a reload"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod availability_settings_tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("centinelo-availability-settings-test.{name}.{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_are_available_and_manual() {
        // available defaults ON (rings normally), auto_answer defaults OFF
        // (opt-in) - see AvailabilitySettings's own doc for why these two
        // fields use different #[serde(default...)] strategies.
        let d = AvailabilitySettings::default();
        assert!(d.available);
        assert!(!d.auto_answer);
        assert!(AppSettings::default().availability.available);
        assert!(!AppSettings::default().availability.auto_answer);
    }

    #[test]
    fn app_settings_without_availability_key_migrates_to_defaults() {
        // A pre-this-field settings.json (or a hand-edited one missing the
        // key) must NOT fail to load, and must resolve to the shipped
        // defaults - same migration discipline every other AppSettings
        // field follows. "9999" is a never-real test extension (public
        // repo).
        let json = r#"{"account":{"host":"pbx.example.test","ext":"9999","secret":"x"}}"#;
        let app: AppSettings = serde_json::from_str(json).unwrap();
        assert!(app.availability.available);
        assert!(!app.availability.auto_answer);
    }

    #[test]
    fn explicit_values_round_trip_through_json() {
        let a = AvailabilitySettings { available: false, auto_answer: true };
        let json = serde_json::to_string(&a).unwrap();
        let back: AvailabilitySettings = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
        assert!(!back.available);
        assert!(back.auto_answer);
    }

    #[test]
    fn fresh_settings_store_starts_available_true_auto_answer_false() {
        let dir = scratch_dir("fresh");
        let store = SettingsStore::load(&dir).unwrap();
        assert!(store.snapshot().availability.available);
        assert!(!store.snapshot().availability.auto_answer);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_available_persists_and_survives_reload() {
        let dir = scratch_dir("available-roundtrip");
        let store = SettingsStore::load(&dir).unwrap();
        store.update_available(false).unwrap();
        assert!(!store.snapshot().availability.available);

        let reloaded = SettingsStore::load(&dir).unwrap();
        assert!(
            !reloaded.snapshot().availability.available,
            "available=false must survive a reload"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_auto_answer_persists_and_survives_reload() {
        let dir = scratch_dir("auto-answer-roundtrip");
        let store = SettingsStore::load(&dir).unwrap();
        store.update_auto_answer(true).unwrap();
        assert!(store.snapshot().availability.auto_answer);

        let reloaded = SettingsStore::load(&dir).unwrap();
        assert!(
            reloaded.snapshot().availability.auto_answer,
            "auto_answer=true must survive a reload"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_available_and_auto_answer_are_independent() {
        // Flipping one field must not disturb the other - both live in the
        // same struct but are set through separate commands (set_available/
        // set_auto_answer), each touching only its own field.
        let dir = scratch_dir("independent");
        let store = SettingsStore::load(&dir).unwrap();
        store.update_auto_answer(true).unwrap();
        store.update_available(false).unwrap();
        let snap = store.snapshot().availability;
        assert!(!snap.available);
        assert!(snap.auto_answer, "auto_answer must be untouched by update_available");
        let _ = fs::remove_dir_all(&dir);
    }
}

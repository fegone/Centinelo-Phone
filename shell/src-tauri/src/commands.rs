//! Tauri commands exposed to the frontend. Kept thin: validation + admin-lock
//! enforcement here, actual work delegated to `settings` / `sidecar`.

use crate::premium::{CapabilityStatusView, PremiumHandle, PremiumInfoView};
use crate::sidecar::SidecarHandle;
use crate::settings::{
    self, AccountSettings, AdminSession, CallDirection, FavoriteSlot, LocalePref, ModelTier,
    RecentCall, RemoteBackend, SettingsStore, SttMode, ThemePref, TranscriptionActivation, TranscriptionMode,
    TranscriptionSettings, TransportPriority,
};
use crate::transcription::{PendingRetryView, TranscriptionHandle};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;

fn require_unlocked(admin: &AdminSession) -> Result<(), String> {
    if admin.is_unlocked() {
        Ok(())
    } else {
        Err("Settings are locked. Unlock with the admin password first.".to_string())
    }
}

// ---- sidecar control ------------------------------------------------------

#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_dial(sidecar: State<SidecarHandle>, uri: String) -> Result<(), String> {
    let uri = uri.trim();
    if uri.is_empty() {
        return Err("Enter a number or extension first.".to_string());
    }
    sidecar.send_cmd(serde_json::json!({ "cmd": "dial", "uri": uri }))
}

#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_answer(sidecar: State<SidecarHandle>) -> Result<(), String> {
    sidecar.send_cmd(serde_json::json!({ "cmd": "answer" }))
}

/// `call_id` targets a specific call (e.g. a consultation leg during an
/// attended transfer, from the console); omitted/`None` falls back to
/// "the current call" (`core/PROTOCOL.md`'s own default) - unchanged
/// behavior for the main window's own hangup/decline buttons, which never
/// pass one.
#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_hangup(sidecar: State<SidecarHandle>, call_id: Option<String>) -> Result<(), String> {
    sidecar.send_cmd(with_call_id(serde_json::json!({ "cmd": "hangup" }), call_id))
}

/// Manual "retry now" (also used right after saving new account settings).
#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_restart(sidecar: State<SidecarHandle>) {
    sidecar.restart_now();
}

/// Last known sidecar status, for the frontend's initial paint.
#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_status(sidecar: State<SidecarHandle>) -> crate::sidecar::StatusPayload {
    sidecar.status()
}

// ---- account settings ------------------------------------------------------

#[derive(Serialize)]
pub struct AccountSettingsView {
    pub host: String,
    pub ext: String,
    pub display_name: String,
    pub transport_priority: TransportPriority,
    pub secret_set: bool,
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_account_settings(settings: State<Arc<SettingsStore>>) -> AccountSettingsView {
    let account = settings.snapshot().account;
    AccountSettingsView {
        secret_set: !account.secret.is_empty(),
        host: account.host,
        ext: account.ext,
        display_name: account.display_name,
        transport_priority: account.transport_priority,
    }
}

#[derive(Deserialize)]
pub struct SaveAccountInput {
    pub host: String,
    pub ext: String,
    /// Empty/omitted = keep the currently stored secret unchanged.
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub display_name: String,
    pub transport_priority: TransportPriority,
}

#[tauri::command(rename_all = "snake_case")]
pub fn save_account_settings(
    settings: State<Arc<SettingsStore>>,
    admin: State<AdminSession>,
    sidecar: State<SidecarHandle>,
    input: SaveAccountInput,
) -> Result<(), String> {
    require_unlocked(&admin)?;
    if input.host.trim().is_empty() || input.ext.trim().is_empty() {
        return Err("Host and extension are required.".to_string());
    }
    let host = input.host.trim().to_string();
    let ext = input.ext.trim().to_string();
    let display_name = input.display_name.trim().to_string();
    // Pulls the *whole* previous account, not just the secret (was
    // `previous_secret` only, pre-provisioning) - `..previous` below
    // preserves `tls_pin_sha256` across a manual Settings save instead of
    // silently dropping it, since there's no manual-entry field for that
    // one yet (see settings.rs AccountSettings doc) - a provisioned pin
    // shouldn't vanish the next time someone edits, say, the display name
    // in Settings.
    let previous = settings.snapshot().account;
    let secret = match input.secret {
        Some(s) if !s.is_empty() => s,
        _ => previous.secret.clone(),
    };
    let tls_pin_sha256 = resolved_tls_pin(&host, &previous.host, previous.tls_pin_sha256.clone());
    crate::settings::validate_account_fields(&host, &ext, &secret, &display_name)?;
    let account = AccountSettings {
        host,
        ext,
        secret,
        display_name,
        transport_priority: input.transport_priority,
        tls_pin_sha256,
    };
    settings.update_account(account).map_err(|e| e.to_string())?;
    sidecar.restart_now();
    Ok(())
}

/// A TLS pin is a fingerprint of ONE host's certificate - carrying it over
/// when the host itself changes would silently apply PBX A's pin as
/// `CENT_TLS_PIN` against PBX B, failing that connection for a reason
/// invisible in this UI (no field here shows/clears the pin - see
/// settings.rs `AccountSettings` doc). Extracted as a pure function
/// (2026-07-16 4R re-review, M2) so this rule is unit-testable without a
/// full Tauri `State`/`AppHandle` - see `resolved_tls_pin_tests` below,
/// same pattern `reveal_path_is_allowed` already established in this file
/// for the same reason.
fn resolved_tls_pin(new_host: &str, previous_host: &str, previous_pin: Option<String>) -> Option<String> {
    if new_host == previous_host {
        previous_pin
    } else {
        None
    }
}

#[cfg(test)]
mod resolved_tls_pin_tests {
    use super::*;

    #[test]
    fn same_host_keeps_the_pin() {
        assert_eq!(
            resolved_tls_pin("pbx.example.test", "pbx.example.test", Some("AA".repeat(32))),
            Some("AA".repeat(32))
        );
    }

    #[test]
    fn changed_host_clears_the_pin() {
        assert_eq!(resolved_tls_pin("pbx-b.example.test", "pbx-a.example.test", Some("AA".repeat(32))), None);
    }

    #[test]
    fn no_previous_pin_stays_none_regardless_of_host_change() {
        assert_eq!(resolved_tls_pin("pbx-b.example.test", "pbx-a.example.test", None), None);
        assert_eq!(resolved_tls_pin("pbx.example.test", "pbx.example.test", None), None);
    }

    #[test]
    fn first_time_setting_a_host_from_empty_clears_any_stale_pin() {
        // previous_host == "" only happens on a fresh/never-configured
        // account - any pin present there would be leftover/impossible
        // state, not a real "same host" case.
        assert_eq!(resolved_tls_pin("pbx.example.test", "", Some("AA".repeat(32))), None);
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_core_binary_path(settings: State<Arc<SettingsStore>>) -> Option<String> {
    settings.snapshot().core_binary_path
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_core_binary_path(
    settings: State<Arc<SettingsStore>>,
    admin: State<AdminSession>,
    sidecar: State<SidecarHandle>,
    path: Option<String>,
) -> Result<(), String> {
    require_unlocked(&admin)?;
    let cleaned = path.and_then(|p| {
        let t = p.trim().to_string();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    });
    settings
        .update_core_binary_path(cleaned)
        .map_err(|e| e.to_string())?;
    sidecar.restart_now();
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_favorites(settings: State<Arc<SettingsStore>>) -> Vec<FavoriteSlot> {
    settings.snapshot().favorites
}

/// Last-known BLF state per watched extension (see sidecar.rs
/// `Shared::blf_states`) - fetched at `boot()` so a devtools reload
/// mid-session repaints the favorites grid immediately instead of waiting
/// for the next NOTIFY (BLF re-notifies on change, not on a timer).
#[tauri::command(rename_all = "snake_case")]
pub fn get_blf_states(sidecar: State<SidecarHandle>) -> std::collections::HashMap<String, String> {
    sidecar.blf_states()
}

/// Admin-gated, like account/transport/core-path - favorites in a real
/// clinic dial real people's extensions (see shell task spec). Restarts the
/// sidecar so the new list takes effect immediately: a fresh process
/// re-registers and re-issues `blf_subscribe` for the saved extensions
/// (see sidecar.rs's stdout reader) rather than needing separate
/// subscribe/unsubscribe diffing against the previous list.
#[tauri::command(rename_all = "snake_case")]
pub fn save_favorites(
    settings: State<Arc<SettingsStore>>,
    admin: State<AdminSession>,
    sidecar: State<SidecarHandle>,
    favorites: Vec<FavoriteSlot>,
) -> Result<Vec<FavoriteSlot>, String> {
    require_unlocked(&admin)?;
    settings.update_favorites(favorites).map_err(|e| e.to_string())?;
    sidecar.restart_now();
    Ok(settings.snapshot().favorites)
}

// ---- BLF master switch (P4, "BLF favorites admin toggle") ---------------
//
// A single admin-gated persisted bool (settings::BlfSettings) that turns BLF
// (the free 4-favorite grid AND the premium receptionist console) fully off,
// engine-level. `get_blf_enabled` is a free read (the UI needs the value to
// decide whether to render the favorites grid / console at all, same unlocked-
// read reasoning `get_theme`/`get_locale` already use); `set_blf_enabled` is
// admin-gated like `save_favorites`. The subscribe-loop call-site gating that
// actually enforces "no BLF at all" while off, plus the UI hide, are P5 - this
// command only owns the persisted switch and the teardown half of a live
// `true -> false` transition. See `settings::BlfSettings` and
// `docs/SPEC-2026-07-17-blf-admin-toggle-design.md` §3/§4.

/// Free-readable (no admin-lock) BLF master switch. Not sensitive on its own
/// (a read-only boolean), and the frontend can't decide whether to even show
/// the lock-protected toggle without first knowing the value - same shape as
/// `get_theme`/`get_locale`. P5 reads this to gate the favorites grid +
/// console UI.
#[tauri::command(rename_all = "snake_case")]
pub fn get_blf_enabled(settings: State<Arc<SettingsStore>>) -> bool {
    settings.snapshot().blf.enabled
}

/// Admin-gated writer for the BLF master switch. On a `true -> false`
/// transition it tears down every currently-tracked BLF subscription BEFORE
/// persisting, so the frontend can never observe "the setting says off but
/// live SUBSCRIBE (RFC 4235) traffic is still running" - SPEC §2's
/// "gone, not hidden". The tracked-extensions source is `get_blf_states`'s
/// keys (`sidecar.blf_states()`, the only already-exposed BLF-tracking
/// surface); `SidecarHandle::blf_unsubscribe` is idempotent (see its doc), so
/// an ext present in `blf_states` that isn't currently subscribed is a
/// harmless no-op. Unsubscribe failures are warned-and-continued (best-effort
/// teardown), then the value is persisted regardless so a half-flaky teardown
/// never leaves the user stuck with BLF "on" after they asked for "off".
#[tauri::command(rename_all = "snake_case")]
pub fn set_blf_enabled(
    settings: State<Arc<SettingsStore>>,
    admin: State<AdminSession>,
    sidecar: State<SidecarHandle>,
    enabled: bool,
) -> Result<(), String> {
    require_unlocked(&admin)?;
    if !enabled {
        for ext in sidecar.blf_states().keys() {
            if let Err(e) = sidecar.blf_unsubscribe(ext) {
                log::warn!("set_blf_enabled: blf_unsubscribe({ext}) failed: {e}");
            }
        }
    }
    settings.update_blf_enabled(enabled).map_err(|e| e.to_string())
}

// ---- audio devices (real-audio-devices fix) --------------------------
//
// Free-tier readable (mic/speaker choice isn't sensitive the way the SIP
// account or a call-center's favorites roster is), but only admin-unlocked
// callers can change it - same rationale as HidSettings just below and
// favorites above: an agent shouldn't be able to silently repoint the app
// at a different device than the one an admin verified works against the
// test PBX. See settings.rs `AudioSettings`'s doc for the persisted shape
// and sidecar.rs `audio_config_lines` for exactly how these values (or
// their absence) become the engine's `audio_source`/`audio_player`/
// `audio_alert` config lines.

#[tauri::command(rename_all = "snake_case")]
pub fn get_audio_settings(settings: State<Arc<SettingsStore>>) -> settings::AudioSettings {
    settings.snapshot().audio
}

/// Fires `core/PROTOCOL.md`'s `devices` command - fire-and-forget, like
/// every other `sidecar_*` control command in this file (`sidecar_hold`,
/// `sidecar_mute`, ...): the sidecar has no request/response correlation
/// wired up here (v1.2's `id`/`result` exists in the protocol but this app
/// doesn't use it yet - see that file's "Commands" table), so the actual
/// `{"event":"devices","input":[...],"output":[...]}` payload arrives on
/// the same `sidecar-event` stream every other engine event already rides
/// (sidecar.rs `EVENT_LINE`) - a caller (frontend, or `e2e.rs`'s
/// `list_devices` step) listens for `event === "devices"` there, same
/// pattern already used for `blf`/`call_state`/... .
#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_list_devices(sidecar: State<SidecarHandle>) -> Result<(), String> {
    sidecar.send_cmd(serde_json::json!({ "cmd": "devices" }))
}

/// `None`/omitted on a field = this save doesn't touch that field, keeping
/// whatever's already persisted (2026-07-16 4R review, A3 - see
/// `merge_device_choice`'s doc for why this matters: the old shape blindly
/// replaced the *whole* `AudioSettings` struct, so an input-only save
/// silently wiped a previously-saved output device). `Some("")` (or
/// all-whitespace) on a field = an explicit clear back to the platform
/// default. `Some(name)` = an explicit new device, validated via
/// `settings::validate_device_name` (S1) before being accepted at all.
#[derive(Deserialize)]
pub struct SaveAudioInput {
    #[serde(default)]
    pub input_device: Option<String>,
    #[serde(default)]
    pub output_device: Option<String>,
}

/// Resolves one field's (`input_device`/`output_device`) next persisted
/// value from what this particular save actually touched (2026-07-16 4R
/// review, A3 - see `SaveAudioInput`'s doc for the three-way semantics
/// this implements: not-touched / explicit-clear / explicit-set). Also the
/// S1 VETO's persist-side validation: an explicit set that fails
/// `settings::validate_device_name` (e.g. an embedded newline - baresip
/// config-line injection, see that function's doc) is rejected outright
/// here, before it ever reaches `settings.update_audio` - the whole save
/// fails with a clear message rather than silently dropping just the bad
/// field.
fn merge_device_choice(previous: Option<String>, input: Option<String>) -> Result<Option<String>, String> {
    match input {
        None => Ok(previous),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => {
            let trimmed = s.trim().to_string();
            settings::validate_device_name(&trimmed)?;
            Ok(Some(trimmed))
        }
    }
}

/// Pure decision half of `apply_live_device` (2026-07-16 4R review) -
/// whether a live `set_device` needs to be sent for this save, and with
/// what device name - factored out so it's unit-testable without a real
/// `SidecarHandle`/`AppHandle` (this crate has no mock for either - see
/// `live_device_command_tests`). `None` = nothing to send: either this
/// save didn't actually change `kind` (`previous == updated` - a save
/// that only touched the *other* direction must not re-send a redundant
/// `set_device` for this one), or it's an explicit clear (`Some -> None`)
/// on a platform with no real driver to revert to (`platform_default` is
/// `None` - nothing sensible to send live either way).
///
/// The explicit-clear-needs-a-live-command-too case (A5, 2026-07-16 4R
/// review) is the one this replaces a naive "only send when `updated` is
/// `Some`" would have missed: clearing a previously-selected device back
/// to "default" must still push `<driver>,default` live via
/// `sidecar::platform_default_device_string`, or an active call keeps
/// using the old device even though settings now say "default".
fn live_device_command(
    previous: &Option<String>,
    updated: &Option<String>,
    platform_default: Option<String>,
) -> Option<String> {
    if previous == updated {
        return None;
    }
    match updated {
        Some(n) => Some(n.clone()),
        None => platform_default,
    }
}

/// Best-effort live hot-swap of one direction (`"input"`/`"output"`) via
/// `core/PROTOCOL.md`'s `set_device` - which the protocol documents as
/// hot-swapping a call in progress with no re-INVITE (`ctrl_json.c`'s
/// `cmd_set_device()`). See `live_device_command` for exactly when a
/// command is sent at all. A `send_cmd` failure (e.g. sidecar not running
/// right now - no account configured yet) is surfaced via
/// `SidecarHandle::emit_notice` (a real, user-visible signal - see that
/// method's doc, not just a log line) rather than swallowed silently; it
/// still doesn't fail the *save* itself, since the persisted choice takes
/// effect on the next spawn/call either way, matching
/// `save_favorites`/`save_account_settings`'s own "persist first,
/// best-effort live-apply second" shape.
fn apply_live_device(sidecar: &SidecarHandle, kind: &str, previous: &Option<String>, updated: &Option<String>) {
    let Some(name) = live_device_command(previous, updated, crate::sidecar::platform_default_device_string()) else {
        return;
    };
    if let Err(e) = sidecar.send_cmd(serde_json::json!({"cmd": "set_device", "kind": kind, "name": name})) {
        sidecar.emit_notice(&format!(
            "couldn't apply the {kind} device change to the running call ({e}) - it will take effect on the next call or restart"
        ));
    }
}

/// Persists the operator's device choice, merged onto whatever was already
/// saved (A3 - see `merge_device_choice`), and best-effort applies each
/// *changed* direction live (A5 - see `apply_live_device`).
#[tauri::command(rename_all = "snake_case")]
pub fn save_audio_settings(
    settings: State<Arc<SettingsStore>>,
    admin: State<AdminSession>,
    sidecar: State<SidecarHandle>,
    input: SaveAudioInput,
) -> Result<(), String> {
    require_unlocked(&admin)?;
    let previous = settings.snapshot().audio;
    let updated = settings::AudioSettings {
        input_device: merge_device_choice(previous.input_device.clone(), input.input_device)?,
        output_device: merge_device_choice(previous.output_device.clone(), input.output_device)?,
    };
    settings.update_audio(updated.clone()).map_err(|e| e.to_string())?;
    apply_live_device(&sidecar, "input", &previous.input_device, &updated.input_device);
    apply_live_device(&sidecar, "output", &previous.output_device, &updated.output_device);
    Ok(())
}

#[cfg(test)]
mod audio_settings_command_tests {
    use super::*;

    #[test]
    fn untouched_field_keeps_the_previous_value() {
        assert_eq!(
            merge_device_choice(Some("coreaudio,Old Mic".to_string()), None).unwrap(),
            Some("coreaudio,Old Mic".to_string())
        );
    }

    #[test]
    fn explicit_empty_string_clears_to_platform_default() {
        assert_eq!(merge_device_choice(Some("coreaudio,Old Mic".to_string()), Some("".to_string())).unwrap(), None);
        assert_eq!(merge_device_choice(Some("coreaudio,Old Mic".to_string()), Some("   ".to_string())).unwrap(), None);
    }

    #[test]
    fn explicit_value_replaces_the_previous_one() {
        assert_eq!(
            merge_device_choice(Some("coreaudio,Old Mic".to_string()), Some("coreaudio,New Mic".to_string())).unwrap(),
            Some("coreaudio,New Mic".to_string())
        );
    }

    #[test]
    fn injected_newline_is_rejected_not_silently_dropped() {
        // S1 VETO (2026-07-16 4R review) - persist-side half of the dual
        // defense; sidecar::resolve_device is the sink-side half.
        let err = merge_device_choice(None, Some("coreaudio,Mic\nmodule cons.so".to_string())).unwrap_err();
        assert!(err.contains("control"), "unexpected message: {err}");
    }

    // ---- A5 (2026-07-16 4R review): live_device_command ------------------

    #[test]
    fn unchanged_field_sends_nothing_live() {
        let same = Some("coreaudio,Mic".to_string());
        assert_eq!(live_device_command(&same, &same, Some("coreaudio,default".to_string())), None);
        assert_eq!(live_device_command(&None, &None, Some("coreaudio,default".to_string())), None);
    }

    #[test]
    fn explicit_new_device_sends_that_device_live() {
        assert_eq!(
            live_device_command(&None, &Some("coreaudio,New Mic".to_string()), Some("coreaudio,default".to_string())),
            Some("coreaudio,New Mic".to_string())
        );
    }

    #[test]
    fn clearing_a_device_sends_the_platform_default_live_not_nothing() {
        // The exact bug A5 fixes: without this, clearing a device back to
        // "default" would leave an in-progress call on the old device
        // even though settings now say "default".
        let previous = Some("coreaudio,Old Mic".to_string());
        assert_eq!(
            live_device_command(&previous, &None, Some("coreaudio,default".to_string())),
            Some("coreaudio,default".to_string())
        );
    }

    #[test]
    fn clearing_with_no_platform_driver_sends_nothing_theres_nothing_sensible_to_revert_to() {
        let previous = Some("ausine,440".to_string());
        assert_eq!(live_device_command(&previous, &None, None), None);
    }
}

// ---- theme ------------------------------------------------------------

#[tauri::command(rename_all = "snake_case")]
pub fn get_theme(settings: State<Arc<SettingsStore>>) -> ThemePref {
    settings.snapshot().theme
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_theme(settings: State<Arc<SettingsStore>>, theme: ThemePref) -> Result<(), String> {
    settings.update_theme(theme).map_err(|e| e.to_string())
}

// ---- language (i18n, F4 packaging sprint) ---------------------------------
// Same "Auto" semantic as theme (see settings.rs LocalePref doc) - resolved
// client-side (ui/js/i18n.js detectSystemLocale), not gated behind
// require_unlocked() here for the same reason set_theme isn't: the ONE
// admin-lock enforcement point for both is visual, index.html's
// #lock-overlay covering the whole #settings-body (task brief: "setting
// bajo admin lock" - reaching either control at all already requires an
// admin unlock, see index.html's #locale-row comment).

#[tauri::command(rename_all = "snake_case")]
pub fn get_locale(settings: State<Arc<SettingsStore>>) -> LocalePref {
    settings.snapshot().locale
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_locale(settings: State<Arc<SettingsStore>>, locale: LocalePref) -> Result<(), String> {
    settings.update_locale(locale).map_err(|e| e.to_string())
}

// ---- auto-updater (roadmap debt fix) --------------------------------------
// Only the persisted preference lives here - checking/downloading/
// installing itself goes straight from ui/js/updater.js to
// @tauri-apps/plugin-updater's own auto-registered commands (`updater:
// default` in capabilities/default.json), never through this file. Not
// admin-gated, same reasoning as get_theme/set_theme/get_locale/set_locale
// immediately above (see settings.rs UpdaterSettings's own doc).

#[derive(Serialize)]
pub struct UpdaterSettingsView {
    pub check_on_startup: bool,
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_updater_settings(settings: State<Arc<SettingsStore>>) -> UpdaterSettingsView {
    UpdaterSettingsView { check_on_startup: settings.snapshot().updater.check_on_startup }
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_updater_check_on_startup(settings: State<Arc<SettingsStore>>, check_on_startup: bool) -> Result<(), String> {
    settings.update_updater_check_on_startup(check_on_startup).map_err(|e| e.to_string())
}

// ---- admin lock ------------------------------------------------------

#[derive(Serialize)]
pub struct AdminStatus {
    pub configured: bool,
    pub unlocked: bool,
}

#[tauri::command(rename_all = "snake_case")]
pub fn admin_status(settings: State<Arc<SettingsStore>>, admin: State<AdminSession>) -> AdminStatus {
    AdminStatus {
        configured: settings.admin_password_hash().is_some(),
        unlocked: admin.is_unlocked(),
    }
}

/// Sets the admin password. Allowed when no password is configured yet
/// (first-run setup), or when the session is already unlocked (change
/// password). Either way, succeeds by leaving the session unlocked.
#[tauri::command(rename_all = "snake_case")]
pub fn admin_set_password(
    settings: State<Arc<SettingsStore>>,
    admin: State<AdminSession>,
    new_password: String,
) -> Result<(), String> {
    if new_password.len() < 8 {
        return Err("Use at least 8 characters.".to_string());
    }
    let already_configured = settings.admin_password_hash().is_some();
    if already_configured && !admin.is_unlocked() {
        return Err("Unlock with the current admin password first.".to_string());
    }
    let hash = settings::hash_password(&new_password)?;
    settings
        .set_admin_password_hash(hash)
        .map_err(|e| e.to_string())?;
    admin.set_unlocked(true);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn admin_unlock(settings: State<Arc<SettingsStore>>, admin: State<AdminSession>, password: String) -> bool {
    match settings.admin_password_hash() {
        Some(hash) if settings::verify_password(&password, &hash) => {
            admin.set_unlocked(true);
            true
        }
        _ => false,
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn admin_lock(admin: State<AdminSession>) {
    admin.set_unlocked(false);
}

// ---- recents ------------------------------------------------------

#[tauri::command(rename_all = "snake_case")]
pub fn get_recents(settings: State<Arc<SettingsStore>>) -> Vec<RecentCall> {
    settings::load_recents(settings.recents_path())
}

#[derive(Deserialize)]
pub struct AddRecentInput {
    pub peer: String,
    pub direction: CallDirection,
    pub started_at: u64,
    pub duration_secs: u64,
    pub missed: bool,
}

#[tauri::command(rename_all = "snake_case")]
pub fn add_recent(
    settings: State<Arc<SettingsStore>>,
    input: AddRecentInput,
) -> Result<Vec<RecentCall>, String> {
    let entry = RecentCall {
        peer: input.peer,
        direction: input.direction,
        started_at: input.started_at,
        duration_secs: input.duration_secs,
        missed: input.missed,
    };
    settings::add_recent(settings.recents_path(), entry).map_err(|e| e.to_string())
}

// ---- click-to-call bridge + deep links ------------------------------------
// Not admin-gated (unlike account/favorites): these are behavioral toggles,
// not credentials - the bridge's actual security boundary is the token
// itself (settings.bridge.token, never round-tripped except here, where the
// operator explicitly needs to read/copy it to pair the browser extension -
// same as v1's settings.js, which put `clickToCallToken` straight into a
// visible field).

#[derive(Serialize)]
pub struct BridgeSettingsView {
    pub token: String,
    pub port: u16,
    pub auto_dial: bool,
    pub register_tel_handler: bool,
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_bridge_settings(settings: State<Arc<SettingsStore>>) -> BridgeSettingsView {
    let bridge = settings.snapshot().bridge;
    BridgeSettingsView {
        token: bridge.token,
        port: crate::bridge::BRIDGE_PORT,
        auto_dial: bridge.auto_dial,
        register_tel_handler: bridge.register_tel_handler,
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_auto_dial(settings: State<Arc<SettingsStore>>, auto_dial: bool) -> Result<(), String> {
    settings.update_bridge_auto_dial(auto_dial).map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_register_tel_handler(
    app: tauri::AppHandle,
    settings: State<Arc<SettingsStore>>,
    enabled: bool,
) -> Result<(), String> {
    settings
        .update_bridge_register_tel(enabled)
        .map_err(|e| e.to_string())?;
    crate::deeplink::apply_tel_registration(&app, enabled);
    Ok(())
}

// ---- auto-provisioning (spec §5) ---------------------------------------
//
// Two-step resolve/apply so a fetched config's secret never round-trips
// to the frontend (see provisioning.rs's module doc): `provisioning_resolve`
// parses + (if remote) fetches + validates, stashes the result server-side
// in `ProvisioningPending`, and hands back a secret-free preview for the
// confirmation screen; `provisioning_apply` commits whatever's currently
// pending. The deep-link entry path (provisioning.rs `handle_deep_link`,
// wired from deeplink.rs) fills the same `ProvisioningPending` slot and
// emits the same preview shape as an event instead of a command return
// value - see ui/js/app.js's `showProvisioningConfirm`, shared by both.

#[tauri::command(rename_all = "snake_case")]
pub fn provisioning_resolve(
    provisioning: State<crate::provisioning::ProvisioningPending>,
    input: String,
) -> Result<crate::provisioning::ProvisioningPreviewView, String> {
    let config = crate::provisioning::resolve_input(&input)?;
    let preview = crate::provisioning::ProvisioningPreviewView::from(&config);
    provisioning.set(config);
    Ok(preview)
}

/// Non-consuming read of whatever's currently pending, if anything - used
/// by the frontend once at `boot()` to catch a preview that was already
/// resolved (and its `provisioning://preview` event already fired) before
/// `attachTauriListeners()` had a chance to register a listener for it
/// (2026-07-16 4R re-review, R3). This is the scenario a cold-start
/// `centinelo://provision?config=...` deep link hits every time: the
/// `config=` embedded form resolves synchronously (no network wait to
/// cover the gap), inside `.setup()`, well before the webview has even
/// loaded `index.html`, let alone run `boot()` - Tauri's `emit` doesn't
/// queue/replay for listeners that attach after the fact, so without this
/// command that preview (and the confirmation screen it should have
/// shown) is simply lost, silently, from the operator's point of view
/// ("I clicked the link and nothing happened"). See `ui/js/app.js`
/// `boot()`: listeners attach first, then this is checked once - between
/// the two, no window remains where a preview could go unseen.
#[tauri::command(rename_all = "snake_case")]
pub fn provisioning_pending_preview(
    provisioning: State<crate::provisioning::ProvisioningPending>,
) -> Option<crate::provisioning::ProvisioningPreviewView> {
    provisioning.peek().as_ref().map(crate::provisioning::ProvisioningPreviewView::from)
}

/// Admin-gated *unless* this is the very first provisioning on a clean
/// install (no account configured yet) - matches the task spec's explicit
/// carve-out ("salvo el primer provisioning en instalación limpia, que es
/// el caso de setup inicial"): a brand-new install has no admin password
/// set yet either in the common case (see settings.rs `AdminSettings`,
/// `password_hash: None` until the operator sets one), so requiring
/// unlock here would strand a fresh install with the very
/// `admin_set_password` UI this account is needed to even reach. Any
/// later re-provision of an already-configured install goes through the
/// same `require_unlocked` check `save_account_settings` already applies
/// to a manual account edit - provisioning isn't a lesser-privileged way
/// to change the account than typing it in by hand.
#[tauri::command(rename_all = "snake_case")]
pub fn provisioning_apply(
    settings: State<Arc<SettingsStore>>,
    admin: State<AdminSession>,
    sidecar: State<SidecarHandle>,
    provisioning: State<crate::provisioning::ProvisioningPending>,
) -> Result<(), String> {
    let already_configured = settings.snapshot().account.is_configured();
    if already_configured {
        require_unlocked(&admin)?;
    }
    // A provisioning request can arrive via a centinelo://provision deep
    // link at any moment - unlike opening Settings by hand, it's not
    // necessarily "I'm between calls and ready to reconfigure". Refuse
    // rather than silently dropping whatever call is in progress when
    // sidecar.restart_now() below runs (2026-07-16 4R re-review, R4) -
    // the frontend's existing #prov-confirm-error slot surfaces this
    // message directly, same as any other error from this command.
    if sidecar.has_active_call() {
        return Err("You're on a call — finish or hang up before connecting to a new phone system.".to_string());
    }
    // peek(), not take() (2026-07-16 4R re-review, R1): if update_account
    // below fails (disk full, NAS-mounted app-data dir gone), the pending
    // config must still be there for a retry - consuming it up front and
    // only then discovering the persist failed left "Connect" looking
    // like it had silently forgotten the link (a bewildering "Nothing
    // pending" on retry) instead of surfacing the real, and often
    // transient, disk-write error. Only cleared below once update_account
    // has actually succeeded.
    let config = provisioning
        .peek()
        .ok_or_else(|| "Nothing pending - paste a provisioning link first.".to_string())?;
    settings.update_account(config.into()).map_err(|e| e.to_string())?;
    provisioning.clear();
    sidecar.restart_now();
    Ok(())
}

/// Backs the confirmation screen's "Cancel" - discards a pending config
/// without applying it. Also safe to call defensively any time (e.g. the
/// frontend closing the confirmation screen for any reason); a
/// second/stale pending config left over from a dismissed confirmation
/// should never be applicable by some other, unrelated path later.
#[tauri::command(rename_all = "snake_case")]
pub fn provisioning_cancel(provisioning: State<crate::provisioning::ProvisioningPending>) {
    provisioning.clear();
}

// ---- license activation (P3 of the activation-server plan) --------------
//
// All logic lives in `activation.rs` - this command is thin plumbing
// (admin-lock + Result mapping), matching this file's own module doc
// ("validation + admin-lock enforcement here, actual work delegated").
// See `activation.rs`'s module doc for why the serial is never persisted,
// why errors cross this boundary as short codes (not the ES prose the
// P3 task brief specifies - that lives in `ui/js/i18n.js` instead), and
// "The real gap this piece leaves open" (activating writes a verified
// `license.json`, but nothing reads it back yet - a licensing/premium
// follow-up, out of this crate's scope).

#[derive(Serialize)]
pub struct LicenseSettingsView {
    pub activation_server_url: String,
    /// Whether `license.json` currently exists on disk - lets the UI show
    /// "already activated on this machine" without needing to re-parse or
    /// re-verify it (that parsing only ever happens inside
    /// `activation::activate_and_persist`, right before a fresh write).
    pub license_present: bool,
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_license_settings(settings: State<Arc<SettingsStore>>) -> LicenseSettingsView {
    let snapshot = settings.snapshot();
    LicenseSettingsView {
        activation_server_url: snapshot.license.activation_server_url,
        license_present: settings.license_path().is_file(),
    }
}

#[derive(Serialize)]
pub struct ActivationOutcomeView {
    pub customer: String,
    pub features: Vec<String>,
    pub seats: u32,
    pub expiry: Option<String>,
}

impl From<crate::activation::ActivationResult> for ActivationOutcomeView {
    fn from(r: crate::activation::ActivationResult) -> Self {
        Self { customer: r.customer, features: r.features, seats: r.seats, expiry: r.expiry }
    }
}

/// Admin-gated - "licencia" is explicitly one of the sensitive settings
/// classes this shell locks behind the admin password (shell-tauri
/// skill's own rule, alongside account/transport/transcription). Returns
/// `Err(String)` where the string is a short error CODE (see
/// `activation::ActivationError::code`), not displayed prose - the
/// frontend maps it through `t("activation.error." + code)`.
#[tauri::command(rename_all = "snake_case")]
pub fn activate_license(
    settings: State<Arc<SettingsStore>>,
    admin: State<AdminSession>,
    serial: String,
    server_url: String,
) -> Result<ActivationOutcomeView, String> {
    require_unlocked(&admin)?;
    let validated_url = crate::activation::validate_server_url(&server_url).map_err(|e| e.code().to_string())?;
    // Persisted as soon as the URL is well-formed, regardless of whether
    // this particular activation attempt then succeeds - see
    // `SettingsStore::update_license_server_url`'s own doc. Best-effort:
    // a failure to persist the URL preference is not a reason to abandon
    // an activation attempt that might otherwise succeed.
    let _ = settings.update_license_server_url(validated_url.clone());
    crate::activation::activate_and_persist(&settings.license_path(), &validated_url, &serial)
        .map(ActivationOutcomeView::from)
        .map_err(|e| e.code().to_string())
}

// ---- premium ---------------------------------------------------------
//
// No admin-lock check here (contrast the settings/account commands above)
// - these are read-only status queries, not mutations, and the premium
// module itself is the only thing that ever decides "licensed" (see
// premium.rs's doc, "Where the license check actually happens"). Nothing
// here can turn a feature on; it can only report what's already true.

#[tauri::command(rename_all = "snake_case")]
pub fn premium_info(premium: State<PremiumHandle>) -> Option<PremiumInfoView> {
    premium.info()
}

#[tauri::command(rename_all = "snake_case")]
pub fn premium_capability_status(
    premium: State<PremiumHandle>,
    capability: String,
) -> CapabilityStatusView {
    premium.capability_status(&capability)
}

/// Short, non-user-facing reason string - see `PremiumHandle::diagnostic`.
#[tauri::command(rename_all = "snake_case")]
pub fn premium_diagnostic(premium: State<PremiumHandle>) -> String {
    premium.diagnostic().to_string()
}

/// Opens (or focuses, if already open) the premium receptionist console
/// window - see `console.rs::open_or_focus` for the license-gate
/// re-check this delegates to. The tray menu entry and the main window's
/// own button that call this are both already gated on
/// `premium_capability_status`/`console::is_unlocked` before they're ever
/// shown (`tray.rs`, `ui/js/app.js`) - this command re-checks anyway,
/// since it's reachable by any webview that can invoke it, hidden button
/// or not.
#[tauri::command(rename_all = "snake_case")]
pub fn open_console(app: tauri::AppHandle) -> Result<(), String> {
    crate::console::open_or_focus(&app)
}

// ---- EngineBridge verbs (console) -------------------------------------
//
// One thin command per core/PROTOCOL.md verb, matching this file's
// existing dial/answer/hangup convention rather than a single generic
// passthrough - see premium/console-ui/README.md "EngineBridge contract",
// "Option B". Not admin-gated: these are the same call-control primitives
// dial/answer/hangup above already expose ungated - the premium *gate* is
// on whether the console WINDOW is offered at all (console.rs), not on
// these underlying sidecar verbs, which core/PROTOCOL.md documents as
// ordinary v1.1 call control, nothing console-exclusive about them.

/// Folds an optional `call_id` onto a command object - every call-scoped
/// `core/PROTOCOL.md` command accepts it, falling back to "the current
/// call" when omitted.
fn with_call_id(mut cmd: serde_json::Value, call_id: Option<String>) -> serde_json::Value {
    if let Some(id) = call_id {
        if let serde_json::Value::Object(map) = &mut cmd {
            map.insert("call_id".to_string(), serde_json::Value::String(id));
        }
    }
    cmd
}

#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_hold(sidecar: State<SidecarHandle>, call_id: Option<String>) -> Result<(), String> {
    sidecar.send_cmd(with_call_id(serde_json::json!({ "cmd": "hold" }), call_id))
}

#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_resume(sidecar: State<SidecarHandle>, call_id: Option<String>) -> Result<(), String> {
    sidecar.send_cmd(with_call_id(serde_json::json!({ "cmd": "resume" }), call_id))
}

#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_mute(
    sidecar: State<SidecarHandle>,
    on: bool,
    call_id: Option<String>,
) -> Result<(), String> {
    sidecar.send_cmd(with_call_id(serde_json::json!({ "cmd": "mute", "on": on }), call_id))
}

#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_blind_transfer(
    sidecar: State<SidecarHandle>,
    uri: String,
    call_id: Option<String>,
) -> Result<(), String> {
    sidecar.send_cmd(with_call_id(
        serde_json::json!({ "cmd": "blind_transfer", "uri": uri }),
        call_id,
    ))
}

#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_attended_transfer(
    sidecar: State<SidecarHandle>,
    uri: String,
    call_id: Option<String>,
) -> Result<(), String> {
    sidecar.send_cmd(with_call_id(
        serde_json::json!({ "cmd": "attended_transfer", "uri": uri }),
        call_id,
    ))
}

#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_complete_transfer(
    sidecar: State<SidecarHandle>,
    call_id: Option<String>,
) -> Result<(), String> {
    sidecar.send_cmd(with_call_id(
        serde_json::json!({ "cmd": "complete_transfer" }),
        call_id,
    ))
}

#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_abort_transfer(sidecar: State<SidecarHandle>) -> Result<(), String> {
    sidecar.send_cmd(serde_json::json!({ "cmd": "abort_transfer" }))
}

/// Idempotent: a second `blf_subscribe` for an extension already being
/// watched is a no-op success rather than the `error` event
/// `core/PROTOCOL.md` documents for a raw duplicate subscribe - see
/// `SidecarHandle::blf_subscribe`'s own doc for why that's necessary now
/// that two independent callers (the favorites auto-subscribe on
/// registration, and the console mounting with a roster that can overlap
/// favorites) both legitimately want "make sure this extension is
/// watched".
#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_blf_subscribe(sidecar: State<SidecarHandle>, ext: String) -> Result<(), String> {
    // INFO, not DEBUG: this line is part of this repo's e2e evidence
    // trail (mirrors sidecar.rs's own "Evidence trail" doc on its
    // per-event log line) - the console-ui package's ConsoleStore.start()
    // calls this once per roster extension via EngineBridge on mount, so
    // seeing it in a captured log is direct, Rust-log-visible proof the
    // console's *own vendored JS* reached this command over real IPC -
    // distinct from sidecar.rs's favorites auto-subscribe, which never
    // goes through this `#[tauri::command]` at all (it calls
    // `blf_subscribe_raw` directly from the stdout-reader thread).
    log::info!("commands: sidecar_blf_subscribe({ext}) invoked over IPC");
    sidecar.blf_subscribe(&ext)
}

#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_blf_unsubscribe(sidecar: State<SidecarHandle>, ext: String) -> Result<(), String> {
    log::info!("commands: sidecar_blf_unsubscribe({ext}) invoked over IPC");
    sidecar.blf_unsubscribe(&ext)
}

// ---- transcription (F4) -----------------------------------------------
//
// Settings are admin-gated (require_unlocked) like account/favorites, AND
// gated behind the `transcription` premium capability
// (crate::transcription::is_unlocked) - per the task spec, a
// Community/unlicensed build never even sees these settings exist
// (get_transcription_settings returns None), matching how console.rs's
// window itself never appears rather than appearing disabled.

#[derive(Serialize)]
pub struct TranscriptionSettingsView {
    pub mode: TranscriptionMode,
    pub activation: TranscriptionActivation,
    pub keep_audio: bool,
    pub storage_dir: String,
    pub view_only: bool,
    pub model_tier: ModelTier,
    pub language: String,
    pub stt_mode: SttMode,
    pub remote_backend: RemoteBackend,
    pub remote_url: String,
    pub remote_model: String,
    // NOTE: `remote_api_key` is intentionally NOT echoed back to the
    // frontend here - it's a secret. The frontend's "is a key set?" affordance
    // reads a separate boolean (see `get_transcription_settings` below).
}

impl From<TranscriptionSettings> for TranscriptionSettingsView {
    fn from(t: TranscriptionSettings) -> Self {
        Self {
            mode: t.mode,
            activation: t.activation,
            keep_audio: t.keep_audio,
            storage_dir: t.storage_dir,
            view_only: t.view_only,
            model_tier: t.model_tier,
            language: t.language,
            stt_mode: t.stt_mode,
            remote_backend: t.remote_backend,
            remote_url: t.remote_url,
            remote_model: t.remote_model,
        }
    }
}

/// `None` when the `transcription` capability isn't licensed - the
/// frontend's own contract for "this settings section doesn't exist",
/// same shape `premium_capability_status` already gives every other
/// premium surface.
#[tauri::command(rename_all = "snake_case")]
pub fn get_transcription_settings(
    settings: State<Arc<SettingsStore>>,
    premium: State<PremiumHandle>,
) -> Option<TranscriptionSettingsView> {
    if !crate::transcription::is_unlocked(&premium) {
        return None;
    }
    Some(settings.snapshot().transcription.into())
}

#[derive(Deserialize)]
pub struct SaveTranscriptionInput {
    pub mode: TranscriptionMode,
    pub activation: TranscriptionActivation,
    pub keep_audio: bool,
    pub storage_dir: String,
    pub view_only: bool,
    pub model_tier: ModelTier,
    pub language: String,
    pub stt_mode: SttMode,
    pub remote_backend: RemoteBackend,
    pub remote_url: String,
    pub remote_api_key: String,
    pub remote_model: String,
}

#[tauri::command(rename_all = "snake_case")]
pub fn save_transcription_settings(
    settings: State<Arc<SettingsStore>>,
    admin: State<AdminSession>,
    premium: State<PremiumHandle>,
    input: SaveTranscriptionInput,
) -> Result<(), String> {
    require_unlocked(&admin)?;
    if !crate::transcription::is_unlocked(&premium) {
        return Err("Transcription is not licensed on this installation.".to_string());
    }
    let storage_dir = input.storage_dir.trim().to_string();
    if input.mode != TranscriptionMode::Off && storage_dir.is_empty() {
        return Err("Set a storage folder (local or NAS path) before turning transcription on.".to_string());
    }
    if input.language.trim().is_empty() {
        return Err("Language is required (e.g. \"es\" or \"auto\").".to_string());
    }
    // Remote URL is only required when the operator actually picked the remote
    // path - empty is the default for both modes. But a non-empty value is
    // validated through the shared url_policy rule BEFORE it's persisted, so
    // this shell never stores (let alone calls) a plaintext-HTTP remote host.
    let remote_url = input.remote_url.trim().to_string();
    if !remote_url.is_empty() {
        if let Err(e) = crate::url_policy::validate_https_or_localhost(&remote_url) {
            return Err(format!("Remote STT URL is not valid: {e}"));
        }
    }
    let updated = TranscriptionSettings {
        mode: input.mode,
        activation: input.activation,
        keep_audio: input.keep_audio,
        storage_dir,
        view_only: input.view_only,
        model_tier: input.model_tier,
        language: input.language.trim().to_string(),
        stt_mode: input.stt_mode,
        remote_backend: input.remote_backend,
        remote_url,
        remote_api_key: input.remote_api_key.trim().to_string(),
        remote_model: input.remote_model.trim().to_string(),
    };
    settings.update_transcription(updated).map_err(|e| e.to_string())
}

// ---- remote STT connection probe (P6) ----------------------------------
//
// "Probar conexión" button: a typed, no-audio GET that just checks the
// remote backend is reachable and speaks the protocol the operator picked.
// For `Centinelo` we hit `{url}/health` (the Centinelo STT service's own
// readiness endpoint). For `OpenaiCompat` there is no guaranteed `/health`
// contract, so we try `{url}/v1/models` best-effort - a 401/403 still means
// "the endpoint is there, the key is wrong", not "can't reach it". The URL
// is re-validated through url_policy (defense in depth - it was validated
// on save, but the operator can change the field and click this button
// before saving).

#[derive(Serialize)]
pub struct RemoteSttProbeResult {
    pub ok: bool,
    /// Short human message for the UI, locale-translated client-side from the
    /// stable `code` below when it matters.
    pub code: String,
    pub detail: String,
}

#[derive(Deserialize)]
pub struct TestRemoteSttInput {
    pub remote_url: String,
    pub remote_backend: RemoteBackend,
    /// Optional key: included as a Bearer header so a protected endpoint's
    /// probe isn't a false negative.
    pub remote_api_key: Option<String>,
}

/// Core probe logic, free of Tauri `State` so it's unit-testable directly.
/// The command below is a thin admin-gate wrapper around this.
fn probe_remote_stt(
    raw_url: &str,
    backend: RemoteBackend,
    api_key: Option<&str>,
) -> RemoteSttProbeResult {
    let base = match crate::url_policy::validate_https_or_localhost(raw_url.trim()) {
        Ok(u) => u,
        Err(e) => {
            return RemoteSttProbeResult {
                ok: false,
                code: "bad_url".to_string(),
                detail: e,
            };
        }
    };
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(8))
        .build();
    let probe_path = match backend {
        RemoteBackend::Centinelo => "/health",
        RemoteBackend::OpenaiCompat => "/v1/models",
    };
    let mut req = agent.get(&format!("{base}{probe_path}"));
    if let Some(key) = api_key {
        let key = key.trim();
        if !key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
    }
    match req.call() {
        Ok(_) => RemoteSttProbeResult {
            ok: true,
            code: "ok".to_string(),
            detail: "Connection successful.".to_string(),
        },
        // ureq 2.x: a 4xx/5xx is `Error::Status`, everything else (DNS,
        // connection refused, TLS, timeout) is `Error::Transport`.
        Err(ureq::Error::Status(code, _)) => {
            // 401/403 on the OpenAI-compat probe still means "the endpoint
            // answered" - a partial success worth distinguishing from a hard
            // network failure so the UI can hint at the key.
            let reachable =
                matches!(code, 401 | 403) && backend == RemoteBackend::OpenaiCompat;
            RemoteSttProbeResult {
                ok: reachable,
                code: if reachable { "auth_required".to_string() } else { "http_error".to_string() },
                detail: format!("HTTP {code}"),
            }
        }
        Err(ureq::Error::Transport(e)) => RemoteSttProbeResult {
            ok: false,
            code: "network".to_string(),
            detail: e.to_string(),
        },
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn test_remote_stt_connection(
    admin: State<AdminSession>,
    input: TestRemoteSttInput,
) -> RemoteSttProbeResult {
    // Admin-gated like every other settings mutation/probe.
    if !admin.is_unlocked() {
        return RemoteSttProbeResult {
            ok: false,
            code: "locked".to_string(),
            detail: "Unlock Settings first.".to_string(),
        };
    }
    probe_remote_stt(
        &input.remote_url,
        input.remote_backend,
        input.remote_api_key.as_deref(),
    )
}

/// Manual per-call start (`transcription.activation == "manual"`) - the
/// ola-2 panel's per-call button. Not admin-gated (a runtime call-control
/// action, not a settings mutation - same reasoning as `sidecar_hold`/
/// `sidecar_mute` above); the license + `mode != off` checks happen
/// inside `TranscriptionHandle::manual_start`.
#[tauri::command(rename_all = "snake_case")]
pub fn transcription_manual_start(
    transcription: State<TranscriptionHandle>,
    call_id: String,
    peer: String,
) -> Result<(), String> {
    transcription.manual_start(&call_id, &peer)
}

#[tauri::command(rename_all = "snake_case")]
pub fn transcription_manual_stop(
    transcription: State<TranscriptionHandle>,
    call_id: String,
) -> Result<(), String> {
    transcription.manual_stop(&call_id)
}

/// Calls whose transcript/audio couldn't be written to `storage_dir`
/// (NAS down, disk full, engine binary missing) - see
/// `transcription::Inner::pending_retries`'s doc. The ola-2 panel is
/// expected to surface these with a "retry" button; this command just
/// exposes the data today.
#[tauri::command(rename_all = "snake_case")]
pub fn transcription_pending_retries(transcription: State<TranscriptionHandle>) -> Vec<PendingRetryView> {
    transcription.pending_retries()
}

#[tauri::command(rename_all = "snake_case")]
pub fn transcription_retry(transcription: State<TranscriptionHandle>, call_id: String) -> Result<(), String> {
    transcription.retry(&call_id)
}

// ---- transcription model management (F4 item 5) -----------------------

#[derive(Serialize)]
pub struct ModelStatusView {
    pub tier: ModelTier,
    pub present: bool,
    pub path: String,
    pub size_bytes: Option<u64>,
}

#[tauri::command(rename_all = "snake_case")]
pub fn transcription_model_status(app: tauri::AppHandle, tier: ModelTier) -> ModelStatusView {
    let path = crate::transcription::model_path(&app, tier);
    let meta = std::fs::metadata(&path).ok();
    ModelStatusView {
        tier,
        present: meta.is_some(),
        path: path.display().to_string(),
        size_bytes: meta.map(|m| m.len()),
    }
}

/// Kicks off a background download with progress (`transcription://
/// model-download-progress`) and completion
/// (`transcription://model-download-done`/`-error`) events - see
/// `transcription::spawn_model_download`. Gated behind the license (same
/// as the settings that would select this tier) so a Community build
/// can't be used to pull hundreds of MB of model file it can never use.
#[tauri::command(rename_all = "snake_case")]
pub fn download_transcription_model(
    app: tauri::AppHandle,
    premium: State<PremiumHandle>,
    tier: ModelTier,
) -> Result<(), String> {
    if !crate::transcription::is_unlocked(&premium) {
        return Err("Transcription is not licensed on this installation.".to_string());
    }
    crate::transcription::spawn_model_download(app, tier);
    Ok(())
}

/// Opens the OS file manager with `path` selected (macOS Finder/Windows
/// Explorer) - backs the transcript panel's "Show in folder"/"Show local
/// copy" actions (`premium/design/mockups/transcript-panel.html`).
///
/// # Why this validates `path` before ever spawning anything
///
/// `path` comes straight from the frontend, which itself only ever got it
/// from a `transcription://done` event payload or a
/// `transcription_pending_retries` entry - both backend-sourced, not
/// user-typed - but a Tauri command is still reachable by any script
/// running in the webview (e.g. via devtools), same threat model
/// `console.rs`'s `asset_protocol_handler` documents for its own
/// path-traversal guard. Revealing an arbitrary OS path in the file
/// manager doesn't execute anything, but it's still a real information
/// disclosure ("does this file exist, what's its icon/preview") this
/// shell shouldn't hand to arbitrary webview content unchecked. Bounded
/// to two known-safe roots: the operator's configured transcription
/// `storage_dir` (where a finished transcript actually lives) and the OS
/// temp directory's `centinelo-transcribe-tap.*` prefix (where a
/// not-yet-moved one sits during a pending retry - see
/// `transcription.rs`'s `start_tap`) - the only two places this feature
/// ever writes a transcript.
#[tauri::command(rename_all = "snake_case")]
pub fn reveal_in_file_manager(settings: State<Arc<SettingsStore>>, path: String) -> Result<(), String> {
    let candidate = std::path::PathBuf::from(&path);
    let canonical = candidate
        .canonicalize()
        .map_err(|_| "That file is no longer on disk.".to_string())?;

    let storage_dir = settings.snapshot().transcription.storage_dir;
    if !reveal_path_is_allowed(&canonical, &storage_dir) {
        log::warn!("reveal_in_file_manager: refusing path outside known transcription roots: {path:?}");
        return Err("That location isn't part of a transcript this app saved.".to_string());
    }

    reveal_path(&canonical)
}

/// Pure check: does `canonical` (an already-`canonicalize()`d path - the
/// caller resolved it, which is also what neutralizes a symlink planted
/// inside `storage_dir` pointing outside it, since `canonicalize()`
/// follows the link to its real target before this function ever sees
/// it) fall under the configured `storage_dir` or a
/// `centinelo-transcribe-tap.*` OS-temp-dir prefix - the only two places
/// this feature ever writes a transcript (`transcription.rs`'s
/// `start_tap`/`finalize_artifacts`). Extracted from
/// `reveal_in_file_manager` so it's unit-testable without a Tauri
/// `State`/`AppHandle` (2026-07-16 4R re-review, T2 - this validation had
/// no test coverage at all before, despite being this command's entire
/// security boundary).
fn reveal_path_is_allowed(canonical: &std::path::Path, storage_dir: &str) -> bool {
    let mut allowed_roots = Vec::new();
    if !storage_dir.trim().is_empty() {
        if let Ok(root) = std::path::PathBuf::from(storage_dir.trim()).canonicalize() {
            allowed_roots.push(root);
        }
    }
    // canonicalize() here too, not just on storage_dir above - macOS's own
    // std::env::temp_dir() (e.g. /var/folders/...) is itself commonly a
    // symlink target (/var -> /private/var), which canonicalize() on
    // `canonical` above already resolved through; comparing an
    // unresolved temp_root against an already-resolved `canonical` via
    // strip_prefix would never match (found by this fn's own test suite,
    // 2026-07-16 4R re-review follow-up: T2's test coverage caught this
    // on the very platform this repo develops on).
    let temp_root = std::env::temp_dir().canonicalize().unwrap_or_else(|_| std::env::temp_dir());
    let within_temp_tap_dir = canonical.strip_prefix(&temp_root).is_ok_and(|rest| {
        rest.components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .is_some_and(|first| first.starts_with("centinelo-transcribe-tap."))
    });
    let within_storage_dir = allowed_roots.iter().any(|root| canonical.starts_with(root));
    within_storage_dir || within_temp_tap_dir
}

/// Strips Windows' `\\?\`/`\\?\UNC\` extended-length path prefix - what
/// `Path::canonicalize()` actually returns on Windows (see the stdlib's
/// own `fs::canonicalize` docs, "On Windows...") - before handing a path
/// to `explorer.exe`. `explorer /select,<path>` frequently fails to
/// resolve that prefix silently (`spawn()` still returns `Ok`, but
/// Explorer opens with nothing selected, or a plain window) - most
/// relevant precisely for this command's own `storage_dir` case, since a
/// NAS/SMB-mounted `storage_dir` canonicalizes to the `\\?\UNC\server\
/// share\...` form (2026-07-16 4R re-review, M3). Pure string transform,
/// deliberately not `cfg`'d (only its Windows-only call site,
/// [`reveal_path`], is) so it's unit-testable on any host - stripping a
/// literal backslash-prefixed string doesn't depend on the host's actual
/// filesystem semantics. `allow(dead_code)`: genuinely unused outside a
/// Windows build (its only non-test caller is `cfg`'d to
/// `target_os = "windows"`) - kept un-`cfg`'d on purpose, see above, so
/// `cargo test` on any host (including this repo's own macOS dev
/// machine) still exercises it directly.
#[allow(dead_code)]
fn strip_windows_extended_prefix(path: &std::path::Path) -> std::path::PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return std::path::PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return std::path::PathBuf::from(rest);
    }
    path.to_path_buf()
}

#[cfg(target_os = "macos")]
fn reveal_path(path: &std::path::Path) -> Result<(), String> {
    std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open Finder: {e}"))
}

#[cfg(target_os = "windows")]
fn reveal_path(path: &std::path::Path) -> Result<(), String> {
    let normalized = strip_windows_extended_prefix(path);
    // explorer.exe's /select, syntax wants one argument, comma-glued to
    // the path, not a separate argv entry.
    let mut arg = std::ffi::OsString::from("/select,");
    arg.push(normalized.as_os_str());
    std::process::Command::new("explorer")
        .arg(arg)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open Explorer: {e}"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn reveal_path(path: &std::path::Path) -> Result<(), String> {
    let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
    std::process::Command::new("xdg-open")
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open a file manager: {e}"))
}

#[cfg(test)]
mod reveal_in_file_manager_tests {
    use super::*;

    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("centinelo-reveal-test.{name}.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn allowed_inside_storage_dir() {
        let storage = scratch_dir("storage-ok");
        let nested = storage.join("2026").join("07");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("call.txt");
        std::fs::write(&file, b"hi").unwrap();

        let canonical = file.canonicalize().unwrap();
        assert!(reveal_path_is_allowed(&canonical, &storage.to_string_lossy()));

        let _ = std::fs::remove_dir_all(&storage);
    }

    #[test]
    fn allowed_inside_temp_tap_dir_even_without_storage_dir_configured() {
        let tap_dir = std::env::temp_dir().join(format!("centinelo-transcribe-tap.test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tap_dir);
        std::fs::create_dir_all(&tap_dir).unwrap();
        let file = tap_dir.join("call-rx.wav");
        std::fs::write(&file, b"hi").unwrap();

        let canonical = file.canonicalize().unwrap();
        assert!(reveal_path_is_allowed(&canonical, "")); // storage_dir not configured yet

        let _ = std::fs::remove_dir_all(&tap_dir);
    }

    #[test]
    fn rejected_outside_both_roots() {
        let storage = scratch_dir("storage-unrelated");
        let outside = scratch_dir("outside");
        let file = outside.join("secret.txt");
        std::fs::write(&file, b"hi").unwrap();

        let canonical = file.canonicalize().unwrap();
        assert!(!reveal_path_is_allowed(&canonical, &storage.to_string_lossy()));

        let _ = std::fs::remove_dir_all(&storage);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn rejected_when_storage_dir_not_configured_and_not_temp_tap_dir() {
        let outside = scratch_dir("no-storage-configured");
        let file = outside.join("secret.txt");
        std::fs::write(&file, b"hi").unwrap();

        let canonical = file.canonicalize().unwrap();
        assert!(!reveal_path_is_allowed(&canonical, ""));

        let _ = std::fs::remove_dir_all(&outside);
    }

    /// A symlink planted *inside* `storage_dir` pointing at a file
    /// *outside* it must not grant access - `reveal_in_file_manager`
    /// canonicalizes the candidate path before ever calling
    /// `reveal_path_is_allowed`, and `canonicalize()` follows symlinks to
    /// their real target, so the check below sees the real (outside)
    /// path, not the symlink's own (inside) location. Unix-only:
    /// `std::os::unix::fs::symlink` (2026-07-16 4R re-review, T2 -
    /// explicitly requested "symlink escape rechazado").
    #[cfg(unix)]
    #[test]
    fn rejected_for_a_symlink_inside_storage_dir_pointing_outside_it() {
        let storage = scratch_dir("storage-symlink-victim");
        let outside = scratch_dir("symlink-target-outside");
        let secret = outside.join("secret.txt");
        std::fs::write(&secret, b"hi").unwrap();
        let link = storage.join("innocuous-looking-link.txt");
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        // This mirrors what reveal_in_file_manager itself does: canonicalize
        // the frontend-supplied path BEFORE checking it.
        let canonical = link.canonicalize().unwrap();
        assert_eq!(canonical, secret.canonicalize().unwrap(), "canonicalize should follow the symlink to its real target");
        assert!(!reveal_path_is_allowed(&canonical, &storage.to_string_lossy()));

        let _ = std::fs::remove_dir_all(&storage);
        let _ = std::fs::remove_dir_all(&outside);
    }

    // ---- strip_windows_extended_prefix (M3) - pure string transform,
    // deliberately not cfg'd, so these run on every host. ----

    #[test]
    fn strip_windows_extended_prefix_removes_plain_prefix() {
        let p = strip_windows_extended_prefix(std::path::Path::new(r"\\?\C:\Users\front-desk\calls\2026\07"));
        assert_eq!(p, std::path::PathBuf::from(r"C:\Users\front-desk\calls\2026\07"));
    }

    #[test]
    fn strip_windows_extended_prefix_removes_unc_prefix_and_restores_leading_slashes() {
        // canonicalize()'s own UNC form for a NAS-mounted storage_dir - the
        // exact case M3 flagged: explorer.exe fails to resolve \\?\UNC\...
        // silently.
        let p = strip_windows_extended_prefix(std::path::Path::new(r"\\?\UNC\nas01\front-desk\calls\2026\07"));
        assert_eq!(p, std::path::PathBuf::from(r"\\nas01\front-desk\calls\2026\07"));
    }

    #[test]
    fn strip_windows_extended_prefix_leaves_a_plain_path_unchanged() {
        let p = strip_windows_extended_prefix(std::path::Path::new(r"C:\Users\front-desk\calls"));
        assert_eq!(p, std::path::PathBuf::from(r"C:\Users\front-desk\calls"));
    }
}

#[cfg(test)]
mod blf_enabled_command_tests {
    use super::*;
    use crate::settings::SettingsStore;
    use std::path::PathBuf;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("centinelo-blf-cmd-test.{name}.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Note on test shape: `#[tauri::command]` fns take `State<T>`, which can't
    // be constructed outside the Tauri runtime, so these tests exercise the
    // exact code path the commands themselves run - `require_unlocked` (the
    // first line of `set_blf_enabled`, identical to `save_favorites`) and
    // `SettingsStore::update_blf_enabled` (its persistence step) - against a
    // real `SettingsStore` + `AdminSession`. This mirrors how the rest of this
    // file's command tests (e.g. `audio_settings_command_tests`) test the
    // helpers a command calls rather than the State-wrapped command fn itself.

    #[test]
    fn set_blf_enabled_is_rejected_without_unlock() {
        // P4 test #2: `set_blf_enabled` must be refused while locked, with the
        // SAME error string shape `save_favorites`'s `require_unlocked` guard
        // already produces (so the frontend's existing "settings locked"
        // handling applies unchanged). `AdminSession` defaults to locked.
        let admin = AdminSession::default();
        assert!(!admin.is_unlocked());
        let err = require_unlocked(&admin).unwrap_err();
        assert_eq!(
            err,
            "Settings are locked. Unlock with the admin password first."
        );
    }

    #[test]
    fn set_blf_enabled_flips_and_persists_when_unlocked() {
        // P4 test #3: while unlocked, flipping `true -> false` (then back to
        // `true`) must move the persisted value and be observable via the same
        // `snapshot().blf.enabled` read `get_blf_enabled` uses. Simulates the
        // command body (require_unlocked -> update_blf_enabled) end-to-end
        // against a real on-disk store. (The live unsubscribe teardown in the
        // command needs a running sidecar, exercised by P5's integration tests
        // - here we assert the persisted-switch half, which is this piece's
        // scope.)
        let dir = scratch_dir("flip");
        let store = SettingsStore::load(&dir).unwrap();
        let admin = AdminSession::default();
        admin.set_unlocked(true);
        require_unlocked(&admin).unwrap();
        assert!(store.snapshot().blf.enabled);

        store.update_blf_enabled(false).unwrap();
        assert!(!store.snapshot().blf.enabled);

        store.update_blf_enabled(true).unwrap();
        assert!(store.snapshot().blf.enabled);

        // A fresh reload sees the last-written value (true) - the flip hit
        // disk, not just the in-memory copy.
        let reloaded = SettingsStore::load(&dir).unwrap();
        assert!(reloaded.snapshot().blf.enabled);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod remote_stt_probe_tests {
    use super::*;

    /// Spawns a one-shot mock HTTP server that replies with the given status
    /// code, returning its base URL (e.g. `http://127.0.0.1:<port>`). Same
    /// `tiny_http` pattern activation.rs's own tests already use.
    fn spawn_mock_server(status: u16, body: String) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr();
        let handle = std::thread::spawn(move || {
            // The probe is a single GET, so one request is enough.
            if let Ok(request) = server.recv() {
                let response = tiny_http::Response::from_string(body).with_status_code(status);
                let _ = request.respond(response);
            }
        });
        // Detach: the thread exits after the one request is served.
        let _ = handle;
        format!("http://{addr}")
    }

    #[test]
    fn bad_url_rejected_before_any_network_call() {
        let result = probe_remote_stt("not a url", RemoteBackend::Centinelo, None);
        assert!(!result.ok);
        assert_eq!(result.code, "bad_url");
    }

    #[test]
    fn plain_http_remote_rejected() {
        let result = probe_remote_stt("http://stt.example.test", RemoteBackend::Centinelo, None);
        assert!(!result.ok);
        assert_eq!(result.code, "bad_url");
    }

    #[test]
    fn centinelo_health_200_is_ok() {
        let url = spawn_mock_server(200, r#"{"status":"ok"}"#.to_string());
        let result = probe_remote_stt(&url, RemoteBackend::Centinelo, None);
        assert!(result.ok, "expected ok, got {:?} / {}", result.code, result.detail);
        assert_eq!(result.code, "ok");
    }

    #[test]
    fn centinelo_health_500_is_http_error() {
        let url = spawn_mock_server(500, "internal".to_string());
        let result = probe_remote_stt(&url, RemoteBackend::Centinelo, None);
        assert!(!result.ok);
        assert_eq!(result.code, "http_error");
    }

    #[test]
    fn openai_compat_200_is_ok() {
        let url = spawn_mock_server(200, r#"{"data":[]}"#.to_string());
        let result = probe_remote_stt(&url, RemoteBackend::OpenaiCompat, None);
        assert!(result.ok);
        assert_eq!(result.code, "ok");
    }

    #[test]
    fn openai_compat_401_is_reachable_but_auth_required() {
        // A 401 means the endpoint answered - the key is wrong, not the URL.
        // The UI can hint "check your API key" instead of "can't reach host".
        let url = spawn_mock_server(401, r#"unauthorized"#.to_string());
        let result = probe_remote_stt(&url, RemoteBackend::OpenaiCompat, None);
        assert!(result.ok, "401 on openai_compat should be reachable");
        assert_eq!(result.code, "auth_required");
    }

    #[test]
    fn network_failure_is_network_code() {
        // Port 1 on localhost is almost certainly closed -> connection refused.
        let result = probe_remote_stt("http://127.0.0.1:1", RemoteBackend::Centinelo, None);
        assert!(!result.ok);
        assert_eq!(result.code, "network");
    }
}

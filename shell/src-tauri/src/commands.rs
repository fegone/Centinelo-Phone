//! Tauri commands exposed to the frontend. Kept thin: validation + admin-lock
//! enforcement here, actual work delegated to `settings` / `sidecar`.

use crate::sidecar::SidecarHandle;
use crate::settings::{
    self, AccountSettings, AdminSession, CallDirection, FavoriteSlot, RecentCall, SettingsStore,
    ThemePref, TransportPriority,
};
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

#[tauri::command(rename_all = "snake_case")]
pub fn sidecar_hangup(sidecar: State<SidecarHandle>) -> Result<(), String> {
    sidecar.send_cmd(serde_json::json!({ "cmd": "hangup" }))
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
    let previous_secret = settings.snapshot().account.secret;
    let secret = match input.secret {
        Some(s) if !s.is_empty() => s,
        _ => previous_secret,
    };
    let account = AccountSettings {
        host: input.host.trim().to_string(),
        ext: input.ext.trim().to_string(),
        secret,
        display_name: input.display_name.trim().to_string(),
        transport_priority: input.transport_priority,
    };
    settings.update_account(account).map_err(|e| e.to_string())?;
    sidecar.restart_now();
    Ok(())
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

// ---- theme ------------------------------------------------------------

#[tauri::command(rename_all = "snake_case")]
pub fn get_theme(settings: State<Arc<SettingsStore>>) -> ThemePref {
    settings.snapshot().theme
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_theme(settings: State<Arc<SettingsStore>>, theme: ThemePref) -> Result<(), String> {
    settings.update_theme(theme).map_err(|e| e.to_string())
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

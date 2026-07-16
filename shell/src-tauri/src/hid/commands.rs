//! `#[tauri::command]`s for HID headset support. Kept in this module
//! (rather than added to `crate::commands`, already 900+ lines and actively
//! touched by other in-flight work this session) so this feature's whole
//! diff stays inside `crate::hid` plus the couple of lines `lib.rs` needs to
//! register it - see this feature's task report.

use super::device::DeviceSummary;
use super::{HidHandle, HidStatus};
use crate::settings::{AdminSession, HidSettings, SettingsStore};
use std::sync::Arc;
use tauri::State;

/// Deliberately duplicated from `crate::commands::require_unlocked` (same
/// four lines) rather than making that function `pub(crate)` and importing
/// it - see this file's module doc for why touching `commands.rs` at all
/// was avoided this round.
fn require_unlocked(admin: &AdminSession) -> Result<(), String> {
    if admin.is_unlocked() {
        Ok(())
    } else {
        Err("Settings are locked. Unlock with the admin password first.".to_string())
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn hid_status(hid: State<HidHandle>) -> HidStatus {
    hid.status()
}

/// Fresh enumeration for a device picker in Settings - not gated behind
/// admin unlock (matches `sidecar_status`/`get_blf_states`'s own
/// read-only, ungated precedent): seeing *which* HID devices exist is not
/// itself sensitive, only *changing* which one this app uses is (see
/// `save_hid_settings` below).
#[tauri::command(rename_all = "snake_case")]
pub fn hid_list_devices(hid: State<HidHandle>) -> Vec<DeviceSummary> {
    hid.list_candidates()
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_hid_settings(settings: State<Arc<SettingsStore>>) -> HidSettings {
    settings.snapshot().hid
}

/// Admin-gated like every other sensitive settings group (account,
/// favorites, transcription) - a call-center agent can use whichever
/// headset is plugged in, but shouldn't be able to repoint the app at a
/// different device, or turn the feature off, without the admin password
/// (spec §5: "Admin lock ... Applies shell-wide").
#[tauri::command(rename_all = "snake_case")]
pub fn save_hid_settings(settings: State<Arc<SettingsStore>>, admin: State<AdminSession>, value: HidSettings) -> Result<(), String> {
    require_unlocked(&admin)?;
    settings.update_hid(value).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::AdminSession;

    #[test]
    fn require_unlocked_matches_commands_rs_own_wording() {
        // Locks in the exact copy the frontend's error toast shows - a
        // silent divergence from crate::commands::require_unlocked's own
        // text (this function's whole reason to exist is being an
        // intentional duplicate of it) would be confusing, not dangerous,
        // but still worth catching.
        let admin = AdminSession::default();
        let err = require_unlocked(&admin).unwrap_err();
        assert_eq!(err, "Settings are locked. Unlock with the admin password first.");
        admin.set_unlocked(true);
        assert!(require_unlocked(&admin).is_ok());
    }
}

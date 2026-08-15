//! Static invariant check on `capabilities/*.json`: the `console` window
//! (the LEGACY `SEPARATE_WINDOW_ENV` path in `console.rs` - see that
//! file's doc comment) must never be granted the admin/license/
//! provisioning/settings surface, no matter what future edits do to
//! `capabilities/console.json`.
//!
//! This is a **capability-file parse test, not a runtime ACL-resolution
//! test**: it asserts on the JSON tauri-build reads at compile time, not
//! on what `tauri::ipc::InvokeRequest` resolution actually decides for a
//! real invoke at runtime. Reaching a true runtime test would mean
//! standing up a `tauri::test` mock app with both windows and asserting
//! on `Invoke::acl` per command, which `tauri-build`'s generated
//! `__app-acl__` manifest (an `OUT_DIR` artifact, not a public API) makes
//! awkward to reach from an integration test in this crate version - see
//! this module's report for what was and wasn't verified. What this DOES
//! prove: the exact bug class this whole change closes (a forbidden
//! command's `allow-*` permission identifier landing in
//! `capabilities/console.json`) is caught by `cargo test`, verified by
//! mutation - see `git log` on this file's introducing commit for the
//! "insert the mutation, watch it fail, revert" transcript.
#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    const DEFAULT_CAPABILITY_JSON: &str = include_str!("../capabilities/default.json");
    const CONSOLE_CAPABILITY_JSON: &str = include_str!("../capabilities/console.json");

    /// Command names (snake_case, matching `build.rs`'s `APP_COMMANDS`)
    /// that `capabilities/console.json` must NEVER grant `allow-<slug>`
    /// for, regardless of window ("console" is a license-gated,
    /// console-ui-rendering window - see that file's own module doc for
    /// the full threat reasoning). Deliberately broader than "what
    /// console.rs's embedded HTML happens to invoke today" - this is the
    /// permanent blocklist, not a snapshot of current usage.
    const FORBIDDEN_ON_CONSOLE: &[&str] = &[
        "admin_unlock",
        "admin_set_password",
        "admin_lock",
        "admin_status",
        "activate_license",
        "get_license_settings",
        "set_core_binary_path",
        "provisioning_resolve",
        "provisioning_pending_preview",
        "provisioning_apply",
        "provisioning_cancel",
        "save_account_settings",
        "updater_download",
        "updater_install",
    ];

    /// Parses a `capabilities/*.json` file's `permissions` array into the
    /// set of app-command slugs it grants (`allow-<slug>` entries only -
    /// `core:*`/`updater:*`/`process:*` plugin permissions and `deny-*`
    /// entries are irrelevant to "can this window invoke command X" for
    /// our own commands and are filtered out).
    fn granted_app_command_slugs(capability_json: &str) -> HashSet<String> {
        let parsed: serde_json::Value =
            serde_json::from_str(capability_json).expect("capability file must be valid JSON");
        parsed["permissions"]
            .as_array()
            .expect("capability file must have a permissions array")
            .iter()
            .filter_map(|p| p.as_str())
            .filter_map(|p| p.strip_prefix("allow-"))
            .map(|slug| slug.to_string())
            .collect()
    }

    /// `command_name` (snake_case) -> the slug `tauri-build`'s
    /// `autogenerate_command_permissions` derives for it (`_` -> `-`,
    /// same transform `build.rs`'s `AppManifest::commands` relies on -
    /// see tauri-utils's `acl/build.rs`).
    fn slug(command_name: &str) -> String {
        command_name.replace('_', "-")
    }

    #[test]
    fn console_window_is_never_granted_a_forbidden_command() {
        let granted = granted_app_command_slugs(CONSOLE_CAPABILITY_JSON);
        let leaked: Vec<&&str> = FORBIDDEN_ON_CONSOLE
            .iter()
            .filter(|cmd| granted.contains(&slug(cmd)))
            .collect();
        assert!(
            leaked.is_empty(),
            "capabilities/console.json grants forbidden command(s): {leaked:?} - the console \
             window must never reach admin/license/provisioning/settings commands"
        );
    }

    /// The console window's embedded HTML (`console.rs`'s `INDEX_HTML`)
    /// only ever calls this fixed set of commands - a regression here
    /// means either the grant list drifted out of sync with what the
    /// window actually needs (breaking it) or ballooned past what it
    /// needs (widening its blast radius for no reason).
    #[test]
    fn console_window_has_exactly_the_commands_its_embedded_html_calls() {
        let granted = granted_app_command_slugs(CONSOLE_CAPABILITY_JSON);
        let expected: HashSet<String> = [
            "console_frontend_fatal",
            "console_frontend_ready",
            "get_favorites",
            "sidecar_dial",
            "sidecar_answer",
            "sidecar_hangup",
            "sidecar_restart",
            "sidecar_hold",
            "sidecar_resume",
            "sidecar_mute",
            "sidecar_blind_transfer",
            "sidecar_attended_transfer",
            "sidecar_complete_transfer",
            "sidecar_abort_transfer",
            "sidecar_blf_subscribe",
            "sidecar_blf_unsubscribe",
        ]
        .into_iter()
        .map(slug)
        .collect();
        assert_eq!(
            granted, expected,
            "capabilities/console.json's granted command set no longer matches console.rs's \
             embedded HTML's actual invoke() calls"
        );
    }

    /// Sanity check on the other side of the split: the main window DOES
    /// need the admin/license surface (it's the only window that should
    /// have it) plus the inline console-panel's call-control commands
    /// (the panel mounts inside `main` since 2.1.0).
    #[test]
    fn main_window_has_the_admin_and_console_panel_surface() {
        let granted = granted_app_command_slugs(DEFAULT_CAPABILITY_JSON);
        for cmd in [
            "admin_unlock",
            "admin_set_password",
            "admin_status",
            "activate_license",
            "get_license_settings",
            "provisioning_apply",
            "set_core_binary_path",
            "updater_download",
            "updater_install",
            "sidecar_hold",
            "sidecar_resume",
            "sidecar_mute",
            "sidecar_blind_transfer",
            "sidecar_attended_transfer",
            "sidecar_complete_transfer",
            "sidecar_abort_transfer",
            "sidecar_blf_subscribe",
            "sidecar_blf_unsubscribe",
        ] {
            assert!(
                granted.contains(&slug(cmd)),
                "capabilities/default.json is missing allow-{} - a real feature would silently \
                 die in production the moment the frontend called it",
                slug(cmd)
            );
        }
    }

    /// Commands that are wired into `generate_handler!` (and therefore
    /// live in `build.rs`'s `APP_COMMANDS`, so a permission for them
    /// exists to grant) but that no shipped frontend currently invokes -
    /// see the task report for the full invoke() cross-reference. Neither
    /// window should grant these: granting an unused command "just in
    /// case" is exactly the kind of unnecessary attack surface this
    /// change exists to remove.
    #[test]
    fn unused_commands_are_granted_nowhere() {
        let default_granted = granted_app_command_slugs(DEFAULT_CAPABILITY_JSON);
        let console_granted = granted_app_command_slugs(CONSOLE_CAPABILITY_JSON);
        for cmd in [
            "admin_lock",
            "transcription_manual_stop",
            "premium_info",
            "premium_diagnostic",
            "hid_status",
            "hid_list_devices",
            "get_hid_settings",
            "save_hid_settings",
        ] {
            let s = slug(cmd);
            assert!(
                !default_granted.contains(&s),
                "allow-{s} is granted on `main` but no shipped frontend invokes {cmd} - \
                 confirm it's actually needed before granting it"
            );
            assert!(
                !console_granted.contains(&s),
                "allow-{s} is granted on `console` but no shipped frontend invokes {cmd}"
            );
        }
    }
}

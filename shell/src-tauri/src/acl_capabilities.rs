//! Static invariant checks on `capabilities/*.json`.
//!
//! Two things are checked here, both deliberately **capability-file
//! parse tests, not runtime ACL-resolution tests**: they assert on the
//! JSON `tauri-build` reads at compile time, not on what
//! `tauri::ipc::InvokeRequest` resolution actually decides for a real
//! invoke at runtime. Reaching a true runtime test would mean standing
//! up a `tauri::test` mock app with both windows and asserting on
//! `Invoke::acl` per command, which `tauri-build`'s generated
//! `__app-acl__` manifest (an `OUT_DIR` artifact, not a public API)
//! makes awkward to reach from an integration test in this crate
//! version - see this task's report for what was and wasn't verified.
//!
//! 1. The `console` window (the LEGACY `SEPARATE_WINDOW_ENV` path in
//!    `console.rs` - see that file's doc comment) must never be granted
//!    the admin/license/provisioning/settings surface, no matter what
//!    future edits do to `capabilities/console.json`.
//! 2. Every command a shipped frontend file actually calls must have a
//!    matching `allow-<slug>` grant in `capabilities/default.json` -
//!    checked by DERIVING the invoked-command set straight out of
//!    `shell/ui/js/*.js` (three extraction passes: literal
//!    `invoke("...")` calls, `console-panel.js`'s
//!    `CONSOLE_DISPATCH_TABLE`, and `settings-load.js`'s
//!    `SETTINGS_FIELD_GROUPS`) rather than a hand-copied list that can
//!    silently drift out of sync with the frontend the moment someone
//!    adds a new `invoke()` call and forgets the capability grant - the
//!    exact silent-failure shape this whole project has paid for
//!    eleven times before (see phone/CLAUDE.md's Windows postmortem).
#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::path::Path;

    const DEFAULT_CAPABILITY_JSON: &str = include_str!("../capabilities/default.json");
    const CONSOLE_CAPABILITY_JSON: &str = include_str!("../capabilities/console.json");

    /// Command names (snake_case, matching `build.rs`'s
    /// `commands_from_generate_handler` output) that
    /// `capabilities/console.json` must NEVER grant `allow-<slug>` for,
    /// regardless of window ("console" is a license-gated,
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

    /// True for strings that could plausibly be one of THIS app's own
    /// snake_case command names - lowercase ascii letters/digits/`_`
    /// only, at least one char. Filters out plugin command identifiers
    /// like `plugin:resources|close` (which contain `:`/`|` and show up
    /// both as real invoke() targets for plugin commands - out of this
    /// file's scope, see the task report's noted pre-existing gap - and
    /// inside doc-comment examples like updater.js's `(rid) =>
    /// invoke("plugin:resources|close", ...)`) without needing to tell
    /// "real call" apart from "comment mentioning a call" by anything
    /// smarter than "does it look like one of our own command names".
    fn looks_like_app_command_name(s: &str) -> bool {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }

    /// Pass 1: every literal `invoke("command")` / `invoke('command')`
    /// call in `source` (also matches `deps.invoke(...)`,
    /// `tauri.core.invoke(...)` - anything ending in `invoke(` followed,
    /// after optional whitespace, directly by a quote). Calls where the
    /// first argument is a variable/expression (`invoke(g.cmd)`,
    /// `invoke(entry[0], ...)`) are invisible to this pass by
    /// construction - those are the dynamic paths covered by passes 2
    /// and 3 below, not something a literal-string scan can ever see.
    fn literal_invoke_commands(source: &str) -> Vec<String> {
        let bytes = source.as_bytes();
        let mut out = Vec::new();
        let mut search_from = 0usize;
        while let Some(rel) = source[search_from..].find("invoke(") {
            let mut i = search_from + rel + "invoke(".len();
            while i < bytes.len() && (bytes[i] as char).is_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                let start = i + 1;
                if let Some(end_rel) = source[start..].find(quote as char) {
                    let literal = &source[start..start + end_rel];
                    if looks_like_app_command_name(literal) {
                        out.push(literal.to_string());
                    }
                }
            }
            search_from = search_from + rel + "invoke(".len();
        }
        out
    }

    /// Pass 2: `console-panel.js`'s `CONSOLE_DISPATCH_TABLE` maps each
    /// EngineBridge verb to `[tauriCommand, argBuilder]` - the Tauri
    /// command name is always the array's first (and only string)
    /// element, on a line shaped `<verb>: ["<command>", ...]`. This
    /// table is also byte-for-byte mirrored inside `console.rs`'s
    /// embedded `INDEX_HTML` for the legacy separate window (see
    /// `console_window_has_exactly_the_commands_its_embedded_html_calls`
    /// below for that half); here we only need the copy that runs
    /// inside `main` (the inline panel).
    fn bracketed_first_string_after_colon(source: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut search_from = 0usize;
        while let Some(rel) = source[search_from..].find(": [\"") {
            let start = search_from + rel + ": [\"".len();
            if let Some(end_rel) = source[start..].find('"') {
                let literal = &source[start..start + end_rel];
                if looks_like_app_command_name(literal) {
                    out.push(literal.to_string());
                }
            }
            search_from = search_from + rel + ": [\"".len();
        }
        out
    }

    /// Pass 3: `settings-load.js`'s `SETTINGS_FIELD_GROUPS` drives
    /// `app.js`'s `invoke(g.cmd)` (a variable, invisible to pass 1) -
    /// each group is `{ key: ..., cmd: "<command>", ... }`.
    fn settings_field_group_commands(source: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut search_from = 0usize;
        while let Some(rel) = source[search_from..].find("cmd: \"") {
            let start = search_from + rel + "cmd: \"".len();
            if let Some(end_rel) = source[start..].find('"') {
                let literal = &source[start..start + end_rel];
                if looks_like_app_command_name(literal) {
                    out.push(literal.to_string());
                }
            }
            search_from = search_from + rel + "cmd: \"".len();
        }
        out
    }

    /// Every command any shipped `shell/ui/js/*.js` file invokes on the
    /// `main` window, derived (not hand-listed) from the three passes
    /// above. `*.test.js` files are excluded - they mock `invoke` with
    /// spy assertions that don't imply the real app calls that name.
    fn frontend_invoked_commands() -> HashSet<String> {
        let ui_js_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/js");
        let mut commands = HashSet::new();
        let mut settings_load_seen = false;
        let mut console_panel_seen = false;

        let entries = fs::read_dir(&ui_js_dir).unwrap_or_else(|e| {
            panic!("failed to read {}: {e}", ui_js_dir.display())
        });
        for entry in entries {
            let entry = entry.expect("readdir entry");
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.ends_with(".js") || name.ends_with(".test.js") {
                continue;
            }
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));

            commands.extend(literal_invoke_commands(&source));

            if name == "console-panel.js" {
                console_panel_seen = true;
                commands.extend(bracketed_first_string_after_colon(&source));
            }
            if name == "settings-load.js" {
                settings_load_seen = true;
                commands.extend(settings_field_group_commands(&source));
            }
        }

        // If either file gets renamed/moved, its dynamic-dispatch pass
        // silently stops running and this whole test quietly loses
        // coverage instead of failing - fail loudly instead.
        assert!(
            console_panel_seen,
            "expected to find ui/js/console-panel.js (CONSOLE_DISPATCH_TABLE pass) - did it move?"
        );
        assert!(
            settings_load_seen,
            "expected to find ui/js/settings-load.js (SETTINGS_FIELD_GROUPS pass) - did it move?"
        );

        commands
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
    /// needs (widening its blast radius for no reason). Hand-listed
    /// (unlike `default.json`'s check below) because `INDEX_HTML` is a
    /// Rust string constant inside `console.rs`, not a `.js` file this
    /// module's extraction passes read.
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

    /// The real protection this module exists for: every command
    /// `shell/ui/js/*.js` actually invokes (literal calls +
    /// `CONSOLE_DISPATCH_TABLE` + `SETTINGS_FIELD_GROUPS`, all derived
    /// from the source, not hand-copied) must have a matching
    /// `allow-<slug>` in `capabilities/default.json`. Add a new
    /// `invoke("some_new_command", ...)` to `app.js` without granting it
    /// in `default.json` and THIS test fails, naming the missing
    /// command - the exact gap the RELIABILITY lens flagged in the
    /// hand-enumerated version of this test (it only checked 18 of 64
    /// granted commands, so 46 had zero coverage against frontend
    /// drift).
    #[test]
    fn every_frontend_invoked_command_is_granted_on_main() {
        let granted = granted_app_command_slugs(DEFAULT_CAPABILITY_JSON);
        let invoked = frontend_invoked_commands();
        assert!(
            !invoked.is_empty(),
            "extraction found zero invoked commands - almost certainly means the extraction \
             passes broke (a shell/ui/js rewrite changed the call shapes they look for), not \
             that the frontend stopped calling any commands"
        );
        let missing: Vec<String> = invoked
            .iter()
            .filter(|cmd| !granted.contains(&slug(cmd)))
            .cloned()
            .collect();
        assert!(
            missing.is_empty(),
            "capabilities/default.json is missing allow-<slug> for command(s) the frontend \
             actually invokes: {missing:?} - this is a real feature that would silently die \
             in production the moment a user hits it (the invoke() rejects and the .catch() \
             swallows it, per this project's own history of exactly this failure mode)"
        );
    }

    /// Commands that are wired into `generate_handler!` (and therefore
    /// live in `src/lib.rs`, so `build.rs` generates a permission for
    /// them to grant) but that no shipped frontend currently invokes -
    /// verified by set subtraction over the full 77-command handler list
    /// against both capability files' grants (see the task report for
    /// the script). Neither window should grant these: granting an
    /// unused command "just in case" is exactly the kind of unnecessary
    /// attack surface this change exists to remove. The audio/codec
    /// panels and the BLF-enabled toggle are genuinely dead here (their
    /// UI paints from sidecar-pushed events, not from calling these
    /// getters/setters - confirmed by grep, not assumed), not oversights
    /// left over from a previous version of this list.
    #[test]
    fn unused_commands_are_granted_nowhere() {
        let default_granted = granted_app_command_slugs(DEFAULT_CAPABILITY_JSON);
        let console_granted = granted_app_command_slugs(CONSOLE_CAPABILITY_JSON);
        for cmd in [
            "admin_lock",
            "get_audio_settings",
            "get_codec_settings",
            "get_hid_settings",
            "hid_list_devices",
            "hid_status",
            "premium_diagnostic",
            "premium_info",
            "save_hid_settings",
            "set_blf_enabled",
            "transcription_manual_stop",
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

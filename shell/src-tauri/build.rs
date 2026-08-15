use std::collections::HashSet;
use std::fs;
use tauri_build::{AppManifest, Attributes};

/// The single source of truth for which commands get ACL permissions:
/// `src/lib.rs`'s own `tauri::generate_handler![...]` block, parsed at
/// build time instead of hand-copied into a second list here.
///
/// This whole file exists because without an `AppManifest`,
/// `has_app_acl_manifest` is false and Tauri lets any local webview
/// invoke ANY command in `generate_handler!` with zero permission check
/// (see tauri-2.11.5 `src/webview/mod.rs`'s invoke-resolution gate).
/// `AppManifest::commands()` closes that by autogenerating an
/// `allow-<slug>`/`deny-<slug>` permission pair per command (`_` ->
/// `-`); whether a window is actually GRANTED `allow-<slug>` is decided
/// per-window in `capabilities/*.json`, not here. A command that never
/// makes it into this function's returned list gets no permission at
/// all and can never be invoked by any webview, ACL or not.
///
/// A first version of this file kept a hand-written `APP_COMMANDS`
/// array "in sync with `generate_handler!` by convention" — a 2nd 4R
/// review pass correctly rejected that: adding a command to the handler
/// without remembering to also add it here compiles clean and silently
/// denies the new command in every window forever, which is a real
/// footgun in a codebase whose whole history is silent failures nobody
/// noticed (see phone/CLAUDE.md's Windows-never-worked postmortem).
/// Parsing the actual macro invocation removes the second list instead
/// of trying to test the two lists into staying equal.
fn commands_from_generate_handler(lib_rs_path: &str) -> Vec<String> {
    let src = fs::read_to_string(lib_rs_path).unwrap_or_else(|e| {
        panic!("failed to read {lib_rs_path} for ACL command extraction: {e}")
    });

    let marker = "tauri::generate_handler![";
    let start = src.find(marker).unwrap_or_else(|| {
        panic!(
            "{lib_rs_path}: could not find `{marker}` — ACL command list would come back \
             empty and every command would be denied everywhere"
        )
    }) + marker.len();

    // The macro's argument list is nothing but comma-separated command
    // paths (`module::function` or a bare `function`) — no string
    // literals, no attributes, no nested `[`/`]` of any kind ever
    // appear inside it in this codebase's style (verified: exactly one
    // `]` shows up between the marker and the line that closes the
    // macro call). That makes "first `]` after the marker" a reliable
    // end boundary without writing a real Rust token parser here.
    let end = src[start..]
        .find(']')
        .unwrap_or_else(|| panic!("{lib_rs_path}: `{marker}` has no matching `]`"))
        + start;

    let body = &src[start..end];

    let commands: Vec<String> = body
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|path| {
            // Tauri's command identifier is always the function's own
            // name, never its module path — `commands::sidecar_dial`
            // and `hid::commands::hid_status` both register (and must
            // be granted) as `sidecar_dial` / `hid_status`. See
            // tauri-utils's `autogenerate_command_permissions`, which
            // uses the literal string handed to `AppManifest::commands`
            // as both the permission's `commands.allow` entry and (with
            // `_` -> `-`) its identifier.
            path.rsplit("::").next().unwrap_or(path).to_string()
        })
        .collect();

    assert!(
        !commands.is_empty(),
        "{lib_rs_path}: parsed zero commands out of `generate_handler!` — almost certainly \
         means this parser broke (a syntax change to the macro call), not that the handler \
         is really empty; fix the parser, don't silence this assertion"
    );

    let mut seen = HashSet::new();
    for cmd in &commands {
        assert!(
            seen.insert(cmd.clone()),
            "{lib_rs_path}: `{cmd}` appears twice in `generate_handler!` once module paths \
             are stripped (either the same command is registered twice, or two different \
             modules export same-named commands and collide here) — \
             `AppManifest::commands` would generate a duplicate/overwritten permission file"
        );
    }

    commands
}

fn main() {
    // Re-run this build script — and therefore re-derive the ACL command
    // list — whenever `generate_handler!`'s own file changes, so adding
    // or removing a command can never leave stale permissions behind.
    println!("cargo:rerun-if-changed=src/lib.rs");

    let commands = commands_from_generate_handler("src/lib.rs");
    // `AppManifest::commands` wants `&'static [&'static str]`; leaking is
    // fine here since build.rs is a short-lived process that exits right
    // after `try_build` returns.
    let commands: &'static [&'static str] = Vec::leak(
        commands
            .into_iter()
            .map(|s| -> &'static str { s.leak() })
            .collect(),
    );

    let attributes = Attributes::new().app_manifest(AppManifest::new().commands(commands));

    if let Err(error) = tauri_build::try_build(attributes) {
        // Mirrors tauri_build::build()'s own error handling (it isn't
        // reusable directly since it always builds with default
        // Attributes) so a broken ACL still fails the build with the same
        // "unknown field -> stale tauri-build" hint instead of a bare
        // panic.
        let error = format!("{error:#}");
        println!("{error}");
        if error.starts_with("unknown field") {
            print!("found an unknown configuration field. This usually means that you are using a CLI version that is newer than `tauri-build` and is incompatible. ");
            println!(
                "Please try updating the Rust crates by running `cargo update` in the Tauri app folder."
            );
        }
        std::process::exit(1);
    }
}

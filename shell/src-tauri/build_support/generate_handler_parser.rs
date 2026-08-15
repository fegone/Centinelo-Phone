// Pure parsing logic for extracting the `generate_handler!` command list
// out of `src/lib.rs`'s source text.
//
// Spliced (via `include!`) into TWO otherwise-unrelated compilation
// units: `build.rs` (the real, load-bearing use - it feeds
// `AppManifest::commands()`) and `src/generate_handler_parser_tests.rs`
// (a `#[cfg(test)]`-only module, so `cargo test` actually exercises
// this logic with fixtures). `build.rs` is compiled as a separate
// build-script binary Cargo never runs under `cargo test`, so without
// this second inclusion the parser - the one piece of this whole ACL
// change with no runtime/JSON safety net under it - would have had
// zero test coverage no matter how thorough the capability-file tests
// downstream of it are. RELIABILITY 4R round-4 finding: a `]` inside a
// `// see [ADR-12](url)`-style comment (a link style this codebase
// already uses elsewhere) used to truncate the command list silently
// in the general case - it happened to be loud today only because the
// last handler entry (`log_frontend_error`) is also granted in
// `capabilities/default.json`, so `validate_capabilities` had
// something to complain about. That's an accident of ordering, not a
// guarantee - fixed by skipping `//` comments while hunting for the
// closing `]`, verified below with a fixture that has one.
//
// (Regular `//` comments, not `//!` inner doc comments: this file is
// spliced via `include!` partway through another file, where an inner
// doc comment isn't legal syntax - `//!` only works at the true top of
// a file/module.)

/// Finds the first `]` in `source` starting at byte offset `start`,
/// treating `// ...` through end-of-line as a comment - a `]` inside a
/// comment can never be mistaken for the macro's own closing bracket.
/// Does not need to understand string/char literals or lifetimes:
/// nothing inside `generate_handler!`'s argument list is ever anything
/// but command paths and `//` comments (verified against the real
/// `src/lib.rs`: zero `"`/`'` bytes appear inside the block today), so a
/// full Rust tokenizer would be solving a problem this file doesn't
/// have - documented here rather than silently assumed, since it's the
/// one corner this parser deliberately does NOT handle.
fn find_bracket_close_skipping_line_comments(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = start;
    let mut in_comment = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_comment {
            if c == b'\n' {
                in_comment = false;
            }
            i += 1;
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            in_comment = true;
            i += 2;
            continue;
        }
        if c == b']' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Strips `// ...` line comments out of `source`, cutting each line at
/// its first `//` - same "no string/char literal awareness needed"
/// reasoning as the function above, and only ever called on the small
/// slice between `generate_handler![` and its closing `]`, not the
/// whole file. Without this, a comment sitting between two command
/// paths (e.g. `commands::foo, // see module::bar for the sibling,
/// commands::baz`) would leak its own `::`-separated words into the
/// parsed command list once the body is split on commas.
fn strip_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parses the `generate_handler![...]` command list out of
/// `lib_rs_source` (the full text of `src/lib.rs`, or - for the fixture
/// tests - a small stand-in with the same shape). Returns the list of
/// command names in source order (module path stripped -
/// `commands::sidecar_dial` -> `sidecar_dial`, matching Tauri's own
/// rule that a command's identifier is its function name, never its
/// module path), or an `Err` describing what went wrong.
fn commands_from_generate_handler(lib_rs_source: &str) -> Result<Vec<String>, String> {
    let marker = "tauri::generate_handler![";
    let start = lib_rs_source.find(marker).ok_or_else(|| {
        format!(
            "could not find `{marker}` — is generate_handler! still called with that exact \
             spelling?"
        )
    })? + marker.len();

    let end = find_bracket_close_skipping_line_comments(lib_rs_source, start).ok_or_else(|| {
        format!("`{marker}` has no matching `]` (ignoring `//` comments)")
    })?;

    let body = strip_line_comments(&lib_rs_source[start..end]);

    let commands: Vec<String> = body
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|path| {
            // Tauri's command identifier is always the function's own
            // name, never its module path - `commands::sidecar_dial`
            // and `hid::commands::hid_status` both register (and must
            // be granted) as `sidecar_dial` / `hid_status`.
            path.rsplit("::").next().unwrap_or(path).to_string()
        })
        .collect();

    if commands.is_empty() {
        return Err(
            "parsed zero commands out of `generate_handler!` — almost certainly means this \
             parser broke (a syntax change to the macro call), not that the handler is really \
             empty"
                .to_string(),
        );
    }

    let mut seen = std::collections::HashSet::new();
    for cmd in &commands {
        if !seen.insert(cmd.clone()) {
            return Err(format!(
                "`{cmd}` appears twice in `generate_handler!` once module paths are stripped \
                 (either the same command is registered twice, or two different modules export \
                 same-named commands and collide here) — `AppManifest::commands` would generate \
                 a duplicate/overwritten permission file"
            ));
        }
    }

    Ok(commands)
}

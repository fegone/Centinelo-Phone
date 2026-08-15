// Fixture tests for build.rs's `generate_handler!` parser.
//
// The parser itself lives in
// `build_support/generate_handler_parser.rs` and is spliced into TWO
// places: `build.rs` (real use) and here, via `include!`, purely so
// `cargo test` can exercise it - `build.rs` is its own build-script
// binary that `cargo test` never runs, so without this file the parser
// had zero test coverage no matter how many capability-file tests
// existed downstream of it (RELIABILITY 4R round-4 finding).
include!("../build_support/generate_handler_parser.rs");

#[cfg(test)]
mod tests {
    use super::*;

    /// A normal `generate_handler!` call, shaped like the real
    /// `src/lib.rs` one but trimmed to a handful of entries (some bare,
    /// some module-qualified) - the parser must strip module paths and
    /// return the plain command names in source order.
    const NORMAL: &str = r#"
        .invoke_handler(tauri::generate_handler![
            commands::sidecar_dial,
            commands::sidecar_answer,
            hid::commands::hid_status,
            frontend_log::log_frontend_error,
        ])
        .build(tauri::generate_context!())
    "#;

    #[test]
    fn normal_input_extracts_the_expected_set_in_order() {
        let commands = commands_from_generate_handler(NORMAL).expect("should parse");
        assert_eq!(
            commands,
            vec!["sidecar_dial", "sidecar_answer", "hid_status", "log_frontend_error"]
        );
    }

    /// RELIABILITY 4R round-4 finding: a `]` inside a `//` comment (this
    /// codebase's own `// see [ADR-12](url)` doc-link style) used to be
    /// read as the macro's own closing bracket, truncating the command
    /// list right there - silently in the general case, since whether
    /// the build actually failed depended on whether the truncated-off
    /// commands happened to include one some capability.json still
    /// referenced. This fixture puts the bracketed comment BEFORE the
    /// real closing `]`, with more real commands after it, and asserts
    /// the FULL set - not just a count - survives past it.
    const BRACKET_INSIDE_COMMENT: &str = r#"
        .invoke_handler(tauri::generate_handler![
            commands::sidecar_dial,
            // see [ADR-12](https://example.invalid/adr-12) for why this
            // command exists at all
            commands::sidecar_answer,
            commands::sidecar_hangup,
        ])
    "#;

    #[test]
    fn a_bracket_inside_a_comment_does_not_truncate_the_list() {
        let commands = commands_from_generate_handler(BRACKET_INSIDE_COMMENT).expect("should parse");
        assert_eq!(
            commands,
            vec!["sidecar_dial", "sidecar_answer", "sidecar_hangup"]
        );
    }

    /// A comment between two commands that itself contains `::` (e.g.
    /// referencing another module in prose) must not leak a phantom
    /// command into the parsed set once the body is split on commas.
    const COMMENT_CONTAINS_DOUBLE_COLON: &str = r#"
        .invoke_handler(tauri::generate_handler![
            commands::sidecar_dial,
            // mirrors commands::legacy_dial from the v1 client, removed
            commands::sidecar_answer,
        ])
    "#;

    #[test]
    fn a_comment_containing_double_colon_does_not_leak_a_phantom_command() {
        let commands =
            commands_from_generate_handler(COMMENT_CONTAINS_DOUBLE_COLON).expect("should parse");
        assert_eq!(commands, vec!["sidecar_dial", "sidecar_answer"]);
    }

    #[test]
    fn missing_marker_is_an_error_not_an_empty_result() {
        let err = commands_from_generate_handler("fn main() {}").unwrap_err();
        assert!(err.contains("generate_handler!"), "unexpected error: {err}");
    }

    #[test]
    fn unterminated_bracket_is_an_error() {
        let err =
            commands_from_generate_handler("tauri::generate_handler![commands::sidecar_dial")
                .unwrap_err();
        assert!(err.contains("no matching"), "unexpected error: {err}");
    }

    #[test]
    fn a_duplicate_command_after_stripping_module_paths_is_an_error() {
        let err = commands_from_generate_handler(
            "tauri::generate_handler![commands::sidecar_dial, other::sidecar_dial]",
        )
        .unwrap_err();
        assert!(err.contains("sidecar_dial"), "unexpected error: {err}");
        assert!(err.contains("twice"), "unexpected error: {err}");
    }
}

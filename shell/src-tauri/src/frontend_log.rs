//! Frontend (JavaScript) crash/error reporting.
//!
//! Born from a full day lost chasing a Windows-only bug: the window painted
//! but never responded to keyboard, title-bar buttons, or the Settings
//! screen. Three shipped fixes in a row missed the actual cause because
//! there was nothing to diagnose *from* - `ui/js/` had no `window.onerror`,
//! no `unhandledrejection` listener, no resource-load-failure listener, and
//! `tauri-plugin-log`'s `LogDir` only ever received what Rust code chose to
//! log. If the frontend threw during boot, no trace reached disk, the
//! screen, or anywhere else - two of the three "fixes" were even verified
//! against the wrong target and reported false green. See
//! `shell/README.md`'s "Frontend error logging" section for the short
//! version and `ui/index.html`'s inline `<script>` (armed before
//! `js/app.js` itself even starts loading) plus `ui/js/error-reporting.js`
//! for the browser side that feeds this module via
//! `invoke("log_frontend_error", { report })`.
//!
//! This module only ever produces a `log::` line - it never touches
//! anything visible on screen (that's the inline script's own fallback
//! banner, deliberately independent of both this command succeeding and of
//! `js/app.js` having loaded at all).

use serde::Deserialize;

/// Mirrors the object `ui/js/error-reporting.js`'s `buildErrorReport` (and
/// `ui/index.html`'s inline capture script, which sends the same shape by
/// hand before that module is even guaranteed to be loaded) produces. The
/// frontend already caps string lengths before sending - `format_log_line`
/// below re-applies its own caps and redaction anyway rather than trusting
/// the browser side blindly: the frontend isn't a trust boundary this crate
/// otherwise treats as safe for what ends up in a plaintext file on disk
/// (`lib.rs`'s `is_call_content_log_target` doc makes the identical point
/// about every other log site in this crate).
#[derive(Debug, Deserialize)]
pub struct FrontendErrorReport {
    /// "error" | "unhandledrejection" | "resource" | "milestone" - shapes
    /// the log line's prefix, never matched exhaustively (an unrecognized
    /// kind still prints, just under its own literal name).
    pub kind: String,
    /// "error" | "warn" | "info" - which `log::` macro `log_frontend_error`
    /// calls. Anything else falls back to `log::error!` (see that
    /// function) - a stricter frontend-side enum isn't worth the extra
    /// coupling for a value that only ever picks a log level.
    pub level: String,
    pub message: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub col: Option<u32>,
    #[serde(default)]
    pub stack: Option<String>,
}

const MAX_MESSAGE_LEN: usize = 300;
const MAX_STACK_LEN: usize = 800;

/// Truncates to `max_chars` *characters*, not bytes - a raw byte cut could
/// split a multi-byte UTF-8 codepoint and panic, which an error-reporting
/// path must never do to itself. Appends `…` only when something was
/// actually cut, so a short message round-trips unchanged.
fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut out: String = input.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Collapses every run of 6+ consecutive ASCII digits into a length-only
/// marker (`<7 digits>`) instead of letting it reach the log verbatim.
/// 6 sits below a real phone number (7-15 digits) and above a SIP extension
/// on this project's own test PBX (3-5 digits, `CLAUDE.md`'s "PBX de
/// pruebas") - the false positive it costs (an error message that happens
/// to mention a large line/byte count) is cheap; the false negative it
/// exists to avoid - a dialed number surfacing in a caught exception's
/// message, e.g. from `bridge.rs`'s click-to-call path or `sidecar_dial` -
/// isn't. Deliberately duplicated (in spirit) by
/// `ui/js/error-reporting.js`'s own transport caps: each side of this IPC
/// boundary treats the other as untrusted for call content, so neither
/// skips its own pass on the assumption the other side already did it.
fn redact_digit_runs(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let run_len = i - start;
            if run_len >= 6 {
                out.push_str(&format!("<{run_len} digits>"));
            } else {
                out.extend(&chars[start..i]);
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn sanitize(raw: &str, max_chars: usize) -> String {
    truncate_chars(&redact_digit_runs(raw), max_chars)
}

/// Pure formatting, kept separate from the `#[tauri::command]` below so it
/// can be unit-tested without a running Tauri app (see the `tests` module).
pub fn format_log_line(report: &FrontendErrorReport) -> String {
    let message = sanitize(&report.message, MAX_MESSAGE_LEN);
    let mut line = format!("frontend {}: {}", report.kind, message);

    if let Some(source) = &report.source {
        if !source.is_empty() {
            line.push_str(&format!(" (at {source}"));
            if let Some(l) = report.line {
                line.push_str(&format!(":{l}"));
                if let Some(c) = report.col {
                    line.push_str(&format!(":{c}"));
                }
            }
            line.push(')');
        }
    }

    if let Some(stack) = &report.stack {
        if !stack.is_empty() {
            line.push('\n');
            line.push_str(&sanitize(stack, MAX_STACK_LEN));
        }
    }

    line
}

/// Frontend-initiated log write - `ui/js/error-reporting.js`'s
/// `reportFrontendIssue` (and `ui/index.html`'s inline pre-boot capture
/// script) are the only callers, via `invoke`. Never panics and always
/// succeeds from the caller's point of view: a broken reporting path must
/// not become a *second* source of failure for a frontend that's already in
/// trouble (see those JS files' own "must not break boot" notes).
#[tauri::command(rename_all = "snake_case")]
pub fn log_frontend_error(report: FrontendErrorReport) {
    let line = format_log_line(&report);
    match report.level.as_str() {
        "warn" => log::warn!(target: "app_lib::frontend", "{line}"),
        "info" => log::info!(target: "app_lib::frontend", "{line}"),
        _ => log::error!(target: "app_lib::frontend", "{line}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(kind: &str, level: &str, message: &str) -> FrontendErrorReport {
        FrontendErrorReport {
            kind: kind.to_string(),
            level: level.to_string(),
            message: message.to_string(),
            source: None,
            line: None,
            col: None,
            stack: None,
        }
    }

    #[test]
    fn formats_a_plain_message() {
        let r = report("error", "error", "Cannot read properties of undefined");
        assert_eq!(
            format_log_line(&r),
            "frontend error: Cannot read properties of undefined"
        );
    }

    #[test]
    fn includes_source_location_when_present() {
        let mut r = report("error", "error", "boom");
        r.source = Some("js/app.js".to_string());
        r.line = Some(42);
        r.col = Some(7);
        assert_eq!(format_log_line(&r), "frontend error: boom (at js/app.js:42:7)");
    }

    #[test]
    fn source_without_line_col_still_renders() {
        let mut r = report("resource", "error", "SCRIPT failed to load");
        r.source = Some("js/app.js".to_string());
        assert_eq!(
            format_log_line(&r),
            "frontend resource: SCRIPT failed to load (at js/app.js)"
        );
    }

    #[test]
    fn appends_a_sanitized_stack_on_a_new_line() {
        let mut r = report("error", "error", "boom");
        r.stack = Some("at foo (app.js:1:1)".to_string());
        assert_eq!(format_log_line(&r), "frontend error: boom\nat foo (app.js:1:1)");
    }

    #[test]
    fn redacts_long_digit_runs_in_the_message() {
        let r = report("error", "error", "dial failed for 18095551234");
        assert_eq!(format_log_line(&r), "frontend error: dial failed for <11 digits>");
    }

    #[test]
    fn keeps_short_digit_runs_like_line_numbers_untouched() {
        let r = report("error", "error", "error at position 4210");
        assert_eq!(format_log_line(&r), "frontend error: error at position 4210");
    }

    #[test]
    fn redacts_digit_runs_inside_the_stack_too() {
        let mut r = report("error", "error", "boom");
        r.stack = Some("caller 18095551234 at app.js:1:1".to_string());
        assert_eq!(
            format_log_line(&r),
            "frontend error: boom\ncaller <11 digits> at app.js:1:1"
        );
    }

    #[test]
    fn truncates_an_overlong_message() {
        let long = "x".repeat(MAX_MESSAGE_LEN + 50);
        let r = report("error", "error", &long);
        let line = format_log_line(&r);
        assert!(line.starts_with("frontend error: "));
        assert!(line.ends_with('…'));
        assert_eq!(
            line.chars().count(),
            "frontend error: ".chars().count() + MAX_MESSAGE_LEN + 1
        );
    }

    #[test]
    fn truncates_an_overlong_stack_independently_of_the_message_cap() {
        let mut r = report("error", "error", "boom");
        r.stack = Some("y".repeat(MAX_STACK_LEN + 20));
        let line = format_log_line(&r);
        let stack_part = line.split('\n').nth(1).unwrap();
        assert_eq!(stack_part.chars().count(), MAX_STACK_LEN + 1);
        assert!(stack_part.ends_with('…'));
    }

    #[test]
    fn empty_source_and_stack_are_omitted_not_rendered_as_empty_parens() {
        let mut r = report("error", "error", "boom");
        r.source = Some(String::new());
        r.stack = Some(String::new());
        assert_eq!(format_log_line(&r), "frontend error: boom");
    }

    #[test]
    fn unrecognized_level_falls_back_to_error_without_panicking() {
        // Can't assert which log:: macro fired from a unit test, but this
        // proves log_frontend_error doesn't panic on a level string outside
        // {"warn","info","error"} - e.g. a future JS-side typo.
        let r = report("weird", "trace", "boom");
        log_frontend_error(r);
    }
}

//! Click-to-call localhost bridge - ported from v1's Electron bridge
//! (`src/main/main.js` "Click-to-call localhost bridge") so the v1 Chrome
//! extension in `extension/` at the repo root keeps working against this
//! shell **unchanged**: same port, same `X-Centinelo-Token` header, same
//! `GET /ping` + `POST /dial` (JSON body `{"number":"..."}`), same CORS
//! headers - including `Access-Control-Allow-Private-Network: true`, which
//! Chrome's Private Network Access preflight requires before a public-ish
//! page origin is allowed to `fetch()` a `127.0.0.1` listener at all (see
//! `extension/sw.js`).
//!
//! Two deliberate, additive differences from v1, both backward compatible
//! with the unchanged extension:
//!
//! 1. `number` is also accepted as a query-string parameter (in addition to
//!    the JSON-body form the extension actually sends) - pure convenience
//!    for manual `curl`-based verification; the extension itself never uses
//!    this path and is unaffected either way. `token` deliberately is
//!    **not** accepted this way (RISK 4R finding, 2026-08-16): a bearer
//!    secret in a query string ends up in shell history, proxy/browser
//!    logs, and this server's own request line if it's ever logged - and
//!    with `bridge.auto_dial` on, a leaked token dials numbers with no
//!    confirmation. Header-only, matching what the extension already sends
//!    and what `shell/E2E.md`'s actual curl transcripts already use.
//! 2. A dial request no longer dials silently - it always goes through the
//!    same "call this number?" confirmation the frontend also uses for
//!    favorites and centinelo:// or tel: deep links (see deeplink.rs), unless
//!    `settings.bridge.auto_dial` is on. v1 always dialed immediately; this
//!    shell's default is the safer one. See ui/js/app.js's "click-to-call"
//!    event handling.
//!
//! Runs on its own OS thread - `tiny_http` is a small, blocking/synchronous
//! HTTP server, matching this codebase's existing thread-per-subsystem
//! style (sidecar.rs's supervisor/stdout-reader/stderr-drain threads)
//! rather than pulling in an async runtime for one localhost listener.

use crate::settings::SettingsStore;
use crate::sidecar::SidecarHandle;
use crate::tray;
use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tiny_http::{Header, Method, Response, Server};

pub const BRIDGE_PORT: u16 = 38911;
pub const EVENT_CLICK_TO_CALL: &str = "click-to-call";
const MAX_BODY_BYTES: u64 = 4096;

/// Starts the bridge server on its own thread. Best-effort: if the port is
/// already taken (most likely a second app instance briefly racing before
/// the single-instance plugin hands control back to the first - see
/// lib.rs), this logs and gives up rather than taking the whole app down.
/// The bridge is a convenience feature; calling/registering/BLF are not.
pub fn start(app: AppHandle, settings: Arc<SettingsStore>, sidecar: SidecarHandle) {
    std::thread::spawn(move || {
        let server = match Server::http(("127.0.0.1", BRIDGE_PORT)) {
            Ok(s) => s,
            Err(e) => {
                log::warn!(
                    "click-to-call bridge: couldn't bind 127.0.0.1:{BRIDGE_PORT} ({e}) - bridge disabled for this run"
                );
                return;
            }
        };
        log::info!("click-to-call bridge: listening on 127.0.0.1:{BRIDGE_PORT}");
        for request in server.incoming_requests() {
            handle_request(request, &app, &settings, &sidecar);
        }
    });
}

fn hdr(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static header name/value is valid ASCII")
}

fn add_cors(resp: &mut Response<std::io::Cursor<Vec<u8>>>) {
    resp.add_header(hdr("Access-Control-Allow-Origin", "*")); // localhost-only listener; matches v1
    resp.add_header(hdr("Access-Control-Allow-Headers", "Content-Type, X-Centinelo-Token"));
    resp.add_header(hdr("Access-Control-Allow-Methods", "POST, GET, OPTIONS"));
    resp.add_header(hdr("Access-Control-Allow-Private-Network", "true")); // Chrome PNA preflight
}

fn json_response(status: u16, body: serde_json::Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut resp = Response::from_string(body.to_string()).with_status_code(status);
    resp.add_header(hdr("Content-Type", "application/json"));
    add_cors(&mut resp);
    resp
}

fn empty_response(status: u16) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut resp = Response::from_string(String::new()).with_status_code(status);
    add_cors(&mut resp);
    resp
}

/// `a=b&c=d` (no leading `?`) -> percent-decoded map.
fn parse_query(query: &str) -> HashMap<String, String> {
    form_urlencoded::parse(query.as_bytes()).into_owned().collect()
}

fn split_url(url: &str) -> (&str, &str) {
    match url.split_once('?') {
        Some((path, query)) => (path, query),
        None => (url, ""),
    }
}

/// Resolves the bearer token to authenticate a request with: the
/// `X-Centinelo-Token` header, and *only* that header. `query_params` is
/// still taken as a parameter (rather than dropped entirely) so the
/// omission is visible at the call site and this exact behavior is
/// unit-testable - see this module's doc comment ("Two deliberate,
/// additive differences from v1", point 1) for why a `token` query
/// parameter is deliberately never consulted here even though `number`
/// still is: a bearer secret in a URL ends up in shell history and
/// proxy/browser logs, and with `bridge.auto_dial` on, a leaked token
/// dials with no confirmation. RISK 4R finding, 2026-08-16.
fn extract_bridge_token(header_token: &str, _query_params: &HashMap<String, String>) -> String {
    header_token.to_string()
}

/// Constant-time token compare - the token is a bearer secret shared only
/// with the paired browser extension's synced storage; no reason to leak
/// timing information about how many leading bytes matched.
fn tokens_match(provided: &str, expected: &str) -> bool {
    if provided.is_empty() || expected.is_empty() || provided.len() != expected.len() {
        return false;
    }
    provided
        .bytes()
        .zip(expected.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// Same cleaning rule as v1's bridge handler and the extension's own
/// content script: keep digits and dial-control characters, drop everything
/// else (formatting, spaces, parens, ...).
fn clean_number(raw: &str) -> String {
    raw.chars().filter(|c| c.is_ascii_digit() || matches!(c, '+' | '*' | '#')).collect()
}

/// Non-reversible fingerprint for a dialed number - HIPAA remote-install
/// hardening (2026-08-13, RISK 4R finding on PR #20): a dialed number is a
/// caller's phone number, PHI in the Neola Dental deployment this shell
/// ships to, and must never reach the persistent on-disk log (see lib.rs's
/// `is_call_content_log_target` doc). Unlike the sidecar/transcription
/// module-wide exclusion, this call site's own log line is still genuinely
/// useful for diagnosing the bridge/deep-link path itself (is a request
/// even arriving? is the same number retried repeatedly? is a malformed
/// link producing a 0-digit clean()?) on a remote Windows machine nobody is
/// sitting in front of - so redact at the point of emission instead of
/// dropping the whole module from the log file. `DefaultHasher` is *not*
/// cryptographic and isn't meant to be here - the fix this closes is
/// "plaintext PHI written to disk", not "information-theoretic secrecy";
/// two log lines for the same number producing the same fingerprint is the
/// whole point (it lets support see "this number again" without the digits
/// ever touching disk).
pub(crate) fn number_fingerprint(number: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    number.hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

/// `"<N digits>#<fingerprint>"` - safe to write to the persistent log in
/// place of a raw dialed number. `N` alone already answers most bridge/
/// deep-link bug reports ("a 0-digit number means clean()/extraction found
/// nothing"); the fingerprint answers "is this the same number as that
/// other log line" without ever writing the number itself.
pub(crate) fn redacted_log_number(number: &str) -> String {
    format!("{} digits#{}", number.chars().count(), number_fingerprint(number))
}

fn handle_request(
    mut request: tiny_http::Request,
    app: &AppHandle,
    settings: &Arc<SettingsStore>,
    sidecar: &SidecarHandle,
) {
    // `request.respond()` consumes `self`, so everything needed from it has
    // to be pulled out as owned data up front.
    let url = request.url().to_string();
    let (path, query) = split_url(&url);
    let path = path.to_string();
    let query_params = parse_query(query);
    let method = request.method().clone();
    let header_token = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("X-Centinelo-Token"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();

    if method == Method::Options {
        // Matches v1 exactly: CORS preflight never needs the token.
        let _ = request.respond(empty_response(204));
        return;
    }

    let mut body = String::new();
    if method == Method::Post {
        let _ = request.as_reader().take(MAX_BODY_BYTES).read_to_string(&mut body);
    }

    let expected_token = settings.snapshot().bridge.token;
    let provided_token = extract_bridge_token(&header_token, &query_params);
    if !tokens_match(&provided_token, &expected_token) {
        let _ = request.respond(json_response(403, serde_json::json!({"error": "bad token"})));
        return;
    }

    match (method, path.as_str()) {
        (Method::Get, "/ping") => {
            let payload = serde_json::json!({
                "app": "centinelo-phone",
                "state": sidecar.ping_state(),
            });
            let _ = request.respond(json_response(200, payload));
        }
        (Method::Post, "/dial") => {
            let number_from_body = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("number").and_then(|n| n.as_str()).map(str::to_string));
            let raw_number = query_params.get("number").cloned().or(number_from_body).unwrap_or_default();
            let clean = clean_number(&raw_number);
            if clean.len() < 2 {
                let _ = request.respond(json_response(400, serde_json::json!({"error": "bad request"})));
                return;
            }
            let auto_dial = settings.snapshot().bridge.auto_dial;
            // Redacted - `clean` is the dialed number (PHI), see
            // `redacted_log_number`'s doc.
            log::info!(
                "click-to-call bridge: /dial request for {} (auto_dial={auto_dial}) - {}",
                redacted_log_number(&clean),
                if auto_dial { "dialing immediately" } else { "asking for confirmation" }
            );
            let _ = app.emit(
                EVENT_CLICK_TO_CALL,
                serde_json::json!({
                    "number": clean,
                    "source": "bridge",
                    "auto_dial": auto_dial,
                }),
            );
            tray::show_and_focus(app);
            let _ = request.respond(json_response(200, serde_json::json!({"ok": true})));
        }
        _ => {
            let _ = request.respond(json_response(404, serde_json::json!({"error": "not found"})));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_match_equal() {
        assert!(tokens_match("abc123", "abc123"));
    }

    #[test]
    fn tokens_match_rejects_mismatch() {
        assert!(!tokens_match("abc123", "abc124"));
    }

    #[test]
    fn tokens_match_rejects_empty() {
        assert!(!tokens_match("", ""));
        assert!(!tokens_match("abc", ""));
        assert!(!tokens_match("", "abc"));
    }

    #[test]
    fn tokens_match_rejects_length_mismatch() {
        assert!(!tokens_match("abc", "abcd"));
    }

    #[test]
    fn clean_number_strips_formatting() {
        assert_eq!(clean_number("(352) 555-0199"), "3525550199");
    }

    #[test]
    fn clean_number_keeps_dial_controls() {
        assert_eq!(clean_number("+1*43#"), "+1*43#");
    }

    #[test]
    fn split_url_with_query() {
        assert_eq!(split_url("/dial?number=501&token=abc"), ("/dial", "number=501&token=abc"));
    }

    #[test]
    fn split_url_without_query() {
        assert_eq!(split_url("/ping"), ("/ping", ""));
    }

    #[test]
    fn parse_query_decodes() {
        let q = parse_query("number=%2A43&token=abc%20def");
        assert_eq!(q.get("number").map(String::as_str), Some("*43"));
        assert_eq!(q.get("token").map(String::as_str), Some("abc def"));
    }

    // RISK 4R finding (2026-08-16): a bearer token in a query string ends
    // up in shell history / proxy logs, unlike a header. These pin the
    // actual guarantee - a query-string `token` must never authenticate a
    // request, no matter what the header is - not just that the function
    // returns *some* string.

    #[test]
    fn extract_bridge_token_ignores_a_token_in_the_query_string() {
        let mut q = HashMap::new();
        q.insert("token".to_string(), "leaked-in-the-url".to_string());
        // No header sent at all - if the query string were still a
        // fallback, this would resolve to "leaked-in-the-url".
        assert_eq!(extract_bridge_token("", &q), "");
    }

    #[test]
    fn extract_bridge_token_uses_the_header_when_present() {
        let q = HashMap::new();
        assert_eq!(extract_bridge_token("abc123", &q), "abc123");
    }

    #[test]
    fn extract_bridge_token_prefers_the_header_even_if_query_also_has_one() {
        let mut q = HashMap::new();
        q.insert("token".to_string(), "wrong-leaked-token".to_string());
        assert_eq!(extract_bridge_token("the-real-header-token", &q), "the-real-header-token");
    }

    // HIPAA remote-install hardening (2026-08-13) - `redacted_log_number`
    // is what stands between a caller's phone number and the persistent
    // on-disk log for every /dial request. These pin the actual guarantee
    // (RELIABILITY's fix-pass note: prove the behavior, not just the
    // predicate) rather than just exercising the function.

    #[test]
    fn redacted_log_number_never_contains_the_digits() {
        let number = "3525550199";
        let redacted = redacted_log_number(number);
        assert!(!redacted.contains(number), "redacted form leaked the raw number: {redacted}");
        // ...nor any digit-substring long enough to be re-identifying on
        // its own (a phone number's own area code, say).
        for window in number.as_bytes().windows(4) {
            let s = std::str::from_utf8(window).unwrap();
            assert!(!redacted.contains(s), "redacted form leaked a 4-digit substring ({s}): {redacted}");
        }
    }

    #[test]
    fn redacted_log_number_reports_the_real_digit_count() {
        assert!(redacted_log_number("3525550199").starts_with("10 digits#"));
        assert!(redacted_log_number("*43").starts_with("3 digits#"));
        assert!(redacted_log_number("").starts_with("0 digits#"));
    }

    #[test]
    fn redacted_log_number_is_stable_for_the_same_number() {
        // The whole point of fingerprinting instead of just dropping the
        // line: support can see "this is the same number retrying" across
        // two separate log lines without either one ever carrying PHI.
        assert_eq!(redacted_log_number("3525550199"), redacted_log_number("3525550199"));
    }

    #[test]
    fn redacted_log_number_differs_for_different_numbers() {
        assert_ne!(redacted_log_number("3525550199"), redacted_log_number("3525550198"));
    }
}

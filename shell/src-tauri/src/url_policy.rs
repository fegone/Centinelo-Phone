//! Shared "HTTPS-or-localhost" URL policy.
//!
//! One rule, used by (1) license-activation server URLs and (2) remote-STT
//! backend URLs: `https://` is always accepted; `http://127.0.0.1` /
//! `http://localhost` (any port, any path) is accepted for local testing;
//! everything else is rejected *before* this shell ever sends a request to
//! the typed host. See SPEC-2026-07-17-remote-stt-design.md §6 ("TLS policy")
//! and the activation-server spec §5.3.
//!
//! Extracted here (2026-07-17, P6) rather than reimplemented a third time:
//! `activation::validate_server_url` owned the rule, the remote-STT settings
//! needed the same rule, and a third copy would have drifted. This module
//! returns a plain `Result<String, String>` (the normalized URL / a short
//! human message) so neither caller is forced to import the other's error
//! enum — `activation::validate_server_url` keeps its typed
//! [`ActivationError`](crate::activation::ActivationError) wrapper (and its
//! existing tests still pass) by mapping the string through.

/// Normalizes and validates a server base URL under the HTTPS-or-localhost
/// policy.
///
/// Trims surrounding whitespace (an admin pasting a URL commonly leaves some)
/// and strips a trailing `/` so `format!("{base}/path")` never produces a
/// double slash. Empty input is rejected.
pub fn validate_https_or_localhost(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty".to_string());
    }
    let url = url::Url::parse(trimmed).map_err(|_| "not a valid URL".to_string())?;
    let host_is_local = matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"));
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && host_is_local);
    if !scheme_ok {
        return Err(
            "must be https://, or http://127.0.0.1 / http://localhost for local testing".to_string(),
        );
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_accepted_and_trailing_slash_trimmed() {
        assert_eq!(
            validate_https_or_localhost("https://stt.example.test/").unwrap(),
            "https://stt.example.test"
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            validate_https_or_localhost("  https://stt.example.test  ").unwrap(),
            "https://stt.example.test"
        );
    }

    #[test]
    fn http_localhost_any_port_accepted() {
        assert!(validate_https_or_localhost("http://localhost:8720").is_ok());
        assert!(validate_https_or_localhost("http://127.0.0.1:8720").is_ok());
    }

    #[test]
    fn plain_http_remote_rejected() {
        assert!(validate_https_or_localhost("http://stt.example.test").is_err());
    }

    #[test]
    fn empty_rejected() {
        assert!(validate_https_or_localhost("").is_err());
        assert!(validate_https_or_localhost("   ").is_err());
    }

    #[test]
    fn garbage_rejected() {
        assert!(validate_https_or_localhost("not a url at all").is_err());
    }

    #[test]
    fn other_scheme_rejected() {
        assert!(validate_https_or_localhost("ftp://stt.example.test").is_err());
    }
}

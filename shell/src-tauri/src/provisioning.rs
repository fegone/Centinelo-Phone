//! Auto-provisioning (spec §5): "paste URL or scan QR -> fetch config JSON
//! -> registered in ~30s". Answers Edgar's "easy for whoever installs it" -
//! a front-desk operator shouldn't have to hand-type a host/extension/
//! secret from a sticky note.
//!
//! ## Two ways in, one flow after that
//!
//! 1. **Manual paste** (`commands::provisioning_resolve`): the operator
//!    pastes a link into the onboarding field (see `ui/index.html`
//!    `#prov-input`, modeled on `premium/design/mockups/onboarding.html`'s
//!    "Step 1 of 2 - Connect").
//! 2. **Deep link** (`handle_deep_link`, wired from `deeplink.rs`): a
//!    `centinelo://provision?...` link arrives the same way `tel:`/
//!    `centinelo://<number>` dial links already do (OS protocol handler or
//!    a second-instance launch - see `deeplink.rs`'s own module doc for the
//!    macOS/Windows/Linux platform split, unchanged by this feature).
//!
//! Both paths funnel into [`resolve_input`], which never touches settings
//! by itself - it only parses, optionally fetches, and validates, handing
//! back a [`ProvisioningConfig`]. The **secret never round-trips to the
//! frontend**: [`ProvisioningPending`] stashes the resolved config
//! server-side; the frontend only ever sees a secret-free
//! [`ProvisioningPreviewView`] (host/ext/display_name/transport, plus
//! whether a TLS pin was included - never the pin value or the secret) to
//! show a "connect to this?" confirmation, matching the mockup's "Treat it
//! like a password" whisper line. `commands::provisioning_apply` is a
//! separate IPC call that commits whatever's currently pending - see that
//! command's doc for the admin-lock rule (first provisioning on a clean
//! install is unlocked by definition; a re-provision of an already-
//! configured install requires admin unlock like every other account
//! mutation).
//!
//! ## The link forms this module accepts
//!
//! See `shell/PROVISIONING.md` for the full schema + worked examples this
//! doc intentionally doesn't duplicate. Short version:
//!
//! - `https://<installer-host>/<path>` - the common case, a bare link an
//!   installer's provisioning page hands out. Fetched with a plain GET;
//!   the response body must be the JSON config directly.
//! - `centinelo://provision?url=<percent-encoded https url>` - same fetch,
//!   reached via the OS protocol handler instead of a paste.
//! - `centinelo://provision?config=<base64url, no padding, JSON>` - the
//!   config embedded directly in the link, no network fetch at all. Exists
//!   so a future QR code (out of scope for this task - see
//!   shell/PROVISIONING.md "QR") can encode a link that works fully
//!   offline, and so this module's happy path is unit-testable without a
//!   live server.
//!
//! `http://` (unencrypted) is rejected outright, for both the pasted link
//! itself and a `url=` fetch target - the response contains a SIP secret
//! in plaintext, which a plain HTTP fetch would put on the wire in the
//! clear the very first time it's used.
//!
//! ## Why the config format has no file-path fields
//!
//! The obvious way to express "trust this CA" would be a path to a PEM
//! file. This config comes from a URL the operator pastes, or from a deep
//! link reachable from *outside* the app entirely (e.g. a crafted link in
//! an email or webpage); accepting a filesystem path from that source and
//! having the shell later read it would be a path-traversal/arbitrary-
//! file-read primitive for zero benefit (`commands::reveal_in_file_manager`'s
//! doc describes this exact threat model for a Tauri command's own input -
//! a remote-sourced config deserves at least as much suspicion).
//! `tls_pin_sha256` sidesteps this entirely: it's an inline hex fingerprint
//! string, not a path, and maps straight onto `core/PROTOCOL.md`'s
//! existing `CENT_TLS_PIN` env var (see sidecar.rs). A full custom-CA
//! field isn't implemented for the same reason `core` doesn't support one
//! yet (single-pin only, see `core/PROTOCOL.md`'s TLS section); adding a
//! config field the engine can't act on would be a silent no-op, not a
//! real feature (see shell/PROVISIONING.md "Not supported yet").

use crate::settings::{AccountSettings, TransportPriority};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

pub const EVENT_PREVIEW: &str = "provisioning://preview";
pub const EVENT_ERROR: &str = "provisioning://error";

const CENTINELO_SCHEME: &str = "centinelo";
const PROVISION_HOST: &str = "provision";

/// Caps both the embedded (`config=`) and fetched (`url=`/pasted https)
/// forms - generous for a JSON config (a few hundred bytes in practice),
/// stingy enough that a misbehaving/malicious server streaming an
/// unbounded response can't grow this process's memory unbounded either.
const MAX_CONFIG_BYTES: usize = 16 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);

const MAX_HOST_LEN: usize = 253; // RFC 1035 full-name limit
const MAX_EXT_LEN: usize = 64;
const MAX_SECRET_LEN: usize = 256;
const MAX_DISPLAY_NAME_LEN: usize = 128;

// ---------------------------------------------------------------------------
// Config shape
// ---------------------------------------------------------------------------

/// The provisioning config JSON, resolved but not yet validated. See
/// `shell/PROVISIONING.md` for the wire schema and worked examples.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisioningConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    pub host: String,
    pub ext: String,
    pub secret: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub transport_priority: TransportPriority,
    /// Hex SHA-256 leaf-cert fingerprint (colons optional) - see this
    /// module's doc, "no file-path fields", for why this is the only TLS
    /// option and why it's a string, never a path.
    #[serde(default)]
    pub tls_pin_sha256: Option<String>,
}

fn default_version() -> u32 {
    1
}

impl From<ProvisioningConfig> for AccountSettings {
    fn from(c: ProvisioningConfig) -> Self {
        AccountSettings {
            host: c.host.trim().to_string(),
            ext: c.ext.trim().to_string(),
            secret: c.secret,
            display_name: c.display_name.trim().to_string(),
            transport_priority: c.transport_priority,
            tls_pin_sha256: c.tls_pin_sha256,
        }
    }
}

/// What the frontend's confirmation screen actually gets - deliberately
/// missing `secret` and the raw `tls_pin_sha256` value (see module doc,
/// "the secret never round-trips to the frontend"). `has_tls_pin` is
/// enough for the confirmation copy ("TLS pin included") without handing
/// back a value that's only useful for impersonating the pin itself.
#[derive(Debug, Clone, Serialize)]
pub struct ProvisioningPreviewView {
    pub host: String,
    pub ext: String,
    pub display_name: String,
    pub transport_priority: TransportPriority,
    pub has_tls_pin: bool,
}

impl From<&ProvisioningConfig> for ProvisioningPreviewView {
    fn from(c: &ProvisioningConfig) -> Self {
        Self {
            host: c.host.clone(),
            ext: c.ext.clone(),
            display_name: c.display_name.clone(),
            transport_priority: c.transport_priority,
            has_tls_pin: c.tls_pin_sha256.as_deref().is_some_and(|p| !p.is_empty()),
        }
    }
}

// ---------------------------------------------------------------------------
// Pending state (bridges provisioning_resolve -> provisioning_apply)
// ---------------------------------------------------------------------------

/// Session-only holding pen for a resolved-but-not-yet-applied config.
/// Managed as Tauri state (`app.manage(ProvisioningPending::default())`,
/// lib.rs), mirroring `settings::AdminSession`'s "in-memory only, a fresh
/// launch starts empty" shape. A second resolve (paste or deep link)
/// simply overwrites whatever was pending - there's only ever one
/// confirmation screen visible at a time in this UI.
#[derive(Default)]
pub struct ProvisioningPending {
    inner: Mutex<Option<ProvisioningConfig>>,
}

impl ProvisioningPending {
    pub fn set(&self, config: ProvisioningConfig) {
        *self.inner.lock().expect("provisioning-pending mutex poisoned") = Some(config);
    }

    /// Consumes the pending config (used by `provisioning_apply` - a
    /// config is applied at most once, then it's gone).
    pub fn take(&self) -> Option<ProvisioningConfig> {
        self.inner.lock().expect("provisioning-pending mutex poisoned").take()
    }

    pub fn clear(&self) {
        *self.inner.lock().expect("provisioning-pending mutex poisoned") = None;
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum ProvisioningSource {
    Embedded(ProvisioningConfig),
    Remote(url::Url),
}

/// Parses + (if remote) fetches + validates a pasted link or deep-link URL
/// in one call - the single entry point both `commands::provisioning_resolve`
/// and [`handle_deep_link`] use, so a pasted link and a clicked link are
/// verified identically.
pub fn resolve_input(input: &str) -> Result<ProvisioningConfig, String> {
    let source = parse_input(input)?;
    let config = match source {
        ProvisioningSource::Embedded(c) => c,
        ProvisioningSource::Remote(url) => fetch_remote(&url)?,
    };
    validate(&config)?;
    Ok(config)
}

fn parse_input(input: &str) -> Result<ProvisioningSource, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Paste a provisioning link first.".to_string());
    }
    let url = url::Url::parse(trimmed).map_err(|_| "That doesn't look like a valid link.".to_string())?;
    match url.scheme() {
        "https" => Ok(ProvisioningSource::Remote(url)),
        "http" => Err(
            "Provisioning links must use https, not http - the config contains your account's \
             password."
                .to_string(),
        ),
        CENTINELO_SCHEME if url.host_str() == Some(PROVISION_HOST) => parse_centinelo_provision(&url),
        CENTINELO_SCHEME => Err("That centinelo: link isn't a provisioning link.".to_string()),
        other => Err(format!(
            "Unsupported link type ({other}://) - use an https:// provisioning link."
        )),
    }
}

fn parse_centinelo_provision(url: &url::Url) -> Result<ProvisioningSource, String> {
    // `into_owned()` up front - `url` itself is borrowed and this closure
    // otherwise can't return owned Strings past the pairs iterator's
    // lifetime.
    let pairs: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();

    if let Some(encoded) = pairs.get("config") {
        return decode_embedded_config(encoded).map(ProvisioningSource::Embedded);
    }
    if let Some(target) = pairs.get("url") {
        let target_url =
            url::Url::parse(target).map_err(|_| "The embedded provisioning URL is malformed.".to_string())?;
        if target_url.scheme() != "https" {
            return Err("The embedded provisioning URL must use https.".to_string());
        }
        return Ok(ProvisioningSource::Remote(target_url));
    }
    Err("This provisioning link is missing its \"config\" or \"url\" parameter.".to_string())
}

fn decode_embedded_config(encoded: &str) -> Result<ProvisioningConfig, String> {
    use base64::Engine;
    // URL_SAFE_NO_PAD: the config travels inside a URL query parameter
    // (already itself percent-decoded by `query_pairs()` above) - standard
    // base64's `+`/`/`/`=` would need re-escaping there for no benefit.
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "The embedded provisioning data is corrupted (bad base64).".to_string())?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err("The embedded provisioning data is too large.".to_string());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("The embedded provisioning data isn't valid: {e}"))
}

// ---------------------------------------------------------------------------
// Fetch
// ---------------------------------------------------------------------------

/// Reads at most `cap + 1` bytes from `reader`, erroring if that many were
/// actually available (i.e. the real body exceeds `cap`) - extracted as a
/// pure function over any [`Read`] so it's unit-testable with an in-memory
/// `Cursor` (see tests below) without a live HTTP server, unlike
/// [`fetch_remote`] itself.
fn read_capped_body(mut reader: impl Read, cap: usize) -> Result<Vec<u8>, String> {
    let mut buf = Vec::with_capacity(cap.min(4096));
    reader
        .by_ref()
        .take((cap as u64) + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("Couldn't read the provisioning response: {e}"))?;
    if buf.len() > cap {
        return Err("The provisioning server's response is too large.".to_string());
    }
    Ok(buf)
}

fn fetch_remote(url: &url::Url) -> Result<ProvisioningConfig, String> {
    if url.scheme() != "https" {
        // Defense in depth - every caller into this function has already
        // enforced https at parse time (parse_input/parse_centinelo_provision),
        // but this function isn't otherwise `pub(self)`-locked to that
        // invariant holding forever.
        return Err("Provisioning links must use https.".to_string());
    }
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout(FETCH_TIMEOUT)
        // No automatic redirect following: a redirect to an unexpected
        // host/scheme should be a visible error the operator can inspect
        // (re-paste the final link), not a silent extra hop this shell
        // decided on their behalf.
        .redirects(0)
        .build();

    let response = match agent.get(url.as_str()).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => {
            return Err(format!("The provisioning server returned HTTP {code}."));
        }
        Err(ureq::Error::Transport(e)) => {
            return Err(format!("Couldn't reach the provisioning server: {e}"));
        }
    };

    let body = read_capped_body(response.into_reader(), MAX_CONFIG_BYTES)?;
    let text = String::from_utf8(body).map_err(|_| "The provisioning response isn't valid UTF-8 text.".to_string())?;
    serde_json::from_str(&text).map_err(|e| format!("The provisioning response isn't a valid config: {e}"))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// The injection-relevant boundary: `sidecar.rs`'s `write_accounts_file`
/// interpolates `host`/`ext`/`secret` unquoted, unescaped, straight into a
/// single-line baresip accounts entry (`<sip:{ext}@{host}:{port};...>` +
/// `;auth_pass={secret};...`) - a `;` inside `secret` would prematurely
/// close `auth_pass=` and let the rest of the string inject arbitrary
/// account params; a newline anywhere would inject an entirely separate
/// account line. This validation exists specifically because provisioning
/// config is more exposed than the existing manual Settings form: it can
/// come from a URL an operator merely pasted (or, for the `url=` deep-link
/// form, a link someone else sent them) rather than something they typed
/// themselves field-by-field.
fn validate(config: &ProvisioningConfig) -> Result<(), String> {
    let host = config.host.trim();
    if host.is_empty() {
        return Err("The provisioning config is missing \"host\".".to_string());
    }
    if host.chars().count() > MAX_HOST_LEN {
        return Err("\"host\" in the provisioning config is too long.".to_string());
    }
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
    {
        return Err(
            "\"host\" in the provisioning config contains characters that aren't allowed in a hostname or IP address."
                .to_string(),
        );
    }

    let ext = config.ext.trim();
    if ext.is_empty() {
        return Err("The provisioning config is missing \"ext\".".to_string());
    }
    if ext.chars().count() > MAX_EXT_LEN {
        return Err("\"ext\" in the provisioning config is too long.".to_string());
    }
    if !ext
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '*' | '#' | '+'))
    {
        return Err("\"ext\" in the provisioning config contains characters that aren't allowed in an extension.".to_string());
    }

    if config.secret.is_empty() {
        return Err("The provisioning config is missing \"secret\".".to_string());
    }
    if config.secret.chars().count() > MAX_SECRET_LEN {
        return Err("\"secret\" in the provisioning config is too long.".to_string());
    }
    if config.secret.chars().any(|c| c.is_control() || c == ';') {
        return Err(
            "\"secret\" in the provisioning config contains characters that aren't allowed (control characters or \";\")."
                .to_string(),
        );
    }

    if config.display_name.chars().count() > MAX_DISPLAY_NAME_LEN {
        return Err("\"display_name\" in the provisioning config is too long.".to_string());
    }
    if config.display_name.chars().any(|c| c.is_control()) {
        return Err("\"display_name\" in the provisioning config contains control characters.".to_string());
    }

    if let Some(pin) = &config.tls_pin_sha256 {
        let cleaned: String = pin.chars().filter(|c| *c != ':').collect();
        if !cleaned.is_empty() && (cleaned.len() != 64 || !cleaned.chars().all(|c| c.is_ascii_hexdigit())) {
            return Err(
                "\"tls_pin_sha256\" in the provisioning config must be a 32-byte SHA-256 fingerprint (64 hex characters, colons optional)."
                    .to_string(),
            );
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Deep link entry point (wired from deeplink.rs)
// ---------------------------------------------------------------------------

/// Called from `deeplink.rs`'s `handle_url` for a `centinelo://provision`
/// link - the OS-launch/second-instance path, not the paste-a-link
/// onboarding field. Runs the resolve (which may block on a network fetch)
/// on its own thread so it never stalls the deep-link callback itself,
/// then emits the same secret-free preview the paste flow's
/// `commands::provisioning_resolve` returns directly, so
/// `ui/js/app.js`'s confirmation screen doesn't need to know which path a
/// given confirmation came from - see that file's
/// `showProvisioningConfirm`.
pub fn handle_deep_link(app: AppHandle, url: url::Url) {
    std::thread::spawn(move || {
        match resolve_input(url.as_str()) {
            Ok(config) => {
                let preview = ProvisioningPreviewView::from(&config);
                if let Some(pending) = app.try_state::<ProvisioningPending>() {
                    pending.set(config);
                } else {
                    // Shouldn't happen (lib.rs manages this before
                    // deeplink::setup runs) - fails safe by not emitting a
                    // preview the "Connect" button couldn't actually apply.
                    log::error!("provisioning: deep link resolved but ProvisioningPending isn't managed yet");
                    let _ = app.emit(
                        EVENT_ERROR,
                        serde_json::json!({ "message": "Internal error - try pasting the link into Settings instead." }),
                    );
                    return;
                }
                log::info!("provisioning: deep link resolved for host={} ext={}", preview.host, preview.ext);
                let _ = app.emit(EVENT_PREVIEW, &preview);
                crate::tray::show_and_focus(&app);
            }
            Err(message) => {
                log::warn!("provisioning: deep link couldn't be resolved: {message}");
                let _ = app.emit(EVENT_ERROR, serde_json::json!({ "message": message }));
                crate::tray::show_and_focus(&app);
            }
        }
    });
}

pub(crate) fn is_provision_link(url: &url::Url) -> bool {
    url.scheme() == CENTINELO_SCHEME && url.host_str() == Some(PROVISION_HOST)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> ProvisioningConfig {
        ProvisioningConfig {
            version: 1,
            host: "pbx.example.test".to_string(),
            ext: "1001".to_string(),
            secret: "s3cret".to_string(),
            display_name: "Front Desk".to_string(),
            transport_priority: TransportPriority::Auto,
            tls_pin_sha256: None,
        }
    }

    fn embed(config: &ProvisioningConfig) -> String {
        use base64::Engine;
        let json = serde_json::to_string(config).unwrap();
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes());
        format!("centinelo://provision?config={encoded}")
    }

    // ---- parse_input --------------------------------------------------

    #[test]
    fn bare_https_link_is_remote() {
        match parse_input("https://provision.example.test/config.json").unwrap() {
            ProvisioningSource::Remote(url) => assert_eq!(url.as_str(), "https://provision.example.test/config.json"),
            ProvisioningSource::Embedded(_) => panic!("expected Remote"),
        }
    }

    #[test]
    fn http_link_rejected() {
        let err = parse_input("http://provision.example.test/config.json").unwrap_err();
        assert!(err.contains("https"), "unexpected message: {err}");
    }

    #[test]
    fn garbage_input_rejected() {
        assert!(parse_input("not a link at all").is_err());
        assert!(parse_input("   ").is_err());
        assert!(parse_input("").is_err());
    }

    #[test]
    fn unsupported_scheme_rejected() {
        let err = parse_input("ftp://example.test/x").unwrap_err();
        assert!(err.contains("ftp"), "unexpected message: {err}");
    }

    #[test]
    fn centinelo_dial_link_is_not_a_provisioning_link() {
        // `centinelo://501` (a plain dial deep link) must never be treated
        // as provisioning input - deeplink.rs routes by host before ever
        // calling into this module, but resolve_input/parse_input stay
        // defensive on their own.
        assert!(parse_input("centinelo://501").is_err());
        assert!(parse_input("centinelo://dial?number=501").is_err());
    }

    #[test]
    fn centinelo_provision_with_url_param_is_remote() {
        match parse_input("centinelo://provision?url=https%3A%2F%2Fprov.example.test%2Fc.json").unwrap() {
            ProvisioningSource::Remote(url) => assert_eq!(url.as_str(), "https://prov.example.test/c.json"),
            ProvisioningSource::Embedded(_) => panic!("expected Remote"),
        }
    }

    #[test]
    fn centinelo_provision_url_param_must_be_https() {
        let err =
            parse_input("centinelo://provision?url=http%3A%2F%2Fprov.example.test%2Fc.json").unwrap_err();
        assert!(err.contains("https"), "unexpected message: {err}");
    }

    #[test]
    fn centinelo_provision_missing_both_params_rejected() {
        let err = parse_input("centinelo://provision?foo=bar").unwrap_err();
        assert!(err.contains("config") || err.contains("url"), "unexpected message: {err}");
    }

    #[test]
    fn centinelo_provision_config_param_round_trips() {
        let cfg = valid_config();
        let link = embed(&cfg);
        match parse_input(&link).unwrap() {
            ProvisioningSource::Embedded(decoded) => assert_eq!(decoded, cfg),
            ProvisioningSource::Remote(_) => panic!("expected Embedded"),
        }
    }

    // ---- decode_embedded_config ----------------------------------------

    #[test]
    fn embedded_config_bad_base64_rejected() {
        assert!(decode_embedded_config("not-valid-base64!!!").is_err());
    }

    #[test]
    fn embedded_config_too_large_rejected() {
        use base64::Engine;
        let huge = "x".repeat(MAX_CONFIG_BYTES + 1);
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(huge.as_bytes());
        let err = decode_embedded_config(&encoded).unwrap_err();
        assert!(err.contains("too large"), "unexpected message: {err}");
    }

    #[test]
    fn embedded_config_not_json_rejected() {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"not json");
        assert!(decode_embedded_config(&encoded).is_err());
    }

    // ---- read_capped_body ------------------------------------------------

    #[test]
    fn read_capped_body_under_cap_ok() {
        let data = b"hello world";
        let out = read_capped_body(std::io::Cursor::new(data), 100).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn read_capped_body_exactly_at_cap_ok() {
        let data = vec![b'x'; 10];
        let out = read_capped_body(std::io::Cursor::new(&data), 10).unwrap();
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn read_capped_body_over_cap_rejected() {
        let data = vec![b'x'; 11];
        let err = read_capped_body(std::io::Cursor::new(&data), 10).unwrap_err();
        assert!(err.contains("too large"), "unexpected message: {err}");
    }

    // ---- validate -------------------------------------------------------

    #[test]
    fn valid_config_passes() {
        assert!(validate(&valid_config()).is_ok());
    }

    #[test]
    fn empty_host_rejected() {
        let mut c = valid_config();
        c.host = "  ".to_string();
        assert!(validate(&c).is_err());
    }

    #[test]
    fn empty_ext_rejected() {
        let mut c = valid_config();
        c.ext = String::new();
        assert!(validate(&c).is_err());
    }

    #[test]
    fn empty_secret_rejected() {
        let mut c = valid_config();
        c.secret = String::new();
        assert!(validate(&c).is_err());
    }

    #[test]
    fn secret_with_semicolon_rejected_account_line_injection() {
        // The actual attack this guards against: a secret designed to
        // break out of `;auth_pass={secret};mediaenc=...` in
        // sidecar.rs's write_accounts_file and inject a bogus extra
        // param.
        let mut c = valid_config();
        c.secret = "x;outbound=\"sip:evil.example.test\"".to_string();
        let err = validate(&c).unwrap_err();
        assert!(err.contains("secret"), "unexpected message: {err}");
    }

    #[test]
    fn secret_with_newline_rejected_account_line_injection() {
        // Guards the other half of the same threat: a newline would
        // inject an entirely separate accounts-file line.
        let mut c = valid_config();
        c.secret = "x\n<sip:evil@evil.example.test>;auth_pass=y".to_string();
        assert!(validate(&c).is_err());
    }

    #[test]
    fn host_with_semicolon_rejected() {
        let mut c = valid_config();
        c.host = "pbx.example.test;evilparam=1".to_string();
        assert!(validate(&c).is_err());
    }

    #[test]
    fn ext_with_at_sign_rejected() {
        let mut c = valid_config();
        c.ext = "1001@evil.example.test".to_string();
        assert!(validate(&c).is_err());
    }

    #[test]
    fn host_too_long_rejected() {
        let mut c = valid_config();
        c.host = "a".repeat(MAX_HOST_LEN + 1);
        assert!(validate(&c).is_err());
    }

    #[test]
    fn display_name_control_char_rejected() {
        let mut c = valid_config();
        c.display_name = "Front\nDesk".to_string();
        assert!(validate(&c).is_err());
    }

    #[test]
    fn tls_pin_valid_hex_with_colons_accepted() {
        let mut c = valid_config();
        c.tls_pin_sha256 = Some(
            "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99"
                .to_string(),
        );
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn tls_pin_wrong_length_rejected() {
        let mut c = valid_config();
        c.tls_pin_sha256 = Some("AABBCC".to_string());
        assert!(validate(&c).is_err());
    }

    #[test]
    fn tls_pin_non_hex_rejected() {
        let mut c = valid_config();
        c.tls_pin_sha256 = Some("z".repeat(64));
        assert!(validate(&c).is_err());
    }

    // ---- resolve_input end-to-end (embedded path, no network needed) ----

    #[test]
    fn resolve_input_embedded_happy_path() {
        let cfg = valid_config();
        let link = embed(&cfg);
        let resolved = resolve_input(&link).unwrap();
        assert_eq!(resolved, cfg);
    }

    #[test]
    fn resolve_input_embedded_invalid_config_rejected_by_validate() {
        let mut cfg = valid_config();
        cfg.secret = String::new(); // fails validate(), not parsing
        let link = embed(&cfg);
        assert!(resolve_input(&link).is_err());
    }

    // ---- ProvisioningConfig -> AccountSettings / preview -----------------

    #[test]
    fn into_account_settings_trims_and_carries_tls_pin() {
        let mut cfg = valid_config();
        cfg.host = "  pbx.example.test  ".to_string();
        cfg.tls_pin_sha256 = Some("AA".repeat(32));
        let account: AccountSettings = cfg.into();
        assert_eq!(account.host, "pbx.example.test");
        assert_eq!(account.tls_pin_sha256.as_deref(), Some("AA".repeat(32).as_str()));
    }

    #[test]
    fn preview_never_carries_secret_or_pin_value() {
        let mut cfg = valid_config();
        cfg.tls_pin_sha256 = Some("AA".repeat(32));
        let preview = ProvisioningPreviewView::from(&cfg);
        // Compile-time guarantee too (ProvisioningPreviewView has no
        // `secret`/`tls_pin_sha256` field at all) - this assertion is
        // just the runtime half: the flag is set, no value leaks with it.
        assert!(preview.has_tls_pin);
        let json = serde_json::to_string(&preview).unwrap();
        assert!(!json.contains("s3cret"));
        assert!(!json.contains(&"AA".repeat(32)));
    }

    // ---- ProvisioningPending ---------------------------------------------

    #[test]
    fn pending_set_then_take_returns_config_once() {
        let pending = ProvisioningPending::default();
        pending.set(valid_config());
        assert_eq!(pending.take(), Some(valid_config()));
        assert_eq!(pending.take(), None); // consumed
    }

    #[test]
    fn pending_clear_discards_without_applying() {
        let pending = ProvisioningPending::default();
        pending.set(valid_config());
        pending.clear();
        assert_eq!(pending.take(), None);
    }

    #[test]
    fn pending_second_resolve_overwrites_first() {
        let pending = ProvisioningPending::default();
        pending.set(valid_config());
        let mut second = valid_config();
        second.ext = "2002".to_string();
        pending.set(second.clone());
        assert_eq!(pending.take(), Some(second));
    }

    // ---- is_provision_link -------------------------------------------

    #[test]
    fn is_provision_link_matches_only_the_provision_host() {
        assert!(is_provision_link(&url::Url::parse("centinelo://provision?url=x").unwrap()));
        assert!(!is_provision_link(&url::Url::parse("centinelo://501").unwrap()));
        assert!(!is_provision_link(&url::Url::parse("https://provision").unwrap()));
    }
}

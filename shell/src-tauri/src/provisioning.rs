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
/// `MAX_CONFIG_BYTES` bytes of raw JSON, base64-encoded (no padding)
/// expands by ~4/3 - `decode_embedded_config` checks the *encoded*
/// string's length against this before ever calling `.decode()` (2026-07-16
/// 4R re-review, B1: decoding first and only checking the decoded length
/// afterward let an attacker-sized base64 string allocate an unbounded
/// buffer before this module ever got to say no). `+ 4` is slack for
/// base64's block-rounding.
const MAX_CONFIG_BYTES_ENCODED: usize = MAX_CONFIG_BYTES * 4 / 3 + 4;
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);

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
    /// This session-only cache holds nothing more sensitive than what a
    /// pending confirmation screen is about to show (still including the
    /// secret internally, only *displayed* fields are filtered - see
    /// `ProvisioningPreviewView`), and a poisoned mutex here only ever
    /// means some unrelated panic happened elsewhere while a lock was
    /// held - `.unwrap_or_else(|e| e.into_inner())` recovers the
    /// last-known value and carries on instead of cascading that
    /// unrelated panic into losing (or crashing on) a pending provisioning
    /// confirmation (2026-07-16 4R re-review, B2 - was `.expect(...)` on
    /// all three methods).
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<ProvisioningConfig>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set(&self, config: ProvisioningConfig) {
        *self.lock() = Some(config);
    }

    /// Non-consuming read - see `commands::provisioning_pending_preview`
    /// (R3: lets the frontend catch a preview whose event fired before
    /// any listener attached) and `commands::provisioning_apply` (R1:
    /// peek, only `clear()` once the apply has actually succeeded, so a
    /// failed apply leaves the config available to retry instead of
    /// forcing a re-paste).
    pub fn peek(&self) -> Option<ProvisioningConfig> {
        self.lock().clone()
    }

    pub fn clear(&self) {
        *self.lock() = None;
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
    // Reject on the ENCODED length first (2026-07-16 4R re-review, B1) -
    // see MAX_CONFIG_BYTES_ENCODED's doc for why checking only the
    // decoded length (the original version of this function) isn't
    // enough on its own.
    if encoded.len() > MAX_CONFIG_BYTES_ENCODED {
        return Err("The embedded provisioning data is too large.".to_string());
    }
    // URL_SAFE_NO_PAD: the config travels inside a URL query parameter
    // (already itself percent-decoded by `query_pairs()` above) - standard
    // base64's `+`/`/`/`=` would need re-escaping there for no benefit.
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "The embedded provisioning data is corrupted (bad base64).".to_string())?;
    // Kept as defense-in-depth even though the encoded-length check above
    // already makes this unreachable in practice (base64 only shrinks).
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

/// IPv4 ranges with no legitimate reason for a PBX provisioning server to
/// ever resolve to them - loopback (any local service on the operator's
/// own machine), link-local/169.254.0.0/16 (includes the 169.254.169.254
/// cloud-metadata endpoint, the textbook SSRF target), unspecified, and
/// multicast/reserved (`>= 224`, covers 255.255.255.255 broadcast too).
/// Deliberately does **not** block RFC 1918 private ranges (10/8,
/// 172.16/12, 192.168/16) or the RFC 6598 CGNAT range (100.64.0.0/10) -
/// both are exactly where a *real* on-prem PBX or a Tailscale-hosted
/// provisioning page legitimately lives (this workspace's own test PBX
/// is a CGNAT/Tailscale address); blocking them would break this
/// feature's primary use case, not just close an edge case. See
/// PROVISIONING.md "Security notes" for the full reasoning and an
/// explicit note that this is a deliberately scoped subset of "block
/// everything private", not a full SSRF-hardened fetch.
fn ipv4_should_be_blocked(ip: std::net::Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 127 // loopback
        || o[0] == 0 // unspecified / "this network"
        || (o[0] == 169 && o[1] == 254) // link-local, incl. cloud metadata
        || o[0] >= 224 // multicast (224/4) + reserved (240/4) + broadcast
}

/// IPv6 equivalent of [`ipv4_should_be_blocked`] - loopback (`::1`),
/// unspecified (`::`), link-local (`fe80::/10`), multicast (`ff00::/8`),
/// and an IPv4-mapped address (`::ffff:a.b.c.d`) checked against the same
/// IPv4 rules. ULA (`fc00::/7`, IPv6's RFC1918-equivalent) is deliberately
/// NOT blocked, matching the IPv4 function's own RFC1918 carve-out.
fn ipv6_should_be_blocked(ip: std::net::Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        return ipv4_should_be_blocked(v4);
    }
    let seg0 = ip.segments()[0];
    (seg0 & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (seg0 & 0xff00) == 0xff00 // multicast ff00::/8
}

fn ip_should_be_blocked(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => ipv4_should_be_blocked(v4),
        std::net::IpAddr::V6(v6) => ipv6_should_be_blocked(v6),
    }
}

/// A [`ureq::Resolver`] that resolves normally (`ToSocketAddrs`, the same
/// stdlib resolution `ureq`'s default resolver uses) but drops any
/// resulting address in [`ip_should_be_blocked`]'s set before handing the
/// list back - and ureq connects to one of exactly *these* addresses, no
/// second resolution afterward. That "resolve once, connect to what you
/// resolved" property is what actually defeats DNS rebinding (2026-07-16
/// 4R re-review, M1): a naive "resolve, check, then let the HTTP client
/// connect by hostname" sequence has a gap where the client's own,
/// independent resolution (its second DNS lookup, for the real
/// connection) can return a different, attacker-flipped answer than the
/// one that was checked. Here there is no second lookup - the checked
/// list *is* what gets connected to.
fn ssrf_safe_resolve(netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
    use std::net::ToSocketAddrs;
    let resolved: Vec<std::net::SocketAddr> = netloc.to_socket_addrs()?.collect();
    let allowed: Vec<std::net::SocketAddr> = resolved.into_iter().filter(|addr| !ip_should_be_blocked(addr.ip())).collect();
    if allowed.is_empty() {
        return Err(std::io::Error::other(
            "every address this host resolved to is a loopback/link-local/multicast address, which a provisioning fetch never legitimately needs",
        ));
    }
    Ok(allowed)
}

fn build_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout(FETCH_TIMEOUT)
        // No automatic redirect following: a redirect to an unexpected
        // host/scheme should be a visible error the operator can inspect
        // (re-paste the final link), not a silent extra hop this shell
        // decided on their behalf.
        .redirects(0)
        // SSRF/DNS-rebinding hardening - see ssrf_safe_resolve's doc.
        .resolver(ssrf_safe_resolve as fn(&str) -> std::io::Result<Vec<std::net::SocketAddr>>)
        .build()
}

/// The actual GET + response handling, over whatever [`ureq::Agent`] the
/// caller supplies - split out from [`fetch_remote`] so this half (status
/// codes, the size-capped body read, JSON parsing) is unit-testable
/// against a real loopback HTTP server (`tests::fetch_via_agent_tests`
/// below) without needing a valid TLS certificate, which the `https`-only
/// enforcement + [`build_agent`]'s SSRF-safe resolver both deliberately
/// stay outside of (2026-07-16 4R re-review, B3 - this was previously
/// exercised only by unrelated pure-parsing tests, never a real request/
/// response round trip).
fn fetch_via_agent(agent: &ureq::Agent, url: &str) -> Result<ProvisioningConfig, String> {
    let response = match agent.get(url).call() {
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

fn fetch_remote(url: &url::Url) -> Result<ProvisioningConfig, String> {
    if url.scheme() != "https" {
        // Defense in depth - every caller into this function has already
        // enforced https at parse time (parse_input/parse_centinelo_provision),
        // but this function isn't otherwise `pub(self)`-locked to that
        // invariant holding forever.
        return Err("Provisioning links must use https.".to_string());
    }
    fetch_via_agent(&build_agent(), url.as_str())
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// The injection-relevant character/length checks (host/ext/secret/
/// display_name) live in `settings::validate_account_fields` now, not
/// here - it's the single source of truth called from every writer of an
/// `AccountSettings` the sidecar will eventually spawn with, not just this
/// provisioning path (2026-07-16 4R re-review, A1: the first version of
/// this function had its own copy of those checks, which meant
/// `commands::save_account_settings` - manual entry in Settings - wasn't
/// covered by them at all). What's left here is provisioning-specific:
/// which of the three required fields are actually required (a manual
/// Settings save allows an empty secret to mean "keep the existing one";
/// a provisioning config without one is simply useless) and the
/// `tls_pin_sha256` format, which only this schema has.
fn validate(config: &ProvisioningConfig) -> Result<(), String> {
    let host = config.host.trim();
    if host.is_empty() {
        return Err("The provisioning config is missing \"host\".".to_string());
    }
    let ext = config.ext.trim();
    if ext.is_empty() {
        return Err("The provisioning config is missing \"ext\".".to_string());
    }
    if config.secret.is_empty() {
        return Err("The provisioning config is missing \"secret\".".to_string());
    }
    crate::settings::validate_account_fields(host, ext, &config.secret, &config.display_name)
        .map_err(|e| format!("The provisioning config is invalid: {e}"))?;

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
        c.host = "a".repeat(crate::settings::MAX_HOST_LEN + 1);
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
    fn pending_set_then_peek_then_clear_leaves_it_empty() {
        let pending = ProvisioningPending::default();
        pending.set(valid_config());
        assert_eq!(pending.peek(), Some(valid_config()));
        pending.clear();
        assert_eq!(pending.peek(), None);
    }

    #[test]
    fn pending_clear_discards_without_applying() {
        let pending = ProvisioningPending::default();
        pending.set(valid_config());
        pending.clear();
        assert_eq!(pending.peek(), None);
    }

    #[test]
    fn pending_second_resolve_overwrites_first() {
        let pending = ProvisioningPending::default();
        pending.set(valid_config());
        let mut second = valid_config();
        second.ext = "2002".to_string();
        pending.set(second.clone());
        assert_eq!(pending.peek(), Some(second));
    }

    #[test]
    fn pending_peek_does_not_consume() {
        // R1/R3's whole point: peek() must be side-effect-free so
        // provisioning_apply can look before it leaps, and boot()'s
        // provisioning_pending_preview can check without racing a real
        // apply/cancel.
        let pending = ProvisioningPending::default();
        pending.set(valid_config());
        assert_eq!(pending.peek(), Some(valid_config()));
        assert_eq!(pending.peek(), Some(valid_config())); // still there
        pending.clear();
        assert_eq!(pending.peek(), None);
    }

    // ---- is_provision_link -------------------------------------------

    #[test]
    fn is_provision_link_matches_only_the_provision_host() {
        assert!(is_provision_link(&url::Url::parse("centinelo://provision?url=x").unwrap()));
        assert!(!is_provision_link(&url::Url::parse("centinelo://501").unwrap()));
        assert!(!is_provision_link(&url::Url::parse("https://provision").unwrap()));
    }

    // ---- SSRF/DNS-rebinding hardening (2026-07-16 4R re-review, M1) -----

    #[test]
    fn loopback_v4_is_blocked() {
        assert!(ipv4_should_be_blocked("127.0.0.1".parse().unwrap()));
        assert!(ipv4_should_be_blocked("127.53.1.1".parse().unwrap()));
    }

    #[test]
    fn cloud_metadata_link_local_v4_is_blocked() {
        // 169.254.169.254 - the textbook SSRF target (AWS/GCP/Azure
        // instance-metadata endpoints all live here).
        assert!(ipv4_should_be_blocked("169.254.169.254".parse().unwrap()));
        assert!(ipv4_should_be_blocked("169.254.0.1".parse().unwrap()));
    }

    #[test]
    fn unspecified_and_multicast_v4_blocked() {
        assert!(ipv4_should_be_blocked("0.0.0.0".parse().unwrap()));
        assert!(ipv4_should_be_blocked("224.0.0.1".parse().unwrap()));
        assert!(ipv4_should_be_blocked("255.255.255.255".parse().unwrap()));
    }

    #[test]
    fn rfc1918_and_cgnat_v4_are_deliberately_allowed() {
        // NOT blocked on purpose - see ipv4_should_be_blocked's doc: this
        // product's real deployments (including this workspace's own test
        // PBX, a Tailscale/CGNAT address) live in exactly these ranges.
        assert!(!ipv4_should_be_blocked("192.168.1.50".parse().unwrap()));
        assert!(!ipv4_should_be_blocked("10.0.0.5".parse().unwrap()));
        assert!(!ipv4_should_be_blocked("172.16.0.5".parse().unwrap()));
        assert!(!ipv4_should_be_blocked("100.100.1.1".parse().unwrap())); // CGNAT/Tailscale
    }

    #[test]
    fn public_v4_is_allowed() {
        assert!(!ipv4_should_be_blocked("93.184.216.34".parse().unwrap()));
    }

    #[test]
    fn loopback_and_unspecified_v6_blocked() {
        assert!(ipv6_should_be_blocked("::1".parse().unwrap()));
        assert!(ipv6_should_be_blocked("::".parse().unwrap()));
    }

    #[test]
    fn link_local_and_multicast_v6_blocked() {
        assert!(ipv6_should_be_blocked("fe80::1".parse().unwrap()));
        assert!(ipv6_should_be_blocked("ff02::1".parse().unwrap()));
    }

    #[test]
    fn ula_v6_deliberately_allowed() {
        assert!(!ipv6_should_be_blocked("fc00::1".parse().unwrap()));
        assert!(!ipv6_should_be_blocked("fd12:3456:789a::1".parse().unwrap()));
    }

    #[test]
    fn ipv4_mapped_v6_defers_to_the_v4_rules() {
        // ::ffff:127.0.0.1 - a rebinding trick specifically aimed at
        // resolvers that check the "obvious" v4/v6 cases but forget the
        // mapped form.
        assert!(ipv6_should_be_blocked("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!ipv6_should_be_blocked("::ffff:93.184.216.34".parse().unwrap()));
    }

    #[test]
    fn public_v6_is_allowed() {
        assert!(!ipv6_should_be_blocked("2606:2800:220:1:248:1893:25c8:1946".parse().unwrap()));
    }

    #[test]
    fn ssrf_safe_resolve_rejects_when_every_address_is_blocked() {
        // "localhost" resolves only to loopback addresses on every
        // platform this app targets.
        let err = ssrf_safe_resolve("localhost:443").unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn ssrf_safe_resolve_passes_through_a_public_looking_literal() {
        // A bare IP literal:port resolves synchronously, no real DNS
        // needed - keeps this test hermetic (no network access required
        // to run `cargo test`).
        let addrs = ssrf_safe_resolve("93.184.216.34:443").unwrap();
        assert_eq!(addrs, vec!["93.184.216.34:443".parse().unwrap()]);
    }

    #[test]
    fn ssrf_safe_resolve_rejects_a_loopback_literal() {
        assert!(ssrf_safe_resolve("127.0.0.1:8080").is_err());
    }

    // ---- decode_embedded_config: pre-decode length cap (B1) --------------

    #[test]
    fn embedded_config_oversized_encoded_string_rejected_before_decoding() {
        // Not valid base64 at all (repeated '!') - if this were rejected
        // via the OLD "decode first, check length after" order it would
        // fail with the "corrupted (bad base64)" message instead; getting
        // "too large" here proves the length check runs first and never
        // reaches .decode() at all (2026-07-16 4R re-review, B1).
        let huge_garbage = "!".repeat(MAX_CONFIG_BYTES_ENCODED + 1);
        let err = decode_embedded_config(&huge_garbage).unwrap_err();
        assert!(err.contains("too large"), "unexpected message: {err}");
    }

    // ---- fetch_via_agent: real request/response round trip (B3) ---------
    // A loopback tiny_http server (already a project dependency, see
    // bridge.rs) standing in for a provisioning server - exercises the
    // parts build_agent()'s https-only/SSRF-resolver wrapping deliberately
    // stays outside of: status handling, the size cap against a REAL
    // streamed response (not an in-memory Cursor), and JSON parsing.

    fn test_agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(5))
            .redirects(0)
            .build()
    }

    #[test]
    fn fetch_via_agent_happy_path() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr();
        let handle = std::thread::spawn(move || {
            let request = server.recv().unwrap();
            let body = r#"{"host":"pbx.example.test","ext":"9999","secret":"x"}"#;
            let response = tiny_http::Response::from_string(body);
            request.respond(response).unwrap();
        });
        let url = format!("http://{addr}/config.json");
        let config = fetch_via_agent(&test_agent(), &url).unwrap();
        assert_eq!(config.host, "pbx.example.test");
        assert_eq!(config.ext, "9999");
        assert_eq!(config.secret, "x");
        handle.join().unwrap();
    }

    #[test]
    fn fetch_via_agent_non_2xx_status_surfaces_as_error() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr();
        let handle = std::thread::spawn(move || {
            let request = server.recv().unwrap();
            let response = tiny_http::Response::from_string("not found").with_status_code(404);
            request.respond(response).unwrap();
        });
        let url = format!("http://{addr}/missing.json");
        let err = fetch_via_agent(&test_agent(), &url).unwrap_err();
        assert!(err.contains("404"), "unexpected message: {err}");
        handle.join().unwrap();
    }

    #[test]
    fn fetch_via_agent_malformed_json_surfaces_as_error() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr();
        let handle = std::thread::spawn(move || {
            let request = server.recv().unwrap();
            let response = tiny_http::Response::from_string("not json at all");
            request.respond(response).unwrap();
        });
        let url = format!("http://{addr}/bad.json");
        assert!(fetch_via_agent(&test_agent(), &url).is_err());
        handle.join().unwrap();
    }

    #[test]
    fn fetch_via_agent_oversized_body_rejected() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr();
        let handle = std::thread::spawn(move || {
            let request = server.recv().unwrap();
            let huge = "x".repeat(MAX_CONFIG_BYTES + 1);
            let response = tiny_http::Response::from_string(huge);
            request.respond(response).unwrap();
        });
        let url = format!("http://{addr}/huge.json");
        let err = fetch_via_agent(&test_agent(), &url).unwrap_err();
        assert!(err.contains("too large"), "unexpected message: {err}");
        handle.join().unwrap();
    }

    // ---- JSON with required fields genuinely ABSENT, not empty (B3) -----

    #[test]
    fn json_missing_required_fields_entirely_rejected_by_deserialization() {
        // Different code path from empty_host_rejected/empty_ext_rejected/
        // empty_secret_rejected above (those go through validate() on an
        // already-constructed Rust struct with a `""` value) - this
        // exercises real JSON deserialization failing because the key is
        // ABSENT, not present-but-empty, since host/ext/secret have no
        // #[serde(default)] (2026-07-16 4R re-review, B3).
        assert!(serde_json::from_str::<ProvisioningConfig>(r#"{"ext":"9999","secret":"x"}"#).is_err());
        assert!(serde_json::from_str::<ProvisioningConfig>(r#"{"host":"pbx.example.test","secret":"x"}"#).is_err());
        assert!(serde_json::from_str::<ProvisioningConfig>(r#"{"host":"pbx.example.test","ext":"9999"}"#).is_err());
    }
}

//! License activation flow (P3 of the activation-server plan): the shell
//! side of "generic signed serial in -> seat-counted, machine-bound,
//! signed license out". Read-only references for the design this
//! implements (private premium repo, never a dependency of this crate -
//! see this module's own "Never depends on centinelo-license" section
//! below): `premium/docs/SPEC-2026-07-17-activation-server-design.md`
//! §5.4, `premium/docs/PLAN-2026-07-17-activation-server.md` §P3.
//!
//! # Never depends on `centinelo-license`
//!
//! Exactly the same rule `premium.rs`'s own doc states for the dylib
//! loader ("Where the license check actually happens... not here"): this
//! is a public, forkable repo, and a fork could trivially delete any
//! gating logic that lived here. The private `centinelo-license` crate
//! (signed-container schema, `machine_fingerprint()`) is therefore never a
//! dependency of this crate - the small, non-secret *shapes* this feature
//! needs from it (the `{payload, sig}` envelope, the SHA-256-of-a-
//! platform-id fingerprint algorithm) are duplicated here by hand instead,
//! the same precedent `centinelo-premium-abi/src/capability.rs` already
//! sets for the `FEATURE_*` name strings ("can't share the constant...
//! duplicated here by hand"). Read-only references for what's duplicated:
//! `premium/crates/centinelo-license/src/container.rs` (envelope +
//! verify order) and `.../src/fingerprint.rs` (the algorithm this
//! module's `machine_fingerprint` mirrors bit-for-bit, so a license this
//! shell requests binds to the same fingerprint a future real consumer of
//! `license.json` would compute for the same machine).
//!
//! # Where the file this piece writes actually gets read
//!
//! `activate_and_persist` below writes a verified `license.json` next to
//! `settings.json` (see `settings::SettingsStore::license_path`).
//! `centinelo-premium`'s own `src/license.rs` (private repo, read-only
//! reference, this repo's own scope never includes editing it) now has a
//! `load_and_verify_license_from_disk()` that reads exactly that file and
//! re-verifies it against the same dual-pubkey rule this module's own
//! `verify_container` already uses locally before writing (founder/release
//! pubkey first, activation pubkey second - either one accepting is
//! enough). `active_license()` there is
//! `load_and_verify_license_from_disk().unwrap_or_else(founder_license)`:
//! any failure at any step (no file, corrupt JSON, bad signature against
//! both pubkeys, expired, wrong machine) collapses to the founder license,
//! never a panic - same graceful-degradation contract a missing license
//! file already had. This piece's own job stops at a correctly-verified
//! `license.json` landing on disk; whether that file is actually being
//! consumed on a given build depends on which `centinelo-premium` revision
//! it's linked against (licensing agent's ambit, not this one) - not
//! independently re-verified from this side beyond reading that crate's
//! source.
//!
//! # Error codes, not prose, cross the Tauri command boundary
//!
//! [`ActivationError::code`] returns a short, stable identifier
//! (`"seats_exhausted"`, `"serial_revoked"`, ...), never the displayed
//! sentence - `commands::activate_license` returns exactly that code as
//! its `Err(String)`. The actual copy (the ES wording the P3 task brief
//! specifies, plus EN/PT-BR) lives in `ui/js/i18n.js`'s
//! `activation.error.<code>` keys and is rendered through this app's one
//! real i18n system, same as every other user-facing string in Settings -
//! see that file for the exact text. This repo's own rule ("UI text
//! English" - `.claude/skills/shell-tauri/SKILL.md`) is about what ships
//! hardcoded in Rust; the localized *display* copy is a frontend i18n
//! concern the same way every other Settings error already works (compare
//! `commands::provisioning_apply`'s English-only backend string, which by
//! contrast never runs through i18n.js today - this feature deliberately
//! does better, not worse, matching the product's real EN/PT-BR/ES
//! surface instead of adding a second, inconsistent all-Spanish-only
//! error path).

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

// ---------------------------------------------------------------------
// Activation server public key
// ---------------------------------------------------------------------

/// # DEV/TEST PLACEHOLDER — replace before shipping a real Pro build
///
/// `SigningKey::from_bytes(&[0x77; 32]).verifying_key().to_bytes()` - a
/// fixed, publicly-documented dev/test seed, deliberately distinct from
/// `premium.rs`'s own `LIB_PUBKEY_BYTES` dev seed (`[0x24; 32]`, a
/// different key for a different job: that one authenticates the
/// `centinelo_premium` dylib *binary*, this one authenticates a *license*
/// issued by an activation server) and from `centinelo-premium`'s
/// `DEV_TEST_LICENSE_SIGNING_SEED` (`[0x42; 32]`, private repo, gates the
/// dev-only `CENTINELO_PREMIUM_LICENSE_PATH` override) - three separate
/// keypairs, three separate jobs, per
/// `premium/docs/SPEC-2026-07-17-activation-server-design.md` §3's key
/// model table.
///
/// **Before an official release build**: Felix generates a real Ed25519
/// activation keypair offline (same ceremony `shell/README.md`'s "Dev
/// signing key" section already documents for the updater's own dev
/// pubkey), replaces the bytes below with the real public half, and the
/// real private half becomes `centinelo-activationd`'s `ACTIVATIOND_KEY`
/// (server-side, private repo, never in this repo). Until that swap
/// happens, this shell only ever accepts a license signed by the
/// well-known dev key above — a safe default (no real activation can
/// verify), not a broken one.
pub const ACTIVATION_PUBKEY_BYTES: [u8; 32] = [
    200, 83, 173, 15, 12, 210, 182, 25, 174, 169, 44, 238, 196, 253, 86, 162, 77, 100, 153, 213,
    132, 206, 121, 37, 126, 69, 207, 216, 19, 155, 96, 167,
];

fn activation_pubkey() -> VerifyingKey {
    VerifyingKey::from_bytes(&ACTIVATION_PUBKEY_BYTES)
        .expect("ACTIVATION_PUBKEY_BYTES must be a valid Ed25519 public key - see its doc comment")
}

// ---------------------------------------------------------------------
// Machine fingerprint (duplicated from centinelo-license::machine_fingerprint
// - see this module's top doc, "Never depends on centinelo-license")
// ---------------------------------------------------------------------

/// Same override env var name `centinelo_license::fingerprint` uses - a
/// deliberate match, not a coincidence: qa-e2e's local loop (P4) and any
/// future real consumer both need the SAME override to select the SAME
/// fingerprint for the SAME test machine, regardless of which of the two
/// (independently compiled) copies of this algorithm computed it.
const MACHINE_ID_OVERRIDE_ENV: &str = "CENTINELO_MACHINE_ID";

/// Computes a stable SHA-256 hex fingerprint identifying the local
/// machine — the exact algorithm
/// `premium/crates/centinelo-license/src/fingerprint.rs::machine_fingerprint`
/// uses (macOS: `IOPlatformUUID` via `ioreg`; Windows: `MachineGuid` from
/// the registry; `CENTINELO_MACHINE_ID` env override for dev/test/CI on
/// any platform), duplicated here rather than imported (see this module's
/// top doc).
pub fn machine_fingerprint() -> Result<String, String> {
    if let Ok(value) = std::env::var(MACHINE_ID_OVERRIDE_ENV) {
        if !value.is_empty() {
            return Ok(hex::encode(sha256(value.as_bytes())));
        }
    }
    raw_machine_id().map(|id| hex::encode(sha256(id.as_bytes())))
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

#[cfg(target_os = "macos")]
fn raw_machine_id() -> Result<String, String> {
    use std::process::Command;

    let output = Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .map_err(|e| format!("failed to run ioreg: {e}"))?;
    if !output.status.success() {
        return Err(format!("ioreg exited with {}", output.status));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_ioplatformuuid(&text).ok_or_else(|| "IOPlatformUUID not found in ioreg output".to_string())
}

/// Parses `"IOPlatformUUID" = "XXXX..."` out of `ioreg` output - split out
/// for testability without actually running `ioreg` (this crate's CI also
/// runs macOS, unlike centinelo-license's Linux CI arm, but keeping the
/// parser pure and independently testable is cheap and matches the
/// function it mirrors).
#[cfg(target_os = "macos")]
fn parse_ioplatformuuid(ioreg_output: &str) -> Option<String> {
    for line in ioreg_output.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("\"IOPlatformUUID\"") else {
            continue;
        };
        let start = rest.find('"')?;
        let after = &rest[start + 1..];
        let end = after.find('"')?;
        return Some(after[..end].to_string());
    }
    None
}

#[cfg(target_os = "windows")]
fn raw_machine_id() -> Result<String, String> {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let key = hklm
        .open_subkey("SOFTWARE\\Microsoft\\Cryptography")
        .map_err(|e| format!("failed to open registry key: {e}"))?;
    let guid: String = key
        .get_value("MachineGuid")
        .map_err(|e| format!("failed to read MachineGuid: {e}"))?;
    Ok(guid)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn raw_machine_id() -> Result<String, String> {
    Err("machine fingerprinting is only implemented for macOS and Windows (Centinelo Phone's \
         target platforms); set CENTINELO_MACHINE_ID for local development or testing on this \
         platform"
        .to_string())
}

// ---------------------------------------------------------------------
// Signed license container (mirrors centinelo_license::SignedLicense's
// {payload, sig} envelope shape exactly - see this module's top doc)
// ---------------------------------------------------------------------

/// The subset of `centinelo_license::License`'s fields this shell needs
/// to tell the operator what activation just granted. `schema` and
/// `machine_id` round-trip through JSON parsing but aren't surfaced here,
/// since this shell has no consumer that acts on them yet (see this
/// module's top doc, "The real gap this piece leaves open").
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct LicensedFeatures {
    pub customer: String,
    pub features: Vec<String>,
    pub seats: u32,
    #[serde(default)]
    pub expiry: Option<String>,
}

/// Verifies `container_json` (a `{"payload": "<base64>", "sig":
/// "<base64>"}` envelope) against `pubkey` and, only once the signature
/// checks out, parses `payload` into [`LicensedFeatures`]. Never
/// re-serializes to check the signature - the Ed25519 check runs against
/// the raw decoded `payload` bytes first, exactly the discipline
/// `centinelo_license::verify`'s own doc describes and `premium.rs`'s
/// dylib signature check already follows in this shell.
fn verify_container(container_json: &[u8], pubkey: &VerifyingKey) -> Result<LicensedFeatures, ActivationError> {
    #[derive(Deserialize)]
    struct Envelope {
        payload: String,
        sig: String,
    }

    let envelope: Envelope =
        serde_json::from_slice(container_json).map_err(|_| ActivationError::LocalVerifyFailed)?;
    let payload_bytes = BASE64.decode(envelope.payload.as_bytes()).map_err(|_| ActivationError::LocalVerifyFailed)?;
    let sig_bytes = BASE64.decode(envelope.sig.as_bytes()).map_err(|_| ActivationError::LocalVerifyFailed)?;
    let sig_array: [u8; 64] = sig_bytes.as_slice().try_into().map_err(|_| ActivationError::LocalVerifyFailed)?;
    let signature = Signature::from_bytes(&sig_array);
    pubkey
        .verify(&payload_bytes, &signature)
        .map_err(|_| ActivationError::LocalVerifyFailed)?;
    serde_json::from_slice(&payload_bytes).map_err(|_| ActivationError::LocalVerifyFailed)
}

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivationError {
    BadUrl(String),
    InvalidSerial,
    ExpiredSerial,
    BadFingerprint,
    SeatsExhausted,
    SerialRevoked,
    RateLimited,
    Network(String),
    ServerError(String),
    LocalVerifyFailed,
    FingerprintUnavailable(String),
    Io(String),
}

impl ActivationError {
    /// Stable machine-readable code — see this module's top doc, "Error
    /// codes, not prose, cross the Tauri command boundary".
    pub fn code(&self) -> &'static str {
        match self {
            ActivationError::BadUrl(_) => "bad_url",
            ActivationError::InvalidSerial => "invalid_serial",
            ActivationError::ExpiredSerial => "expired_serial",
            ActivationError::BadFingerprint => "bad_fingerprint",
            ActivationError::SeatsExhausted => "seats_exhausted",
            ActivationError::SerialRevoked => "serial_revoked",
            ActivationError::RateLimited => "rate_limited",
            ActivationError::Network(_) => "network",
            ActivationError::ServerError(_) => "server_error",
            ActivationError::LocalVerifyFailed => "local_verify_failed",
            ActivationError::FingerprintUnavailable(_) => "fingerprint_unavailable",
            ActivationError::Io(_) => "io_error",
        }
    }
}

impl std::fmt::Display for ActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActivationError::BadUrl(detail) => write!(f, "bad activation server URL: {detail}"),
            ActivationError::Network(detail) => write!(f, "network error: {detail}"),
            ActivationError::ServerError(detail) => write!(f, "server error: {detail}"),
            ActivationError::FingerprintUnavailable(detail) => write!(f, "fingerprint unavailable: {detail}"),
            ActivationError::Io(detail) => write!(f, "local write failed: {detail}"),
            other => write!(f, "{}", other.code()),
        }
    }
}

// ---------------------------------------------------------------------
// Server URL validation
// ---------------------------------------------------------------------

/// `https://` is always accepted. `http://127.0.0.1` / `http://localhost`
/// (any port, any path) is accepted too, but ONLY for local testing (P3
/// task brief / spec §5.4 - `centinelo-activationd` has no TLS of its own,
/// that's the reverse proxy's job per spec §5.3, so a real deployment is
/// always `https://`). Everything else is rejected before this shell ever
/// sends a serial anywhere. Trims first - an admin pasting a URL commonly
/// leaves surrounding whitespace; trailing `/` is stripped too so
/// `format!("{base}/activate")` never produces a double slash.
pub fn validate_server_url(raw: &str) -> Result<String, ActivationError> {
    // The actual rule lives in [`crate::url_policy`] now (P6): it's shared with
    // the remote-STT settings, so it has to return a plain `Result<String, String>`
    // instead of this enum. We keep the typed `ActivationError` wrapper (and the
    // stable `bad_url` code callers/tests depend on) by mapping through.
    crate::url_policy::validate_https_or_localhost(raw)
        .map_err(ActivationError::BadUrl)
}

// ---------------------------------------------------------------------
// HTTP: POST /activate
// ---------------------------------------------------------------------

#[derive(Serialize)]
struct ActivateRequestBody<'a> {
    serial: &'a str,
    machine_fingerprint: &'a str,
}

#[derive(Deserialize)]
struct ActivateOkBody {
    license: serde_json::Value,
}

#[derive(Deserialize)]
struct ActivateErrBody {
    error: String,
}

const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(15);

fn build_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(ACTIVATION_TIMEOUT)
        .timeout(ACTIVATION_TIMEOUT)
        .redirects(0)
        .build()
}

fn read_capped_body(mut reader: impl Read, cap: usize) -> Result<Vec<u8>, ActivationError> {
    let mut buf = Vec::with_capacity(cap.min(4096));
    reader
        .by_ref()
        .take((cap as u64) + 1)
        .read_to_end(&mut buf)
        .map_err(|e| ActivationError::Network(e.to_string()))?;
    if buf.len() > cap {
        return Err(ActivationError::ServerError("response too large".to_string()));
    }
    Ok(buf)
}

/// Maps a non-2xx status + optional parsed `{"error": "<code>"}` body onto
/// a typed [`ActivationError`] - see `premium/docs/
/// SPEC-2026-07-17-activation-server-design.md` §5.3's response table.
/// An unrecognized 400 body still counts as a client-side problem with the
/// serial, so it falls back to `InvalidSerial` rather than a bucket the
/// UI has no specific copy for.
fn map_status_error(status: u16, error_code: Option<&str>) -> ActivationError {
    match status {
        400 => match error_code {
            Some("expired_serial") => ActivationError::ExpiredSerial,
            Some("bad_fingerprint") => ActivationError::BadFingerprint,
            _ => ActivationError::InvalidSerial,
        },
        403 => ActivationError::SerialRevoked,
        409 => ActivationError::SeatsExhausted,
        429 => ActivationError::RateLimited,
        other => ActivationError::ServerError(format!("unexpected status {other}")),
    }
}

/// The actual POST + response handling, over whatever [`ureq::Agent`] the
/// caller supplies — split out from [`activate_and_persist`] so this half
/// is unit-testable against a real loopback HTTP server
/// (`tests::post_activate_tests`) without needing TLS, matching
/// `provisioning.rs`'s `fetch_via_agent`/`fetch_remote` split.
fn post_activate(agent: &ureq::Agent, base_url: &str, serial: &str, fingerprint: &str) -> Result<Vec<u8>, ActivationError> {
    let url = format!("{base_url}/activate");
    let body = ActivateRequestBody { serial, machine_fingerprint: fingerprint };
    match agent.post(&url).send_json(serde_json::json!(body)) {
        Ok(response) => {
            if response.status() != 200 {
                return Err(ActivationError::ServerError(format!("unexpected status {}", response.status())));
            }
            read_capped_body(response.into_reader(), MAX_RESPONSE_BYTES)
        }
        Err(ureq::Error::Status(code, response)) => {
            let body_bytes = read_capped_body(response.into_reader(), MAX_RESPONSE_BYTES).unwrap_or_default();
            let error_code = serde_json::from_slice::<ActivateErrBody>(&body_bytes).ok().map(|b| b.error);
            Err(map_status_error(code, error_code.as_deref()))
        }
        Err(ureq::Error::Transport(e)) => Err(ActivationError::Network(e.to_string())),
    }
}

// ---------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ActivationResult {
    pub customer: String,
    pub features: Vec<String>,
    pub seats: u32,
    pub expiry: Option<String>,
}

/// The full activation flow: validate the URL, compute this machine's
/// fingerprint, `POST /activate`, and — only on a 200 whose license
/// verifies locally against [`ACTIVATION_PUBKEY_BYTES`] — write it
/// atomically to `license_path` (via `settings::write_private_file`, the
/// same tmp+rename+fsync+0600 write every other sensitive file in this
/// app uses). Any failure at any step (bad URL, network, a non-2xx
/// response, a 200 whose signature does NOT verify) returns an
/// [`ActivationError`] and leaves `license_path` completely untouched -
/// server bugs, or a 200 that fails local verification, must never
/// corrupt or replace whatever license state already existed (spec §5.4:
/// "a failed activation changes nothing").
pub fn activate_and_persist(license_path: &Path, server_url: &str, serial: &str) -> Result<ActivationResult, ActivationError> {
    let serial = serial.trim();
    if serial.is_empty() {
        return Err(ActivationError::InvalidSerial);
    }
    let base_url = validate_server_url(server_url)?;
    let fingerprint = machine_fingerprint().map_err(ActivationError::FingerprintUnavailable)?;

    let agent = build_agent();
    let response_bytes = post_activate(&agent, &base_url, serial, &fingerprint)?;

    let ok_body: ActivateOkBody =
        serde_json::from_slice(&response_bytes).map_err(|_| ActivationError::ServerError("malformed 200 response".to_string()))?;
    // One canonical byte representation of the license envelope, used for
    // BOTH the signature check and the on-disk write below - no second,
    // possibly-different re-serialization anywhere in this path.
    let container_bytes = serde_json::to_vec(&ok_body.license).map_err(|_| ActivationError::ServerError("malformed license field".to_string()))?;

    let features = verify_container(&container_bytes, &activation_pubkey())?;

    crate::settings::write_private_file(license_path, &container_bytes).map_err(|e| ActivationError::Io(e.to_string()))?;

    Ok(ActivationResult {
        customer: features.customer,
        features: features.features,
        seats: features.seats,
        expiry: features.expiry,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    // ---- machine_fingerprint -------------------------------------------

    #[test]
    fn dev_override_is_deterministic_and_hashed() {
        let _guard = env_lock();
        std::env::set_var(MACHINE_ID_OVERRIDE_ENV, "test-machine-alpha");
        let a = machine_fingerprint().unwrap();
        let b = machine_fingerprint().unwrap();
        std::env::remove_var(MACHINE_ID_OVERRIDE_ENV);

        assert_eq!(a, b);
        assert_eq!(a.len(), 64, "sha256 hex digest is 64 chars");
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, "test-machine-alpha", "must be hashed, not passed through");
    }

    #[test]
    fn different_overrides_produce_different_fingerprints() {
        let _guard = env_lock();
        std::env::set_var(MACHINE_ID_OVERRIDE_ENV, "machine-a");
        let a = machine_fingerprint().unwrap();
        std::env::set_var(MACHINE_ID_OVERRIDE_ENV, "machine-b");
        let b = machine_fingerprint().unwrap();
        std::env::remove_var(MACHINE_ID_OVERRIDE_ENV);
        assert_ne!(a, b);
    }

    /// Environment variables are process-global - serialize the tests
    /// that touch MACHINE_ID_OVERRIDE_ENV (cargo test runs tests in
    /// parallel threads by default), same discipline
    /// centinelo-license's own fingerprint tests use.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_real_ioreg_shaped_output() {
        let sample = "+-o Mac : <class IOPlatformExpertDevice>\n  \"IOPlatformSerialNumber\" = \"C02ABC123XYZ\"\n  \"IOPlatformUUID\" = \"12345678-ABCD-4EF0-9876-FEDCBA098765\"\n";
        assert_eq!(parse_ioplatformuuid(sample).as_deref(), Some("12345678-ABCD-4EF0-9876-FEDCBA098765"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn missing_ioplatformuuid_returns_none() {
        assert_eq!(parse_ioplatformuuid("no relevant lines here"), None);
    }

    // ---- validate_server_url -------------------------------------------

    #[test]
    fn https_url_accepted() {
        assert_eq!(validate_server_url("https://activation.example.test").unwrap(), "https://activation.example.test");
    }

    #[test]
    fn https_url_trailing_slash_trimmed() {
        assert_eq!(validate_server_url("https://activation.example.test/").unwrap(), "https://activation.example.test");
    }

    #[test]
    fn http_localhost_accepted_for_testing() {
        assert!(validate_server_url("http://localhost:8720").is_ok());
        assert!(validate_server_url("http://127.0.0.1:8720").is_ok());
    }

    #[test]
    fn http_non_loopback_rejected() {
        let err = validate_server_url("http://activation.example.test").unwrap_err();
        assert_eq!(err.code(), "bad_url");
    }

    #[test]
    fn empty_url_rejected() {
        assert_eq!(validate_server_url("   ").unwrap_err().code(), "bad_url");
    }

    #[test]
    fn garbage_url_rejected() {
        assert_eq!(validate_server_url("not a url at all").unwrap_err().code(), "bad_url");
    }

    #[test]
    fn other_scheme_rejected() {
        assert_eq!(validate_server_url("ftp://activation.example.test").unwrap_err().code(), "bad_url");
    }

    #[test]
    fn surrounding_whitespace_trimmed() {
        assert_eq!(validate_server_url("  https://activation.example.test  ").unwrap(), "https://activation.example.test");
    }

    // ---- verify_container -------------------------------------------

    const TEST_ACTIVATION_SEED: [u8; 32] = [0x77; 32];
    const WRONG_SEED: [u8; 32] = [0x99; 32];

    fn test_activation_signing_key() -> SigningKey {
        SigningKey::from_bytes(&TEST_ACTIVATION_SEED)
    }

    fn sample_payload_json(features: &[&str], seats: u32) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema": 1,
            "customer": "Test Customer",
            "features": features,
            "seats": seats,
            "machine_id": "a".repeat(64),
            "issued_at": "2026-07-17T00:00:00Z",
            "expiry": serde_json::Value::Null,
        }))
        .unwrap()
    }

    fn sign_container(signing_key: &SigningKey, payload_bytes: &[u8]) -> Vec<u8> {
        let sig = signing_key.sign(payload_bytes);
        serde_json::to_vec(&serde_json::json!({
            "payload": BASE64.encode(payload_bytes),
            "sig": BASE64.encode(sig.to_bytes()),
        }))
        .unwrap()
    }

    #[test]
    fn valid_signature_verifies_and_parses() {
        let payload = sample_payload_json(&["blf_console", "transcription"], 5);
        let container = sign_container(&test_activation_signing_key(), &payload);
        let features = verify_container(&container, &activation_pubkey()).unwrap();
        assert_eq!(features.customer, "Test Customer");
        assert_eq!(features.features, vec!["blf_console", "transcription"]);
        assert_eq!(features.seats, 5);
    }

    #[test]
    fn tampered_payload_fails_verification() {
        let payload = sample_payload_json(&["blf_console"], 1);
        let mut container: serde_json::Value = {
            let bytes = sign_container(&test_activation_signing_key(), &payload);
            serde_json::from_slice(&bytes).unwrap()
        };
        // Flip one byte of the base64 payload - tamper detection.
        let mut payload_b64 = container["payload"].as_str().unwrap().to_string();
        let mid = payload_b64.len() / 2;
        let flipped = if payload_b64.as_bytes()[mid] == b'A' { 'B' } else { 'A' };
        payload_b64.replace_range(mid..mid + 1, &flipped.to_string());
        container["payload"] = serde_json::Value::String(payload_b64);
        let container_bytes = serde_json::to_vec(&container).unwrap();
        let err = verify_container(&container_bytes, &activation_pubkey()).unwrap_err();
        assert_eq!(err, ActivationError::LocalVerifyFailed);
    }

    #[test]
    fn wrong_signing_key_fails_verification() {
        let payload = sample_payload_json(&["blf_console"], 1);
        let container = sign_container(&SigningKey::from_bytes(&WRONG_SEED), &payload);
        let err = verify_container(&container, &activation_pubkey()).unwrap_err();
        assert_eq!(err, ActivationError::LocalVerifyFailed);
    }

    #[test]
    fn malformed_container_json_fails_verification() {
        let err = verify_container(b"not json", &activation_pubkey()).unwrap_err();
        assert_eq!(err, ActivationError::LocalVerifyFailed);
    }

    // ---- post_activate: real request/response round trip --------------

    fn test_agent() -> ureq::Agent {
        ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(5)).redirects(0).build()
    }

    fn spawn_mock_server(status: u16, body: String) -> (String, std::thread::JoinHandle<()>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr();
        let handle = std::thread::spawn(move || {
            let mut request = server.recv().unwrap();
            let mut sent_body = String::new();
            let _ = request.as_reader().read_to_string(&mut sent_body);
            let response = tiny_http::Response::from_string(body).with_status_code(status);
            request.respond(response).unwrap();
        });
        (format!("http://{addr}"), handle)
    }

    #[test]
    fn post_activate_200_returns_body() {
        let payload = sample_payload_json(&["blf_console"], 3);
        let container = sign_container(&test_activation_signing_key(), &payload);
        let container_value: serde_json::Value = serde_json::from_slice(&container).unwrap();
        let body = serde_json::json!({ "license": container_value }).to_string();
        let (url, handle) = spawn_mock_server(200, body);
        let result = post_activate(&test_agent(), &url, "CENT1-abc", "f".repeat(64).as_str());
        handle.join().unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn post_activate_409_maps_to_seats_exhausted() {
        let (url, handle) = spawn_mock_server(409, r#"{"error":"seats_exhausted"}"#.to_string());
        let err = post_activate(&test_agent(), &url, "CENT1-abc", &"f".repeat(64)).unwrap_err();
        handle.join().unwrap();
        assert_eq!(err, ActivationError::SeatsExhausted);
    }

    #[test]
    fn post_activate_403_maps_to_serial_revoked() {
        let (url, handle) = spawn_mock_server(403, r#"{"error":"serial_revoked"}"#.to_string());
        let err = post_activate(&test_agent(), &url, "CENT1-abc", &"f".repeat(64)).unwrap_err();
        handle.join().unwrap();
        assert_eq!(err, ActivationError::SerialRevoked);
    }

    #[test]
    fn post_activate_400_invalid_serial() {
        let (url, handle) = spawn_mock_server(400, r#"{"error":"invalid_serial"}"#.to_string());
        let err = post_activate(&test_agent(), &url, "CENT1-abc", &"f".repeat(64)).unwrap_err();
        handle.join().unwrap();
        assert_eq!(err, ActivationError::InvalidSerial);
    }

    #[test]
    fn post_activate_400_expired_serial() {
        let (url, handle) = spawn_mock_server(400, r#"{"error":"expired_serial"}"#.to_string());
        let err = post_activate(&test_agent(), &url, "CENT1-abc", &"f".repeat(64)).unwrap_err();
        handle.join().unwrap();
        assert_eq!(err, ActivationError::ExpiredSerial);
    }

    #[test]
    fn post_activate_400_bad_fingerprint() {
        let (url, handle) = spawn_mock_server(400, r#"{"error":"bad_fingerprint"}"#.to_string());
        let err = post_activate(&test_agent(), &url, "CENT1-abc", &"f".repeat(64)).unwrap_err();
        handle.join().unwrap();
        assert_eq!(err, ActivationError::BadFingerprint);
    }

    #[test]
    fn post_activate_429_maps_to_rate_limited() {
        let (url, handle) = spawn_mock_server(429, "{}".to_string());
        let err = post_activate(&test_agent(), &url, "CENT1-abc", &"f".repeat(64)).unwrap_err();
        handle.join().unwrap();
        assert_eq!(err, ActivationError::RateLimited);
    }

    #[test]
    fn post_activate_unexpected_5xx_maps_to_server_error() {
        let (url, handle) = spawn_mock_server(500, "internal error".to_string());
        let err = post_activate(&test_agent(), &url, "CENT1-abc", &"f".repeat(64)).unwrap_err();
        handle.join().unwrap();
        assert_eq!(err.code(), "server_error");
    }

    #[test]
    fn post_activate_connection_refused_is_network_error() {
        // Nothing listening on this port - a real, synchronous connection
        // failure (matches provisioning.rs's own transport-error test
        // convention: a loopback port a test server never binds).
        let err = post_activate(&test_agent(), "http://127.0.0.1:1", "CENT1-abc", &"f".repeat(64)).unwrap_err();
        assert_eq!(err.code(), "network");
    }

    // ---- activate_and_persist: full flow, license file side effects ---

    #[test]
    fn full_flow_200_good_license_writes_file_and_returns_features() {
        let payload = sample_payload_json(&["blf_console", "transcription"], 5);
        let container = sign_container(&test_activation_signing_key(), &payload);
        let container_value: serde_json::Value = serde_json::from_slice(&container).unwrap();
        let body = serde_json::json!({ "license": container_value }).to_string();
        let (url, handle) = spawn_mock_server(200, body);

        let dir = tempdir();
        let license_path = dir.join("license.json");
        let _guard = env_lock();
        std::env::set_var(MACHINE_ID_OVERRIDE_ENV, "test-machine");
        let result = activate_and_persist(&license_path, &url, "CENT1-abcdef");
        std::env::remove_var(MACHINE_ID_OVERRIDE_ENV);
        handle.join().unwrap();

        let outcome = result.unwrap();
        assert_eq!(outcome.customer, "Test Customer");
        assert_eq!(outcome.features, vec!["blf_console", "transcription"]);
        assert_eq!(outcome.seats, 5);
        assert!(license_path.is_file(), "license.json must be written on a good 200");
        let on_disk = std::fs::read_to_string(&license_path).unwrap();
        assert!(on_disk.contains("payload"), "on-disk file must be the signed container");
        cleanup_tempdir(dir);
    }

    #[test]
    fn full_flow_200_bad_signature_errors_and_does_not_write() {
        let payload = sample_payload_json(&["blf_console"], 1);
        // Signed with the WRONG key - the server response itself is
        // well-formed JSON, but the signature will never verify against
        // this shell's embedded ACTIVATION_PUBKEY_BYTES.
        let container = sign_container(&SigningKey::from_bytes(&WRONG_SEED), &payload);
        let container_value: serde_json::Value = serde_json::from_slice(&container).unwrap();
        let body = serde_json::json!({ "license": container_value }).to_string();
        let (url, handle) = spawn_mock_server(200, body);

        let dir = tempdir();
        let license_path = dir.join("license.json");
        let _guard = env_lock();
        std::env::set_var(MACHINE_ID_OVERRIDE_ENV, "test-machine");
        let result = activate_and_persist(&license_path, &url, "CENT1-abcdef");
        std::env::remove_var(MACHINE_ID_OVERRIDE_ENV);
        handle.join().unwrap();

        assert_eq!(result.unwrap_err(), ActivationError::LocalVerifyFailed);
        assert!(!license_path.exists(), "a bad-signature 200 must never write license.json");
        cleanup_tempdir(dir);
    }

    #[test]
    fn full_flow_existing_license_untouched_on_any_failure() {
        // A prior successful activation already wrote a real file...
        let dir = tempdir();
        let license_path = dir.join("license.json");
        std::fs::write(&license_path, b"previous-license-bytes").unwrap();

        // ...a later attempt against a server returning 409 must leave it
        // byte-for-byte alone.
        let (url, handle) = spawn_mock_server(409, r#"{"error":"seats_exhausted"}"#.to_string());
        let _guard = env_lock();
        std::env::set_var(MACHINE_ID_OVERRIDE_ENV, "test-machine");
        let result = activate_and_persist(&license_path, &url, "CENT1-abcdef");
        std::env::remove_var(MACHINE_ID_OVERRIDE_ENV);
        handle.join().unwrap();

        assert_eq!(result.unwrap_err(), ActivationError::SeatsExhausted);
        assert_eq!(std::fs::read(&license_path).unwrap(), b"previous-license-bytes");
        cleanup_tempdir(dir);
    }

    #[test]
    fn full_flow_network_down_leaves_prior_license_intact() {
        let dir = tempdir();
        let license_path = dir.join("license.json");
        std::fs::write(&license_path, b"previous-license-bytes").unwrap();

        let _guard = env_lock();
        std::env::set_var(MACHINE_ID_OVERRIDE_ENV, "test-machine");
        // Loopback port nothing is listening on.
        let result = activate_and_persist(&license_path, "http://127.0.0.1:1", "CENT1-abcdef");
        std::env::remove_var(MACHINE_ID_OVERRIDE_ENV);

        assert_eq!(result.unwrap_err().code(), "network");
        assert_eq!(std::fs::read(&license_path).unwrap(), b"previous-license-bytes");
        cleanup_tempdir(dir);
    }

    #[test]
    fn full_flow_bad_url_rejected_before_any_network_call() {
        let dir = tempdir();
        let license_path = dir.join("license.json");
        let result = activate_and_persist(&license_path, "http://not-localhost.example.test", "CENT1-abcdef");
        assert_eq!(result.unwrap_err().code(), "bad_url");
        assert!(!license_path.exists());
        cleanup_tempdir(dir);
    }

    #[test]
    fn full_flow_empty_serial_rejected() {
        let dir = tempdir();
        let license_path = dir.join("license.json");
        let result = activate_and_persist(&license_path, "https://activation.example.test", "   ");
        assert_eq!(result.unwrap_err(), ActivationError::InvalidSerial);
        cleanup_tempdir(dir);
    }

    // ---- tiny per-test temp dir (no extra dev-dependency) --------------
    //
    // Uniqueness must not rely on the nanosecond timestamp alone: under
    // parallel `cargo test` execution (the default) two threads can call
    // `tempdir()` close enough together that a low-resolution system clock
    // returns the same `SystemTime::now()` value for both, colliding on
    // the same directory (QA/Ornith 4R, 2026-07-17: observed 1/9 runs -
    // one test's `settings.json` write made a sibling test's "must not
    // exist" assertion fail). A per-process `AtomicU64` counter is
    // guaranteed unique across every call in this process regardless of
    // clock resolution; `process::id()` is kept alongside it so two
    // separate `cargo test` processes (e.g. a stray leftover from a prior
    // run) still can't collide either.
    fn tempdir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut dir = std::env::temp_dir();
        let unique = format!("centinelo-activation-test-{}-{}-{}", std::process::id(), seq, rand_suffix());
        dir.push(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rand_suffix() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
    }

    fn cleanup_tempdir(dir: std::path::PathBuf) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

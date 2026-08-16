use tauri_build::{AppManifest, Attributes};

// The actual parsing logic lives in build_support/generate_handler_parser.rs
// so it can ALSO be included (separately) from
// src/generate_handler_parser_tests.rs and get real `cargo test`
// coverage - build.rs is its own build-script binary, never compiled
// under `cargo test`, so keeping the parser only here would leave it
// with zero test coverage no matter how many tests exist downstream of
// it. See that file's own doc comment for the RELIABILITY 4R finding
// this split fixes (a `]` inside a `// comment` used to be able to
// truncate the command list silently).
include!("build_support/generate_handler_parser.rs");

fn main() {
    // RISK finding, licensing agent, 2026-08-16: block a `--release`
    // build from silently shipping the dev/test placeholder signing keys.
    // See the function doc below for the full threat model and design.
    check_no_dev_placeholder_keys_in_release();

    // Re-run this build script - and therefore re-derive the ACL command
    // list - whenever `generate_handler!`'s own file (or the parser
    // itself) changes, so adding or removing a command can never leave
    // stale permissions behind.
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=build_support/generate_handler_parser.rs");

    let lib_rs = std::fs::read_to_string("src/lib.rs")
        .unwrap_or_else(|e| panic!("failed to read src/lib.rs for ACL command extraction: {e}"));
    let commands = commands_from_generate_handler(&lib_rs)
        .unwrap_or_else(|e| panic!("src/lib.rs: {e}"));

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

// ---------------------------------------------------------------------
// Release build key-gate (RISK finding, licensing agent, 2026-08-16)
// ---------------------------------------------------------------------
//
// `src/premium.rs`'s `LIB_PUBKEY_BYTES` and `src/activation.rs`'s
// `ACTIVATION_PUBKEY_BYTES` both ship as documented dev/test placeholders
// (see each constant's own "# DEV/TEST PLACEHOLDER" doc comment, and
// `premium/docs/PACKAGING-SIGNATURES.md`, "Where the real public key
// goes"). Today that is fine - there is no paying customer yet, and the
// license/dylib model degrades to free mode by design either way. The day
// someone ships a `--release` build meant for a real customer without
// swapping those two constants first, this shell would accept a
// `centinelo_premium` dylib (or an activation-issued license) signed with
// a WELL-KNOWN, PUBLICLY-DOCUMENTED private key as if it were Felix's
// real one - full native-code execution inside the softphone process
// (SIP credentials, live call audio), silently. That swap is today a
// doc-comment reminder, not something the build itself enforces.
//
// This turns it into a build failure, but ONLY in the one situation the
// threat above actually requires:
//
//   1. `PROFILE != "release"` (cargo sets this env var for every build
//      script automatically) -> does nothing. Ordinary `cargo build`,
//      `cargo check`, `cargo clippy --all-targets`, and `cargo tauri dev`
//      all use the debug profile and are never touched - this cannot
//      slow down or block day-to-day development.
//   2. `PROFILE == "release"` AND `CENTINELO_ALLOW_DEV_SIGNING_KEYS` is
//      set (to "1" or "true") -> does nothing but print a
//      `cargo:warning=` so the override is still visible in every build
//      log it applies to, never silent. This is the escape hatch three
//      EXISTING, legitimate release-profile paths need today and must
//      keep working:
//        - `phone/.github/workflows/shell-build.yml` (every-PR
//          build-verification job - the actual merge gate)
//        - `phone/.github/workflows/release.yml` (public Community
//          edition - ships in release profile FOREVER with the
//          placeholder key, since Community never bundles a premium
//          dylib at all; the key is dead weight there, not a risk)
//        - `phone/.github/workflows/windows-installer.yml`'s
//          `resources-mechanism-smoke-test` job (synthetic-fixture
//          packaging smoke test, not a real signed dylib)
//        - `premium/.github/workflows/official-windows-build.yml`
//          (devsigned smoke test - deliberately signs a REAL dylib with
//          the SAME well-known dev seed to prove the packaging pipeline
//          end-to-end; see that workflow's own header comment)
//   3. `PROFILE == "release"`, no escape hatch, and the constants still
//      hold the dev/test placeholder bytes -> hard build failure with an
//      actionable message. This is the state that must be unreachable
//      for any build actually destined for a customer.
//
// Why an opt-in escape hatch rather than trying to detect "is this an
// official release" some other way: cargo/build.rs has no reliable
// signal for that (there is no dedicated "official release" workflow
// today - PACKAGING-SIGNATURES.md documents that the real release is
// assembled BY HAND, offline, on a machine Felix controls). Requiring an
// opt-IN flag for enforcement (rather than an opt-OUT escape hatch)
// would reproduce the exact bug this exists to close: nothing would
// enforce anything until a future release process remembered to add a
// flag. Fail-closed-by-default with a named, loud opt-out is the
// stronger guarantee: a brand-new "build the real Pro release" step that
// does nothing special gets blocked by default, and the escape hatch is
// something a human has to type on purpose - not something a real
// release build would ever have a reason to set, since once
// `LIB_PUBKEY_BYTES`/`ACTIVATION_PUBKEY_BYTES` hold Felix's real public
// keys (a normal, public `phone` commit per PACKAGING-SIGNATURES.md)
// this check passes on its own and the escape hatch is unnecessary.
//
// Discarded alternative: gate on `PROFILE == "release"` alone, with no
// escape hatch. Rejected because release.yml's Community edition and
// shell-build.yml's PR gate are BOTH already-shipping, legitimate
// release-profile builds that will hold the placeholder key forever (or
// until the real swap) - gating on profile alone would break the actual
// merge gate on this very PR, not just some hypothetical future misuse.
fn check_no_dev_placeholder_keys_in_release() {
    println!("cargo:rerun-if-env-changed=CENTINELO_ALLOW_DEV_SIGNING_KEYS");
    println!("cargo:rerun-if-changed=src/premium.rs");
    println!("cargo:rerun-if-changed=src/activation.rs");

    let profile = std::env::var("PROFILE").unwrap_or_default();
    if profile != "release" {
        return;
    }

    if let Ok(allow) = std::env::var("CENTINELO_ALLOW_DEV_SIGNING_KEYS") {
        if allow == "1" || allow.eq_ignore_ascii_case("true") {
            println!(
                "cargo:warning=CENTINELO_ALLOW_DEV_SIGNING_KEYS is set - this --release build \
                 is allowed to ship the dev/test placeholder Ed25519 keys \
                 (LIB_PUBKEY_BYTES / ACTIVATION_PUBKEY_BYTES). Correct for CI build \
                 verification, Community edition, or a devsigned smoke test. NOT correct for \
                 a real customer-facing Pro release - see build.rs's \
                 check_no_dev_placeholder_keys_in_release for the full rule."
            );
            return;
        }
    }

    let premium_src = std::fs::read_to_string("src/premium.rs")
        .unwrap_or_else(|e| panic!("release key-gate check: failed to read src/premium.rs: {e}"));
    let activation_src = std::fs::read_to_string("src/activation.rs").unwrap_or_else(|e| {
        panic!("release key-gate check: failed to read src/activation.rs: {e}")
    });

    let mut offenders: Vec<&str> = Vec::new();

    match extract_const_bytes(&premium_src, "LIB_PUBKEY_BYTES") {
        Ok(bytes) => {
            if bytes.len() != 32 {
                panic!(
                    "release key-gate check: premium.rs's LIB_PUBKEY_BYTES parsed to {} bytes, \
                     expected 32 - the extractor or the constant's format drifted; fix \
                     extract_const_bytes in build.rs",
                    bytes.len()
                );
            }
            if bytes == DEV_LIB_PUBKEY_BYTES {
                offenders.push("premium.rs's LIB_PUBKEY_BYTES");
            }
        }
        Err(e) => panic!("release key-gate check: {e}"),
    }

    match extract_const_bytes(&activation_src, "ACTIVATION_PUBKEY_BYTES") {
        Ok(bytes) => {
            if bytes.len() != 32 {
                panic!(
                    "release key-gate check: activation.rs's ACTIVATION_PUBKEY_BYTES parsed to \
                     {} bytes, expected 32 - the extractor or the constant's format drifted; \
                     fix extract_const_bytes in build.rs",
                    bytes.len()
                );
            }
            if bytes == DEV_ACTIVATION_PUBKEY_BYTES {
                offenders.push("activation.rs's ACTIVATION_PUBKEY_BYTES");
            }
        }
        Err(e) => panic!("release key-gate check: {e}"),
    }

    if !offenders.is_empty() {
        panic!(
            "\n\n\
             ================================================================\n\
             RELEASE BUILD BLOCKED: dev/test placeholder signing key(s) found\n\
             ================================================================\n\
             \n\
             This is a --release build (PROFILE=release) and the following \n\
             constant(s) still hold the well-known, PUBLICLY-DOCUMENTED \n\
             dev/test Ed25519 key(s), not Felix's real one:\n\
             \n  - {}\n\
             \n\
             Shipping this build to a real customer would let anyone who \n\
             knows the (public) dev seed sign their own premium module or \n\
             activation-issued license and have this shell load/accept it \n\
             as genuine - see premium/docs/PACKAGING-SIGNATURES.md, \n\
             \"Where the real public key goes\".\n\
             \n\
             To fix, before a real release: generate Felix's real \n\
             library-integrity / activation keypair(s) offline, replace \n\
             the placeholder bytes with the real public half, and re-sign \n\
             the dylib with the matching real private key. Full steps in \n\
             premium/docs/PACKAGING-SIGNATURES.md.\n\
             \n\
             If this IS a deliberate non-shipping release-profile build \n\
             (CI build verification, Community edition, a devsigned smoke \n\
             test, or local perf testing), set \n\
             CENTINELO_ALLOW_DEV_SIGNING_KEYS=1 to proceed.\n\
             ================================================================\n",
            offenders.join(", ")
        );
    }
}

// `extract_const_bytes` + the known dev/test placeholder byte arrays live
// in build_support/key_gate.rs, `include!`d here AND (separately) from
// src/key_gate_tests.rs, for the same reason build.rs already does this
// for build_support/generate_handler_parser.rs (see this file's own top
// comment and that file's header): build.rs is its own build-script
// binary that `cargo test` never compiles, so keeping this logic only
// here would leave it with zero test coverage no matter how many tests
// exist downstream of it.
include!("build_support/key_gate.rs");

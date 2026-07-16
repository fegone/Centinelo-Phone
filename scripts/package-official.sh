#!/usr/bin/env bash
# scripts/package-official.sh — F5 official-build packaging skeleton.
#
# Assembles the "official" Centinelo Phone bundle: core (baresip/libre +
# ctrl_json, this public repo) + shell (Tauri, this public repo) + premium
# module (dylib/dll + signature + console-ui assets, private repo — NOT
# checked out here). See premium/docs/SPEC-2026-07-15-centinelo-2.0-design.md
# §2 and shell/README.md "Premium module loader"/"Premium console window"
# for the full design this mirrors.
#
# Status: DRAFT / not wired into CI yet. Exercised locally on macOS against
# real core+shell builds and synthetic (non-secret) premium fixtures — see
# .claude/reports/release-ci-2026-07-16-f5-prep.md for the exact commands
# and what they produced. Not yet run against a *real* signed premium dylib
# (that requires Felix's offline signing key, which never touches this repo
# or CI) or on Windows (no Windows machine available this pass — the
# Windows branch below is written to the same contract core-build.yml's
# Windows job already proves builds/links, but is unexercised by this
# script specifically).
#
# 🚨 This script NEVER embeds, generates, or receives a signing key. It only
# copies already-built, already-signed artifacts whose paths are passed in
# by the caller (CLI flags or env vars) or a real CI secret store. Nothing
# here is a private-repo checkout, and nothing here is internal (IPs,
# hostnames) — safe to run from the public repo's CI.
#
# Usage:
#   scripts/package-official.sh [options]
#
# Options:
#   --target {macos|windows}       Build target. Default: current OS (uname).
#   --skip-core-build               Reuse an existing core/deps/baresip/build
#                                    instead of rebuilding it (faster local
#                                    iteration; CI should NOT pass this).
#   --skip-shell-build              Reuse an existing shell/src-tauri/target
#                                    build instead of rebuilding it (ditto).
#   --premium-dylib PATH            Signed premium dylib/dll. Omit for a
#                                    Community-edition package (no premium
#                                    module — shell degrades to free mode,
#                                    same as a missing dylib at runtime, see
#                                    shell/README.md "Premium module loader").
#   --premium-sig PATH              REQUIRED if --premium-dylib is given.
#                                    The dylib's Ed25519 .sig side-car.
#   --premium-console-assets DIR    premium/console-ui/src (private repo) —
#                                    optional; omitting it means the console
#                                    window stays gated even if the dylib
#                                    unlocks the capability (assets missing).
#   --output-dir DIR                Where to stage/report the final layout.
#                                    Default: dist/<target>/.
#   -h, --help                      Show this help.
#
# Secrets/paths are ALWAYS passed as parameters (CLI flags) or env vars,
# NEVER hardcoded. In a real CI job these would come from repository/
# environment secrets that hold *paths inside a private-checkout workspace*
# (or artifact download locations) — the actual signing key stays offline
# with Felix per CLAUDE.md's release rules; this script only ever touches
# its *output* (the already-signed dylib + .sig).
set -euo pipefail

# ---------------------------------------------------------------------------
# 0. Argument parsing
# ---------------------------------------------------------------------------

TARGET=""
SKIP_CORE_BUILD=0
SKIP_SHELL_BUILD=0
PREMIUM_DYLIB=""
PREMIUM_SIG=""
PREMIUM_CONSOLE_ASSETS=""
OUTPUT_DIR=""

usage() {
    sed -n '2,55p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)
            TARGET="$2"; shift 2 ;;
        --skip-core-build)
            SKIP_CORE_BUILD=1; shift ;;
        --skip-shell-build)
            SKIP_SHELL_BUILD=1; shift ;;
        --premium-dylib)
            PREMIUM_DYLIB="$2"; shift 2 ;;
        --premium-sig)
            PREMIUM_SIG="$2"; shift 2 ;;
        --premium-console-assets)
            PREMIUM_CONSOLE_ASSETS="$2"; shift 2 ;;
        --output-dir)
            OUTPUT_DIR="$2"; shift 2 ;;
        -h|--help)
            usage; exit 0 ;;
        *)
            echo "error: unknown argument: $1" >&2
            usage
            exit 1 ;;
    esac
done

# ---------------------------------------------------------------------------
# 1. Resolve repo root + target platform
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

if [[ -z "$TARGET" ]]; then
    case "$(uname -s)" in
        Darwin) TARGET="macos" ;;
        MINGW*|MSYS*|CYGWIN*) TARGET="windows" ;;
        *) echo "error: cannot auto-detect --target on $(uname -s); pass --target explicitly" >&2; exit 1 ;;
    esac
fi
case "$TARGET" in
    macos|windows) ;;
    *) echo "error: --target must be 'macos' or 'windows' (got: $TARGET)" >&2; exit 1 ;;
esac

if [[ -z "$OUTPUT_DIR" ]]; then
    OUTPUT_DIR="$REPO_ROOT/dist/$TARGET"
fi

echo "== package-official.sh =="
echo "target:      $TARGET"
echo "repo root:   $REPO_ROOT"
echo "output dir:  $OUTPUT_DIR"
echo

# ---------------------------------------------------------------------------
# 2. Validate premium artifact combination (dylib and sig are a pair)
# ---------------------------------------------------------------------------

IS_COMMUNITY=1
if [[ -n "$PREMIUM_DYLIB" || -n "$PREMIUM_SIG" ]]; then
    if [[ -z "$PREMIUM_DYLIB" || -z "$PREMIUM_SIG" ]]; then
        echo "error: --premium-dylib and --premium-sig must be given together (got only one)" >&2
        exit 1
    fi
    [[ -f "$PREMIUM_DYLIB" ]] || { echo "error: --premium-dylib not found: $PREMIUM_DYLIB" >&2; exit 1; }
    [[ -f "$PREMIUM_SIG"   ]] || { echo "error: --premium-sig not found: $PREMIUM_SIG" >&2; exit 1; }
    IS_COMMUNITY=0
fi
if [[ -n "$PREMIUM_CONSOLE_ASSETS" ]]; then
    [[ -d "$PREMIUM_CONSOLE_ASSETS" ]] || { echo "error: --premium-console-assets not a directory: $PREMIUM_CONSOLE_ASSETS" >&2; exit 1; }
fi

if [[ "$IS_COMMUNITY" -eq 1 ]]; then
    echo "No --premium-dylib given -> building a COMMUNITY edition (no premium module)."
    echo "This is a supported, intentional mode (shell/README.md 'Premium module"
    echo "loader': a missing dylib degrades to free mode, never fails startup)."
    echo
fi

# ---------------------------------------------------------------------------
# 3. Build core/ (skip with --skip-core-build to reuse an existing build)
# ---------------------------------------------------------------------------
# Mirrors core/BUILD.md steps 2-4 exactly (macOS path shown; the Windows CI
# job in .github/workflows/core-build.yml uses a different re-install-prefix
# dance — see core/BUILD.md "Windows CI" for why — not yet ported into this
# script's --target windows branch, see "Known gaps" at the bottom).

CORE_BUILD_DIR="$REPO_ROOT/core/deps/baresip/build"
CORE_BIN_NAME="baresip"
[[ "$TARGET" == "windows" ]] && CORE_BIN_NAME="baresip.exe"

if [[ "$SKIP_CORE_BUILD" -eq 1 ]]; then
    echo "-- Skipping core build (--skip-core-build); expecting an existing build at:"
    echo "   $CORE_BUILD_DIR"
    [[ -f "$CORE_BUILD_DIR/$CORE_BIN_NAME" ]] || {
        echo "error: --skip-core-build given but $CORE_BUILD_DIR/$CORE_BIN_NAME does not exist" >&2
        exit 1
    }
elif [[ "$TARGET" == "macos" ]]; then
    echo "-- Building core/ (macOS) — see core/BUILD.md for the same steps run by hand"
    git submodule update --init --recursive
    git apply --directory=core/deps/re core/patches/0001-re-configurable-sip-ws-path.patch 2>/dev/null || true
    git apply --directory=core/deps/re core/patches/0002-re-tls-fingerprint-pin.patch 2>/dev/null || true
    git apply --directory=core/deps/baresip core/patches/0003-baresip-json-stdout-purity.patch 2>/dev/null || true
    git apply --directory=core/deps/re core/patches/0004-re-json-stdout-purity.patch 2>/dev/null || true
    OPENSSL_PREFIX="$(brew --prefix openssl@3 2>/dev/null || brew --prefix openssl)"
    cmake -S core/deps/re -B core/deps/re/build -DCMAKE_BUILD_TYPE=Release \
        -DOPENSSL_ROOT_DIR="$OPENSSL_PREFIX"
    cmake --build core/deps/re/build -j"$(sysctl -n hw.ncpu)"
    cmake -S core/deps/baresip -B core/deps/baresip/build -DCMAKE_BUILD_TYPE=Release \
        -DOPENSSL_ROOT_DIR="$OPENSSL_PREFIX" \
        -DMODULES="account;g711;auconv;auresamp;ausine;aufile;ice;dtls_srtp;menu" \
        -DAPP_MODULES="ctrl_json" -DAPP_MODULES_DIR="$REPO_ROOT/core/modules"
    cmake --build core/deps/baresip/build -j"$(sysctl -n hw.ncpu)"
else
    echo "error: --target windows core build is not implemented in this skeleton yet." >&2
    echo "       Use --skip-core-build with a Windows core/deps/baresip/build produced" >&2
    echo "       by .github/workflows/core-build.yml's Windows job commands (see" >&2
    echo "       core/BUILD.md 'Windows CI' for the exact re-install-prefix sequence" >&2
    echo "       — porting it here is a follow-up, see 'Known gaps' in this file's" >&2
    echo "       header)." >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 4. Build shell/ (skip with --skip-shell-build to reuse an existing build)
# ---------------------------------------------------------------------------

if [[ "$SKIP_SHELL_BUILD" -eq 1 ]]; then
    echo "-- Skipping shell build (--skip-shell-build)"
else
    echo "-- Building shell/ (tauri build, release)"
    (cd shell && npm install && npx tauri build --bundles "$([[ "$TARGET" == "macos" ]] && echo app || echo none)")
fi

# ---------------------------------------------------------------------------
# 5. Locate the built executable's directory (where premium artifacts land)
# ---------------------------------------------------------------------------
# Mirrors centinelo-premium-abi::expected_library_path: "directly beside the
# running executable" — macOS .app/Contents/MacOS/, Windows install dir
# (here: the raw cargo/tauri release output dir; a real installer's final
# install path is a later, not-yet-built step, see "Known gaps").

if [[ "$TARGET" == "macos" ]]; then
    APP_BUNDLE="$(find "$REPO_ROOT/shell/src-tauri/target/release/bundle/macos" -maxdepth 1 -name '*.app' | head -n1)"
    if [[ -z "$APP_BUNDLE" ]]; then
        echo "error: no .app bundle found under shell/src-tauri/target/release/bundle/macos" >&2
        exit 1
    fi
    EXE_DIR="$APP_BUNDLE/Contents/MacOS"
    PREMIUM_LIB_NAME="libcentinelo_premium.dylib"
else
    EXE_DIR="$REPO_ROOT/shell/src-tauri/target/release"
    PREMIUM_LIB_NAME="centinelo_premium.dll"
fi
[[ -d "$EXE_DIR" ]] || { echo "error: expected exe dir does not exist: $EXE_DIR" >&2; exit 1; }
echo "-- exe dir: $EXE_DIR"

# Filename mapping above must stay in sync with
# shell/src-tauri/centinelo-premium-abi/src/paths.rs::expected_library_filename()
# — there is no shared source between this bash script and that Rust file
# today; if that function's naming changes, update this mapping to match.

# ---------------------------------------------------------------------------
# 6. Copy core engine binary + modules beside the exe
# ---------------------------------------------------------------------------
# KNOWN GAP (see header + bottom "Known gaps"): shell/src-tauri/src/sidecar.rs
# `default_core_binary_path()` only walk-up-searches a *dev* checkout layout
# (`core/deps/baresip/build/baresip` relative to cwd/exe) — it does NOT yet
# look beside the exe in an installed layout. Dropping the binary here is
# necessary but not sufficient: today an installed build still needs
# Settings > Advanced > core binary path (or CENTINELO_CORE_BIN at launch)
# pointed at this directory by hand. Flagged for shell-tauri to close before
# F5 ships to anyone but Felix/Edgar.
CORE_ENGINE_DIR="$EXE_DIR/core-engine"
mkdir -p "$CORE_ENGINE_DIR"
cp "$CORE_BUILD_DIR/$CORE_BIN_NAME" "$CORE_ENGINE_DIR/"
# Every module baresip's own CMake symlinked flat into build/ (see
# core/BUILD.md step 4b) — ship them all; ctrl_json.so/.dll is the one the
# shell actually depends on, the rest are baresip's own runtime deps for
# the module set core/BUILD.md documents.
find "$CORE_BUILD_DIR" -maxdepth 1 -type f \( -name '*.so' -o -name '*.dylib' -o -name '*.dll' \) -exec cp {} "$CORE_ENGINE_DIR/" \;
echo "-- copied core engine binary + modules -> $CORE_ENGINE_DIR"

# ---------------------------------------------------------------------------
# 7. Copy premium dylib + .sig (Pro builds only)
# ---------------------------------------------------------------------------

if [[ "$IS_COMMUNITY" -eq 0 ]]; then
    cp "$PREMIUM_DYLIB" "$EXE_DIR/$PREMIUM_LIB_NAME"
    cp "$PREMIUM_SIG" "$EXE_DIR/$PREMIUM_LIB_NAME.sig"
    echo "-- copied premium dylib+sig -> $EXE_DIR/$PREMIUM_LIB_NAME(.sig)"
else
    echo "-- no premium dylib to copy (Community edition)"
fi

# ---------------------------------------------------------------------------
# 8. Copy premium-console-assets/ (optional even in Pro builds)
# ---------------------------------------------------------------------------

if [[ -n "$PREMIUM_CONSOLE_ASSETS" ]]; then
    CONSOLE_DEST="$EXE_DIR/premium-console-assets"
    rm -rf "$CONSOLE_DEST"
    cp -r "$PREMIUM_CONSOLE_ASSETS" "$CONSOLE_DEST"
    echo "-- copied premium console-ui assets -> $CONSOLE_DEST"
else
    echo "-- no premium console assets to copy (console window stays gated even if licensed)"
fi

# ---------------------------------------------------------------------------
# 9. Report the final layout
# ---------------------------------------------------------------------------

mkdir -p "$OUTPUT_DIR"
LAYOUT_FILE="$OUTPUT_DIR/layout.txt"
{
    echo "package-official.sh — final layout ($TARGET, $([[ "$IS_COMMUNITY" -eq 1 ]] && echo community || echo pro))"
    echo "exe dir: $EXE_DIR"
    echo
    find "$EXE_DIR" -maxdepth 2 | sort
} > "$LAYOUT_FILE"
echo
echo "== Done. Layout written to $LAYOUT_FILE =="
cat "$LAYOUT_FILE"

# ---------------------------------------------------------------------------
# Known gaps (honest, not hidden — see also inline comments above):
#   1. --target windows does not build core itself (needs the re-install-
#      prefix dance core-build.yml's Windows job already runs — porting it
#      here is a follow-up, not done this pass; use --skip-core-build with
#      a CI-produced Windows build in the meantime).
#   2. No installer step (DMG signing/notarization, Windows MSI/NSIS +
#      OV cert signing) — this script only produces the flat "everything
#      beside the exe" layout `cargo tauri build` already makes; wiring a
#      real installer artifact is separate F5 work, and OS code-signing is
#      explicitly deferred until public launch per CLAUDE.md.
#   3. shell/src-tauri/src/sidecar.rs's default_core_binary_path() doesn't
#      look in the installed-layout `core-engine/` subdir this script
#      creates yet — an installed build still needs a manual Settings >
#      Advanced path override (or CENTINELO_CORE_BIN) until shell-tauri
#      wires that lookup, OR this moves to Tauri's own `externalBin`
#      sidecar mechanism (tauri.conf.json has no `bundle.externalBin`
#      entry today) — either is a shell-tauri decision, not made here.
#   4. Not wired into any GitHub Actions workflow. The private-repo access
#      question ("build oficial = con acceso al repo privado" per
#      .claude/skills/release-ci/SKILL.md) — submodule vs. artifact-
#      download vs. separate private-repo-triggered workflow — is a Felix
#      decision, not made by this script.
#   5. NOT idempotent / no clean step: re-running against an exe dir that
#      already has premium artifacts from a previous run (e.g. packaging a
#      Community build right after a Pro build, same output tree) leaves
#      the old premium-console-assets/libcentinelo_premium.* files in
#      place instead of removing them — confirmed by testing exactly this
#      sequence locally (see the report). Always package into a fresh
#      `tauri build` output, or add an explicit clean step, before this is
#      used for anything but manual local testing.
# ---------------------------------------------------------------------------

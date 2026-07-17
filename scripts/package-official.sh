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
# Status: DRAFT / partially wired into CI. Exercised locally on macOS against
# real core+shell builds and synthetic (non-secret) premium fixtures — see
# .claude/reports/release-ci-2026-07-16-f5-prep.md (main report + 4R-fix
# appendix) for the exact commands and what they produced, including a
# dedicated symlink-fixture re-test (see "Known gaps" #1 below). Not yet run
# against a *real* signed premium dylib (that requires Felix's offline
# signing key, which never touches this repo or CI) or a *real* Windows core
# build via THIS script specifically — core-win (windows-media-modules,
# ausine/aufile/ice/dtls_srtp/wasapi) merged to v2 at 47df112 and its own
# core-build.yml job is green, but --target windows here still hard-fails
# before reaching that build (see "Known gaps" #1) since porting the
# re-install-prefix dance itself into this script wasn't done this pass.
#
# Windows target (added 2026-07-16, windows-installer pass): unlike macOS —
# where the .app is just a directory, so copying files in after `tauri
# build` works fine — NSIS/MSI installers are built from a manifest
# (`bundle.resources` in tauri.conf.json), not by scanning whatever's sitting
# in target/release/ afterward. So on Windows this script stages artifacts
# into shell/dist-injected/ (gitignored) instead of beside a pre-built exe,
# and the actual installer only gets built at the very end (step 9b), via
# `tauri build --config tauri.official.generated.json` — step 9a generates
# that file by filtering shell/src-tauri/tauri.official.conf.json (a
# template, sibling to tauri.conf.json, checked in) down to only the
# bundle.resources entries this run actually staged (4R/RELIABILITY fix
# 2026-07-16: the template alone unconditionally declared every resource,
# which made tauri build hard-fail for Community/Pro-without-console runs —
# see step 9a's own comment). tauri.conf.json's base config never declares
# `bundle.resources` at all, so plain community/dev builds and
# shell-build.yml's existing Windows CI job are entirely unaffected.
# Verified end-to-end with synthetic fixtures via
# .github/workflows/windows-installer.yml's resources-mechanism-smoke-test
# job (matrix over Community / Pro-without-console / Pro-with-console, run
# URL in the report) — not yet against a real core-win build or a real
# signed dylib.
#
# 🚨 This script NEVER embeds, generates, or receives a signing key. It only
# copies already-built, already-signed artifacts whose paths are passed in
# by the caller (CLI flags or env vars) or a real CI secret store. Nothing
# here is a private-repo checkout, and nothing here is internal (IPs,
# hostnames) — safe to run from the public repo's CI.
#
# See usage() below (or --help) for the flag reference.
#
# Secrets/paths are ALWAYS passed as parameters (CLI flags) or env vars,
# NEVER hardcoded. In a real CI job these would come from repository/
# environment secrets that hold *paths inside a private-checkout workspace*
# (or artifact download locations) — the actual signing key stays offline
# with Felix per CLAUDE.md's release rules; this script only ever touches
# its *output* (the already-signed dylib + .sig).
set -euo pipefail

# Captured before this script ever `cd`s anywhere (see "Resolve repo root"
# below) — every path-shaped flag is resolved against THIS, not against
# $REPO_ROOT, so `--output-dir ../foo` (or any relative path) means what the
# caller's own shell would mean by it, regardless of where in the repo this
# script happens to live or `cd` to later. 4R/RESILIENCE fix 2026-07-16:
# previously flags were parsed here but validated/used after the `cd
# "$REPO_ROOT"` below, so a relative path resolved against the wrong
# directory — silently wrong, not even a clean error.
ORIGINAL_PWD="$PWD"

# ---------------------------------------------------------------------------
# 0. Argument parsing
# ---------------------------------------------------------------------------

TARGET=""
SKIP_CORE_BUILD=0
SKIP_SHELL_BUILD=0
ALLOW_STALE_CORE=0
PREMIUM_DYLIB=""
PREMIUM_SIG=""
PREMIUM_CONSOLE_ASSETS=""
OUTPUT_DIR=""
OPENSSL_DLL_DIR=""
SKIP_OPENSSL_DLLS=0

# Self-contained --help text (4R/READABILITY fix 2026-07-16: this used to be
# `sed -n '2,55p' "$0"` against the header comment above — fragile hardcoded
# line range that silently truncated mid-sentence the moment the header
# comment grew or shrank, and dumped the whole design-rationale narrative
# into --help output instead of just the flag reference). This heredoc has
# no dependency on the file's line numbers at all.
usage() {
    cat <<'USAGE_EOF'
scripts/package-official.sh — F5 official-build packaging skeleton.

Assembles the official Centinelo Phone bundle: core + shell (this public
repo) + premium module (dylib/dll + signature + console-ui assets, private
repo — never checked out here). See
premium/docs/SPEC-2026-07-15-centinelo-2.0-design.md §2 and
shell/README.md "Premium module loader" / "Premium console window" for the
full design this mirrors.

Usage:
  scripts/package-official.sh [options]

Options:
  --target {macos|windows}       Build target. Default: current OS (uname).
                                   macOS: artifacts land beside the built
                                   .app's exe. Windows: artifacts are staged
                                   into shell/dist-injected/, then a real
                                   NSIS/MSI installer is built around them
                                   via `tauri build --config
                                   tauri.official.generated.json` (a
                                   filtered copy of tauri.official.conf.json
                                   this script generates - see "Windows
                                   target" in this file's header comment).
  --skip-core-build               Reuse an existing core/deps/baresip/build
                                   instead of rebuilding it (faster local
                                   iteration; CI should NOT pass this unless
                                   it built that binary itself earlier in the
                                   SAME job — see official-windows-build.yml
                                   in the private premium repo for that
                                   pattern). A freshness check compares the
                                   existing binary's mtime against the last
                                   commit that touched core/ and ABORTS if
                                   the binary looks stale (older than that
                                   commit) — see --allow-stale-core to
                                   override.
  --allow-stale-core              Explicit escape hatch: proceed even if
                                   --skip-core-build's freshness check finds
                                   the existing core binary older than the
                                   last commit touching core/. Only for
                                   local iteration when you KNOW the stale
                                   warning is a false positive (e.g.
                                   uncommitted local core/ changes you
                                   haven't rebuilt yet on purpose). CI should
                                   not need this — a CI job building core
                                   fresh earlier in the same job already
                                   passes the freshness check on its own.
  --skip-shell-build              Reuse an existing shell/src-tauri/target
                                   build instead of rebuilding it (ditto).
                                   On Windows this also skips building the
                                   final NSIS/MSI installer (step 9b) —
                                   artifacts are staged but no .msi/.exe is
                                   produced.
  --premium-dylib PATH            Signed premium dylib/dll. Omit for a
                                   Community-edition package (no premium
                                   module — shell degrades to free mode,
                                   same as a missing dylib at runtime, see
                                   shell/README.md "Premium module loader").
  --premium-sig PATH              REQUIRED if --premium-dylib is given.
                                   The dylib's Ed25519 .sig side-car.
  --premium-console-assets DIR    premium/console-ui/src (private repo) —
                                   optional; omitting it means the console
                                   window stays gated even if the dylib
                                   unlocks the capability (assets missing).
  --output-dir DIR                Where to stage/report the final layout.
                                   Default: dist/<target>/.
  --openssl-dll-dir DIR           Windows only, effectively REQUIRED: directory
                                   holding the OpenSSL runtime DLLs (e.g. the
                                   Chocolatey openssl package's bin/ dir,
                                   "$OPENSSL_ROOT_DIR\bin" in core-build.yml's
                                   Windows job). baresip.exe links OpenSSL
                                   dynamically even in its STATIC build and
                                   will not start without these DLLs beside
                                   it on a clean machine (no dev tools). Every
                                   *.dll directly in this dir is copied flat
                                   into core-engine/ beside baresip.exe, but
                                   the dir MUST contain at least one
                                   libssl-*.dll and one libcrypto-*.dll (case
                                   insensitive) - those two are what
                                   baresip.exe actually needs to start.
                                   Mutually exclusive with
                                   --skip-openssl-dlls.
  --skip-openssl-dlls             Explicit opt-out of the above, for local
                                   iteration only (e.g. exercising the rest
                                   of the packaging pipeline without a full
                                   OpenSSL install to hand). CI must NOT pass
                                   this for a build meant to run on a real
                                   machine. Mutually exclusive with
                                   --openssl-dll-dir.
  -h, --help                      Show this help.

Secrets/paths are ALWAYS CLI flags or env, NEVER hardcoded. This script
never sees or generates a signing key — it only copies already-signed
output. See this file's own top-of-file comments for full design notes,
and the "Known gaps" block at the bottom for what's still open.
USAGE_EOF
}

# 4R/RESILIENCE fix 2026-07-16: every flag below that consumes a value now
# checks `$#` before touching `$2` — previously `--target` as the last
# argument (no value after it) fell straight through to `TARGET="$2"` with
# nothing left to shift, surfacing bash's own raw `$2: unbound variable`
# instead of a clean, actionable error.
while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)
            [[ $# -ge 2 ]] || { echo "error: --target requires a value" >&2; exit 1; }
            TARGET="$2"; shift 2 ;;
        --skip-core-build)
            SKIP_CORE_BUILD=1; shift ;;
        --allow-stale-core)
            ALLOW_STALE_CORE=1; shift ;;
        --skip-shell-build)
            SKIP_SHELL_BUILD=1; shift ;;
        --premium-dylib)
            [[ $# -ge 2 ]] || { echo "error: --premium-dylib requires a value" >&2; exit 1; }
            PREMIUM_DYLIB="$2"; shift 2 ;;
        --premium-sig)
            [[ $# -ge 2 ]] || { echo "error: --premium-sig requires a value" >&2; exit 1; }
            PREMIUM_SIG="$2"; shift 2 ;;
        --premium-console-assets)
            [[ $# -ge 2 ]] || { echo "error: --premium-console-assets requires a value" >&2; exit 1; }
            PREMIUM_CONSOLE_ASSETS="$2"; shift 2 ;;
        --output-dir)
            [[ $# -ge 2 ]] || { echo "error: --output-dir requires a value" >&2; exit 1; }
            OUTPUT_DIR="$2"; shift 2 ;;
        --openssl-dll-dir)
            [[ $# -ge 2 ]] || { echo "error: --openssl-dll-dir requires a value" >&2; exit 1; }
            OPENSSL_DLL_DIR="$2"; shift 2 ;;
        --skip-openssl-dlls)
            SKIP_OPENSSL_DLLS=1; shift ;;
        -h|--help)
            usage; exit 0 ;;
        *)
            echo "error: unknown argument: $1" >&2
            usage
            exit 1 ;;
    esac
done

# 4R/RESILIENCE nit fix 2026-07-16: --openssl-dll-dir and --skip-openssl-dlls
# are opposite instructions for the same question ("where do the OpenSSL
# runtime DLLs come from") - giving both silently let --openssl-dll-dir win
# (it's checked first in section 2b below), hiding a caller mistake instead
# of surfacing it.
if [[ -n "$OPENSSL_DLL_DIR" && "$SKIP_OPENSSL_DLLS" -eq 1 ]]; then
    echo "error: --openssl-dll-dir and --skip-openssl-dlls are contradictory (both given) - pick one: either point at a real OpenSSL DLL dir, or explicitly skip bundling them for local-only iteration" >&2
    exit 1
fi

# Resolve every path-shaped flag to an absolute path against $ORIGINAL_PWD
# right here — before anything below `cd`s to $REPO_ROOT. 4R/RESILIENCE fix
# 2026-07-16, see the $ORIGINAL_PWD comment above for the bug this closes.
# Plain string-join, not a realpath/readlink call: OUTPUT_DIR in particular
# may not exist yet (this script creates it), so resolution must not
# require the path to already be real.
resolve_abs_path() {
    local p="$1"
    if [[ "$p" == /* ]]; then
        printf '%s\n' "$p"
    else
        printf '%s\n' "$ORIGINAL_PWD/$p"
    fi
}
[[ -n "$PREMIUM_DYLIB" ]] && PREMIUM_DYLIB="$(resolve_abs_path "$PREMIUM_DYLIB")"
[[ -n "$PREMIUM_SIG" ]] && PREMIUM_SIG="$(resolve_abs_path "$PREMIUM_SIG")"
[[ -n "$PREMIUM_CONSOLE_ASSETS" ]] && PREMIUM_CONSOLE_ASSETS="$(resolve_abs_path "$PREMIUM_CONSOLE_ASSETS")"
[[ -n "$OUTPUT_DIR" ]] && OUTPUT_DIR="$(resolve_abs_path "$OUTPUT_DIR")"
[[ -n "$OPENSSL_DLL_DIR" ]] && OPENSSL_DLL_DIR="$(resolve_abs_path "$OPENSSL_DLL_DIR")"

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
# Temp-file cleanup trap (used by the atomic premium-artifact copy in step 7)
# ---------------------------------------------------------------------------
# 4R/RESILIENCE fix 2026-07-16: registered here, before anything that could
# create a temp file, so a failure at ANY later step still cleans up.
CLEANUP_TMP_FILES=()
cleanup_tmp() {
    local exit_code=$?
    local f
    for f in "${CLEANUP_TMP_FILES[@]:-}"; do
        [[ -n "$f" && -e "$f" ]] && rm -f "$f"
    done
    # Explicit, unconditional return of the ORIGINAL exit code, not
    # whatever the last `[[ ... ]] && rm -f` in the loop above happened to
    # return (usually 1 - "nothing to clean up" - false by design once the
    # tmp files have already been mv'd into place). Caught by testing:
    # without this, a fully successful run's exit code got silently
    # clobbered to 1 by this trap, because it's an EXIT trap and its own
    # exit status becomes the shell's exit status when nothing after it
    # calls `exit` explicitly - confirmed by reproducing it in isolation
    # (a two-line trap+for-loop repro, same shape, same bug) before fixing
    # here.
    return "$exit_code"
}
trap cleanup_tmp EXIT

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
    # An existing-but-empty dir passes the -d check above yet still leaves
    # include_console=1 downstream (step 9a), keeping the
    # "premium-console-assets/*" glob in the generated Tauri resources
    # config with nothing on disk to match it - Tauri's bundler fails that
    # the exact same way as the original GlobPathNotFound blocker (4R/
    # RESILIENCE finding 2026-07-16). Fail fast here instead.
    [[ -n "$(find "$PREMIUM_CONSOLE_ASSETS" -mindepth 1 -print -quit)" ]] || { echo "error: --premium-console-assets directory is empty: $PREMIUM_CONSOLE_ASSETS" >&2; exit 1; }
fi

if [[ "$IS_COMMUNITY" -eq 1 ]]; then
    echo "No --premium-dylib given -> building a COMMUNITY edition (no premium module)."
    echo "This is a supported, intentional mode (shell/README.md 'Premium module"
    echo "loader': a missing dylib degrades to free mode, never fails startup)."
    echo
fi

# ---------------------------------------------------------------------------
# 2b. Validate OpenSSL DLL dir (Windows runtime requirement)
# ---------------------------------------------------------------------------
# core/ links OpenSSL dynamically on Windows even though baresip itself
# builds STATIC there - the static re/baresip libs still reference OpenSSL's
# import libs, not a statically-linked copy of libssl/libcrypto (see
# core-build.yml's Windows job "Sanity" step comment: "the static build
# still links OpenSSL's import libs and would need the OpenSSL DLLs on PATH
# at runtime"). Without libssl-3-x64.dll/libcrypto-3-x64.dll (or whichever
# names the installed OpenSSL version uses) sitting beside baresip.exe, the
# packaged core.exe fails to even start on a clean machine with no dev tools
# - this was the #1 blocker for Edgar's Windows beta (docs/HANDOFF.md). Fixed
# here (2026-07-16, release-ci pass) - closes "Known gaps" #1 at the bottom
# of this file.
#
# 4R/RESILIENCE fix 2026-07-16 (blocker #2): the original check here only
# asserted "at least one *.dll exists in the dir" - a directory containing
# the WRONG DLLs (e.g. only a legacy provider .dll, or a totally unrelated
# *.dll someone pointed --openssl-dll-dir at by mistake) passed this check
# and produced an installer that still fails to start baresip.exe, silently,
# on a clean machine. Now asserts the two basenames baresip.exe actually
# needs - libssl-*.dll and libcrypto-*.dll (case-insensitive: Chocolatey's
# openssl package, and OpenSSL builds in general, are not consistent about
# casing) - are each present by name, not just "some .dll exists".
if [[ "$TARGET" == "windows" ]]; then
    if [[ -n "$OPENSSL_DLL_DIR" ]]; then
        [[ -d "$OPENSSL_DLL_DIR" ]] || { echo "error: --openssl-dll-dir not a directory: $OPENSSL_DLL_DIR" >&2; exit 1; }
        shopt -s nullglob nocaseglob
        _openssl_libssl_check=("$OPENSSL_DLL_DIR"/libssl-*.dll)
        _openssl_libcrypto_check=("$OPENSSL_DLL_DIR"/libcrypto-*.dll)
        shopt -u nullglob nocaseglob
        if [[ ${#_openssl_libssl_check[@]} -eq 0 ]]; then
            echo "error: --openssl-dll-dir has no libssl-*.dll file: $OPENSSL_DLL_DIR" >&2
            echo "       (found: $(ls "$OPENSSL_DLL_DIR" 2>/dev/null | tr '\n' ' '))" >&2
            exit 1
        fi
        if [[ ${#_openssl_libcrypto_check[@]} -eq 0 ]]; then
            echo "error: --openssl-dll-dir has no libcrypto-*.dll file: $OPENSSL_DLL_DIR" >&2
            echo "       (found: $(ls "$OPENSSL_DLL_DIR" 2>/dev/null | tr '\n' ' '))" >&2
            exit 1
        fi
    elif [[ "$SKIP_OPENSSL_DLLS" -eq 1 ]]; then
        echo "-- --skip-openssl-dlls given: packaging a Windows build WITHOUT the OpenSSL"
        echo "   runtime DLLs. core.exe will NOT start on a machine without OpenSSL"
        echo "   already on PATH (dev machines only). Do not ship this."
        echo
    else
        echo "error: --target windows requires --openssl-dll-dir (or the explicit" >&2
        echo "       --skip-openssl-dlls opt-out for local-only iteration) - without" >&2
        echo "       it, the packaged core.exe cannot start on a clean end-user machine" >&2
        echo "       (missing libssl/libcrypto DLLs at runtime). See core-build.yml's" >&2
        echo "       Windows job for how it locates OPENSSL_ROOT_DIR; pass" >&2
        echo "       \"\$OPENSSL_ROOT_DIR\\bin\"." >&2
        exit 1
    fi
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
# core-build.yml's Windows job builds via MSVC (a multi-config generator),
# so `cmake --build --config Release` lands the exe one level deeper than
# the single-config Unix Makefiles macOS build does: core/deps/baresip/
# build/Release/baresip.exe, not .../build/baresip.exe (confirmed by
# reading that job's own "Sanity - baresip + ctrl_json built" step, which
# checks exactly that path). CORE_BIN_SUBDIR captures that difference so
# every consumer below (--skip-core-build's existence check, the copy in
# step 6) resolves the same real path instead of only working for macOS.
CORE_BIN_SUBDIR=""
if [[ "$TARGET" == "windows" ]]; then
    CORE_BIN_NAME="baresip.exe"
    CORE_BIN_SUBDIR="Release"
fi
CORE_BIN_PATH="$CORE_BUILD_DIR${CORE_BIN_SUBDIR:+/$CORE_BIN_SUBDIR}/$CORE_BIN_NAME"

# Applies one patch idempotently: skip cleanly if it's already applied
# (checked via `git apply --reverse --check`), apply it if it isn't, and —
# critically — surface the REAL error and fail the build if it's neither
# (a genuine conflict). 4R/READABILITY fix 2026-07-16: this replaces four
# `git apply ... 2>/dev/null || true` lines that swallowed every failure
# unconditionally, including real ones a CI run should fail loudly on, not
# just the intended "already applied, this is fine" case.
apply_patch() {
    local dir="$1" patch="$2"
    if git apply --directory="$dir" --check "$patch" 2>/dev/null; then
        git apply --directory="$dir" "$patch"
        echo "-- applied: $patch"
    elif git apply --directory="$dir" --reverse --check "$patch" 2>/dev/null; then
        echo "-- already applied, skipping: $patch"
    else
        echo "error: $patch does not apply cleanly to $dir, and is not already" >&2
        echo "       applied there either (both --check and --reverse --check" >&2
        echo "       failed). Re-running without --check to show the real conflict:" >&2
        git apply --directory="$dir" "$patch"
    fi
}

# Portable single-file mtime (epoch seconds) / commit-timestamp formatter —
# BSD stat/date (macOS, where most local iteration + the "elif macos" build
# below runs) and GNU stat/date (Windows CI runners, which invoke this
# script via Git Bash/MSYS — see official-windows-build.yml in the private
# premium repo) disagree on flag syntax, so try BSD first, then GNU.
file_mtime_epoch() {
    stat -f %m "$1" 2>/dev/null || stat -c %Y "$1" 2>/dev/null
}
format_epoch() {
    date -r "$1" 2>/dev/null || date -d "@$1" 2>/dev/null || printf '%s (epoch)\n' "$1"
}

# --skip-core-build freshness check: reusing a stale core/deps/baresip/build
# binary has reached qa-e2e TWICE (once pre-v1.3, once again the same day
# this check was added — see docs/HANDOFF.md) because nothing verified the
# binary sitting on disk still matched the source. Compares the existing
# binary's mtime against the last commit that touched core/ (source,
# patches, or the submodule pointers themselves) and aborts if the binary
# predates that commit — it cannot possibly contain that change. The one
# legitimate case this must NOT block is a CI job that builds core fresh in
# an earlier step of the SAME job, then calls this script with
# --skip-core-build right after (premium's official-windows-build.yml does
# exactly this): there the binary's mtime is always "just now", newer than
# any historical commit, so it passes this check on its own without needing
# --allow-stale-core at all.
check_core_freshness() {
    local last_core_commit_ts bin_mtime
    last_core_commit_ts="$(git log -1 --format=%ct -- core/ 2>/dev/null || true)"
    if [[ -z "$last_core_commit_ts" ]]; then
        echo "warning: could not determine the last commit touching core/ (shallow clone? no history?) — skipping the --skip-core-build freshness check" >&2
        return 0
    fi
    bin_mtime="$(file_mtime_epoch "$CORE_BIN_PATH")"
    if [[ -z "$bin_mtime" ]]; then
        echo "warning: could not read $CORE_BIN_PATH's mtime — skipping the --skip-core-build freshness check" >&2
        return 0
    fi
    if [[ "$bin_mtime" -ge "$last_core_commit_ts" ]]; then
        echo "-- core binary freshness OK: $CORE_BIN_PATH ($(format_epoch "$bin_mtime")) is newer than the last commit touching core/ ($(format_epoch "$last_core_commit_ts"))"
        return 0
    fi
    if [[ "$ALLOW_STALE_CORE" -eq 1 ]]; then
        echo "-- WARNING: $CORE_BIN_PATH ($(format_epoch "$bin_mtime")) is OLDER than the last commit touching core/ ($(format_epoch "$last_core_commit_ts")) — packaging it anyway because --allow-stale-core was given." >&2
        return 0
    fi
    echo "error: $CORE_BIN_PATH looks STALE — refusing to package it." >&2
    echo "       binary mtime:                $(format_epoch "$bin_mtime")" >&2
    echo "       last commit touching core/:  $(format_epoch "$last_core_commit_ts")" >&2
    echo "       core/ has changed since this binary was built, so --skip-core-build" >&2
    echo "       would silently ship a stale core (this has reached qa-e2e twice —" >&2
    echo "       see docs/HANDOFF.md). Drop --skip-core-build to rebuild, or if you" >&2
    echo "       are certain this binary is fine (e.g. it was built in an earlier" >&2
    echo "       step of this same CI job), pass --allow-stale-core explicitly." >&2
    exit 1
}

if [[ "$SKIP_CORE_BUILD" -eq 1 ]]; then
    echo "-- Skipping core build (--skip-core-build); expecting an existing build at:"
    echo "   $CORE_BIN_PATH"
    [[ -f "$CORE_BIN_PATH" ]] || {
        echo "error: --skip-core-build given but $CORE_BIN_PATH does not exist" >&2
        exit 1
    }
    check_core_freshness
elif [[ "$TARGET" == "macos" ]]; then
    echo "-- Building core/ (macOS) — see core/BUILD.md for the same steps run by hand"
    # 4R/RELIABILITY fix: force a clean build every time --skip-core-build is
    # NOT given, instead of letting CMake's incremental build reuse whatever
    # was already sitting in these dirs from a previous, possibly-different
    # run. An incremental build was the OTHER way a stale core binary reached
    # qa-e2e (docs/HANDOFF.md) — e.g. a patch that failed to apply cleanly
    # the second time, or a MODULES list that changed between runs, could
    # leave a build/ dir whose binary doesn't reflect the config this run
    # asked for, and CMake would happily "build" that into a no-op.
    # Removing both build dirs up front makes "core build ran" and "core
    # build dir is clean-cut from this run's source+config" the same thing.
    echo "-- Removing existing core/deps/{re,baresip}/build for a clean rebuild"
    rm -rf core/deps/re/build core/deps/baresip/build
    git submodule update --init --recursive
    apply_patch core/deps/re core/patches/0001-re-configurable-sip-ws-path.patch
    apply_patch core/deps/re core/patches/0002-re-tls-fingerprint-pin.patch
    apply_patch core/deps/baresip core/patches/0003-baresip-json-stdout-purity.patch
    apply_patch core/deps/re core/patches/0004-re-json-stdout-purity.patch
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
# macOS only. Windows does NOT build here anymore (2026-07-16 windows-
# installer pass) — see step 9b below for why: the NSIS/MSI bundlers are
# config-driven (they package whatever `bundle.resources` in
# tauri.conf.json/tauri.official.conf.json declares at build time), not a
# post-hoc scan of whatever loose files happen to sit in target/release/.
# So for Windows, premium artifacts must be staged into shell/dist-injected/
# (step 5-8 below) BEFORE `tauri build` ever runs — running it here, before
# staging, would just produce a plain unsigned-community exe and waste a
# full compile that step 9b repeats anyway.

if [[ "$TARGET" == "macos" ]]; then
    if [[ "$SKIP_SHELL_BUILD" -eq 1 ]]; then
        echo "-- Skipping shell build (--skip-shell-build)"
    else
        echo "-- Building shell/ (tauri build, release, macOS .app bundle)"
        (cd shell && npm install && npx tauri build --bundles app)
    fi
else
    echo "-- Windows: shell build deferred to step 9b (after premium artifacts are staged)"
fi

# ---------------------------------------------------------------------------
# 5. Locate where premium artifacts land
# ---------------------------------------------------------------------------
# macOS: mirrors centinelo-premium-abi::expected_library_path exactly -
# "directly beside the running executable", i.e. the already-built
# .app/Contents/MacOS/. The .app is a plain directory, so files copied in
# here ride along unchanged into a later `hdiutil`/zip step - no re-build
# needed after this script runs.
#
# Windows: NOT the exe dir (see step 4's comment above) - this is
# shell/dist-injected/, the staging directory tauri.official.conf.json's
# `bundle.resources` template reads from (step 9a filters it per-run, step
# 9b's `tauri build --config tauri.official.generated.json` is what
# actually copies the filtered set into the NSIS/MSI installer), at the
# SAME per-file destinations
# (core-engine/, centinelo_premium.dll(.sig), premium-console-assets/)
# this script has always used for macOS - kept identical on purpose so the
# runtime lookup code (premium.rs, PROTOCOL.md's ctrl_json contract) sees
# the same layout on both platforms once installed.

if [[ "$TARGET" == "macos" ]]; then
    APP_BUNDLE="$(find "$REPO_ROOT/shell/src-tauri/target/release/bundle/macos" -maxdepth 1 -name '*.app' | head -n1)"
    if [[ -z "$APP_BUNDLE" ]]; then
        echo "error: no .app bundle found under shell/src-tauri/target/release/bundle/macos" >&2
        exit 1
    fi
    ARTIFACT_DEST_DIR="$APP_BUNDLE/Contents/MacOS"
    PREMIUM_LIB_NAME="libcentinelo_premium.dylib"
    [[ -d "$ARTIFACT_DEST_DIR" ]] || { echo "error: expected exe dir does not exist: $ARTIFACT_DEST_DIR" >&2; exit 1; }
else
    ARTIFACT_DEST_DIR="$REPO_ROOT/shell/dist-injected"
    PREMIUM_LIB_NAME="centinelo_premium.dll"
    mkdir -p "$ARTIFACT_DEST_DIR"
fi
echo "-- artifact staging dir: $ARTIFACT_DEST_DIR"

# Filename mapping above must stay in sync with
# shell/src-tauri/centinelo-premium-abi/src/paths.rs::expected_library_filename()
# — there is no shared source between this bash script and that Rust file
# today; if that function's naming changes, update this mapping to match.
# See "Known gaps" #7 for a proposal to remove this duplication properly.

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
CORE_ENGINE_DIR="$ARTIFACT_DEST_DIR/core-engine"
rm -rf "$CORE_ENGINE_DIR"
mkdir -p "$CORE_ENGINE_DIR"
cp "$CORE_BIN_PATH" "$CORE_ENGINE_DIR/"
# Every module baresip's own CMake build actually *symlinks* flat into
# build/ (core/BUILD.md step 4b: "this is baresip's own CMake doing that
# symlinking... a post-build step") — NOT plain regular files. 4R/BLOCKING
# fix 2026-07-16: the previous `find ... -type f` does not match symlinks
# at all (a symlink's own type is "l", not "f"), so `ctrl_json.so` — the
# one module the shell actually depends on — was silently never copied.
# The script still printed "copied core engine binary + modules" and
# exited 0; only a real symlinked-module fixture caught this (a flat-file
# fixture passes -type f trivially and hides the bug — see the report's
# 4R-fix appendix for the re-test with a real `ln -s` fixture). `-L` makes
# `find` follow symlinks for the purposes of `-type`, so a symlink whose
# target is a regular file now correctly matches `-type f`.
#
# Windows-only wrinkle (2026-07-16 windows-installer pass, read straight off
# core-build.yml's Windows job comments — confirmed still true after
# merging core-win, windows-media-modules @ 47df112, into this branch:
# adding ausine/aufile/ice/dtls_srtp/wasapi to MODULES didn't change the
# STATIC-vs-shared story): baresip on Windows builds STATIC ("there is no
# ctrl_json.dll - the module is compiled into the static baresip lib via
# the generated src/static.c exports table"), so this `find` step will
# legitimately find zero module files there - that is NOT a bug, unlike
# macOS where finding zero .dylib files would mean something broke.
# (Searching
# $CORE_BUILD_DIR itself, not the MSVC Release/ subdir CORE_BIN_PATH uses -
# baresip's CMake symlinks modules flat into the build root per
# core/BUILD.md step 4b; unverified for Windows specifically since this
# branch never runs there today, module count is zero either way.)
find -L "$CORE_BUILD_DIR" -maxdepth 1 -type f \( -name '*.so' -o -name '*.dylib' -o -name '*.dll' \) -exec cp -L {} "$CORE_ENGINE_DIR/" \;
echo "-- copied core engine binary + modules -> $CORE_ENGINE_DIR"
# Post-copy verification, not just an optimistic exit 0: ctrl_json is the
# one module this whole packaging exercise exists to ship (it's the
# shell<->core protocol bridge, PROTOCOL.md) — if it didn't make it into
# the bundle, the packaged app is silently broken, and that must be a hard
# failure here, not discovered later at first launch.
#
# Windows is exempt from this specific check (2026-07-16 windows-installer
# pass): per the comment above, ctrl_json is statically linked into
# baresip.exe there, so there is no standalone module file to find - only
# the binary itself is verified. This is a WEAKER guarantee than macOS's
# (a successful link isn't the same proof as "the module file shipped"),
# documented here rather than silently narrowed.
if [[ "$TARGET" == "windows" ]]; then
    [[ -f "$CORE_ENGINE_DIR/$CORE_BIN_NAME" ]] || {
        echo "error: $CORE_BIN_NAME not found in $CORE_ENGINE_DIR after copy." >&2
        exit 1
    }
else
    ls "$CORE_ENGINE_DIR"/ctrl_json.* >/dev/null 2>&1 || {
        echo "error: ctrl_json module not found in $CORE_ENGINE_DIR after copy." >&2
        echo "       The shell cannot function without it (core/PROTOCOL.md)." >&2
        echo "       Check that $CORE_BUILD_DIR contains a ctrl_json.{so,dylib,dll}" >&2
        echo "       (baresip symlinks it there, see core/BUILD.md step 4b) from a" >&2
        echo "       real baresip CMake build with -DAPP_MODULES=\"ctrl_json\"." >&2
        exit 1
    }
fi

# ---------------------------------------------------------------------------
# 6b. Copy OpenSSL runtime DLLs beside baresip.exe (Windows only)
# ---------------------------------------------------------------------------
# See "2b. Validate OpenSSL DLL dir" above for why this is required. Flat
# copy into the SAME directory as baresip.exe (CORE_ENGINE_DIR) - Windows's
# default DLL search order checks the launching executable's own directory
# first, before PATH, so this is sufficient without touching PATH at all.
# tauri.official.conf.json's `"../dist-injected/core-engine/*": "core-engine/"`
# resource mapping is already a flat glob over this whole directory, so no
# config change was needed to carry these into the installer once they land
# here - verified in windows-installer.yml's smoke-test job (synthetic DLL
# fixtures, same marker-string + same-directory-as-baresip.exe checks used
# for the other staged files).
if [[ "$TARGET" == "windows" && -n "$OPENSSL_DLL_DIR" ]]; then
    cp "$OPENSSL_DLL_DIR"/*.dll "$CORE_ENGINE_DIR/"
    echo "-- copied OpenSSL runtime DLLs -> $CORE_ENGINE_DIR:"
    ls "$CORE_ENGINE_DIR"/*.dll
fi

# ---------------------------------------------------------------------------
# 7. Copy premium dylib + .sig (Pro builds only)
# ---------------------------------------------------------------------------
# 4R/RESILIENCE fixes 2026-07-16, two issues in the previous version:
#   (a) a Community-edition run (no --premium-dylib) reused the same exe
#       dir a previous Pro run had packaged into left the old signed dylib
#       + .sig sitting there untouched — a "Community" package could ship
#       a real premium module by accident. Fixed: unconditionally remove
#       both destination filenames first, regardless of which mode this
#       run is in, THEN copy back in only if this is actually a Pro run.
#   (b) the two `cp` calls (dylib, then .sig) were not atomic as a pair —
#       a crash/kill between them left a dylib with no matching .sig (or a
#       stale .sig from a previous dylib), and a later `--skip-shell-build`
#       rerun wouldn't necessarily repair it (mtimes wouldn't force a
#       recopy). Fixed: copy both to temp names first, then `mv` (atomic
#       rename on the same filesystem) each into its final name only once
#       both copies have succeeded — so the observable end state is always
#       either "no premium files" or "a complete, matched pair", never a
#       broken partial. The `cleanup_tmp` trap (registered above) removes
#       any leftover temp file if this script dies before the `mv`s run.
rm -f "$ARTIFACT_DEST_DIR/$PREMIUM_LIB_NAME" "$ARTIFACT_DEST_DIR/$PREMIUM_LIB_NAME.sig"
if [[ "$IS_COMMUNITY" -eq 0 ]]; then
    TMP_DYLIB="$ARTIFACT_DEST_DIR/.${PREMIUM_LIB_NAME}.tmp.$$"
    TMP_SIG="$ARTIFACT_DEST_DIR/.${PREMIUM_LIB_NAME}.sig.tmp.$$"
    CLEANUP_TMP_FILES+=("$TMP_DYLIB" "$TMP_SIG")
    cp "$PREMIUM_DYLIB" "$TMP_DYLIB"
    cp "$PREMIUM_SIG" "$TMP_SIG"
    mv "$TMP_DYLIB" "$ARTIFACT_DEST_DIR/$PREMIUM_LIB_NAME"
    mv "$TMP_SIG" "$ARTIFACT_DEST_DIR/$PREMIUM_LIB_NAME.sig"
    echo "-- copied premium dylib+sig (atomic) -> $ARTIFACT_DEST_DIR/$PREMIUM_LIB_NAME(.sig)"
else
    echo "-- no premium dylib to copy (Community edition; any stale dylib+sig from a"
    echo "   previous run in this exe dir were removed above)"
fi

# ---------------------------------------------------------------------------
# 8. Copy premium-console-assets/ (optional even in Pro builds)
# ---------------------------------------------------------------------------
# 4R/RESILIENCE fix 2026-07-16: the stale-directory removal now always runs
# (same "clear first, then repopulate only if given" shape as step 7 above)
# instead of being nested inside the `if PREMIUM_CONSOLE_ASSETS` branch —
# previously a Pro-with-console run followed by a later Community (or
# Pro-without-console) run into the same exe dir left the old console-ui
# assets in place.
CONSOLE_DEST="$ARTIFACT_DEST_DIR/premium-console-assets"
rm -rf "$CONSOLE_DEST"
if [[ -n "$PREMIUM_CONSOLE_ASSETS" ]]; then
    cp -r "$PREMIUM_CONSOLE_ASSETS" "$CONSOLE_DEST"
    echo "-- copied premium console-ui assets -> $CONSOLE_DEST"
else
    echo "-- no premium console assets to copy (console window stays gated even if licensed;"
    echo "   any stale assets from a previous run in this exe dir were removed above)"
fi

# ---------------------------------------------------------------------------
# 9. Report the final layout
# ---------------------------------------------------------------------------

mkdir -p "$OUTPUT_DIR"
LAYOUT_FILE="$OUTPUT_DIR/layout.txt"
{
    echo "package-official.sh — final layout ($TARGET, $([[ "$IS_COMMUNITY" -eq 1 ]] && echo community || echo pro))"
    echo "$([[ "$TARGET" == "macos" ]] && echo "exe dir" || echo "artifact staging dir"): $ARTIFACT_DEST_DIR"
    echo
    find "$ARTIFACT_DEST_DIR" -maxdepth 2 | sort
} > "$LAYOUT_FILE"
echo
echo "== Done. Layout written to $LAYOUT_FILE =="
cat "$LAYOUT_FILE"

# ---------------------------------------------------------------------------
# 9a. Windows only: generate a FILTERED tauri.official.conf.json — dropping
#     any bundle.resources entry whose source is NOT actually staged this
#     run (skipped if --skip-shell-build, since no build happens then)
# ---------------------------------------------------------------------------
# 4R/RELIABILITY fix 2026-07-16 (BLOCKER #1): tauri.official.conf.json (the
# checked-in template) unconditionally declares the premium dylib/.sig and
# premium-console-assets/ resources. tauri build hard-fails with
# ResourcePathNotFound/GlobPathNotFound the moment a declared resource
# doesn't exist on disk — which is exactly what a Community edition (no
# --premium-dylib) or a Pro-without-console build (no
# --premium-console-assets) produces, since steps 7-8 above only populate
# those paths when the corresponding flag was given. Every combination
# package-official.sh --help documents as supported (Community /
# Pro-without-console / Pro-with-console) must actually build. Fixed by
# filtering the template down to only the entries this run's staging
# directory actually has content for, via jq, and building against that
# filtered copy instead of the static template.
generate_windows_resources_config() {
    local template="$REPO_ROOT/shell/src-tauri/tauri.official.conf.json"
    local generated="$REPO_ROOT/shell/src-tauri/tauri.official.generated.json"
    command -v jq >/dev/null 2>&1 || {
        echo "error: jq is required to generate the Windows installer resources config" >&2
        echo "       (--target windows) and was not found on PATH." >&2
        exit 1
    }

    local include_premium=0 include_console=0
    [[ "$IS_COMMUNITY" -eq 0 ]] && include_premium=1
    [[ -n "$PREMIUM_CONSOLE_ASSETS" ]] && include_console=1

    local filtered
    filtered="$(jq --argjson premium "$include_premium" --argjson console "$include_console" '
        .bundle.resources |= with_entries(
            select(
                (.key | startswith("../dist-injected/core-engine/"))
                or (($premium == 1) and (.key | contains("centinelo_premium.dll")))
                or (($console == 1) and (.key == "../dist-injected/premium-console-assets/*"))
            )
        )
    ' "$template")"

    # premium-console-assets/ SUBFOLDER entries (components/, store/,
    # bridge/, and any future one) are synthesized here from the REAL
    # subfolders present under --premium-console-assets, instead of a fixed
    # list hardcoded in the template (4R/READABILITY fix 2026-07-16 — see
    # tauri.official.conf.json for why a plain "premium-console-assets/*"
    # entry alone is not enough: Tauri's glob resources are non-recursive,
    # so each subfolder genuinely needs its own "**/*" entry, and a new
    # subfolder added to the private premium-console-ui source would
    # otherwise silently never make it into the installer until someone
    # remembered to also hand-edit this JSON file).
    if [[ "$include_console" -eq 1 ]]; then
        local subdir name subdirs_output
        # `find` piped straight into `while read < <(...)` runs the loop in
        # the current shell (good, "local" above stays visible) but the
        # process substitution means `find`'s exit code never reaches
        # `set -e` - an unreadable subfolder (permissions, unexpected
        # symlink loop) would make find fail and the loop would just quietly
        # see fewer subdirs instead of aborting the build (4R/RESILIENCE
        # finding 2026-07-16). Capture the output via a plain command
        # substitution instead: `set -e` DOES fire on a failed
        # `var="$(cmd)"` assignment, then iterate over the captured text.
        subdirs_output="$(find "$PREMIUM_CONSOLE_ASSETS" -mindepth 1 -maxdepth 1 -type d)"
        while IFS= read -r subdir; do
            [[ -z "$subdir" ]] && continue
            name="$(basename "$subdir")"
            filtered="$(printf '%s' "$filtered" | jq --arg name "$name" \
                '.bundle.resources["../dist-injected/premium-console-assets/" + $name + "/**/*"] = ("premium-console-assets/" + $name + "/")')"
        done <<< "$subdirs_output"
    fi

    printf '%s\n' "$filtered" > "$generated"
    echo "-- generated Windows installer resources config (community=$([[ $include_premium -eq 1 ]] && echo no || echo yes), console=$([[ $include_console -eq 1 ]] && echo yes || echo no)) -> $generated"
    jq '.bundle.resources' "$generated"
}

# ---------------------------------------------------------------------------
# 9b. Windows only: build the actual NSIS/MSI installer around the staged
#     artifacts (skip with --skip-shell-build to just leave the staging
#     directory populated for manual inspection)
# ---------------------------------------------------------------------------
# This is the step that makes step 5's "staging dir, not exe dir" comment
# true: `tauri build --config tauri.official.generated.json` (generated by
# 9a immediately above, see BLOCKER #1 fix there) reads the FILTERED
# `bundle.resources` map, which points at shell/dist-injected/ - the exact
# directory steps 6-8 just populated - and copies each entry into the
# NSIS/MSI installer at build time. This only runs NOW, after staging,
# because both bundlers are config/manifest-driven: they package what
# `bundle.resources` declares, not whatever loose files happen to be
# sitting in target/release/ afterward (that's the mac-only trick step 4/5
# rely on - a .app is just a directory, an NSIS/MSI installer is not).
#
# tauri.conf.json's base config intentionally does NOT declare these
# resources (only tauri.official.conf.json / tauri.official.generated.json,
# merged in here via --config) so a plain community `cargo tauri
# build`/`tauri dev`, or shell-build.yml's existing Windows CI job, is
# completely unaffected by this file's existence - it never passes
# --config, so it never looks at shell/dist-injected/ at all, and doesn't
# care whether that directory exists or is empty.
if [[ "$TARGET" == "windows" ]]; then
    if [[ "$SKIP_SHELL_BUILD" -eq 1 ]]; then
        echo
        echo "-- Skipping installer build (--skip-shell-build); staged artifacts are"
        echo "   sitting in $ARTIFACT_DEST_DIR but no .msi/.exe was produced this run."
    else
        echo
        generate_windows_resources_config
        echo "-- Building the Windows installer (tauri build --config tauri.official.generated.json, nsis+msi)"
        (cd shell && npm install && npx tauri build --config src-tauri/tauri.official.generated.json --bundles nsis,msi)
        NSIS_OUT_DIR="$REPO_ROOT/shell/src-tauri/target/release/bundle/nsis"
        MSI_OUT_DIR="$REPO_ROOT/shell/src-tauri/target/release/bundle/msi"
        # 4R/RESILIENCE fix 2026-07-16: `tauri build` exiting 0 is not proof
        # either bundler actually produced its installer file — assert both
        # explicitly, same shape as every other post-copy check in this
        # script, rather than letting a silently-empty bundle/ dir surface
        # three steps later as a confusing "no artifact to upload" in CI.
        shopt -s nullglob
        _nsis_check=("$NSIS_OUT_DIR"/*.exe)
        _msi_check=("$MSI_OUT_DIR"/*.msi)
        shopt -u nullglob
        [[ ${#_nsis_check[@]} -gt 0 ]] || { echo "error: tauri build reported success but no .exe found in $NSIS_OUT_DIR" >&2; exit 1; }
        [[ ${#_msi_check[@]} -gt 0 ]] || { echo "error: tauri build reported success but no .msi found in $MSI_OUT_DIR" >&2; exit 1; }
        echo "-- installer(s) at: shell/src-tauri/target/release/bundle/{nsis,msi}/"
    fi
fi

# ---------------------------------------------------------------------------
# Known gaps (honest, not hidden — see also inline comments above):
#   1. [Partially closed 2026-07-16, release-ci pass] --target windows still
#      does not build core itself (needs the re-install-prefix dance
#      core-build.yml's Windows job already runs — porting it here is a
#      follow-up, not done this pass; use --skip-core-build with a
#      CI-produced Windows build in the meantime). The OTHER half of this
#      gap - baresip.exe's STATIC build still needing the Chocolatey-
#      installed OpenSSL DLLs beside it at *runtime* (no ctrl_json.dll, no
#      statically-linked OpenSSL - see the step 6 comment above) - IS now
#      closed: `--openssl-dll-dir DIR` (see "2b. Validate OpenSSL DLL dir"
#      and "6b. Copy OpenSSL runtime DLLs" above) copies every *.dll from
#      the given dir flat into core-engine/ beside baresip.exe, required by
#      default for --target windows. windows-installer.yml's
#      official-pro-build job passes "$OPENSSL_ROOT_DIR\bin" (the same dir
#      core-build.yml's Windows job resolves OpenSSL to); the
#      resources-mechanism-smoke-test job proves the DLLs ride into the
#      actual NSIS/MSI installer beside baresip.exe using synthetic
#      fixtures. This was blocker #1 for Edgar's Windows beta
#      (docs/HANDOFF.md). 4R/RELIABILITY correction 2026-07-16: an earlier
#      draft of this paragraph said this was "verified end-to-end via
#      windows-installer.yml" — that overclaimed. What is actually verified
#      end-to-end in CI, this pass, is the STAGING+PACKAGING MECHANISM
#      (--openssl-dll-dir copying flat into core-engine/, and
#      bundle.resources carrying that into the real NSIS/MSI installer at
#      the right path) using synthetic fixture DLLs — not real Chocolatey
#      OpenSSL DLLs, not a real core-win build, and not the
#      official-pro-build job (still gated off, see gap #4). The real path
#      (`--openssl-dll-dir "$OPENSSL_ROOT_DIR/bin"` against a genuine
#      Chocolatey install, feeding a genuine core-win-built baresip.exe)
#      has NOT had a first real run yet — that only happens once
#      official-pro-build is actually dispatched with
#      i_understand_public_artifact_risk=true, which needs PREMIUM_REPO_PAT
#      to exist first (gap #4) — and until then this closes the packaging
#      MECHANISM half of blocker #1, not the blocker itself. Final proof on
#      a real end-user machine additionally needs qa-e2e with a physical
#      Windows box.
#   2. [Partially closed 2026-07-16, windows-installer pass] Windows now HAS
#      an installer path: tauri.official.conf.json's `bundle.resources`
#      template, filtered per-run into tauri.official.generated.json (step
#      9a) and merged via `tauri build --config` (step 9b), turns the
#      artifacts this script stages in shell/dist-injected/ into a real
#      NSIS/MSI installer — verified end-to-end with synthetic fixtures
#      against resources placement + build succeeding across all three
#      supported combinations (Community / Pro-without-console /
#      Pro-with-console), see
#      .claude/reports/release-ci-2026-07-16-windows-installer.md for the
#      run URL. Still open: macOS DMG signing/notarization, and Windows OV
#      cert code-signing (`bundle.windows.certificateThumbprint` is
#      deliberately unset) — both explicitly deferred until public launch
#      per CLAUDE.md, SmartScreen/Gatekeeper will warn once for the beta.
#   3. shell/src-tauri/src/sidecar.rs's default_core_binary_path() doesn't
#      look in the installed-layout `core-engine/` subdir this script
#      creates yet — an installed build still needs a manual Settings >
#      Advanced path override (or CENTINELO_CORE_BIN) until shell-tauri
#      wires that lookup, OR this moves to Tauri's own `externalBin`
#      sidecar mechanism (tauri.conf.json has no `bundle.externalBin`
#      entry today) — either is a shell-tauri decision, not made here.
#   4. Not wired into a fully automated GitHub Actions workflow yet.
#      .github/workflows/windows-installer.yml (added this pass) proves the
#      NSIS/MSI + `bundle.resources` mechanism works with synthetic
#      fixtures, but its "official Pro build" job (checkout the private
#      premium repo via a PAT secret, build core-win, run this script, then
#      the real `tauri build --config`) is deliberately gated off
#      (workflow_dispatch input, defaults to false) and UNTESTED — it needs
#      core-win merged, a `PREMIUM_REPO_PAT` repo secret Felix has not
#      created yet, and a real signed dylib to exist somewhere the
#      workflow can fetch it from (proposed convention, not yet agreed:
#      Felix commits the offline-signed dylib+.sig to a fixed path inside
#      the private premium repo after each local sign). See the report for
#      the full design and exactly what's blocking it.
#   5. [Fixed 2026-07-16, 4R review] Premium dylib/.sig and console-assets
#      are now cleared unconditionally every run before being
#      (re-)populated (steps 7-8 above), so re-running against a dirty exe
#      dir — e.g. Community right after Pro — no longer leaves stale
#      premium artifacts behind. core-engine/ (step 6) is likewise now
#      `rm -rf`'d before each repopulation.
#   6. [Count corrected 2026-07-16, windows-installer pass] The four core/
#      patches (0001-0004) are listed independently in FOUR places: this
#      script's apply_patch() calls, core-build.yml's macOS job,
#      core-build.yml's Windows job, and — added this pass —
#      windows-installer.yml's official-pro-build job ("Apply local
#      re/baresip patches" step, four inline `git apply` calls) — no shared
#      source, so a new patch (or a reordering) has to be hand-added in all
#      four places or silently drifts. Proposed fix (not implemented here —
#      touches CI, out of this pass's scope, and would need core-build.yml's
#      Windows job — GATING per CLAUDE.md — re-verified green before
#      merging any change to it): a `core/patches/series.txt` listing
#      `<submodule-dir> <patch-file>` pairs in apply order, read by a tiny
#      shared shell function all four consumers call instead of hardcoding
#      the list each.
#   7. PREMIUM_LIB_NAME (step 5) duplicates
#      centinelo-premium-abi::expected_library_filename()'s per-OS naming
#      by hand in bash, with only a comment (not a build-time check) tying
#      them together — if that Rust function's naming ever changes, this
#      script silently keeps copying to the OLD filename and the loader
#      won't find it. Proposed fix (not implemented here — would need a
#      small companion Rust binary or build-time codegen exposing the name,
#      which is a centinelo-premium-abi/shell-tauri decision, not a
#      release-ci one): a tiny `--print-library-filename` mode on that
#      crate (or a build script step) this script could shell out to
#      instead of hardcoding the match.
# ---------------------------------------------------------------------------

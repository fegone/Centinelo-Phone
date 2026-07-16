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
# .claude/reports/release-ci-2026-07-16-f5-prep.md (main report + 4R-fix
# appendix) for the exact commands and what they produced, including a
# dedicated symlink-fixture re-test (see item 1 below). Not yet run against
# a *real* signed premium dylib (that requires Felix's offline signing key,
# which never touches this repo or CI) or on Windows (no Windows machine
# available this pass — the Windows branch below is written to the same
# contract core-build.yml's Windows job already proves builds/links, but is
# unexercised by this script specifically).
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
PREMIUM_DYLIB=""
PREMIUM_SIG=""
PREMIUM_CONSOLE_ASSETS=""
OUTPUT_DIR=""

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
  --skip-core-build               Reuse an existing core/deps/baresip/build
                                   instead of rebuilding it (faster local
                                   iteration; CI should NOT pass this).
  --skip-shell-build              Reuse an existing shell/src-tauri/target
                                   build instead of rebuilding it (ditto).
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
        -h|--help)
            usage; exit 0 ;;
        *)
            echo "error: unknown argument: $1" >&2
            usage
            exit 1 ;;
    esac
done

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
CORE_ENGINE_DIR="$EXE_DIR/core-engine"
rm -rf "$CORE_ENGINE_DIR"
mkdir -p "$CORE_ENGINE_DIR"
cp "$CORE_BUILD_DIR/$CORE_BIN_NAME" "$CORE_ENGINE_DIR/"
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
find -L "$CORE_BUILD_DIR" -maxdepth 1 -type f \( -name '*.so' -o -name '*.dylib' -o -name '*.dll' \) -exec cp -L {} "$CORE_ENGINE_DIR/" \;
echo "-- copied core engine binary + modules -> $CORE_ENGINE_DIR"
# Post-copy verification, not just an optimistic exit 0: ctrl_json is the
# one module this whole packaging exercise exists to ship (it's the
# shell<->core protocol bridge, PROTOCOL.md) — if it didn't make it into
# the bundle, the packaged app is silently broken, and that must be a hard
# failure here, not discovered later at first launch.
ls "$CORE_ENGINE_DIR"/ctrl_json.* >/dev/null 2>&1 || {
    echo "error: ctrl_json module not found in $CORE_ENGINE_DIR after copy." >&2
    echo "       The shell cannot function without it (core/PROTOCOL.md)." >&2
    echo "       Check that $CORE_BUILD_DIR contains a ctrl_json.{so,dylib,dll}" >&2
    echo "       (baresip symlinks it there, see core/BUILD.md step 4b) from a" >&2
    echo "       real baresip CMake build with -DAPP_MODULES=\"ctrl_json\"." >&2
    exit 1
}

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
rm -f "$EXE_DIR/$PREMIUM_LIB_NAME" "$EXE_DIR/$PREMIUM_LIB_NAME.sig"
if [[ "$IS_COMMUNITY" -eq 0 ]]; then
    TMP_DYLIB="$EXE_DIR/.${PREMIUM_LIB_NAME}.tmp.$$"
    TMP_SIG="$EXE_DIR/.${PREMIUM_LIB_NAME}.sig.tmp.$$"
    CLEANUP_TMP_FILES+=("$TMP_DYLIB" "$TMP_SIG")
    cp "$PREMIUM_DYLIB" "$TMP_DYLIB"
    cp "$PREMIUM_SIG" "$TMP_SIG"
    mv "$TMP_DYLIB" "$EXE_DIR/$PREMIUM_LIB_NAME"
    mv "$TMP_SIG" "$EXE_DIR/$PREMIUM_LIB_NAME.sig"
    echo "-- copied premium dylib+sig (atomic) -> $EXE_DIR/$PREMIUM_LIB_NAME(.sig)"
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
CONSOLE_DEST="$EXE_DIR/premium-console-assets"
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
#   5. [Fixed 2026-07-16, 4R review] Premium dylib/.sig and console-assets
#      are now cleared unconditionally every run before being
#      (re-)populated (steps 7-8 above), so re-running against a dirty exe
#      dir — e.g. Community right after Pro — no longer leaves stale
#      premium artifacts behind. core-engine/ (step 6) is likewise now
#      `rm -rf`'d before each repopulation.
#   6. The four core/ patches (0001-0004) are listed independently in THREE
#      places: this script's apply_patch() calls, core-build.yml's macOS
#      job, and core-build.yml's Windows job — no shared source, so a new
#      patch (or a reordering) has to be hand-added in three places or
#      silently drifts. Proposed fix (not implemented here — touches CI,
#      out of this pass's scope): a `core/patches/series.txt` listing
#      `<submodule-dir> <patch-file>` pairs in apply order, read by a tiny
#      shared shell function all three consumers call instead of hardcoding
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

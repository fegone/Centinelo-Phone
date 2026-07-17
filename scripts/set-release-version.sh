#!/usr/bin/env bash
# scripts/set-release-version.sh — stamps a release version into every file
# a `tauri build` invocation reads a version from, so the built binary's
# app.getVersion() (compared against latest.json's own "version" field by
# the auto-updater — see shell/README.md "Contract for release-ci") and the
# bundler's own asset-naming ([AppName]_[version]_arch...) both reflect the
# real release tag, not whatever version string happens to be checked into
# git at the time.
#
# Updates three files, in place, no other side effects:
#   - shell/src-tauri/tauri.conf.json  (.version)         — read by `tauri
#     build` for both the embedded app version AND asset filenames.
#   - shell/src-tauri/Cargo.toml       ([package].version) — kept in sync so
#     `cargo build`/`cargo tauri dev` never disagrees with tauri.conf.json
#     about the app's own version.
#   - shell/package.json               (.version)          — cosmetic (npm
#     metadata only, Tauri itself does not read this), kept in lockstep so
#     nothing that assumes package.json/tauri.conf.json agree (e.g. a stray
#     local `npm version` run) silently drifts.
#
# Deliberately does NOT touch shell/src-tauri/Cargo.toml's
# `[workspace.package] version = "0.1.0"` (centinelo-premium-abi's inherited
# fallback — see that section's own comment in Cargo.toml): only the FIRST
# `version = "..."` line in the file, which is `[package]`'s (this crate's
# own, always listed first in the file), is replaced.
#
# Uses python3's `re` module for a precise first-match-only substitution
# instead of sed, because macOS ships BSD sed (no `0,/re/` first-match
# addressing) while GitHub's windows-latest bash (Git for Windows) ships GNU
# sed — the same BSD/GNU split package-official.sh's own file_mtime_epoch()
# already had to work around (see that script's comment on the subject).
# python3 is preinstalled on every GitHub-hosted runner image this repo's CI
# uses (ubuntu-latest, macos-latest, windows-latest), so there is no
# portability gap here.
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <version>   (e.g. 2.1.0 — no leading 'v')" >&2
    exit 1
fi
VERSION="$1"

# Same SemVer shape release.yml's own preflight job already validates before
# this script is ever called from CI — re-checked here too so this script is
# safe to run standalone (local iteration), not just as a trusted CI-only
# tool.
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
    echo "error: '$VERSION' is not a valid SemVer version (expected X.Y.Z, optionally -prerelease/+build, no leading 'v')" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TAURI_CONF="$REPO_ROOT/shell/src-tauri/tauri.conf.json"
CARGO_TOML="$REPO_ROOT/shell/src-tauri/Cargo.toml"
PACKAGE_JSON="$REPO_ROOT/shell/package.json"

for f in "$TAURI_CONF" "$CARGO_TOML" "$PACKAGE_JSON"; do
    [[ -f "$f" ]] || { echo "error: expected file not found: $f" >&2; exit 1; }
done

command -v jq >/dev/null 2>&1 || { echo "error: jq is required (preinstalled on GitHub-hosted runners)" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "error: python3 is required (preinstalled on GitHub-hosted runners)" >&2; exit 1; }

tmp_json="$(mktemp)"
jq --arg v "$VERSION" '.version = $v' "$TAURI_CONF" > "$tmp_json" && mv "$tmp_json" "$TAURI_CONF"
echo "-- shell/src-tauri/tauri.conf.json .version -> $VERSION"

tmp_json="$(mktemp)"
jq --arg v "$VERSION" '.version = $v' "$PACKAGE_JSON" > "$tmp_json" && mv "$tmp_json" "$PACKAGE_JSON"
echo "-- shell/package.json .version -> $VERSION"

python3 - "$CARGO_TOML" "$VERSION" <<'PYEOF'
import re
import sys

path, version = sys.argv[1], sys.argv[2]
with open(path, "r", encoding="utf-8") as fh:
    text = fh.read()

pattern = re.compile(r'^version = "[^"]*"', re.MULTILINE)
new_text, count = pattern.subn(f'version = "{version}"', text, count=1)
if count != 1:
    sys.exit(
        f"error: expected exactly one 'version = \"...\"' line to replace "
        f"in {path} (found {count}) - has [package]'s version field moved "
        f"or been renamed?"
    )

with open(path, "w", encoding="utf-8") as fh:
    fh.write(new_text)
PYEOF
echo "-- shell/src-tauri/Cargo.toml [package].version -> $VERSION"

echo "OK: version stamped as $VERSION in tauri.conf.json, Cargo.toml, package.json"

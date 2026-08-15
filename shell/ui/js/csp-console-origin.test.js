// Ties tauri.conf.json's CSP to the ORIGINS console-panel.js can actually
// request, so the two can never drift apart again the way they did before
// this file existed: console-panel.js used to hardcode the macOS/Linux
// scheme-origin shape only, and tauri.conf.json's CSP listed only that
// same shape - internally "consistent" while being wrong for Windows
// (WebView2 needs `http://<scheme>.localhost/...`, never
// `<scheme>://localhost/...` - see resolveConsoleAssetOrigin's doc in
// console-panel.js and console.rs::asset_protocol_handler's doc for the
// evidence). Fixing only one side (the JS origin OR the CSP entry) trades
// one failure mode for another - a origin the JS never requests is dead
// weight, but a CSP that doesn't list an origin the JS DOES request is a
// silent script/style block. This file is the regression net for BOTH
// directions of that coupling.
//
// Run: `npm test` (from shell/) or `node --test ui/js/*.test.js`.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { ASSET_SCHEME, resolveConsoleAssetOrigin } from "./console-panel.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const TAURI_CONF_PATH = path.join(__dirname, "..", "..", "src-tauri", "tauri.conf.json");

/// The exact two origin shapes console-panel.js can ever request an asset
/// from - derived from resolveConsoleAssetOrigin itself (not
/// re-hardcoded), so a change to how that function builds its result
/// flows straight into what this test expects the CSP to allow.
const MACOS_LINUX_ORIGIN = resolveConsoleAssetOrigin((path_, protocol) => `${protocol}://localhost/${path_}`);
const WINDOWS_ANDROID_ORIGIN = resolveConsoleAssetOrigin((path_, protocol) => `http://${protocol}.localhost/${path_}`);

function readCsp() {
  const conf = JSON.parse(readFileSync(TAURI_CONF_PATH, "utf8"));
  const csp = conf.app && conf.app.security && conf.app.security.csp;
  assert.ok(typeof csp === "string" && csp.length > 0, "tauri.conf.json must have a non-empty app.security.csp");
  return csp;
}

/// A directive's source list as its raw tokens (e.g. "script-src" ->
/// ["'self'", "premium-console:", "http://premium-console.localhost"]) -
/// good enough for membership checks without a full CSP parser.
function directiveSources(csp, directiveName) {
  const match = csp.split(";").map((d) => d.trim()).find((d) => d.startsWith(directiveName + " "));
  assert.ok(match, `CSP has no ${directiveName} directive: ${csp}`);
  return match.slice(directiveName.length).trim().split(/\s+/);
}

for (const directive of ["script-src", "style-src"]) {
  test(`CSP ${directive}: allows the macOS/Linux console-asset origin (${ASSET_SCHEME}:)`, () => {
    const sources = directiveSources(readCsp(), directive);
    // The scheme-only form (e.g. "premium-console:") is what CSP source
    // syntax uses for a whole-scheme allowance - it covers every
    // premium-console://localhost/... request, matching MACOS_LINUX_ORIGIN.
    assert.ok(
      sources.includes(`${ASSET_SCHEME}:`),
      `${directive} must list "${ASSET_SCHEME}:" to allow requests to ${MACOS_LINUX_ORIGIN} - got: ${sources.join(" ")}`
    );
  });

  test(`CSP ${directive}: allows the Windows/Android console-asset origin (http://${ASSET_SCHEME}.localhost)`, () => {
    // THE coupling this file exists to pin: reverting tauri.conf.json's
    // CSP to list only the "premium-console:" form (the pre-fix state)
    // makes THIS assertion fail, even though console-panel.js's origin
    // resolver is correct - exactly the "fixed one side, not both" bug
    // the shell-tauri task that added this file called out.
    const sources = directiveSources(readCsp(), directive);
    const windowsOrigin = `http://${ASSET_SCHEME}.localhost`;
    assert.ok(
      sources.includes(windowsOrigin),
      `${directive} must list "${windowsOrigin}" to allow requests to ${WINDOWS_ANDROID_ORIGIN} - got: ${sources.join(" ")}`
    );
  });
}

test("the two origins under test really are the two distinct shapes resolveConsoleAssetOrigin can produce", () => {
  // Sanity check on this file's own fakes, so a future edit that
  // accidentally makes both fakes agree can't turn every test above into
  // a tautology.
  assert.notEqual(MACOS_LINUX_ORIGIN, WINDOWS_ANDROID_ORIGIN);
  assert.equal(MACOS_LINUX_ORIGIN, `${ASSET_SCHEME}://localhost/`);
  assert.equal(WINDOWS_ANDROID_ORIGIN, `http://${ASSET_SCHEME}.localhost/`);
});

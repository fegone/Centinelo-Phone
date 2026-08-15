// Tests for console-panel.js's pure helpers (inline BLF console panel,
// 2026-08-14). Node's built-in test runner (`node:test`/`node:assert`) -
// no new devDependency, matching this project's "no bundler, no frontend
// framework" philosophy. Run: `npm test` (from shell/) or
// `node --test ui/js/*.test.js`.
//
// Only the DOM-free half of the module is covered here (CONSOLE_SCRIPTS
// order, assetUrl, favoritesToRoster, the dispatch table) - the stateful
// half (openConsolePanel/closeConsolePanel/ensureAssetsLoaded) builds real
// <link>/<script> nodes and touches document/window, which this project
// has no jsdom-style dependency for; that half is verified by the same
// Rust-side INDEX_HTML glue contract it mirrors plus visual QA (see
// docs/console-inline-panel-decision.md).
//
// The asset-list assertions double as a contract pin against console.rs's
// INDEX_HTML: if either side drifts (script renamed, order changed), this
// file fails before a licensed operator ever sees a half-loaded panel.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  CONSOLE_ASSET_ORIGIN,
  CONSOLE_STYLES,
  CONSOLE_SCRIPTS,
  assetUrl,
  favoritesToRoster,
  CONSOLE_DISPATCH_TABLE,
  makeConsoleDispatcher,
} from "./console-panel.js";

test("CONSOLE_ASSET_ORIGIN pins the premium-console:// scheme host", () => {
  // Must stay in lockstep with console.rs::ASSET_SCHEME + the host its
  // handler serves (and therefore with tauri.conf.json's CSP entry).
  assert.equal(CONSOLE_ASSET_ORIGIN, "premium-console://localhost/");
});

test("CONSOLE_STYLES: tokens.css before console.css (custom-property order)", () => {
  assert.deepEqual([...CONSOLE_STYLES], ["tokens.css", "console.css"]);
});

test("CONSOLE_SCRIPTS: dependency order, dom-utils first and console-app last", () => {
  // Verbatim order of console.rs INDEX_HTML's <script> tags, which is
  // itself the package's own dev/mock.html order. Only the anchors are
  // asserted (first/last/store-before-bridge) plus the full count, so a
  // mid-list reorder the package itself makes still needs a human look,
  // but a rename/removal fails loudly here.
  assert.equal(CONSOLE_SCRIPTS[0], "components/dom-utils.js");
  assert.equal(CONSOLE_SCRIPTS[1], "components/icons.js");
  assert.equal(CONSOLE_SCRIPTS.indexOf("store/ConsoleStore.js") < CONSOLE_SCRIPTS.indexOf("bridge/EngineBridge.js"), true);
  assert.equal(CONSOLE_SCRIPTS[CONSOLE_SCRIPTS.length - 1], "console-app.js");
  assert.equal(CONSOLE_SCRIPTS.length, 11);
});

test("assetUrl joins origin + path without double slashes", () => {
  assert.equal(assetUrl("console-app.js"), "premium-console://localhost/console-app.js");
  assert.equal(assetUrl("components/icons.js"), "premium-console://localhost/components/icons.js");
});

test("favoritesToRoster: keeps ext-bearing favorites, trims, falls back to Ext label", () => {
  const roster = favoritesToRoster([
    { ext: "101", label: "Front Desk" },
    { ext: " 202 ", label: "   " }, // blank label -> Ext fallback, trimmed ext
    { ext: "", label: "No extension" }, // dropped: nothing to subscribe/call
    { ext: "   ", label: "Whitespace ext" }, // dropped, same reason
    { label: "No ext key at all" }, // dropped (undefined ext)
  ]);
  assert.deepEqual(roster, [
    { ext: "101", name: "Front Desk", group: "Favorites" },
    { ext: "202", name: "Ext 202", group: "Favorites" },
  ]);
});

test("favoritesToRoster: null/undefined list degrades to empty roster", () => {
  assert.deepEqual(favoritesToRoster(undefined), []);
  assert.deepEqual(favoritesToRoster(null), []);
});

test("dispatch table: one existing commands.rs verb per console verb (Option B, no passthrough)", () => {
  // Same verb set as console.rs INDEX_HTML's DISPATCH - a console verb
  // without a mapping here would dead-end with no dispatch at runtime.
  assert.deepEqual(Object.keys(CONSOLE_DISPATCH_TABLE).sort(), [
    "abort_transfer",
    "answer",
    "attended_transfer",
    "blf_subscribe",
    "blf_unsubscribe",
    "blind_transfer",
    "complete_transfer",
    "dial",
    "hangup",
    "hold",
    "mute",
    "register",
    "resume",
  ]);
  for (const [cmd, [tauriCmd]] of Object.entries(CONSOLE_DISPATCH_TABLE)) {
    assert.match(tauriCmd, /^sidecar_(dial|answer|hangup|restart|hold|resume|mute|blind_transfer|attended_transfer|complete_transfer|abort_transfer|blf_subscribe|blf_unsubscribe)$/, `verb ${cmd}`);
  }
});

test("makeConsoleDispatcher: invokes the mapped command with mapped args", async () => {
  const calls = [];
  const dispatch = makeConsoleDispatcher((cmd, args) => {
    calls.push([cmd, args]);
    return Promise.resolve("ok");
  });
  await dispatch({ cmd: "dial", uri: "sip:101@host" });
  await dispatch({ cmd: "hangup" }); // no call_id -> null ("current call")
  await dispatch({ cmd: "mute", on: 1 }); // truthy -> boolean
  await dispatch({ cmd: "blf_subscribe", ext: 101 }); // number -> String
  assert.deepEqual(calls, [
    ["sidecar_dial", { uri: "sip:101@host" }],
    ["sidecar_hangup", { call_id: null }],
    ["sidecar_mute", { on: true, call_id: null }],
    ["sidecar_blf_subscribe", { ext: "101" }],
  ]);
});

test("makeConsoleDispatcher: unknown verbs reject instead of silently no-op'ing", async () => {
  const dispatch = makeConsoleDispatcher(() => Promise.resolve("should not run"));
  await assert.rejects(dispatch({ cmd: "teleport" }), /no dispatch for cmd 'teleport'/);
  await assert.rejects(dispatch(null), /no dispatch for cmd 'null'/);
});

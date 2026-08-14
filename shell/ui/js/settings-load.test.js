// Tests for settings-load.js's pure logic behind openSettings' partial-
// load fix: one failed IPC call among the 8 openSettings loads must
// degrade only its own group, never blank the whole screen (the old
// Promise.all behavior this replaces). DOM rendering (markFieldLoadFailed,
// renderSettingsLoadErrors) stays in app.js and isn't covered here - only
// the settled-results bookkeeping is.

import { test } from "node:test";
import assert from "node:assert/strict";
import { SETTINGS_FIELD_GROUPS, describeError, summarizeSettledResults } from "./settings-load.js";

function fulfilled(value) {
  return { status: "fulfilled", value };
}
function rejected(reason) {
  return { status: "rejected", reason };
}

// ---------------------------------------------------------------------
// describeError
// ---------------------------------------------------------------------

test("describeError: uses .message when the rejection is an Error", () => {
  assert.equal(describeError(new Error("boom")), "boom");
});

test("describeError: stringifies a plain string rejection", () => {
  assert.equal(describeError("network unreachable"), "network unreachable");
});

test("describeError: never throws on undefined/null", () => {
  assert.doesNotThrow(() => describeError(undefined));
  assert.doesNotThrow(() => describeError(null));
});

// ---------------------------------------------------------------------
// summarizeSettledResults
// ---------------------------------------------------------------------

test("summarizeSettledResults: all 8 groups present when every call succeeds", () => {
  const results = SETTINGS_FIELD_GROUPS.map(() => fulfilled({ ok: true }));
  const { values, failed } = summarizeSettledResults(results);
  assert.equal(failed.length, 0);
  for (const group of SETTINGS_FIELD_GROUPS) {
    assert.deepEqual(values[group.key], { ok: true });
  }
});

test("summarizeSettledResults: a single rejected command degrades only its own group", () => {
  // This is the core regression this fix exists for: with the old
  // Promise.all, ONE rejection here would have thrown before any of the
  // other 7 values were ever read. allSettled + this function must still
  // surface every successful value.
  const results = SETTINGS_FIELD_GROUPS.map((group, i) =>
    group.cmd === "get_bridge_settings" ? rejected(new Error("bridge unreachable")) : fulfilled({ index: i })
  );
  const { values, failed } = summarizeSettledResults(results);

  assert.equal(failed.length, 1);
  assert.equal(failed[0].cmd, "get_bridge_settings");
  assert.equal(failed[0].key, "bridge");
  assert.equal(failed[0].error, "bridge unreachable");
  assert.equal(failed[0].labelKey, "settings.bridgeHeading");

  assert.equal(values.bridge, undefined);
  // every other group still has its real value, not swallowed by the
  // one rejection.
  for (const group of SETTINGS_FIELD_GROUPS) {
    if (group.key === "bridge") continue;
    assert.notEqual(values[group.key], undefined);
  }
});

test("summarizeSettledResults: multiple independent failures are all reported", () => {
  const results = SETTINGS_FIELD_GROUPS.map((group) =>
    group.cmd === "get_license_settings" || group.cmd === "get_theme"
      ? rejected(new Error(`${group.cmd} failed`))
      : fulfilled({})
  );
  const { failed } = summarizeSettledResults(results);
  const failedCmds = failed.map((f) => f.cmd).sort();
  assert.deepEqual(failedCmds, ["get_license_settings", "get_theme"]);
});

test("summarizeSettledResults: a fulfilled falsy/empty value is kept, not treated as failed", () => {
  // get_core_binary_path resolving to "" (no override configured) is a
  // real, meaningful value - must not be confused with the "undefined ==
  // failed" sentinel this module uses for actual rejections.
  const results = SETTINGS_FIELD_GROUPS.map((group) => (group.cmd === "get_core_binary_path" ? fulfilled("") : fulfilled({})));
  const { values, failed } = summarizeSettledResults(results);
  assert.equal(failed.length, 0);
  assert.equal(values.corePath, "");
});

test("summarizeSettledResults: failed entries never carry the raw rejection reason, only a string", () => {
  const err = new Error("raw error object");
  err.someSensitiveField = "must not leak through untouched";
  const results = SETTINGS_FIELD_GROUPS.map((group) => (group.cmd === "get_account_settings" ? rejected(err) : fulfilled({})));
  const { failed } = summarizeSettledResults(results);
  assert.equal(typeof failed[0].error, "string");
  assert.equal(failed[0].error, "raw error object");
});

test("summarizeSettledResults: every SETTINGS_FIELD_GROUPS entry has a unique key and cmd", () => {
  const keys = SETTINGS_FIELD_GROUPS.map((g) => g.key);
  const cmds = SETTINGS_FIELD_GROUPS.map((g) => g.cmd);
  assert.equal(new Set(keys).size, keys.length);
  assert.equal(new Set(cmds).size, cmds.length);
});

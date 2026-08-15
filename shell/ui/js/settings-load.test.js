// Tests for settings-load.js's pure logic behind openSettings' partial-
// load fix: one failed IPC call among the 8 openSettings loads must
// degrade only its own group, never blank the whole screen (the old
// Promise.all behavior this replaces). DOM rendering (markFieldLoadFailed,
// renderSettingsLoadErrors) stays in app.js and isn't covered here - only
// the settled-results bookkeeping is.

import { test } from "node:test";
import assert from "node:assert/strict";
import { SETTINGS_FIELD_GROUPS, describeError, summarizeSettledResults, runSettingsPaintSteps } from "./settings-load.js";

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

// ---------------------------------------------------------------------
// runSettingsPaintSteps
//
// Regression coverage for the field defect this task fixes: openSettings()
// used to run its DOM-painting steps as one flat unguarded sequence ending
// in the screen's reveal line, so a throw partway through (a missing
// element, a bad value, ...) skipped the reveal entirely - "click Settings,
// nothing happens, no banner, no screen". These tests exercise the actual
// bug: a step in the middle of the list throws, and the contract that
// matters is (a) every OTHER step still runs (so the rest of the screen
// still paints) and (b) the failure is reported back to the caller instead
// of escaping - never that the caller's later reveal line runs, since that
// line lives in app.js, not here; app.js's own try/finally is what turns
// "failure reported" into "screen still opens", and that finally block is
// unconditional by construction (nothing here could make it skip).
// ---------------------------------------------------------------------

test("runSettingsPaintSteps: every step runs even when one throws (mutation target)", () => {
  const ran = [];
  const failed = runSettingsPaintSteps([
    { name: "a", run: () => ran.push("a") },
    {
      name: "b",
      run: () => {
        ran.push("b-before-throw");
        throw new TypeError("cannot read property 'value' of null");
      },
    },
    { name: "c", run: () => ran.push("c") },
  ]);
  // The step AFTER the throwing one still ran - this is the exact
  // guarantee that keeps one broken renderer from blanking the rest of
  // Settings. Mutate runSettingsPaintSteps to `return steps.map(s =>
  // s.run())` (no try/catch) and this assertion is what breaks: "c" never
  // gets pushed because the throw from "b" would escape the loop instead
  // of being caught per-step.
  assert.deepEqual(ran, ["a", "b-before-throw", "c"]);
  assert.equal(failed.length, 1);
  assert.equal(failed[0].name, "b");
});

test("runSettingsPaintSteps: no failures when every step succeeds", () => {
  const failed = runSettingsPaintSteps([{ name: "a", run: () => {} }, { name: "b", run: () => {} }]);
  assert.deepEqual(failed, []);
});

test("runSettingsPaintSteps: a thrown value is stringified, never forwarded raw", () => {
  const err = new Error("boom");
  err.secretField = "must not leak";
  const failed = runSettingsPaintSteps([{ name: "x", run: () => { throw err; } }]);
  assert.equal(typeof failed[0].error, "string");
  assert.equal(failed[0].error, "boom");
});

test("runSettingsPaintSteps: multiple independent throws are all reported, each tagged with its own step name", () => {
  const failed = runSettingsPaintSteps([
    { name: "one", run: () => { throw new Error("first"); } },
    { name: "two", run: () => {} },
    { name: "three", run: () => { throw new Error("second"); } },
  ]);
  assert.deepEqual(
    failed.map((f) => f.name),
    ["one", "three"],
  );
});

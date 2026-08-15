// Tests for settings-load.js's pure logic behind openSettings' partial-
// load AND partial-paint fixes: one failed IPC call among the 8
// openSettings loads must degrade only its own group (never blank the
// whole screen - the old Promise.all behavior this replaces), and one
// broken paint step among the ones that follow must never cost the
// operator the Settings screen itself - see runSettingsPaintSteps' own
// header comment for why the reveal-always-happens guarantee lives (and
// is tested) here rather than in app.js. Actual DOM rendering
// (markFieldLoadFailed, renderSettingsLoadErrors, and the real
// `$("screen-settings").hidden = false` reveal app.js passes in as
// `reveal`) stays in app.js and isn't covered here - only the pure
// decision logic each of those functions is built on.

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
// nothing happens, no banner, no screen".
//
// Ronda 2 (coordinator review, 2026-08-15): the first pass only tested
// step isolation (every step runs, failures are collected) and left the
// actual reveal-always-happens guarantee living in app.js's own
// try/finally - unverifiable, since app.js is DOM/webview-only by
// convention and has no test harness. `reveal` is now a required second
// argument to runSettingsPaintSteps itself, called exactly once after
// every step has run, so THAT guarantee - not just step isolation - has a
// real test here. A fake `reveal` (a counting/recording function, no DOM)
// is what makes this testable without a webview.
// ---------------------------------------------------------------------

function countingReveal() {
  const fn = () => {
    fn.calls += 1;
  };
  fn.calls = 0;
  return fn;
}

test("runSettingsPaintSteps: every step runs even when one throws (mutation target for step isolation)", () => {
  const ran = [];
  const reveal = countingReveal();
  const failed = runSettingsPaintSteps(
    [
      { name: "a", run: () => ran.push("a") },
      {
        name: "b",
        run: () => {
          ran.push("b-before-throw");
          throw new TypeError("cannot read property 'value' of null");
        },
      },
      { name: "c", run: () => ran.push("c") },
    ],
    reveal,
  );
  // The step AFTER the throwing one still ran - this is the exact
  // guarantee that keeps one broken renderer from blanking the rest of
  // Settings. Mutate runSettingsPaintSteps to `steps.forEach(s => s.run())`
  // (no try/catch) and this assertion is what breaks: "c" never gets
  // pushed because the throw from "b" would escape the loop instead of
  // being caught per-step.
  assert.deepEqual(ran, ["a", "b-before-throw", "c"]);
  assert.equal(failed.length, 1);
  assert.equal(failed[0].name, "b");
});

test("runSettingsPaintSteps: no failures when every step succeeds", () => {
  const reveal = countingReveal();
  const failed = runSettingsPaintSteps([{ name: "a", run: () => {} }, { name: "b", run: () => {} }], reveal);
  assert.deepEqual(failed, []);
});

test("runSettingsPaintSteps: a thrown value is stringified, never forwarded raw", () => {
  const err = new Error("boom");
  err.secretField = "must not leak";
  const failed = runSettingsPaintSteps([{ name: "x", run: () => { throw err; } }], countingReveal());
  assert.equal(typeof failed[0].error, "string");
  assert.equal(failed[0].error, "boom");
});

test("runSettingsPaintSteps: multiple independent throws are all reported, each tagged with its own step name", () => {
  const failed = runSettingsPaintSteps(
    [
      { name: "one", run: () => { throw new Error("first"); } },
      { name: "two", run: () => {} },
      { name: "three", run: () => { throw new Error("second"); } },
    ],
    countingReveal(),
  );
  assert.deepEqual(
    failed.map((f) => f.name),
    ["one", "three"],
  );
});

// -- the reveal guarantee itself (ronda 2's actual ask) ------------------

test("runSettingsPaintSteps: reveal(1): ALL steps throw -> reveal is still called, exactly once (mutation target for the reveal guarantee)", () => {
  const reveal = countingReveal();
  const failed = runSettingsPaintSteps(
    [
      { name: "a", run: () => { throw new Error("a broke"); } },
      { name: "b", run: () => { throw new Error("b broke"); } },
      { name: "c", run: () => { throw new Error("c broke"); } },
    ],
    reveal,
  );
  // This is the invariant the whole task exists for: Settings must always
  // end up visible, no matter how many paint steps failed - including the
  // worst case, ALL of them. Remove the `reveal()` call (or the try/catch
  // around each step's `run()`, which would make an earlier throw skip
  // both the reveal AND the later steps) and this assertion is what
  // catches it: `reveal.calls` stays 0 instead of becoming 1.
  assert.equal(reveal.calls, 1);
  assert.equal(failed.length, 3);
});

test("runSettingsPaintSteps: reveal(2): a mix of throwing and succeeding steps -> reveal is called, failed[] names only the ones that threw", () => {
  const reveal = countingReveal();
  const failed = runSettingsPaintSteps(
    [
      { name: "ok1", run: () => {} },
      { name: "bad1", run: () => { throw new Error("bad1 broke"); } },
      { name: "ok2", run: () => {} },
      { name: "bad2", run: () => { throw new Error("bad2 broke"); } },
    ],
    reveal,
  );
  assert.equal(reveal.calls, 1);
  assert.deepEqual(
    failed.map((f) => f.name),
    ["bad1", "bad2"],
  );
});

test("runSettingsPaintSteps: reveal(3): no step throws -> reveal is called once, zero failures", () => {
  const reveal = countingReveal();
  const failed = runSettingsPaintSteps([{ name: "a", run: () => {} }, { name: "b", run: () => {} }], reveal);
  assert.equal(reveal.calls, 1);
  assert.deepEqual(failed, []);
});

test("runSettingsPaintSteps: reveal(4), the ugly case: reveal() itself throws -> the exception propagates (not swallowed, not retried), reveal was still attempted exactly once", () => {
  // Documented behavior (see runSettingsPaintSteps' own header comment):
  // a broken `reveal` is a different, rarer failure than a broken paint
  // step - the mechanism that shows the screen is itself broken, not a
  // section that failed to paint. This function does not catch that
  // (there's nothing sane to do with the already-collected `failed` array
  // once the reveal step is what's broken), does not retry, and does not
  // turn it into a silent success. It must be loud.
  let revealAttempts = 0;
  const explodingReveal = () => {
    revealAttempts += 1;
    throw new Error("reveal itself is broken");
  };
  assert.throws(
    () => runSettingsPaintSteps([{ name: "a", run: () => {} }], explodingReveal),
    /reveal itself is broken/,
  );
  // Not retried: exactly one attempt, not zero (never called) and not more
  // than one (silently retried after failing).
  assert.equal(revealAttempts, 1);
});

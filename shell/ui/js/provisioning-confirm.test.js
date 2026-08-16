// node:test coverage for provisioning-confirm.js's pure deep-link warning
// rule (RISK 4R finding, 2026-08-16, round 2). Mirrors blf-ui.test.js /
// updater.test.js's pure-function convention - no DOM harness; the rule
// itself is what's locked, app.js's applier (inside showProvisioningConfirm)
// is verified visually like the other renderers here.

import { test } from "node:test";
import assert from "node:assert/strict";
import { computeDeepLinkWarning } from "./provisioning-confirm.js";

// The whole point of round 2's fix: a deep-link-sourced preview turns the
// warning on.
test("from_deep_link true: warning shown, host highlighted", () => {
  const result = computeDeepLinkWarning({ from_deep_link: true, host: "pbx.example.test", ext: "9999" });
  assert.equal(result.hostWarnClass, true, "host plate must get the warning class");
  assert.equal(result.warningHidden, false, "warning text must be visible");
});

// A manual paste (provisioning_resolve) never shows the warning - pasting a
// link already implies the operator trusted the source enough to copy it.
test("from_deep_link false: warning hidden, no highlight", () => {
  const result = computeDeepLinkWarning({ from_deep_link: false, host: "pbx.example.test", ext: "9999" });
  assert.equal(result.hostWarnClass, false);
  assert.equal(result.warningHidden, true);
});

// Defensive default: showProvisioningConfirm already guards on `!preview`
// before calling this, but the pure function itself must never throw or
// default to the WARNING-SHOWN state on missing/malformed input - a silent
// failure here should look like "no preview", not "false alarm every time".
test("missing or malformed preview defaults to hidden, not shown", () => {
  assert.deepEqual(computeDeepLinkWarning(null), { hostWarnClass: false, warningHidden: true });
  assert.deepEqual(computeDeepLinkWarning(undefined), { hostWarnClass: false, warningHidden: true });
  assert.deepEqual(computeDeepLinkWarning({}), { hostWarnClass: false, warningHidden: true });
});

// Truthiness coercion: `from_deep_link` must be read as a boolean, not
// passed through raw - a stray truthy non-boolean (defensive; the Rust side
// always serializes an actual bool) must still resolve to exactly `true`/
// `false`, not something merely truthy/falsy, since these values feed
// classList.toggle()'s second argument directly.
test("from_deep_link is coerced to an actual boolean", () => {
  assert.equal(computeDeepLinkWarning({ from_deep_link: 1 }).hostWarnClass, true);
  assert.equal(computeDeepLinkWarning({ from_deep_link: 1 }).warningHidden, false);
  assert.equal(computeDeepLinkWarning({ from_deep_link: 0 }).hostWarnClass, false);
});

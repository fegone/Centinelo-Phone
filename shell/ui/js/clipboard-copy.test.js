import { test } from "node:test";
import assert from "node:assert/strict";
import { resolveClipboardCopyOutcome } from "./clipboard-copy.js";

test("async Clipboard API succeeds: copied, no fallback used", () => {
  const outcome = resolveClipboardCopyOutcome({ asyncOk: true, fallbackAttempted: false, fallbackOk: false });
  assert.deepEqual(outcome, { copied: true, usedFallback: false });
});

test("async fails, execCommand fallback succeeds: copied via fallback", () => {
  const outcome = resolveClipboardCopyOutcome({ asyncOk: false, fallbackAttempted: true, fallbackOk: true });
  assert.deepEqual(outcome, { copied: true, usedFallback: true });
});

test("hallazgo #4 - async fails AND execCommand fallback returns false: NOT copied", () => {
  const outcome = resolveClipboardCopyOutcome({ asyncOk: false, fallbackAttempted: true, fallbackOk: false });
  assert.deepEqual(outcome, { copied: false, usedFallback: true });
});

test("hallazgo #4 - async fails AND execCommand itself throws (caller passes fallbackOk:false): NOT copied", () => {
  // Same shape a caller reports when execCommand("copy") throws instead of
  // returning false - the caller catches it and passes fallbackOk:false,
  // fallbackAttempted:true either way.
  const outcome = resolveClipboardCopyOutcome({ asyncOk: false, fallbackAttempted: true, fallbackOk: false });
  assert.equal(outcome.copied, false);
});

test("async fails and fallback never attempted (e.g. no #bridge-token element): NOT copied", () => {
  const outcome = resolveClipboardCopyOutcome({ asyncOk: false, fallbackAttempted: false, fallbackOk: false });
  assert.deepEqual(outcome, { copied: false, usedFallback: false });
});

import { test } from "node:test";
import assert from "node:assert/strict";
import { shouldFinalizeClosedCall } from "./call-lifecycle.js";

test("matching call_id: finalizes", () => {
  const current = { callId: "abc123", state: "established" };
  assert.equal(shouldFinalizeClosedCall(current, "abc123"), true);
});

test("hallazgo #5 - mismatched call_id: does NOT finalize the call on screen", () => {
  // The exact scenario the audit confirmed reachable: a second incoming
  // call ("callB") has overwritten state.call, but the FIRST call
  // ("callA") is the one that just closed at the engine.
  const stillOnScreen = { callId: "callB", state: "established" };
  assert.equal(shouldFinalizeClosedCall(stillOnScreen, "callA"), false);
});

test("no current call: nothing to finalize", () => {
  assert.equal(shouldFinalizeClosedCall(null, "abc123"), false);
  assert.equal(shouldFinalizeClosedCall(undefined, "abc123"), false);
});

test("current call has no callId (v0-compat/never-observed-an-id edge): falls back to always finalize", () => {
  const current = { state: "established" }; // no callId field at all
  assert.equal(shouldFinalizeClosedCall(current, "abc123"), true);
});

test("closed event carries no call_id (v0-compat edge): falls back to always finalize", () => {
  const current = { callId: "abc123", state: "established" };
  assert.equal(shouldFinalizeClosedCall(current, null), true);
  assert.equal(shouldFinalizeClosedCall(current, undefined), true);
});

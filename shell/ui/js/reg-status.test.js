// Tests for reg-status.js's pure state machine (the Settings-Save ->
// registration-result handshake). Same node:test style as
// updater.test.js; DOM/i18n rendering (renderSaveStatusForRegState,
// releaseSaveButton) stays in app.js and isn't covered here - only the
// reducer's decisions are.

import { test } from "node:test";
import assert from "node:assert/strict";
import { armHandshake, reduceRegHandshake, isTerminalRegAction, shouldReleaseSaveButton } from "./reg-status.js";

// ---------------------------------------------------------------------
// armHandshake
// ---------------------------------------------------------------------

test("armHandshake: wraps the generation it's given, nothing more", () => {
  assert.deepEqual(armHandshake(1), { generation: 1 });
  assert.deepEqual(armHandshake(7), { generation: 7 });
});

// ---------------------------------------------------------------------
// (1) registered after a save -> Connected
// ---------------------------------------------------------------------

test("reduceRegHandshake: registered at the handshake's own generation -> show-connected, terminal", () => {
  const handshake = armHandshake(1);
  const action = reduceRegHandshake(handshake, 1, { type: "reg_state", state: "registered" });
  assert.deepEqual(action, { type: "show-connected" });
  assert.equal(isTerminalRegAction(action), true);
});

// ---------------------------------------------------------------------
// (2) failed with a reason -> "Attempt failed: <reason> - retrying..."
// (regStatus.failedRetrying's {reason} placeholder - see i18n.js)
// ---------------------------------------------------------------------

test("reduceRegHandshake: failed with a reason -> show-failed-retrying, NOT terminal", () => {
  const handshake = armHandshake(1);
  const action = reduceRegHandshake(handshake, 1, {
    type: "reg_state",
    state: "failed",
    reason: "401 Unauthorized",
  });
  assert.deepEqual(action, { type: "show-failed-retrying", reason: "401 Unauthorized" });
  assert.equal(isTerminalRegAction(action), false);
});

test("reduceRegHandshake: failed with no reason -> show-failed-retrying with a null reason (caller's t() fills the fallback)", () => {
  const handshake = armHandshake(1);
  const action = reduceRegHandshake(handshake, 1, { type: "reg_state", state: "failed", reason: null });
  assert.deepEqual(action, { type: "show-failed-retrying", reason: null });
});

// ---------------------------------------------------------------------
// (3) failed, then registered -> ends Connected, never stuck on the first
// failure (the engine auto-retries registration after a failure; only
// `registered` is terminal for this handshake).
// ---------------------------------------------------------------------

test("reduceRegHandshake: failed then registered on the SAME handshake -> ends show-connected, not frozen on the earlier failure", () => {
  const handshake = armHandshake(3);

  const first = reduceRegHandshake(handshake, 3, { type: "reg_state", state: "failed", reason: "408 Timeout" });
  assert.equal(first.type, "show-failed-retrying");
  assert.equal(isTerminalRegAction(first), false);

  // Real app.js never clears `handshake` on a non-terminal action (see
  // renderSaveStatusForRegState) - the same handshake object is still the
  // one in play for the next event.
  const second = reduceRegHandshake(handshake, 3, { type: "reg_state", state: "registered" });
  assert.deepEqual(second, { type: "show-connected" });
  assert.equal(isTerminalRegAction(second), true);
});

// ---------------------------------------------------------------------
// (4) a reg_state from an old generation -> ignored (a newer Save has
// since armed its own handshake; this stale one must not resolve/repaint).
// ---------------------------------------------------------------------

test("reduceRegHandshake: currentGeneration ahead of the handshake's own -> ignore-stale, even for a registered event", () => {
  const staleHandshake = armHandshake(1);
  const action = reduceRegHandshake(staleHandshake, 2, { type: "reg_state", state: "registered" });
  assert.deepEqual(action, { type: "ignore-stale" });
  assert.equal(isTerminalRegAction(action), false);
});

test("reduceRegHandshake: no handshake at all -> ignore-stale (no Save is awaiting anything)", () => {
  const action = reduceRegHandshake(null, 5, { type: "reg_state", state: "registered" });
  assert.deepEqual(action, { type: "ignore-stale" });
});

// ---------------------------------------------------------------------
// non-terminal / in-flight reg_states
// ---------------------------------------------------------------------

test("reduceRegHandshake: registering/unregistered/anything non-terminal -> keep-connecting", () => {
  const handshake = armHandshake(1);
  for (const s of ["registering", "unregistered", "", undefined]) {
    const action = reduceRegHandshake(handshake, 1, { type: "reg_state", state: s });
    assert.deepEqual(action, { type: "keep-connecting" });
  }
});

// ---------------------------------------------------------------------
// (5) timeout with no registered ever arriving
// ---------------------------------------------------------------------

test("reduceRegHandshake: timeout on the still-current handshake -> timeout, terminal", () => {
  const handshake = armHandshake(4);
  const action = reduceRegHandshake(handshake, 4, { type: "timeout" });
  assert.deepEqual(action, { type: "timeout" });
  assert.equal(isTerminalRegAction(action), true);
});

test("reduceRegHandshake: timeout on an already-superseded handshake -> ignore-stale, not timeout", () => {
  const staleHandshake = armHandshake(1);
  const action = reduceRegHandshake(staleHandshake, 2, { type: "timeout" });
  assert.deepEqual(action, { type: "ignore-stale" });
});

// ---------------------------------------------------------------------
// isTerminalRegAction
// ---------------------------------------------------------------------

test("isTerminalRegAction: only show-connected and timeout are terminal", () => {
  assert.equal(isTerminalRegAction({ type: "show-connected" }), true);
  assert.equal(isTerminalRegAction({ type: "timeout" }), true);
  assert.equal(isTerminalRegAction({ type: "keep-connecting" }), false);
  assert.equal(isTerminalRegAction({ type: "show-failed-retrying", reason: "x" }), false);
  assert.equal(isTerminalRegAction({ type: "ignore-stale" }), false);
});

// ---------------------------------------------------------------------
// shouldReleaseSaveButton
// ---------------------------------------------------------------------

test("shouldReleaseSaveButton: true when the finishing save is still the newest generation", () => {
  assert.equal(shouldReleaseSaveButton(2, 2), true);
});

test("shouldReleaseSaveButton: false when a newer save has since armed - an older save's terminal path must not re-enable the button out from under it", () => {
  assert.equal(shouldReleaseSaveButton(3, 2), false);
});

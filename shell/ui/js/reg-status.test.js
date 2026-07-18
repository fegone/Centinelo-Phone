// Tests for reg-status.js's pure state machine (the Settings-Save ->
// registration-result handshake). Same node:test style as
// updater.test.js; DOM/i18n rendering (renderSaveStatusForRegState,
// releaseSaveButton) stays in app.js and isn't covered here - only the
// reducer's decisions are.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  armHandshake,
  reduceRegHandshake,
  isTerminalRegAction,
  shouldReleaseSaveButton,
  shouldShowInterimConnecting,
  reduceRegResult,
} from "./reg-status.js";

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

// ---------------------------------------------------------------------
// shouldShowInterimConnecting / reduceRegResult (2026-07-18 RELIABILITY
// regression fix: saveAccountSettings used to repaint #save-status to
// "Connecting…" unconditionally right before awaiting the handshake's
// result, even when a terminal reg_state had already landed - and already
// resolved that same promise - during the save's own intermediate invokes.
// That stomped an already-correct "Connected" back to "Connecting…", which
// then never got corrected because the code assumed "already painted,
// nothing to do here".
// ---------------------------------------------------------------------

test("shouldShowInterimConnecting: true while THIS save's own handshake is still genuinely pending", () => {
  assert.equal(shouldShowInterimConnecting(1, 1), true);
});

test("shouldShowInterimConnecting: false once the handshake has already settled (pendingRegResult cleared)", () => {
  assert.equal(shouldShowInterimConnecting(null, 1), false);
});

test("shouldShowInterimConnecting: false if a newer save has since armed its own handshake", () => {
  assert.equal(shouldShowInterimConnecting(2, 1), false);
});

test("reduceRegResult: a registered result -> show-connected", () => {
  assert.deepEqual(reduceRegResult({ state: "registered" }), { type: "show-connected" });
});

test("reduceRegResult: a timedOut result -> keep-last (don't touch whatever's already shown)", () => {
  assert.deepEqual(reduceRegResult({ timedOut: true }), { type: "keep-last" });
});

test("regression: a terminal reg_state arriving BEFORE the final await ends in Connected, not a stale Connecting", () => {
  // Simulates saveAccountSettings's own sequence for one Save (generation 1):
  const myGeneration = 1;
  const handshake = armHandshake(myGeneration);

  // The terminal reg_state lands during the save's intermediate invokes,
  // exactly like renderSaveStatusForRegState's real call: settles the
  // handshake and (in app.js) paints #save-status green immediately.
  const liveAction = reduceRegHandshake(handshake, myGeneration, { type: "reg_state", state: "registered" });
  assert.deepEqual(liveAction, { type: "show-connected" });
  assert.equal(isTerminalRegAction(liveAction), true);

  // app.js clears state.pendingRegResult the instant that happens - by the
  // time saveAccountSettings reaches its own interim-repaint check, there
  // is no pending handshake left for this generation.
  const pendingHandshakeGenerationAfterSettling = null;

  // The fix: the interim "Connecting…" repaint must NOT run here.
  assert.equal(shouldShowInterimConnecting(pendingHandshakeGenerationAfterSettling, myGeneration), false);

  // awaitRegResult's promise already resolved with the real outcome the
  // instant renderSaveStatusForRegState settled it above.
  const result = { state: "registered" };
  const finalAction = reduceRegResult(result);
  assert.deepEqual(finalAction, { type: "show-connected" });
  // i.e. #save-status ends on "Connected" - never overwritten to
  // "Connecting…" in between and never left stuck there.
});

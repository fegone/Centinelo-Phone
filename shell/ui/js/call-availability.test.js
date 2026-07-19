// Tests for call-availability.js's pure computeCallHandling rule. Same
// node:test style as reg-status.test.js/updater.test.js. Mirrors
// sidecar.rs's own `effective_answer_mode`/`should_auto_reject_incoming`
// unit tests one-to-one - both sides must agree on all 4 combinations.

import { test } from "node:test";
import assert from "node:assert/strict";
import { computeCallHandling } from "./call-availability.js";

test("available + manual (shipped default): manual answer mode, no auto-reject", () => {
  const result = computeCallHandling({ available: true, autoAnswer: false });
  assert.deepEqual(result, { answerMode: "manual", autoRejectIncoming: false });
});

test("available + auto-answer on: auto answer mode, no auto-reject", () => {
  const result = computeCallHandling({ available: true, autoAnswer: true });
  assert.deepEqual(result, { answerMode: "auto", autoRejectIncoming: false });
});

test("not available (DND) + auto-answer off: manual mode, auto-reject every incoming call", () => {
  const result = computeCallHandling({ available: false, autoAnswer: false });
  assert.deepEqual(result, { answerMode: "manual", autoRejectIncoming: true });
});

test("not available (DND) + auto-answer ON: availability still wins - manual mode, auto-reject, auto_answer is ignored", () => {
  const result = computeCallHandling({ available: false, autoAnswer: true });
  assert.deepEqual(result, { answerMode: "manual", autoRejectIncoming: true });
});

test("regression: toggling auto_answer while NOT available never flips autoRejectIncoming off", () => {
  // The exact race the task brief calls out: "la interacción no debe
  // tener race - la disponibilidad manda sobre el auto-answer". Both
  // calls below share available:false; only autoAnswer changes, and
  // autoRejectIncoming must stay true in both.
  const off = computeCallHandling({ available: false, autoAnswer: false });
  const on = computeCallHandling({ available: false, autoAnswer: true });
  assert.equal(off.autoRejectIncoming, true);
  assert.equal(on.autoRejectIncoming, true);
  assert.equal(off.answerMode, "manual");
  assert.equal(on.answerMode, "manual");
});

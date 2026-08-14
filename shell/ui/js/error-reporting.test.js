// Tests for error-reporting.js's pure report-shaping logic. The IPC/DOM-
// touching half (reportFrontendIssue, logMilestone) isn't covered here —
// same convention as reg-status.test.js vs app.js's own rendering glue.

import { test } from "node:test";
import assert from "node:assert/strict";
import { capText, levelForKind, buildErrorReport } from "./error-reporting.js";

// ---------------------------------------------------------------------
// capText
// ---------------------------------------------------------------------

test("capText: returns short strings unchanged", () => {
  assert.equal(capText("boom", 10), "boom");
});

test("capText: truncates strings longer than maxLength", () => {
  assert.equal(capText("abcdefgh", 4), "abcd");
});

test("capText: non-string input becomes an empty string, never throws", () => {
  assert.equal(capText(undefined, 10), "");
  assert.equal(capText(null, 10), "");
  assert.equal(capText(42, 10), "");
});

// ---------------------------------------------------------------------
// levelForKind
// ---------------------------------------------------------------------

test("levelForKind: milestone reports are info", () => {
  assert.equal(levelForKind("milestone"), "info");
});

test("levelForKind: every other kind defaults to error", () => {
  assert.equal(levelForKind("error"), "error");
  assert.equal(levelForKind("unhandledrejection"), "error");
  assert.equal(levelForKind("resource"), "error");
  assert.equal(levelForKind("something_new"), "error");
});

// ---------------------------------------------------------------------
// buildErrorReport
// ---------------------------------------------------------------------

test("buildErrorReport: shapes a plain error with no extra fields", () => {
  assert.deepEqual(buildErrorReport("error", { message: "boom" }), {
    kind: "error",
    level: "error",
    message: "boom",
    source: null,
    line: null,
    col: null,
    stack: null,
  });
});

test("buildErrorReport: carries source/line/col/stack through when present", () => {
  const report = buildErrorReport("error", {
    message: "boom",
    source: "js/app.js",
    line: 42,
    col: 7,
    stack: "at foo (app.js:1:1)",
  });
  assert.equal(report.source, "js/app.js");
  assert.equal(report.line, 42);
  assert.equal(report.col, 7);
  assert.equal(report.stack, "at foo (app.js:1:1)");
});

test("buildErrorReport: an explicit level overrides the kind-based default", () => {
  const report = buildErrorReport("resource", { message: "SCRIPT failed to load", level: "warn" });
  assert.equal(report.level, "warn");
});

test("buildErrorReport: milestone reports default to level info", () => {
  const report = buildErrorReport("milestone", { message: "ui_ready" });
  assert.equal(report.level, "info");
});

test("buildErrorReport: a missing/falsy kind falls back to \"error\"", () => {
  assert.equal(buildErrorReport(undefined, { message: "boom" }).kind, "error");
  assert.equal(buildErrorReport("", { message: "boom" }).kind, "error");
});

test("buildErrorReport: non-finite line/col (NaN, undefined, Infinity) become null, not NaN", () => {
  const report = buildErrorReport("error", { message: "boom", line: NaN, col: undefined });
  assert.equal(report.line, null);
  assert.equal(report.col, null);
});

test("buildErrorReport: an empty source/stack string is treated as absent", () => {
  const report = buildErrorReport("error", { message: "boom", source: "", stack: "" });
  assert.equal(report.source, null);
  assert.equal(report.stack, null);
});

test("buildErrorReport: never throws on a non-Error rejection reason (e.g. a plain string or object)", () => {
  assert.doesNotThrow(() => buildErrorReport("unhandledrejection", { message: 42 }));
  assert.doesNotThrow(() => buildErrorReport("unhandledrejection", {}));
});

test("buildErrorReport: an overlong message is capped, not silently dropped", () => {
  const long = "x".repeat(5000);
  const report = buildErrorReport("error", { message: long });
  assert.equal(report.message.length, 2000);
});

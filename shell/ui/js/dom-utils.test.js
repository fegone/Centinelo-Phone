// Tests for dom-utils.js (2026-07-16 4R re-review, READABILITY R2 -
// extracted from app.js/transcript-panel.js, which had grown byte-
// identical copies of these two functions). transcript-panel.test.js
// already exercises escapeHtml/escapeAttr indirectly via
// transcript-panel.js's own `__testables` (now re-exporting these same
// imported functions) - this file covers the shared module directly so
// coverage doesn't depend on which consumer happens to re-export it.

import { test } from "node:test";
import assert from "node:assert/strict";
import { escapeHtml, escapeAttr } from "./dom-utils.js";

test("escapeHtml: escapes &, <, > for safe text-node content", () => {
  assert.equal(escapeHtml(`<script>alert("hi")</script> & Co`), `&lt;script&gt;alert("hi")&lt;/script&gt; &amp; Co`);
});

test("escapeHtml: does NOT escape quotes (only safe for text-node content, not attributes)", () => {
  assert.equal(escapeHtml(`say "hi"`), `say "hi"`);
});

test("escapeHtml: null/undefined become an empty string, not the literal 'null'/'undefined'", () => {
  assert.equal(escapeHtml(null), "");
  assert.equal(escapeHtml(undefined), "");
});

test("escapeHtml: non-string input is coerced via String(), not thrown on", () => {
  assert.equal(escapeHtml(42), "42");
});

test("escapeAttr: escapes quotes on top of everything escapeHtml already does - safe inside a double-quoted HTML attribute", () => {
  const malicious = `Wednesday" onmouseover="alert(1)`;
  const escaped = escapeAttr(malicious);
  assert.ok(!escaped.includes('"'), "no raw double-quote should survive");
  assert.ok(escaped.includes("&quot;"), "double-quotes become &quot;");
  const rebuilt = `<div data-x="${escaped}">`;
  assert.equal((rebuilt.match(/"/g) || []).length, 2, "exactly the two attribute-delimiter quotes should remain");
});

test("escapeAttr: also escapes single quotes", () => {
  assert.ok(!escapeAttr(`it's a test`).includes("'"));
});

test("escapeAttr: also escapes &, <, > (inherits escapeHtml's behavior)", () => {
  assert.equal(escapeAttr(`<a & b>`), "&lt;a &amp; b&gt;");
});

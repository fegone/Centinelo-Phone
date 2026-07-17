// Tests for transcript-panel.js's pure logic (2026-07-16 4R re-review, T1
// - this module had zero automated coverage despite exporting
// `__testables` specifically for it). Node's built-in test runner
// (`node:test`/`node:assert`) - no new devDependency, matching this
// project's existing "no bundler, no frontend framework" philosophy.
// Run: `npm test` (from shell/) or `node --test ui/js/*.test.js`.
//
// Only the DOM-free half of the module is covered here (sortedSegments,
// speakerLabel, highlightQuery, escapeHtml/escapeAttr, tapeHtml,
// plainTextTranscript) - `renderTranscriptBody`/`renderPendingRetriesOnly`
// build real DOM nodes and attach event listeners, which this project has
// no jsdom-style dependency for; those are verified visually instead (see
// shell/dev/transcript-mock.html + shell/E2E.md "Transcript panel").

import { test } from "node:test";
import assert from "node:assert/strict";
import { plainTextTranscript, __testables } from "./transcript-panel.js";

const { sortedSegments, speakerLabel, fmtDuration, fmtTurnClock, highlightQuery, escapeHtml, escapeAttr, tapeHtml, LIVE_TAPE_MAX_TURNS } =
  __testables;

// ---------------------------------------------------------------------
// sortedSegments - chronological interleave (ENTREGABLE 1: "dos voces...
// intercaladas cronologicamente por t0_ms", not trusted from arrival
// order since rx/tx are independent streams).
// ---------------------------------------------------------------------

test("sortedSegments: reorders out-of-arrival-order segments by t0Ms", () => {
  const segments = [
    { speaker: "caller", t0Ms: 5000, text: "second" },
    { speaker: "agent", t0Ms: 1000, text: "first" },
    { speaker: "agent", t0Ms: 9000, text: "third" },
  ];
  const sorted = sortedSegments(segments);
  assert.deepEqual(
    sorted.map((s) => s.text),
    ["first", "second", "third"]
  );
});

test("sortedSegments: stable for ties (keeps arrival order among equal t0Ms)", () => {
  const segments = [
    { speaker: "agent", t0Ms: 1000, text: "a" },
    { speaker: "caller", t0Ms: 1000, text: "b" },
  ];
  const sorted = sortedSegments(segments);
  assert.deepEqual(
    sorted.map((s) => s.text),
    ["a", "b"]
  );
});

test("sortedSegments: does not mutate the input array", () => {
  const segments = [
    { speaker: "caller", t0Ms: 5000, text: "second" },
    { speaker: "agent", t0Ms: 1000, text: "first" },
  ];
  const original = [...segments];
  sortedSegments(segments);
  assert.deepEqual(segments, original);
});

test("sortedSegments: treats a missing t0Ms as 0, not NaN/crash", () => {
  const segments = [{ speaker: "agent", text: "no timestamp" }, { speaker: "caller", t0Ms: 100, text: "has one" }];
  const sorted = sortedSegments(segments);
  assert.deepEqual(
    sorted.map((s) => s.text),
    ["no timestamp", "has one"]
  );
});

test("sortedSegments: empty/undefined input returns an empty array, not a throw", () => {
  assert.deepEqual(sortedSegments([]), []);
  assert.deepEqual(sortedSegments(undefined), []);
});

// ---------------------------------------------------------------------
// speakerLabel - tag word never inverted (the other half of ENTREGABLE
// 1's "distinguibles sin color-solo": word + pattern + weight).
// ---------------------------------------------------------------------

test("speakerLabel: agent -> You, caller -> Caller (never inverted)", () => {
  assert.equal(speakerLabel("agent"), "You");
  assert.equal(speakerLabel("caller"), "Caller");
});

test("speakerLabel: unknown speaker falls back to its own uppercased value, not You/Caller", () => {
  assert.equal(speakerLabel("robot"), "ROBOT");
});

test("speakerLabel: empty/missing speaker falls back to an em dash, not a crash", () => {
  assert.equal(speakerLabel(""), "—");
  assert.equal(speakerLabel(undefined), "—");
});

// ---------------------------------------------------------------------
// highlightQuery - the one amber pixel this module allows (TOKENS §1.4).
// ---------------------------------------------------------------------

test("highlightQuery: wraps a case-insensitive match in <mark>", () => {
  assert.equal(highlightQuery("Wednesday at 2:15", "wednesday"), "<mark>Wednesday</mark> at 2:15");
});

test("highlightQuery: wraps every occurrence, not just the first", () => {
  assert.equal(highlightQuery("Wednesday, then Wednesday again", "Wednesday"), "<mark>Wednesday</mark>, then <mark>Wednesday</mark> again");
});

test("highlightQuery: empty/whitespace-only query is a no-op", () => {
  assert.equal(highlightQuery("Wednesday at 2:15", ""), "Wednesday at 2:15");
  assert.equal(highlightQuery("Wednesday at 2:15", "   "), "Wednesday at 2:15");
});

test("highlightQuery: regex-special characters in the query are treated literally, not as regex syntax", () => {
  // A query like "2:15" or "a.b" must not be interpreted as a regex
  // (".": any char) - would silently over-match and could throw on
  // genuinely invalid regex syntax like an unbalanced "(".
  assert.equal(highlightQuery("call at 2:15 today", "2:15"), "call at <mark>2:15</mark> today");
  assert.doesNotThrow(() => highlightQuery("some (parenthetical) text", "(parenthetical"));
});

// ---------------------------------------------------------------------
// escapeHtml / escapeAttr (2026-07-16 4R re-review, M1 - RISK)
// ---------------------------------------------------------------------

test("escapeHtml: escapes &, <, > for safe text-node content", () => {
  assert.equal(escapeHtml(`<script>alert("hi")</script> & Co`), `&lt;script&gt;alert("hi")&lt;/script&gt; &amp; Co`);
});

test("escapeHtml: does NOT escape quotes (by design - only safe for text-node content, not attributes)", () => {
  // This is exactly the gap M1 exploited when this string used to be
  // interpolated into an attribute (`value="${escapeHtml(query)}"`) -
  // escapeAttr (below) is what closes it; asserting the un-escaped
  // behavior here documents the boundary so a future change doesn't
  // silently reintroduce the M1 shape by "fixing" escapeHtml instead of
  // using escapeAttr where an attribute context needs it.
  assert.equal(escapeHtml(`say "hi"`), `say "hi"`);
});

test("escapeAttr: escapes quotes on top of everything escapeHtml already does - safe inside a double-quoted HTML attribute", () => {
  const malicious = `Wednesday" onmouseover="alert(1)`;
  const escaped = escapeAttr(malicious);
  assert.ok(!escaped.includes('"'), "no raw double-quote should survive");
  assert.ok(escaped.includes("&quot;"), "double-quotes become &quot;");
  // Round-trip sanity: embedding the escaped value inside a
  // double-quoted attribute never produces a second `"` that could
  // close the attribute early.
  const rebuilt = `<div data-x="${escaped}">`;
  assert.equal((rebuilt.match(/"/g) || []).length, 2, "exactly the two attribute-delimiter quotes should remain");
});

test("escapeAttr: also escapes single quotes", () => {
  assert.ok(!escapeAttr(`it's a test`).includes("'"));
});

// ---------------------------------------------------------------------
// fmtDuration / fmtTurnClock - sanity on the mm:ss formatting used
// throughout the tape/facts chips.
// ---------------------------------------------------------------------

test("fmtDuration: zero-pads minutes and seconds", () => {
  assert.equal(fmtDuration(5), "00:05");
  assert.equal(fmtDuration(65), "01:05");
  assert.equal(fmtDuration(522), "08:42");
});

test("fmtDuration: clamps negative input to 00:00 rather than a negative/garbage string", () => {
  assert.equal(fmtDuration(-5), "00:00");
});

test("fmtTurnClock: converts a t0Ms timestamp to the same mm:ss format", () => {
  assert.equal(fmtTurnClock(522_000), "08:42");
  assert.equal(fmtTurnClock(0), "00:00");
});

// ---------------------------------------------------------------------
// tapeHtml - the live-view cap (2026-07-16 4R re-review, M4) must never
// silently reorder or drop chronological correctness, only bound how
// much of the (still fully correct) sorted list actually renders.
// ---------------------------------------------------------------------

function segmentsSpanning(count) {
  const out = [];
  for (let i = 0; i < count; i++) {
    out.push({ speaker: i % 2 === 0 ? "agent" : "caller", t0Ms: i * 1000, text: `turn ${i}` });
  }
  return out;
}

test("tapeHtml: capLive bounds the rendered turns to LIVE_TAPE_MAX_TURNS, keeping the MOST RECENT ones", () => {
  const model = { phase: "live", segments: segmentsSpanning(LIVE_TAPE_MAX_TURNS + 20), startedAt: Date.now() };
  const html = tapeHtml(model, { capLive: true });
  // Every one of the oldest 20 turns must be gone; every one of the most
  // recent LIVE_TAPE_MAX_TURNS must still be present.
  for (let i = 0; i < 20; i++) {
    assert.ok(!html.includes(`turn ${i}<`), `turn ${i} should have been dropped from the capped live view`);
  }
  for (let i = 20; i < LIVE_TAPE_MAX_TURNS + 20; i++) {
    assert.ok(html.includes(`turn ${i}<`), `turn ${i} should still be present`);
  }
  assert.ok(html.includes("Showing the most recent"), "should note the view is truncated");
});

test("tapeHtml: capLive is a no-op when the call has fewer segments than the cap", () => {
  const model = { phase: "live", segments: segmentsSpanning(5), startedAt: Date.now() };
  const html = tapeHtml(model, { capLive: true });
  assert.ok(!html.includes("Showing the most recent"), "should not claim truncation when nothing was truncated");
  for (let i = 0; i < 5; i++) assert.ok(html.includes(`turn ${i}<`));
});

test("tapeHtml: without capLive (the done/error phases), every segment renders regardless of count", () => {
  const model = { phase: "done", segments: segmentsSpanning(LIVE_TAPE_MAX_TURNS + 20), startedAt: Date.now(), endedAt: Date.now() };
  const html = tapeHtml(model);
  for (let i = 0; i < LIVE_TAPE_MAX_TURNS + 20; i++) {
    assert.ok(html.includes(`turn ${i}<`), `turn ${i} should be present - done phase never caps`);
  }
});

test("tapeHtml: an empty, non-listening call shows the empty-tape fallback message", () => {
  const html = tapeHtml({ phase: "done", segments: [], startedAt: Date.now() });
  assert.ok(html.includes("No speech was picked up"));
});

// ---------------------------------------------------------------------
// plainTextTranscript - the "Copy" action's output, always the FULL
// (uncapped) chronological list.
// ---------------------------------------------------------------------

test("plainTextTranscript: chronologically orders and labels every segment, one per line", () => {
  const model = {
    segments: [
      { speaker: "caller", t0Ms: 7000, text: "Hi there." },
      { speaker: "agent", t0Ms: 2000, text: "Hello!" },
    ],
  };
  assert.equal(plainTextTranscript(model), "[00:02] You: Hello!\n[00:07] Caller: Hi there.");
});

test("plainTextTranscript: never caps, unlike the live tape view", () => {
  const model = { segments: segmentsSpanning(LIVE_TAPE_MAX_TURNS + 20) };
  const lines = plainTextTranscript(model).split("\n");
  assert.equal(lines.length, LIVE_TAPE_MAX_TURNS + 20);
});

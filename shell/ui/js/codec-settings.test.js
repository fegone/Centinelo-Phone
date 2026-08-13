import { test } from "node:test";
import assert from "node:assert/strict";
import {
  describeCodec,
  buildCodecState,
  isDirty,
  applyToggle,
  applyMove,
  buildSaveCodecsInput,
  renderCodecsList,
} from "./codec-settings.js";

// ---- describeCodec --------------------------------------------------------

test("describeCodec: known codecs get their real, non-invented copy", () => {
  const opus = describeCodec("opus", { srate: 48000, ch: 2 });
  assert.equal(opus.name, "Opus");
  assert.equal(opus.chip, "Recommended");
  assert.match(opus.whisper, /^OPUS/);

  const pcmu = describeCodec("PCMU", { srate: 8000, ch: 1 });
  assert.equal(pcmu.name, "G.711 (µ-law)");
  assert.equal(pcmu.chip, null);
});

test("describeCodec: the G.711 whisper says U-LAW, never µ-LAW", () => {
  // The exact bug the task brief calls out: text-transform:uppercase on
  // the whisper's CSS class turns a literal µ into Μ (Greek capital mu),
  // reading as "M-LAW" - so the source string must already say "U-LAW".
  const pcmu = describeCodec("pcmu", { srate: 8000, ch: 1 });
  assert.ok(pcmu.whisper.includes("U-LAW"));
  assert.ok(!pcmu.whisper.includes("µ"), "the whisper must not contain the mu character at all");
});

test("describeCodec: PCMU and PCMA report identical meters on purpose", () => {
  const pcmu = describeCodec("PCMU", {});
  const pcma = describeCodec("PCMA", {});
  assert.deepEqual(pcmu.meters, pcma.meters);
});

test("describeCodec: an unrecognized name still renders, using only real engine data", () => {
  const g722 = describeCodec("G722", { srate: 16000, ch: 1 });
  assert.equal(g722.name, "G722");
  assert.equal(g722.chip, null);
  assert.equal(g722.meters, null, "no fabricated quality/data profile for an unknown codec");
  assert.ok(g722.whisper.includes("G722"));
  assert.ok(g722.whisper.includes("16 KHZ"));
  assert.ok(g722.whisper.includes("1CH"));
});

test("describeCodec: an unrecognized name with no srate/ch still renders without throwing", () => {
  const unknown = describeCodec("mystery", {});
  assert.equal(unknown.whisper, "MYSTERY");
});

// ---- buildCodecState -------------------------------------------------------

test("buildCodecState: order mirrors 'active' verbatim, off is the rest of 'available'", () => {
  const available = [{ name: "opus", srate: 48000, ch: 2 }, { name: "PCMU", srate: 8000, ch: 1 }, { name: "PCMA", srate: 8000, ch: 1 }];
  const active = ["opus", "PCMU"];
  const state = buildCodecState(available, active);
  assert.deepEqual(state, { order: ["opus", "PCMU"], off: ["PCMA"] });
});

test("buildCodecState: a freshly-registered engine with no customization reflects available verbatim as active", () => {
  // core/PROTOCOL.md: "active" falls back to "available" verbatim when the
  // account never had set_codecs applied - buildCodecState doesn't need to
  // special-case this, it just reflects whatever active says.
  const available = [{ name: "PCMU" }, { name: "PCMA" }];
  const active = ["PCMU", "PCMA"];
  assert.deepEqual(buildCodecState(available, active), { order: ["PCMU", "PCMA"], off: [] });
});

test("buildCodecState: a name in active that's no longer in available is dropped defensively", () => {
  const available = [{ name: "PCMU" }];
  const active = ["opus", "PCMU"]; // "opus" vanished from this build somehow
  assert.deepEqual(buildCodecState(available, active), { order: ["PCMU"], off: [] });
});

// ---- isDirty ----------------------------------------------------------------

test("isDirty: false when state matches saved, true otherwise", () => {
  const saved = { order: ["opus", "PCMU"], off: ["PCMA"] };
  assert.equal(isDirty(structuredClone(saved), saved), false);
  assert.equal(isDirty({ order: ["PCMU", "opus"], off: ["PCMA"] }, saved), true);
});

// ---- applyToggle --------------------------------------------------------

test("applyToggle: turning off a non-last codec moves it to the front of off", () => {
  const state = { order: ["opus", "PCMU"], off: ["PCMA"] };
  const result = applyToggle(state, "PCMU");
  assert.deepEqual(result.state, { order: ["opus"], off: ["PCMU", "PCMA"] });
  assert.equal(result.touchedId, "PCMU");
  assert.ok(!result.guarded);
});

test("applyToggle: turning on an off codec appends it to the end of order", () => {
  const state = { order: ["opus"], off: ["PCMU", "PCMA"] };
  const result = applyToggle(state, "PCMA");
  assert.deepEqual(result.state, { order: ["opus", "PCMA"], off: ["PCMU"] });
});

test("applyToggle: the last active codec cannot be turned off - guarded, state untouched", () => {
  const state = { order: ["opus"], off: ["PCMU", "PCMA"] };
  const result = applyToggle(state, "opus");
  assert.equal(result.guarded, true);
  assert.strictEqual(result.state, state, "state reference is unchanged, not merely equal");
});

// ---- applyMove ----------------------------------------------------------

test("applyMove: swaps with the neighbor in the given direction", () => {
  const state = { order: ["PCMU", "opus", "PCMA"], off: [] };
  const up = applyMove(state, "opus", "up");
  assert.deepEqual(up.state.order, ["opus", "PCMU", "PCMA"]);
  assert.equal(up.newPosition, 1);

  const down = applyMove(state, "opus", "down");
  assert.deepEqual(down.state.order, ["PCMU", "PCMA", "opus"]);
  assert.equal(down.newPosition, 3);
});

test("applyMove: refuses past either end", () => {
  const state = { order: ["opus", "PCMU"], off: [] };
  assert.equal(applyMove(state, "opus", "up"), null);
  assert.equal(applyMove(state, "PCMU", "down"), null);
});

test("applyMove: refuses to move an off codec at all", () => {
  const state = { order: ["opus"], off: ["PCMU"] };
  assert.equal(applyMove(state, "PCMU", "up"), null);
});

// ---- buildSaveCodecsInput -------------------------------------------------

test("buildSaveCodecsInput: matches commands::SaveCodecsInput's shape exactly", () => {
  const state = { order: ["opus", "PCMU"], off: ["PCMA"] };
  assert.deepEqual(buildSaveCodecsInput(state), { codecs: ["opus", "PCMU"] });
});

// ---- renderCodecsList -----------------------------------------------------

test("renderCodecsList: renders one row per available codec, on codecs numbered in order", () => {
  const available = [{ name: "opus", srate: 48000, ch: 2 }, { name: "PCMU", srate: 8000, ch: 1 }, { name: "PCMA", srate: 8000, ch: 1 }];
  const state = { order: ["opus", "PCMU"], off: ["PCMA"] };
  const html = renderCodecsList({ available, state, touched: new Set() });

  assert.match(html, /data-codec-id="opus"[\s\S]*?>1</);
  assert.match(html, /data-codec-id="PCMU"[\s\S]*?>2</);
  assert.match(html, /class="offhead"/);
  assert.match(html, /class="codec off"[^>]*data-codec-id="PCMA"/);
});

test("renderCodecsList: a touched row gets the 'changed' class, an untouched one doesn't", () => {
  const available = [{ name: "opus" }, { name: "PCMU" }];
  const state = { order: ["opus", "PCMU"], off: [] };
  const html = renderCodecsList({ available, state, touched: new Set(["opus"]) });
  assert.match(html, /class="codec changed" role="listitem" data-codec-id="opus"/);
  assert.match(html, /class="codec" role="listitem" data-codec-id="PCMU"/);
});

test("renderCodecsList: escapes an engine-supplied codec name before it reaches markup", () => {
  // Defense in depth: core/PROTOCOL.md's decode_codecs already rejects
  // most malformed names, but this UI must never trust a raw engine string
  // straight into innerHTML regardless.
  const available = [{ name: '<img src=x onerror=alert(1)>', srate: 8000, ch: 1 }];
  const state = { order: ['<img src=x onerror=alert(1)>'], off: [] };
  const html = renderCodecsList({ available, state, touched: new Set() });
  assert.ok(!html.includes("<img src=x"));
  assert.ok(html.includes("&lt;img"));
});

test("renderCodecsList: no meters rendered for an unrecognized codec (nothing invented)", () => {
  const available = [{ name: "G722", srate: 16000, ch: 1 }];
  const state = { order: ["G722"], off: [] };
  const html = renderCodecsList({ available, state, touched: new Set() });
  assert.ok(!html.includes('class="meters"'));
});

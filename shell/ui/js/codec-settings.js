// Settings → Audio & devices → Audio codecs (Plate 09,
// premium/design/mockups/settings-codecs.html, PR #6 design/settings-codecs).
//
// Pure state/render logic, zero Tauri dependency — same "testable without a
// live runtime" convention transcript-panel.js/transcription-settings.js/
// call-availability.js already document for themselves. app.js owns the
// invoke/listen wiring (querying `sidecar_list_codecs`, calling
// `save_codec_settings`, reacting to the `codecs`/`error` sidecar events)
// and hands this module plain data; this file never touches `document`,
// `invoke`, or a module-level mutable singleton.
//
// **Fuente de verdad (this workspace's own rule)**: the SET of codecs, and
// which ones are on/off/in-what-order, ALWAYS comes straight from the
// engine's own `core/PROTOCOL.md` v1.6 `codecs` event (`available`+
// `active`) via `buildCodecState` below — never a list hardcoded in this
// UI. What IS hardcoded here (`KNOWN_CODECS`) is presentation copy only —
// the human-language description/whisper/meter for a codec name this
// build's real event already told us exists. `describeCodec`'s fallback
// path proves this isn't secretly a codec catalog: an unrecognized name
// still renders (using only the real srate/ch the event provided), it just
// gets generic copy instead of hand-written copy.
//
// **No re-register, no in-call interruption, no deferred-until-hangup
// state** — `core/PROTOCOL.md`'s `set_codecs` takes effect starting the
// very next call (`call_streams_alloc()` reads the account's codec list
// fresh only when a NEW `struct call` is allocated); an already-established
// call keeps whatever it negotiated. There is deliberately no "apply is
// disabled during a call" logic anywhere below or in commands.rs/sidecar.rs
// — an earlier design draft assumed a re-register was needed and was
// wrong (see premium/design/notes/settings-codecs.md's own corrected
// premise note).

import { t } from "./i18n.js";
import { escapeHtml, escapeAttr } from "./dom-utils.js";

const METER_MAX_BARS = 4;

// Known per-codec presentation copy, keyed by the engine's own codec name
// (case-insensitively — core/PROTOCOL.md's `codecs` event lists whatever
// this build actually compiled in, "opus"/"PCMU"/"PCMA" today, see
// core/BUILD.md "Module selection"). Names are technical tokens and are
// NEVER translated (i18n.js's own header documents this register split);
// the G.711 whisper strings are identical in all three languages by design
// (design/notes/settings-codecs.md) so they're plain literals here too,
// not i18n.js entries.
const KNOWN_CODECS = {
  opus: {
    name: "Opus",
    chipKey: "settings.codecRecommended",
    descKey: "settings.codecOpusDesc",
    whisper: () => t("settings.codecOpusWhisper"),
    quality: { bars: 4, labelKey: "settings.codecQualityExcellent" },
    data: { bars: 1, labelKey: "settings.codecDataLow" },
  },
  pcmu: {
    name: "G.711 (µ-law)",
    chipKey: null,
    descKey: "settings.codecPcmuDesc",
    // "U-LAW", never "µ-LAW" — the whisper's CSS class applies
    // text-transform:uppercase, which turns µ into Μ (Greek capital mu),
    // reading as "M-LAW" (design note's own implementation callout).
    whisper: () => "G.711 U-LAW · PCMU · 64 KBPS",
    quality: { bars: 2, labelKey: "settings.codecQualityStandard" },
    data: { bars: 2, labelKey: "settings.codecDataMedium" },
  },
  pcma: {
    name: "G.711 (A-law)",
    chipKey: null,
    descKey: "settings.codecPcmaDesc",
    whisper: () => "G.711 A-LAW · PCMA · 64 KBPS",
    // Same 64 kbps as PCMU on purpose, not a typo — "el medidor no exagera
    // para vender a Opus" (design note).
    quality: { bars: 2, labelKey: "settings.codecQualityStandard" },
    data: { bars: 2, labelKey: "settings.codecDataMedium" },
  },
};

/// Presentation metadata for one codec name. `srate`/`ch` (from the
/// engine's own `available[].{srate,ch}`) are only used by the fallback
/// path, for a name `KNOWN_CODECS` doesn't recognize — real engine data,
/// never invented.
export function describeCodec(name, { srate, ch } = {}) {
  const known = KNOWN_CODECS[String(name || "").toLowerCase()];
  if (known) {
    return {
      name: known.name,
      chip: known.chipKey ? t(known.chipKey) : null,
      desc: t(known.descKey),
      whisper: known.whisper(),
      meters: { quality: known.quality, data: known.data },
    };
  }
  const parts = [String(name || "").toUpperCase()];
  if (srate) {
    const khz = srate % 1000 === 0 ? String(srate / 1000) : (srate / 1000).toFixed(1);
    parts.push(`${khz} KHZ`);
  }
  if (ch) parts.push(`${ch}CH`);
  return {
    name: String(name || ""),
    chip: null,
    desc: t("settings.codecUnknownDesc"),
    whisper: parts.join(" · "),
    meters: null, // no known quality/data profile for this name — omit, don't invent
  };
}

/// Builds this panel's editable state straight from the engine's own
/// `codecs` event: `available` is `[{name,srate,ch}]`, `active` is
/// `[name,...]` in offer-priority order (both `core/PROTOCOL.md` v1.6).
/// `order` = the ON codecs, same names/order as `active`, filtered to
/// names this build's `available` actually lists (defensive — a name in
/// `active` that vanished from `available` between builds shouldn't render
/// a phantom row); `off` = every `available` name NOT in `active`, in the
/// engine's own `available` order.
export function buildCodecState(available, active) {
  const availableNames = (available || []).map((c) => c.name);
  const activeSet = new Set((active || []).map(String));
  const order = (active || []).map(String).filter((n) => availableNames.includes(n));
  const off = availableNames.filter((n) => !activeSet.has(n));
  return { order, off };
}

export function isDirty(state, saved) {
  return JSON.stringify(state) !== JSON.stringify(saved);
}

/// Turns one codec on/off. Refuses (`{ guarded: true }`, state unchanged)
/// when asked to turn off the LAST remaining on codec — mirrors the
/// mockup's "the toggle doesn't yield" guard: `core/PROTOCOL.md`'s
/// `set_codecs` structurally rejects an empty list anyway, but this stops
/// the UI from ever attempting it, so the calm guard message replaces an
/// error-after-the-fact, never joins it.
export function applyToggle(state, id) {
  const isOn = state.order.includes(id);
  if (isOn) {
    if (state.order.length === 1) {
      return { state, guarded: true };
    }
    return {
      state: { order: state.order.filter((x) => x !== id), off: [id, ...state.off] },
      touchedId: id,
    };
  }
  return {
    state: { order: [...state.order, id], off: state.off.filter((x) => x !== id) },
    touchedId: id,
  };
}

/// Moves an ON codec one slot up/down within `order`. Returns `null`
/// (no-op) for an OFF codec or a move past either end — mirrors the
/// mockup's disabled move buttons at the first/last position.
export function applyMove(state, id, direction) {
  const i = state.order.indexOf(id);
  if (i < 0) return null;
  const j = direction === "up" ? i - 1 : i + 1;
  if (j < 0 || j >= state.order.length) return null;
  const order = [...state.order];
  [order[i], order[j]] = [order[j], order[i]];
  return { state: { order, off: state.off }, touchedId: id, newPosition: j + 1 };
}

/// The exact payload `save_codec_settings` expects
/// (`commands::SaveCodecsInput` — `{ codecs: [...] }`, snake_case field,
/// matches `#[tauri::command(rename_all = "snake_case")]`).
export function buildSaveCodecsInput(state) {
  return { codecs: [...state.order] };
}

function meterHtml(meter, kind) {
  if (!meter) return "";
  const bars = Array.from({ length: METER_MAX_BARS }, (_, i) => `<i${i < meter.bars ? ' class="f"' : ""}></i>`).join("");
  const labelKey = kind === "quality" ? "settings.codecQualityLabel" : "settings.codecDataLabel";
  return `<span class="meter"><span class="lbl">${escapeHtml(t(labelKey))}</span><span class="bars" aria-hidden="true">${bars}</span><span class="val">${escapeHtml(t(meter.labelKey))}</span></span>`;
}

const GRIP_SVG =
  '<svg width="8" height="14" viewBox="0 0 8 14" fill="currentColor" aria-hidden="true"><circle cx="2" cy="2" r="1.2"/><circle cx="6" cy="2" r="1.2"/><circle cx="2" cy="7" r="1.2"/><circle cx="6" cy="7" r="1.2"/><circle cx="2" cy="12" r="1.2"/><circle cx="6" cy="12" r="1.2"/></svg>';
const UP_SVG =
  '<svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 14.5l6-6 6 6"/></svg>';
const DOWN_SVG =
  '<svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 9.5l6 6 6-6"/></svg>';

function renderCodecRow(id, descriptor, { on, position, total, changed }) {
  const chip = descriptor.chip ? `<span class="mchip">${escapeHtml(descriptor.chip)}</span>` : "";
  const meters = descriptor.meters
    ? `<span class="meters">${meterHtml(descriptor.meters.quality, "quality")}${meterHtml(descriptor.meters.data, "data")}</span>`
    : "";
  const idAttr = escapeAttr(id);
  const posLabel = escapeAttr(t("settings.codecsPositionAria", { n: position }));
  const upLabel = escapeAttr(t("settings.codecsMoveUpAria", { name: descriptor.name }));
  const downLabel = escapeAttr(t("settings.codecsMoveDownAria", { name: descriptor.name }));
  const toggleLabel = escapeAttr(t(on ? "settings.codecsToggleOffAria" : "settings.codecsToggleOnAria", { name: descriptor.name }));
  return `<div class="codec${on ? "" : " off"}${changed ? " changed" : ""}" role="listitem" data-codec-id="${idAttr}">
    <span class="grip" aria-hidden="true">${GRIP_SVG}</span>
    <span class="num" aria-label="${posLabel}">${on ? position : ""}</span>
    <span class="cx">
      <span class="namerow"><b>${escapeHtml(descriptor.name)}</b>${chip}</span>
      <span class="desc">${escapeHtml(descriptor.desc)}</span>
      <span class="plate">${escapeHtml(descriptor.whisper)}</span>
    </span>
    ${meters}
    <span class="mv">
      <button type="button" data-mv="up" data-codec-id="${idAttr}" aria-label="${upLabel}"${!on || position === 1 ? " disabled" : ""}>${UP_SVG}</button>
      <button type="button" data-mv="down" data-codec-id="${idAttr}" aria-label="${downLabel}"${!on || position === total ? " disabled" : ""}>${DOWN_SVG}</button>
    </span>
    <button type="button" class="sw" role="switch" aria-checked="${on}" data-codec-id="${idAttr}" aria-label="${toggleLabel}"><span class="tgl" aria-hidden="true"></span></button>
  </div>`;
}

/// Renders the full `#codecs-list` inner HTML (ON rows in order, then the
/// "Not offered" divider + OFF rows) — `available`/`state`/`touched` are
/// exactly `buildCodecState`'s inputs/output plus the caller's own touched-
/// row `Set` (session-only "which rows did the operator actually move/
/// toggle" bookkeeping — a reorder shifts every position, but the story is
/// "you moved Opus", not "N rows changed", same as the mockup).
export function renderCodecsList({ available, state, touched }) {
  const byName = new Map((available || []).map((c) => [c.name, c]));
  const touchedSet = touched || new Set();
  const total = state.order.length;
  let html = state.order
    .map((id, i) => renderCodecRow(id, describeCodec(id, byName.get(id) || {}), { on: true, position: i + 1, total, changed: touchedSet.has(id) }))
    .join("");
  if (state.off.length) {
    html += `<div class="offhead">${escapeHtml(t("settings.codecsNotOffered"))}</div>`;
    html += state.off
      .map((id) => renderCodecRow(id, describeCodec(id, byName.get(id) || {}), { on: false, position: 0, total, changed: touchedSet.has(id) }))
      .join("");
  }
  return html;
}

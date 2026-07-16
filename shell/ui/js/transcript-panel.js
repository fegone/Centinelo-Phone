// Transcript panel — pure rendering module (F4 "ola 2").
//
// Renders the "four lives of a call" from
// premium/design/mockups/transcript-panel.html (plate 07) against a plain
// state object, with zero Tauri dependency — app.js owns all the
// invoke/listen wiring and hands this module a `model` object built from
// real events; ui/dev/transcript-mock.html hands it a fabricated one for
// screenshot verification. Both call the exact same `renderTranscriptBody`.
//
// Design law this module exists to enforce (premium/design/TOKENS.md
// §1.4, creative-vigilia's 2026-07-16 report): the amber never marks
// transcription. Two voices are told apart by structure — tag word
// (YOU/CALLER), rule pattern (solid/dotted), ink weight — never by hue.
// The only amber pixel this module is allowed to emit is a user-requested
// find-highlight (<mark>), which is exactly what the mockup's own
// footnote says.
//
// model shape (see buildTranscriptModel-style callers in app.js):
// {
//   phase: "live" | "writing" | "done" | "error",
//   peerName: string, peerNumber: string,
//   direction: "inbound" | "outbound",
//   startedAt: number (ms) | null, endedAt: number (ms) | null,
//   segments: [{ speaker: "agent"|"caller", t0Ms, t1Ms, text }],
//   done: null | { txtPath, jsonPath, audioKept, channelsFailed: string[] },
//   error: null | { message, retryable, localTxtPath, localJsonPath },
//   // Other calls' unresolved save failures (2026-07-16 4R re-review,
//   // M2) - never dropped just because a new call started tracking here.
//   otherPendingRetries: [{ callId, peer, lastError }],
// }
//
// i18n (F4 packaging sprint): every human-readable string this module
// emits comes from `t()` (ui/js/i18n.js) - defaults to English, matching
// this module's pre-i18n literal strings exactly, so the existing
// transcript-panel.test.js pure-logic assertions (speakerLabel, tapeHtml's
// "No speech was picked up"/"Showing the most recent" substrings, ...)
// keep passing unmodified under plain `node --test` (no locale ever gets
// switched there - i18n.js defaults to "en").

import { t, localeTag } from "./i18n.js";
// escapeHtml/escapeAttr used to be defined here (and byte-identically
// copied into app.js) - extracted to dom-utils.js (2026-07-16 4R
// re-review, READABILITY R2) once i18n work made both files lean on them
// more heavily; see that module's own header for why they're DOM-free
// pure functions (real `node:test` coverage, no jsdom dependency).
import { escapeHtml, escapeAttr } from "./dom-utils.js";

const SPEAKER_TAG_KEY = { agent: "panel.you", caller: "panel.caller" };

// Bounds how many turns the LIVE tape actually renders (2026-07-16 4R
// re-review, M4): every new segment used to trigger a full re-render of
// the ENTIRE tape so far, which is O(n) work n times over a call = O(n^2)
// total - visibly janky on a long call center call with hundreds of
// segments. Capping the live view to the most recent N turns bounds each
// render to O(N), independent of how long the call has run; the `done`
// phase always renders every segment (that render only ever happens
// once, and is the authoritative saved transcript).
const LIVE_TAPE_MAX_TURNS = 50;

function fmtClock(ms) {
  return new Date(ms).toLocaleTimeString(localeTag(), { hour: "numeric", minute: "2-digit" });
}

function fmtDate(ms) {
  return new Date(ms).toLocaleDateString(localeTag(), { month: "short", day: "numeric" });
}

function fmtDuration(totalSeconds) {
  totalSeconds = Math.max(0, Math.floor(totalSeconds));
  const m = Math.floor(totalSeconds / 60);
  const s = totalSeconds % 60;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

function fmtTurnClock(t0Ms) {
  const totalSeconds = Math.max(0, Math.floor(t0Ms / 1000));
  return fmtDuration(totalSeconds);
}

function initials(text) {
  const clean = (text || "").replace(/[^a-zA-Z0-9]/g, "");
  return (clean.slice(0, 2) || "--").toUpperCase();
}

/// Chronological interleave — segments arrive per-channel (rx/tx are two
/// independent streams by construction, `core/PROTOCOL.md` v1.2), so
/// arrival order is not reliably conversation order. Sorted here, once,
/// at render time (stable sort — ties keep arrival order) rather than
/// trusted from the wire.
function sortedSegments(segments) {
  return [...(segments || [])].sort((a, b) => (a.t0Ms ?? 0) - (b.t0Ms ?? 0));
}

function speakerLabel(speaker) {
  const key = SPEAKER_TAG_KEY[speaker];
  return (key ? t(key) : "") || (speaker || "").toUpperCase() || "—";
}

/// Highlights `query` (case-insensitive) inside already-escaped `html`
/// text using the one amber pixel this module allows — a highlight the
/// user asked for, not a state (TOKENS §1.4 / mockup footnote).
function highlightQuery(escapedText, query) {
  if (!query) return escapedText;
  const q = query.trim();
  if (!q) return escapedText;
  const escapedQuery = q.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return escapedText.replace(new RegExp(`(${escapedQuery})`, "ig"), "<mark>$1</mark>");
}

function turnHtml(seg, query) {
  const isYou = seg.speaker === "agent";
  const body = highlightQuery(escapeHtml(seg.text || ""), query);
  return `<div class="turn${isYou ? " you" : ""}">
    <span class="tag">${speakerLabel(seg.speaker)} · ${fmtTurnClock(seg.t0Ms || 0)}</span>
    <p>${body}</p>
  </div>`;
}

function tapeHtml(model, opts = {}) {
  const { query = "", trailingListening = false, capLive = false } = opts;
  let segs = sortedSegments(model.segments);
  let truncated = false;
  if (capLive && segs.length > LIVE_TAPE_MAX_TURNS) {
    segs = segs.slice(segs.length - LIVE_TAPE_MAX_TURNS);
    truncated = true;
  }
  const parts = [];
  if (truncated) {
    parts.push(`<p class="tr-truncated-note">${escapeHtml(t("panel.truncatedNote", { n: LIVE_TAPE_MAX_TURNS }))}</p>`);
  } else if (model.startedAt) {
    parts.push(`<div class="tmark"><span>${escapeHtml(t("panel.callBegan", { time: fmtClock(model.startedAt) }))}</span></div>`);
  }
  for (const seg of segs) parts.push(turnHtml(seg, query));
  if (trailingListening) {
    parts.push(
      `<div class="listening" aria-label="${escapeAttr(t("panel.listeningAria"))}"><span class="dots" aria-hidden="true"><i></i><i></i><i></i></span><span>${escapeHtml(t("panel.listening"))}</span></div>`
    );
  }
  if (model.phase === "done" && model.endedAt) {
    parts.push(`<div class="tmark"><span>${escapeHtml(t("panel.callEnded", { time: fmtClock(model.endedAt) }))}</span></div>`);
  }
  if (!segs.length && !trailingListening) {
    parts.push(`<p class="tr-empty">${escapeHtml(t("panel.noSpeechYet"))}</p>`);
  }
  return parts.join("\n");
}

// ---------------------------------------------------------------------
// icons (paths ported verbatim from transcript-panel.html)
// ---------------------------------------------------------------------
const ICON = {
  copy: `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M15 5H7a2 2 0 0 0-2 2v10"/></svg>`,
  folder: `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z"/></svg>`,
  folderDown: `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z"/><path d="M4.5 20L20 4.5"/></svg>`,
  // A crossed-out mic, not the folder-down glyph above - channelsFailed is
  // an AUDIO read problem (one side's tap couldn't be read), not a
  // storage/folder one; folderDown is reserved for the actual save-failed
  // card (renderError/renderPendingRetriesOnly). "One metaphor per icon"
  // (creative-vigilia's 2026-07-16 panel-fidelity report, finding #3).
  micOff: `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V5a3 3 0 0 0-5.94-.6"/><path d="M17 16.95A7 7 0 0 1 5 12v-2"/><path d="M19 10v2a7 7 0 0 1-.11 1.23"/><line x1="12" y1="19" x2="12" y2="23"/><line x1="8" y1="23" x2="16" y2="23"/><line x1="1" y1="1" x2="23" y2="23"/></svg>`,
  search: `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="11" cy="11" r="6.5"/><path d="M16 16l4.5 4.5"/></svg>`,
  play: `<svg width="11" height="11" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M8 5.5v13l11-6.5-11-6.5z"/></svg>`,
  shield: `<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3l7 3v5c0 4.5-3 8-7 10-4-2-7-5.5-7-10V6l7-3z"/><path d="M9 11.5l2.2 2.2 4.3-4.2"/></svg>`,
  scribe: `<svg class="scribe" viewBox="0 0 44 44" fill="none" aria-hidden="true"><g class="ring"><path d="M35.6 12.5A16.5 16.5 0 1 0 38.5 22" stroke="var(--text-2)" stroke-width="3" stroke-linecap="round"/></g><circle cx="22" cy="22" r="4.5" fill="var(--st-off)"/></svg>`,
};

// ---------------------------------------------------------------------
// phase renderers
// ---------------------------------------------------------------------

function renderLive(model) {
  return `
    <div class="tr-live">
      <div class="tr-toprow"><span class="workchip"><i aria-hidden="true"></i>${escapeHtml(t("panel.liveBadge"))}</span></div>
      <div class="tape" id="tr-tape">${tapeHtml(model, { trailingListening: true, capLive: true })}</div>
      <p class="trail">${escapeHtml(t("panel.trailNote"))}</p>
    </div>
    ${pendingRetryRowsHtml(model.otherPendingRetries)}`;
}

function renderWriting(model) {
  return `
    <div class="writing">
      ${ICON.scribe}
      <b>${escapeHtml(t("panel.writingHeading"))}</b>
      <span class="sub">${escapeHtml(t("panel.writingBody"))}</span>
    </div>
    ${pendingRetryRowsHtml(model.otherPendingRetries)}`;
}

function factsHtml(model) {
  const chips = [];
  chips.push(`<span class="mchip">${escapeHtml(model.direction === "outbound" ? t("panel.outgoingCall") : t("panel.incomingCall"))}</span>`);
  if (model.startedAt) chips.push(`<span class="mchip">${fmtDate(model.startedAt)} · ${fmtClock(model.startedAt)}</span>`);
  if (model.startedAt && model.endedAt) {
    chips.push(`<span class="mchip">${escapeHtml(t("panel.lasted", { duration: fmtDuration((model.endedAt - model.startedAt) / 1000) }))}</span>`);
  }
  if (model.phase === "done" && model.done) {
    chips.push(`<span class="mchip ok">${escapeHtml(t("panel.savedChip"))}</span>`);
  }
  return chips.join("");
}

/// Three whole-sentence variants (not a "{who} audio" template built by
/// joining possessive words) - "your"/"the caller's"/"both sides'" don't
/// share one grammatical slot cleanly across English/Portuguese/Spanish
/// (possessive placement, singular/plural agreement on "couldn't be
/// read"), so each language gets to phrase its own sentence naturally
/// instead of fighting a shared template (see i18n.js panel.channelsFailed*).
function channelsFailedNotice(channelsFailed) {
  if (!channelsFailed || !channelsFailed.length) return "";
  const hasAgent = channelsFailed.includes("agent");
  const hasCaller = channelsFailed.includes("caller");
  const key = hasAgent && hasCaller ? "panel.channelsFailedBoth" : hasAgent ? "panel.channelsFailedYouOnly" : "panel.channelsFailedCallerOnly";
  return `<div class="tr-partial" role="status">
    <span>${ICON.micOff}</span>
    <p>${escapeHtml(t(key))}</p>
  </div>`;
}

/// Rows for calls OTHER than the one currently displayed whose save is
/// still pending (2026-07-16 4R re-review, M2) - shown regardless of
/// `model.phase`, since a save failure on a previous call can be pending
/// while a brand new call is already `live`. `retries` entries only need
/// `callId`/`peer`/`lastError` here - the fuller shape
/// (`local*Path`/`channelsFailed`) is for the currently-displayed one via
/// `model.error`, not this list.
///
/// The human paragraph is always the same calm, fixed copy - never the
/// raw backend error (creative-vigilia's 2026-07-16 panel-fidelity
/// report, finding #2: a string like "could not write to storage_dir: No
/// such file or directory" read as prose violates the panel's "cero
/// jerga" rule). The technical detail, when present, still surfaces - as
/// a mono whisper line, not prose. `.plate`'s own uppercase+heavy
/// letter-spacing treatment reads fine for short state words but would
/// hurt legibility on an arbitrary sentence-length backend message, so
/// this reuses its font/size/color without those two properties (see
/// transcript.css's own `.tr-pending-detail`).
function pendingRetryRowsHtml(retries) {
  const items = retries || [];
  if (!items.length) return "";
  const rows = items
    .map(
      (r) => `<div class="tr-pending-row">
        <div class="bx">
          <b>${escapeHtml(r.peer || t("panel.unknownCaller"))}</b>
          <p>${escapeHtml(t("panel.savedErrorFallback"))}</p>
          ${r.lastError ? `<p class="tr-pending-detail">${escapeHtml(r.lastError)}</p>` : ""}
        </div>
        <button class="ghostbtn tr-pending-retry-btn" data-call-id="${escapeAttr(r.callId)}">${escapeHtml(t("panel.retryNow"))}</button>
      </div>`
    )
    .join("");
  return `<div class="tr-pending-list" aria-label="${escapeAttr(t("panel.otherPendingTitle"))}">
    <p class="tr-pending-title">${escapeHtml(t("panel.otherPendingTitle"))}</p>
    ${rows}
  </div>`;
}

function renderDone(model, opts) {
  const query = (opts && opts.query) || "";
  const audioKept = !!(model.done && model.done.audioKept);
  return `
    <div class="callhead">
      <div class="idrow">
        <span class="medal">${initials(model.peerName || model.peerNumber)}</span>
        <div class="who">
          <b>${escapeHtml(model.peerName || model.peerNumber || t("panel.unknownCaller"))}</b>
          ${model.peerNumber ? `<span class="num">${escapeHtml(model.peerNumber)}</span>` : ""}
        </div>
        <div class="acts">
          <button class="ghostbtn" id="tr-btn-copy">${ICON.copy}${escapeHtml(t("panel.copy"))}</button>
          <button class="ghostbtn" id="tr-btn-folder"${model.done && model.done.txtPath ? "" : " disabled"}>${ICON.folder}${escapeHtml(t("panel.showInFolder"))}</button>
        </div>
      </div>
      <div class="plates" aria-label="Call facts">${factsHtml(model)}</div>
    </div>
    ${channelsFailedNotice(model.done && model.done.channelsFailed)}
    <div class="findrow">
      <label class="find">
        ${ICON.search}
        <input type="text" id="tr-find-input" placeholder="${escapeAttr(t("panel.findPlaceholder"))}" aria-label="${escapeAttr(t("panel.findAria"))}">
      </label>
      <span class="hits" id="tr-find-hits"></span>
    </div>
    <div class="tape" id="tr-tape">${tapeHtml(model, { query })}</div>
    ${
      audioKept
        ? `<div class="audiorow" aria-label="Kept audio">
             <button class="playbtn" aria-label="${escapeAttr(t("panel.playAria"))}">${ICON.play}</button>
             <span class="wave" aria-hidden="true"></span>
             <span class="aud">${escapeHtml(t("panel.audioKept"))}</span>
           </div>`
        : ""
    }
    <div class="tfoot">${ICON.shield}<span class="tfoot-text"><span class="tfoot-full">${escapeHtml(t("panel.trustLine"))}</span><span class="tfoot-short">${escapeHtml(t("panel.trustLineShort"))}</span></span></div>
    ${pendingRetryRowsHtml(model.otherPendingRetries)}`;
}

function renderError(model) {
  const err = model.error || {};
  const hasLocalCopy = !!(err.localTxtPath || err.localJsonPath);
  return `
    <div class="tr-foldercard">
      <div class="tr-fc-body">
        <span>${ICON.folderDown}</span>
        <div class="bx">
          <b>${escapeHtml(t("panel.cantSaveNow"))}</b>
          <p>${escapeHtml(t("panel.safeOnComputer"))}</p>
        </div>
      </div>
      <div class="bacts">
        <button class="ghostbtn" id="tr-btn-retry"${err.retryable === false ? " disabled" : ""}>${escapeHtml(t("panel.retryNow"))}</button>
        <button class="ghostbtn" id="tr-btn-local"${hasLocalCopy ? "" : " disabled"}>${escapeHtml(t("panel.showLocalCopy"))}</button>
      </div>
    </div>
    ${channelsFailedNotice(err.channelsFailed)}
    <div class="tape" id="tr-tape">${tapeHtml(model)}</div>
    ${pendingRetryRowsHtml(model.otherPendingRetries)}`;
}

/// Renders `model` into `container` (the `.transcript-body` element),
/// re-wiring the phase-specific handlers passed in `handlers` (all
/// optional — the mock harness typically passes none):
/// { onCopy(text), onShowFolder(path), onRetry(callId),
///   onShowLocal(path), onFindInput(query), onRetryOther(callId) }
export function renderTranscriptBody(container, model, handlers = {}) {
  if (!container || !model) return;
  const query = container.dataset.trQuery || "";
  let html;
  if (model.phase === "live") html = renderLive(model);
  else if (model.phase === "writing") html = renderWriting(model);
  else if (model.phase === "error") html = renderError(model);
  else html = renderDone(model, { query });

  container.innerHTML = html;
  container.classList.toggle("tr-phase-writing", model.phase === "writing");

  const $ = (id) => container.querySelector(`#${id}`);

  const copyBtn = $("tr-btn-copy");
  if (copyBtn && handlers.onCopy) {
    copyBtn.addEventListener("click", () => handlers.onCopy(plainTextTranscript(model)));
  }
  const folderBtn = $("tr-btn-folder");
  if (folderBtn && handlers.onShowFolder && model.done && model.done.txtPath) {
    folderBtn.addEventListener("click", () => handlers.onShowFolder(model.done.txtPath));
  }
  const retryBtn = $("tr-btn-retry");
  if (retryBtn && handlers.onRetry) {
    retryBtn.addEventListener("click", () => handlers.onRetry());
  }
  const localBtn = $("tr-btn-local");
  if (localBtn && handlers.onShowLocal && model.error) {
    const path = model.error.localTxtPath || model.error.localJsonPath;
    if (path) localBtn.addEventListener("click", () => handlers.onShowLocal(path));
  }
  container.querySelectorAll(".tr-pending-retry-btn").forEach((btn) => {
    if (handlers.onRetryOther) {
      btn.addEventListener("click", () => handlers.onRetryOther(btn.dataset.callId));
    }
  });
  const findInput = $("tr-find-input");
  if (findInput) {
    // Set the DOM property directly rather than interpolating `query`
    // into the markup as a `value="..."` attribute (2026-07-16 4R
    // re-review, M1 - RISK): a query containing `"` would otherwise
    // break out of the attribute on the very next re-render (any
    // segment arriving mid-typing rebuilds this container's innerHTML)
    // and inject arbitrary markup/event handlers. This is airtight
    // against that class of bug entirely, not just escaped harder - the
    // browser's own attribute parser never sees `query` as markup text
    // at all.
    findInput.value = query;
    findInput.addEventListener("input", () => {
      container.dataset.trQuery = findInput.value;
      // Re-render the tape FIRST so the <mark> count below reflects the
      // query that was just typed, not the previous one - without losing
      // focus on the input itself (only the tape's innerHTML is touched).
      const tape = $("tr-tape");
      if (tape) tape.innerHTML = tapeHtml(model, { query: findInput.value });
      const hits = container.querySelectorAll("mark").length;
      const hitsEl = $("tr-find-hits");
      // "N matches", not "N OF N" - there's no current-match cursor to
      // navigate between (no next/prev affordance exists), so a fraction
      // implying one would be misleading (2026-07-16 4R re-review, B1).
      if (hitsEl) hitsEl.textContent = findInput.value.trim() ? t(hits === 1 ? "panel.matchOne" : "panel.matchOther", { n: hits }) : "";
      if (handlers.onFindInput) handlers.onFindInput(findInput.value);
    });
  }
}

/// Renders just the "other calls waiting to save" list as the *primary*
/// content of `container` - used when there is no current/live call to
/// show (`state.transcript === null`) but one or more earlier calls still
/// have an unresolved save failure (2026-07-16 4R re-review, M2: this is
/// what keeps those reachable after `beginTranscript` moves on to a new
/// call, or after an app restart re-hydrates `transcription_pending_retries`
/// with nothing currently `live`).
export function renderPendingRetriesOnly(container, retries, handlers = {}) {
  if (!container) return;
  const items = retries || [];
  if (!items.length) {
    container.innerHTML = "";
    return;
  }
  container.innerHTML = `
    <div class="tr-foldercard">
      <div class="tr-fc-body">
        ${ICON.folderDown}
        <div class="bx">
          <b>${escapeHtml(t("panel.waitingToSaveTitle"))}</b>
          <p>${escapeHtml(t("panel.waitingToSaveBody"))}</p>
        </div>
      </div>
    </div>
    ${pendingRetryRowsHtml(items)}`;
  container.querySelectorAll(".tr-pending-retry-btn").forEach((btn) => {
    if (handlers.onRetryOther) {
      btn.addEventListener("click", () => handlers.onRetryOther(btn.dataset.callId));
    }
  });
}

/// Plain-text export for the "Copy" action - `HH:MM  You: ...` per line,
/// chronologically interleaved (same ordering the tape itself uses).
/// Always the FULL segment list, never the live tape's capped view
/// (`LIVE_TAPE_MAX_TURNS`) - Copy is only offered on the `done` phase,
/// where the complete transcript is already available.
export function plainTextTranscript(model) {
  return sortedSegments(model.segments)
    .map((seg) => `[${fmtTurnClock(seg.t0Ms || 0)}] ${speakerLabel(seg.speaker)}: ${seg.text || ""}`)
    .join("\n");
}

export const __testables = {
  sortedSegments,
  speakerLabel,
  fmtDuration,
  fmtTurnClock,
  highlightQuery,
  escapeHtml,
  escapeAttr,
  tapeHtml,
  LIVE_TAPE_MAX_TURNS,
};

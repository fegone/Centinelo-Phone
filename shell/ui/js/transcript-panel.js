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
// }

const SPEAKER_TAG = { agent: "You", caller: "Caller" };

function escapeHtml(s) {
  const d = document.createElement("div");
  d.textContent = s ?? "";
  return d.innerHTML;
}

function fmtClock(ms) {
  return new Date(ms).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

function fmtDate(ms) {
  return new Date(ms).toLocaleDateString([], { month: "short", day: "numeric" });
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
  return SPEAKER_TAG[speaker] || (speaker || "").toUpperCase() || "—";
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
  const { query = "", trailingListening = false } = opts;
  const segs = sortedSegments(model.segments);
  const parts = [];
  if (model.startedAt) {
    parts.push(`<div class="tmark"><span>Call began · ${fmtClock(model.startedAt)}</span></div>`);
  }
  for (const seg of segs) parts.push(turnHtml(seg, query));
  if (trailingListening) {
    parts.push(
      `<div class="listening" aria-label="Listening"><span class="dots" aria-hidden="true"><i></i><i></i><i></i></span><span>Listening</span></div>`
    );
  }
  if (model.phase === "done" && model.endedAt) {
    parts.push(`<div class="tmark"><span>Call ended · ${fmtClock(model.endedAt)}</span></div>`);
  }
  if (!segs.length && !trailingListening) {
    parts.push(`<p class="tr-empty">No speech was picked up on this call yet.</p>`);
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
      <div class="tr-toprow"><span class="workchip"><i aria-hidden="true"></i>Live</span></div>
      <div class="tape" id="tr-tape">${tapeHtml(model, { trailingListening: true })}</div>
      <p class="trail">Runs a few seconds behind the conversation — turns land whole, already attributed. No word-by-word churn.</p>
    </div>`;
}

function renderWriting() {
  return `
    <div class="writing">
      ${ICON.scribe}
      <b>Writing the transcript</b>
      <span class="sub">This can take a few minutes on this computer. You can keep taking calls.</span>
    </div>`;
}

function factsHtml(model) {
  const chips = [];
  chips.push(`<span class="mchip">${model.direction === "outbound" ? "Outgoing call" : "Incoming call"}</span>`);
  if (model.startedAt) chips.push(`<span class="mchip">${fmtDate(model.startedAt)} · ${fmtClock(model.startedAt)}</span>`);
  if (model.startedAt && model.endedAt) {
    chips.push(`<span class="mchip">Lasted ${fmtDuration((model.endedAt - model.startedAt) / 1000)}</span>`);
  }
  if (model.phase === "done" && model.done) {
    chips.push(`<span class="mchip ok">Saved</span>`);
  }
  return chips.join("");
}

function channelsFailedNotice(channelsFailed) {
  if (!channelsFailed || !channelsFailed.length) return "";
  const who = channelsFailed.map((s) => (s === "agent" ? "your" : "the caller's")).join(" and ");
  return `<div class="tr-partial" role="status">
    <span>${ICON.folderDown}</span>
    <p>Part of this call wasn't transcribed — ${escapeHtml(who)} audio couldn't be read. What follows is what was captured, not the full call.</p>
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
          <b>${escapeHtml(model.peerName || model.peerNumber || "Unknown caller")}</b>
          ${model.peerNumber ? `<span class="num">${escapeHtml(model.peerNumber)}</span>` : ""}
        </div>
        <div class="acts">
          <button class="ghostbtn" id="tr-btn-copy">${ICON.copy}Copy</button>
          <button class="ghostbtn" id="tr-btn-folder"${model.done && model.done.txtPath ? "" : " disabled"}>${ICON.folder}Show in folder</button>
        </div>
      </div>
      <div class="plates" aria-label="Call facts">${factsHtml(model)}</div>
    </div>
    ${channelsFailedNotice(model.done && model.done.channelsFailed)}
    <div class="findrow">
      <label class="find">
        ${ICON.search}
        <input type="text" id="tr-find-input" placeholder="Find in transcript" aria-label="Find in transcript" value="${escapeHtml(query)}">
      </label>
      <span class="hits" id="tr-find-hits"></span>
    </div>
    <div class="tape" id="tr-tape">${tapeHtml(model, { query })}</div>
    ${
      audioKept
        ? `<div class="audiorow" aria-label="Kept audio">
             <button class="playbtn" aria-label="Play audio">${ICON.play}</button>
             <span class="wave" aria-hidden="true"></span>
             <span class="aud">Audio kept · 2 channels</span>
           </div>`
        : ""
    }
    <div class="tfoot">${ICON.shield}Transcribed on this computer · audio never left it</div>`;
}

function renderError(model) {
  const err = model.error || {};
  const hasLocalCopy = !!(err.localTxtPath || err.localJsonPath);
  return `
    <div class="tr-foldercard">
      <div class="tr-fc-body">
        <span>${ICON.folderDown}</span>
        <div class="bx">
          <b>Can't save this transcript right now.</b>
          <p>The transcript is safe on this computer. It moves over on its own once this is fixed.</p>
        </div>
      </div>
      <div class="bacts">
        <button class="ghostbtn" id="tr-btn-retry"${err.retryable === false ? " disabled" : ""}>Retry now</button>
        <button class="ghostbtn" id="tr-btn-local"${hasLocalCopy ? "" : " disabled"}>Show local copy</button>
      </div>
    </div>
    ${channelsFailedNotice(err.channelsFailed)}
    <div class="tape" id="tr-tape">${tapeHtml(model)}</div>`;
}

/// Renders `model` into `container` (the `.transcript-body` element),
/// re-wiring the phase-specific handlers passed in `handlers` (all
/// optional — the mock harness typically passes none):
/// { onCopy(text), onShowFolder(path), onRetry(callId),
///   onShowLocal(path), onFindInput(query) }
export function renderTranscriptBody(container, model, handlers = {}) {
  if (!container || !model) return;
  const query = container.dataset.trQuery || "";
  let html;
  if (model.phase === "live") html = renderLive(model);
  else if (model.phase === "writing") html = renderWriting();
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
  const findInput = $("tr-find-input");
  if (findInput) {
    findInput.addEventListener("input", () => {
      container.dataset.trQuery = findInput.value;
      // Re-render the tape FIRST so the <mark> count below reflects the
      // query that was just typed, not the previous one - without losing
      // focus on the input itself (only the tape's innerHTML is touched).
      const tape = $("tr-tape");
      if (tape) tape.innerHTML = tapeHtml(model, { query: findInput.value });
      const hits = container.querySelectorAll("mark").length;
      const hitsEl = $("tr-find-hits");
      if (hitsEl) hitsEl.textContent = findInput.value.trim() ? `${hits} OF ${hits}` : "";
      if (handlers.onFindInput) handlers.onFindInput(findInput.value);
    });
  }
}

/// Plain-text export for the "Copy" action - `HH:MM  You: ...` per line,
/// chronologically interleaved (same ordering the tape itself uses).
export function plainTextTranscript(model) {
  return sortedSegments(model.segments)
    .map((seg) => `[${fmtTurnClock(seg.t0Ms || 0)}] ${speakerLabel(seg.speaker)}: ${seg.text || ""}`)
    .join("\n");
}

export const __testables = { sortedSegments, speakerLabel, fmtDuration, fmtTurnClock, highlightQuery };

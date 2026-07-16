// Centinelo Phone shell — frontend logic.
// No bundler: this is a plain ES module loaded directly by the webview.
// Talks to the Rust backend exclusively through Tauri commands/events
// (window.__TAURI__, injected because tauri.conf.json sets
// app.withGlobalTauri = true) — never touches the sidecar process or the
// settings file directly.

import { renderTranscriptBody } from "./transcript-panel.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow } = window.__TAURI__.window;

const win = getCurrentWindow();

// ---------------------------------------------------------------------------
// state
// ---------------------------------------------------------------------------
const state = {
  dial: "",
  account: null, // AccountSettingsView from the backend
  favorites: [],
  blf: {}, // ext (string) -> "idle"|"ringing"|"busy"|"offline", from sidecar "blf" events
  bridge: null, // BridgeSettingsView from the backend (click-to-call + deep links)
  regState: "unregistered", // unregistered|registering|registered|failed
  transport: null,
  sidecarStatus: { status: "idle" },
  call: null, // { direction, state, peer, callId, createdAt, establishedAt }
  adminConfigured: false,
  adminUnlocked: false,
  theme: "auto",
  callTimerHandle: null,
  pendingDialNumber: null, // set while #dial-confirm-overlay is showing
  // ---- transcription (F4 ola 2) ------------------------------------------
  transcription: { unlocked: false, mode: "off", activation: "all_calls" },
  // The current/last call's transcript, or null ("absent" - unlicensed, off,
  // or manual activation never started for this call). See
  // ui/js/transcript-panel.js's header comment for the full shape.
  transcript: null,
};

const $ = (id) => document.getElementById(id);

function detectOS() {
  const p = (navigator.platform || "").toLowerCase();
  const ua = navigator.userAgent || "";
  return p.includes("mac") || ua.includes("Macintosh") ? "mac" : "other";
}

// ---------------------------------------------------------------------------
// banners
// ---------------------------------------------------------------------------
let bannerTimer = null;
function showBanner(message, kind = "info", ttlMs = 4500) {
  const slot = $("banner-slot");
  slot.innerHTML = "";
  const el = document.createElement("div");
  el.className = `banner ${kind}`;
  el.setAttribute("role", "status");
  el.textContent = message;
  slot.appendChild(el);
  if (bannerTimer) clearTimeout(bannerTimer);
  if (ttlMs > 0) {
    bannerTimer = setTimeout(() => {
      if (el.parentNode) el.parentNode.removeChild(el);
    }, ttlMs);
  }
}

// ---------------------------------------------------------------------------
// formatting helpers
// ---------------------------------------------------------------------------
function fmtDuration(totalSeconds) {
  totalSeconds = Math.max(0, Math.floor(totalSeconds));
  if (totalSeconds >= 3600) {
    const h = Math.floor(totalSeconds / 3600);
    const m = Math.floor((totalSeconds % 3600) / 60);
    return `${h}H${String(m).padStart(2, "0")}`;
  }
  const m = Math.floor(totalSeconds / 60);
  const s = totalSeconds % 60;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

function fmtClock(ms) {
  return new Date(ms).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

function fmtWhen(ms) {
  const d = new Date(ms);
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return fmtClock(ms);
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (d.toDateString() === yesterday.toDateString()) return "Yesterday";
  return d.toLocaleDateString([], { month: "short", day: "numeric" });
}

function extractUser(uri) {
  if (!uri) return "";
  const m = /sip:([^@;>]+)/i.exec(uri);
  return m ? m[1] : uri;
}

function initials(text) {
  const clean = (text || "").replace(/[^a-zA-Z0-9]/g, "");
  return (clean.slice(0, 2) || "--").toUpperCase();
}

// ---------------------------------------------------------------------------
// registration pill / titlebar / watchlamp
// ---------------------------------------------------------------------------
function isHealthy() {
  return state.sidecarStatus.status === "running" && state.regState === "registered";
}

function renderWatchlamp() {
  $("watchlamp-dot").classList.toggle("unhealthy", !isHealthy());
}

function renderRegPill() {
  const pill = $("reg-pill");
  pill.classList.remove("reg-registered", "reg-registering", "reg-failed");
  const transportText = state.transport ? state.transport.toUpperCase() : "—";
  $("reg-pill-transport").textContent = transportText;
  let detail = "";
  if (state.regState === "registered") {
    pill.classList.add("reg-registered");
  } else if (state.regState === "registering") {
    pill.classList.add("reg-registering");
    detail = "CONNECTING";
  } else if (state.regState === "failed") {
    pill.classList.add("reg-failed");
    detail = "RETRYING";
  } else {
    detail = "OFFLINE";
  }
  $("reg-pill-detail").textContent = detail;
  pill.title =
    state.regState === "registered"
      ? `Registered — ${transportText} transport`
      : state.regState === "failed"
        ? "Can't reach your phone system — retrying automatically."
        : "Not registered yet.";
}

function renderTitlebarState() {
  const el = $("titlebar-state");
  const s = state.sidecarStatus;
  if (!state.account || !state.account.host) {
    el.textContent = "Not set up";
  } else if (state.call) {
    const who = extractUser(state.call.peer) || "call";
    if (state.call.state === "established") el.textContent = `On a call — ${who}`;
    else if (state.call.state === "ringing") el.textContent = `Ringing — ${who}`;
    else if (state.call.state === "incoming") el.textContent = `Incoming — ${who}`;
    else el.textContent = `Calling ${who}…`;
  } else if (s.status === "idle") {
    el.textContent = "Not set up";
  } else if (s.status === "starting") {
    el.textContent = "Starting…";
  } else if (s.status === "restarting") {
    el.textContent = `Reconnecting the phone engine… (${s.attempt}/${s.max_attempts})`;
  } else if (s.status === "stopped") {
    el.textContent = "Phone engine stopped";
  } else if (s.status === "failed") {
    el.textContent = "Phone engine crashed — see Settings";
  } else if (state.regState === "registering") {
    el.textContent = "Connecting…";
  } else if (state.regState === "failed") {
    el.textContent = "Can't reach your phone system — retrying";
  } else if (state.regState === "registered") {
    el.textContent = "Ready";
  } else {
    el.textContent = "Ready";
  }
}

function renderAll() {
  renderWatchlamp();
  renderRegPill();
  renderTitlebarState();
}

// ---------------------------------------------------------------------------
// idle / configured area
// ---------------------------------------------------------------------------
function renderIdentity() {
  const configured = !!(state.account && state.account.host && state.account.ext);
  $("setup-prompt").hidden = configured;
  $("configured-area").hidden = !configured;
  if (!configured) return;
  const name = state.account.display_name || `Extension ${state.account.ext}`;
  $("me-name").textContent = name;
  $("me-plate").textContent = `EXT ${state.account.ext}`;
  $("me-medal").textContent = initials(name);
}

function renderDial() {
  $("dial-num").textContent = state.dial;
}

// idle=soft/jade lamp, ringing=amber (ringing OWNS amber - the one-glow
// rule, see premium/design/DIRECTION.md "signature elements"), busy=lit
// coral, offline=dark/faint. CSS classes ported verbatim from
// mockups/main.html's .fav.idle|.ring|.busy|.off (see app.css) - shape
// (lamp-edge bar + pulse ring on .ring only) + color + word, never color
// alone, per the design law's "never color alone" rule.
const BLF_LABEL = { idle: "Available", ringing: "Ringing", busy: "On a call", offline: "Offline" };
const BLF_CSS_STATE = { idle: "idle", ringing: "ring", busy: "busy", offline: "off" };

function renderFavorites() {
  const grid = $("favorites-grid");
  grid.innerHTML = "";
  const slots = state.favorites.length ? state.favorites : [];
  for (const slot of slots) {
    const btn = document.createElement("button");
    const ext = (slot.ext || "").trim();
    const hasExt = !!ext;
    const blfState = hasExt ? state.blf[ext] : null;
    const cssState = hasExt ? BLF_CSS_STATE[blfState] || "off" : "off";
    const label = !hasExt ? "Empty" : blfState ? BLF_LABEL[blfState] || "Offline" : "Not tracked yet";
    btn.className = `fav ${cssState}`;
    btn.disabled = !hasExt;
    btn.innerHTML = `<b>${escapeHtml(slot.label || (hasExt ? `Ext ${ext}` : "Not set up"))}</b>
      <span class="sub"><span class="plate">${hasExt ? "EXT " + escapeHtml(ext) : "—"}</span><span class="st">${label}</span></span>`;
    if (hasExt) {
      // Favorites in a real clinic are real people - always confirm, never
      // dial straight from a click (see shell task spec).
      const name = slot.label && slot.label.trim() ? slot.label.trim() : `Ext ${ext}`;
      btn.addEventListener("click", () => confirmAndDial(ext, `Calling ${name}.`));
    }
    grid.appendChild(btn);
  }
}

function escapeHtml(s) {
  const d = document.createElement("div");
  d.textContent = s ?? "";
  return d.innerHTML;
}

// ---------------------------------------------------------------------------
// dial confirmation - shared by favorites clicks, the click-to-call bridge,
// and centinelo:// or tel: deep links (see handleClickToCall below). A busy
// line never silently gets a second dial attempt: a request that arrives
// mid-call is turned into an honest banner instead of a confirm prompt the
// engine couldn't act on anyway.
// ---------------------------------------------------------------------------
function confirmAndDial(number, sourceText) {
  if (state.call) {
    showBanner(`Can't call ${number} — you're already on a call.`, "err");
    return;
  }
  state.pendingDialNumber = number;
  $("dial-confirm-source").textContent = sourceText;
  $("dial-confirm-number").textContent = number;
  $("dial-confirm-overlay").hidden = false;
}

function closeDialConfirm() {
  state.pendingDialNumber = null;
  $("dial-confirm-overlay").hidden = true;
}

async function loadRecents() {
  try {
    const list = await invoke("get_recents");
    renderRecents(list);
  } catch (e) {
    console.error("get_recents failed", e);
  }
}

function renderRecents(list) {
  const el = $("recents-list");
  el.innerHTML = "";
  if (!list || list.length === 0) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = "Calls you make and take will show up here.";
    el.appendChild(empty);
    return;
  }
  for (const item of list) {
    const row = document.createElement("button");
    row.className = "row";
    const outbound = item.direction === "outbound";
    const missed = !!item.missed;
    const arrow = outbound
      ? `<path d="M7 17L17 7"/><path d="M9 7h8v8"/>`
      : `<path d="M17 7L7 17"/><path d="M7 9v8h8"/>`;
    const metaTop = missed ? `<span class="miss">Missed</span>` : fmtDuration(item.duration_secs);
    row.innerHTML = `
      <span class="ic ${missed ? "missed" : ""}" aria-hidden="true"><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">${arrow}</svg></span>
      <span class="who"><b class="mono">${escapeHtml(item.peer)}</b><i>${outbound ? "Outgoing" : missed ? "Missed call" : "Incoming"}</i></span>
      <span class="meta">${fmtWhen(item.started_at)}<br>${metaTop}</span>`;
    row.addEventListener("click", () => dialUri(item.peer));
    el.appendChild(row);
  }
}

// ---------------------------------------------------------------------------
// dialpad
// ---------------------------------------------------------------------------
function appendDigit(d) {
  if (state.dial.length > 40) return;
  state.dial += d;
  renderDial();
}
function backspace() {
  state.dial = state.dial.slice(0, -1);
  renderDial();
}

async function dialUri(digitsOrExt) {
  const digits = String(digitsOrExt || "").trim();
  if (!digits) return;
  if (!state.account || !state.account.host) {
    showBanner("Add your phone system in Settings first.", "err");
    return;
  }
  const uri = `sip:${digits}@${state.account.host}`;
  try {
    await invoke("sidecar_dial", { uri });
    state.dial = "";
    renderDial();
    state.call = { direction: "outbound", state: "dialing", peer: digits, createdAt: Date.now(), establishedAt: null };
    renderCallOverlay();
  } catch (e) {
    showBanner(String(e), "err");
  }
}

// ---------------------------------------------------------------------------
// call overlay
// ---------------------------------------------------------------------------
function renderCallOverlay() {
  const overlay = $("call-overlay");
  if (!state.call) {
    overlay.hidden = true;
    return;
  }
  overlay.hidden = false;
  const who = extractUser(state.call.peer) || state.call.peer;
  $("call-peer-name").textContent = who;
  $("call-peer-uri").textContent = state.call.direction === "inbound" ? "Incoming call" : "";
  $("call-via").textContent = state.call.direction === "inbound" ? "Main line" : "Calling…";
  $("call-medal").firstChild.textContent = initials(who);
  $("call-lamp").classList.toggle("live", state.call.state === "established");

  const incoming = state.call.direction === "inbound" && state.call.state === "incoming";
  $("call-actions-incoming").hidden = !incoming;
  $("btn-hangup").hidden = incoming;

  const ringing = state.call.state === "ringing" || state.call.state === "dialing";
  $("ringing-label").hidden = !ringing;
  $("ringing-label-text").textContent = state.call.state === "dialing" ? "Calling…" : "Ringing…";

  const established = state.call.state === "established";
  $("call-timer").hidden = !established;
  if (established && !state.callTimerHandle) {
    startCallTimer();
  }
  if (!established && state.callTimerHandle) {
    stopCallTimer();
  }
  renderManualTranscribeButton();
}

function startCallTimer() {
  updateCallTimer();
  state.callTimerHandle = setInterval(updateCallTimer, 1000);
}
function stopCallTimer() {
  if (state.callTimerHandle) {
    clearInterval(state.callTimerHandle);
    state.callTimerHandle = null;
  }
}
function updateCallTimer() {
  if (!state.call || !state.call.establishedAt) return;
  const secs = (Date.now() - state.call.establishedAt) / 1000;
  $("call-timer").textContent = fmtDuration(secs);
}

async function finalizeClosedCall() {
  if (!state.call) return;
  const call = state.call;
  const establishedAt = call.establishedAt;
  const durationSecs = establishedAt ? Math.round((Date.now() - establishedAt) / 1000) : 0;
  const missed = call.direction === "inbound" && !establishedAt;
  stopCallTimer();
  state.call = null;
  renderCallOverlay();
  renderAll();
  try {
    const list = await invoke("add_recent", {
      input: {
        peer: extractUser(call.peer) || call.peer,
        direction: call.direction,
        started_at: call.createdAt,
        duration_secs: durationSecs,
        missed,
      },
    });
    renderRecents(list);
  } catch (e) {
    console.error("add_recent failed", e);
  }
}

// ---------------------------------------------------------------------------
// transcription panel (F4 ola 2)
//
// "Absent, not disabled": state.transcript stays null (and every entry
// point stays hidden) unless the `transcription` premium capability is
// licensed AND a call actually has one in flight - no locked ghost, no
// upsell in the panel itself (Pro is mentioned once, in Settings - not
// built this sprint). See premium/design/mockups/transcript-panel.html
// specimen 04 and creative-vigilia's 2026-07-16 report.
// ---------------------------------------------------------------------------
const TRANSCRIPT_STATE_LABEL = { live: "Live", writing: "Writing…", done: "Saved", error: "Couldn't save" };

async function applyTranscriptionUI() {
  try {
    const status = await invoke("premium_capability_status", { capability: "transcription" });
    const unlocked = status === "available" || status === "not_implemented";
    state.transcription.unlocked = unlocked;
    if (unlocked) {
      const settings = await invoke("get_transcription_settings");
      if (settings) {
        state.transcription.mode = settings.mode;
        state.transcription.activation = settings.activation;
      }
    }
  } catch (e) {
    console.error("applyTranscriptionUI failed", e);
    state.transcription.unlocked = false;
  }
  renderTranscriptButton();
  renderManualTranscribeButton();
}

function renderTranscriptButton() {
  $("btn-transcript").hidden = !state.transcript;
}

/// Starts tracking a transcript the moment the backend is expected to
/// auto-tap (`transcription.rs`'s `should_auto_tap` - mirrors that same
/// decision client-side purely for *when to start showing UI*; the actual
/// tap/gate decision always happens backend-side regardless of what this
/// function guesses). Manual activation is handled by the call overlay's
/// own "Transcribe this call" button instead (see
/// `wireStaticHandlers`/`renderManualTranscribeButton`).
function maybeAutoStartTranscript(callId, peer, direction) {
  if (!callId || !state.transcription.unlocked || state.transcription.mode === "off") return;
  if (state.transcription.activation !== "all_calls") {
    renderManualTranscribeButton();
    return;
  }
  beginTranscript(callId, peer, direction);
}

function beginTranscript(callId, peer, direction) {
  if (state.transcript && state.transcript.callId === callId) return; // already tracking (re-entrant established)
  state.transcript = {
    callId,
    peer: peer || "",
    phase: "live",
    direction: direction || "inbound",
    startedAt: Date.now(),
    endedAt: null,
    segments: [],
    done: null,
    error: null,
  };
  renderTranscriptButton();
  renderManualTranscribeButton();
}

function maybeTranscriptCallEnded(callId) {
  if (!state.transcript || !callId || state.transcript.callId !== callId) return;
  if (state.transcript.phase !== "live") return; // already writing/done/error - a race or a repeat "closed"
  state.transcript.phase = "writing";
  state.transcript.endedAt = Date.now();
  renderTranscriptButton();
  // The "just ended - writing" moment (mockup specimen 02) is worth
  // surfacing on its own - the call overlay just disappeared, so opening
  // this doesn't hide any call controls the way it would mid-call.
  openTranscriptScreen();
}

function openTranscriptScreen() {
  if (!state.transcript) return;
  $("tr-peer-name").textContent = extractUser(state.transcript.peer) || "Transcript";
  renderTranscriptScreenBody();
  $("screen-transcript").hidden = false;
}

function closeTranscriptScreen() {
  $("screen-transcript").hidden = true;
}

function renderTranscriptScreenBody() {
  if (!state.transcript) return;
  $("tr-state-label").textContent = TRANSCRIPT_STATE_LABEL[state.transcript.phase] || "";
  renderTranscriptBody($("transcript-body"), state.transcript, {
    onCopy: async (text) => {
      try {
        await navigator.clipboard.writeText(text);
        showBanner("Transcript copied.", "info");
      } catch (e) {
        console.error("clipboard write failed", e);
      }
    },
    onShowFolder: (path) => {
      invoke("reveal_in_file_manager", { path }).catch((e) => showBanner(String(e), "err"));
    },
    onShowLocal: (path) => {
      invoke("reveal_in_file_manager", { path }).catch((e) => showBanner(String(e), "err"));
    },
    onRetry: () => {
      if (!state.transcript) return;
      invoke("transcription_retry", { call_id: state.transcript.callId }).catch((e) => showBanner(String(e), "err"));
    },
  });
}

function handleTranscriptSegment(payload) {
  if (!state.transcript || !payload || state.transcript.callId !== payload.call_id) return;
  state.transcript.segments.push({ speaker: payload.speaker, t0Ms: payload.t0_ms, t1Ms: payload.t1_ms, text: payload.text });
  if (!$("screen-transcript").hidden) renderTranscriptScreenBody();
}

function handleTranscriptDone(payload) {
  if (!state.transcript || !payload || state.transcript.callId !== payload.call_id) return;
  state.transcript.phase = "done";
  state.transcript.done = {
    txtPath: payload.txt_path,
    jsonPath: payload.json_path,
    audioKept: !!payload.audio_kept,
    channelsFailed: payload.channels_failed || [],
  };
  state.transcript.error = null;
  renderTranscriptButton();
  if (!$("screen-transcript").hidden) {
    renderTranscriptScreenBody();
  } else {
    showBanner(`Transcript ready — ${extractUser(state.transcript.peer) || "call"}.`, "info");
  }
}

function handleTranscriptError(payload) {
  if (!state.transcript || !payload || state.transcript.callId !== payload.call_id) return;
  if (!payload.retryable) {
    // Non-terminal notice (e.g. a live process died early - transcription.rs
    // will run a full post-call pass once the call actually ends). The
    // pipeline resolves itself via a later done/error; nothing to change
    // about the visible phase yet.
    showBanner(payload.message || "Transcription had a hiccup — it will keep trying.", "info");
    return;
  }
  state.transcript.phase = "error";
  state.transcript.error = {
    message: payload.message,
    retryable: true,
    localTxtPath: null,
    localJsonPath: null,
    channelsFailed: state.transcript.done ? state.transcript.done.channelsFailed : [],
  };
  renderTranscriptButton();
  if ($("screen-transcript").hidden) openTranscriptScreen();
  else renderTranscriptScreenBody();
  refreshTranscriptRetryInfo(payload.call_id);
}

/// `transcription://error` only carries `{call_id, message, retryable}` -
/// the local-copy paths (`local_txt_path`/`local_json_path`) and any
/// `channels_failed` the engine already knew about live in the backend's
/// `pending_retries` map instead (`PendingRetryView`,
/// `commands::transcription_pending_retries`). Fetched right after the
/// error event so "Show local copy" has a real path to open, without
/// bloating the event payload itself.
async function refreshTranscriptRetryInfo(callId) {
  try {
    const list = await invoke("transcription_pending_retries");
    const entry = (list || []).find((r) => r.call_id === callId);
    if (!entry || !state.transcript || state.transcript.callId !== callId) return;
    state.transcript.error = {
      message: entry.last_error,
      retryable: true,
      localTxtPath: entry.local_txt_path || null,
      localJsonPath: entry.local_json_path || null,
      channelsFailed: entry.channels_failed || [],
    };
    if (!$("screen-transcript").hidden) renderTranscriptScreenBody();
  } catch (e) {
    console.error("transcription_pending_retries failed", e);
  }
}

/// The manual-activation call-overlay button ("Transcribe this call") -
/// only meaningful mid-call, before a transcript has started for *this*
/// call. Hidden the rest of the time, including once started (there's no
/// mid-call "stop" affordance this sprint - the transcript panel itself,
/// reachable via the titlebar button once it exists, is the surface for
/// everything after that).
function renderManualTranscribeButton() {
  const btn = $("btn-transcribe-manual");
  if (!btn) return;
  const eligible =
    !!state.call &&
    state.call.state === "established" &&
    state.transcription.unlocked &&
    state.transcription.mode !== "off" &&
    state.transcription.activation === "manual" &&
    !(state.transcript && state.transcript.callId === state.call.callId);
  btn.hidden = !eligible;
}

// ---------------------------------------------------------------------------
// sidecar event/status handling
// ---------------------------------------------------------------------------
function handleSidecarStatus(payload) {
  state.sidecarStatus = payload;
  if (payload.status === "failed") {
    showBanner(payload.message || "The phone engine stopped working.", "err", 0);
  }
  if (payload.status === "idle" || payload.status === "stopped") {
    state.regState = "unregistered";
    state.transport = null;
  }
  // A fresh process (crash-restart, settings save, "Restart engine") starts
  // with no BLF subscriptions until it re-registers - clear stale lamps
  // rather than showing a state that's no longer being watched. The backend
  // re-issues blf_subscribe per favorite the moment it re-registers (see
  // sidecar.rs), so this is a brief gap, not a lasting "off" state.
  if (payload.status === "idle" || payload.status === "stopped" || payload.status === "starting") {
    if (Object.keys(state.blf).length) {
      state.blf = {};
      renderFavorites();
    }
  }
  renderAll();
}

function handleSidecarEvent(evt) {
  if (!evt || typeof evt !== "object") return;
  switch (evt.event) {
    case "ready":
      break;
    case "reg_state":
      state.regState = evt.state || "unregistered";
      state.transport = evt.transport || state.transport;
      renderAll();
      break;
    case "call_state":
      handleCallState(evt);
      break;
    case "blf":
      handleBlfEvent(evt);
      break;
    case "error":
      showBanner(evt.message || "Something went wrong.", "err");
      break;
    default:
      break;
  }
}

function handleBlfEvent(evt) {
  if (!evt.ext) return;
  state.blf[evt.ext] = evt.state || "offline";
  renderFavorites();
}

function handleCallState(evt) {
  // core/PROTOCOL.md carries both "id" (v0-compat) and "call_id" (v1+) with
  // the same value on every call_state event - captured uniformly here
  // (not just on "incoming", as before) since the transcript lifecycle
  // needs it on "established"/"closed" too.
  const callId = evt.call_id || evt.id || null;
  switch (evt.state) {
    case "incoming":
      state.call = {
        direction: "inbound",
        state: "incoming",
        peer: evt.peer || "",
        callId,
        createdAt: Date.now(),
        establishedAt: null,
      };
      break;
    case "ringing":
      if (state.call) {
        state.call.state = "ringing";
        if (evt.peer) state.call.peer = evt.peer;
        if (callId) state.call.callId = callId;
      } else {
        state.call = { direction: "outbound", state: "ringing", peer: evt.peer || "", callId, createdAt: Date.now(), establishedAt: null };
      }
      break;
    case "established":
      if (!state.call) {
        state.call = { direction: "inbound", state: "established", peer: evt.peer || "", callId, createdAt: Date.now(), establishedAt: Date.now() };
      } else {
        state.call.state = "established";
        state.call.establishedAt = Date.now();
        if (evt.peer) state.call.peer = evt.peer;
        if (callId) state.call.callId = callId;
      }
      maybeAutoStartTranscript(state.call.callId, state.call.peer, state.call.direction);
      break;
    case "closed":
      maybeTranscriptCallEnded(callId);
      finalizeClosedCall();
      renderAll();
      return;
    default:
      break;
  }
  renderCallOverlay();
  renderAll();
}

// ---------------------------------------------------------------------------
// settings screen
// ---------------------------------------------------------------------------
let selectedTransport = "auto";

function setTransportUI(t) {
  selectedTransport = t;
  document.querySelectorAll("#transport-choice .tcard").forEach((card) => {
    card.classList.toggle("sel", card.dataset.transport === t);
  });
}

function applyLockUI() {
  const overlay = $("lock-overlay");
  const unlockCard = $("lock-card-unlock");
  const firstrunCard = $("lock-card-firstrun");
  const saveBtn = $("btn-save-settings");
  if (!state.adminConfigured) {
    overlay.hidden = false;
    unlockCard.hidden = true;
    firstrunCard.hidden = false;
    $("firstrun-password").value = "";
    $("firstrun-error").textContent = "";
  } else if (!state.adminUnlocked) {
    overlay.hidden = false;
    unlockCard.hidden = false;
    firstrunCard.hidden = true;
    $("unlock-password").value = "";
    $("unlock-error").textContent = "";
  } else {
    overlay.hidden = true;
  }
  saveBtn.disabled = !state.adminUnlocked;
}

function renderFavoritesFields(favorites) {
  const container = $("favorites-fields");
  container.innerHTML = "";
  const slots = favorites && favorites.length ? favorites : [];
  slots.forEach((slot, i) => {
    const row = document.createElement("div");
    row.className = "field-row";
    row.innerHTML = `
      <div class="field">
        <label for="fav-label-${i}">Label</label>
        <input id="fav-label-${i}" type="text" placeholder="Front desk" maxlength="40" style="font-family:var(--font-ui)">
      </div>
      <div class="field">
        <label for="fav-ext-${i}">Extension</label>
        <input id="fav-ext-${i}" type="text" placeholder="Empty" maxlength="20">
      </div>`;
    container.appendChild(row);
    $(`fav-label-${i}`).value = slot.label || "";
    $(`fav-ext-${i}`).value = slot.ext || "";
  });
}

function collectFavoritesFields() {
  const out = [];
  for (let i = 0; i < 4; i++) {
    const labelEl = $(`fav-label-${i}`);
    const extEl = $(`fav-ext-${i}`);
    if (!labelEl || !extEl) continue;
    out.push({ label: labelEl.value.trim(), ext: extEl.value.trim() });
  }
  return out;
}

function setBoolRowUI(rowId, value) {
  document.querySelectorAll(`#${rowId} button`).forEach((b) => {
    b.classList.toggle("on", (b.dataset.boolChoice === "true") === value);
  });
}

function renderBridgeFields(bridge) {
  $("bridge-address").value = `127.0.0.1:${bridge.port}`;
  $("bridge-token").value = bridge.token || "";
  setBoolRowUI("auto-dial-row", !!bridge.auto_dial);
  setBoolRowUI("tel-handler-row", !!bridge.register_tel_handler);
}

async function openSettings() {
  try {
    const [account, theme, corePath, adminStatus, favorites, bridge] = await Promise.all([
      invoke("get_account_settings"),
      invoke("get_theme"),
      invoke("get_core_binary_path"),
      invoke("admin_status"),
      invoke("get_favorites"),
      invoke("get_bridge_settings"),
    ]);
    state.account = { ...state.account, ...account };
    state.adminConfigured = adminStatus.configured;
    state.adminUnlocked = adminStatus.unlocked;
    state.bridge = bridge;

    $("in-display-name").value = account.display_name || "";
    $("in-host").value = account.host || "";
    $("in-ext").value = account.ext || "";
    $("in-secret").value = "";
    $("secret-hint").textContent = account.secret_set
      ? "Currently set — leave blank to keep it unchanged."
      : "Not set yet.";
    $("in-core-path").value = corePath || "";
    setTransportUI(account.transport_priority || "auto");
    setThemeUI(theme || "auto");
    renderFavoritesFields(favorites);
    renderBridgeFields(bridge);
    $("save-status").textContent = "";
    $("save-status").className = "status";
  } catch (e) {
    console.error("openSettings load failed", e);
  }
  applyLockUI();
  $("screen-settings").hidden = false;
}

function closeSettings() {
  $("screen-settings").hidden = true;
  renderIdentity();
}

function setThemeUI(theme) {
  state.theme = theme;
  document.querySelectorAll("#theme-row button").forEach((b) => {
    b.classList.toggle("on", b.dataset.themeChoice === theme);
  });
  applyTheme(theme);
}

function applyTheme(theme) {
  const root = document.documentElement;
  if (theme === "auto") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", theme);
}

async function saveAccountSettings() {
  const statusEl = $("save-status");
  statusEl.className = "status";
  statusEl.textContent = "Saving…";
  const host = $("in-host").value.trim();
  const ext = $("in-ext").value.trim();
  const secret = $("in-secret").value;
  const displayName = $("in-display-name").value.trim();
  try {
    await invoke("save_account_settings", {
      input: {
        host,
        ext,
        secret: secret.length ? secret : null,
        display_name: displayName,
        transport_priority: selectedTransport,
      },
    });
    const corePathValue = $("in-core-path").value.trim();
    await invoke("set_core_binary_path", { path: corePathValue.length ? corePathValue : null });
    const favorites = await invoke("save_favorites", { favorites: collectFavoritesFields() });
    state.favorites = favorites;
    renderFavorites();
    statusEl.textContent = "Saved — reconnecting…";
    statusEl.className = "status ok";
    const account = await invoke("get_account_settings");
    state.account = account;
    renderIdentity();
    $("in-secret").value = "";
    $("secret-hint").textContent = account.secret_set
      ? "Currently set — leave blank to keep it unchanged."
      : "Not set yet.";
  } catch (e) {
    statusEl.textContent = String(e);
    statusEl.className = "status err";
  }
}

// ---------------------------------------------------------------------------
// wiring
// ---------------------------------------------------------------------------
function wireStaticHandlers() {
  $("btn-minimize").addEventListener("click", () => win.minimize());
  $("btn-close").addEventListener("click", () => win.hide());
  $("btn-settings").addEventListener("click", openSettings);
  $("settings-back").addEventListener("click", closeSettings);
  $("btn-cancel-settings").addEventListener("click", closeSettings);
  $("btn-console").addEventListener("click", () => {
    invoke("open_console").catch((e) => showBanner(String(e), "err"));
  });

  $("btn-transcript").addEventListener("click", openTranscriptScreen);
  $("transcript-back").addEventListener("click", closeTranscriptScreen);
  $("btn-transcribe-manual").addEventListener("click", async () => {
    if (!state.call || !state.call.callId) return;
    const btn = $("btn-transcribe-manual");
    btn.disabled = true;
    try {
      await invoke("transcription_manual_start", { call_id: state.call.callId, peer: state.call.peer || "" });
      beginTranscript(state.call.callId, state.call.peer, state.call.direction);
    } catch (e) {
      showBanner(String(e), "err");
    } finally {
      btn.disabled = false;
    }
  });

  $("setup-open-settings").addEventListener("click", openSettings);

  document.querySelectorAll("#dialpad .key").forEach((key) => {
    key.addEventListener("click", () => appendDigit(key.dataset.digit));
  });
  $("btn-backspace").addEventListener("click", backspace);
  $("btn-call").addEventListener("click", () => dialUri(state.dial));

  $("btn-hangup").addEventListener("click", () => invoke("sidecar_hangup").catch((e) => showBanner(String(e), "err")));
  $("btn-decline").addEventListener("click", () => invoke("sidecar_hangup").catch((e) => showBanner(String(e), "err")));
  $("btn-answer").addEventListener("click", () => invoke("sidecar_answer").catch((e) => showBanner(String(e), "err")));

  document.querySelectorAll("#transport-choice .tcard").forEach((card) => {
    card.addEventListener("click", () => setTransportUI(card.dataset.transport));
  });
  document.querySelectorAll("#theme-row button").forEach((b) => {
    b.addEventListener("click", async () => {
      setThemeUI(b.dataset.themeChoice);
      try {
        await invoke("set_theme", { theme: b.dataset.themeChoice });
      } catch (e) {
        console.error("set_theme failed", e);
      }
    });
  });

  $("btn-save-settings").addEventListener("click", saveAccountSettings);
  $("btn-restart-engine").addEventListener("click", () => {
    invoke("sidecar_restart");
    showBanner("Restarting the phone engine…", "info");
  });

  $("btn-unlock").addEventListener("click", async () => {
    const pw = $("unlock-password").value;
    const ok = await invoke("admin_unlock", { password: pw });
    if (ok) {
      state.adminUnlocked = true;
      applyLockUI();
    } else {
      $("unlock-error").textContent = "Incorrect password.";
    }
  });
  $("unlock-password").addEventListener("keydown", (e) => {
    if (e.key === "Enter") $("btn-unlock").click();
  });
  $("btn-cancel-unlock").addEventListener("click", closeSettings);

  $("btn-firstrun-set").addEventListener("click", async () => {
    const pw = $("firstrun-password").value;
    if (pw.length < 8) {
      $("firstrun-error").textContent = "Use at least 8 characters.";
      return;
    }
    try {
      await invoke("admin_set_password", { new_password: pw });
      state.adminConfigured = true;
      state.adminUnlocked = true;
      applyLockUI();
    } catch (e) {
      $("firstrun-error").textContent = String(e);
    }
  });

  $("btn-set-admin-password").addEventListener("click", async () => {
    const pw = $("in-admin-new").value;
    const statusEl = $("admin-password-status");
    if (pw.length < 8) {
      statusEl.textContent = "Use at least 8 characters.";
      statusEl.className = "hint err";
      return;
    }
    try {
      await invoke("admin_set_password", { new_password: pw });
      $("in-admin-new").value = "";
      statusEl.textContent = "Password updated.";
      statusEl.className = "hint ok";
    } catch (e) {
      statusEl.textContent = String(e);
      statusEl.className = "hint err";
    }
  });

  document.addEventListener("keydown", (e) => {
    if (!$("dial-confirm-overlay").hidden) return; // handled by its own listener below
    if (!$("screen-settings").hidden) return;
    if (state.call) return;
    if (/^[0-9*#]$/.test(e.key)) appendDigit(e.key);
    else if (e.key === "Backspace") backspace();
    else if (e.key === "Enter") dialUri(state.dial);
  });

  // ---- dial confirmation (favorites + click-to-call + deep links) --------
  $("btn-dial-confirm-call").addEventListener("click", () => {
    const number = state.pendingDialNumber;
    closeDialConfirm();
    if (number) dialUri(number);
  });
  $("btn-dial-confirm-cancel").addEventListener("click", closeDialConfirm);
  document.addEventListener("keydown", (e) => {
    if ($("dial-confirm-overlay").hidden) return;
    if (e.key === "Enter") $("btn-dial-confirm-call").click();
    else if (e.key === "Escape") $("btn-dial-confirm-cancel").click();
  });

  // ---- click-to-call bridge settings ---------------------------------
  $("btn-copy-token").addEventListener("click", async () => {
    const token = $("bridge-token").value;
    const statusEl = $("copy-token-status");
    try {
      await navigator.clipboard.writeText(token);
    } catch (e) {
      // Fallback if the webview didn't grant the async Clipboard API.
      const input = $("bridge-token");
      input.removeAttribute("readonly");
      input.select();
      document.execCommand("copy");
      input.setAttribute("readonly", "");
    }
    statusEl.textContent = "Copied.";
    statusEl.className = "hint ok";
    setTimeout(() => {
      statusEl.textContent = "";
    }, 2500);
  });
  document.querySelectorAll("#auto-dial-row button").forEach((b) => {
    b.addEventListener("click", async () => {
      const value = b.dataset.boolChoice === "true";
      setBoolRowUI("auto-dial-row", value);
      try {
        await invoke("set_auto_dial", { auto_dial: value });
        if (state.bridge) state.bridge.auto_dial = value;
      } catch (e) {
        showBanner(String(e), "err");
      }
    });
  });
  document.querySelectorAll("#tel-handler-row button").forEach((b) => {
    b.addEventListener("click", async () => {
      const value = b.dataset.boolChoice === "true";
      setBoolRowUI("tel-handler-row", value);
      try {
        await invoke("set_register_tel_handler", { enabled: value });
        if (state.bridge) state.bridge.register_tel_handler = value;
      } catch (e) {
        showBanner(String(e), "err");
      }
    });
  });
}

// Mirrors v1's `broadcast('dial-request', number)` origin story (the
// keypad/hotkey/protocol paths all funneled into one channel there too) -
// here it's the click-to-call bridge (bridge.rs) and centinelo:// or tel:
// deep links (deeplink.rs), unified into one "click-to-call" event so the
// UI only needs one confirmation flow (see confirmAndDial above).
const CLICK_TO_CALL_SOURCE_LABEL = {
  bridge: "your browser",
  tel: "a tel: link",
  centinelo: "a centinelo: link",
};

function handleClickToCall(payload) {
  if (!payload || !payload.number) return;
  const sourceLabel = CLICK_TO_CALL_SOURCE_LABEL[payload.source] || "outside the app";
  if (state.call) {
    showBanner(`Can't dial ${payload.number} — you're already on a call.`, "err");
    return;
  }
  if (payload.auto_dial) {
    showBanner(`Dialing ${payload.number} from ${sourceLabel}.`, "info");
    dialUri(payload.number);
    return;
  }
  confirmAndDial(payload.number, `From ${sourceLabel}.`);
}

async function attachTauriListeners() {
  await listen("sidecar-status", (e) => handleSidecarStatus(e.payload));
  await listen("sidecar-event", (e) => handleSidecarEvent(e.payload));
  await listen("click-to-call", (e) => handleClickToCall(e.payload));
  await listen("transcription://segment", (e) => handleTranscriptSegment(e.payload));
  await listen("transcription://done", (e) => handleTranscriptDone(e.payload));
  await listen("transcription://error", (e) => handleTranscriptError(e.payload));
}

// Premium receptionist console entry point - hidden unless the license
// gate cleared. "Available" or "not_implemented" both mean "offer it"
// (see console.rs::unlocks_console's doc for why NotImplemented, v0's
// actual answer for a licensed capability, counts here); "not_licensed"/
// "unavailable" (no dylib, tampered signature, FFI error) hide it -
// mirrors tray.rs's own gating exactly, so both surfaces agree.
async function applyPremiumUI() {
  try {
    const status = await invoke("premium_capability_status", { capability: "blf_console" });
    const unlocked = status === "available" || status === "not_implemented";
    $("btn-console").hidden = !unlocked;
  } catch (e) {
    console.error("premium_capability_status failed", e);
    $("btn-console").hidden = true;
  }
}

async function boot() {
  document.documentElement.dataset.os = detectOS();
  wireStaticHandlers();

  try {
    const [account, favorites, theme, sidecarStatus, blfStates] = await Promise.all([
      invoke("get_account_settings"),
      invoke("get_favorites"),
      invoke("get_theme"),
      invoke("sidecar_status"),
      invoke("get_blf_states"),
    ]);
    state.account = account;
    state.favorites = favorites;
    state.sidecarStatus = sidecarStatus;
    state.blf = blfStates || {};
    applyTheme(theme || "auto");
  } catch (e) {
    console.error("boot load failed", e);
  }

  renderIdentity();
  renderDial();
  renderFavorites();
  renderAll();
  await loadRecents();
  await attachTauriListeners();
  await applyPremiumUI();
  await applyTranscriptionUI();
}

boot();

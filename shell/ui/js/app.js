// Centinelo Phone shell — frontend logic.
// No bundler: this is a plain ES module loaded directly by the webview.
// Talks to the Rust backend exclusively through Tauri commands/events
// (window.__TAURI__, injected because tauri.conf.json sets
// app.withGlobalTauri = true) — never touches the sidecar process or the
// settings file directly.

import { renderTranscriptBody, renderPendingRetriesOnly } from "./transcript-panel.js";
import { t, setLocale, localeTag, applyStaticI18n } from "./i18n.js";

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
  localePref: "auto", // "auto" | "en" | "pt-BR" | "es" - see i18n.js setLocale
  callTimerHandle: null,
  pendingDialNumber: null, // set while #dial-confirm-overlay is showing
  // ---- transcription (F4 ola 2) ------------------------------------------
  transcription: { unlocked: false, mode: "off", activation: "all_calls" },
  // The current/last call's transcript, or null ("absent" - unlicensed, off,
  // or manual activation never started for this call). See
  // ui/js/transcript-panel.js's header comment for the full shape.
  transcript: null,
  // Every call (any call, not just state.transcript's own) whose save is
  // still pending backend-side - independent of which one is "current"
  // client-side, so switching calls never loses visibility into an
  // earlier one's unresolved failure (2026-07-16 4R re-review, M2).
  // [{callId, peer, startedAt, lastError, channelsFailed, localTxtPath, localJsonPath}]
  pendingRetries: [],
  // Last list rendered into #recents-list - cached purely so a live
  // language switch (see refreshAllUiText) can re-render it in the new
  // locale's date/duration formatting without an extra round-trip.
  recents: [],
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
  return new Date(ms).toLocaleTimeString(localeTag(), { hour: "numeric", minute: "2-digit" });
}

function fmtWhen(ms) {
  const d = new Date(ms);
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return fmtClock(ms);
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (d.toDateString() === yesterday.toDateString()) return t("main.yesterday");
  return d.toLocaleDateString(localeTag(), { month: "short", day: "numeric" });
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
    detail = t("regPill.connecting");
  } else if (state.regState === "failed") {
    pill.classList.add("reg-failed");
    detail = t("regPill.retrying");
  } else {
    detail = t("regPill.offline");
  }
  $("reg-pill-detail").textContent = detail;
  pill.title =
    state.regState === "registered"
      ? t("regPill.registeredTitle", { transport: transportText })
      : state.regState === "failed"
        ? t("regPill.failedTitle")
        : t("regPill.notRegisteredTitle");
}

function renderTitlebarState() {
  const el = $("titlebar-state");
  const s = state.sidecarStatus;
  if (!state.account || !state.account.host) {
    el.textContent = t("titlebarState.notSetUp");
  } else if (state.call) {
    const who = extractUser(state.call.peer) || t("transcript.callWord");
    if (state.call.state === "established") el.textContent = t("titlebarState.onCallWith", { who });
    else if (state.call.state === "ringing") el.textContent = t("titlebarState.ringingWith", { who });
    else if (state.call.state === "incoming") el.textContent = t("titlebarState.incomingWith", { who });
    else el.textContent = t("titlebarState.callingWith", { who });
  } else if (s.status === "idle") {
    el.textContent = t("titlebarState.notSetUp");
  } else if (s.status === "starting") {
    el.textContent = t("titlebarState.starting");
  } else if (s.status === "restarting") {
    el.textContent = t("titlebarState.reconnecting", { attempt: s.attempt, max: s.max_attempts });
  } else if (s.status === "stopped") {
    el.textContent = t("titlebarState.stopped");
  } else if (s.status === "failed") {
    el.textContent = t("titlebarState.crashed");
  } else if (state.regState === "registering") {
    el.textContent = t("titlebarState.connecting");
  } else if (state.regState === "failed") {
    el.textContent = t("titlebarState.cantReachRetrying");
  } else if (state.regState === "registered") {
    el.textContent = t("titlebarState.ready");
  } else {
    el.textContent = t("titlebarState.ready");
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
  const el = $("dial-num");
  el.textContent = state.dial;
  // The empty-state placeholder is a CSS `content: attr(...)` pseudo-
  // element (app.css `.display .num:empty::before`), not text this
  // element ever actually contains - keeping it in sync with the active
  // locale here too (cheap: renderDial() already runs on every digit
  // press and on refreshAllUiText's language-switch pass).
  el.setAttribute("data-empty-placeholder", t("main.dialPlaceholder"));
}

// idle=soft/jade lamp, ringing=amber (ringing OWNS amber - the one-glow
// rule, see premium/design/DIRECTION.md "signature elements"), busy=lit
// coral, offline=dark/faint. CSS classes ported verbatim from
// mockups/main.html's .fav.idle|.ring|.busy|.off (see app.css) - shape
// (lamp-edge bar + pulse ring on .ring only) + color + word, never color
// alone, per the design law's "never color alone" rule.
const BLF_LABEL_KEY = { idle: "favorites.available", ringing: "favorites.ringing", busy: "favorites.onCall", offline: "favorites.offline" };
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
    const label = !hasExt ? t("favorites.empty") : blfState ? t(BLF_LABEL_KEY[blfState] || "favorites.offline") : t("favorites.notTrackedYet");
    const extFallback = t("favorites.extFallback", { ext });
    btn.className = `fav ${cssState}`;
    btn.disabled = !hasExt;
    btn.innerHTML = `<b>${escapeHtml(slot.label || (hasExt ? extFallback : t("favorites.notSetUp")))}</b>
      <span class="sub"><span class="plate">${hasExt ? "EXT " + escapeHtml(ext) : "—"}</span><span class="st">${label}</span></span>`;
    if (hasExt) {
      // Favorites in a real clinic are real people - always confirm, never
      // dial straight from a click (see shell task spec).
      const name = slot.label && slot.label.trim() ? slot.label.trim() : extFallback;
      btn.addEventListener("click", () => confirmAndDial(ext, t("favorites.callingName", { name })));
    }
    grid.appendChild(btn);
  }
}

function escapeHtml(s) {
  const d = document.createElement("div");
  d.textContent = s ?? "";
  return d.innerHTML;
}

/// `escapeHtml` covers text-node content only - not safe to interpolate
/// into an HTML attribute value (a translated string containing `"` would
/// break out of a `placeholder="..."` attribute otherwise). Mirrors
/// transcript-panel.js's own escapeAttr for the same reason (2026-07-16
/// 4R re-review, M1 there) - the only two places in this codebase that
/// build HTML attributes from translated/dynamic strings.
function escapeAttr(s) {
  return escapeHtml(s).replace(/"/g, "&quot;").replace(/'/g, "&#39;");
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
    showBanner(t("call.cantCallBusy", { number }), "err");
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

// ---------------------------------------------------------------------------
// auto-provisioning (spec §5) - see provisioning.rs for the Rust half.
// Both the manual paste (#prov-connect) and a centinelo://provision deep
// link (the "provisioning://preview" event, attached below) end up calling
// showProvisioningConfirm with the same secret-free preview shape
// (provisioning::ProvisioningPreviewView).
// ---------------------------------------------------------------------------
const PROV_TRANSPORT_LABEL_KEY = {
  auto: "provisioning.transportAuto",
  wss: "provisioning.transportWss",
  classic: "provisioning.transportClassic",
};

async function provisioningResolveFromInput() {
  const input = $("prov-input").value.trim();
  $("prov-error").hidden = true;
  if (!input) return;
  const btn = $("prov-connect");
  btn.disabled = true;
  try {
    const preview = await invoke("provisioning_resolve", { input });
    showProvisioningConfirm(preview);
  } catch (e) {
    $("prov-error").textContent = String(e);
    $("prov-error").hidden = false;
  } finally {
    btn.disabled = false;
  }
}

function showProvisioningConfirm(preview) {
  if (!preview) return;
  $("prov-confirm-host").textContent = preview.host;
  const extLabel = preview.display_name
    ? t("provisioning.extensionNamed", { ext: preview.ext, name: preview.display_name })
    : t("provisioning.extensionOnly", { ext: preview.ext });
  $("prov-confirm-ext").textContent = extLabel;
  const transportLabel = PROV_TRANSPORT_LABEL_KEY[preview.transport_priority] ? t(PROV_TRANSPORT_LABEL_KEY[preview.transport_priority]) : preview.transport_priority;
  $("prov-confirm-transport").textContent = preview.has_tls_pin ? t("provisioning.tlsPinIncluded", { transport: transportLabel }) : transportLabel;
  $("prov-confirm-error").hidden = true;
  $("provision-confirm-overlay").hidden = false;
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
  state.recents = list || [];
  const el = $("recents-list");
  el.innerHTML = "";
  if (!list || list.length === 0) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = t("main.recentsEmpty");
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
    const metaTop = missed ? `<span class="miss">${escapeHtml(t("main.callMissed"))}</span>` : fmtDuration(item.duration_secs);
    const directionLabel = outbound ? t("main.callOutgoing") : missed ? t("main.callMissedCall") : t("main.callIncoming");
    row.innerHTML = `
      <span class="ic ${missed ? "missed" : ""}" aria-hidden="true"><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">${arrow}</svg></span>
      <span class="who"><b class="mono">${escapeHtml(item.peer)}</b><i>${escapeHtml(directionLabel)}</i></span>
      <span class="meta">${escapeHtml(fmtWhen(item.started_at))}<br>${metaTop}</span>`;
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
    showBanner(t("call.addAccountFirst"), "err");
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
  $("call-peer-uri").textContent = state.call.direction === "inbound" ? t("call.incomingCallLabel") : "";
  $("call-via").textContent = state.call.direction === "inbound" ? t("call.mainLine") : t("call.callingEllipsis");
  $("call-medal").firstChild.textContent = initials(who);
  $("call-lamp").classList.toggle("live", state.call.state === "established");

  const incoming = state.call.direction === "inbound" && state.call.state === "incoming";
  $("call-actions-incoming").hidden = !incoming;
  $("btn-hangup").hidden = incoming;

  const ringing = state.call.state === "ringing" || state.call.state === "dialing";
  $("ringing-label").hidden = !ringing;
  $("ringing-label-text").textContent = state.call.state === "dialing" ? t("call.callingEllipsis") : t("call.ringingEllipsis");

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
const TRANSCRIPT_STATE_LABEL_KEY = { live: "transcript.live", writing: "transcript.writing", done: "transcript.saved", error: "transcript.couldntSave" };
function transcriptStateLabel(phase) {
  const key = TRANSCRIPT_STATE_LABEL_KEY[phase];
  return key ? t(key) : "";
}

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

/// Reveals a transcript-owned path (a finished save, or the local copy
/// kept during a pending retry) in the OS file manager - the one place
/// `reveal_in_file_manager` is ever invoked from, shared by "Show in
/// folder" (done phase) and "Show local copy" (error phase), which used
/// to be two byte-identical closures (2026-07-16 4R re-review, B2).
function revealInFileManager(path) {
  invoke("reveal_in_file_manager", { path }).catch((e) => showBanner(String(e), "err"));
}

/// Re-runs a save for `callId` and refreshes the pending-retries list
/// afterward either way (success removes it backend-side; failure
/// re-stashes it with a - possibly different - reason) so the UI never
/// shows a stale entry.
function retryTranscript(callId) {
  invoke("transcription_retry", { call_id: callId })
    .then(refreshPendingRetriesList)
    .catch((e) => {
      showBanner(String(e), "err");
      refreshPendingRetriesList();
    });
}

function renderTranscriptButton() {
  $("btn-transcript").hidden = !(state.transcript || state.pendingRetries.length);
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

/// `callId`/`peer`/`direction` are always caller-supplied values, never
/// read from `state.call` here - see the `btn-transcribe-manual` click
/// handler's own comment (2026-07-16 4R re-review, A1) for why: `state.call`
/// can already be null (a `closed` event raced an in-flight
/// `transcription_manual_start`) by the time this runs.
function beginTranscript(callId, peer, direction) {
  if (state.transcript && state.transcript.callId === callId) return; // already tracking (re-entrant established)
  if (state.transcript && state.transcript.phase === "error") {
    // Don't silently drop an unresolved save failure for the PREVIOUS
    // call just because a new one started (2026-07-16 4R re-review, M2)
    // - transcription_pending_retries already tracks it backend-side
    // independent of state.transcript; refresh state.pendingRetries from
    // it before this call's own info is overwritten, so "Retry now" for
    // the earlier call stays reachable (rendered as part of the new
    // model's own otherPendingRetries, or via the titlebar button's
    // fallback view once nothing is "live").
    refreshPendingRetriesList();
  }
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
  if (!state.transcript && !state.pendingRetries.length) return;
  $("tr-peer-name").textContent = state.transcript ? extractUser(state.transcript.peer) || t("transcript.defaultTitle") : t("transcript.defaultTitle");
  renderTranscriptScreenBody();
  $("screen-transcript").hidden = false;
}

function closeTranscriptScreen() {
  $("screen-transcript").hidden = true;
}

function renderTranscriptScreenBody() {
  if (state.transcript) {
    $("tr-state-label").textContent = transcriptStateLabel(state.transcript.phase);
    const model = {
      ...state.transcript,
      otherPendingRetries: state.pendingRetries.filter((r) => r.callId !== state.transcript.callId),
    };
    renderTranscriptBody($("transcript-body"), model, {
      onCopy: async (text) => {
        try {
          await navigator.clipboard.writeText(text);
          showBanner(t("transcript.copiedToClipboard"), "info");
        } catch (e) {
          console.error("clipboard write failed", e);
        }
      },
      onShowFolder: revealInFileManager,
      onShowLocal: revealInFileManager,
      onRetry: () => {
        if (state.transcript) retryTranscript(state.transcript.callId);
      },
      onRetryOther: retryTranscript,
    });
  } else if (state.pendingRetries.length) {
    $("tr-state-label").textContent = t("transcript.couldntSave");
    renderPendingRetriesOnly($("transcript-body"), state.pendingRetries, { onRetryOther: retryTranscript });
  } else {
    $("tr-state-label").textContent = "";
    $("transcript-body").innerHTML = "";
  }
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
    showBanner(t("transcript.readyFor", { who: extractUser(state.transcript.peer) || t("transcript.callWord") }), "info");
  }
  // This call may have just resolved an entry in the pending list (e.g.
  // it had errored once, then a live-process-died-early retry - A3 in
  // transcription.rs - quietly succeeded on its own) - keep the list
  // honest either way.
  refreshPendingRetriesList();
}

function handleTranscriptError(payload) {
  if (!state.transcript || !payload || state.transcript.callId !== payload.call_id) return;
  if (!payload.retryable) {
    // Non-terminal notice (e.g. a live process died early - transcription.rs
    // will run a full post-call pass once the call actually ends). The
    // pipeline resolves itself via a later done/error; nothing to change
    // about the visible phase yet.
    showBanner(payload.message || t("transcript.hiccup"), "info");
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
  // `transcription://error` only carries {call_id, message, retryable} -
  // the local-copy paths and any channels_failed the engine already knew
  // about live in the backend's pending_retries map instead
  // (PendingRetryView, commands::transcription_pending_retries).
  // refreshPendingRetriesList both populates state.transcript.error with
  // those extra fields (when its callId matches) AND keeps
  // state.pendingRetries - the list every OTHER call's unresolved
  // failure lives in - in sync (M2).
  refreshPendingRetriesList();
}

/// Fetches the backend's full pending-retries list (every call whose
/// transcript couldn't be moved into storage_dir yet, not just the
/// currently-displayed one) and reconciles `state.pendingRetries` +
/// `state.transcript.error` (when it's one of the entries) from it.
/// Called at boot (so an app restart doesn't lose visibility into a
/// retry that was pending when it last quit - 2026-07-16 4R re-review,
/// M2), and after any event that could change the set (a new retryable
/// error, a done that might have resolved one, a retry attempt either
/// way).
async function refreshPendingRetriesList() {
  try {
    const list = await invoke("transcription_pending_retries");
    state.pendingRetries = (list || []).map((r) => ({
      callId: r.call_id,
      peer: r.peer,
      startedAt: r.started_at,
      lastError: r.last_error,
      channelsFailed: r.channels_failed || [],
      localTxtPath: r.local_txt_path || null,
      localJsonPath: r.local_json_path || null,
    }));
    if (state.transcript) {
      const mine = state.pendingRetries.find((r) => r.callId === state.transcript.callId);
      if (mine) {
        state.transcript.error = {
          message: mine.lastError,
          retryable: true,
          localTxtPath: mine.localTxtPath,
          localJsonPath: mine.localJsonPath,
          channelsFailed: mine.channelsFailed,
        };
      }
    }
    renderTranscriptButton();
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
    showBanner(payload.message || t("sidecar.engineStopped"), "err", 0);
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
      showBanner(evt.message || t("sidecar.somethingWrong"), "err");
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
        <label for="fav-label-${i}">${escapeHtml(t("settings.favLabelLabel"))}</label>
        <input id="fav-label-${i}" type="text" placeholder="${escapeAttr(t("settings.favLabelPlaceholder"))}" maxlength="40" style="font-family:var(--font-ui)">
      </div>
      <div class="field">
        <label for="fav-ext-${i}">${escapeHtml(t("settings.favExtLabel"))}</label>
        <input id="fav-ext-${i}" type="text" placeholder="${escapeAttr(t("settings.favExtPlaceholder"))}" maxlength="20">
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
    $("secret-hint").textContent = account.secret_set ? t("settings.secretCurrentlySet") : t("settings.secretNotSet");
    $("in-core-path").value = corePath || "";
    setTransportUI(account.transport_priority || "auto");
    setThemeUI(theme || "auto");
    document.querySelectorAll("#locale-row button").forEach((b) => {
      b.classList.toggle("on", b.dataset.localeChoice === state.localePref);
    });
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

// ---------------------------------------------------------------------------
// language (i18n) - "auto" follows this computer's OS language (see
// i18n.js's detectSystemLocale), same "auto" semantic theme already uses
// (CSS prefers-color-scheme there; navigator.language here) rather than
// writing a resolved value back to settings on first boot. Sits under the
// same admin-lock overlay as every other #settings-body control (task
// brief: "setting bajo admin lock") - no extra plumbing needed, the
// overlay already covers the whole card (see index.html's #locale-row
// comment).
// ---------------------------------------------------------------------------
function setLocaleUI(pref) {
  state.localePref = pref;
  const resolved = setLocale(pref);
  document.querySelectorAll("#locale-row button").forEach((b) => {
    b.classList.toggle("on", b.dataset.localeChoice === pref);
  });
  refreshAllUiText();
  return resolved;
}

/// Re-applies every translated string currently on screen after a live
/// language switch - static [data-i18n*] markup plus the handful of
/// screens whose text is computed from state rather than sitting still in
/// the DOM (titlebar/reg-pill/favorites/call-overlay/recents/settings
/// hints/the transcript panel, if open). Confirmation overlays
/// (dial-confirm/provision-confirm) are NOT re-rendered here - they're
/// short-lived and already build their text with t() at the moment they
/// open, so the next time one opens it's correct in the new language;
/// re-rendering one that happens to be open mid-switch is an accepted
/// edge case, same class as other races this codebase documents rather
/// than fully closes (see e.g. transcription.rs's own comments).
function refreshAllUiText() {
  applyStaticI18n();
  renderAll();
  renderIdentity();
  renderDial();
  renderFavorites();
  renderCallOverlay();
  renderRecents(state.recents);
  if (state.account) {
    $("secret-hint").textContent = state.account.secret_set ? t("settings.secretCurrentlySet") : t("settings.secretNotSet");
  }
  if (!$("screen-transcript").hidden) {
    $("tr-peer-name").textContent = state.transcript ? extractUser(state.transcript.peer) || t("transcript.defaultTitle") : t("transcript.defaultTitle");
    renderTranscriptScreenBody();
  }
}

async function saveAccountSettings() {
  const statusEl = $("save-status");
  statusEl.className = "status";
  statusEl.textContent = t("settings.saving");
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
    statusEl.textContent = t("settings.savedReconnecting");
    statusEl.className = "status ok";
    const account = await invoke("get_account_settings");
    state.account = account;
    renderIdentity();
    $("in-secret").value = "";
    $("secret-hint").textContent = account.secret_set ? t("settings.secretCurrentlySet") : t("settings.secretNotSet");
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
    // Capture everything BEFORE the await (4R re-review 2026-07-16, A1):
    // a call_state:"closed" can land while this invoke is still in
    // flight, and its handler sets state.call = null synchronously
    // (finalizeClosedCall). Reading state.call.* again after the await
    // would then throw (crashing this handler as an uncaught rejection,
    // shown to the operator as a raw JS error banner) AND skip
    // beginTranscript entirely - even though the backend already
    // accepted the tap and will transcribe the call regardless, so every
    // transcription:// event for it would be silently dropped
    // (handleTranscriptSegment/Done/Error all require a live
    // state.transcript.callId match). beginTranscript itself never reads
    // state.call - only these captured values do.
    const callId = state.call.callId;
    const peer = state.call.peer || "";
    const direction = state.call.direction;
    btn.disabled = true;
    try {
      await invoke("transcription_manual_start", { call_id: callId, peer });
      beginTranscript(callId, peer, direction);
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
  document.querySelectorAll("#locale-row button").forEach((b) => {
    b.addEventListener("click", async () => {
      setLocaleUI(b.dataset.localeChoice);
      try {
        await invoke("set_locale", { locale: b.dataset.localeChoice });
      } catch (e) {
        console.error("set_locale failed", e);
      }
    });
  });

  $("btn-save-settings").addEventListener("click", saveAccountSettings);
  $("btn-restart-engine").addEventListener("click", () => {
    invoke("sidecar_restart");
    showBanner(t("settings.restarting"), "info");
  });

  $("btn-unlock").addEventListener("click", async () => {
    const pw = $("unlock-password").value;
    const ok = await invoke("admin_unlock", { password: pw });
    if (ok) {
      state.adminUnlocked = true;
      applyLockUI();
    } else {
      $("unlock-error").textContent = t("settings.incorrectPassword");
    }
  });
  $("unlock-password").addEventListener("keydown", (e) => {
    if (e.key === "Enter") $("btn-unlock").click();
  });
  $("btn-cancel-unlock").addEventListener("click", closeSettings);

  $("btn-firstrun-set").addEventListener("click", async () => {
    const pw = $("firstrun-password").value;
    if (pw.length < 8) {
      $("firstrun-error").textContent = t("settings.useAtLeast8");
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
      statusEl.textContent = t("settings.useAtLeast8");
      statusEl.className = "hint err";
      return;
    }
    try {
      await invoke("admin_set_password", { new_password: pw });
      $("in-admin-new").value = "";
      statusEl.textContent = t("settings.passwordUpdated");
      statusEl.className = "hint ok";
    } catch (e) {
      statusEl.textContent = String(e);
      statusEl.className = "hint err";
    }
  });

  document.addEventListener("keydown", (e) => {
    if (!$("dial-confirm-overlay").hidden) return; // handled by its own listener below
    if (!$("provision-confirm-overlay").hidden) return; // handled by its own listener below
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

  // ---- auto-provisioning (spec §5, provisioning.rs) -----------------------
  // Paste flow: #prov-input + Connect -> provisioning_resolve -> preview
  // shown in #provision-confirm-overlay -> Connect there -> provisioning_apply.
  // The deep-link entry path (a centinelo://provision link) skips straight
  // to the same overlay via the "provisioning://preview" event - see
  // attachTauriListeners below - so both paths share every handler here.
  $("prov-connect").addEventListener("click", () => provisioningResolveFromInput());
  $("prov-input").addEventListener("keydown", (e) => {
    if (e.key === "Enter") provisioningResolveFromInput();
  });
  $("btn-prov-confirm").addEventListener("click", async () => {
    $("prov-confirm-error").hidden = true;
    const btn = $("btn-prov-confirm");
    btn.disabled = true;
    try {
      await invoke("provisioning_apply");
      $("provision-confirm-overlay").hidden = true;
      $("prov-input").value = "";
      showBanner(t("provisioning.connectedRegistering"), "info");
      state.account = await invoke("get_account_settings");
      renderIdentity();
    } catch (e) {
      $("prov-confirm-error").textContent = String(e);
      $("prov-confirm-error").hidden = false;
    } finally {
      btn.disabled = false;
    }
  });
  $("btn-prov-cancel").addEventListener("click", async () => {
    $("provision-confirm-overlay").hidden = true;
    try {
      await invoke("provisioning_cancel");
    } catch (e) {
      console.error("provisioning_cancel failed", e);
    }
  });
  document.addEventListener("keydown", (e) => {
    if ($("provision-confirm-overlay").hidden) return;
    if (e.key === "Enter") $("btn-prov-confirm").click();
    else if (e.key === "Escape") $("btn-prov-cancel").click();
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
    statusEl.textContent = t("settings.copied");
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
const CLICK_TO_CALL_SOURCE_KEY = {
  bridge: "clickToCallSource.bridge",
  tel: "clickToCallSource.tel",
  centinelo: "clickToCallSource.centinelo",
};

function handleClickToCall(payload) {
  if (!payload || !payload.number) return;
  const sourceLabel = CLICK_TO_CALL_SOURCE_KEY[payload.source] ? t(CLICK_TO_CALL_SOURCE_KEY[payload.source]) : t("clickToCallSource.fallback");
  if (state.call) {
    showBanner(t("call.cantDialBusy", { number: payload.number }), "err");
    return;
  }
  if (payload.auto_dial) {
    showBanner(t("call.dialingFrom", { number: payload.number, source: sourceLabel }), "info");
    dialUri(payload.number);
    return;
  }
  confirmAndDial(payload.number, t("dialConfirm.fromSource", { source: sourceLabel }));
}

async function attachTauriListeners() {
  await listen("sidecar-status", (e) => handleSidecarStatus(e.payload));
  await listen("sidecar-event", (e) => handleSidecarEvent(e.payload));
  await listen("click-to-call", (e) => handleClickToCall(e.payload));
  await listen("transcription://segment", (e) => handleTranscriptSegment(e.payload));
  await listen("transcription://done", (e) => handleTranscriptDone(e.payload));
  await listen("transcription://error", (e) => handleTranscriptError(e.payload));
  // Auto-provisioning deep link (provisioning.rs handle_deep_link, wired
  // from deeplink.rs) - same preview shape provisioning_resolve returns
  // directly for the paste flow, so one confirmation screen serves both.
  await listen("provisioning://preview", (e) => showProvisioningConfirm(e.payload));
  await listen("provisioning://error", (e) => {
    const message = e.payload && e.payload.message ? e.payload.message : String(e.payload);
    showBanner(t("provisioning.linkError", { message }), "err");
  });
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

  // Locale first, before any other render below reads t() - "auto" (the
  // default, matching an unconfigured settings.json) resolves against this
  // computer's OS language (see i18n.js detectSystemLocale); an explicit
  // saved choice always wins. Applying [data-i18n*] markup here means the
  // very first real paint (post pre-JS-load flash of index.html's English
  // fallback text) is already in the right language.
  try {
    const localePref = await invoke("get_locale");
    state.localePref = localePref || "auto";
    setLocale(state.localePref);
  } catch (e) {
    console.error("get_locale failed", e);
    setLocale("auto");
  }
  applyStaticI18n();

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
  // Catches a provisioning preview whose "provisioning://preview" event
  // already fired (and was lost - Tauri doesn't queue/replay events for
  // listeners that attach late) before the listener above existed - the
  // real scenario for a cold-start centinelo://provision?config=... deep
  // link, which resolves synchronously in the Rust side's .setup(), well
  // before this script has even loaded (2026-07-16 4R re-review, R3).
  // Checked AFTER attachTauriListeners(), not before: together they cover
  // every timing - anything that fires before this line lands here via
  // the peek, anything after is caught live by the listener, no gap
  // remains either way. Non-consuming (provisioning.rs `peek()`), so this
  // can't race a real apply/cancel that happens to land at the same time.
  try {
    const pendingPreview = await invoke("provisioning_pending_preview");
    if (pendingPreview) showProvisioningConfirm(pendingPreview);
  } catch (e) {
    console.error("provisioning_pending_preview failed", e);
  }
  await applyPremiumUI();
  await applyTranscriptionUI();
  // Re-hydrate any transcript(s) still waiting to save from a previous
  // run (2026-07-16 4R re-review, M2) - transcription_pending_retries is
  // backend-tracked independent of this window's own lifetime, so a
  // restart while a NAS was down, say, shouldn't lose visibility into it.
  if (state.transcription.unlocked) await refreshPendingRetriesList();
}

boot();

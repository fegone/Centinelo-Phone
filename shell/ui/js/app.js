// Centinelo Phone shell — frontend logic.
// No bundler: this is a plain ES module loaded directly by the webview.
// Talks to the Rust backend exclusively through Tauri commands/events
// (window.__TAURI__, injected because tauri.conf.json sets
// app.withGlobalTauri = true) — never touches the sidecar process or the
// settings file directly.

import { renderTranscriptBody, renderPendingRetriesOnly } from "./transcript-panel.js";
import { t, setLocale, localeTag, applyStaticI18n } from "./i18n.js";
import { escapeHtml, escapeAttr } from "./dom-utils.js";
import {
  mapCliTierToSettingsTier,
  buildSaveTranscriptionInput,
  formatModelSize,
  startDownload,
  applyProgressEvent,
  isDownloadStalled,
  computeModelChipState,
  computeDownloadPct,
  computeRemoteSttUiVisibility,
} from "./transcription-settings.js";
import {
  initialUpdaterState,
  withChecking,
  withUpToDate,
  withAvailable,
  withCheckError,
  withDownloadStarted,
  withDownloadProgress,
  withDownloadError,
  withReady,
  withInstalling,
  withInstallError,
  withRestartError,
  withDismissed,
  canStartInstall,
  canRunBackgroundRecheck,
  closePendingUpdateResources,
  renderUpdateBanner,
  renderUpdaterAboutStatus,
} from "./updater.js";
import { computeBlfUiHidden, BLF_UI_TARGETS } from "./blf-ui.js";
import {
  armHandshake,
  reduceRegHandshake,
  shouldReleaseSaveButton,
  shouldShowInterimConnecting,
  reduceRegResult,
} from "./reg-status.js";

// `Channel` (updater download progress) and `Resource` both live on
// window.__TAURI__.core alongside `invoke` - withGlobalTauri bundles the
// WHOLE @tauri-apps/api/core module, not a curated subset (verified
// against tauri's own scripts/bundle.global.js, 2.11.5).
const { invoke, Channel } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow } = window.__TAURI__.window;
const { getVersion } = window.__TAURI__.app;

const win = getCurrentWindow();

// ---------------------------------------------------------------------------
// state
// ---------------------------------------------------------------------------
const state = {
  dial: "",
  account: null, // AccountSettingsView from the backend
  favorites: [],
  blf: {}, // ext (string) -> "idle"|"ringing"|"busy"|"offline", from sidecar "blf" events
  // ---- BLF master switch (P5) ----------------------------------------
  // blfEnabled mirrors settings.blf.enabled (read via get_blf_enabled in
  // boot). false = the favorites grid + heading are hidden and the console
  // entry is absent from view (see renderBlfUi). Defaults to true to keep
  // the shipped cold-boot appearance until the value resolves.
  blfEnabled: true,
  // consoleUnlocked is set by applyPremiumUI (premium_capability_status for
  // blf_console); renderBlfUi ANDs it with blfEnabled so BLF off hides the
  // console regardless of its own license gate.
  consoleUnlocked: false,
  bridge: null, // BridgeSettingsView from the backend (click-to-call + deep links)
  regState: "unregistered", // unregistered|registering|registered|failed
  regReason: null, // last SIP failure reason text (only meaningful when regState === "failed")
  // Set by saveAccountSettings while awaiting the next terminal reg_state after a
  // Save; cleared/resolved by handleSidecarEvent/renderSaveStatusForRegState.
  // { generation, resolve, timer } | null. See reg-status.js's module
  // header for what `generation` buys (and doesn't, today).
  pendingRegResult: null,
  // Per-Save generation counter: bumped each time saveAccountSettings arms a
  // handshake (awaitRegResult). Also gates re-enabling #btn-save-settings
  // (releaseSaveButton) so an older, preempted Save's terminal path can't
  // re-enable a button a newer, still in-flight Save disabled.
  regGeneration: 0,
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
  // ---- auto-updater (roadmap debt fix) -----------------------------------
  // See ui/js/updater.js's header comment for the full state machine.
  // check_on_startup mirrors the persisted setting, not part of the
  // updater state machine itself (same "preference vs. resolved value"
  // split theme/locale already use).
  updater: initialUpdaterState(),
  updaterCheckOnStartup: true,
  // ---- availability / auto-answer (shell task) ---------------------------
  // Mirrors settings.availability - not part of a call_state machine, just
  // the two persisted preferences (see settings.rs AvailabilitySettings and
  // ui/js/call-availability.js's computeCallHandling for the decision they
  // combine into, which Rust alone actually applies to the engine). Seeded
  // optimistically to the shipped defaults so the titlebar button never
  // flashes an unstyled state before boot()'s get_availability_settings
  // resolves.
  availability: { available: true, autoAnswer: false },
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
        ? state.regReason
          ? t("regPill.failedReason", { reason: state.regReason })
          : t("regPill.failedTitle")
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

/// Reflects state.availability.available onto the titlebar's #btn-availability
/// dot + its title/aria-label (availability.titlebarAvailableTitle/
/// titlebarDndTitle) and, if Settings is open, the Availability section's
/// bool rows. auto_answer's OWN visible state lives only in the tray
/// checkmark + the Settings bool row (setAvailabilityFieldsUI) - no
/// titlebar affordance for it, per the shell task brief ("Titlebar/
/// indicador: refleja el estado de disponibilidad", auto-answer isn't
/// named there).
function renderAvailabilityUI() {
  const btn = $("btn-availability");
  const available = state.availability.available;
  btn.classList.toggle("available", available);
  btn.classList.toggle("dnd", !available);
  const label = t(available ? "availability.titlebarAvailableTitle" : "availability.titlebarDndTitle");
  btn.title = label;
  btn.setAttribute("aria-label", label);
  setAvailabilityFieldsUI();
}

/// Settings pane's Availability bool rows - only touches the DOM if the
/// rows exist (they're always in index.html, unlike the transcription
/// section's conditional markup, so this is really just keeping the
/// helper symmetric with the rest of this file's setBoolRowUI call sites).
function setAvailabilityFieldsUI() {
  if (!$("available-row")) return;
  setBoolRowUI("available-row", state.availability.available);
  setBoolRowUI("auto-answer-row", state.availability.autoAnswer);
}

function renderAll() {
  renderWatchlamp();
  renderRegPill();
  renderTitlebarState();
  renderAvailabilityUI();
}

// ---------------------------------------------------------------------------
// idle / configured area
// ---------------------------------------------------------------------------
function renderIdentity() {
  const configured = !!(state.account && state.account.host && state.account.ext);
  $("setup-prompt").hidden = configured;
  $("configured-area").hidden = !configured;
  if (!configured) return;
  const name = state.account.display_name || t("provisioning.extensionOnly", { ext: state.account.ext });
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
      state.regReason = evt.reason || null;
      // FIX B: while a Settings Save handshake is active, mirror the pill into
      // #save-status LIVE on every reg_state. Never freeze on the first
      // `failed`: the engine auto-retries registration after a failure (see
      // regPill.failedReason's "...retrying automatically"), so a `failed` must
      // show "retrying" and keep waiting — a later `registered` then flips
      // #save-status green instead of contradicting a permanent red. Only
      // `registered` is terminal (settles the handshake). Both the pill and
      // #save-status read this same evt, so they can never disagree.
      renderSaveStatusForRegState(evt.state, evt.reason);
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
  // A reg-status handshake in flight (saveAccountSettings disables the
  // button for its duration - see releaseSaveButton) must win over the
  // admin-lock gate here: applyLockUI also runs on re-opening Settings
  // (openSettings) and on admin unlock, both reachable WHILE a Save from
  // moments ago is still awaiting its terminal reg_state, and would
  // otherwise stomp the disabled flag back to enabled out from under it.
  saveBtn.disabled = !state.adminUnlocked || !!state.pendingRegResult;
}

// ---------------------------------------------------------------------------
// transcription settings (Plate 08 - Settings → Transcription)
//
// Absent, not disabled, until state.transcription.unlocked (set by
// applyTranscriptionUI at boot/on call events - reused here rather than
// re-querying premium_capability_status a second time) - matches every
// other premium-gated entry point in this shell. Mirrors the deferred-save
// shape #in-core-path/#favorites-fields already use (collected into one
// payload, sent together with account settings on the "Save" button) since
// the backend only has one save_transcription_settings call that takes the
// whole TranscriptionSettings struct at once - there's no per-field
// command to invoke immediately the way #theme-row/#locale-row have.
// ---------------------------------------------------------------------------
let selectedTranscriptionMode = "off";
let selectedTranscriptionActivation = "all_calls";
let selectedModelTier = "accurate";
let selectedTranscriptionLanguage = "auto";
let transcriptionKeepAudio = false;
let transcriptionViewOnly = false;

// ---- remote STT (P6) ----------------------------------------------------
// stt_mode/remote_backend mirror the settings.rs enums (SttMode/
// RemoteBackend) by their serde snake_case values ("local"/"remote",
// "centinelo"/"openai_compat"). #in-remote-stt-key is NEVER populated from
// get_transcription_settings (TranscriptionSettingsView deliberately omits
// remote_api_key - see commands.rs's own comment on that struct), and
// save_transcription_settings has no "blank = keep unchanged" affordance
// for it the way save_account_settings's `secret: Option<String>` does for
// the SIP password (settings.rs update_transcription replaces the whole
// TranscriptionSettings struct wholesale) - every Save resends whatever is
// currently in that field, blank or not. cachedRemoteApiKey is this app's
// own (never-persisted-as-such, JS-memory-only) mitigation: once a Save
// this session actually wrote a non-empty key, that value re-populates the
// field the next time Settings reopens in the SAME running app, so an
// unrelated field edit (e.g. the storage folder) followed by Save doesn't
// silently wipe a key the operator set five minutes ago. A fresh app
// launch starts this at "" like every other secret field on this screen -
// #settings.remoteSttKeyHint says so.
let selectedSttMode = "local";
let selectedRemoteBackend = "centinelo";
let cachedRemoteApiKey = "";

// tier -> {present, sizeBytes} | {error: true} | null (not yet fetched
// this Settings session) for modelStatus - see transcription-settings.js's
// computeModelChipState doc for what each shape means and why {error:
// true} must stay distinct from {present: false} (RESILIENCE #5: a failed
// status CHECK is not the same fact as a confirmed-absent model, and must
// not silently offer a live "Download" for a tier that might already be
// installed). tier -> a download record (see transcription-settings.js) |
// null (no download in flight) for modelDownload.
const modelStatus = { accurate: null, light: null };
const modelDownload = { accurate: null, light: null };
// tier -> this tier's last-used generation number (see
// transcription-settings.js's startDownload doc for why this can't just
// live on modelDownload[tier] - it must survive the record clearing back
// to null when a download settles). tier -> the watchdog's setInterval id
// | null for downloadWatchdogs (RESILIENCE #3).
const modelDownloadGeneration = { accurate: 0, light: 0 };
const downloadWatchdogs = { accurate: null, light: null };

// A download that's gone this long without a progress event is presumed
// dead (its transcribe-engine sidecar exited without ever emitting a
// terminal event) - checked every DOWNLOAD_WATCHDOG_POLL_MS. Generous on
// purpose: a real download of these models (500 MB-1 GB) over a slow
// connection can plausibly go quiet between chunks for tens of seconds;
// this is a "the process is gone" detector, not a speed complaint.
const DOWNLOAD_WATCHDOG_TIMEOUT_MS = 60_000;
const DOWNLOAD_WATCHDOG_POLL_MS = 5_000;

function setTranscriptionModeUI(mode) {
  selectedTranscriptionMode = mode;
  document.querySelectorAll("#transcription-mode-choice .tcard").forEach((card) => {
    const sel = card.dataset.transcriptionMode === mode;
    card.classList.toggle("sel", sel);
    card.setAttribute("aria-checked", String(sel));
  });
}

function setTranscriptionActivationUI(activation) {
  selectedTranscriptionActivation = activation;
  document.querySelectorAll("#transcription-activation-choice .tcard").forEach((card) => {
    const sel = card.dataset.transcriptionActivation === activation;
    card.classList.toggle("sel", sel);
    card.setAttribute("aria-checked", String(sel));
  });
}

function setTranscriptionLanguageUI(lang) {
  selectedTranscriptionLanguage = lang;
  document.querySelectorAll("#transcription-language-row button").forEach((b) => {
    b.classList.toggle("on", b.dataset.languageChoice === lang);
  });
}

function setModelTierUI(tier) {
  selectedModelTier = tier;
  document.querySelectorAll("#transcription-model-choice .modelrow").forEach((row) => {
    const sel = row.dataset.modelTier === tier;
    row.classList.toggle("sel", sel);
    row.setAttribute("aria-checked", String(sel));
  });
}

// ---- remote STT (P6) ------------------------------------------------------

function setSttModeUI(mode) {
  selectedSttMode = mode;
  document.querySelectorAll("#stt-mode-row button").forEach((b) => {
    b.classList.toggle("on", b.dataset.sttModeChoice === mode);
  });
  applyRemoteSttVisibility();
}

function setRemoteBackendUI(backend) {
  selectedRemoteBackend = backend;
  document.querySelectorAll("#remote-stt-backend-row button").forEach((b) => {
    b.classList.toggle("on", b.dataset.remoteBackendChoice === backend);
  });
  applyRemoteSttVisibility();
}

/// Applies computeRemoteSttUiVisibility's decision to the two DOM nodes it
/// covers - see that function's own doc (transcription-settings.js) for why
/// the model field has its own, narrower condition than the rest of the
/// remote block.
function applyRemoteSttVisibility() {
  const { remoteFieldsHidden, modelFieldHidden } = computeRemoteSttUiVisibility({
    sttMode: selectedSttMode,
    remoteBackend: selectedRemoteBackend,
  });
  $("remote-stt-fields").hidden = remoteFieldsHidden;
  $("remote-stt-model-field").hidden = modelFieldHidden;
}

/// Rebuilds the one `.mrstatus` chip for `tier` from computeModelChipState's
/// verdict on the current (modelDownload[tier], modelStatus[tier]) pair -
/// see transcription-settings.js for the state machine itself (tested
/// there without any DOM). This function's only job is turning that pure
/// state into markup/i18n/DOM. Ink-only throughout (--text-3/--st-idle, no
/// amber) - REVIEW.md §4b's "no amber, nothing glows here" rule for this
/// pane's download UI.
function renderModelStatusChip(tier) {
  const el = $(`model-status-${tier}`);
  if (!el) return;
  const chip = computeModelChipState(modelDownload[tier], modelStatus[tier]);
  switch (chip.kind) {
    case "downloading": {
      const dl = modelDownload[tier];
      const pct = chip.pct;
      const sizeTitle =
        dl.totalBytes != null
          ? escapeAttr(`${formatModelSize(dl.downloadedBytes)} / ${formatModelSize(dl.totalBytes)}`)
          : escapeAttr(formatModelSize(dl.downloadedBytes));
      el.innerHTML = `<span class="dlwrap" title="${sizeTitle}"><span class="dlbar" aria-hidden="true"><i style="width:${pct === null ? 0 : pct}%"></i></span><span class="dlpct">${
        pct === null ? escapeHtml(t("settings.transcriptionModelDownloading")) : `${pct}%`
      }</span></span>`;
      return;
    }
    case "installed": {
      const sizeText = chip.sizeBytes != null ? ` · ${formatModelSize(chip.sizeBytes)}` : "";
      el.innerHTML = `<span class="modelchip ok">${escapeHtml(t("settings.transcriptionModelInstalled"))}${escapeHtml(sizeText)}</span>`;
      return;
    }
    case "offer-download":
      // Confirmed absent - offer the real download this app can actually
      // perform (download_transcription_model), not a dead button.
      el.innerHTML = `<button type="button" class="modelchip btn" data-download-tier="${escapeAttr(tier)}">${escapeHtml(t("settings.transcriptionModelDownload"))}</button>`;
      return;
    case "check-failed":
      // RESILIENCE #5 - the transcription_model_status fetch itself
      // failed, which is NOT "confirmed absent": offer a real retry
      // (retryModelStatus) instead of a "Download" button that could
      // silently re-download an already-installed model.
      el.innerHTML = `<span class="dlwrap"><span class="modelchip">${escapeHtml(t("settings.transcriptionModelCheckFailed"))}</span><button type="button" class="modelchip btn" data-retry-status-tier="${escapeAttr(
        tier,
      )}">${escapeHtml(t("settings.transcriptionModelRetry"))}</button></span>`;
      return;
    case "unknown":
    default:
      el.innerHTML = "";
  }
}

/// Fetches `transcription_model_status` for a single tier and renders its
/// chip - the unit `refreshModelStatuses` (both tiers) and
/// `retryModelStatus` (the check-failed chip's "Retry" button, one tier)
/// both build on. A failed fetch is recorded as `{error: true}`, NOT
/// `{present: false, ...}` (RESILIENCE #5 - see modelStatus's own comment
/// above for why those two must stay distinguishable).
async function refreshOneModelStatus(tier) {
  try {
    const status = await invoke("transcription_model_status", { tier });
    modelStatus[tier] = { present: status.present, sizeBytes: status.size_bytes };
  } catch (e) {
    console.error(`transcription_model_status(${tier}) failed`, e);
    modelStatus[tier] = { error: true };
  }
  renderModelStatusChip(tier);
}

/// Refreshes both tiers - called when Settings opens and again after a
/// download settles (handleModelDownloadSettled), so "Installed · N MB"
/// always reflects what's actually on disk rather than an assumption from
/// the download event alone.
async function refreshModelStatuses() {
  await Promise.all(["accurate", "light"].map(refreshOneModelStatus));
}

/// The check-failed chip's "Retry" action - re-fetches just the one tier
/// that failed rather than both (refreshModelStatuses), so a working tier's
/// chip doesn't flash while only the broken one is being retried.
function retryModelStatus(tier) {
  refreshOneModelStatus(tier);
}

function clearDownloadWatchdog(tier) {
  if (downloadWatchdogs[tier] != null) {
    clearInterval(downloadWatchdogs[tier]);
    downloadWatchdogs[tier] = null;
  }
}

/// RESILIENCE #3: if `download_transcription_model`'s sidecar dies without
/// ever emitting `transcription://model-download-done`/`-error`, nothing
/// would otherwise clear `modelDownload[tier]` - the chip would read
/// "Downloading…" forever. Polls every DOWNLOAD_WATCHDOG_POLL_MS and, once
/// isDownloadStalled says this attempt has gone quiet too long, degrades it
/// to a banner + "Download" button, same shape as any other download
/// failure (startModelDownload's own catch block).
///
/// `generation` is the value this specific attempt was started with
/// (startDownload's return) - checked on every tick (not just once) so
/// that if a NEWER download for the same tier starts and this stale timer
/// somehow wasn't cleared by clearDownloadWatchdog first, it silently
/// stops itself instead of ever touching the newer attempt's record.
function armDownloadWatchdog(tier, generation) {
  clearDownloadWatchdog(tier);
  downloadWatchdogs[tier] = setInterval(() => {
    const dl = modelDownload[tier];
    if (!dl || dl.generation !== generation) {
      clearDownloadWatchdog(tier);
      return;
    }
    if (isDownloadStalled(dl, Date.now(), DOWNLOAD_WATCHDOG_TIMEOUT_MS)) {
      clearDownloadWatchdog(tier);
      modelDownload[tier] = null;
      renderModelStatusChip(tier);
      showBanner(t("settings.transcriptionModelDownloadStalled"), "err");
    }
  }, DOWNLOAD_WATCHDOG_POLL_MS);
}

async function startModelDownload(tier) {
  if (modelDownload[tier]) return; // already downloading
  modelDownload[tier] = startDownload(modelDownloadGeneration[tier], Date.now());
  modelDownloadGeneration[tier] = modelDownload[tier].generation;
  armDownloadWatchdog(tier, modelDownload[tier].generation);
  renderModelStatusChip(tier);
  try {
    // `download_transcription_model` only rejects synchronously for an
    // immediate precondition - it checks the `transcription` license
    // server-side (crate::transcription::is_unlocked) before ever calling
    // spawn_model_download, same gate save_transcription_settings itself
    // uses (commands.rs) - so this catch also covers "license revoked
    // since Settings opened", not just a hypothetical. A download that
    // starts and later fails mid-transfer instead settles via the
    // transcription://model-download-error event, handled by
    // handleModelDownloadSettled. Either way, don't leave the chip stuck
    // on a dead "failed" state forever: clear modelDownload[tier] back to
    // null so the row offers "Download" again, and say what happened as
    // a transient banner instead (this pane's one existing error surface
    // - #save-status is Settings' own, not this chip's).
    await invoke("download_transcription_model", { tier });
  } catch (e) {
    clearDownloadWatchdog(tier);
    modelDownload[tier] = null;
    renderModelStatusChip(tier);
    showBanner(String(e), "err");
  }
}

function handleModelDownloadProgress(payload) {
  if (!payload || !payload.tier) return;
  const tier = mapCliTierToSettingsTier(payload.tier);
  if (!tier || !(tier in modelDownload)) return;
  const next = applyProgressEvent(modelDownload[tier], payload, Date.now());
  if (next === modelDownload[tier]) return; // ignored - see applyProgressEvent's doc (RESILIENCE #4)
  modelDownload[tier] = next;
  renderModelStatusChip(tier);
}

/// Shared tail for both `transcription://model-download-done` and
/// `-error` - either way the download is no longer "in flight", and the
/// real answer to "is it installed now" is always transcription_model_status
/// (disk truth), not an inference from which event fired.
function handleModelDownloadSettled(payload, errorMessage) {
  if (!payload || !payload.tier) return;
  const tier = mapCliTierToSettingsTier(payload.tier);
  if (!tier || !(tier in modelDownload)) return;
  clearDownloadWatchdog(tier);
  modelDownload[tier] = null;
  if (errorMessage) {
    showBanner(errorMessage, "err");
  }
  refreshModelStatuses();
}

// ---------------------------------------------------------------------------
// auto-updater (roadmap debt fix) - see ui/js/updater.js's header comment
// for the full design. This block owns every @tauri-apps/plugin-updater /
// @tauri-apps/plugin-process call - neither plugin's JS package can be
// imported here (bare-specifier `import ... from "@tauri-apps/plugin-
// updater"` can't resolve without a bundler, and this project deliberately
// has none - see shell/README.md's opening line).
//
// check() still calls the plugin's own `plugin:updater|check` directly
// (read-only, no call-safety concern - confirmed against its dist-js
// source). download()/install() do NOT (2026-07-17 4R re-review,
// RESILIENCE blocker): the plugin's own `plugin:updater|install` has no
// way to refuse while a call is active - the ONLY guard available to it
// would have to live in THIS file, reading `state.call`, which
// `beginTranscript`'s own doc elsewhere in this file documents as
// exactly the kind of client-side mirror that can already be stale by
// the time an await resolves (a `call_state:"closed"` racing it). Since
// installing an update kills the ENTIRE process, that's not an acceptable
// place for the real decision to live - `src-tauri/src/updater.rs`'s
// `updater_install` command is this app's OWN command instead, which
// checks `sidecar.has_active_call()` (the authoritative, call_id-tracked
// source hardened after the R4 provisioning bug - see that Rust module's
// own doc) immediately before calling the plugin's `Update::install()`
// directly. `canStartInstall` below stays as UX ONLY (disables the
// button before the round trip even starts) - the Rust command is what
// actually decides, and refuses on its own even if this file's guard
// were ever wrong or bypassed (e.g. via devtools).
// ---------------------------------------------------------------------------

/// `{ rid, currentVersion, version, date, body, rawJson }` from the last
/// successful `check()` that found an update - `rid` is the resource
/// `updater_download`/`updater_install` (src-tauri/src/updater.rs) below
/// need. Cleared (and its Rust-side resource closed) at the start of
/// every fresh check, so a manual re-check never leaks one resource per
/// click.
let pendingUpdateMeta = null;
/// The resource id `updater_download` hands back (the downloaded bytes,
/// this app's OWN resource type Rust-side - NOT the plugin's own private
/// one, see updater.rs's header comment for why) - `updater_install`
/// needs both this and pendingUpdateMeta.rid.
let pendingDownloadedBytesRid = null;
/// Bytes downloaded so far this attempt - the Progress event carries a
/// per-chunk delta (`chunkLength`), not a running total, so this module
/// accumulates it the same way the real plugin's own JS wrapper would if
/// it exposed one (it doesn't - callers are expected to do this
/// themselves, per the official docs' own example - and this app's own
/// `updater_download` command mirrors that same event shape on purpose,
/// see updater.rs).
let downloadedSoFar = 0;

async function pluginUpdaterCheck() {
  return invoke("plugin:updater|check", {});
}

/// `updater_download` (src-tauri/src/updater.rs), not `plugin:updater|
/// download` - see this section's header comment.
async function updaterDownload(updateRid, onEvent) {
  const channel = new Channel();
  channel.onmessage = onEvent;
  return invoke("updater_download", { update_rid: updateRid, on_event: channel });
}

/// `updater_install` (src-tauri/src/updater.rs), not `plugin:updater|
/// install` - this is the command that actually gates on
/// `sidecar.has_active_call()`, see this section's header comment.
async function updaterInstall(updateRid, bytesRid) {
  return invoke("updater_install", { update_rid: updateRid, bytes_rid: bytesRid });
}

async function pluginProcessRestart() {
  return invoke("plugin:process|restart");
}

let updaterRefs = null;
function getUpdaterRefs() {
  if (updaterRefs) return updaterRefs;
  updaterRefs = {
    root: $("update-banner"),
    title: $("update-banner-title"),
    detail: $("update-banner-detail"),
    actions: $("update-banner-actions"),
    downloadBtn: $("btn-update-download"),
    restartBtn: $("btn-update-restart"),
    retryBtn: $("btn-update-retry"),
    laterBtn: $("btn-update-later"),
    progressWrap: $("update-banner-progress"),
    progressFill: $("update-banner-dlbar-fill"),
    progressPct: $("update-banner-dlpct"),
  };
  return updaterRefs;
}

function renderUpdaterUI() {
  renderUpdateBanner(getUpdaterRefs(), state.updater, t, {
    formatBytes: formatModelSize,
    computeDownloadPct,
    canInstallNow: () => canStartInstall(!!state.call),
  });
  renderUpdaterAboutStatus($("updater-settings-status"), $("btn-check-updates"), state.updater, t, { computeDownloadPct });
  // Always-shown version line (separate from the check-status line above -
  // see renderUpdaterAboutStatus's own doc for why the split).
  $("updater-current-version").textContent = state.updater.currentVersion
    ? t("updater.currentVersion", { version: state.updater.currentVersion })
    : "";
}

/// Closes ANY pending update resource - both the update-metadata rid AND
/// (2026-07-17 4R re-review, RELIABILITY M2 - previously only the former
/// was ever closed here, leaking the downloaded-bytes resource on every
/// download-then-abandon/download-then-recheck cycle) the downloaded-bytes
/// rid, if one exists - before starting a fresh check, so repeated manual
/// "Check for updates" clicks don't accumulate resource-table entries.
/// `plugin:resources|close` is the generic resource-table teardown command
/// every `Resource` (core.js) goes through, not a per-plugin one - works
/// identically for `updater.rs`'s own `DownloadedUpdateBytes` type as for
/// the plugin's `Update`. The actual "which ids, in what order, tolerate
/// individual failures" contract lives in `updater.js`'s
/// `closePendingUpdateResources` (unit-tested there with a counting mock)
/// - this is just that function wired to the real `invoke`.
async function closePendingUpdateResource() {
  await closePendingUpdateResources(pendingUpdateMeta, pendingDownloadedBytesRid, (rid) => invoke("plugin:resources|close", { rid }));
  pendingUpdateMeta = null;
  pendingDownloadedBytesRid = null;
}

/// Reentrancy guard - a manual "Check for updates" click landing while a
/// download/install is already in flight (or another check is already
/// running) would otherwise race it: closePendingUpdateResource() below
/// would null out pendingUpdateMeta/pendingDownloadedBytesRid out from
/// under the in-flight download()/install() call, whose own `.then`
/// continuation (still holding the OLD rid captured at its own call time)
/// would then overwrite whatever this fresh check just found once IT
/// resolves. renderUpdaterAboutStatus already disables the Settings button
/// for these phases (belt) - this is the suspenders, since
/// maybeCheckForUpdatesOnStartup() can also call this directly, not only
/// through that button.
function updateCheckInFlight() {
  return ["checking", "downloading", "installing"].includes(state.updater.phase);
}

async function performUpdateCheck() {
  if (updateCheckInFlight()) return;
  await closePendingUpdateResource();
  state.updater = withChecking(state.updater);
  renderUpdaterUI();
  try {
    const metadata = await pluginUpdaterCheck();
    if (metadata) {
      pendingUpdateMeta = metadata;
      state.updater = withAvailable(state.updater, {
        version: metadata.version,
        notes: metadata.body,
        pubDate: metadata.date,
      });
    } else {
      state.updater = withUpToDate(state.updater);
    }
  } catch (e) {
    state.updater = withCheckError(state.updater, String(e && e.message ? e.message : e));
  }
  renderUpdaterUI();
}

async function startUpdateDownload() {
  if (!pendingUpdateMeta) return;
  downloadedSoFar = 0;
  state.updater = withDownloadStarted(state.updater);
  renderUpdaterUI();
  try {
    const bytesRid = await updaterDownload(pendingUpdateMeta.rid, (event) => {
      if (event.event === "Progress") {
        downloadedSoFar += event.data.chunkLength;
        state.updater = withDownloadProgress(state.updater, { downloadedBytes: downloadedSoFar });
      } else if (event.event === "Started") {
        state.updater = withDownloadProgress(state.updater, { downloadedBytes: 0, totalBytes: event.data.contentLength });
      }
      renderUpdaterUI();
    });
    pendingDownloadedBytesRid = bytesRid;
    state.updater = withReady(state.updater);
  } catch (e) {
    state.updater = withDownloadError(state.updater, String(e && e.message ? e.message : e));
  }
  renderUpdaterUI();
}

/// The one safety gate before the disruptive step, client-side - re-
/// checked HERE, immediately before calling `updaterInstall`, not merely
/// back when the update was first found. This is UX only, though
/// (2026-07-17 4R re-review, RESILIENCE blocker): the REAL decision is
/// `src-tauri/src/updater.rs`'s own `sidecar.has_active_call()` check
/// inside the `updater_install` command itself, which is authoritative
/// (call_id-tracked, hardened after the R4 provisioning bug) where
/// `state.call` here is just this file's own client-side mirror - the
/// same kind `beginTranscript`'s doc elsewhere in this file already notes
/// can go stale mid-`await` (a `call_state:"closed"` racing it). This
/// check only saves a round trip for the common case; it can never be the
/// only thing standing between an update install and a live call.
///
/// **Windows-specific gap** (2026-07-17 4R re-review, RESILIENCE #4,
/// documented rather than silently accepted - see shell/README.md
/// "Auto-updater" for the full writeup): on Windows, the plugin's own
/// `Update::install()` hands off to the OS installer via `ShellExecuteW`
/// and calls `std::process::exit(0)` unconditionally right after,
/// discarding `ShellExecuteW`'s own return value - if the handoff itself
/// fails (UAC declined, SmartScreen, AV, permissions), the app still
/// exits with NO error and NO chance for anything below to run at all.
/// The `catch` block below is real and does work - but only on macOS/
/// Linux, where `install()` can actually return an error instead of the
/// whole process vanishing first. This is a real, unclosed gap in the
/// plugin's own Windows install path, not something this file's error
/// handling failed to cover.
async function startUpdateInstall() {
  if (!pendingUpdateMeta || pendingDownloadedBytesRid == null) return;
  if (!canStartInstall(!!state.call)) {
    renderUpdaterUI();
    return;
  }
  state.updater = withInstalling(state.updater);
  renderUpdaterUI();
  try {
    await updaterInstall(pendingUpdateMeta.rid, pendingDownloadedBytesRid);
  } catch (e) {
    state.updater = withInstallError(state.updater, String(e && e.message ? e.message : e));
    renderUpdaterUI();
    return;
  }
  // install() succeeded - the update is safely on disk, and
  // src-tauri/src/updater.rs's updater_install already closed
  // pendingDownloadedBytesRid server-side on this same success path.
  // Clearing this app's own copy too (2026-07-17 4R re-review,
  // RELIABILITY M3) means a later retry can never resend an id the Rust
  // side has already forgotten - see retryRestartOnly's own doc for what
  // happens next and why a relaunch failure from here on is a DIFFERENT,
  // less alarming state than an install failure.
  pendingDownloadedBytesRid = null;
  await retryRestartOnly();
}

/// The step after a successful `install()`: relaunch the (now-updated)
/// app. Split out from `startUpdateInstall` (2026-07-17 4R re-review,
/// RELIABILITY M3) specifically so a relaunch failure lands in
/// `withRestartError`, never `withInstallError` - by this point the
/// update is already installed; re-calling `updaterInstall` (what the
/// OLD single try/catch's Retry button would have done) would fail
/// instantly against an already-closed bytes resource AND, worse, imply
/// the update itself never took effect when it actually did. Also the
/// entry point for the "Restart to update" button in the `errorOrigin:
/// "restart"` banner state (`app.js` wireStaticHandlers) - clicking it
/// there re-attempts ONLY this step, nothing upstream of it.
async function retryRestartOnly() {
  try {
    await pluginProcessRestart();
    // Tears the process down - nothing below this line runs on success.
  } catch (e) {
    state.updater = withRestartError(state.updater, String(e && e.message ? e.message : e));
    renderUpdaterUI();
  }
}

/// Fire-and-forget on purpose (task brief: "check_on_startup, default on")
/// - boot() must not block the app becoming usable on a network round
/// trip, and a startup check failing (offline laptop) must stay silent in
/// the main window regardless (see updater.js's shouldShowBanner doc) -
/// there is nothing for a caller to `await` a meaningful outcome from.
function maybeCheckForUpdatesOnStartup() {
  if (!state.updaterCheckOnStartup) return;
  performUpdateCheck().catch((e) => console.error("startup update check failed", e));
}

/// Periodic background re-check (2026-07-17 4R re-review, RESILIENCE #3,
/// non-blocking finding, decided rather than left unaddressed): boot()'s
/// own check only ever runs once per launch, and this is a softphone that
/// can sit minimized in the tray for weeks - a build shipped the week
/// after a launch would otherwise go unnoticed indefinitely. Cheap to add
/// (one `setInterval`, no new backend surface), so implemented rather than
/// just flagged. `check_on_startup` doubles as the master switch here too,
/// per the review's own suggestion - one preference, not two, for "does
/// this app ever check on its own."
///
/// 24h, not shorter: an update is at most ~1 build old by the time this
/// would notice it even in the worst case (checked right after boot,
/// missed the interval by a second) - frequent enough for a product this
/// size, without hammering GitHub's static latest.json on every install.
const UPDATE_RECHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;

/// Background re-checks must never clobber anything already on screen -
/// the actual "is it safe right now" decision (which phases are OK to
/// silently re-check from) is `updater.js`'s `canRunBackgroundRecheck`,
/// pure and unit-tested there (see its own doc for the exact matrix, and
/// the 2026-07-17 4R re-review finding it fixes - a check-origin error
/// must stay re-checkable, or a launch with no network yet permanently
/// disables its own recheck).
function scheduleUpdatePeriodicRecheck() {
  setInterval(() => {
    if (!state.updaterCheckOnStartup) return;
    if (!canRunBackgroundRecheck(state.updater)) return;
    performUpdateCheck().catch((e) => console.error("periodic update check failed", e));
  }, UPDATE_RECHECK_INTERVAL_MS);
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

/// The serial is deliberately never round-tripped from the backend (see
/// activation.rs's module doc: "the serial itself is not persisted") -
/// #in-license-serial always starts blank, same as #in-secret's
/// "unchanged placeholder" convention but with no placeholder text either
/// (there is nothing to keep unchanged; every activation attempt sends
/// whatever is currently typed). Only the activation server URL and the
/// "is a license file already saved here" status round-trip.
function renderLicenseFields(license) {
  $("in-license-serial").value = "";
  $("in-license-server-url").value = license.activation_server_url || "";
  $("license-status-hint").textContent = license.license_present
    ? t("settings.licenseAlreadyPresentHint")
    : t("settings.licenseNotActivatedHint");
  $("license-activate-status").textContent = "";
  $("license-activate-status").className = "hint";
}

/// Maps an `activate_license` error back to displayed copy. Backend
/// errors cross the Tauri command boundary as short codes (see
/// activation.rs's own doc) except for the admin-lock check shared with
/// every other gated Settings command (`require_unlocked`'s own English
/// prose, e.g. "Settings are locked...") - `t()` returns the key itself
/// on a miss (i18n.js's own documented fallback), so an unmapped code
/// (that prose, or any future code this UI hasn't caught up with yet)
/// falls back to showing the raw backend string rather than a broken
/// "activation.error.<code>" literal.
function activationErrorText(code) {
  const key = `activation.error.${code}`;
  const translated = t(key);
  return translated === key ? String(code) : translated;
}

/// Maps a `test_remote_stt_connection` result (commands.rs's
/// RemoteSttProbeResult - {ok, code, detail}) to displayed copy. Same
/// stable-code-to-translated-string shape as activationErrorText above, but
/// this command never throws for a reachability failure - a bad URL, an
/// unreachable host, or a rejected key are all normal `ok: false` RETURN
/// values (see probe_remote_stt's own doc), not caught exceptions. `detail`
/// is appended for "network"/"http_error" specifically because those two
/// carry a real, varying diagnostic (a raw OS transport error, or the exact
/// HTTP status) - "ok"/"locked"/"bad_url"/"auth_required" all come back
/// with a fixed detail string that would just repeat the headline.
function remoteSttProbeText(result) {
  const key = `settings.remoteSttProbe.${result.code}`;
  const translated = t(key);
  const headline = translated === key ? String(result.detail || result.code) : translated;
  if (translated !== key && (result.code === "network" || result.code === "http_error") && result.detail) {
    return `${headline} (${result.detail})`;
  }
  return headline;
}

async function openSettings() {
  try {
    const [account, theme, corePath, adminStatus, favorites, bridge, license, availability] = await Promise.all([
      invoke("get_account_settings"),
      invoke("get_theme"),
      invoke("get_core_binary_path"),
      invoke("admin_status"),
      invoke("get_favorites"),
      invoke("get_bridge_settings"),
      invoke("get_license_settings"),
      invoke("get_availability_settings"),
    ]);
    state.account = { ...state.account, ...account };
    state.adminConfigured = adminStatus.configured;
    state.adminUnlocked = adminStatus.unlocked;
    state.bridge = bridge;
    // 4R RELIABILITY fix (2026-07-18): re-fetch rather than trust whatever
    // state.availability already held - boot()'s own copy can be stale by
    // the time Settings is opened (e.g. a tray toggle that landed before
    // the availability-changed listener existed, or simply this window
    // having been backgrounded through a change made from elsewhere before
    // this fix's event wiring). Cheap (one extra command in the same
    // Promise.all batch) and matches every other field in this list, which
    // is already a fresh re-fetch on every open, not a state.* reuse.
    state.availability = { available: !!availability.available, autoAnswer: !!availability.auto_answer };

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
    renderLicenseFields(license);
    await openTranscriptionSettingsSection();
    setBoolRowUI("updater-check-on-startup-row", state.updaterCheckOnStartup);
    // renderAvailabilityUI (not just setAvailabilityFieldsUI) so the
    // titlebar dot is re-synced too, not only the Settings pane rows -
    // belt-and-suspenders alongside the availability-changed listener.
    renderAvailabilityUI();
    $("save-status").textContent = "";
    $("save-status").className = "status";
  } catch (e) {
    console.error("openSettings load failed", e);
  }
  renderUpdaterUI();
  applyLockUI();
  $("screen-settings").hidden = false;
}

/// Populates and reveals #transcription-section, or keeps it absent - see
/// that element's own comment in index.html for why "absent, hidden
/// attribute" rather than "present but greyed out" is this shell's
/// convention for an unlicensed premium surface. state.transcription.unlocked
/// is set by applyTranscriptionUI (boot + call-state events); re-read here
/// rather than re-querying premium_capability_status a second time.
async function openTranscriptionSettingsSection() {
  const section = $("transcription-section");
  if (!state.transcription.unlocked) {
    section.hidden = true;
    return;
  }
  let settings = null;
  try {
    settings = await invoke("get_transcription_settings");
  } catch (e) {
    console.error("get_transcription_settings failed", e);
  }
  if (!settings) {
    // License check raced/changed between applyTranscriptionUI and here
    // (e.g. dylib removed mid-session) - stay honest, don't show a stale
    // or half-populated section.
    section.hidden = true;
    return;
  }
  section.hidden = false;
  setTranscriptionModeUI(settings.mode);
  setTranscriptionActivationUI(settings.activation);
  setModelTierUI(settings.model_tier);
  setTranscriptionLanguageUI(settings.language);
  transcriptionKeepAudio = !!settings.keep_audio;
  transcriptionViewOnly = !!settings.view_only;
  setBoolRowUI("transcription-keep-audio-row", transcriptionKeepAudio);
  setBoolRowUI("transcription-view-only-row", transcriptionViewOnly);
  $("in-transcription-storage-dir").value = settings.storage_dir || "";
  setSttModeUI(settings.stt_mode || "local");
  setRemoteBackendUI(settings.remote_backend || "centinelo");
  $("in-remote-stt-url").value = settings.remote_url || "";
  $("in-remote-stt-model").value = settings.remote_model || "";
  // Never from `settings` (get_transcription_settings doesn't carry the key
  // at all) - only this session's own cache, see cachedRemoteApiKey's doc.
  $("in-remote-stt-key").value = cachedRemoteApiKey;
  $("remote-stt-test-status").textContent = "";
  $("remote-stt-test-status").className = "hint";
  modelStatus.accurate = null;
  modelStatus.light = null;
  // RESILIENCE #3 belt-and-suspenders: the watchdog interval (see
  // armDownloadWatchdog) already degrades a silently-dead download on its
  // own poll cycle, but a backgrounded/suspended window (macOS can throttle
  // or fully pause a hidden webview's timers) may not have ticked while
  // Settings was closed. Re-check on every reopen too, so a download that
  // went stale while this section was closed doesn't sit there reading
  // "Downloading…" until the next poll happens to land.
  for (const tier of ["accurate", "light"]) {
    if (modelDownload[tier] && isDownloadStalled(modelDownload[tier], Date.now(), DOWNLOAD_WATCHDOG_TIMEOUT_MS)) {
      clearDownloadWatchdog(tier);
      modelDownload[tier] = null;
      showBanner(t("settings.transcriptionModelDownloadStalled"), "err");
    }
  }
  renderModelStatusChip("accurate");
  renderModelStatusChip("light");
  await refreshModelStatuses();
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
  // Favorites card inside Settings (4R re-review 2026-07-16, A1): its
  // "Label"/"Extension" field labels + placeholders are built in JS via
  // t() (renderFavoritesFields), not sitting in static [data-i18n*]
  // markup, so a language switch with Settings open left them stale until
  // Settings was closed/reopened. Re-render it here too - but seeded from
  // whatever's CURRENTLY TYPED in the fields (collectFavoritesFields),
  // never from state.favorites (the main dial screen's own copy, which
  // can already differ from an in-progress, unsaved edit here): a naive
  // renderFavoritesFields(state.favorites) would silently discard
  // whatever the admin was mid-typing the moment they picked a language -
  // trading one staleness bug for a data-loss one. Only when Settings is
  // actually open - closed, #favorites-fields has nothing to relabel.
  if (!$("screen-settings").hidden) {
    renderFavoritesFields(collectFavoritesFields());
    // Same staleness class as the favorites fields above: the model
    // status chips are built in JS via t() ("Installed"/"Download"/
    // "Downloading…"), not static [data-i18n*] markup.
    if (!$("transcription-section").hidden) {
      renderModelStatusChip("accurate");
      renderModelStatusChip("light");
    }
  }
  if (!$("screen-transcript").hidden) {
    $("tr-peer-name").textContent = state.transcript ? extractUser(state.transcript.peer) || t("transcript.defaultTitle") : t("transcript.defaultTitle");
    renderTranscriptScreenBody();
  }
  renderUpdaterUI();
}

/// How long saveAccountSettings waits for a terminal reg_state
/// ("registered" / "failed") after a successful Save before giving up on the
/// inline connect-status feedback. The engine re-registers on save, so a
/// reg_state almost always follows; this just bounds the wait so the
/// "Connecting…" message can never hang forever if one doesn't arrive.
const REG_RESULT_TIMEOUT_MS = 10000;

/// Resolves with the real registration outcome the next time
/// handleSidecarEvent sees a terminal reg_state ({ state, reason }), or with
/// { timedOut: true } after REG_RESULT_TIMEOUT_MS. state.pendingRegResult is
/// the handshake: saveAccountSettings sets it, the reg_state handler clears +
/// resolves it. If the user clicks Save again while one is already pending,
/// the previous wait is settled (timedOut) first so its stale timer can't
/// fire into the new wait.
function awaitRegResult() {
  // Each Save gets a fresh generation (see reg-status.js's module header
  // for what this buys); the handshake records it so a timeout/reg_state
  // evaluated against a stale handshake reduces to ignore-stale instead of
  // resolving/repainting for the wrong Save.
  state.regGeneration += 1;
  const generation = state.regGeneration;
  if (state.pendingRegResult) {
    const stale = state.pendingRegResult;
    state.pendingRegResult = null;
    clearTimeout(stale.timer);
    stale.resolve({ timedOut: true });
  }
  return new Promise((resolve) => {
    const entry = { generation, resolve, timer: 0 };
    entry.timer = setTimeout(() => {
      const action = reduceRegHandshake(
        state.pendingRegResult === entry ? armHandshake(generation) : null,
        state.regGeneration,
        { type: "timeout" },
      );
      if (action.type === "timeout") {
        state.pendingRegResult = null;
        resolve({ timedOut: true });
      }
    }, REG_RESULT_TIMEOUT_MS);
    state.pendingRegResult = entry;
  });
}

/// Live-update #save-status for a reg_state while a Settings Save handshake
/// is active (FIX B). Keeps pill <-> save-status coherent: both read this
/// same reg_state, so they can never disagree. `registered` settles the
/// handshake (terminal success, painted green and any prior failed-retrying
/// red cleared); `failed` paints "retrying" but does NOT settle - the engine
/// auto-retries registration after a failure, so a later `registered` must be
/// able to flip #save-status green rather than contradict a permanent red.
/// No-op when no handshake is active (then the reg-pill alone is the live
/// source of truth; #save-status is just stale "save" feedback).
function renderSaveStatusForRegState(regState, reason) {
  const pending = state.pendingRegResult;
  const handshake = pending ? armHandshake(pending.generation) : null;
  const action = reduceRegHandshake(handshake, state.regGeneration, {
    type: "reg_state",
    state: regState,
    reason,
  });
  if (action.type === "ignore-stale") return;
  const statusEl = $("save-status");
  if (action.type === "show-connected") {
    state.pendingRegResult = null;
    clearTimeout(pending.timer);
    statusEl.className = "status ok";
    statusEl.textContent = t("regStatus.connected");
    pending.resolve({ state: "registered" });
  } else if (action.type === "show-failed-retrying") {
    statusEl.className = "status err";
    statusEl.textContent = t("regStatus.failedRetrying", {
      reason: action.reason || t("regStatus.unknownReason"),
    });
  } else {
    // keep-connecting: registering / unregistered / anything else non-terminal.
    statusEl.className = "status";
    statusEl.textContent = t("regStatus.connecting");
  }
}

/// Re-enable the Save button, but only if THIS save is still the newest: a
/// newer Save keeps it disabled, so an older Save's terminal path must not
/// re-enable it out from under the in-flight newer handshake. In practice
/// the button is disabled synchronously at Save start (see
/// saveAccountSettings), so a newer Save can't start until this one ends —
/// shouldReleaseSaveButton's generation check is the documented safety net
/// for that invariant, same framing as reduceRegHandshake's.
function releaseSaveButton(myGeneration) {
  if (shouldReleaseSaveButton(state.regGeneration, myGeneration)) {
    $("btn-save-settings").disabled = false;
  }
}

/// Two independent backend calls make up one "Save": save_account_settings
/// (+ set_core_binary_path + save_favorites, which all live or die
/// together with it) always runs first and reconnects the engine on
/// success; save_transcription_settings runs after, and can fail on its
/// own validation (e.g. mode=live with no storage_dir) without the account
/// half having failed at all. Structured as two nested try blocks rather
/// than one flat one specifically so those two outcomes stay distinguishable
/// (2026-07-17 4R re-review, RELIABILITY #2 - the flat version used to
/// treat a transcription-only failure as if NOTHING saved: raw backend
/// error text with no "account saved" context, and identity/secret-hint
/// left showing pre-save values even though the account was, in fact,
/// already saved and the engine already reconnected by that point).
async function saveAccountSettings() {
  // Disabled for the whole handshake below (re-enabled by every terminal
  // path via releaseSaveButton): the only call site is this button's own
  // click listener, so disabling it synchronously here, before any await,
  // closes the double-click reentrancy window - two overlapping
  // saveAccountSettings() runs would each fire their own backend save
  // calls and race to clear/repopulate #in-secret in the finally block.
  $("btn-save-settings").disabled = true;
  const statusEl = $("save-status");
  statusEl.className = "status";
  statusEl.textContent = t("settings.saving");
  const host = $("in-host").value.trim();
  const ext = $("in-ext").value.trim();
  const secret = $("in-secret").value;
  const displayName = $("in-display-name").value.trim();

  // FIX A (race: listener armed late): arm the registration-result handshake
  // BEFORE the save invoke that restarts the sidecar. save_account_settings
  // reconnects the engine on success, firing a fresh reg_state; if we armed
  // pendingRegResult only AFTER that await returned, the reg_state could land
  // in the gap and be lost, leaving #save-status stuck on "Connecting…"
  // until REG_RESULT_TIMEOUT_MS. Arm first, await later.
  const regPromise = awaitRegResult();
  // Safe to read synchronously right here (no await has run yet since
  // awaitRegResult bumped it): this Save's own generation, used to guard
  // releaseSaveButton below against a stale re-enable if a newer Save
  // somehow preempts this one before it reaches a terminal path.
  const myGeneration = state.regGeneration;

  let accountSaved = false;
  let transcriptionError = null;
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
    accountSaved = true;

    if (!$("transcription-section").hidden) {
      try {
        const remoteApiKeyValue = $("in-remote-stt-key").value;
        await invoke("save_transcription_settings", {
          input: buildSaveTranscriptionInput({
            mode: selectedTranscriptionMode,
            activation: selectedTranscriptionActivation,
            keepAudio: transcriptionKeepAudio,
            storageDir: $("in-transcription-storage-dir").value,
            viewOnly: transcriptionViewOnly,
            modelTier: selectedModelTier,
            language: selectedTranscriptionLanguage,
            sttMode: selectedSttMode,
            remoteBackend: selectedRemoteBackend,
            remoteUrl: $("in-remote-stt-url").value,
            remoteApiKey: remoteApiKeyValue,
            remoteModel: $("in-remote-stt-model").value,
          }),
        });
        // mode/activation feed maybeAutoStartTranscript/renderManualTranscribeButton
        // on the main window - keep state.transcription in sync with what
        // was just saved rather than waiting for the next
        // applyTranscriptionUI call (boot/call-state events only).
        state.transcription.mode = selectedTranscriptionMode;
        state.transcription.activation = selectedTranscriptionActivation;
        renderManualTranscribeButton();
        // Only on a CONFIRMED write - see cachedRemoteApiKey's own doc for
        // why this cache exists at all (save_transcription_settings has no
        // "blank = keep unchanged" affordance for this one field, unlike
        // the SIP secret).
        cachedRemoteApiKey = remoteApiKeyValue;
      } catch (e) {
        transcriptionError = String(e);
      }
    }
  } catch (e) {
    // Account-level failure: save_account_settings/set_core_binary_path/
    // save_favorites, in that order - nothing after this point ran,
    // including transcription. Nothing to refresh either (accountSaved
    // stays false), so this is the one path that skips the finally
    // block's identity refresh below. Also cancel the handshake armed
    // above: no re-register follows a failed save, so settle regPromise
    // (timedOut shape) and clear its timer rather than letting it dangle.
    if (state.pendingRegResult) {
      const pending = state.pendingRegResult;
      state.pendingRegResult = null;
      clearTimeout(pending.timer);
      pending.resolve({ timedOut: true });
    }
    statusEl.textContent = String(e);
    statusEl.className = "status err";
    releaseSaveButton(myGeneration);
    return;
  } finally {
    // Runs whenever save_account_settings's own try block succeeded
    // (the only early `return` above is on ITS failure) - identity/
    // secret-hint must always reflect the real server-side state at that
    // point, independent of whether the transcription save that follows
    // it also succeeded.
    if (accountSaved) {
      try {
        const account = await invoke("get_account_settings");
        state.account = account;
        renderIdentity();
        $("in-secret").value = "";
        $("secret-hint").textContent = account.secret_set ? t("settings.secretCurrentlySet") : t("settings.secretNotSet");
      } catch (e) {
        console.error("post-save get_account_settings failed", e);
      }
    }
  }

  // Reflect the REAL registration result inline (was a hardcoded optimistic
  // "Saved — reconnecting…" that gave no signal about whether the SIP account
  // actually registered). Save triggers an engine re-register, so a terminal
  // reg_state ("registered"/"failed") follows shortly — awaitRegResult
  // resolves on it via handleSidecarEvent, or times out after
  // REG_RESULT_TIMEOUT_MS. Neutral while connecting; green on success; red +
  // the real SIP reason (e.g. 401 Unauthorized / 408 Timeout) on failure.
  //
  // 2026-07-18 RELIABILITY regression fix: the "Connecting…" repaint below
  // used to be unconditional. But a terminal reg_state can land DURING the
  // invokes above (set_core_binary_path/save_favorites/save_transcription_
  // settings/the finally block's get_account_settings - IPC round-trips
  // that run concurrently with the engine's own SIP re-register), in which
  // case renderSaveStatusForRegState already painted the real outcome live
  // and resolved regPromise. Repainting "Connecting…" over that
  // unconditionally froze #save-status on a stale message the instant the
  // already-resolved regPromise resolved right after - the exact original
  // "stuck Connecting" bug, reintroduced in this timing window.
  // shouldShowInterimConnecting only allows the repaint while THIS save's
  // own handshake is still genuinely pending; reduceRegResult below then
  // always (re)asserts the true terminal text from `result` itself, so the
  // final state is never left depending on "surely it was already painted".
  if (
    shouldShowInterimConnecting(
      state.pendingRegResult ? state.pendingRegResult.generation : null,
      myGeneration,
    )
  ) {
    statusEl.className = "status";
    statusEl.textContent = t("regStatus.connecting");
  }
  const result = await regPromise;
  let regOk = false;
  const finalAction = reduceRegResult(result);
  if (finalAction.type === "show-connected") {
    regOk = true;
    // Explicit, not assumed: renderSaveStatusForRegState already painted
    // this green live in the common case, but this line is what actually
    // guarantees it - including the case the repaint above was skipped
    // for (handshake settled during the intermediate invokes).
    statusEl.className = "status ok";
    statusEl.textContent = t("regStatus.connected");
  } else {
    // keep-last (timedOut): no terminal `registered` arrived within
    // REG_RESULT_TIMEOUT_MS. Leave whatever #save-status last showed
    // ("Connecting…" if no reg_state came, or the most recent "failed —
    // retrying"). The live reg-pill keeps showing the true state, so we
    // don't clobber #save-status with a message that could contradict the
    // pill once the engine eventually registers.
  }
  // A transcription-only failure is secondary to the SIP outcome: surface it
  // only when registration itself succeeded, so a real connect failure stays
  // the headline when both go wrong (matches the two-nested-try split above).
  if (transcriptionError && regOk) {
    statusEl.textContent = t("settings.savedAccountTranscriptionFailed", { reason: transcriptionError });
    statusEl.className = "status err";
  }
  // Terminal: registered, failed-then-timed-out, or never-registered-timed-
  // out all end the wait here (result is either {state:"registered"} or
  // {timedOut:true} - both settle regPromise, see awaitRegResult/
  // renderSaveStatusForRegState). A newer Save preempting this one instead
  // (see awaitRegResult's own stale-cancel) also resolves regPromise with
  // {timedOut:true} and reaches this same line - releaseSaveButton's
  // generation guard is what keeps that case from re-enabling a button the
  // newer Save has since disabled again.
  releaseSaveButton(myGeneration);
}

// ---------------------------------------------------------------------------
// wiring
// ---------------------------------------------------------------------------
function wireStaticHandlers() {
  $("btn-minimize").addEventListener("click", () => win.minimize());
  $("btn-close").addEventListener("click", () => win.hide());
  $("btn-settings").addEventListener("click", openSettings);
  $("settings-back").addEventListener("click", closeSettings);
  // Availability indicator doubles as a toggle (shell task) - same
  // set_available command + optimistic-then-reconciled-by-render pattern
  // the Settings pane's own available-row button uses below.
  $("btn-availability").addEventListener("click", async () => {
    const next = !state.availability.available;
    try {
      await invoke("set_available", { available: next });
      state.availability.available = next;
      renderAvailabilityUI();
    } catch (e) {
      showBanner(String(e), "err");
    }
  });
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

  // ---- transcription settings (Plate 08) - all deferred to the "Save"
  // button except the model download, which is its own real backend action
  // (download_transcription_model), not a settings mutation - fires the
  // moment a not-yet-installed tier is picked, same "act immediately"
  // reasoning as #btn-restart-engine below. ----------------------------
  document.querySelectorAll("#transcription-mode-choice .tcard").forEach((card) => {
    card.addEventListener("click", () => setTranscriptionModeUI(card.dataset.transcriptionMode));
  });
  document.querySelectorAll("#transcription-activation-choice .tcard").forEach((card) => {
    card.addEventListener("click", () => setTranscriptionActivationUI(card.dataset.transcriptionActivation));
  });
  document.querySelectorAll("#transcription-language-row button").forEach((b) => {
    b.addEventListener("click", () => setTranscriptionLanguageUI(b.dataset.languageChoice));
  });
  document.querySelectorAll("#transcription-keep-audio-row button").forEach((b) => {
    b.addEventListener("click", () => {
      transcriptionKeepAudio = b.dataset.boolChoice === "true";
      setBoolRowUI("transcription-keep-audio-row", transcriptionKeepAudio);
    });
  });
  document.querySelectorAll("#transcription-view-only-row button").forEach((b) => {
    b.addEventListener("click", () => {
      transcriptionViewOnly = b.dataset.boolChoice === "true";
      setBoolRowUI("transcription-view-only-row", transcriptionViewOnly);
    });
  });
  $("transcription-model-choice").addEventListener("click", (e) => {
    const downloadBtn = e.target.closest("[data-download-tier]");
    if (downloadBtn) {
      startModelDownload(downloadBtn.dataset.downloadTier);
      return; // don't also treat this as a row-selection click
    }
    const retryBtn = e.target.closest("[data-retry-status-tier]");
    if (retryBtn) {
      retryModelStatus(retryBtn.dataset.retryStatusTier);
      return; // don't also treat this as a row-selection click
    }
    const row = e.target.closest(".modelrow");
    if (row) setModelTierUI(row.dataset.modelTier);
  });
  $("transcription-model-choice").addEventListener("keydown", (e) => {
    if (e.key !== "Enter" && e.key !== " ") return;
    const row = e.target.closest(".modelrow");
    if (!row) return;
    e.preventDefault();
    setModelTierUI(row.dataset.modelTier);
  });

  // ---- remote STT (P6) - mode/backend selection deferred to "Save" like
  // every other #transcription-section control above; "Test connection" is
  // the one immediate action, same pattern as #btn-activate-license.
  document.querySelectorAll("#stt-mode-row button").forEach((b) => {
    b.addEventListener("click", () => setSttModeUI(b.dataset.sttModeChoice));
  });
  document.querySelectorAll("#remote-stt-backend-row button").forEach((b) => {
    b.addEventListener("click", () => setRemoteBackendUI(b.dataset.remoteBackendChoice));
  });
  $("btn-test-remote-stt").addEventListener("click", async () => {
    const statusEl = $("remote-stt-test-status");
    const btn = $("btn-test-remote-stt");
    const remoteUrl = $("in-remote-stt-url").value.trim();
    const remoteApiKey = $("in-remote-stt-key").value;
    btn.disabled = true;
    statusEl.textContent = t("settings.remoteSttTesting");
    statusEl.className = "hint";
    try {
      const result = await invoke("test_remote_stt_connection", {
        input: {
          remote_url: remoteUrl,
          remote_backend: selectedRemoteBackend,
          remote_api_key: remoteApiKey.length ? remoteApiKey : null,
        },
      });
      statusEl.textContent = remoteSttProbeText(result);
      statusEl.className = result.ok ? "hint ok" : "hint err";
    } catch (e) {
      statusEl.textContent = String(e);
      statusEl.className = "hint err";
    } finally {
      btn.disabled = false;
    }
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

  $("btn-activate-license").addEventListener("click", async () => {
    const serial = $("in-license-serial").value.trim();
    const serverUrl = $("in-license-server-url").value.trim();
    const statusEl = $("license-activate-status");
    const btn = $("btn-activate-license");
    if (!serial) {
      statusEl.textContent = t("settings.licenseSerialRequired");
      statusEl.className = "hint err";
      return;
    }
    btn.disabled = true;
    statusEl.textContent = t("settings.licenseActivating");
    statusEl.className = "hint";
    try {
      const outcome = await invoke("activate_license", { serial, server_url: serverUrl });
      $("in-license-serial").value = "";
      statusEl.textContent = t("settings.licenseActivatedStatus", { customer: outcome.customer });
      statusEl.className = "hint ok";
      $("license-status-hint").textContent = t("settings.licenseAlreadyPresentHint");
    } catch (e) {
      statusEl.textContent = activationErrorText(e);
      statusEl.className = "hint err";
    } finally {
      btn.disabled = false;
    }
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

  // ---- availability / auto-answer (shell task) ------------------------
  // Immediate-save bool rows, same shape as updater-check-on-startup-row
  // just below - not admin-gated (see index.html's own comment on this
  // section). set_available/set_auto_answer also push the tray's own
  // checkmarks back in sync (tray::sync_availability_menu) and reapply the
  // effective answer mode - none of that needs anything further from here,
  // this handler only owns the DOM + state.availability half.
  //
  // 4R RESILIENCE fix (2026-07-18): setBoolRowUI paints the CLICKED value
  // optimistically before the invoke() even starts (so the row feels
  // instant) - the catch used to only show an error banner, leaving the
  // row painted on the failed value while state.availability (and the
  // titlebar dot, and the engine itself) were all still on the OLD one.
  // Reverting via renderAvailabilityUI()/setAvailabilityFieldsUI() on
  // failure repaints every row from the last known-good state.availability
  // (never touched on the failure path), so a rejected change snaps
  // straight back instead of lying until the next unrelated repaint.
  document.querySelectorAll("#available-row button").forEach((b) => {
    b.addEventListener("click", async () => {
      const value = b.dataset.boolChoice === "true";
      setBoolRowUI("available-row", value);
      try {
        await invoke("set_available", { available: value });
        state.availability.available = value;
        renderAvailabilityUI();
      } catch (e) {
        showBanner(String(e), "err");
        renderAvailabilityUI(); // revert available-row (and the titlebar dot) to the real, unchanged state.availability
      }
    });
  });
  document.querySelectorAll("#auto-answer-row button").forEach((b) => {
    b.addEventListener("click", async () => {
      const value = b.dataset.boolChoice === "true";
      setBoolRowUI("auto-answer-row", value);
      try {
        await invoke("set_auto_answer", { auto_answer: value });
        state.availability.autoAnswer = value;
      } catch (e) {
        showBanner(String(e), "err");
        setAvailabilityFieldsUI(); // revert auto-answer-row to the real, unchanged state.availability
      }
    });
  });

  // ---- auto-updater (roadmap debt fix) -------------------------------
  // Settings > About's "Check for updates" button + the check_on_startup
  // toggle (immediate-save, same shape as auto-dial-row/tel-handler-row
  // just above) + the main-window banner's own buttons (available ->
  // Download/Later, ready -> Restart/Later, error -> Retry/Later). See
  // updater.js's header comment for the full flow these call into.
  $("btn-check-updates").addEventListener("click", () => {
    performUpdateCheck().catch((e) => console.error("manual update check failed", e));
  });
  document.querySelectorAll("#updater-check-on-startup-row button").forEach((b) => {
    b.addEventListener("click", async () => {
      const value = b.dataset.boolChoice === "true";
      setBoolRowUI("updater-check-on-startup-row", value);
      try {
        await invoke("set_updater_check_on_startup", { check_on_startup: value });
        state.updaterCheckOnStartup = value;
      } catch (e) {
        showBanner(String(e), "err");
      }
    });
  });
  $("btn-update-download").addEventListener("click", () => {
    startUpdateDownload().catch((e) => console.error("update download failed", e));
  });
  // Shared by the `ready` phase's own "Restart to update" AND the
  // `errorOrigin: "restart"` error state's same-labeled button
  // (renderUpdateBanner shows exactly one of the two at a time) - the
  // former is a fresh install attempt, the latter only retries the
  // relaunch step (2026-07-17 4R re-review, RELIABILITY M3: install()
  // already succeeded by the time errorOrigin is "restart", re-calling
  // startUpdateInstall - which re-calls updaterInstall - would fail
  // instantly against an already-closed bytes resource).
  $("btn-update-restart").addEventListener("click", () => {
    if (state.updater.phase === "error" && state.updater.errorOrigin === "restart") {
      retryRestartOnly().catch((e) => console.error("relaunch retry failed", e));
    } else {
      startUpdateInstall().catch((e) => console.error("update install failed", e));
    }
  });
  $("btn-update-retry").addEventListener("click", () => {
    // Only ever visible for errorOrigin "download" or "install"
    // (renderUpdateBanner routes "restart" to the restart button above
    // instead, with its own honest copy - see updater.js's header
    // comment) - branch on the real origin rather than inferring it from
    // pendingDownloadedBytesRid's presence, more direct and correct.
    if (state.updater.errorOrigin === "install") {
      startUpdateInstall().catch((e) => console.error("update install retry failed", e));
    } else {
      startUpdateDownload().catch((e) => console.error("update download retry failed", e));
    }
  });
  $("btn-update-later").addEventListener("click", () => {
    state.updater = withDismissed(state.updater);
    renderUpdaterUI();
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
  // Model manager (Settings → Transcription's model rows) - see
  // `openSettings`/`startModelDownload`. Wired unconditionally like every
  // other listener here (not gated on Settings being open): a download
  // kicked off just before Settings was closed should still finish and
  // update `modelDownload`/`modelStatus` quietly, ready to render
  // correctly the next time Settings opens.
  await listen("transcription://model-download-progress", (e) => handleModelDownloadProgress(e.payload));
  await listen("transcription://model-download-done", (e) => handleModelDownloadSettled(e.payload, null));
  await listen("transcription://model-download-error", (e) => handleModelDownloadSettled(e.payload, e.payload && e.payload.message));
  // Auto-provisioning deep link (provisioning.rs handle_deep_link, wired
  // from deeplink.rs) - same preview shape provisioning_resolve returns
  // directly for the paste flow, so one confirmation screen serves both.
  await listen("provisioning://preview", (e) => showProvisioningConfirm(e.payload));
  await listen("provisioning://error", (e) => {
    const message = e.payload && e.payload.message ? e.payload.message : String(e.payload);
    showBanner(t("provisioning.linkError", { message }), "err");
  });
  // Availability/auto-answer (4R RELIABILITY fix, 2026-07-18): the tray
  // menu's own "Available"/"Auto-answer" checkboxes change this preference
  // WITHOUT going through invoke() at all (tray.rs's click handler calls
  // settings::update_available/update_auto_answer directly) - before this
  // listener existed, a tray-originated change never reached this window,
  // so the titlebar dot and Settings pane bool rows kept showing the last
  // value from boot/the last invoke() round-trip while the engine's real
  // answer-mode/auto-reject behavior had already changed. tray.rs emits
  // this from `sync_availability_menu`, the one point every route (tray
  // clicks AND set_available/set_auto_answer) funnels through, so this
  // single listener covers all of them - including a self-triggered one
  // from this same window's own invoke() call, which is harmless
  // (identical values, renderAvailabilityUI is idempotent).
  await listen("availability-changed", (e) => {
    const payload = e.payload || {};
    state.availability = { available: !!payload.available, autoAnswer: !!payload.auto_answer };
    renderAvailabilityUI();
  });
}

// Premium receptionist console entry point - hidden unless the license
// gate cleared. "Available" or "not_implemented" both mean "offer it"
// (see console.rs::unlocks_console's doc for why NotImplemented, v0's
// actual answer for a licensed capability, counts here); "not_licensed"/
// "unavailable" (no dylib, tampered signature, FFI error) hide it -
// mirrors tray.rs's own gating exactly, so both surfaces agree.
/// Applies the BLF master switch to the DOM. Pure policy lives in
/// computeBlfUiHidden (blf-ui.js): with blfEnabled === false the favorites grid
/// + heading disappear and the console entry is absent from view, REGARDLESS of
/// its own license gate. Called both from boot() (after get_blf_enabled) and
/// applyPremiumUI() (when the console's own license gate resolves), so either
/// signal re-evaluates both surfaces. id names come from BLF_UI_TARGETS so the
/// contract is pinned in one testable place.
function renderBlfUi() {
  const hidden = computeBlfUiHidden({ blfEnabled: state.blfEnabled, consoleUnlocked: state.consoleUnlocked });
  $(BLF_UI_TARGETS.favoritesHeading).hidden = hidden.favoritesHeading;
  $(BLF_UI_TARGETS.favoritesGrid).hidden = hidden.favoritesGrid;
  $(BLF_UI_TARGETS.console).hidden = hidden.console;
}

async function applyPremiumUI() {
  try {
    const status = await invoke("premium_capability_status", { capability: "blf_console" });
    state.consoleUnlocked = status === "available" || status === "not_implemented";
  } catch (e) {
    console.error("premium_capability_status failed", e);
    state.consoleUnlocked = false;
  }
  // Re-applied through renderBlfUi so the console respects BOTH its license
  // gate (state.consoleUnlocked) and the BLF master switch (state.blfEnabled):
  // BLF off hides the console even when the license would clear it.
  renderBlfUi();
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
    const [account, favorites, theme, sidecarStatus, blfStates, blfEnabled, availability] = await Promise.all([
      invoke("get_account_settings"),
      invoke("get_favorites"),
      invoke("get_theme"),
      invoke("sidecar_status"),
      invoke("get_blf_states"),
      invoke("get_blf_enabled"),
      invoke("get_availability_settings"),
    ]);
    state.account = account;
    state.favorites = favorites;
    state.sidecarStatus = sidecarStatus;
    state.blf = blfStates || {};
    // P5 master switch: blfEnabled gates both the favorites grid/heading and the
    // console entry (see renderBlfUi). Read here alongside the other boot reads
    // so the gate applies on the first paint; applyPremiumUI re-applies it once
    // the console's own license gate resolves further down boot().
    state.blfEnabled = blfEnabled === true;
    if (availability) {
      state.availability = { available: !!availability.available, autoAnswer: !!availability.auto_answer };
    }
    applyTheme(theme || "auto");
  } catch (e) {
    console.error("boot load failed", e);
  }

  // Auto-updater (roadmap debt fix) - version + the check_on_startup
  // preference, both needed before the startup check itself can run.
  // Best-effort/non-fatal like everything else in boot(): getVersion()
  // failing (shouldn't, it's a trivial core:app command) just leaves the
  // About section's version line blank rather than blocking boot.
  try {
    const [currentVersion, updaterSettings] = await Promise.all([getVersion(), invoke("get_updater_settings")]);
    state.updater.currentVersion = currentVersion;
    state.updaterCheckOnStartup = !!(updaterSettings && updaterSettings.check_on_startup);
  } catch (e) {
    console.error("updater boot load failed", e);
  }
  renderUpdaterUI();

  renderIdentity();
  renderDial();
  renderFavorites();
  // Apply the BLF master switch to the DOM now that state.blfEnabled has been
  // read; applyPremiumUI() below re-applies it once the console license gate
  // resolves (both touch renderBlfUi, so order-independent).
  renderBlfUi();
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

  // Fire-and-forget, deliberately last and deliberately not awaited - see
  // maybeCheckForUpdatesOnStartup's own doc for why boot() must not block
  // on a network round trip here.
  maybeCheckForUpdatesOnStartup();
  scheduleUpdatePeriodicRecheck();
}

boot();

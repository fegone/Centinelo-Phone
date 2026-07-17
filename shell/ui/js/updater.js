// Auto-updater — pure state machine + rendering (roadmap debt fix: every
// build used to require a manual reinstall, see shell/README.md
// "Auto-updater").
//
// Same "zero Tauri dependency" shape transcript-panel.js documents for
// itself: nothing here touches `window.__TAURI__`, `document` at module
// scope, or a mutable module-level singleton — app.js owns all
// `@tauri-apps/plugin-updater` / `@tauri-apps/plugin-process` wiring and
// hands this module's reducers the events it gets back; `dev/updater-
// mock.html` hands the render functions a fabricated state for visual
// verification via a headless Browser pane (this project's standing
// alternative to desktop GUI automation — see shell-tauri's own rule
// against automating the real app window).
//
// Two-step download-then-install (not the plugin's own combined
// `downloadAndInstall`) is a deliberate product choice, not an API
// limitation: this is a softphone, and installing an update restarts the
// process. `download()` alone never touches the running app or an active
// call — always safe to kick off the moment an update is found.
// `install()` is the one disruptive step, gated by `canStartInstall()`
// below on there being no active call *at the moment the operator clicks
// "Restart to update"* — app.js re-checks `state.call` right there,
// immediately before calling `install()`, not merely when the update was
// first found (a call can start or end in between). This closes most of
// the race, though not all of it — see shell/README.md "Auto-updater" for
// the one window this still can't close (a call starting during the
// install() call itself) and why that's an accepted, documented gap
// rather than a silent one.
//
// state shape:
// {
//   phase: "idle" | "checking" | "up_to_date" | "available" | "downloading"
//        | "ready" | "installing" | "error",
//   version: string | null,       // the update's version, once known
//   notes: string | null,         // release notes body, once known
//   pubDate: string | null,
//   currentVersion: string | null, // this app's own version (getVersion())
//   downloadedBytes: number,
//   totalBytes: number | null,    // null = indeterminate progress
//   errorMessage: string | null,
//   errorOrigin: "check" | "download" | "install" | "restart" | null,
//   dismissed: boolean,           // banner hidden (Settings status is unaffected)
// }
//
// `errorOrigin: "restart"` (2026-07-17 4R re-review, RELIABILITY M3) is
// deliberately distinct from `"install"`: on macOS/Linux, `install()`
// itself can succeed (the update IS on disk, safely) and it's the
// FOLLOW-UP `relaunch()` call that fails - a real, if rare, possibility
// (OS refuses the relaunch, race with something else quitting the app,
// ...). Reaching that point with plain `"install"` would (a) show a
// scary "Update failed" when the update actually succeeded, and (b) let
// Retry re-call `install()` with an already-closed bytes resource
// (app.js's `updaterInstall`/Rust's `updater_install` close it on
// success) - an instant, confusing failure on an update that's already
// done. `withRestartError` keeps the honest "installed, just restart"
// language and app.js's own retry handler for this origin only ever
// re-attempts the relaunch step, never `install()` again.

export function initialUpdaterState() {
  return {
    phase: "idle",
    version: null,
    notes: null,
    pubDate: null,
    currentVersion: null,
    downloadedBytes: 0,
    totalBytes: null,
    errorMessage: null,
    errorOrigin: null,
    dismissed: false,
  };
}

export function withChecking(state) {
  return { ...state, phase: "checking", errorMessage: null, errorOrigin: null };
}

export function withUpToDate(state, currentVersion) {
  return {
    ...state,
    phase: "up_to_date",
    currentVersion: currentVersion ?? state.currentVersion,
    errorMessage: null,
    errorOrigin: null,
  };
}

/// A fresh "available" result always clears `dismissed` — a user who
/// clicked Later on an older check (or the same one, re-run by hand)
/// expects the banner back once a real new check confirms an update is
/// still there.
export function withAvailable(state, { version, notes, pubDate, currentVersion } = {}) {
  return {
    ...state,
    phase: "available",
    version: version ?? null,
    notes: notes ?? null,
    pubDate: pubDate ?? null,
    currentVersion: currentVersion ?? state.currentVersion,
    downloadedBytes: 0,
    totalBytes: null,
    errorMessage: null,
    errorOrigin: null,
    dismissed: false,
  };
}

/// Check failures are never user-initiated (the automatic startup check
/// never asked permission, and even a manual "Check for updates" click
/// only ever expects "found"/"not found"/"couldn't check", not a
/// disruptive banner) — `shouldShowBanner` below reads `errorOrigin` to
/// keep this phase in Settings-only territory. See this module's header
/// comment for the full "why".
export function withCheckError(state, message) {
  return { ...state, phase: "error", errorMessage: message || "", errorOrigin: "check" };
}

export function withDownloadStarted(state) {
  return { ...state, phase: "downloading", downloadedBytes: 0, totalBytes: null, errorMessage: null, errorOrigin: null };
}

/// Stale-event guard, same shape transcription-settings.js's own
/// `applyProgressEvent` documents: a progress event that arrives after the
/// phase has already moved on (settled, or a fresh check reset it) must
/// not revive a ghost progress bar. Negative bytes clamp to 0 for the same
/// reason that file's own doc gives — a wire/parsing bug upstream is
/// better shown as 0% than a negative-width bar.
export function withDownloadProgress(state, { downloadedBytes, totalBytes } = {}) {
  if (state.phase !== "downloading") return state;
  return {
    ...state,
    downloadedBytes: Math.max(0, downloadedBytes ?? state.downloadedBytes),
    totalBytes: totalBytes ?? state.totalBytes,
  };
}

export function withDownloadError(state, message) {
  return { ...state, phase: "error", errorMessage: message || "", errorOrigin: "download" };
}

export function withReady(state) {
  return { ...state, phase: "ready", errorMessage: null, errorOrigin: null };
}

export function withInstalling(state) {
  return { ...state, phase: "installing", errorMessage: null, errorOrigin: null };
}

export function withInstallError(state, message) {
  return { ...state, phase: "error", errorMessage: message || "", errorOrigin: "install" };
}

/// See this module's header comment ("errorOrigin: restart") for why this
/// is a separate transition from `withInstallError` - reached only AFTER
/// `install()` itself has already succeeded, when the follow-up
/// `relaunch()` call is what failed.
export function withRestartError(state, message) {
  return { ...state, phase: "error", errorMessage: message || "", errorOrigin: "restart" };
}

export function withDismissed(state) {
  return { ...state, dismissed: true };
}

/// Which resource ids need closing given the current pending-update
/// bookkeeping, and actually closes them via the injected `closeFn`
/// (real caller: `(rid) => invoke("plugin:resources|close", {rid})`) -
/// dependency-injected specifically so this contract is unit-testable
/// with a counting mock (2026-07-17 4R re-review, RELIABILITY M2): the
/// previous version of this cleanup (app.js, inline) only ever closed
/// `pendingUpdateMeta`'s own resource, silently leaking the downloaded-
/// bytes resource (tens of MB) on every download-then-abandon ("Later"
/// after a download finished) or download-then-recheck cycle, for the
/// life of the process. Each close is attempted independently - one
/// failing (already closed, e.g.) never stops the other from being
/// attempted, matching this codebase's existing "best-effort cleanup
/// shouldn't itself become a new failure mode" convention (see
/// settings.rs's orphaned-tmp-file cleanup).
export async function closePendingUpdateResources(pendingUpdateMeta, pendingDownloadedBytesRid, closeFn) {
  const ids = [];
  if (pendingUpdateMeta && pendingUpdateMeta.rid != null) ids.push(pendingUpdateMeta.rid);
  if (pendingDownloadedBytesRid != null) ids.push(pendingDownloadedBytesRid);
  await Promise.all(
    ids.map((rid) =>
      Promise.resolve()
        .then(() => closeFn(rid))
        .catch(() => {})
    )
  );
}

/// The one safety gate before the disruptive step (`install()` + relaunch)
/// — see this module's header comment for the full reasoning. Pure and
/// trivial on purpose: the actual "is there a call right now" read lives
/// in app.js's `state.call`, this function only names the decision so it
/// has one documented, tested definition instead of an inline `if` at the
/// call site.
export function canStartInstall(hasActiveCall) {
  return !hasActiveCall;
}

/// Which phases are worth surfacing in the main window, outside Settings.
/// "up_to_date" and a check-originated "error" are deliberately excluded —
/// a silent background check (startup, or an offline laptop) must never
/// pop an intrusive banner; those two phases are Settings-only status text
/// (see app.js's `renderUpdaterAboutStatus`, which renders every phase
/// unconditionally — "no UI muerta" per the task brief). A download/install
/// error IS shown here: by the time either can happen the operator already
/// opted in by clicking Download/Restart, so silence would hide feedback
/// on an action they just took.
const BANNER_PHASES = new Set(["available", "downloading", "ready", "installing"]);

export function shouldShowBanner(state) {
  if (state.dismissed) return false;
  if (state.phase === "error") return state.errorOrigin !== "check";
  return BANNER_PHASES.has(state.phase);
}

/// 0-100 clamped, or `null` while `totalBytes` hasn't arrived yet (an
/// indeterminate download, same convention transcription-settings.js's own
/// `computeDownloadPct` uses for the model-download progress bar — no
/// second implementation of this needed, app.js imports that one directly
/// for updater progress too; not re-exported from here to keep this
/// module's surface to "the update state machine" only).

// ---------------------------------------------------------------------------
// rendering — pure DOM writes into an already-mounted `#update-banner`
// container (index.html), same "container + state + handlers" shape
// transcript-panel.js's renderTranscriptBody uses. Text content only
// (textContent, never innerHTML from a dynamic value) — `version`/
// `errorMessage` both ultimately come from a network response (the
// manifest's own JSON, or a plugin error message), so neither is treated
// as markup-safe even though the manifest itself is signature-verified
// before anything downloads.
// ---------------------------------------------------------------------------

/// `refs` is `{ root, title, detail, actions, downloadBtn, restartBtn,
/// retryBtn, laterBtn, progressWrap, progressFill, progressPct }` — every
/// element this function touches, queried once by the caller
/// (app.js's getUpdaterRefs / dev/updater-mock.html) rather than
/// re-queried on every render. The banner's icon (index.html) is static
/// markup, not part of this — nothing here ever changes it.
export function renderUpdateBanner(refs, state, t, { formatBytes, computeDownloadPct, canInstallNow } = {}) {
  const show = shouldShowBanner(state);
  refs.root.hidden = !show;
  if (!show) return;

  refs.progressWrap.hidden = true;
  refs.actions.hidden = false;
  refs.downloadBtn.hidden = true;
  refs.restartBtn.hidden = true;
  refs.retryBtn.hidden = true;
  refs.restartBtn.disabled = false;
  refs.restartBtn.removeAttribute("title");

  if (state.phase === "available") {
    refs.title.textContent = t("updater.bannerAvailableTitle");
    refs.detail.textContent = t("updater.bannerAvailableDetail", { version: state.version || "" });
    refs.downloadBtn.hidden = false;
  } else if (state.phase === "downloading") {
    refs.title.textContent = t("updater.bannerDownloadingTitle");
    refs.detail.textContent = "";
    refs.actions.hidden = true;
    refs.progressWrap.hidden = false;
    renderProgress(refs, state, formatBytes, computeDownloadPct);
  } else if (state.phase === "ready") {
    refs.title.textContent = t("updater.bannerReadyTitle");
    refs.detail.textContent = t("updater.bannerReadyDetail", { version: state.version || "" });
    refs.restartBtn.hidden = false;
    const canInstall = typeof canInstallNow === "function" ? canInstallNow() : true;
    if (!canInstall) {
      refs.restartBtn.disabled = true;
      refs.restartBtn.title = t("updater.finishCallFirstTitle");
    }
  } else if (state.phase === "installing") {
    refs.title.textContent = t("updater.bannerInstallingTitle");
    refs.detail.textContent = "";
    refs.actions.hidden = true;
  } else if (state.phase === "error" && state.errorOrigin === "restart") {
    // The update itself is already safely installed - only the automatic
    // relaunch failed. Honest, calm copy (never "Update failed") + the
    // SAME "Restart to update" action as the `ready` phase, not "Retry" -
    // there's nothing to retry-download-or-reinstall, just restart. See
    // this module's header comment ("errorOrigin: restart").
    refs.title.textContent = t("updater.bannerRestartFailedTitle");
    refs.detail.textContent = t("updater.bannerRestartFailedDetail", { version: state.version || "" });
    refs.restartBtn.hidden = false;
    const canInstall = typeof canInstallNow === "function" ? canInstallNow() : true;
    if (!canInstall) {
      refs.restartBtn.disabled = true;
      refs.restartBtn.title = t("updater.finishCallFirstTitle");
    }
  } else if (state.phase === "error") {
    refs.title.textContent = t("updater.bannerErrorTitle");
    refs.detail.textContent = state.errorMessage || "";
    refs.retryBtn.hidden = false;
  }
}

function renderProgress(refs, state, formatBytes, computeDownloadPct) {
  const pct = computeDownloadPct ? computeDownloadPct({ downloadedBytes: state.downloadedBytes, totalBytes: state.totalBytes }) : null;
  refs.progressFill.style.width = `${pct ?? 0}%`;
  if (pct != null) {
    refs.progressPct.textContent = `${pct}%`;
  } else if (formatBytes) {
    refs.progressPct.textContent = formatBytes(state.downloadedBytes);
  } else {
    refs.progressPct.textContent = "";
  }
}

/// Settings > About's status line — renders every phase unconditionally
/// (unlike the banner above), so opening Settings after a silent startup
/// check failure still shows the truth instead of a blank "not checked
/// yet" (task brief: "NO UI muerta"). `statusEl` gets the text; `checkBtn`
/// is disabled/relabeled only while a check is actually in flight (never
/// while downloading/installing — those don't block a fresh check being
/// requested, though app.js's own click handler is free to no-op that in
/// practice; this function only owns rendering, not that policy). The
/// app's own current version is a SEPARATE, always-shown element
/// (`#updater-current-version`, app.js's own renderUpdaterUI) — this
/// function's "idle" case (phase never checked yet) leaves `statusEl`
/// blank rather than duplicating that line here.
const CHECK_BUTTON_BUSY_PHASES = new Set(["checking", "downloading", "installing"]);

export function renderUpdaterAboutStatus(statusEl, checkBtn, state, t, { computeDownloadPct } = {}) {
  // Disabled for the same phases app.js's own updateCheckInFlight() reentrancy
  // guard checks - a re-check must never race an in-flight download/install
  // (see that function's doc for the exact failure mode this prevents).
  checkBtn.disabled = CHECK_BUTTON_BUSY_PHASES.has(state.phase);
  checkBtn.textContent = state.phase === "checking" ? t("updater.checking") : t("updater.checkButton");

  switch (state.phase) {
    case "checking":
      statusEl.textContent = t("updater.checking");
      break;
    case "up_to_date":
      statusEl.textContent = t("updater.upToDate");
      break;
    case "available":
      statusEl.textContent = t("updater.aboutAvailable", { version: state.version || "" });
      break;
    case "downloading": {
      // Wired to the real percentage when known (2026-07-17 4R re-review,
      // minor #5 - `updater.aboutDownloading`'s {pct} placeholder used to
      // be dead, never referenced from anywhere). `computeDownloadPct` is
      // injected the same way `renderUpdateBanner` already takes it - this
      // function has no import of its own (kept Tauri/format-library-free,
      // same reasoning as this whole module's header comment).
      const pct = computeDownloadPct ? computeDownloadPct({ downloadedBytes: state.downloadedBytes, totalBytes: state.totalBytes }) : null;
      statusEl.textContent = pct != null ? t("updater.aboutDownloading", { pct }) : t("updater.aboutDownloadingIndeterminate");
      break;
    }
    case "ready":
      statusEl.textContent = t("updater.aboutReady", { version: state.version || "" });
      break;
    case "installing":
      statusEl.textContent = t("updater.installing");
      break;
    case "error":
      // See this module's header comment ("errorOrigin: restart") - the
      // update is already installed, only the relaunch failed, so this
      // reads "installed, restart to finish" rather than "couldn't
      // update" for that one origin specifically.
      statusEl.textContent =
        state.errorOrigin === "restart"
          ? t("updater.restartFailedStatus", { version: state.version || "" })
          : t("updater.errorStatus", { message: state.errorMessage || "" });
      break;
    default:
      statusEl.textContent = "";
  }
}

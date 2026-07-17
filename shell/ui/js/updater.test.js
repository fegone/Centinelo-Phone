// Tests for updater.js's pure state machine (roadmap debt fix - auto-
// updater). Same node:test style as transcription-settings.test.js;
// rendering (renderUpdateBanner/renderUpdaterAboutStatus) is verified
// visually via dev/updater-mock.html + a headless Browser pane instead of
// jsdom, matching transcript-panel.js's own precedent (see that module's
// header comment and shell/README.md "Auto-updater").

import { test } from "node:test";
import assert from "node:assert/strict";
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
  shouldShowBanner,
  closePendingUpdateResources,
} from "./updater.js";

// ---------------------------------------------------------------------
// initial state
// ---------------------------------------------------------------------

test("initialUpdaterState: starts idle, nothing dismissed, no error", () => {
  const s = initialUpdaterState();
  assert.equal(s.phase, "idle");
  assert.equal(s.version, null);
  assert.equal(s.dismissed, false);
  assert.equal(s.errorMessage, null);
  assert.equal(s.errorOrigin, null);
});

// ---------------------------------------------------------------------
// the full happy path: idle -> checking -> available -> downloading ->
// ready -> installing (then app.js calls relaunch(), no further state).
// ---------------------------------------------------------------------

test("happy path: check finds an update, downloads, and is ready to install", () => {
  let s = initialUpdaterState();

  s = withChecking(s);
  assert.equal(s.phase, "checking");

  s = withAvailable(s, { version: "2.1.0", notes: "Bug fixes", pubDate: "2026-07-17T00:00:00Z" });
  assert.equal(s.phase, "available");
  assert.equal(s.version, "2.1.0");
  assert.equal(s.notes, "Bug fixes");
  assert.equal(s.dismissed, false);

  s = withDownloadStarted(s);
  assert.equal(s.phase, "downloading");
  assert.equal(s.downloadedBytes, 0);
  assert.equal(s.totalBytes, null);

  s = withDownloadProgress(s, { downloadedBytes: 5_000_000, totalBytes: 20_000_000 });
  assert.equal(s.downloadedBytes, 5_000_000);
  assert.equal(s.totalBytes, 20_000_000);

  s = withReady(s);
  assert.equal(s.phase, "ready");
  // version/notes survive the download -> ready transition (still needed
  // to render "Update ready — v2.1.0").
  assert.equal(s.version, "2.1.0");

  s = withInstalling(s);
  assert.equal(s.phase, "installing");
});

test("up-to-date path: check finds nothing, phase settles quietly", () => {
  let s = initialUpdaterState();
  s = withChecking(s);
  s = withUpToDate(s, "2.0.0");
  assert.equal(s.phase, "up_to_date");
  assert.equal(s.currentVersion, "2.0.0");
});

// ---------------------------------------------------------------------
// withAvailable always un-dismisses (a re-check after "Later" must
// re-surface the banner if the update is still there).
// ---------------------------------------------------------------------

test("withAvailable: a fresh result clears a previous dismissal", () => {
  let s = initialUpdaterState();
  s = withAvailable(s, { version: "2.1.0" });
  s = withDismissed(s);
  assert.equal(s.dismissed, true);

  s = withAvailable(s, { version: "2.1.0" });
  assert.equal(s.dismissed, false);
});

// ---------------------------------------------------------------------
// withDownloadProgress: stale-event guard (mirrors transcription-
// settings.js's applyProgressEvent contract exactly - a progress event
// after the phase already moved on must not revive a ghost bar).
// ---------------------------------------------------------------------

test("withDownloadProgress: ignored (same reference back) when not in the downloading phase", () => {
  const s = initialUpdaterState(); // phase: idle
  const result = withDownloadProgress(s, { downloadedBytes: 999, totalBytes: 1000 });
  assert.equal(result, s);
});

test("withDownloadProgress: a straggler after settling to 'ready' is also ignored", () => {
  let s = initialUpdaterState();
  s = withAvailable(s, { version: "2.1.0" });
  s = withDownloadStarted(s);
  s = withReady(s); // settled - download finished
  const result = withDownloadProgress(s, { downloadedBytes: 1, totalBytes: 2 });
  assert.equal(result, s);
});

test("withDownloadProgress: clamps a negative downloaded_bytes to 0", () => {
  let s = initialUpdaterState();
  s = withDownloadStarted(s);
  s = withDownloadProgress(s, { downloadedBytes: -5, totalBytes: 100 });
  assert.equal(s.downloadedBytes, 0);
});

test("withDownloadProgress: a missing totalBytes leaves it null (indeterminate), not overwritten to 0", () => {
  let s = initialUpdaterState();
  s = withDownloadStarted(s);
  s = withDownloadProgress(s, { downloadedBytes: 500 });
  assert.equal(s.totalBytes, null);
  s = withDownloadProgress(s, { downloadedBytes: 900, totalBytes: 1000 });
  assert.equal(s.totalBytes, 1000);
  // a later event omitting totalBytes again keeps the last known value,
  // never resets to null once known (?? in withDownloadProgress).
  s = withDownloadProgress(s, { downloadedBytes: 950 });
  assert.equal(s.totalBytes, 1000);
});

// ---------------------------------------------------------------------
// error paths - the three distinct origins (check/download/install)
// determine banner visibility (see shouldShowBanner tests below), each
// reachable from a different point in the flow.
// ---------------------------------------------------------------------

test("withCheckError: tags errorOrigin 'check'", () => {
  let s = initialUpdaterState();
  s = withChecking(s);
  s = withCheckError(s, "network unreachable");
  assert.equal(s.phase, "error");
  assert.equal(s.errorOrigin, "check");
  assert.equal(s.errorMessage, "network unreachable");
});

test("withDownloadError: tags errorOrigin 'download'", () => {
  let s = initialUpdaterState();
  s = withAvailable(s, { version: "2.1.0" });
  s = withDownloadStarted(s);
  s = withDownloadError(s, "connection reset");
  assert.equal(s.phase, "error");
  assert.equal(s.errorOrigin, "download");
});

test("withInstallError: tags errorOrigin 'install'", () => {
  let s = initialUpdaterState();
  s = withAvailable(s, { version: "2.1.0" });
  s = withDownloadStarted(s);
  s = withReady(s);
  s = withInstalling(s);
  s = withInstallError(s, "signature mismatch");
  assert.equal(s.phase, "error");
  assert.equal(s.errorOrigin, "install");
});

// 2026-07-17 4R re-review, RELIABILITY M3: install() succeeding but the
// FOLLOW-UP relaunch() failing must land in a distinct origin from a real
// install failure - see updater.js's header comment ("errorOrigin:
// restart") for why conflating the two is a real bug (a dead-resource
// retry, and a scary "failed" message for an update that actually
// succeeded).
test("withRestartError: tags errorOrigin 'restart' - distinct from 'install', reachable only after install() itself already succeeded", () => {
  let s = initialUpdaterState();
  s = withAvailable(s, { version: "2.1.0" });
  s = withDownloadStarted(s);
  s = withReady(s);
  s = withInstalling(s);
  s = withRestartError(s, "the OS refused the relaunch");
  assert.equal(s.phase, "error");
  assert.equal(s.errorOrigin, "restart");
  assert.notEqual(s.errorOrigin, "install");
  // version survives - the "Update installed — v2.1.0, restart to
  // finish" copy needs it (renderUpdateBanner/renderUpdaterAboutStatus).
  assert.equal(s.version, "2.1.0");
});

test("a missing/empty error message never throws, becomes an empty string", () => {
  let s = withCheckError(initialUpdaterState(), undefined);
  assert.equal(s.errorMessage, "");
  s = withDownloadError(initialUpdaterState(), null);
  assert.equal(s.errorMessage, "");
});

// ---------------------------------------------------------------------
// shouldShowBanner - the single source of truth for "worth interrupting
// the main window" vs. "Settings-only status text" (task brief: banner
// must be non-intrusive, never pop for a silent background check).
// ---------------------------------------------------------------------

test("shouldShowBanner: false for idle/checking/up_to_date - nothing worth interrupting for", () => {
  assert.equal(shouldShowBanner(initialUpdaterState()), false);
  assert.equal(shouldShowBanner(withChecking(initialUpdaterState())), false);
  assert.equal(shouldShowBanner(withUpToDate(initialUpdaterState(), "2.0.0")), false);
});

test("shouldShowBanner: false for a check-originated error - a silent startup check must never pop a banner", () => {
  const s = withCheckError(withChecking(initialUpdaterState()), "offline");
  assert.equal(shouldShowBanner(s), false);
});

test("shouldShowBanner: true for available/downloading/ready/installing", () => {
  let s = withAvailable(initialUpdaterState(), { version: "2.1.0" });
  assert.equal(shouldShowBanner(s), true);
  s = withDownloadStarted(s);
  assert.equal(shouldShowBanner(s), true);
  s = withReady(s);
  assert.equal(shouldShowBanner(s), true);
  s = withInstalling(s);
  assert.equal(shouldShowBanner(s), true);
});

test("shouldShowBanner: true for a download/install error - the operator already opted in by clicking", () => {
  let s = withAvailable(initialUpdaterState(), { version: "2.1.0" });
  s = withDownloadStarted(s);
  s = withDownloadError(s, "connection reset");
  assert.equal(shouldShowBanner(s), true);

  s = withAvailable(initialUpdaterState(), { version: "2.1.0" });
  s = withDownloadStarted(s);
  s = withReady(s);
  s = withInstalling(s);
  s = withInstallError(s, "disk full");
  assert.equal(shouldShowBanner(s), true);
});

test("shouldShowBanner: dismissed always wins, regardless of phase", () => {
  let s = withAvailable(initialUpdaterState(), { version: "2.1.0" });
  assert.equal(shouldShowBanner(s), true);
  s = withDismissed(s);
  assert.equal(shouldShowBanner(s), false);
});

test("shouldShowBanner: true for a restart (post-install relaunch) error too - the operator already opted in", () => {
  let s = withAvailable(initialUpdaterState(), { version: "2.1.0" });
  s = withDownloadStarted(s);
  s = withReady(s);
  s = withInstalling(s);
  s = withRestartError(s, "the OS refused the relaunch");
  assert.equal(shouldShowBanner(s), true);
});

// ---------------------------------------------------------------------
// closePendingUpdateResources - 2026-07-17 4R re-review, RELIABILITY M2:
// the previous inline version of this cleanup only ever closed
// pendingUpdateMeta's own rid, silently leaking the downloaded-bytes
// resource (tens of MB) on every download-then-abandon or download-then-
// recheck cycle. `closeFn` is dependency-injected specifically so this is
// testable with a counting mock instead of a real `invoke`.
// ---------------------------------------------------------------------

test("closePendingUpdateResources: closes BOTH the update metadata and the downloaded-bytes resource when both are pending", async () => {
  const closed = [];
  await closePendingUpdateResources({ rid: 7 }, 12, (rid) => {
    closed.push(rid);
  });
  assert.deepEqual(closed.sort((a, b) => a - b), [7, 12]);
});

test("closePendingUpdateResources: closes only what's actually pending, never invents a call", async () => {
  let closed = [];
  await closePendingUpdateResources(null, null, (rid) => closed.push(rid));
  assert.deepEqual(closed, []);

  closed = [];
  await closePendingUpdateResources({ rid: 3 }, null, (rid) => closed.push(rid));
  assert.deepEqual(closed, [3]);

  closed = [];
  await closePendingUpdateResources(null, 9, (rid) => closed.push(rid));
  assert.deepEqual(closed, [9]);
});

test("closePendingUpdateResources: a bytesRid of 0 is a real resource id, not treated as falsy/absent", () => {
  // ResourceId 0 is a perfectly valid id (Tauri's resource table starts
  // counting from 0/1 depending on version) - this function must check
  // `!= null`, never plain truthiness, or the very first download of a
  // session could silently never get closed.
  const closed = [];
  return closePendingUpdateResources(null, 0, (rid) => closed.push(rid)).then(() => {
    assert.deepEqual(closed, [0]);
  });
});

test("closePendingUpdateResources: one close failing never stops the other from being attempted", async () => {
  const closed = [];
  await closePendingUpdateResources({ rid: 1 }, 2, (rid) => {
    closed.push(rid);
    if (rid === 1) throw new Error("already closed");
  });
  assert.deepEqual(closed.sort((a, b) => a - b), [1, 2]);
});

test("closePendingUpdateResources: also tolerates closeFn returning a rejected promise, not just a throw", async () => {
  const closed = [];
  await closePendingUpdateResources({ rid: 1 }, 2, (rid) => {
    closed.push(rid);
    return rid === 1 ? Promise.reject(new Error("boom")) : Promise.resolve();
  });
  assert.deepEqual(closed.sort((a, b) => a - b), [1, 2]);
});

// ---------------------------------------------------------------------
// canStartInstall - the one safety gate before the disruptive step
// (install() + relaunch()) - never allowed to fire out from under an
// active call.
// ---------------------------------------------------------------------

test("canStartInstall: true only when there is no active call", () => {
  assert.equal(canStartInstall(false), true);
  assert.equal(canStartInstall(true), false);
  assert.equal(canStartInstall(null), true); // falsy "no call object" reads as no active call
  assert.equal(canStartInstall(undefined), true);
});

// ---------------------------------------------------------------------
// canRunBackgroundRecheck - 2026-07-17 4R re-review (same-day follow-up):
// the periodic 24h timer must be able to recover from a check-origin
// error (typical: boot() ran before the network was up) or it silently
// disables itself for the rest of the process's life - the exact
// "softphone sits in the tray for weeks and never hears about updates"
// failure this whole feature exists to prevent. Must NOT recheck from
// anything that has a real pending resource/state to protect
// (available/downloading/ready/installing, or a download/install/restart
// error - each of those carries something a silent background re-check
// would clobber).
// ---------------------------------------------------------------------

test("canRunBackgroundRecheck: true from idle and up_to_date - nothing pending to protect", () => {
  assert.equal(canRunBackgroundRecheck(initialUpdaterState()), true);
  assert.equal(canRunBackgroundRecheck(withUpToDate(initialUpdaterState(), "2.0.0")), true);
});

test("canRunBackgroundRecheck: true from a check-origin error - THE bug this pass fixes", () => {
  const s = withCheckError(withChecking(initialUpdaterState()), "offline");
  assert.equal(s.errorOrigin, "check");
  assert.equal(canRunBackgroundRecheck(s), true);
});

test("canRunBackgroundRecheck: false from download/install/restart-origin errors - each has real pending state", () => {
  let s = withAvailable(initialUpdaterState(), { version: "2.1.0" });
  s = withDownloadStarted(s);
  const downloadErr = withDownloadError(s, "connection reset");
  assert.equal(canRunBackgroundRecheck(downloadErr), false);

  s = withReady(withDownloadStarted(withAvailable(initialUpdaterState(), { version: "2.1.0" })));
  const installErr = withInstallError(withInstalling(s), "signature mismatch");
  assert.equal(canRunBackgroundRecheck(installErr), false);

  const restartErr = withRestartError(withInstalling(s), "the OS refused the relaunch");
  assert.equal(canRunBackgroundRecheck(restartErr), false);
});

test("canRunBackgroundRecheck: false from available/downloading/ready/installing - all have something pending", () => {
  let s = withAvailable(initialUpdaterState(), { version: "2.1.0" });
  assert.equal(canRunBackgroundRecheck(s), false);
  s = withDownloadStarted(s);
  assert.equal(canRunBackgroundRecheck(s), false);
  s = withReady(s);
  assert.equal(canRunBackgroundRecheck(s), false);
  s = withInstalling(s);
  assert.equal(canRunBackgroundRecheck(s), false);
});

test("canRunBackgroundRecheck: false while a check is already in flight - checking is not idle", () => {
  assert.equal(canRunBackgroundRecheck(withChecking(initialUpdaterState())), false);
});

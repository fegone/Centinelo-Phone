// Console panel — inline host module for the premium receptionist console.
//
// The console UI itself is the PRIVATE console-ui package (repo
// centinelo-premium), distributed as an installer resource in
// premium-console-assets/ beside the executable and served at runtime by
// console.rs's `premium-console://` scheme — never bundled in this public
// repo (see docs/console-inline-panel-decision.md for the whole design:
// CSP option analysis, license gating, permissions). This module is the
// public shell-side glue that brings it into the MAIN window as a panel,
// the exact `#screen-settings` pattern: a `#screen-console` div that
// `hidden = false` reveals, instead of the legacy separate WebviewWindow
// (still available behind console.rs's SEPARATE_WINDOW_ENV during the
// transition).
//
// Everything here mirrors what console.rs's embedded INDEX_HTML glue
// already did for the separate window — same script dependency order
// (premium console-ui's own dev/mock.html documents it), same
// EngineBridge "Option B" wiring (one Tauri command per verb, matching
// commands.rs's convention), same get_favorites roster semantics — but
// executed against THIS document, where:
//   - a load failure is just a banner in a window that still has its own
//     chrome (no watchdog / close-on-failure / mark_ready dance needed),
//   - closing the panel is `hidden = true` + `app.destroy()` (the
//     package's own teardown: store stop, BLF unsubscriptions, tick
//     interval), plus `console_panel_closed` so Rust restores the main
//     window's pre-panel size (console.rs::panel_closed),
//   - CSP: tauri.conf.json allows `premium-console:` in script-src and
//     style-src; every byte still passes console.rs's license-gated
//     handler (404 unless blf_console is licensed).
//
// Pure helpers (CONSOLE_SCRIPTS order, assetUrl, favoritesToRoster, the
// dispatch table) are exported for console-panel.test.js — same
// "pure logic unit-testable without a webview" convention blf-ui.js /
// settings-load.js already follow. The stateful half takes its Tauri
// bindings via initConsolePanel({ invoke, listen, showBanner,
// reportFrontendIssue }) from app.js rather than touching
// window.__TAURI__ directly, so tests never need the global.

import { t } from "./i18n.js";

/// Base URL every console-ui asset is served from — matches
/// console.rs::ASSET_SCHEME + the host its handler serves (the same URL
/// the legacy separate window loaded). Pinned here so the test can pin
/// the shell↔scheme contract in one place.
export const CONSOLE_ASSET_ORIGIN = "premium-console://localhost/";

/// Stylesheets, in the same order the package's own dev/mock.html loads
/// them (tokens first: console.css consumes its custom properties).
export const CONSOLE_STYLES = Object.freeze(["tokens.css", "console.css"]);

/// Classic scripts in the package's own dependency order — verbatim the
/// order dev/mock.html's <script> tags use (dom-utils before everything,
/// store/bridge before the components that consume them, console-app
/// last). Dynamically-inserted classic scripts execute in LOAD order,
/// not insertion order, so openConsolePanel loads these strictly
/// sequentially — do not "optimize" into a Promise.all.
export const CONSOLE_SCRIPTS = Object.freeze([
  "components/dom-utils.js",
  "components/icons.js",
  "store/ConsoleStore.js",
  "bridge/EngineBridge.js",
  "components/extension-tile.js",
  "components/extension-grid.js",
  "components/active-call.js",
  "components/incoming-queue.js",
  "components/statusbar.js",
  "components/drag-controller.js",
  "console-app.js",
]);

/// Absolute URL for one console-ui asset path (e.g. "console-app.js").
export function assetUrl(path) {
  return CONSOLE_ASSET_ORIGIN + path;
}

/// Maps the operator's configured favorites (commands::get_favorites —
/// the same source the main window's favorites grid reads) onto the
/// roster shape ConsoleApp.mount() takes. Favorites with a blank
/// extension are dropped (nothing to subscribe/call); a blank label
/// falls back to "Ext <n>"; group is constant — this shell has no
/// directory/CRM grouping (same semantics as the legacy INDEX_HTML glue,
/// which this replaces). selfExt stays null on purpose: every configured
/// favorite gets a live subscribed tile; the "Your call" panel is driven
/// by call_state events regardless.
export function favoritesToRoster(favorites) {
  return (favorites || [])
    .filter((f) => (f.ext || "").trim())
    .map((f) => {
      const ext = f.ext.trim();
      return { ext, name: (f.label || "").trim() || "Ext " + ext, group: "Favorites" };
    });
}

/// EngineBridge verb → [Tauri command, argument builder]. "Option B" from
/// the console-ui README's EngineBridge contract: one existing commands.rs
/// verb per console verb — no generic passthrough. `call_id: null` means
/// "the current call" server-side (commands.rs's own convention).
export const CONSOLE_DISPATCH_TABLE = Object.freeze({
  dial: ["sidecar_dial", (c) => ({ uri: c.uri })],
  answer: ["sidecar_answer", () => ({})],
  hangup: ["sidecar_hangup", (c) => ({ call_id: c.call_id || null })],
  register: ["sidecar_restart", () => ({})],
  hold: ["sidecar_hold", (c) => ({ call_id: c.call_id || null })],
  resume: ["sidecar_resume", (c) => ({ call_id: c.call_id || null })],
  mute: ["sidecar_mute", (c) => ({ on: !!c.on, call_id: c.call_id || null })],
  blind_transfer: ["sidecar_blind_transfer", (c) => ({ uri: c.uri, call_id: c.call_id || null })],
  attended_transfer: ["sidecar_attended_transfer", (c) => ({ uri: c.uri, call_id: c.call_id || null })],
  complete_transfer: ["sidecar_complete_transfer", (c) => ({ call_id: c.call_id || null })],
  abort_transfer: ["sidecar_abort_transfer", () => ({})],
  blf_subscribe: ["sidecar_blf_subscribe", (c) => ({ ext: String(c.ext) })],
  blf_unsubscribe: ["sidecar_blf_unsubscribe", (c) => ({ ext: String(c.ext) })],
});

/// Builds the sendCommand fn EngineBridge.init() takes, over an injected
/// `invoke` (so tests can pass a spy). Unknown verbs reject — same
/// explicit failure the legacy glue had.
export function makeConsoleDispatcher(invoke) {
  return function dispatch(cmd) {
    const entry = CONSOLE_DISPATCH_TABLE[cmd && cmd.cmd];
    if (!entry) {
      return Promise.reject(new Error("console: no dispatch for cmd '" + (cmd && cmd.cmd) + "'"));
    }
    return invoke(entry[0], entry[1](cmd));
  };
}

// ---------------------------------------------------------------------------
// stateful half — runs in the webview only
// ---------------------------------------------------------------------------

let deps = null; // set by initConsolePanel: { invoke, listen, showBanner, reportFrontendIssue }
let assetsPromise = null; // one load of styles+scripts per session (reset on failure so a retry is possible)
const loadedAssets = new Set(); // asset URLs with a live node in <head> - lets a retry resume past what already loaded
let openingPromise = null; // the in-flight openConsolePanel() run - see openConsolePanel()'s doc
let mounted = null; // the ConsoleApp.mount() return value ({ store, element, destroy }) while the panel shows

/// Hands this module its Tauri bindings + UI surfaces. Called once from
/// app.js's boot(), before any panel can open.
export function initConsolePanel(d) {
  deps = d;
}

/// Test-only: console-panel.test.js's stateful-half tests reset this
/// module's singletons (deps, in-flight open, assets cache, mounted
/// instance) so each open/close scenario runs in isolation. Never call
/// from app.js.
export function resetConsolePanelStateForTests() {
  deps = null;
  openingPromise = null;
  assetsPromise = null;
  mounted = null;
  loadedAssets.clear();
}

export function isConsolePanelOpen() {
  const el = document.getElementById("screen-console");
  return !!el && !el.hidden;
}

/// Shows the console panel: load assets (once), mount the package on
/// #console-host, reveal #screen-console. Rust has already re-checked the
/// license and grown the window before the open event that leads here
/// (console.rs::open_panel); if the asset handler still refuses (license
/// vanished mid-session, assets dir gone), the sequential loader rejects
/// and failConsolePanel() unwinds everything, banner included.
///
/// Reentrancy is impossible by construction, not by timing: the whole
/// open is one in-flight promise (`openingPromise`) stored BEFORE its
/// first `await` resolves. `isConsolePanelOpen()` stays false until the
/// very last step, so on its own it guards nothing while
/// ensureAssetsLoaded()/mountConsole() are pending - without this guard,
/// a double click ran two ConsoleApp.mount()s, each with its own
/// EngineBridge `deps.listen("sidecar-event", ...)`: duplicated BLF
/// subscriptions, duplicated dispatch, and only the last mount
/// teardown-able. console.rs::open_panel holds its own open state and
/// drops duplicate events the same way, so this does not depend on the
/// frontend alone.
export function openConsolePanel() {
  if (!deps || isConsolePanelOpen()) return Promise.resolve();
  if (!openingPromise) openingPromise = runConsoleOpen();
  return openingPromise;
}

/// The single open-in-flight. Its `openingPromise = runConsoleOpen()`
/// assignment above is synchronous - no `await` between the
/// `isConsolePanelOpen()` check and it - so two openConsolePanel() calls
/// can never interleave past the guard (JS runs the caller to completion
/// first); the second just gets the first's promise. Never rejects: a
/// failed open is failConsolePanel()'s banner, not an unhandled rejection
/// in every caller. The `finally` clears the guard so the next open - a
/// retry after failure, a reopen after close - starts fresh instead of
/// silently attaching to a dead promise.
async function runConsoleOpen() {
  try {
    await ensureAssetsLoaded();
    await mountConsole();
    document.getElementById("screen-console").hidden = false;
  } catch (err) {
    failConsolePanel(err);
  } finally {
    openingPromise = null;
  }
}

/// Hides the panel and tears the mounted console down. The package's
/// destroy() unsubscribes its BLF lines and stops its tick interval;
/// `console_panel_closed` then lets Rust restore the main window's
/// pre-panel size (console.rs::panel_closed). Safe to call with nothing
/// mounted (the failure path relies on that).
export function closeConsolePanel() {
  const screen = document.getElementById("screen-console");
  if (screen) screen.hidden = true;
  if (mounted) {
    try {
      mounted.destroy();
    } catch (e) {
      console.error("console panel: destroy() threw", e);
    }
    mounted = null;
  }
  if (deps) {
    deps.invoke("console_panel_closed").catch((e) => console.error("console_panel_closed failed", e));
  }
}

function failConsolePanel(err) {
  console.error("console panel failed to load", err);
  if (deps && deps.reportFrontendIssue) {
    deps.reportFrontendIssue("console_panel_load_failed", {
      message: String((err && err.message) || err),
    });
  }
  closeConsolePanel();
  if (deps) deps.showBanner(t("console.loadFailed"), "err");
}

function loadStyle(url) {
  return new Promise((resolve, reject) => {
    const link = document.createElement("link");
    link.rel = "stylesheet";
    link.href = url;
    link.onload = () => resolve();
    link.onerror = () => {
      // A node that failed to load is dead weight in <head>: drop it now
      // so the retry's fresh node does not stack duplicates.
      link.remove();
      reject(new Error("stylesheet failed to load: " + url));
    };
    document.head.appendChild(link);
  });
}

function loadScript(url) {
  return new Promise((resolve, reject) => {
    const script = document.createElement("script");
    script.src = url;
    script.onload = () => resolve();
    script.onerror = () => {
      script.remove(); // same as loadStyle: no dead nodes left for the retry to pile onto
      reject(new Error("script failed to load: " + url));
    };
    document.head.appendChild(script);
  });
}

/// Injects the package's stylesheets + scripts into THIS document, once
/// per session. Sequential on purpose — see CONSOLE_SCRIPTS' doc. A
/// rejected load resets `assetsPromise` so the next open retries instead
/// of caching the failure — and the retry RESUMES, not restarts: URLs
/// whose node is already live (tracked in `loadedAssets`) are skipped, so
/// a failure at script 7 of 11 neither re-executes scripts 1-6 (their
/// classic-script module bodies would run again: store/bridge
/// singletons rebuilt while the old ones are still referenced) nor
/// stacks duplicate <link>/<script> nodes in <head>. Only the exact URL
/// that failed is retried. openConsolePanel's single-flight guarantees
/// at most one pass is ever in progress.
function ensureAssetsLoaded() {
  if (!assetsPromise) {
    assetsPromise = loadConsoleAssets();
    assetsPromise.catch(() => {
      assetsPromise = null;
    });
  }
  return assetsPromise;
}

async function loadConsoleAssets() {
  for (const style of CONSOLE_STYLES) await loadAssetOnce(loadStyle, style);
  for (const script of CONSOLE_SCRIPTS) await loadAssetOnce(loadScript, script);
}

/// One asset, unless a previous (partial) pass already made it live —
/// the resume half of ensureAssetsLoaded's retry contract. Marks the URL
/// loaded only on success.
async function loadAssetOnce(load, path) {
  const url = assetUrl(path);
  if (loadedAssets.has(url)) return;
  await load(url);
  loadedAssets.add(url);
}

/// Bridge + roster + mount — the same wiring the legacy INDEX_HTML glue
/// performed in its own document. `window.Centinelo` only exists once
/// ensureAssetsLoaded() resolved. A failed get_favorites degrades to an
/// empty roster rather than blocking the panel (the package mounts and
/// renders fine with nothing to show — same fallback as the legacy glue).
async function mountConsole() {
  const host = document.getElementById("console-host");
  if (!host) throw new Error("console panel: #console-host is missing from index.html");
  const Centinelo = window.Centinelo;
  if (!Centinelo || !Centinelo.EngineBridge || !Centinelo.ConsoleApp) {
    throw new Error("console panel: console-ui package did not register on window.Centinelo");
  }

  const bridge = Centinelo.EngineBridge.create();
  bridge.init(
    makeConsoleDispatcher(deps.invoke),
    (handler) => {
      deps.listen("sidecar-event", (e) => handler(e.payload));
    }
  );

  let favorites = [];
  try {
    favorites = await deps.invoke("get_favorites");
  } catch (e) {
    console.error("console: get_favorites failed", e);
  }

  host.replaceChildren();
  mounted = Centinelo.ConsoleApp.mount(host, {
    roster: favoritesToRoster(favorites),
    selfExt: null,
    bridge,
  });
}

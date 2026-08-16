// Tests for console-panel.js's pure helpers (inline BLF console panel,
// 2026-08-14). Node's built-in test runner (`node:test`/`node:assert`) -
// no new devDependency, matching this project's "no bundler, no frontend
// framework" philosophy. Run: `npm test` (from shell/) or
// `node --test ui/js/*.test.js`.
//
// The DOM-free half of the module is covered first (CONSOLE_SCRIPTS
// order, assetUrl, favoritesToRoster, the dispatch table). The stateful
// half (openConsolePanel/closeConsolePanel/ensureAssetsLoaded) is covered
// at the bottom with a hand-rolled document/window double - still no
// jsdom-style dependency, just the exact surface the module touches
// (getElementById, createElement, head.appendChild, element.hidden /
// .remove(), host.replaceChildren()). Those tests are the regression net
// for this panel's reason to exist: a load failure must NEVER reveal
// #screen-console (the white-screen bug this inline design replaced),
// and an open must be single-flight - one mount, one BLF subscription.
//
// The asset-list assertions double as a contract pin against console.rs's
// INDEX_HTML: if either side drifts (script renamed, order changed), this
// file fails before a licensed operator ever sees a half-loaded panel.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  ASSET_SCHEME,
  resolveConsoleAssetOrigin,
  CONSOLE_STYLES,
  CONSOLE_SCRIPTS,
  assetUrl,
  favoritesToRoster,
  CONSOLE_DISPATCH_TABLE,
  makeConsoleDispatcher,
  initConsolePanel,
  openConsolePanel,
  closeConsolePanel,
  resetConsolePanelStateForTests,
} from "./console-panel.js";

// The origin shape a Tauri `convertFileSrc`-alike returns on macOS/Linux
// vs. Windows/Android (tauri 2.11.5's scripts/core.js). Fake spies below
// mimic exactly that contract instead of asserting one hardcoded literal
// - the bug this module used to have (see resolveConsoleAssetOrigin's doc)
// was pinning ONLY the macOS/Linux shape as ground truth.
const MACOS_LINUX_ORIGIN = "premium-console://localhost/";
const WINDOWS_ANDROID_ORIGIN = "http://premium-console.localhost/";
const fakeConvertFileSrcMacLinux = (path, protocol) => `${protocol}://localhost/${path}`;
const fakeConvertFileSrcWindowsAndroid = (path, protocol) => `http://${protocol}.localhost/${path}`;

test("ASSET_SCHEME pins the scheme name console.rs::ASSET_SCHEME must match", () => {
  assert.equal(ASSET_SCHEME, "premium-console");
});

test("resolveConsoleAssetOrigin: macOS/Linux shape (scheme://localhost/)", () => {
  assert.equal(resolveConsoleAssetOrigin(fakeConvertFileSrcMacLinux), MACOS_LINUX_ORIGIN);
});

test("resolveConsoleAssetOrigin: Windows/Android shape (http://scheme.localhost/)", () => {
  // THE regression this module exists to fix: a hardcoded macOS/Linux
  // literal here silently broke Windows for months (see git history /
  // docs/console-inline-panel-decision.md's addendum). This assertion
  // fails if resolveConsoleAssetOrigin ever goes back to ignoring the
  // platform it's told about.
  assert.equal(resolveConsoleAssetOrigin(fakeConvertFileSrcWindowsAndroid), WINDOWS_ANDROID_ORIGIN);
});

test("resolveConsoleAssetOrigin forwards an empty path and the exact scheme name", () => {
  // Empty path matters: convertFileSrc percent-encodes its filePath as one
  // opaque segment (encodeURIComponent escapes "/" too), which would
  // corrupt multi-segment asset paths like "components/dom-utils.js" -
  // encodeURIComponent("") is "", so an empty path is the only call shape
  // that yields a clean bare origin. See resolveConsoleAssetOrigin's doc.
  const calls = [];
  resolveConsoleAssetOrigin((path, protocol) => {
    calls.push([path, protocol]);
    return "unused";
  });
  assert.deepEqual(calls, [["", "premium-console"]]);
});

test("CONSOLE_STYLES: tokens.css before console.css (custom-property order)", () => {
  assert.deepEqual([...CONSOLE_STYLES], ["tokens.css", "console.css"]);
});

test("CONSOLE_SCRIPTS: dependency order, dom-utils first and console-app last", () => {
  // Verbatim order of console.rs INDEX_HTML's <script> tags, which is
  // itself the package's own dev/mock.html order. Only the anchors are
  // asserted (first/last/store-before-bridge) plus the full count, so a
  // mid-list reorder the package itself makes still needs a human look,
  // but a rename/removal fails loudly here.
  assert.equal(CONSOLE_SCRIPTS[0], "components/dom-utils.js");
  assert.equal(CONSOLE_SCRIPTS[1], "components/icons.js");
  assert.equal(CONSOLE_SCRIPTS.indexOf("store/ConsoleStore.js") < CONSOLE_SCRIPTS.indexOf("bridge/EngineBridge.js"), true);
  assert.equal(CONSOLE_SCRIPTS[CONSOLE_SCRIPTS.length - 1], "console-app.js");
  assert.equal(CONSOLE_SCRIPTS.length, 11);
});

test("assetUrl joins an explicit origin + path without double slashes, on both platform shapes", () => {
  // origin is a required argument, never a module default - see
  // assetUrl's doc for why (the same hardcoded-shape bug
  // resolveConsoleAssetOrigin fixes one level up).
  assert.equal(assetUrl(MACOS_LINUX_ORIGIN, "console-app.js"), "premium-console://localhost/console-app.js");
  assert.equal(assetUrl(MACOS_LINUX_ORIGIN, "components/icons.js"), "premium-console://localhost/components/icons.js");
  assert.equal(assetUrl(WINDOWS_ANDROID_ORIGIN, "console-app.js"), "http://premium-console.localhost/console-app.js");
});

test("favoritesToRoster: keeps ext-bearing favorites, trims, falls back to Ext label", () => {
  const roster = favoritesToRoster([
    { ext: "101", label: "Front Desk" },
    { ext: " 202 ", label: "   " }, // blank label -> Ext fallback, trimmed ext
    { ext: "", label: "No extension" }, // dropped: nothing to subscribe/call
    { ext: "   ", label: "Whitespace ext" }, // dropped, same reason
    { label: "No ext key at all" }, // dropped (undefined ext)
  ]);
  assert.deepEqual(roster, [
    { ext: "101", name: "Front Desk", group: "Favorites" },
    { ext: "202", name: "Ext 202", group: "Favorites" },
  ]);
});

test("favoritesToRoster: null/undefined list degrades to empty roster", () => {
  assert.deepEqual(favoritesToRoster(undefined), []);
  assert.deepEqual(favoritesToRoster(null), []);
});

test("dispatch table: one existing commands.rs verb per console verb (Option B, no passthrough)", () => {
  // Same verb set as console.rs INDEX_HTML's DISPATCH - a console verb
  // without a mapping here would dead-end with no dispatch at runtime.
  assert.deepEqual(Object.keys(CONSOLE_DISPATCH_TABLE).sort(), [
    "abort_transfer",
    "answer",
    "attended_transfer",
    "blf_subscribe",
    "blf_unsubscribe",
    "blind_transfer",
    "complete_transfer",
    "dial",
    "hangup",
    "hold",
    "mute",
    "register",
    "resume",
  ]);
  for (const [cmd, [tauriCmd]] of Object.entries(CONSOLE_DISPATCH_TABLE)) {
    assert.match(tauriCmd, /^sidecar_(dial|answer|hangup|restart|hold|resume|mute|blind_transfer|attended_transfer|complete_transfer|abort_transfer|blf_subscribe|blf_unsubscribe)$/, `verb ${cmd}`);
  }
});

test("makeConsoleDispatcher: invokes the mapped command with mapped args", async () => {
  const calls = [];
  const dispatch = makeConsoleDispatcher((cmd, args) => {
    calls.push([cmd, args]);
    return Promise.resolve("ok");
  });
  await dispatch({ cmd: "dial", uri: "sip:101@host" });
  await dispatch({ cmd: "hangup" }); // no call_id -> null ("current call")
  await dispatch({ cmd: "mute", on: 1 }); // truthy -> boolean
  await dispatch({ cmd: "blf_subscribe", ext: 101 }); // number -> String
  assert.deepEqual(calls, [
    ["sidecar_dial", { uri: "sip:101@host" }],
    ["sidecar_hangup", { call_id: null }],
    ["sidecar_mute", { on: true, call_id: null }],
    ["sidecar_blf_subscribe", { ext: "101" }],
  ]);
});

test("makeConsoleDispatcher: unknown verbs reject instead of silently no-op'ing", async () => {
  const dispatch = makeConsoleDispatcher(() => Promise.resolve("should not run"));
  await assert.rejects(dispatch({ cmd: "teleport" }), /no dispatch for cmd 'teleport'/);
  await assert.rejects(dispatch(null), /no dispatch for cmd 'null'/);
});

// ---------------------------------------------------------------------------
// stateful half - minimal DOM double, no jsdom
// ---------------------------------------------------------------------------

/// An element double with exactly the properties console-panel.js reads
/// or writes: rel/href/src, onload/onerror, .remove(). `.remove()` is
/// finalized by installFakeDom so it detaches from the fake <head> like
/// a real node would.
function makeElementDouble(tag) {
  return { tagName: tag.toUpperCase(), rel: "", href: "", src: "", onload: null, onerror: null, removed: false };
}

/// A document double whose <head> settles every appended <link>/<script>
/// from a microtask - after the current synchronous run, like a real
/// network fetch would. `failUrls` (full premium-console:// URLs) make
/// exactly those assets' onerror fire; mutating the returned `fail` set
/// between opens simulates "the outage was fixed" for retry tests.
function installFakeDom({ failUrls = [] } = {}) {
  const fail = new Set(failUrls);
  const head = {
    children: [],
    appendChild(el) {
      head.children.push(el);
      queueMicrotask(() => {
        const url = el.href || el.src;
        if (fail.has(url)) el.onerror && el.onerror(new Error("load failed: " + url));
        else el.onload && el.onload();
      });
    },
  };
  const screen = { hidden: true }; // #screen-console starts hidden - the whole point
  const host = { children: [], replaceChildren() { this.children.length = 0; } };
  const byId = { "screen-console": screen, "console-host": host };
  const document = {
    head,
    getElementById: (id) => byId[id] || null,
    createElement(tag) {
      const el = makeElementDouble(tag);
      el.remove = () => {
        el.removed = true;
        const i = head.children.indexOf(el);
        if (i >= 0) head.children.splice(i, 1);
      };
      return el;
    },
  };
  return {
    fail,
    head,
    screen,
    host,
    document,
    liveNodesFor: (url) => head.children.filter((el) => el.href === url || el.src === url),
  };
}

/// window.Centinelo double: EngineBridge.create().init() subscribes
/// immediately (so every mount = one deps.listen("sidecar-event") call,
/// the subscription count the double-click test pins), and
/// ConsoleApp.mount() renders one child into the host and hands back a
/// destroy-tracking handle like the real package's teardown API.
function installFakeConsoleUi() {
  const ui = {
    mounts: [],
    mounted: [],
    window: {
      Centinelo: {
        EngineBridge: {
          create: () => ({
            init(_sendCommand, subscribe) {
              subscribe(() => {});
            },
          }),
        },
        ConsoleApp: {
          mount(hostElement, opts) {
            const ingestCalls = [];
            const store = { ingestCalls, ingest(event) { ingestCalls.push(event); } };
            const handle = { hostElement, opts, store, destroyed: false, destroy() { this.destroyed = true; } };
            hostElement.children.push({ mountHandle: handle });
            ui.mounts.push({ hostElement, opts });
            ui.mounted.push(handle);
            return handle;
          },
        },
      },
    },
  };
  return ui;
}

/// deps double: every Tauri binding initConsolePanel takes, as a spy.
/// get_favorites resolves [] (the degraded-empty-roster path is not under
/// test here) by default. `regState`/`blfStates`/`failCommands` (set of
/// command names) let hydration tests configure the snapshot commands'
/// responses without touching every other test's setup.
/// `listenDelay: true` makes `listen()` return a Promise that resolves on
/// a LATER microtask (instead of an already-resolved one) - the shape
/// real Tauri `listen()` actually has (it awaits an IPC round-trip before
/// the listener is truly live) - so a test can assert nothing else runs
/// until it settles.
function installFakeDeps({ regState = null, blfStates = null, failCommands = [] } = {}) {
  const fail = new Set(failCommands);
  const d = {
    invokeCalls: [],
    listenCalls: [],
    showBannerCalls: [],
    reportCalls: [],
    invoke(cmd, args) {
      d.invokeCalls.push([cmd, args]);
      if (fail.has(cmd)) return Promise.reject(new Error(cmd + " failed"));
      if (cmd === "get_favorites") return Promise.resolve([]);
      if (cmd === "get_reg_state") return Promise.resolve(regState);
      if (cmd === "get_blf_states") return Promise.resolve(blfStates);
      return Promise.resolve(null);
    },
    listen(event, handler) {
      d.listenCalls.push([event, handler]);
      return Promise.resolve(() => {});
    },
    showBanner(message, kind) { d.showBannerCalls.push([message, kind]); },
    reportFrontendIssue(kind, data) { d.reportCalls.push([kind, data]); },
    // Fixed to the macOS/Linux shape - the stateful tests below don't
    // exercise platform selection (that's resolveConsoleAssetOrigin's
    // own tests above); they just need SOME origin initConsolePanel can
    // resolve, consistently, so assetUrl(TEST_ORIGIN, path) below matches
    // what the module under test actually requests.
    convertFileSrc: fakeConvertFileSrcMacLinux,
  };
  return d;
}

/// The origin installFakeDeps's convertFileSrc resolves to - matches what
/// initConsolePanel(deps) computes internally, so tests can build the
/// exact URLs the module will fetch without hardcoding a separate literal.
const TEST_ORIGIN = MACOS_LINUX_ORIGIN;

/// One stateful scenario: swap in fresh document/window doubles (saving
/// whatever was there), reset the module's singletons, silence the
/// module's own console.error noise from the failure paths under test.
/// Call teardown() in a finally.
function setupStateful({ failUrls = [], regState, blfStates, failCommands } = {}) {
  const prevDocument = globalThis.document;
  const prevWindow = globalThis.window;
  const prevError = console.error;
  console.error = () => {};
  const dom = installFakeDom({ failUrls });
  const ui = installFakeConsoleUi();
  const deps = installFakeDeps({ regState, blfStates, failCommands });
  globalThis.document = dom.document;
  globalThis.window = ui.window;
  resetConsolePanelStateForTests();
  initConsolePanel(deps);
  return {
    dom,
    ui,
    deps,
    teardown() {
      console.error = prevError;
      globalThis.document = prevDocument;
      globalThis.window = prevWindow;
      resetConsolePanelStateForTests();
    },
  };
}

test("P3-1 asset load failure: panel NEVER reveals, error banner + report fire instead", async () => {
  // THE anti-white-screen regression test: if a future refactor reveals
  // #screen-console before the assets are guaranteed loaded, this fails.
  const failAt = assetUrl(TEST_ORIGIN, CONSOLE_SCRIPTS[0]); // both styles load, first script 404s
  const s = setupStateful({ failUrls: [failAt] });
  try {
    await openConsolePanel();
    assert.equal(s.dom.screen.hidden, true, "a failed load must never reveal #screen-console");
    assert.equal(s.ui.mounts.length, 0, "nothing may mount on a failed load");
    assert.equal(s.deps.showBannerCalls.length, 1, "operator sees exactly one banner");
    assert.equal(s.deps.showBannerCalls[0][1], "err");
    assert.ok(typeof s.deps.showBannerCalls[0][0] === "string" && s.deps.showBannerCalls[0][0].length > 0, "banner text resolved via t(), not a raw key crash");
    assert.deepEqual(s.deps.reportCalls.map(([kind]) => kind), ["console_panel_load_failed"]);
    // unwind tells Rust, so the window-size restore still happens
    assert.ok(s.deps.invokeCalls.some(([cmd]) => cmd === "console_panel_closed"));
    // the two styles that DID load are live exactly once; the failed script left no dead node
    assert.equal(s.dom.liveNodesFor(assetUrl(TEST_ORIGIN, CONSOLE_STYLES[0])).length, 1);
    assert.equal(s.dom.liveNodesFor(assetUrl(TEST_ORIGIN, CONSOLE_STYLES[1])).length, 1);
    assert.equal(s.dom.liveNodesFor(failAt).length, 0, "failed node removed, not stacked for the retry");
  } finally {
    s.teardown();
  }
});

test("P3-2 open / close / reopen: one live mount at a time, full teardown each cycle", async () => {
  const s = setupStateful();
  try {
    await openConsolePanel();
    assert.equal(s.dom.screen.hidden, false);
    assert.equal(s.ui.mounts.length, 1);
    assert.equal(s.deps.listenCalls.filter(([ev]) => ev === "sidecar-event").length, 1, "one BLF subscription per mount");
    // open while already open is a no-op, not a second mount
    await openConsolePanel();
    assert.equal(s.ui.mounts.length, 1);

    closeConsolePanel();
    assert.equal(s.dom.screen.hidden, true);
    assert.equal(s.ui.mounted[0].destroyed, true, "close runs the package's own teardown");
    assert.ok(s.deps.invokeCalls.some(([cmd]) => cmd === "console_panel_closed"), "close tells Rust to restore the window size");

    // reopen: a fresh mount (new bridge/subscription), assets NOT re-injected
    const headCount = s.dom.head.children.length;
    await openConsolePanel();
    assert.equal(s.ui.mounts.length, 2, "reopen mounts a fresh console");
    assert.equal(s.dom.head.children.length, headCount, "assets load once per session - reopen injects zero new nodes");
    assert.equal(s.deps.listenCalls.filter(([ev]) => ev === "sidecar-event").length, 2, "subscription of cycle 1 paired with its destroy, cycle 2 has its own");
    assert.equal(s.ui.mounted.filter((m) => !m.destroyed).length, 1, "exactly one live mount");
    assert.equal(s.dom.host.children.length, 1, "host.replaceChildren dropped the torn-down mount's element");
    assert.equal(s.dom.host.children[0].mountHandle, s.ui.mounted[1], "the live element belongs to the current mount");
  } finally {
    s.teardown();
  }
});

test("P3-3 double click while assets still loading: single flight, one mount, one subscription", async () => {
  // The P1 race: both clicks land before the first open's awaits settle
  // (isConsolePanelOpen() is still false - it only turns true at the very
  // end). The second call must attach to the first in-flight open.
  const s = setupStateful();
  try {
    const first = openConsolePanel();
    const second = openConsolePanel();
    assert.notEqual(second, undefined);
    assert.equal(second, first, "reentrant call attaches to the in-flight open instead of starting a second one");
    await Promise.all([first, second]);
    assert.equal(s.ui.mounts.length, 1, "exactly one ConsoleApp.mount");
    assert.equal(s.deps.listenCalls.filter(([ev]) => ev === "sidecar-event").length, 1, "exactly one EngineBridge subscription - no duplicated BLF");
    assert.equal(s.deps.invokeCalls.filter(([cmd]) => cmd === "get_favorites").length, 1, "roster fetched once");
    assert.equal(s.dom.screen.hidden, false, "the open still completes normally");
  } finally {
    s.teardown();
  }
});

test("P3-4 retry after failure resumes: no re-injected or re-executed assets", async () => {
  // The P2 contract: failure at script 1, outage "fixed", next open must
  // not re-run the two styles that already loaded (they'd execute twice
  // / stack duplicate nodes), only fetch what never landed.
  const firstScript = assetUrl(TEST_ORIGIN, CONSOLE_SCRIPTS[0]);
  const s = setupStateful({ failUrls: [firstScript] });
  try {
    await openConsolePanel(); // fails at the first script
    assert.equal(s.dom.screen.hidden, true);
    s.dom.fail.delete(firstScript); // outage over
    await openConsolePanel(); // retry
    assert.equal(s.dom.screen.hidden, false, "retry succeeds and reveals");
    assert.equal(s.ui.mounts.length, 1, "the retry mounts exactly once");
    for (const style of CONSOLE_STYLES) {
      assert.equal(s.dom.liveNodesFor(assetUrl(TEST_ORIGIN, style)).length, 1, style + " live exactly once - never duplicated by the retry");
    }
    for (const script of CONSOLE_SCRIPTS) {
      assert.equal(s.dom.liveNodesFor(assetUrl(TEST_ORIGIN, script)).length, 1, script + " live exactly once");
    }
    assert.equal(s.dom.head.children.length, CONSOLE_STYLES.length + CONSOLE_SCRIPTS.length, "head holds one node per asset, no dead leftovers");
  } finally {
    s.teardown();
  }
});

test("P4 reopen during the close's IPC round-trip: the click opens, and no ack can clobber the newer session", async () => {
  // PR #45's last open finding: closeConsolePanel() fires
  // console_panel_closed fire-and-forget, so there is a round-trip window
  // where the panel is already closed but Rust has not processed the ack.
  // This test pins BOTH halves of the contract that makes that window
  // harmless by construction:
  //   1. the frontend side - an immediate reopen must mount a fresh
  //      console, never a swallowed click (Rust keeps no pending flag
  //      that could veto the second open event), and
  //   2. the protocol side - each close ack carries the session id of the
  //      open it closes, so Rust can classify ack #1 as stale (superseded
  //      by open #2) instead of restoring the window size under the
  //      freshly reopened panel.
  // If someone reintroduces a Rust- or JS-side "pending" mirror that
  // swallows the reopen, assertion (a) fails; if the session echo is
  // dropped, assertion (b) fails.
  const s = setupStateful();
  try {
    await openConsolePanel({ session: 1 });
    assert.equal(s.dom.screen.hidden, false);
    closeConsolePanel(); // hides + destroys synchronously; ack in flight, NOT awaited
    // THE RACE: the operator clicks console again before Rust could have
    // processed console_panel_closed (session 1). Rust emits a fresh open
    // event (session 2) - this must open, not no-op.
    await openConsolePanel({ session: 2 });
    assert.equal(s.dom.screen.hidden, false, "(a) the reopen during the close's round-trip must reveal the panel");
    assert.equal(s.ui.mounts.length, 2, "a fresh mount, not a swallowed click");
    assert.equal(s.ui.mounted.filter((m) => !m.destroyed).length, 1, "exactly one live mount");
    assert.equal(s.ui.mounted[0].destroyed, true, "cycle 1's teardown really ran");

    // Closing cycle 2 must ack session 2 - Rust's LIVE session - so the
    // restore is honored; ack #1 (session 1) is already identifiable as
    // stale by inequality in Rust's PanelSizeState (pinned Rust-side in
    // console.rs's stale_ack_superseded_by_a_newer_open_never_restores).
    closeConsolePanel();
    const acks = s.deps.invokeCalls
      .filter(([cmd]) => cmd === "console_panel_closed")
      .map(([, args]) => args);
    assert.deepEqual(acks, [{ session: 1 }, { session: 2 }], "(b) each close acks its own open's session id");
    assert.equal(s.ui.mounted.filter((m) => !m.destroyed).length, 0);
  } finally {
    s.teardown();
  }
});

test("P5 superseded open events still update the session the eventual close acks", async () => {
  // The half of openConsolePanel's payload bookkeeping P4 cannot pin: P4
  // opens at most once per cycle, but Rust does no open-dedupe of its own
  // (the mirror is gone), so a double click - or button + tray menu firing
  // console-open-panel twice - reaches this module as TWO events with
  // different session ids. The second is an OPEN no-op in both shapes it
  // can arrive in: while the first open is still loading (attaches to the
  // in-flight openingPromise) or once the panel is fully open (the
  // isConsolePanelOpen() early-return). In BOTH shapes openSession must
  // still advance, because the eventual closeConsolePanel() acks whatever
  // openSession holds - if it names the superseded session, Rust's
  // panel_closed classifies it stale and the main window stays grown
  // forever. Reordering the session update below either early-return
  // ("simplifying" it into the branch that actually opens) is exactly the
  // mutation this test exists to kill.
  const s = setupStateful();
  try {
    // Timing 1 - second event while the first open is still loading:
    // isConsolePanelOpen() is false, so the no-op shape is "attach to the
    // in-flight open". The session must still advance to 2.
    const first = openConsolePanel({ session: 1 });
    const second = openConsolePanel({ session: 2 });
    assert.equal(second, first, "the superseded call attaches to the in-flight open, no second mount");
    await first;
    closeConsolePanel();
    assert.deepEqual(
      s.deps.invokeCalls.filter(([cmd]) => cmd === "console_panel_closed").map(([, args]) => args),
      [{ session: 2 }],
      "the close acks the LATEST stamped session, not the one whose open did the loading"
    );

    // Timing 2 - second event once the panel is fully OPEN: now the
    // isConsolePanelOpen() early-return is what makes the call a no-op,
    // and it is the early-return a reorder would strand the session
    // update behind. Reopen (fresh cycle, session 3), then a superseded
    // event (session 4) lands on the already-open panel.
    await openConsolePanel({ session: 3 });
    assert.equal(s.ui.mounts.length, 2, "the reopen after close is a fresh mount (assets cached)");
    openConsolePanel({ session: 4 }); // already open: pure no-op - session must STILL advance
    assert.equal(s.ui.mounts.length, 2, "the superseded event must not mount anything");
    closeConsolePanel();
    const acks = s.deps.invokeCalls
      .filter(([cmd]) => cmd === "console_panel_closed")
      .map(([, args]) => args);
    assert.deepEqual(acks, [{ session: 2 }, { session: 4 }], "each close acks the session Rust considers live at that moment, never a superseded one");
  } finally {
    s.teardown();
  }
});

// ---------------------------------------------------------------------------
// hydration - the console must not start blind to what already happened
// before it existed to hear it (2026-08-16, reg_state/blf snapshot fix)
// ---------------------------------------------------------------------------

test("H1 mount hydrates the store from get_reg_state + get_blf_states, after mounting", async () => {
  const s = setupStateful({
    regState: { state: "registered", transport: "wss", account: "sip:101@pbx.example", reason: null },
    blfStates: { "101": "idle", "102": "busy" },
  });
  try {
    await openConsolePanel();
    const store = s.ui.mounted[0].store;
    assert.deepEqual(store.ingestCalls, [
      { event: "reg_state", state: "registered", transport: "wss", account: "sip:101@pbx.example", reason: null },
      { event: "blf", ext: "101", state: "idle" },
      { event: "blf", ext: "102", state: "busy" },
    ]);
    // No failure -> no visible banner, nothing reported.
    assert.equal(s.deps.showBannerCalls.length, 0);
    assert.equal(s.deps.reportCalls.length, 0);
  } finally {
    s.teardown();
  }
});

test("H2 mount with nothing to hydrate (fresh engine, no favorites) ingests nothing and stays quiet", async () => {
  const s = setupStateful({ regState: null, blfStates: {} });
  try {
    await openConsolePanel();
    const store = s.ui.mounted[0].store;
    assert.deepEqual(store.ingestCalls, [], "null reg_state and an empty BLF map ingest nothing, not a malformed event");
    assert.equal(s.deps.showBannerCalls.length, 0);
  } finally {
    s.teardown();
  }
});

test("H3 get_reg_state failure: reported + banner shown, but the panel stays open and BLF hydration still runs", async () => {
  const s = setupStateful({
    blfStates: { "101": "idle" },
    failCommands: ["get_reg_state"],
  });
  try {
    await openConsolePanel();
    assert.equal(s.dom.screen.hidden, false, "a hydration failure must never take the panel down");
    const store = s.ui.mounted[0].store;
    assert.deepEqual(store.ingestCalls, [{ event: "blf", ext: "101", state: "idle" }], "the OTHER snapshot still hydrates independently");
    assert.deepEqual(s.deps.reportCalls.map(([kind]) => kind), ["console_panel_hydrate_failed"]);
    assert.equal(s.deps.reportCalls[0][1].source, "get_reg_state");
    assert.equal(s.deps.showBannerCalls.length, 1);
    assert.equal(s.deps.showBannerCalls[0][1], "info", "a hydration hiccup is informational, not an error banner - the panel still works");
  } finally {
    s.teardown();
  }
});

test("H4 get_blf_states failure: reported + banner shown, reg_state hydration still applied", async () => {
  const s = setupStateful({
    regState: { state: "registered", transport: "udp", account: null, reason: null },
    failCommands: ["get_blf_states"],
  });
  try {
    await openConsolePanel();
    const store = s.ui.mounted[0].store;
    assert.deepEqual(store.ingestCalls, [{ event: "reg_state", state: "registered", transport: "udp", account: null, reason: null }]);
    assert.deepEqual(s.deps.reportCalls.map(([, data]) => data.source), ["get_blf_states"]);
    assert.equal(s.deps.showBannerCalls.length, 1);
  } finally {
    s.teardown();
  }
});

test("H5 both snapshot commands fail: exactly one banner (not one per failure), panel still open", async () => {
  const s = setupStateful({ failCommands: ["get_reg_state", "get_blf_states"] });
  try {
    await openConsolePanel();
    assert.equal(s.dom.screen.hidden, false);
    assert.deepEqual(s.deps.reportCalls.map(([, data]) => data.source), ["get_reg_state", "get_blf_states"], "both failures are individually logged/reported");
    assert.equal(s.deps.showBannerCalls.length, 1, "one summarizing banner, not a banner storm");
  } finally {
    s.teardown();
  }
});

test("H6 the live listener is confirmed registered before any snapshot/roster fetch runs", async () => {
  // Regression net for the ordering fix: deps.listen()'s returned promise
  // must be awaited BEFORE get_favorites/get_reg_state/get_blf_states -
  // otherwise a live event racing the (real, async) Tauri listener
  // registration could be lost with nothing subscribed yet to catch it.
  // A deferred listen() lets this test observe the invariant directly:
  // while it's pending, NOTHING past it may have been invoked yet.
  const s = setupStateful();
  try {
    let resolveListen;
    const originalListen = s.deps.listen;
    s.deps.listen = (event, handler) => {
      s.deps.listenCalls.push([event, handler]);
      return new Promise((resolve) => {
        resolveListen = () => resolve(() => {});
      });
    };
    const opening = openConsolePanel();
    // Poll (never a fixed tick count - asset loading takes a variable
    // number of microtask turns) until bridge.init() itself has actually
    // run and called deps.listen(); that call is synchronous with
    // bridge.init, so its presence means we're exactly at the point right
    // before `await listenerReady` - the earliest moment worth asserting.
    for (let i = 0; i < 1000 && s.deps.listenCalls.length === 0; i++) {
      await Promise.resolve();
    }
    assert.equal(s.deps.listenCalls.length, 1, "sanity: the listener registration itself did happen");
    assert.deepEqual(
      s.deps.invokeCalls,
      [],
      "get_favorites/get_reg_state/get_blf_states must not fire until the listener is confirmed live"
    );
    resolveListen();
    await opening;
    assert.ok(s.deps.invokeCalls.some(([cmd]) => cmd === "get_favorites"), "the fetches proceed once the listener is live");
    s.deps.listen = originalListen;
  } finally {
    s.teardown();
  }
});

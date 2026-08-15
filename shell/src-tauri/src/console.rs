//! Premium "console" window: the receptionist BLF console
//! (`premium/console-ui` in the private repo) embedded as its own Tauri
//! window.
//!
//! # Why a custom URI scheme instead of `frontendDist`
//!
//! `tauri.conf.json`'s `frontendDist` (`../ui`) is bundled straight into
//! every build, including a plain Community `git clone` + `cargo build` -
//! anything placed there ships in the public repo. The console-ui package
//! is premium and must never ship there (see
//! `premium/docs/loader-integration.md`'s sibling doc on the console, and
//! this repo's own workspace `CLAUDE.md`: "premium UI assets must NOT
//! ship in the public repo"). So this module never bundles console-ui's
//! files at all - it registers a custom `premium-console://` protocol
//! (see [`asset_protocol_handler`]) that reads them, at runtime, from a
//! directory *beside the running executable* - the exact same "next to
//! the exe" convention `centinelo_premium_abi::expected_library_path`
//! already uses for the signed dylib (see `premium.rs`), so one packaging
//! step drops the dylib, its `.sig`, and the console-ui asset tree in the
//! same place. On a Community build (or an official build before that
//! directory exists), the protocol handler simply 404s - harmless, since
//! the window is never created in the first place (see
//! [`open_or_focus`]'s gating).
//!
//! # The inline panel is the default now (2026-02)
//!
//! [`open`] - what `commands::open_console` and the tray both call - by
//! default tells the MAIN window to show the console as an in-app panel
//! (`#screen-console`, the exact `#screen-settings` pattern in
//! `ui/index.html`) instead of building a second window. Root cause this
//! retires: the separate window went blank on the owner's Windows machine
//! for two days while Settings - same webview, same document, just a
//! `div` - kept working, so the console now lives in that same document.
//! The panel loads the same runtime assets over this same scheme
//! (`tauri.conf.json`'s `script-src`/`style-src` allow `premium-console:`;
//! see `docs/console-inline-panel-decision.md` for the full option
//! analysis), and the license gate moved INTO
//! [`asset_protocol_handler`] - a 404 for unlicensed requests, strictly
//! stronger than the window era's "the window is never created" argument,
//! which an inline panel wouldn't have had. [`SEPARATE_WINDOW_ENV`]`=1`
//! opts back into the old separate window during the transition:
//! [`open_or_focus`] and everything it documents (the watchdog, the
//! trapped-window safety nets, [`INDEX_HTML`]) are kept intact for it.
//!
//! Only `index.html` is NOT read from that directory: it's a small,
//! wholly-generic wrapper (a handful of `<script src>` tags in
//! `dev/mock.html`'s own documented dependency order, plus the
//! shell-integration glue `premium/console-ui/README.md` describes -
//! "Option B", matching this file's own per-verb `commands.rs`
//! convention) with **zero console-ui-specific content** - so it's
//! embedded directly in this public source file ([`INDEX_HTML`]) rather
//! than also having to live in the runtime-only assets directory. Every
//! byte of the actual premium UI (`tokens.css`, `console.css`,
//! `components/*.js`, `store/ConsoleStore.js`, `bridge/EngineBridge.js`,
//! `console-app.js`) still only ever comes from the runtime directory.
//!
//! # Populating the assets directory
//!
//! Dev/test: copy `premium/console-ui/src/*` (private repo) verbatim,
//! preserving its internal `components/`/`store`/`bridge/` structure,
//! into the directory [`assets_dir`] resolves to (or point
//! `CENTINELO_PREMIUM_ASSETS_DIR` at wherever you put it). Official
//! installer layout: see `shell/README.md` "Premium console assets".

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::frontend_log::{self, FrontendErrorReport};
use crate::premium::{CapabilityStatusView, PremiumHandle};

/// Custom URI scheme the "console" window loads from - see this module's
/// doc. Registered once, unconditionally, on the app `Builder` (see
/// `lib.rs`); harmless when nothing is ever there to serve.
pub const ASSET_SCHEME: &str = "premium-console";

/// Window label for the console - used to find-or-create it
/// ([`open_or_focus`]) and by `capabilities/console.json` to scope its
/// IPC permissions.
pub const WINDOW_LABEL: &str = "console";

/// The [`centinelo_premium_abi::Capability::feature_name`] this window is
/// gated behind.
const CAPABILITY: &str = "blf_console";

const ASSETS_DIR_ENV: &str = "CENTINELO_PREMIUM_ASSETS_DIR";
const ASSETS_DIR_NAME: &str = "premium-console-assets";

/// Opts back into the legacy separate console window during the
/// transition - see this module's "inline panel is the default" doc. Only
/// the exact value `1` counts; anything else (including unset) means the
/// inline panel.
pub const SEPARATE_WINDOW_ENV: &str = "CENTINELO_CONSOLE_SEPARATE_WINDOW";

/// The [`ASSET_SCHEME`]-scoped URL the legacy separate window navigates
/// to on first open, in the shape the current platform actually resolves.
/// See [`open_or_focus`]'s doc for why a single hardcoded shape broke
/// Windows. `windows_or_android_scheme_host` should always be
/// `cfg!(any(target_os = "windows", target_os = "android"))` at the real
/// call site; it's a parameter here purely so both branches are testable
/// without cross-compiling (`cfg!` bakes in the CURRENT build's target,
/// so a unit test running on macOS can never exercise the Windows arm any
/// other way).
fn console_index_url(windows_or_android_scheme_host: bool) -> String {
    if windows_or_android_scheme_host {
        format!("http://{ASSET_SCHEME}.localhost/index.html")
    } else {
        format!("{ASSET_SCHEME}://localhost/index.html")
    }
}

/// Event [`open_panel`] emits (app-wide) to tell the main window to show
/// the inline console panel. `ui/js/console-panel.js` listens for it; only
/// [`open`] ever emits it, always after its own license re-check, so no
/// other code path can talk the frontend into opening the panel. The
/// payload carries a monotonic `session` id which the panel echoes back in
/// `console_panel_closed`, so a close ack can always be told apart from a
/// newer open - see [`PanelSizeState`].
pub const PANEL_OPEN_EVENT: &str = "console-open-panel";

/// Logical size the main window is grown to when the console panel opens,
/// if it is currently smaller (a larger window is never shrunk). Sized
/// for the console-ui package's own layout: `rail` 56px + a `main` grid
/// of `minmax(150px,1fr)` tiles + `side` 288px (its console.css) - at the
/// main window's 380px default that layout collapses to a zero-width
/// grid, which is why the panel grows the window it lives in. See
/// `docs/console-inline-panel-decision.md` §6.
const PANEL_TARGET_SIZE: (f64, f64) = (900.0, 640.0);

/// Minimum logical size forced on the main window while the panel is open
/// - the console-ui package's own `.cent-console.win` floor, verbatim.
const PANEL_MIN_SIZE: (f64, f64) = (760.0, 520.0);

/// The main window's configured minimum (`tauri.conf.json` `windows[0]`
/// `minWidth`/`minHeight`), restored when the panel closes. Tauri exposes
/// no getter for a window's live min constraints, so the restore value is
/// this mirror; the only writers of min-size besides this module are that
/// config and [`PANEL_MIN_SIZE`] below.
const MAIN_DEFAULT_MIN_SIZE: (f64, f64) = (340.0, 560.0);

/// Everything Rust remembers about the inline panel, in one guarded
/// struct - and deliberately NOT a "panel is open" flag. The
/// authoritative source of that fact is the main window's own frontend:
/// `ui/js/console-panel.js`'s `openConsolePanel()` refuses to start a
/// second open while one is in flight or showing **synchronously**
/// (`openingPromise` is stored before its first `await` can yield, so JS
/// run-to-completion makes check-and-set atomic; `isConsolePanelOpen`
/// covers the already-showing case), and that guard is pinned by tests
/// (console-panel.test.js P3-2/P3-3/P4). This module used to ALSO keep a
/// `PANEL_OPEN_PENDING` bool as "defense in depth", but such a bool is a
/// *mirror* of the frontend's state, and a lagging one: it only ever
/// learned "closed" via the `console_panel_closed` round-trip, so for
/// that entire round-trip it held `true` while the panel was already
/// gone - and `open_panel`, trusting the mirror over the source, answered
/// the operator's reopen click with a silent no-op. That is the exact
/// shape of the BLF teardown bug this project already paid for (trusting
/// a NOTIFY cache over the authoritative source), so the mirror is gone:
/// `open_panel` always emits, and the frontend's guard is the one and
/// only open-dedupe.
///
/// What Rust genuinely must remember is *sizing* - grow the window on
/// open, restore it on close - and sizing still has to learn "closed"
/// across the `console_panel_closed` round-trip. That residual race is
/// closed with identity, not timing: every open stamps a fresh session
/// id into the event payload, the close ack echoes it back, and
/// [`panel_closed`] ignores any ack naming a session a newer open has
/// already superseded (a late ack would otherwise shrink the window
/// right back under a freshly reopened panel and consume the restore
/// baseline while at it). See [`CloseOutcome`].
struct PanelSizeState {
    /// Session id stamped into the last [`PANEL_OPEN_EVENT`] payload
    /// (`0` = never opened). Monotonic: advanced on every open and on
    /// every honored close, so a given ack matches at most one close.
    session: u64,
    /// Logical size the main window had before the panel grew it,
    /// restored by [`panel_closed`]. `None` = nothing to restore, which
    /// also makes a second `open_console` before any close a sizing no-op
    /// (the first pre-panel size is the one true baseline; it must not be
    /// overwritten with the already-grown size).
    restore: Option<(f64, f64)>,
}

static PANEL_SIZE_STATE: Mutex<PanelSizeState> = Mutex::new(PanelSizeState {
    session: 0,
    restore: None,
});

/// Pure decision half of [`panel_closed`] - what a close ack means for
/// [`PanelSizeState`], with no window access, so the interleaving
/// invariants are unit-testable (`mod tests`).
#[derive(Debug, PartialEq)]
enum CloseOutcome {
    /// The ack names the live session: reset the window minimum and
    /// restore this size (`None` = the open never captured a size; the
    /// minimum reset still applies). Advances the session so a duplicate
    /// ack is stale, never a second restore.
    Restore(Option<(f64, f64)>),
    /// The ack names a session a newer open already superseded: the
    /// window that newer open grew must NOT be shrunk back under it.
    Stale { ack: u64, current: u64 },
    /// The ack carried no session identity (a defensive close with
    /// nothing ever opened - the frontend never saw an open event, so
    /// nothing was ever grown on its behalf).
    Anonymous,
}

impl PanelSizeState {
    /// Pure half of `open_panel`'s grow: stamp the next session id and
    /// remember the pre-panel baseline exactly once per grow era - a
    /// reopen before any close (the exact window this module's history is
    /// about) keeps the FIRST baseline, since the window is still grown
    /// from it.
    fn begin_open(&mut self, current: Option<(f64, f64)>) -> u64 {
        self.session += 1;
        if self.restore.is_none() {
            self.restore = current;
        }
        self.session
    }

    /// Pure half of `panel_closed` - see [`CloseOutcome`].
    fn close(&mut self, ack: Option<u64>) -> CloseOutcome {
        let Some(ack) = ack else {
            return CloseOutcome::Anonymous;
        };
        if ack != self.session {
            return CloseOutcome::Stale {
                ack,
                current: self.session,
            };
        }
        self.session += 1;
        CloseOutcome::Restore(self.restore.take())
    }
}

/// Log target for everything console-window-specific, deliberately
/// distinct from `frontend_log.rs`'s `"app_lib::frontend"` (the *main*
/// window's own crash-report target). Born from a real mix-up during this
/// bug's own diagnosis: an earlier session remote-debugged a window titled
/// "Centinelo Console" believing it was the main window, which cost hours.
/// Every log line this module emits also starts with the literal
/// `[console]` tag for the same reason, in case a future reader is
/// scanning a flat log file without `RUST_LOG` target filtering.
const LOG_TARGET: &str = "app_lib::console";

/// How long [`open_or_focus`] waits for the console-ui bundle to call
/// [`mark_ready`] (`console_frontend_ready`, wired at the end of
/// `INDEX_HTML`'s `mount()`) before concluding the window is never going
/// to render anything and closing it - see this module's "trapped window"
/// doc on [`open_or_focus`] for why an undecorated window that fails to
/// load must never just sit there. Generous on purpose: `get_favorites`
/// (the one IPC round-trip `boot()` waits on before `mount()`) is a local
/// settings-file read, not a network call, so a healthy boot finishes in
/// well under a second - this is sized for a cold machine under load, not
/// the happy path.
const LOAD_TIMEOUT: Duration = Duration::from_secs(8);

/// Whether the console-ui bundle has confirmed it actually rendered
/// (`mark_ready`) for the *current* window instance - reset to `false`
/// every time [`open_or_focus`] builds a fresh window. A plain module-level
/// flag is enough because [`WINDOW_LABEL`] is a singleton: `open_or_focus`
/// itself never creates a second "console" window, it finds-or-focuses the
/// existing one instead.
static READY: AtomicBool = AtomicBool::new(false);

/// Whether this window instance's "never leave the operator trapped"
/// handling has already fired - guards [`close_after_failure`] against
/// running twice (a fatal frontend report racing the [`LOAD_TIMEOUT`]
/// watchdog, or either racing a window the operator already closed
/// themselves via Escape) and, separately, suppresses a spurious
/// load-failure banner when the window closes normally before ever
/// signalling ready (see the `Destroyed` handler in [`open_or_focus`]).
static FAILURE_HANDLED: AtomicBool = AtomicBool::new(false);

/// Marks the current console window as having actually rendered -
/// `commands::console_frontend_ready` (invoked by `INDEX_HTML`'s
/// `mount()`, both on a real roster and on the `get_favorites` failure
/// fallback - an empty roster still means the window itself works) calls
/// this. Disarms both the load-timeout watchdog and
/// [`report_frontend_fatal`]'s window-closing behavior for the rest of
/// this window's lifetime - a *later* JS error (e.g. a runtime bridge hiccup
/// once the operator is already using the console) is a bug worth logging,
/// not a reason to slam a working window shut.
pub fn mark_ready() {
    READY.store(true, Ordering::SeqCst);
    log::info!(target: LOG_TARGET, "[console] frontend signalled ready");
}

/// `commands::console_frontend_fatal` - the console window's counterpart to
/// `frontend_log::log_frontend_error`, reached by the early inline capture
/// script at the top of `INDEX_HTML` (armed before any vendored console-ui
/// script runs, same "before ANYTHING else can fail" reasoning as
/// `ui/js/error-capture.js`'s own doc comment). Unlike the main window's
/// version, this one can actually act: a pre-`mark_ready` report means the
/// console-ui bundle failed to boot, and this undecorated window has no OS
/// chrome of its own to fall back on - see [`close_after_failure`].
pub fn report_frontend_fatal(app: &AppHandle, report: FrontendErrorReport) {
    let line = frontend_log::format_log_line(&report);
    log::error!(target: LOG_TARGET, "[console] {line}");
    if !READY.load(Ordering::SeqCst) {
        close_after_failure(app, "frontend_error");
    }
}

/// Closes the console window and tells the main window why, exactly once
/// per window instance ([`FAILURE_HANDLED`]) - the shared tail end of both
/// [`report_frontend_fatal`] and the [`LOAD_TIMEOUT`] watchdog in
/// [`open_or_focus`]. `ui/js/app.js` listens for `"console-load-failed"`
/// (mirrors `tray.rs`'s `sync_availability_menu` broadcast-and-let-the-
/// window-that-cares-listen pattern) and shows a banner - the console
/// itself cannot, by construction, since the whole point is that its own
/// content never rendered.
fn close_after_failure(app: &AppHandle, reason: &str) {
    if FAILURE_HANDLED.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.close();
    }
    let _ = app.emit(
        "console-load-failed",
        serde_json::json!({ "reason": reason }),
    );
}

/// Spawned once per fresh window build in [`open_or_focus`] - same
/// plain-thread-plus-flag shape as `sidecar.rs`'s
/// `arm_force_kill_watchdog` (this crate's existing convention for "wait,
/// then act only if nothing cleared the flag in time" rather than pulling
/// in an async timer). If [`mark_ready`] never fires within
/// [`LOAD_TIMEOUT`] - no error was even thrown, the bundle just silently
/// never got there - this is the backstop that guarantees the window
/// closes anyway; [`report_frontend_fatal`] and the operator's own Escape
/// key (wired client-side in `INDEX_HTML`) are the two faster paths that
/// usually win the race.
fn spawn_load_watchdog(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(LOAD_TIMEOUT);
        if !READY.load(Ordering::SeqCst) {
            log::error!(
                target: LOG_TARGET,
                "[console] frontend did not signal ready within {LOAD_TIMEOUT:?} — closing the window instead of leaving an undecorated, uncloseable blank window onscreen"
            );
            close_after_failure(&app, "timeout");
        }
    });
}

/// Whether a [`CapabilityStatusView`] means "the console-ui feature this
/// shell implements should be offered" - i.e. whether the premium license
/// gate cleared, independent of whether `centinelo-premium`'s *own* FFI
/// stub behind the capability happens to be implemented yet.
///
/// # Why this isn't just `status == Available`
///
/// Early v0 builds of `centinelo-premium`'s `capability_status_for`
/// (private repo, `crates/centinelo-premium/src/license.rs`) resolved
/// *every* licensed capability to `NotImplemented`, never `Available` -
/// none of v0's three capabilities (`blf_console`, `transcription`,
/// `recording`) had real behavior behind its FFI stub yet, by design (see
/// that function's own doc history: "all of v0's capabilities resolve
/// here once they clear the license gate"). That's since changed for
/// `blf_console` specifically - the tech debt item tracked in this
/// workspace's `docs/HANDOFF.md` ("dylib v0 reporta NotImplemented pa
/// blf_console — debe pasar a Available") - so a current dylib can now
/// answer either `Available` or `NotImplemented` for a licensed
/// `blf_console`, depending on build. This function accepts both on
/// purpose, not just for historical-build compatibility: see the next
/// paragraph for why `NotImplemented` alone was never actually "not
/// ready", even before that fix landed.
///
/// The console **window** this module builds is not one of
/// `centinelo-premium`'s FFI capabilities at all - it's implemented
/// entirely in this shell plus the vendored console-ui package, talking
/// to the sidecar directly (see `commands.rs`'s `sidecar_*` verbs).
/// `centinelo-premium`'s only role is the license *probe*: "does the
/// active license include `blf_console`". Gating window visibility on
/// only the literal `Available` discriminant would have made the console
/// unable to open under any pre-fix dylib build, in any configuration,
/// including a founder license - which would have made the e2e scenario
/// this integration is required to prove (`shell/E2E.md` "F4 premium",
/// scenario (c): "valid dylib + founder license -> console available,
/// opens") structurally impossible to pass on those builds. Treating
/// either status as "gate cleared, offer the shell-side feature" is what
/// `centinelo_premium_abi::Capability`'s own crate doc describes as the
/// contract: "the shell never decides whether a capability is licensed,
/// it only ever asks the dylib" - the dylib's answer *is* "yes,
/// licensed" either way; `NotImplemented` was only ever its honest report
/// that a given build's own stub had nothing behind it yet, which is
/// irrelevant to whether *this shell's* console window should be
/// offered.
///
/// `NotLicensed` and `Unavailable` (no dylib loaded, tampered signature,
/// FFI error, unrecognized capability name) both still hide the console,
/// exactly as before - this only changes how a *cleared* gate is read.
fn unlocks_console(status: CapabilityStatusView) -> bool {
    matches!(
        status,
        CapabilityStatusView::Available | CapabilityStatusView::NotImplemented
    )
}

/// Whether the console window/menu entry should be offered right now.
/// Single source of truth for both gating surfaces: `tray.rs`'s menu
/// construction, and the frontend's own `premium_capability_status` check
/// (see `commands::premium_capability_status`), plus the defense-in-depth
/// check inside [`open_or_focus`] itself.
pub fn is_unlocked(premium: &PremiumHandle) -> bool {
    unlocks_console(premium.capability_status(CAPABILITY))
}

/// Entry point for both console surfaces (the tray menu item and
/// `commands::open_console`): routes to the inline panel (default) or the
/// legacy separate window ([`SEPARATE_WINDOW_ENV`]), re-checking the
/// license gate either way - see [`open_or_focus`]'s doc for why a plain
/// IPC command must never trust that its caller already checked.
pub fn open(app: &AppHandle) -> Result<(), String> {
    if separate_window_mode() {
        open_or_focus(app)
    } else {
        open_panel(app)
    }
}

/// Whether [`SEPARATE_WINDOW_ENV`] opts into the legacy separate window.
fn separate_window_mode() -> bool {
    std::env::var(SEPARATE_WINDOW_ENV).is_ok_and(|v| v == "1")
}

/// Inline-panel path: re-check the license (same reasoning as
/// [`open_or_focus`] - the button and tray item are hidden without it,
/// but `open_console` is still a plain IPC command any webview can
/// invoke), grow the main window to the console's layout floor, then emit
/// [`PANEL_OPEN_EVENT`] with a fresh session id. Loading the assets and
/// mounting console-ui is `ui/js/console-panel.js`'s job; every request
/// it makes for console-ui code goes back through
/// [`asset_protocol_handler`], which enforces the same license gate again
/// - defense in depth: absent button, gated command, gated bytes.
///
/// There is deliberately NO Rust-side open-dedupe here: "the panel is
/// open" is a fact about the frontend, and the frontend's own
/// single-flight guard decides it authoritatively (see
/// [`PanelSizeState`]'s doc). The old `PANEL_OPEN_PENDING` mirror used to
/// answer a reopen click with a silent `Ok(())` whenever the close ack
/// was still crossing IPC - the "button does nothing" window this
/// module must never have.
fn open_panel(app: &AppHandle) -> Result<(), String> {
    let premium = app
        .try_state::<PremiumHandle>()
        .ok_or_else(|| "premium module not initialized".to_string())?;
    if !is_unlocked(&premium) {
        return Err("premium console is not licensed".to_string());
    }
    log::info!(target: LOG_TARGET, "[console] opening inline panel");
    let session = grow_main_window_for_panel(app);
    app.emit(PANEL_OPEN_EVENT, serde_json::json!({ "session": session }))
        .map_err(|e| e.to_string())
}

/// Called by `commands::console_panel_closed` once the frontend has hidden
/// the inline panel (after its own `app.destroy()` teardown of the mounted
/// console-ui: store stop, BLF unsubscriptions, tick interval): restores
/// the main window's pre-panel size and its configured minimum.
/// `session` is the id [`open_panel`] stamped into the open event being
/// closed; an ack naming a session a newer open already superseded is
/// ignored (and logged) instead of restoring - see [`PanelSizeState`].
pub fn panel_closed(app: &AppHandle, session: Option<u64>) {
    let outcome = {
        let mut state = PANEL_SIZE_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.close(session)
    };
    let restore = match outcome {
        CloseOutcome::Restore(restore) => restore,
        CloseOutcome::Stale { ack, current } => {
            log::info!(
                target: LOG_TARGET,
                "[console] panel-closed ack for superseded session {ack} (current {current}) ignored - a newer open already re-grew the window"
            );
            return;
        }
        CloseOutcome::Anonymous => {
            log::info!(
                target: LOG_TARGET,
                "[console] panel-closed ack carried no session id - nothing to restore"
            );
            return;
        }
    };
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.set_min_size(Some(tauri::LogicalSize::new(
        MAIN_DEFAULT_MIN_SIZE.0,
        MAIN_DEFAULT_MIN_SIZE.1,
    )));
    if let Some((w, h)) = restore {
        let _ = window.set_size(tauri::LogicalSize::new(w, h));
    }
}

/// Grows the main window to [`PANEL_TARGET_SIZE`] and lifts its minimum to
/// [`PANEL_MIN_SIZE`] for as long as the console panel is open - see
/// [`PANEL_TARGET_SIZE`]'s doc for why the panel needs the room. Stamps
/// and returns the new session id for [`PANEL_OPEN_EVENT`]'s payload,
/// recording the pre-open baseline exactly once per grow era, in logical
/// pixels, so the restore is exact on any display scale factor. The state
/// lock is held across the window calls too: [`panel_closed`] takes the
/// same lock, so a close ack can never interleave between this stamp and
/// the grow it belongs to - the two sides' size changes are submitted in
/// a well-defined order.
fn grow_main_window_for_panel(app: &AppHandle) -> u64 {
    let mut state = PANEL_SIZE_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let window = app.get_webview_window("main");
    let current = window.as_ref().and_then(|w| {
        let scale = w.scale_factor().unwrap_or(1.0);
        w.inner_size().ok().map(|phys| {
            let logical: tauri::LogicalSize<f64> = phys.to_logical(scale);
            (logical.width, logical.height)
        })
    });
    let session = state.begin_open(current);
    if let Some(window) = window {
        let _ = window.set_min_size(Some(tauri::LogicalSize::new(
            PANEL_MIN_SIZE.0,
            PANEL_MIN_SIZE.1,
        )));
        if let Some((w, h)) = current {
            let _ = window.set_size(tauri::LogicalSize::new(
                w.max(PANEL_TARGET_SIZE.0),
                h.max(PANEL_TARGET_SIZE.1),
            ));
        }
    }
    session
}

/// Finds the already-open console window and focuses it, or builds a
/// fresh one. Re-checks the license gate itself (not just trusting that a
/// caller already checked) - the tray menu item and the main window's
/// button are both gated on [`is_unlocked`] before they're ever shown
/// (see `tray.rs`, `ui/js/app.js`), but `commands::open_console` is a
/// plain IPC command a webview could still invoke directly (e.g. via
/// devtools) even with the button hidden - the *window itself* is the
/// thing that must never appear unlicensed, not merely the button that
/// usually opens it.
///
/// # The trapped-window problem this also guards against
///
/// `.decorations(false)` below means this window has **no OS titlebar at
/// all** - its close/minimize controls are hand-drawn DOM elements wired
/// by `INDEX_HTML`'s own JS (see that constant's `mount()`). If the
/// console-ui bundle this window loads over [`ASSET_SCHEME`] fails to
/// render for any reason (a missing/corrupt `premium-console-assets/`
/// file, a runtime exception in one of the vendored scripts, anything),
/// there is nothing else in the window an operator could click, drag, or
/// even see - a completely blank, completely uncloseable window sitting on
/// top of the app, recoverable only via Alt+F4/Cmd+Q, which no user is
/// going to guess. Three independent, redundant safety nets exist so this
/// can never actually happen, listed fastest-to-slowest:
///
/// 1. **Escape key** - wired in `INDEX_HTML`'s very first inline
///    `<script>`, before any vendored console-ui file is even requested.
///    Works even if every single one of those files 404s.
/// 2. **[`report_frontend_fatal`]** - the same early script's
///    `window.addEventListener("error"/"unhandledrejection", ..., true)`
///    reports the first fatal failure (a failed script/style load, or an
///    uncaught exception/rejection) straight to Rust, which closes the
///    window immediately and tells the main window why.
/// 3. **[`spawn_load_watchdog`]** - the backstop for the case where
///    neither of the above fires at all (e.g. the webview's JS engine
///    itself never runs anything) - closes the window unconditionally
///    after [`LOAD_TIMEOUT`] if [`mark_ready`] never arrived.
///
/// None of this changes anything about how the window looks or behaves
/// when the console-ui bundle actually loads (the common case, and the one
/// the Vigilia mockup's fidelity criteria apply to) - `mark_ready` disarms
/// both (2) and (3) for the rest of that window's lifetime the moment
/// `mount()` finishes, and (1) never had any visible side effect to begin
/// with.
///
/// # Origin shape: same bug this module's asset handler doc already
/// documents, one layer up
///
/// This is the LEGACY path (only reachable behind
/// `CENTINELO_CONSOLE_SEPARATE_WINDOW` since the 2.1.0 inline-panel
/// migration made this module's doc's `#screen-console` panel the
/// default) - but it has the identical origin-shape defect
/// `console-panel.js`'s `resolveConsoleAssetOrigin` fixes for the inline
/// panel: a `WebviewUrl::CustomProtocol` is just a `Url` handed straight
/// to wry/webview2 (`tauri` 2.11.5's `manager/webview.rs`,
/// `WebviewUrl::CustomProtocol(url) => url.clone()` with no per-OS
/// rewriting anywhere in that path), so hardcoding the macOS/Linux
/// `<scheme>://localhost/...` shape here means WebView2 never resolves it
/// on Windows either, for the same reason the inline panel's asset
/// requests never reached `asset_protocol_handler` at all. Fixed here the
/// same way Tauri's own `convertFileSrc` picks the shape (see
/// `console_index_url`'s doc) - Rust gets to do it at compile time
/// instead of at runtime, since `target_os` is known here and a webview
/// never needs to guess.
///
/// NOT verified against a real Windows build - this path is unused in
/// the shipped 2.1.0 default (inline panel) and no Windows machine has
/// exercised it since this fix. If `CENTINELO_CONSOLE_SEPARATE_WINDOW`
/// is ever revived for real use, treat this as unconfirmed until someone
/// actually opens the separate window on Windows and checks the log.
pub fn open_or_focus(app: &AppHandle) -> Result<(), String> {
    let premium = app
        .try_state::<PremiumHandle>()
        .ok_or_else(|| "premium module not initialized".to_string())?;
    if !is_unlocked(&premium) {
        return Err("premium console is not licensed".to_string());
    }

    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }

    log::info!(
        target: LOG_TARGET,
        "[console] opening window; assets_dir = {:?}",
        assets_dir()
    );

    READY.store(false, Ordering::SeqCst);
    FAILURE_HANDLED.store(false, Ordering::SeqCst);

    let url = WebviewUrl::CustomProtocol(
        console_index_url(cfg!(any(target_os = "windows", target_os = "android")))
            .parse()
            .expect("static console URL is always a valid Url"),
    );
    let window = WebviewWindowBuilder::new(app, WINDOW_LABEL, url)
        .title("Centinelo Console")
        .inner_size(1100.0, 720.0)
        .min_inner_size(900.0, 600.0)
        .resizable(true)
        .decorations(false)
        .build()
        .map_err(|e| e.to_string())?;

    // A window the operator (or one of the safety nets above) closed
    // normally, before `mark_ready` ever fired, is not a load failure -
    // don't let a watchdog that's still mid-sleep pop a stale "console
    // failed to load" banner over it after the fact.
    window.on_window_event(|event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            FAILURE_HANDLED.store(true, Ordering::SeqCst);
        }
    });

    spawn_load_watchdog(app);
    Ok(())
}

/// Resolves the directory premium console-ui assets should be served
/// from (everything except `index.html` - see this module's doc).
/// `CENTINELO_PREMIUM_ASSETS_DIR` overrides it for dev/test, same shape
/// as `sidecar.rs`'s `CENTINELO_CORE_BIN`; the default mirrors
/// `centinelo_premium_abi::expected_library_path`'s "beside the running
/// executable" convention. Returns `None` if the resolved directory
/// doesn't exist - the ordinary Community-build / not-yet-installed-Pro
/// case, handled the same "absent is fine" way `premium.rs`'s loader
/// treats a missing dylib.
fn assets_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var(ASSETS_DIR_ENV) {
        let pb = PathBuf::from(p);
        return pb.is_dir().then_some(pb);
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let dir = exe_dir.join(ASSETS_DIR_NAME);
    dir.is_dir().then_some(dir)
}

/// `register_uri_scheme_protocol` handler for [`ASSET_SCHEME`] - serves
/// [`INDEX_HTML`] for the root/`index.html` request and everything else
/// from [`assets_dir`], with a path-traversal guard (defense in depth:
/// every request this handler will ever actually receive originates from
/// this module's own trusted `INDEX_HTML` or console-ui's own script/link
/// tags, never user input, but a local protocol handler reading arbitrary
/// files off disk earns the same "verify, don't assume" care `premium.rs`
/// gives the dylib load path).
///
/// Every outcome is logged at [`LOG_TARGET`] - a blank-console bug report
/// with nothing but "it's blank" in it is nearly undiagnosable otherwise
/// (this is exactly the failure mode `README.md`'s "Frontend error
/// logging" section describes for the *main* window, which had zero
/// tracing for a day before that history repeated itself here). A real
/// Windows repro's log now says definitively whether the protocol handler
/// ever ran at all, whether `assets_dir()` resolved, and which specific
/// file 404'd - see this function's own log lines below rather than
/// guessing from the frontend's own (best-effort, only reachable if *some*
/// JS ran at all) [`report_frontend_fatal`] reports.
///
/// # CSP: investigated, ruled out
///
/// It's tempting to suspect `tauri.conf.json`'s `app.security.csp` here -
/// this window's origin (`premium-console://localhost`) is a different
/// scheme than the main window's, and CSP-blocked IPC was exactly
/// `README.md`'s "Content-Security-Policy and the IPC startup watchdog"
/// 2026-08-13 hotfix, one window class of bug up from this one. Traced
/// through `tauri` 2.11.5's own source to be sure rather than guessing
/// twice in this codebase: the configured CSP is only ever injected as an
/// HTML `<meta>` tag by `AppManager::get_asset` (`tauri::manager::mod`),
/// which is exclusively the code path behind `frontendDist`/`WebviewUrl::App`
/// windows (the main window). A window built with `WebviewUrl::CustomProtocol`
/// against an app-registered scheme - this window - never calls that
/// function; nothing in `tauri`, `tauri-runtime-wry`, or `wry` itself
/// injects a `Content-Security-Policy` header or meta tag into a plain
/// `register_uri_scheme_protocol` response. So: no CSP applies to this
/// window at all, in either direction (nothing to loosen, and nothing to
/// blame here). If a future Tauri upgrade ever changes that, the symptom
/// would most likely be `INDEX_HTML`'s big inline boot `<script>` refusing
/// to run at all (CSP blocks inline scripts without `'unsafe-inline'`/a
/// nonce) - which the early inline capture script (armed first, see
/// [`open_or_focus`]'s doc) would report as an `unhandledrejection`-free,
/// silent failure... except a CSP violation doesn't fire `window.onerror`
/// either. Worth remembering if this bug ever resurfaces after a Tauri
/// bump: check the WebView2/WKWebView devtools console directly for a
/// `Content-Security-Policy` violation line, the same way `README.md`'s
/// hotfix section was originally diagnosed.
///
/// That analysis covers the legacy separate window. Since the
/// inline-panel migration (this module's doc) the MAIN window also
/// loads console-ui code over this scheme, and the main window DOES
/// carry `tauri.conf.json`'s CSP - which is why that config now lists
/// `premium-console:` in `script-src` and `style-src` (and only
/// there: `connect-src` is untouched, the lesson of the 2.0.3 IPC
/// hotfix; no `font-src`/`img-src` entries needed - the console-ui
/// stylesheets reference no `url()`/`@font-face` at all). BOTH origin
/// shapes are listed, not just one: `premium-console:` covers the
/// macOS/Linux form (`premium-console://localhost/...`) and
/// `http://premium-console.localhost` covers the Windows/Android form
/// the frontend actually requests there (`console-panel.js`'s
/// `resolveConsoleAssetOrigin` picks the shape at runtime via Tauri's
/// own `convertFileSrc` - see its doc there); shipping only the first
/// form again would trade the white-screen-on-Windows bug for a CSP
/// violation on the exact platform the fix targets. The full option
/// trade-off - why same-origin interception isn't implementable with
/// Tauri's public API and why a `blob:` approach would weaken the CSP
/// more - is `docs/console-inline-panel-decision.md` §1.
pub fn asset_protocol_handler(
    ctx: tauri::UriSchemeContext<'_, tauri::Wry>,
    request: tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Cow<'static, [u8]>> {
    // License gate FIRST, before any disk access. Since the inline-panel
    // migration the MAIN window loads its console <script>/<link> tags
    // over this same scheme (see this module's doc), so the window-era
    // argument this handler used to lean on - "the only requester, the
    // console window, is never created unlicensed" - no longer covers
    // every request. Unlicensed requests get the same 404 a Community
    // build with no assets directory produces, so a webview that shows
    // the panel DOM by force (devtools) still never receives a byte of
    // console-ui code. A missing PremiumHandle state fails closed.
    let unlocked = ctx
        .app_handle()
        .try_state::<PremiumHandle>()
        .map(|premium| is_unlocked(&premium))
        .unwrap_or(false);
    if !unlocked {
        log::warn!(
            target: LOG_TARGET,
            "[console] 404: blf_console not licensed - refusing asset request"
        );
        return not_found();
    }

    let raw_path = request.uri().path();
    let route = raw_path.trim_start_matches('/');
    let route = if route.is_empty() {
        "index.html"
    } else {
        route
    };

    if route == "index.html" {
        log::info!(target: LOG_TARGET, "[console] serving embedded index.html");
        return html_response(INDEX_HTML);
    }

    let Some(dir) = assets_dir() else {
        log::error!(
            target: LOG_TARGET,
            "[console] 404 {route:?}: no premium-console-assets directory resolved (checked ${ASSETS_DIR_ENV} and <exe dir>/{ASSETS_DIR_NAME})"
        );
        return not_found();
    };
    let Ok(canonical_dir) = dir.canonicalize() else {
        log::error!(target: LOG_TARGET, "[console] 404 {route:?}: assets dir {dir:?} failed to canonicalize");
        return not_found();
    };
    let candidate = dir.join(route);
    let Ok(canonical) = candidate.canonicalize() else {
        log::error!(target: LOG_TARGET, "[console] 404 {route:?}: {candidate:?} does not exist (or failed to canonicalize)");
        return not_found();
    };
    if !canonical.starts_with(&canonical_dir) {
        log::warn!(target: LOG_TARGET, "[console] 404 {route:?}: asset request escaped assets dir");
        return not_found();
    }

    match std::fs::read(&canonical) {
        Ok(bytes) => {
            log::info!(target: LOG_TARGET, "[console] served {route:?} ({} bytes)", bytes.len());
            tauri::http::Response::builder()
                .header(
                    tauri::http::header::CONTENT_TYPE,
                    content_type_for(&canonical),
                )
                .body(Cow::Owned(bytes))
                .unwrap_or_else(|_| not_found())
        }
        Err(e) => {
            log::error!(target: LOG_TARGET, "[console] 404 {route:?}: read of {canonical:?} failed: {e}");
            not_found()
        }
    }
}

fn html_response(body: &'static str) -> tauri::http::Response<Cow<'static, [u8]>> {
    tauri::http::Response::builder()
        .header(
            tauri::http::header::CONTENT_TYPE,
            "text/html; charset=utf-8",
        )
        .body(Cow::Borrowed(body.as_bytes()))
        .expect("static index.html response is well-formed")
}

fn not_found() -> tauri::http::Response<Cow<'static, [u8]>> {
    tauri::http::Response::builder()
        .status(tauri::http::StatusCode::NOT_FOUND)
        .body(Cow::Borrowed(&b"not found"[..]))
        .expect("static 404 response is well-formed")
}

fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        _ => "application/octet-stream",
    }
}

/// Wholly-generic shell-integration wrapper - contains no console-ui
/// source, only `<script src>`/`<link>` references to files served from
/// [`assets_dir`] and the EngineBridge wiring `premium/console-ui/README.md`
/// documents for a shell team to write (its "Option B", matching this
/// crate's existing one-command-per-verb `commands.rs` convention -
/// see that file's `sidecar_hold`/`sidecar_blind_transfer`/etc.).
const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Centinelo Console</title>
<style>html,body{margin:0;height:100%;overflow:hidden}#console-host{height:100vh}</style>

<!-- Trapped-window prevention (see console.rs's open_or_focus doc,
     "The trapped-window problem this also guards against") - deliberately
     the very first thing in <head>, before the stylesheets and every
     vendored console-ui script below, same "armed before ANYTHING else
     can fail" reasoning as the main window's ui/js/error-capture.js. This
     window has .decorations(false) (no OS titlebar) and its own
     close/minimize controls are hand-drawn DOM the vendored bundle below
     wires up in mount() - if that bundle never runs, this script is the
     only thing standing between the operator and an uncloseable blank
     window. Inline, not an external <script src>, on purpose: unlike the
     main window, no CSP is ever applied to this one (console.rs's own doc
     traces through Tauri's source to confirm this precisely, so it isn't
     blocked the way an inline script would be over there) - and an
     external file would itself be one more premium-console-assets/ file
     that could go missing, defeating the entire point. -->
<script>
(function () {
  "use strict";
  var reported = false;

  function invoke(cmd, args) {
    try {
      return window.__TAURI__.core.invoke(cmd, args || {});
    } catch (e) {
      return Promise.reject(e);
    }
  }

  function closeThisWindow() {
    try { window.__TAURI__.window.getCurrentWindow().close(); } catch (e) { /* nothing else to do */ }
  }

  // Escape always closes this window, full stop - independent of whether
  // console-ui's own titlebar close button (wired only once mount() below
  // succeeds) ever exists.
  document.addEventListener("keydown", function (ev) {
    if (ev.key === "Escape" || ev.key === "Esc") closeThisWindow();
  });

  function reportFatal(kind, fields) {
    if (reported) return; // one report is enough while the window unwinds
    reported = true;
    invoke("console_frontend_fatal", {
      report: {
        kind: kind,
        level: "error",
        message: String((fields && fields.message) || ""),
        source: (fields && fields.source) || null,
        line: (fields && fields.line) || null,
        col: (fields && fields.col) || null,
        stack: (fields && fields.stack) || null,
      },
    }).catch(function () { /* the Rust side's own watchdog is the backstop */ });
  }

  // Same capture-phase pattern as ui/js/error-capture.js: catches both a
  // failed <script>/<link> load (non-bubbling, capture-phase-only event)
  // and any uncaught exception thrown by a vendored script below.
  window.addEventListener(
    "error",
    function (ev) {
      var target = ev.target;
      if (target && target !== window && target.tagName) {
        reportFatal("resource", {
          message: target.tagName + " failed to load: " + (target.src || target.href || ""),
          source: target.src || target.href || "",
        });
        return;
      }
      reportFatal("error", {
        message: ev.message,
        source: ev.filename,
        line: ev.lineno,
        col: ev.colno,
        stack: ev.error && ev.error.stack,
      });
    },
    true
  );
  window.addEventListener("unhandledrejection", function (ev) {
    var reason = ev.reason;
    reportFatal("unhandledrejection", {
      message: (reason && reason.message) || String(reason),
      stack: reason && reason.stack,
    });
  });
})();
</script>

<link rel="stylesheet" href="tokens.css">
<link rel="stylesheet" href="console.css">
</head>
<body>
<div id="console-host"></div>

<!-- console-ui package, classic scripts, dependency order per
     premium/console-ui/dev/mock.html - served from the runtime premium
     assets directory, never bundled in this repo. -->
<script src="components/dom-utils.js"></script>
<script src="components/icons.js"></script>
<script src="store/ConsoleStore.js"></script>
<script src="bridge/EngineBridge.js"></script>
<script src="components/extension-tile.js"></script>
<script src="components/extension-grid.js"></script>
<script src="components/active-call.js"></script>
<script src="components/incoming-queue.js"></script>
<script src="components/statusbar.js"></script>
<script src="components/drag-controller.js"></script>
<script src="console-app.js"></script>

<script>
(function () {
  "use strict";
  var invoke = window.__TAURI__.core.invoke;
  var listen = window.__TAURI__.event.listen;
  var win = window.__TAURI__.window.getCurrentWindow();

  // EngineBridge <-> sidecar wiring ("Option B" from
  // premium/console-ui/README.md's "EngineBridge contract" - one Tauri
  // command per verb, matching commands.rs's existing dial/answer/hangup
  // convention rather than adding a single generic passthrough).
  var DISPATCH = {
    dial: function (c) { return invoke("sidecar_dial", { uri: c.uri }); },
    answer: function () { return invoke("sidecar_answer"); },
    hangup: function (c) { return invoke("sidecar_hangup", { call_id: c.call_id || null }); },
    register: function () { return invoke("sidecar_restart"); },
    hold: function (c) { return invoke("sidecar_hold", { call_id: c.call_id || null }); },
    resume: function (c) { return invoke("sidecar_resume", { call_id: c.call_id || null }); },
    mute: function (c) { return invoke("sidecar_mute", { on: !!c.on, call_id: c.call_id || null }); },
    blind_transfer: function (c) { return invoke("sidecar_blind_transfer", { uri: c.uri, call_id: c.call_id || null }); },
    attended_transfer: function (c) { return invoke("sidecar_attended_transfer", { uri: c.uri, call_id: c.call_id || null }); },
    complete_transfer: function (c) { return invoke("sidecar_complete_transfer", { call_id: c.call_id || null }); },
    abort_transfer: function () { return invoke("sidecar_abort_transfer"); },
    blf_subscribe: function (c) { return invoke("sidecar_blf_subscribe", { ext: String(c.ext) }); },
    blf_unsubscribe: function (c) { return invoke("sidecar_blf_unsubscribe", { ext: String(c.ext) }); },
  };

  var bridge = Centinelo.EngineBridge.create();
  bridge.init(
    function (cmd) {
      var fn = DISPATCH[cmd.cmd];
      if (!fn) return Promise.reject(new Error("console: no dispatch for cmd '" + cmd.cmd + "'"));
      return fn(cmd);
    },
    function (handler) { listen("sidecar-event", function (e) { handler(e.payload); }); }
  );

  // Roster: sourced from the operator's configured favorites - the only
  // extension directory this shell has (see
  // premium/console-ui/README.md "Shell integration steps", item 3: "out
  // of this package's scope; F2's shell has no directory/CRM lookup yet
  // either"). Real, user-configured data, same source the main window's
  // own favorites grid already reads (commands::get_favorites) - not
  // fabricated. selfExt is deliberately left null: every configured
  // favorite (including one that happens to match the operator's own
  // account extension) gets a live, subscribed BLF tile in the grid; the
  // "Your call" panel's own state is driven by call_state events
  // regardless of selfExt (see console-ui/README.md fidelity note #2).
  function boot() {
    invoke("get_favorites").then(function (favorites) {
      var roster = (favorites || [])
        .filter(function (f) { return (f.ext || "").trim(); })
        .map(function (f) {
          var ext = f.ext.trim();
          return { ext: ext, name: (f.label || "").trim() || ("Ext " + ext), group: "Favorites" };
        });
      mount(roster);
    }, function (err) {
      console.error("console: get_favorites failed", err);
      mount([]);
    });
  }

  function mount(roster) {
    var host = document.getElementById("console-host");
    var app = Centinelo.ConsoleApp.mount(host, {
      roster: roster,
      selfExt: null,
      bridge: bridge,
    });

    // Window chrome: console-ui's own titlebar renders minimize/close
    // glyphs inert by design ("Wired by the embedding shell" tooltip -
    // console-app.js's own doc comment) - wire them to this real window
    // here. No data-tauri-drag-region attribute is possible on
    // console-ui's JS-constructed DOM, so dragging uses the explicit
    // window API instead.
    var titlebar = app.element.querySelector(".titlebar");
    if (titlebar) {
      titlebar.addEventListener("mousedown", function (e) {
        if (e.target.closest(".wbtn")) return;
        if (e.buttons === 1) win.startDragging();
      });
      var buttons = titlebar.querySelectorAll(".wbtn");
      buttons.forEach(function (b) {
        b.addEventListener("click", function () {
          if (b.classList.contains("close")) win.close();
          else win.minimize();
        });
      });
    }

    window.addEventListener("beforeunload", function () { app.destroy(); });

    // Disarms the Rust-side load-timeout watchdog and the fatal-error
    // window-close path (see console.rs's mark_ready doc) - the bundle
    // reached this point, so the window is no longer at risk of being a
    // blank, uncloseable trap. Fires on both the real-roster path and the
    // get_favorites-failed fallback below (mount([])) - an empty roster
    // still means the console itself rendered successfully.
    invoke("console_frontend_ready").catch(function () { /* best-effort */ });
  }

  boot();
})();
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    //! The inline panel's sizing/session state machine - the pure half of
    //! `open_panel`/`panel_closed`, exercised with no AppHandle so every
    //! interleaving that matters can be pinned deterministically. The
    //! stateful frontend half of the same contract (reopen during the
    //! close's IPC round-trip must work) is pinned in
    //! `ui/js/console-panel.test.js` ("P4").
    use super::{console_index_url, CloseOutcome, PanelSizeState, ASSET_SCHEME};

    fn fresh() -> PanelSizeState {
        PanelSizeState {
            session: 0,
            restore: None,
        }
    }

    #[test]
    fn console_index_url_macos_linux_shape() {
        assert_eq!(
            console_index_url(false),
            format!("{ASSET_SCHEME}://localhost/index.html")
        );
    }

    #[test]
    fn console_index_url_windows_android_shape() {
        // MUTATION TARGET: if the `if`/`else` branches of console_index_url
        // ever get swapped, this and the macOS/Linux test above both still
        // pass compilation but one of the two now asserts the wrong shape
        // for its branch - together they pin BOTH arms, not just that the
        // function returns *a* URL.
        assert_eq!(
            console_index_url(true),
            format!("http://{ASSET_SCHEME}.localhost/index.html")
        );
    }

    #[test]
    fn honored_ack_restores_its_own_eras_baseline() {
        let mut s = fresh();
        let first = s.begin_open(Some((380.0, 560.0)));
        assert_eq!(first, 1, "session ids start at 1 (0 = never opened)");
        assert_eq!(
            s.close(Some(first)),
            CloseOutcome::Restore(Some((380.0, 560.0)))
        );
        // Baseline consumed: the next grow era records a fresh one. The
        // new id is 3, not 2 - the honored close advanced the session to
        // make acks one-shot, and begin_open steps past it.
        let second = s.begin_open(Some((900.0, 640.0)));
        assert_eq!(second, first + 2);
        assert_eq!(
            s.close(Some(second)),
            CloseOutcome::Restore(Some((900.0, 640.0)))
        );
    }

    #[test]
    fn stale_ack_superseded_by_a_newer_open_never_restores() {
        // THE interleaving this PR closes: close(session 1) is still
        // crossing IPC when the operator's reopen click runs open_panel
        // again (session 2, window re-grown from the same baseline). The
        // late ack for session 1 must be identified as stale and ignored -
        // honoring it would shrink the window right back under the
        // freshly reopened panel AND consume the baseline the live
        // session's own close still needs.
        let mut s = fresh();
        let first = s.begin_open(Some((380.0, 560.0)));
        let second = s.begin_open(None); // reopen before any close ack landed
        assert_eq!(
            s.restore, Some((380.0, 560.0)),
            "a reopen before any close must keep the FIRST baseline, not overwrite it with the grown size"
        );
        assert_eq!(
            s.close(Some(first)),
            CloseOutcome::Stale {
                ack: first,
                current: second
            }
        );
        assert_eq!(
            s.restore,
            Some((380.0, 560.0)),
            "a stale ack must not consume the baseline"
        );
        assert_eq!(
            s.close(Some(second)),
            CloseOutcome::Restore(Some((380.0, 560.0))),
            "the live session's close still restores the one true baseline"
        );
    }

    #[test]
    fn duplicate_ack_is_stale_not_a_second_restore() {
        let mut s = fresh();
        let id = s.begin_open(Some((380.0, 560.0)));
        assert!(matches!(s.close(Some(id)), CloseOutcome::Restore(_)));
        assert_eq!(
            s.close(Some(id)),
            CloseOutcome::Stale {
                ack: id,
                current: id + 1
            },
            "an honored close advances the session, so a replayed ack cannot restore twice"
        );
    }

    #[test]
    fn anonymous_and_future_acks_do_nothing() {
        let mut s = fresh();
        assert!(matches!(s.close(None), CloseOutcome::Anonymous));
        let id = s.begin_open(Some((380.0, 560.0)));
        // An ack naming a session that was never emitted (defensive
        // frontend, corrupted state) must not restore either.
        assert!(matches!(
            s.close(Some(id + 5)),
            CloseOutcome::Stale { .. }
        ));
        assert_eq!(s.restore, Some((380.0, 560.0)), "untouched");
    }

    #[test]
    fn session_ids_are_strictly_monotonic_across_eras() {
        let mut s = fresh();
        let mut prev = 0;
        for _ in 0..3 {
            let id = s.begin_open(None);
            assert!(id > prev, "each open stamps a fresh, larger session id");
            assert!(matches!(s.close(Some(id)), CloseOutcome::Restore(_)));
            prev = id;
        }
    }
}

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

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

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

/// Finds the already-open console window and focuses it, or builds a
/// fresh one. Re-checks the license gate itself (not just trusting that a
/// caller already checked) - the tray menu item and the main window's
/// button are both gated on [`is_unlocked`] before they're ever shown
/// (see `tray.rs`, `ui/js/app.js`), but `commands::open_console` is a
/// plain IPC command a webview could still invoke directly (e.g. via
/// devtools) even with the button hidden - the *window itself* is the
/// thing that must never appear unlicensed, not merely the button that
/// usually opens it.
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

    let url = WebviewUrl::CustomProtocol(
        format!("{ASSET_SCHEME}://localhost/index.html")
            .parse()
            .expect("static console URL is always a valid Url"),
    );
    WebviewWindowBuilder::new(app, WINDOW_LABEL, url)
        .title("Centinelo Console")
        .inner_size(1100.0, 720.0)
        .min_inner_size(900.0, 600.0)
        .resizable(true)
        .decorations(false)
        .build()
        .map_err(|e| e.to_string())?;
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
pub fn asset_protocol_handler(
    _ctx: tauri::UriSchemeContext<'_, tauri::Wry>,
    request: tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Cow<'static, [u8]>> {
    let raw_path = request.uri().path();
    let route = raw_path.trim_start_matches('/');
    let route = if route.is_empty() { "index.html" } else { route };

    if route == "index.html" {
        return html_response(INDEX_HTML);
    }

    let Some(dir) = assets_dir() else {
        return not_found();
    };
    let Ok(canonical_dir) = dir.canonicalize() else {
        return not_found();
    };
    let candidate = dir.join(route);
    let Ok(canonical) = candidate.canonicalize() else {
        return not_found();
    };
    if !canonical.starts_with(&canonical_dir) {
        log::warn!("premium console: asset request escaped assets dir: {route:?}");
        return not_found();
    }

    match std::fs::read(&canonical) {
        Ok(bytes) => tauri::http::Response::builder()
            .header(tauri::http::header::CONTENT_TYPE, content_type_for(&canonical))
            .body(Cow::Owned(bytes))
            .unwrap_or_else(|_| not_found()),
        Err(_) => not_found(),
    }
}

fn html_response(body: &'static str) -> tauri::http::Response<Cow<'static, [u8]>> {
    tauri::http::Response::builder()
        .header(tauri::http::header::CONTENT_TYPE, "text/html; charset=utf-8")
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
<link rel="stylesheet" href="tokens.css">
<link rel="stylesheet" href="console.css">
<style>html,body{margin:0;height:100%;overflow:hidden}#console-host{height:100vh}</style>
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
  }

  boot();
})();
</script>
</body>
</html>
"##;

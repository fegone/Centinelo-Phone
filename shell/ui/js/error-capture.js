// Frontend crash/error capture — armed as early as physically possible.
//
// Loaded from ui/index.html's <head> as a plain classic (non-module)
// <script src>, BEFORE the stylesheet <link> tags and well before
// js/app.js's own <script type="module">. That ordering is the entire
// point: this file's listeners must be live before ANYTHING else on the
// page can fail, so a 404/CSP-block/syntax-error in app.js itself, or a
// stylesheet that fails to load, still leaves a trace — see
// shell/README.md's "Frontend error logging" section for the day-long
// Windows bug that made this necessary (the window painted and never
// responded to anything, and nothing anywhere recorded why).
//
// Deliberately NOT an ES module and NOT importing ui/js/error-reporting.js
// (the testable, shared shaping logic used by js/app.js once it's
// running): a dynamic import() here would resolve asynchronously, leaving
// a real gap between "parser reaches this script" and "the module graph is
// ready" during which the exact failures this exists to catch (a stylesheet
// or the app.js module itself failing to load) could slip through
// uncaught. This file instead duplicates the minimum needed — build a
// plain object, call invoke() directly — so it has zero dependency on any
// other script on the page having loaded successfully. Its Rust
// counterpart, frontend_log.rs's format_log_line, does the real
// redaction/truncation regardless of which of the two send paths a report
// took, so nothing here needs to reimplement that.
(function () {
  "use strict";

  function send(report) {
    try {
      var tauri = window.__TAURI__;
      if (!tauri || !tauri.core || typeof tauri.core.invoke !== "function") return;
      tauri.core.invoke("log_frontend_error", { report: report }).catch(function () {});
    } catch (e) {
      // A broken reporter must never become a second source of failure.
    }
  }

  // Flipped by js/app.js (window.__centineloMarkBooted()) once
  // wireStaticHandlers() has run in boot() — the point at which title-bar
  // buttons, Settings, and the rest of the interface actually respond to
  // input. Before that point a JS exception reproduces exactly today's bug
  // ("paints but does nothing"), so it earns the on-screen fallback banner
  // below; after it, the app is at least interactive and the existing
  // per-feature error handling (banners, console.error, ...) is doing its
  // job instead.
  var booted = false;
  var fatalBannerShown = false;
  window.__centineloMarkBooted = function () {
    booted = true;
  };

  function showFatalBanner() {
    if (fatalBannerShown) return;
    fatalBannerShown = true;
    function render() {
      var el = document.createElement("div");
      el.setAttribute("role", "alert");
      el.textContent = "Centinelo Phone failed to start. Check the log for details.";
      var slot = document.getElementById("banner-slot");
      if (slot) {
        // Matches app.js's own showBanner() markup (see app.css's
        // .banner/.err rules) so it looks native if app.css did load.
        el.className = "banner err";
        slot.innerHTML = "";
        slot.appendChild(el);
        return;
      }
      if (document.body) {
        // app.css itself may not have loaded (this banner also covers a
        // failed stylesheet) — inline styles so the message is legible
        // regardless.
        el.style.cssText =
          "position:fixed;top:0;left:0;right:0;padding:10px 16px;" +
          "background:#7a1f1f;color:#fff;font:13px sans-serif;z-index:99999;";
        document.body.appendChild(el);
      }
    }
    if (document.body) render();
    else document.addEventListener("DOMContentLoaded", render, { once: true });
  }

  function reportFatal(kind, fields) {
    send({
      kind: kind,
      level: "error",
      message: String((fields && fields.message) || ""),
      source: (fields && fields.source) || null,
      line: (fields && fields.line) || null,
      col: (fields && fields.col) || null,
      stack: (fields && fields.stack) || null,
    });
    if (!booted) showFatalBanner();
  }

  // Capture phase (3rd arg `true`): a failed <script>/<link>/<img> load
  // fires a non-bubbling "error" event at the element itself, which only a
  // capturing listener up the tree — not a bubble-phase one — ever sees. A
  // plain uncaught JS exception ALSO reaches this same listener (dispatched
  // with target === window), so one listener covers both of this file's
  // two "resource" and "error" cases.
  window.addEventListener(
    "error",
    function (ev) {
      var target = ev.target;
      if (target && target !== window && target.tagName) {
        reportFatal("resource", {
          message: target.tagName + " failed to load",
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

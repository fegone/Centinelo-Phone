// Frontend crash/error reporting — the JS side that feeds
// `frontend_log::log_frontend_error` (shell/src-tauri/src/frontend_log.rs)
// so an uncaught exception, an unhandled promise rejection, or a failed
// script/stylesheet load leaves a trace in the same persistent LogDir file
// every other diagnostic in this app already writes to.
//
// Born from a full day lost chasing a Windows-only bug: the window painted
// but never responded to anything, and there was nothing anywhere — no
// window.onerror, no log line, no banner — to diagnose it *from*. See
// shell/README.md's "Frontend error logging" section for the short version.
//
// This file is the *testable* half. The actual earliest-possible capture
// (window.onerror-equivalent, unhandledrejection, resource-load-failure
// listeners, and the on-screen fallback banner) lives in ui/index.html's
// inline <script>, deliberately: it has to be armed BEFORE this module (or
// js/app.js, which imports it) even starts loading, so a 404/CSP-block/
// syntax-error in app.js itself — the exact failure class this whole thing
// exists for — still gets caught. That inline script duplicates the
// minimum needed (a raw invoke() call) rather than depending on this module
// having loaded successfully. js/app.js imports the helpers below for its
// own boot-milestone breadcrumbs and any error it catches itself, once it's
// running.

// Caps here only guard the IPC payload itself against something
// pathological (a stack trace with megabytes of repeated frames) — the
// real truncation/redaction of call content happens on the Rust side
// (frontend_log.rs's format_log_line), which never trusts this side's
// caps as the last word. Generous on purpose: no reason to lose useful
// diagnostic detail in transit just because Rust re-trims it anyway.
const TRANSPORT_MESSAGE_CAP = 2000;
const TRANSPORT_STACK_CAP = 4000;
const TRANSPORT_SOURCE_CAP = 500;

/// Caps a string to `maxLength` characters. Never throws — a non-string
/// input (a thrown non-Error value, `undefined`, ...) becomes "" rather
/// than crashing the very code that exists to report crashes.
export function capText(raw, maxLength) {
  if (typeof raw !== "string") return "";
  return raw.length > maxLength ? raw.slice(0, maxLength) : raw;
}

/// "milestone" reports are informational breadcrumbs (app.js boot
/// progress); everything else defaults to "error" — a resource-load
/// failure or an unhandled rejection is exactly the kind of thing today's
/// bug needed visible in the log, not filtered by a log-level default.
export function levelForKind(kind) {
  return kind === "milestone" ? "info" : "error";
}

/// Pure shaping of the object sent to `invoke("log_frontend_error", {
/// report })` — no DOM/Tauri access, so this is unit-testable under plain
/// `node:test` (see error-reporting.test.js). `fields` mirrors the raw
/// pieces callers have on hand: `{ message, source, line, col, stack,
/// level }`.
export function buildErrorReport(kind, fields = {}) {
  const safeKind = typeof kind === "string" && kind ? kind : "error";
  const level = fields.level || levelForKind(safeKind);
  return {
    kind: safeKind,
    level,
    message: capText(String(fields.message ?? ""), TRANSPORT_MESSAGE_CAP),
    source: fields.source ? capText(String(fields.source), TRANSPORT_SOURCE_CAP) : null,
    line: Number.isFinite(fields.line) ? fields.line : null,
    col: Number.isFinite(fields.col) ? fields.col : null,
    stack: fields.stack ? capText(String(fields.stack), TRANSPORT_STACK_CAP) : null,
  };
}

/// Best-effort send to the Rust log — swallows every failure by design
/// (module doc above: a broken reporter must never become a *second*
/// source of failure). Not unit-tested (touches window.__TAURI__), same
/// convention as the rest of this codebase's DOM/IPC-touching functions
/// (see e.g. reg-status.js vs app.js's own rendering glue).
export function reportFrontendIssue(kind, fields) {
  try {
    const tauri = window.__TAURI__;
    if (!tauri || !tauri.core || typeof tauri.core.invoke !== "function") return;
    const report = buildErrorReport(kind, fields);
    tauri.core.invoke("log_frontend_error", { report }).catch(() => {});
  } catch (e) {
    // Never let the reporter itself throw.
  }
}

/// Boot-milestone breadcrumb — app.js calls this at a handful of
/// significant points (module evaluated, handlers wired, first engine
/// round-trip, UI ready), not per-function. See app.js's `boot()` for the
/// actual call sites and why those four.
export function logMilestone(name) {
  reportFrontendIssue("milestone", { message: name });
}

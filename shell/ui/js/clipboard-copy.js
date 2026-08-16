// Pure decision logic for "Copy token" (app.js, Settings > click-to-call
// bridge) - zero DOM/clipboard dependency, same convention
// call-availability.js/call-lifecycle.js already document for themselves.
//
// Why this exists (UI-silent-failures audit, 2026-08-16): the click
// handler used to always paint "Copied." after trying
// `navigator.clipboard.writeText` and, if that rejected, falling back to
// `document.execCommand("copy")` - but neither outcome was ever inspected.
// `execCommand` returns a boolean (false on failure) and can itself throw
// in a webview that doesn't support it at all; a rejected async write
// followed by a failed/thrown fallback still painted the same green
// "Copied." text a real success would. This token gates the click-to-call
// bridge's own auth (see shell-tauri skill/README) - a user who believes
// they copied it and pastes something stale/empty into the extension has
// no way to tell from the UI that nothing was actually on their clipboard.
//
// The actual clipboard/DOM calls stay in app.js (this module never touches
// `navigator`/`document`) - callers report what happened as three plain
// booleans and this function turns that into the single outcome the UI
// needs to paint.

/// @param asyncOk - true if `navigator.clipboard.writeText` resolved.
/// @param fallbackAttempted - true if the caller even tried the
///   `execCommand("copy")` fallback (only happens when asyncOk is false).
/// @param fallbackOk - the fallback's own success signal (execCommand's
///   boolean return, false if it threw instead of returning) - meaningless
///   when fallbackAttempted is false, but a caller can always pass false.
/// @returns { copied, usedFallback } - `copied` is what decides
///   success/error banner text; `usedFallback` is purely informational
///   (useful for the failure log line, not required for the banner
///   itself).
export function resolveClipboardCopyOutcome({ asyncOk, fallbackAttempted, fallbackOk }) {
  if (asyncOk) return { copied: true, usedFallback: false };
  if (fallbackAttempted && fallbackOk) return { copied: true, usedFallback: true };
  return { copied: false, usedFallback: fallbackAttempted };
}

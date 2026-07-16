// Shared DOM-string escaping helpers.
//
// Extracted from app.js/transcript-panel.js (2026-07-16 4R re-review,
// READABILITY R2): both files had grown byte-identical copies of these two
// functions as i18n work added more attribute interpolation (translated
// placeholders/aria-labels/titles) across both - one file editing its copy
// without the other was a real drift risk. Pure string functions, no DOM
// dependency (not the `document.createElement("div"); d.textContent = s;
// return d.innerHTML` trick app.js used to use) so this works identically
// in the real webview AND under plain `node:test` (see
// transcript-panel.test.js, which already relied on exactly this shape
// before the extraction - dom-utils.test.js covers it directly now too).

/// Escapes `&`, `<`, `>` - safe for TEXT NODE content only (interpolating
/// into `innerHTML`/template-literal markup as a child's text). The
/// WHATWG HTML serialization algorithm escapes exactly these three
/// characters in text content; `"`/`'` don't need escaping there.
export function escapeHtml(s) {
  return String(s ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

/// `escapeHtml` is NOT safe for interpolating into an HTML ATTRIBUTE value
/// (`placeholder="${...}"`, `aria-label="${...}"`, `data-call-id="${...}"`,
/// ...) - a value containing a literal `"` would break out of the
/// attribute and inject arbitrary markup/event handlers on the next
/// re-render (this is exactly what 2026-07-16's 4R re-review M1 finding
/// was, in transcript-panel.js's find input, before this module existed).
/// Use this instead for any string placed inside an attribute; prefer
/// setting the DOM property directly (`el.value = x`) over interpolating
/// into markup at all wherever that's an option.
export function escapeAttr(s) {
  return escapeHtml(s).replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}

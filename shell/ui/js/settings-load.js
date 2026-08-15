// Pure helpers for openSettings()'s multi-command load (app.js). Split out
// so the "one failed IPC call must not blank the rest of Settings" fix is
// unit-testable without a Tauri runtime (same convention as
// codec-settings.js/device-settings.js/updater.js/reg-status.js - the DOM-
// touching half stays in app.js, the decision logic lives here).
//
// Born from a confirmed field defect: openSettings() used to load its 8
// backend commands with `Promise.all`, which rejects the *entire* batch the
// instant any ONE of them fails - a user could see a fully blank Settings
// screen even though 7 of 8 commands actually succeeded. The fix is
// `Promise.allSettled` plus per-group handling (see app.js's openSettings),
// with this module owning the "which group is which command, and what do we
// do with a mixed settled/rejected result array" logic.

/// The Tauri commands openSettings loads in one batch, in call order, each
/// tagged with the i18n key for its user-facing label. Most labelKeys reuse
/// a heading that already exists elsewhere on the Settings screen (e.g.
/// "settings.favoritesHeading") rather than inventing a parallel set of
/// group names - `settings.fieldGroup.account` is the one exception,
/// because the account fields (display name/host/extension) don't sit under
/// a single heading of their own.
export const SETTINGS_FIELD_GROUPS = [
  { key: "account", cmd: "get_account_settings", labelKey: "settings.fieldGroup.account" },
  { key: "theme", cmd: "get_theme", labelKey: "settings.themeAria" },
  { key: "corePath", cmd: "get_core_binary_path", labelKey: "settings.corePathLabel" },
  { key: "adminStatus", cmd: "admin_status", labelKey: "settings.adminHeading" },
  { key: "favorites", cmd: "get_favorites", labelKey: "settings.favoritesHeading" },
  { key: "bridge", cmd: "get_bridge_settings", labelKey: "settings.bridgeHeading" },
  { key: "license", cmd: "get_license_settings", labelKey: "settings.licenseHeading" },
  { key: "availability", cmd: "get_availability_settings", labelKey: "availability.settingsHeading" },
];

/// Best-effort stringification of a thrown/rejected value - Tauri command
/// errors, plain strings, and non-Error objects can all reach here. Same
/// shape the rest of this codebase's catch blocks already use (e.g.
/// app.js's other `String(e && e.message ? e.message : e)` call sites).
export function describeError(e) {
  return String(e && e.message ? e.message : e);
}

/// Pairs `Promise.allSettled`'s per-command results (must be in
/// SETTINGS_FIELD_GROUPS order - openSettings maps the same array to build
/// the promises) back up with which group each belongs to.
///
/// `values[key]` is `undefined` for any group that failed - callers MUST
/// treat "undefined" as "leave this field/section alone and mark it
/// failed", never as "the backend returned nothing" (that's a real,
/// meaningful value for some fields, e.g. get_core_binary_path resolving to
/// "" for "not set").
///
/// `failed` carries only what logging/display needs (group key, command
/// name, label key, stringified error) - never the raw rejection reason, so
/// callers can't accidentally forward something that still needs the
/// redaction frontend_log.rs's format_log_line applies on the Rust side.
export function summarizeSettledResults(results) {
  const values = {};
  const failed = [];
  SETTINGS_FIELD_GROUPS.forEach((group, i) => {
    const r = results[i];
    if (r && r.status === "fulfilled") {
      values[group.key] = r.value;
    } else {
      values[group.key] = undefined;
      failed.push({
        key: group.key,
        cmd: group.cmd,
        labelKey: group.labelKey,
        error: describeError(r ? r.reason : undefined),
      });
    }
  });
  return { values, failed };
}

/// Paint-phase counterpart to summarizeSettledResults. The load-phase fix
/// above (Promise.allSettled) only isolates failures in the 8 backend
/// invoke() calls - openSettings() then spends ~15 more statements PAINTING
/// those results into the DOM (setTransportUI, renderCodecsSection,
/// applyLockUI, ...), and until this existed that paint phase was one flat
/// synchronous sequence ending in the screen's reveal line. A throw
/// anywhere in it (a missing element, a malformed value from a group that
/// "fulfilled" with junk, ...) aborted the whole function before the
/// reveal ever ran - reported from the field 2026-08-15 as "clicking
/// Settings does nothing at all, no banner, no screen".
///
/// `steps` is an array of `{ name, run }`; `run` is a zero-arg closure
/// (the DOM/state access it needs comes from its caller's closure, same as
/// every other paint call in openSettings). Every step's `run` is invoked
/// regardless of an earlier step throwing - each failure is caught,
/// tagged with its step's `name` and a stringified error (never the raw
/// thrown value, same "no call content forwarded" contract
/// summarizeSettledResults follows), and collected in the returned
/// `failed` array instead of escaping. The caller can then still reach its
/// own "reveal the screen" line unconditionally.
export function runSettingsPaintSteps(steps) {
  const failed = [];
  for (const step of steps) {
    try {
      step.run();
    } catch (e) {
      failed.push({ name: step.name, error: describeError(e) });
    }
  }
  return failed;
}

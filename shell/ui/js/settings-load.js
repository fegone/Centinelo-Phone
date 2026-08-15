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

/// Paint-phase counterpart to summarizeSettledResults, AND the owner of the
/// one guarantee this whole module exists for: Settings must always end up
/// visible, no matter how many paint steps failed.
///
/// History: the load-phase fix above (Promise.allSettled) only isolates
/// failures in the 8 backend invoke() calls - openSettings() then spends
/// ~15 more statements PAINTING those results into the DOM
/// (setTransportUI, renderCodecsSection, applyLockUI, ...), and until this
/// existed that paint phase was one flat synchronous sequence ending in
/// the screen's reveal line. A throw anywhere in it (a missing element, a
/// malformed value from a group that "fulfilled" with junk, ...) aborted
/// the whole function before the reveal ever ran - reported from the field
/// 2026-08-15 as "clicking Settings does nothing at all, no banner, no
/// screen". A first pass fixed that by isolating each paint step (this
/// function, then named `runSettingsPaintSteps`) but left the actual
/// reveal call sitting in app.js's own `try/finally` - which is real, but
/// which this project has NO way to unit-test (app.js is DOM/webview-only
/// by convention; see this module's header comment on why the pure logic
/// always lives here instead). A `try/finally` verified only by reading it
/// is exactly the kind of "critical invariant held together by inspection"
/// this project has spent months paying for (2026-08-13/14 postmortem).
/// This second pass moves the reveal call itself in here, so the
/// invariant - not just the per-step isolation - has a real,
/// mutation-verified test (see settings-load.test.js's "always reveals"
/// suite).
///
/// `buildSteps` is a ZERO-ARG closure that RETURNS an array of
/// `{ name, run }` (app.js passes `() => buildSettingsPaintSteps(values)`,
/// keeping the ~15-entry step list itself out of openSettings for
/// readability - see that function's own header comment). It is a builder,
/// not a plain array, and it is called from INSIDE the try below rather
/// than by the caller before this function is even entered - ronda 4,
/// coordinator caution: extracting the step list to its own function meant
/// evaluating it as a call argument (`runSettingsPaintSteps(buildSteps(),
/// reveal)`) would run it BEFORE this function's try/finally exists at
/// all, so a throw while merely constructing the array (a typo'd variable
/// reference, say) would skip `reveal()` entirely - the exact class of bug
/// this whole file exists to close, reintroduced by the refactor meant to
/// make it more readable. Taking a thunk and calling it inside the guarded
/// region closes that: see settings-load.test.js's "buildSteps-throws"
/// test. A thrown `buildSteps()` is caught, recorded as a single `failed`
/// entry named `"buildSteps"`, and treated as "zero steps ran" (nothing
/// painted, but `reveal` still fires and the function still returns
/// normally, same as any other paint failure) - it is NOT treated like the
/// `reveal`-throws case below, because it's still fundamentally "a step
/// failed to run", just the earliest possible one.
///
/// Each of the returned steps' `run` is a zero-arg closure (the DOM/state
/// access it needs comes from its caller's closure, same as every other
/// paint call in openSettings). Every step's `run` is invoked regardless
/// of an earlier step throwing - each failure is caught, tagged with its
/// step's `name` and a stringified error (never the raw thrown value, same
/// "no call content forwarded" contract summarizeSettledResults follows),
/// and collected into `failed`.
///
/// `reveal` is a zero-arg closure (app.js passes
/// `() => { $("screen-settings").hidden = false; }`) called EXACTLY ONCE,
/// unconditionally, after every step has had its turn - whether zero,
/// some, or ALL of them failed. Reveal is "make the screen visible", not
/// "make the screen visible if painting went well": a broken paint step
/// must degrade its own section, never the operator's ability to see the
/// screen at all.
///
/// The ugly case: if `reveal` itself throws, that is a DIFFERENT and
/// rarer failure than a paint step failing - the mechanism that shows the
/// screen is broken, not a section that failed to paint. This function
/// deliberately does NOT catch that, does NOT retry `reveal`, and does NOT
/// swallow it into a silent success: the exception propagates to the
/// caller as-is, and the `failed` array already collected for the paint
/// steps is lost along with it (there is nothing left to hand it to once
/// the reveal step itself is what's broken). This is a conscious choice,
/// not an oversight - the entire point of this task was refusing to trade
/// "some failures are invisible" for a different invisible failure at the
/// reveal step; a `reveal`-throws bug needs to be loud, not caught here
/// and quietly forgotten. The caller (openSettings's own defensive
/// `.catch` in wireStaticHandlers) is what's left to log it.
///
/// `reveal()` runs in a `finally`, wrapping the ENTIRE loop above, not
/// just placed after it (ronda 3, coordinator finding, reproduced live: a
/// thrown value whose `.message` getter itself throws makes
/// `describeError(e)` - called FROM INSIDE the per-step `catch` block -
/// throw a second time; that second throw happens outside the per-step
/// try/catch's protection, so it used to escape the loop entirely and
/// skip a bare `reveal()` call sitting after it, leaving the screen
/// hidden. `finally` is what turns "reveal always runs" from "true as
/// long as nothing else in this function's error handling ever fails
/// either" into a structural guarantee that doesn't depend on that -
/// exactly the class of assumption that left the field with a two-day
/// blank screen. See settings-load.test.js's "getter-throws" test, which
/// reproduces the coordinator's exact repro.
///
/// The double-throw case this creates: if the loop is ALREADY propagating
/// an exception (the getter-throws scenario above, or any other bug this
/// function's own error handling didn't anticipate) and `reveal()` ALSO
/// throws, JavaScript's `finally` semantics mean reveal's exception
/// REPLACES the pending one - the original is lost, not chained. Rather
/// than leave that to whatever the language happens to do, it's the
/// deliberate choice here too: reveal's exception is the one that must
/// surface, because "the mechanism that shows the screen is now ALSO
/// broken" is a strictly more urgent problem than whatever caused the
/// first throw, and the "ugly case" reasoning two paragraphs up already
/// commits to reveal failures being loud above everything else. `reveal`
/// is still only ever attempted once in this case, same as every other
/// path through this function. See settings-load.test.js's
/// "loop-and-reveal-both-throw" test.
export function runSettingsPaintSteps(buildSteps, reveal) {
  const failed = [];
  try {
    let steps;
    try {
      steps = buildSteps();
    } catch (e) {
      failed.push({ name: "buildSteps", error: describeError(e) });
      steps = [];
    }
    for (const step of steps) {
      try {
        step.run();
      } catch (e) {
        failed.push({ name: step.name, error: describeError(e) });
      }
    }
  } finally {
    reveal();
  }
  return failed;
}

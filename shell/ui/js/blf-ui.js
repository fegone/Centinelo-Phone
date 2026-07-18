// BLF master switch — pure UI visibility computation (P5, "BLF favorites
// admin toggle"). The engine-level gate lives in the Rust sidecar
// (sidecar.rs `favorites_to_auto_subscribe`): with `blfEnabled === false` the
// shell issues zero `blf_subscribe` commands to the core, so there's no SIP
// SUBSCRIBE (RFC 4235) to surface. This module only mirrors that decision in
// the DOM — it hides the favorites grid + its heading AND the premium
// receptionist console entry point (absent from view, not greyed: matches
// #btn-console's own "absent by default, not merely disabled" convention in
// index.html).
//
// Pure (no DOM access) so the visibility rule is unit-testable without a
// jsdom harness — the same convention updater.js / i18n.js / dom-utils.js
// already use here. app.js's renderBlfUi() / applyPremiumUI() are thin DOM
// appliers over computeBlfUiHidden().

/// DOM ids of the BLF-gated surfaces. Kept here (not inlined in app.js) so the
/// test can pin the exact contract the applier relies on without reaching into
/// app.js's module globals.
export const BLF_UI_TARGETS = Object.freeze({
  favoritesHeading: "favorites-main-heading",
  favoritesGrid: "favorites-grid",
  console: "btn-console",
});

/// Returns the `hidden` boolean each BLF-gated surface should take, given the
/// master switch plus the premium console's own license gate.
///
/// - `blfEnabled === false` (admin opt-out): the favorites grid + its heading
///   are hidden, and the console is hidden REGARDLESS of `consoleUnlocked`
///   (BLF off = no console surface at all — "gone, not hidden" mirroring the
///   engine decision).
/// - `blfEnabled === true` (default): the favorites grid + heading are shown,
///   and the console stays at its own license gate (`consoleUnlocked`).
///
/// Defaults (`blfEnabled = true`, `consoleUnlocked = false`) preserve the
/// shipped behavior during the brief window between cold-boot paint and the
/// first `get_blf_enabled` / `premium_capability_status` resolves.
export function computeBlfUiHidden({ blfEnabled = true, consoleUnlocked = false } = {}) {
  const blfOn = blfEnabled === true;
  return {
    favoritesHeading: !blfOn,
    favoritesGrid: !blfOn,
    console: !(blfOn && consoleUnlocked === true),
  };
}

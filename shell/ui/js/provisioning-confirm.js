// Deep-link informed-consent warning — pure visibility computation (RISK 4R
// finding, 2026-08-16, round 2 — commands.rs `provisioning_apply`'s doc has
// the full reasoning, including the round-1 typed-confirmation gate this
// replaced after round-2 review found it validated nothing an attacker
// didn't already control).
//
// `ProvisioningPreviewView.from_deep_link` (Rust, provisioning.rs) is `true`
// only when the pending config arrived via `handle_deep_link` (the OS
// `centinelo://provision` scheme handler) rather than a manual paste
// (`commands::provisioning_resolve`). There is no server-side ENFORCEMENT
// keyed on this flag — the attacker who crafted the deep link's `config=`
// payload also controls every field a check could validate against (host,
// ext, ...), so a "confirm what you see" gate would only be checking the
// attacker's own claim against itself. What this flag drives instead is
// purely a UI warning: highlight the host (the fact that actually
// determines where the phone ends up, and the one hardest for a phishing
// narrative to explain away) and say plainly that the link came from
// outside the app. Friction against a blind click-through, not
// authentication.
//
// Pure (no DOM access) so the rule is unit-testable without a jsdom harness
// — same convention blf-ui.js / updater.js / i18n.js already use here.
// app.js's showProvisioningConfirm() is the thin DOM applier over this.

/// Returns the two DOM-facing booleans `showProvisioningConfirm` needs,
/// given the preview object `provisioning_resolve`/`provisioning_pending_preview`/
/// the `provisioning://preview` event payload all share the same shape for.
/// A missing/falsy `preview` (defensive — `showProvisioningConfirm` already
/// guards on it before this is ever called) resolves to the safe default:
/// no warning shown, no highlight applied.
export function computeDeepLinkWarning(preview) {
  const fromDeepLink = !!(preview && preview.from_deep_link);
  return {
    // classList.toggle's second argument for #prov-confirm-host's
    // "deep-link-warn" class (app.css — `--st-busy`, never `--amber`,
    // which stays reserved for ringing).
    hostWarnClass: fromDeepLink,
    // #prov-confirm-deep-link-warning's `.hidden` property.
    warningHidden: !fromDeepLink,
  };
}

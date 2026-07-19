// Availability + auto-answer — pure decision logic, mirrored exactly by
// src-tauri/src/sidecar.rs's `effective_answer_mode`/
// `should_auto_reject_incoming` (the Rust side is what actually talks to
// the engine and owns the real behavior; this module exists so app.js's
// own rendering - the titlebar indicator, the tray label mirrored via
// get_availability_settings, the Settings pane toggles - can derive the
// SAME decision without re-deriving the rule ad hoc, and so the rule
// itself has fast unit tests independent of a live Tauri runtime). Same
// "zero Tauri dependency" shape reg-status.js/updater.js document for
// themselves: nothing here touches `window.__TAURI__`, `document`, or
// i18n.
//
// The rule (see settings.rs `AvailabilitySettings`'s own doc for the
// persisted-field half of this):
//   - available=false ("do not disturb") ALWAYS wins: every incoming call
//     is auto-rejected (486 Busy Here -> mailbox, core/PROTOCOL.md
//     `hangup` on an unanswered incoming call_id) and the engine's answer
//     mode is left/forced to "manual" - auto_answer is entirely ignored
//     in this state (nothing ever reaches ringing for it to answer).
//   - available=true + auto_answer=true -> the engine answers every
//     incoming call itself ("auto").
//   - available=true + auto_answer=false -> normal manual behavior, the
//     shipped default.

/// @param available - settings.availability.available (not admin-gated).
/// @param autoAnswer - settings.availability.auto_answer.
/// @returns { answerMode: "auto"|"manual", autoRejectIncoming: bool } -
///   answerMode is exactly the `mode` value to send with
///   `{"cmd":"set_answer_mode","mode":...}`; autoRejectIncoming is
///   whether an incoming `call_state` should be answered by immediately
///   sending `hangup` for its call_id instead of letting it ring.
export function computeCallHandling({ available, autoAnswer }) {
  if (!available) {
    return { answerMode: "manual", autoRejectIncoming: true };
  }
  return { answerMode: autoAnswer ? "auto" : "manual", autoRejectIncoming: false };
}

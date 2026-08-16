// Pure decision logic for handleCallState's "closed" case (app.js) - zero
// Tauri/DOM dependency, same convention call-availability.js/reg-status.js
// already document for themselves.
//
// Why this exists (UI-silent-failures audit, 2026-08-16): app.js's
// "closed" handler used to call finalizeClosedCall() unconditionally,
// which tears down whatever is CURRENTLY sitting in `state.call` - without
// ever comparing the closed event's own `call_id` against
// `state.call.callId`. `state.call` is a single-slot mirror (see app.js's
// own `confirmAndDial` comment: "a busy line never silently gets a second
// dial attempt"), but the ENGINE this UI talks to is not single-call by
// design:
//
//   - `core/PROTOCOL.md`'s `attended_transfer` explicitly holds the
//     "source" call and dials a second "consultation" call on the SAME UA
//     - two live call_ids at once, by construction (see that command's own
//     row, and `resume`'s "matters the moment there's a second call, i.e.
//     attended transfer").
//   - Independent of any transfer feature this shell exposes today, plain
//     SIP call waiting reaches the exact same shape with zero shell
//     involvement: a second inbound INVITE lands on this UA while the
//     first call is still established, baresip accepts it and fires its
//     own "incoming" bevent with a different call_id (nothing in
//     `ctrl_json.c`'s incoming path rejects a second call), and app.js's
//     own "incoming" case (see handleCallState) unconditionally overwrites
//     `state.call` with the new call's data - so `state.call.callId` can
//     legitimately point at the SECOND call's id while the FIRST call is
//     still ringing/established/live at the engine, unbeknownst to the UI.
//
// The moment that happens, a `closed` event for the FIRST call's id used
// to destroy the overlay now showing the SECOND, still-live call -
// hallazgo #5 of the audit, confirmed reachable (not the "sounds plausible
// but isn't" class of finding this project has been burned by 2026-08-14 -
// see PROTOCOL.md's own `attended_transfer`/`resume` rows for the
// protocol-level acknowledgment that two call_ids on one UA is a real,
// designed-for state, not a hypothetical).
//
// The frontend still only ever RENDERS one call at a time (no second-call
// UI exists yet - attended_transfer has no button in app.js today) - this
// function's job is narrower than "support two calls": it only makes sure
// a closed event for a call that ISN'T the one currently on screen doesn't
// wipe the one that IS.

/// @param currentCall - `state.call` as-is (may be null/undefined - no
///   call on screen at all, e.g. a stray/duplicate "closed" after the UI
///   already cleared it).
/// @param closedCallId - the closed event's own `call_id` (may be
///   null/undefined - see PROTOCOL.md "v0-compat": an old engine build, or
///   a call this UI never saw a call_id for).
/// @returns whether the "closed" handler should tear down `currentCall`.
export function shouldFinalizeClosedCall(currentCall, closedCallId) {
  if (!currentCall) return false; // nothing on screen to close
  // No id to compare on either side - can't tell these apart, so keep the
  // pre-fix behavior (always finalize) rather than getting stuck never
  // closing anything for a call this UI never attached an id to.
  if (!currentCall.callId || !closedCallId) return true;
  return currentCall.callId === closedCallId;
}

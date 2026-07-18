// Settings-Save -> registration-result handshake — pure state machine.
// Same "zero Tauri dependency" shape updater.js/transcript-panel.js
// document for themselves: nothing here touches `window.__TAURI__`,
// `document`, `setTimeout`, or i18n - app.js owns all of that (the actual
// Promise/timer plumbing in awaitRegResult, and the DOM/t() rendering in
// renderSaveStatusForRegState) and hands this module's reducer the events
// it gets back.
//
// Why a handshake needs a generation at all: clicking Save arms a wait for
// the *next* terminal reg_state, but SIP registration events aren't
// correlated to the Save that triggered them (the engine just reports
// "registered"/"failed", not "in response to save #N"). app.js's own
// swap-on-new-Save logic (awaitRegResult always retires the previous
// pendingRegResult before installing a new one) already keeps exactly one
// handshake live at a time, so in practice a stale reg_state can't reach
// an old handshake's resolve callback - `handshake.generation` here is a
// documented safety net for that invariant (same framing as app.js's own
// releaseSaveButton), not something normal operation currently relies on.
// Tested standalone anyway, so a future change to that swap logic fails a
// test here instead of shipping a silent cross-Save mixup.
//
// handshake shape: { generation: number } | null
//   generation - the state.regGeneration value active when this Save
//   armed its wait (see app.js awaitRegResult).

/// Arms a fresh handshake for the generation the caller just advanced to.
/// Pure bookkeeping only - app.js still owns the actual Promise/timer/
/// pendingRegResult object; this just gives that object's `generation`
/// field a name distinct from "some number app.js made up inline".
export function armHandshake(generation) {
  return { generation };
}

/// Reduces one incoming event against the currently active handshake (or
/// null if no Save is awaiting a result) into the next UI action.
///
/// @param handshake - the live handshake, or null.
/// @param currentGeneration - state.regGeneration as of right now. Equal to
///   handshake.generation in every reachable call today (see module header);
///   greater only if a newer Save has since armed its own handshake and,
///   hypothetically, this stale one is still being evaluated somehow.
/// @param event - either
///   { type: "reg_state", state: "registering"|"unregistered"|"registered"|"failed", reason?: string|null }
///   { type: "timeout" }
///
/// @returns one of:
///   { type: "ignore-stale" }            - no handshake, or it's been
///                                          superseded by a newer Save;
///                                          don't touch #save-status.
///   { type: "keep-connecting" }         - still in flight (registering/
///                                          unregistered/anything non-
///                                          terminal); show "Connecting…".
///   { type: "show-connected" }          - registered: TERMINAL, resolves
///                                          the handshake.
///   { type: "show-failed-retrying", reason } - failed: NOT terminal, the
///                                          engine auto-retries after a
///                                          failure, so a later registered
///                                          must still be able to settle
///                                          this same handshake green.
///   { type: "timeout" }                 - the wait's deadline passed with
///                                          no terminal reg_state.
export function reduceRegHandshake(handshake, currentGeneration, event) {
  if (!handshake || currentGeneration > handshake.generation) {
    return { type: "ignore-stale" };
  }
  if (event.type === "timeout") {
    return { type: "timeout" };
  }
  if (event.state === "registered") {
    return { type: "show-connected" };
  }
  if (event.state === "failed") {
    return { type: "show-failed-retrying", reason: event.reason || null };
  }
  return { type: "keep-connecting" };
}

/// True once an action settles the handshake (no further events should be
/// awaited for it - app.js clears pendingRegResult/its timer on this).
export function isTerminalRegAction(action) {
  return action.type === "show-connected" || action.type === "timeout";
}

/// Pure guard for the Save button's re-enable path (FIX C(2)): a save may
/// only re-enable the button if IT is still the newest generation. An older
/// Save's terminal path (settled after being preempted by a newer click)
/// must not re-enable a button a newer, still in-flight Save disabled.
export function shouldReleaseSaveButton(currentGeneration, myGeneration) {
  return currentGeneration === myGeneration;
}

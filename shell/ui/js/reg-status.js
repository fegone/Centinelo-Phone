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

/// Guards saveAccountSettings's interim "Connecting…" repaint, which runs
/// right before it awaits the handshake's result (after save_account_
/// settings/set_core_binary_path/save_favorites/save_transcription_settings/
/// the finally block's get_account_settings - several IPC round-trips that
/// run concurrently with the engine's own SIP re-register).
///
/// 2026-07-18 RELIABILITY regression this exists to close: a terminal
/// reg_state (registered, or failed-then-registered) can land DURING those
/// intermediate invokes. When it does, renderSaveStatusForRegState already
/// painted the real outcome live and resolved the handshake - repainting
/// "Connecting…" over that unconditionally would freeze #save-status on a
/// stale message the instant the already-resolved promise resolves right
/// after (reduceRegResult below is what un-freezes it, but the interim
/// repaint must not run in the first place or it visibly flashes/reverts).
///
/// @param pendingHandshakeGeneration - state.pendingRegResult?.generation,
///   or null if no handshake is currently pending (already settled, or -
///   structurally unreachable today since the Save button is disabled for
///   the whole handshake, but checked the same way as reduceRegHandshake's
///   own safety net - preempted by a newer Save).
/// @param myGeneration - the generation THIS saveAccountSettings call armed.
/// @returns true only if this save's own handshake is still genuinely
///   awaiting its terminal event.
export function shouldShowInterimConnecting(pendingHandshakeGeneration, myGeneration) {
  return pendingHandshakeGeneration === myGeneration;
}

/// Reduces the awaited handshake result into the final #save-status action,
/// independent of whatever renderSaveStatusForRegState already painted
/// live - always correct even where shouldShowInterimConnecting's repaint
/// was skipped (or, before this fix, would have stomped it).
///
/// @param result - awaitRegResult's resolution: { state: "registered" } or
///   { timedOut: true }. ("failed" is never terminal - see
///   reduceRegHandshake's show-failed-retrying doc - so it never reaches
///   here as a result.)
/// @returns { type: "show-connected" } (paint green/Connected) or
///   { type: "keep-last" } (no terminal registered arrived in time - leave
///   whatever #save-status last showed; the live reg-pill stays the true
///   source of truth, see the timedOut handling this replaces).
export function reduceRegResult(result) {
  if (result && result.state === "registered") return { type: "show-connected" };
  return { type: "keep-last" };
}

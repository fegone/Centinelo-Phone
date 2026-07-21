# core/ — ctrl_json wire protocol (v1.5)

`ctrl_json` (`core/modules/ctrl_json/`) is a baresip "application" module
that turns the running engine into a sidecar controllable over stdio:
newline-delimited JSON **commands** in on stdin, newline-delimited JSON
**events** out on stdout. It is the protocol a future Tauri shell (or
this repo's own test harnesses) speaks to drive the engine — modeled on
baresip's own `ctrl_tcp` module (JSON over TCP+netstring) and `stdio`
module (keyboard polling via `fd_listen`), with the transport swapped for
a plain stdio pipe (or, on Windows, a stdio pipe fed by a dedicated
reader thread — see "Framing / stdin" below) and the JSON shape narrowed
to what this engine actually needs.

**v1.1 status: every command below is implemented and e2e-verified
against the real test PBX** (FreePBX 17 / Asterisk 22 at
`<pbx host>`) — see `core/E2E-F1.md` for the v1 evidence and its "F3
regression" section for v1.1's. v1.1 is **fully backward compatible with
v1**: every v1 command/event is byte-for-byte unchanged in shape and
behavior; v1.1 only *adds* the optional `id` field (§"Commands"), two new
commands (`devices`/`set_device`), three new fields on `stats`, and fixes
stdout to be strictly, 100% NDJSON (no behavior change for a consumer
that was already following the "filter for lines starting with `{`"
advice this file used to give — that advice is now unnecessary but
harmless if a v1 consumer's code still does it). See "Changes from v1"
below for the full v1.1 changelog. v0 (dial/answer/hangup/quit only,
reg_state/call_state/error events, no call_id, no BLF/transfer/DTMF/
hold/mute/stats) is superseded; see "Changes from v0" below for exactly
what moved and why, if you're a consumer that was coded against v0.

**v1.2 status: `tap_start`/`tap_stop` are implemented and e2e-verified
against the real test PBX** — see `core/E2E-F1.md` "F4 audio tap". v1.2
is **fully backward compatible with v1.1**: nothing about any existing
command/event changed shape or behavior; v1.2 only *adds* the two new
commands and the `tap_state` event (§"Commands"/"Events"), the
foundation for local per-call transcription (F4/F5 - each direction is a
separate WAV file by construction, i.e. free 2-speaker diarization, no
speaker-separation model needed downstream). See "Changes from v1.1"
below for the full v1.2 changelog.

**v1.3 status: mixed — real e2e evidence below, not a blanket "verified"
claim** (the F4 receptionist-console gaps this version closes). v1.3 is
**fully backward compatible with v1.2**: every existing command/event is
byte-for-byte unchanged; v1.3 only *adds* an optional `call_id` on
`answer`, one new command (`park`) and its `park` event, and two new
`blf` event `state` values (`held`/`dnd`). Per-feature status, each
e2e-verified against the real test PBX (`core/E2E-F1.md` "F5
presence_override"/"F5 park", dual-contact ext 1000 trick):
- **`answer` with explicit `call_id`**: e2e **PASS** — a specific
  incoming call (not just "the" incoming call) answers correctly.
- **`held`**: the parser rule is implemented correctly to the RFC
  4235/3840-documented shape (unit tested against synthetic
  RFC-compliant fixtures) — **but real e2e against this repo's test PBX
  (FreePBX 17.0.30 / Asterisk 22.8.2, chan_pjsip) proved that PBX does
  NOT emit the standard hold signal** (`+sip.rendering` pvalue="no")
  when a dialog is put on local hold; a held call on *this* PBX still
  reports `busy`, not `held`, over `Event: dialog` — see "blf" below and
  `core/E2E-F1.md` "F5 presence_override" for the real captured NOTIFY
  proving this.
- **`dnd`**: best-effort, forward-compatible parser hook — **not
  e2e-verified** (would need toggling DND on the test extension via a
  feature code outside this repo's pre-authorized safe-target list —
  see `core/E2E-F1.md` "F5 presence_override" — and, independent of
  that, standard Asterisk chan_pjsip dialog-info has no dedicated
  element for DND at all, so it may never fire against a real Asterisk
  PBX regardless).
- **`park`**: command dispatches, sends a REFER to the parking lot's
  pilot extension (confirmed reachable and correct on this PBX, read-only
  — deployment-specific verification detail lives in `premium/docs/`, not
  here), and the synchronous `result`/`park` confirmation events fire
  correctly — **but
  real e2e surfaced an unresolved local (client-side, not PBX-rejected)
  error in baresip's own REFER-progress-subscription tracking
  specifically when the target is Asterisk's `Park()` application**
  (`call: subscription closed: Destination address required`, `errno`
  39/`EDESTADDRREQ`, not a SIP-level rejection — the PBX never sent a
  4xx/5xx and the bridged call was left untouched/unaffected, confirmed
  read-only) — see `core/E2E-F1.md` "F5 park" for the full repro and
  root-causing attempt. Not yet confirmed the call actually lands in the
  parking lot end-to-end.
See "Changes from v1.2" below for the full v1.3 changelog, and `core/
E2E-F1.md` "F5 presence_override"/"F5 park" for the underlying evidence
behind every claim above.

**v1.4 status: two independent HANDOFF-tracked engine-side gaps closed,
one of them changing this file's own `park` write-up above.** Both
e2e-verified against the real test PBX — see "Changes from v1.3" below
for the full changelog and `core/E2E-F1.md` for anything added there:

- **`audio_error`** (new event) — a distinct, `call_id`-correlated event
  for `BEVENT_AUDIO_ERROR`, alongside (not instead of) the plain `error`
  event this bevent already produced through v1.3. e2e-verified: a real
  running call's `ausrc` (microphone-side) error callback was fault-
  injected (a temporary, uncommitted 1-line change to
  `core/deps/baresip/modules/aufile/aufile_src.c`'s EOF-`errh` call,
  reverted immediately after capturing evidence — no baresip source
  patch was needed for this half, see "Changes from v1.3") to produce a
  real, nonzero-`err` `BEVENT_AUDIO_ERROR` against a real call bridged
  through the test PBX; both the new `audio_error` (correct `call_id`)
  and the unchanged `error` event fired, then the call correctly closed
  — matching `call.c`'s own unmodified `audio_error_handler()` behavior.
  **A real, separate, *not*-fixed-here gap this closes only half of:**
  only `ausrc` (microphone/input) failures reach `BEVENT_AUDIO_ERROR` at
  all — baresip's `auplay` (speaker/output) abstraction
  (`include/baresip.h`'s `auplay_alloc_h`/`auplay_alloc()`,
  `src/auplay.c`, `src/aureceiver.c`'s `aurecv_start_player()`) has no
  error-callback parameter whatsoever, unlike `ausrc_alloc()`'s own
  `ausrc_error_h` — an output device disconnecting/failing mid-call is
  architecturally invisible to this engine today, on every platform,
  regardless of any patch downstream of it. See "Changes from v1.3" for
  the full writeup of why this wasn't force-fixed here (it needs a
  baresip core API change plus real per-platform device-removal
  detection in every compiled `auplay` backend module — `coreaudio`,
  `wasapi` — verified against real hardware, not available this
  session).
- **`park`'s previously-100%-silent `EDESTADDRREQ` failure now
  surfaces** as a normal `error` event (`"transfer failed: Destination
  address required [39]"`, via the existing, unmodified
  `BEVENT_CALL_TRANSFER_FAILED` path — no new event type) instead of
  vanishing into an engine-local `info()` log line only visible with
  `-s`. **New, more precise finding, real e2e evidence both ways**: the
  `EDESTADDRREQ` failure this file's own v1.3 status paragraph above
  documents is **wss-transport-specific** — re-running the *exact* same
  dual-contact-1000-into-`Park()` scenario over plain `udp` (this
  session, real test PBX, full `-s` SIP trace captured) shows the REFER
  genuinely reaching the wire, a `202 Accepted`, and a final `NOTIFY`
  sipfrag body of `SIP/2.0 200 OK` — **`park` works end-to-end over
  udp/tcp transport today**, contradicting nothing above (every prior
  documented `park`/`held` scenario in `core/E2E-F1.md` used `wss`,
  never `udp`) but meaningfully narrowing the gap: it is not "`park`
  doesn't work", it is "`park` doesn't work specifically when the
  engine's own SIP signaling transport is `wss`". The underlying
  wss-specific root cause inside `re`'s sipevent/websocket-transport
  interaction remains genuinely unresolved (see "Planned" below) — not
  forced, per this version's own scope decision to fix the *silent*
  half (a real, safe, bounded patch) and document rather than guess at
  the *transport* half (deep, `re`-internal, needs a `re`-level trace
  this session's time budget didn't extend to).

**v1.5 status: one new command, `set_answer_mode` — unit-tested (`cmd.c`
parsing, `test/test_main.c`, ASan-clean, see "Changes from v1.4" below)
AND e2e-verified against the real test PBX (2026-07-21) — see
`core/E2E-F1.md` "F7 set_answer_mode".** v1.5 is **fully backward
compatible with v1.4**: every existing command/event is byte-for-byte
unchanged; v1.5 only *adds* `set_answer_mode`. The real incoming-call
auto-answer run against the test PBX (dual-contact self-bridge on ext
1100, engine rebuilt fresh from the `set_answer_mode` commit first) is
done: `mode:"auto"` answered a genuine incoming `INVITE` with **zero**
driver-sent `answer` command, corroborated PBX-side by growing RTP
receive counters (`pjsip show channelstats`) on the answered leg and a
shared `BridgeID`; `mode:"manual"` (the default) correctly did **not**
self-answer under the same conditions (PBX-side: still `Ringing`, no
`BridgeID`, `channelstats` `not valid` — consistent with never
answered) and still answered normally once told to via an explicit
`answer` command; invalid `mode` values are rejected without crashing
the engine; idempotent. No bugs found in `set_answer_mode`/`menu.c`'s
auto-answer path. See "Changes from v1.4" below for the implementation
detail and `core/E2E-F1.md` "F7" for the full run, PBX evidence, and
findings (incl. a stale-build-binary gotcha this session caught before
it could produce a false result).

## Framing

One JSON object per line (`\n`-terminated; a trailing `\r` is tolerated).
No netstring/length-prefix framing — plain newline-delimited JSON
(NDJSON).

**stdout is pure NDJSON, end to end, as of v1.1.** Every line on stdout
is one `ctrl_json`-emitted JSON object; nothing else is ever written
there. Confirmed empirically, not just by inspection: capturing stdout
from a full register→dial→quality_stats→devices→set_device→hangup→quit
run against the real test PBX (with **and** without `-s`/SIP trace) and
running `grep -cv '^{'` against the capture returns `0` both times — see
`core/E2E-F1.md` "F3 regression" for the exact commands and output.
`ctrl_json.c`'s own `emit()` writes each JSON line with `fwrite()` + an
explicit `fflush(stdout)` (not a raw `write()` — see "Changes from v0" —
immediate per-line delivery, no libc stdio buffering surprises).

**v1 status (superseded, kept for history/migration context):** stdout
used to carry three kinds of non-JSON noise ahead of/around
`ctrl_json`'s own lines — baresip's one-line startup banner (printed in
`main()` before any module loads), human-readable module-load log lines
from every module loaded before `ctrl_json` (`ctrl_json` is always
loaded last), and — only if run with `-s` — raw SIP trace text. A v1
consumer had to filter stdout for lines whose first non-whitespace
character is `{` and ignore everything else. **v1.1 fixes all three at
the source** (two small baresip/re patches — `core/patches/0003-*` and
`core/patches/0004-*`, see `core/BUILD.md` "Findings" for the full
per-source breakdown of what was leaking and why) rather than asking
every consumer to keep filtering: the old "filter for `{`" logic is now
unnecessary, but still harmless to leave in a v1 consumer's code if it's
already there (it will just never see anything to filter out anymore).

stderr carries baresip's own human-readable debug/info/warning log (now
*including* everything the v1.1 fix diverted off stdout — see
`core/BUILD.md` "Findings" for exactly what that is and why it wasn't
simply dropped), plus `run-spike.sh`'s own startup summary lines. `-v`
(verbose) / `-s` (SIP trace, see `run-spike.sh`'s `CENT_BARESIP_ARGS`)
add detail there. Controlled by `CENT_JSON_STDOUT` (any non-empty value;
`run-spike.sh` sets it by default — see that script's own header comment
to opt back out, e.g. for interactive by-hand debugging of a fresh build
without `ctrl_json` in the loop at all).

### stdin

- **POSIX** (macOS/Linux): unchanged from v0 — `fd_listen(STDIN_FILENO,
  FD_READ, ...)` + `read()`, proven against the real PBX throughout this
  repo's e2e testing.
- **Windows**: `STDIN_FILENO`/`read()`/`fd_listen()` on a console or
  piped handle has no equivalent in this codebase's win32 support (re's
  `fd_listen()` is a socket-readiness primitive), so `ctrl_json.c` uses a
  dedicated reader thread (`re_thread.h`'s `thrd_create` — a portable
  C11-style wrapper, no `#ifdef` needed for thread creation itself) doing
  blocking `fgets()` on `stdin`, handing each complete line to the main
  / `re_main()` thread via `re_mqueue.h` (documented as the thread-safe
  way to bridge a worker thread into the event loop). Both paths funnel
  into the exact same `process_line()`/command-dispatch code — see
  `ctrl_json.c`'s "stdin - Windows" section for the full rationale,
  including why the reader thread is deliberately never `thrd_join()`'d.
  **Not yet reached by `windows-latest` CI as of this version** — an
  earlier, unrelated step (baresip's own CMake `find_package(re)`
  discovery on Windows) fails first; this file's own code is only
  syntax-checked locally (`clang -fsyntax-only -D_WIN32`), not compiled
  by MSVC or run on real Windows hardware — see core/BUILD.md "Windows
  CI" for the exact CI error and why fixing it is out of this version's
  scope.

## Commands (stdin)

`call_id` is accepted as an optional field on every call-scoped command
below. When omitted, the command targets "the current call" — see
"Changes from v0" for exactly what that means with more than one call in
play (attended transfer). When given, it's resolved via `uag_call_find()`
(searches every UA's call list — this engine registers exactly one UA,
see `run-spike.sh`) — an unresolvable id (or "no current call" when
omitted) always produces an `error` event, never a crash.

**New in v1.1:** `id` (an opaque, caller-chosen string) is accepted as an
optional field on **every** command below, call-scoped or not — see
"Changes from v1" and the `result` event for what it does. It has nothing
to do with `call_id`; the two are independent and a command can carry
either, both, or neither.

| JSON | Effect |
|---|---|
| `{"cmd":"dial","uri":"sip:*43@host"}` | Dial `uri`. Unchanged from v0: `cmd_process_long(commands, "dial <uri>", ...)`, reusing the `menu` module's dial/UA-selection logic. |
| `{"cmd":"answer","call_id":"..."}` | Answer an incoming call. `call_id` is **new in v1.3**, optional (falls back to "the" current incoming call, unchanged v0 behavior) — lets a queue-aware caller (the receptionist console, with more than one incoming call in play) say exactly which one to answer, rather than only ever "the" incoming call. Maps to baresip's long command `accept` (unchanged from v0) — `accept <call-id>` is baresip's own existing menu-module syntax (`modules/menu/static_menu.c` `cmd_answer()`, confirmed by reading it) for resolving a specific call via `uag_call_find()`, so v1.3 only had to build that string when `call_id` is present; no new call-resolution code was needed. e2e-verified — see `core/E2E-F1.md` "F5 presence_override" (the `answer`+`call_id` scenario). |
| `{"cmd":"quit"}` | Clean shutdown. Unchanged from v0: maps to baresip's long command `quit`. Also triggered automatically on stdin EOF/closure (both the POSIX and Windows stdin paths). |
| `{"cmd":"hangup","call_id":"..."}` | Hang up a call (or the current one). **Changed from v0** — see "Changes from v0": now `ua_hangup()` directly (not `cmd_process_long`), for consistent call resolution with every other call-scoped command, and `call_id` is now accepted. |
| `{"cmd":"register"}` | Re-register the engine's one UA at runtime (`ua_register()`). v0 only registered once, at process start. |
| `{"cmd":"unregister"}` | Unregister at runtime (`ua_unregister()`). |
| `{"cmd":"hold","call_id":"..."}` | Put a call on hold (`call_hold(call, true)` — a re-INVITE, `a=sendonly`/similar). Emits `call_state` `"hold"` on success (see "Events"). |
| `{"cmd":"resume","call_id":"..."}` | Take a call off hold (`uag_hold_resume()` — also holds whatever *other* call is currently active first, so two calls are never both off-hold at once; matters the moment there's a second call, i.e. attended transfer). Emits `call_state` `"resumed"` on success. |
| `{"cmd":"dtmf","digits":"123#","call_id":"..."}` | Send RFC2833 DTMF. `digits` is any sequence of `0-9 * # A-D`; invalid characters produce an `error`. |
| `{"cmd":"mute","on":true,"call_id":"..."}` | Mute (`on:true`) or un-mute (`on:false`) the call's outgoing audio. `on` is a required, real JSON boolean (not the string `"true"`). Emits `call_state` `"muted"`/`"unmuted"` on success. |
| `{"cmd":"blind_transfer","uri":"sip:target@host","call_id":"..."}` | REFER the call to `uri` (`call_transfer()`). Does **not** implicitly hold the call first (unlike the interactive `menu` module's own transfer key) — hold is a separate, composable command; send `hold` first if that's the desired UX. Outcome is asynchronous — see "Events". |
| `{"cmd":"attended_transfer","uri":"sip:target@host","call_id":"..."}` | Start an attended transfer: holds the named/current call (the "source"), then dials `uri` as a new "consultation" call on the same UA. Fails with an `error` if another attended transfer is already pending (F1 supports one at a time, matching `modules/menu/dynamic_menu.c`'s own single-slot design) or if the source's peer doesn't support the `Replaces` extension. Emits `call_state` `"hold"` for the source, then `attended_transfer_started` (see "Events"), then the consultation call's normal `call_state` lifecycle (`ringing`/`established`/...). |
| `{"cmd":"complete_transfer"}` | Complete a pending attended transfer: REFERs the source call's peer to the consultation call's peer with a `Replaces` header (`call_replace_transfer()`), so the two outside parties end up connected directly and both of this engine's legs drop. `call_id` is accepted (for a future multi-pending-transfer world) but not currently used to disambiguate — there is at most one pending transfer. |
| `{"cmd":"abort_transfer"}` | Cancel a pending attended transfer without completing it: resumes the held source call (`uag_hold_resume()`) and hangs up the consultation call is left to the caller (send `hangup` with the consultation's `call_id`, from `attended_transfer_started`, if that's also wanted) — `abort_transfer` itself only un-pends the transfer and un-holds the source. Not in the original F1 command list; added because without it a pending attended transfer had no clean cancel path short of hanging up the source outright. |
| `{"cmd":"quality_stats","call_id":"..."}` | Emit a `stats` event (see "Events") for the call's current RTCP-derived counters, **enriched in v1.1** with codec/transport (see "Events" `stats` row). |
| `{"cmd":"blf_subscribe","ext":"510"}` | SIP SUBSCRIBE `Event: dialog` (RFC 4235) to `sip:<ext>@<same PBX host the account registered against>`, `Accept: application/dialog-info+xml`, refreshed automatically by `re`'s sipevent layer for as long as the subscription lives (no polling here). Emits `blf` events (see "Events") as NOTIFYs arrive, starting with the initial one. Errors if already subscribed to that `ext`. |
| `{"cmd":"blf_unsubscribe","ext":"510"}` | Cleanly unsubscribes (`Expires: 0`) and stops tracking `ext`. Errors if not currently subscribed. |
| `{"cmd":"devices"}` | **New in v1.1.** Emit a `devices` event (see "Events") enumerating audio input/output devices and which is active. |
| `{"cmd":"set_device","kind":"input","name":"..."}` | **New in v1.1.** Select an audio device for `kind` (`"input"` or `"output"`, required, case-insensitive). `name` (required) is a `devices` event's own `"name"` value, round-tripped verbatim — see "Events" `devices` row for its `"<module>[,<device>]"` shape. Persists as the default for calls started after this command (`conf_config()->audio.{src,play}_{mod,dev}`) **and** applies live to whatever call is active right now, if any, via baresip's `audio_set_source()`/`audio_set_player()` hot-swap API (investigated while building this: both genuinely stop and restart the running audio source/player against the same live call, no re-INVITE needed — see `ctrl_json.c` `cmd_set_device()`'s own comment). Scoped to "the current call" like every other no-`call_id` command in this file (see `resolve_call()`) — a concurrent second call (an attended-transfer consultation leg) is not touched. |
| `{"cmd":"tap_start","dir":"/abs/path","call_id":"..."}` | **New in v1.2.** Starts tapping the resolved call's audio to two new mono 16-bit PCM WAV files under `dir` (required — an absolute, already-existing, writable directory; this command doesn't create it): `<dir>/<call_id>-rx.wav` (the remote party — decoded incoming audio) and `<dir>/<call_id>-tx.wav` (the local party — outgoing audio before encode). `call_id` is optional like every other call-scoped command (falls back to "the current call" — see `resolve_call()`); resolving to no call is an `error`, same as `hold`/`mute`/etc, **not** an "arm for the next call" — a tap always targets a call that already exists at the moment this command runs (see `core/E2E-F1.md` "F4 audio tap" for why the e2e sequence dials first, then taps). Errors (all `error` events, none of them crash the engine or the call): no current/resolvable call, the call has no audio yet, `dir` is missing/empty, a tap is already running for this call (stop it first), or the output file(s) couldn't be opened (bad `dir`, not writable, ...). Both files exist on disk (0 bytes) as soon as this command succeeds; each one's real WAV header is committed on that direction's first actual audio frame, not synchronously with this command (see `core/modules/ctrl_json/wav_writer.h`) — typically sub-20ms later on an already-flowing call. Sample rate is whatever the negotiated codec's audio path actually runs at, taken from each direction's real first frame, never guessed — this build's actual account (`audio_codecs=pcmu,pcma`, see `run-spike.sh`) runs at 8000 Hz mono; see `core/E2E-F1.md` "F4 audio tap" for the real numbers. Output is always mono — a source frame with more than one channel is downmixed (integer average) first, though this build's actual codec set never produces one (G.711 decodes to mono) — see `audiotap.c` `write_frame()`. |
| `{"cmd":"tap_stop","call_id":"..."}` | **New in v1.2.** Stops a running tap on the resolved call, finalizing both WAV headers (correct final `RIFF`/`data` chunk sizes — a tap is also auto-finalized on call teardown even without this command, see "Events" `tap_state` row). Errors if the resolved call doesn't exist, has no audio, or has no tap currently running. |
| `{"cmd":"park","ext":"<pilot ext>","call_id":"..."}` | **New in v1.3.** Parks a call by blind-transferring it (REFER, the exact same `call_transfer()` mechanism `blind_transfer` already uses) to `ext` — the target parking lot's **pilot** extension. `ext` is **required**, not defaulted — a pilot extension is per-PBX configuration, not a protocol constant this engine should guess at (unlike `*43`/`*60` test codes, which `park` never used either). Same target-address shape as `blf_subscribe`'s own `ext` (`sip:<ext>@<same PBX host the account registered against>` — see `build_pbx_ext_uri()` in `ctrl_json.c`, shared by both). `call_id` is optional, same `resolve_call()` fallback convention as every other call-scoped command. **The confirmation event's `ext` is always the pilot extension targeted, never a specific auto-assigned parking slot** — see "Events" `park` row for why, and this file's own v1.3/v1.4 status paragraphs plus `core/E2E-F1.md` "F5" for this command's current real-PBX e2e status (**v1.4: works end-to-end over udp/tcp; over wss, the async failure now at least surfaces as an `error` event instead of vanishing silently — see v1.4 status paragraph**). |
| `{"cmd":"set_answer_mode","mode":"auto"}` | **New in v1.5.** Sets the engine's one account (`ua_account(primary_ua())`) to auto-answer (`mode:"auto"`) or wait for an explicit `answer` command (`mode:"manual"`, the default a fresh account starts at). `mode` is **required**, case-insensitive, exactly `"auto"` or `"manual"` — anything else is an `error`. Not call-scoped (no `call_id` — this flips a per-account setting, not a per-call one). Maps directly to baresip's own `account_set_answermode()`; when `ANSWERMODE_AUTO`, the `menu` module (already loaded — see `core/BUILD.md` "Module selection") answers every subsequent incoming `INVITE` itself on `BEVENT_CALL_INCOMING`, so the effect is live starting with the very next incoming call, no restart/re-register needed. Idempotent (setting the same mode twice, or `manual` on an already-manual account, is a harmless no-op). No dedicated confirmation event — success is acked the same way `set_device` is: only via the optional request/response `id` correlation (`result`, `ok:true` — see "Events"), since there's no natural per-call follow-up event for an account-level setting. Unit-tested (`cmd.c`/`test/test_main.c`) and **e2e-verified against the real test PBX** (2026-07-21) — see `core/E2E-F1.md` "F7 set_answer_mode" and this file's own top-of-file v1.5 status paragraph. |

Unknown `cmd` values, a required field missing/wrong-typed (e.g. `dial`
without `uri`, `mute` without a real boolean `on`, `set_device` with a
`kind` that isn't `"input"`/`"output"`), or a baresip call that returns
an error, all produce an `error` event rather than crashing or hanging;
the connection stays up. The JSON-decoding + field-validation half of
this (everything except actually calling into baresip) is pure,
unit-tested code — see `core/modules/ctrl_json/cmd.c` and
`test/test_main.c`.

**v1.1 adds per-command request/response correlation** (an optional
`id`, echoed back on a `result` event — see "Events") — the "Planned"
`token`-style envelope this section used to point at under v1 is now
implemented; see "Changes from v1" for the full contract, including
exactly what `ok:true`/`ok:false` do and don't promise about a command's
*asynchronous* outcome.

## Events (stdout)

| JSON | When |
|---|---|
| `{"event":"ready"}` | Once, right after `ctrl_json` finishes initializing — the earliest point commands can safely be sent. Unchanged from v0. |
| `{"event":"reg_state","account":"...","state":"registering\|registered\|failed\|unregistered","transport":"udp\|tcp\|tls\|ws\|wss","reason":"..."}` | On every registration transition — now including transitions caused by the runtime `register`/`unregister` commands, not just process-start registration. `reason` present only on `failed`. Unchanged shape from v0. |
| `{"event":"call_state","state":"...","peer":"...","id":"...","call_id":"...","}` | **`call_id` is new in v1** (added alongside the original `id` field, same value — kept both so a v0 consumer reading `id` doesn't break; a future v2 may drop `id`). `state` values beyond v0's `incoming\|ringing\|established\|closed`: **`hold`/`resumed`** (fired both for this engine's own local hold/resume commands — synthetically, right at the command's own success path, since baresip has no bevent for *locally*-initiated hold/resume, only peer-initiated — and relayed from `BEVENT_CALL_HOLD`/`BEVENT_CALL_RESUME` for a *peer*-initiated hold/resume) and **`muted`/`unmuted`** (from `mute`). None of these correspond to baresip's own `CALL_STATE_*` lifecycle machine changing — hold/mute are attributes of an otherwise-established call, not lifecycle transitions — they're folded into `call_state` anyway rather than inventing a new event per attribute, since from a consumer's perspective they're all "something about this call just changed, here's its id". |
| `{"event":"error","message":"..."}` | Malformed/unparseable input line, unknown `cmd`, a required field missing/wrong-typed, a baresip command that returned an error, `BEVENT_AUDIO_ERROR` (also emits the dedicated `audio_error` event below, **new in v1.4** — this plain `error` still fires too, unchanged, for back-compat), or `BEVENT_CALL_TRANSFER_FAILED` (an async transfer failure reported by the far end after `blind_transfer`/`complete_transfer` already returned success synchronously — reuses this existing event/shape rather than inventing a transfer-specific one; **as of v1.4** also fires for a `park`/`blind_transfer` subscription-*establishment* failure, e.g. the wss-specific `EDESTADDRREQ` finding under `park` below — previously silently swallowed, see "Changes from v1.3" and `core/patches/0005-*`). |
| `{"event":"audio_error","call_id":"...","message":"..."}` | **New in v1.4**, from `BEVENT_AUDIO_ERROR`, alongside the plain `error` event above (both fire, same underlying bevent). `call_id` is the failing call's real id (same convention as `call_state`/`tap_state`/`park`) — the plain `error` event has no `call_id` at all, so this is the only way to know *which* call's audio failed when more than one is in play. `message` is the identical text the plain `error` event's own `message` carries (baresip's own `"%d,%s", err, str` format from `call.c`'s `audio_error_handler()` — not re-parsed/restructured here). e2e-verified via fault injection (see this file's own top-of-file v1.4 status paragraph) — **only reachable for `ausrc`/microphone-side failures today**, see that same paragraph for the separate, unfixed `auplay`/speaker-side gap. |
| `{"event":"stats","call_id":"...","rtt_us":N,"tx_packets":N,"tx_lost":N,"tx_jitter_us":N,"rx_packets":N,"rx_lost":N,"rx_jitter_us":N,"codec":"...","transport":"udp\|tcp\|tls\|ws\|wss"}` | New in v1, from `quality_stats`. Sourced from `stream_rtcp_stats()` (`src/stream.c`) — **this reflects the most recently *received* RTCP Sender/Receiver Report, not a live per-packet counter.** Querying more often than the RTCP interval (empirically ~10-20s against the test PBX, see `core/E2E-F1.md`) returns identical numbers between reports; that's correct RTCP behavior, not a bug or a stale/broken reading. Query again after waiting a few RTCP intervals if you need fresher numbers. `rtt_us` is frequently `0` against a real PBX even while every other field is healthy/non-zero — RTCP round-trip-time needs a full SR/RR/DLSR round trip to populate, which this repo's test PBX has never been observed to complete (see `core/E2E-F1.md` scenario (d)); don't read a `0` there as "stats are broken". **`codec`/`transport` are new in v1.1**: `codec` is the call's negotiated TX/encoder codec name (`audio_codec()`, `src/audio.c`) — omitted entirely (not an empty string) if not yet negotiated; `transport` is the *call's own* actual SIP transport (`call_transp()`, not the account's static config) using the same vocabulary as `reg_state`'s `transport`. |
| `{"event":"blf","ext":"...","state":"idle\|ringing\|busy\|held\|dnd\|offline"}` | New in v1, from `blf_subscribe`. `idle`: no active dialog for that extension (either no `<dialog>` element in the NOTIFY body, *or* one present with `<state>terminated</state>` — both occur in practice, see `core/E2E-F1.md` for the real captured body, which uses the second shape). `ringing`: `<state>` is `early`/`proceeding`/`trying`. `busy`: `<state>confirmed</state>`, no hold signal (see `held` below). `held` (**new in v1.3, "presence_override"**): `<state>confirmed</state>` *and* the dialog's NOTIFY body also carries the RFC 4235/RFC 3840 standard hold indication (a `<target>` `+sip.rendering` param, `pvalue="no"`) — see `core/modules/ctrl_json/dialog_info.h`'s own header comment on `CENT_BLF_HELD` for the full parsing rule. **Real-PBX finding**: this engine's test PBX (FreePBX 17.0.30 / Asterisk 22.8.2, chan_pjsip) does **not** actually emit this signal for a locally-held call — confirmed via a real NOTIFY captured mid-hold (3 separate NOTIFYs across the hold window, `version=` incrementing each time, all byte-identical to the plain `busy` shape) — so a held call on *this* PBX currently still reports `busy`, not `held`; the parser rule itself is implemented correctly to the RFC-documented shape and unit tested against synthetic RFC-compliant fixtures, and will report `held` the moment a NOTIFY body actually carries the param (a different/future PBX config, or a different vendor). See `core/E2E-F1.md` "F5 presence_override" for the full real-capture evidence. `dnd` (**new in v1.3, "presence_override", best-effort**): a non-standard `<dnd>true</dnd>` element or `dnd=` attribute anywhere in the NOTIFY body — see `dialog_info.h`'s `CENT_BLF_DND` comment. **Not verified against a real Asterisk capture** — standard Asterisk chan_pjsip `Event: dialog` hints have no dedicated element for "this extension is in DND" (dialog-info is a *dialog* package; DND is a device-config state, not a dialog — an idle-but-DND'd extension has zero active dialogs either way, indistinguishable from plain idle at this layer without something extra in the XML, which this repo has not observed Asterisk actually send). `offline`: the subscription itself failed/was rejected/expired before a NOTIFY could be parsed, *or* a `<dialog>` element was present with no parseable `<state>` — the "can't currently tell" bucket. Parsing is pure, tiny, and unit-tested against both synthetic bodies and real captures (idle *and*, new in v1.3, the mid-hold "still busy" real capture) — see `core/modules/ctrl_json/dialog_info.c` and `test/test_main.c`. |
| `{"event":"attended_transfer_started","source_call_id":"...","target_call_id":"..."}` | New in v1, from `attended_transfer`, right after the consultation call's dial succeeds. Lets a consumer correlate exactly which two `call_id`s a pending `complete_transfer`/`abort_transfer` will act on — there's no other way to learn `target_call_id` (it's a brand new call, not something the caller supplied). |
| `{"event":"devices","input":[{"name":"...","active":true\|false},...],"output":[...]}` | **New in v1.1**, from `devices`. `name` is a `"<module>[,<device>]"` composite (matching baresip's own `audio_source`/`audio_player` config-file syntax) — pass it straight back as `set_device`'s own `"name"` field to select that device. This spike's actual module set (`ausine` input / `aufile` output only, see `core/BUILD.md` "Module selection" — no `coreaudio`/`alsa`/`wasapi`/...) has no real per-device enumeration, so today each of `input`/`output` always has exactly one entry — the driver module itself standing in for "the device" — rather than a genuinely empty or fake-populated list; a future real device-backend module plugs in with no protocol change (see `ctrl_json.c` `devices_add_driver()`'s own comment for exactly how the fallback works). |
| `{"event":"result","id":"...","ok":true\|false,"error":"...?", ...}` | **New in v1.1**, from any command that carried an `id` (see "Commands") — a direct, correlated acknowledgment of that *specific* command's own synchronous dispatch. `ok:true` means the command was accepted and dispatched without a synchronous validation/API failure — it is **not** a promise about anything asynchronous: e.g. a `blind_transfer` that gets `result ok:true` can still fail far-end minutes later, surfaced the same way it always was, as a `BEVENT_CALL_TRANSFER_FAILED`-sourced `error` event — watch the normal `call_state`/`reg_state`/`stats`/`blf`/... events for that, same as always; `result` only ever reports the exact same synchronous success/failure an `id`-less send of the same command would have shown via a normal event (an `error` event on failure, nothing extra on success) — `id` doesn't change *what* happens, only whether you get a correlated acknowledgment of it. `error` is present (and identical to the text a plain `error` event would carry) only when `ok:false`. `quality_stats` and `devices` additionally merge their own "command-specific fields" (the same fields their own `stats`/`devices` event would carry) directly onto a successful `result`, so a correlated caller doesn't need to also match up a second event by hand just to read the data it asked for — every other command's `result` is just `{"event":"result","id":"...","ok":true}` on success. `tap_start`/`tap_stop` are **not** in the merge list (like `hold`/`mute`/`blind_transfer`/...) — they're action commands, not query commands; their real data travels on the dedicated `tap_state` event below, the same way `hold`'s travels on `call_state`. |
| `{"event":"tap_state","call_id":"...","state":"started"\|"stopped","rx_path":"...","tx_path":"...", ...}` | **New in v1.2**, from `tap_start` (`state:"started"`) and from `tap_stop` **or** call teardown (`state:"stopped"` either way — see `audiotap.h` `audiotap_call_closed()`: a tap that outlives its own `tap_stop`, e.g. the peer hangs up first, is auto-finalized so a WAV file is never left open/corrupt). `call_id` is always the resolved call's real id, regardless of whether the triggering command supplied one — same convention as `call_state`. `rx_path`/`tx_path` are present on both states (the same two paths `tap_start` chose, echoed back on `stopped` too so a consumer doesn't have to have kept them from the `started` event). `"stopped"` additionally carries `rx_bytes`/`tx_bytes` (PCM data bytes written, WAV header excluded) and `rx_duration_ms`/`tx_duration_ms` (derived from bytes/sample-rate/sample-size, integer math) — `"started"` never carries these fields at all (nothing's been written yet, not even a zero) — see `core/E2E-F1.md` "F4 audio tap" for real captured numbers. **Security note (v1.3):** `rx_path`/`tx_path`'s filename component is derived from `call_id`, which — for an *incoming* call — is the far end's own SIP `Call-ID` header, not an engine-generated value; see "Changes from v1.2" below and `core/modules/ctrl_json/pathsafe.h` for why that value is sanitized before ever reaching a filesystem path, and confirm any future code that interpolates a call_id into a path does the same. |
| `{"event":"park","call_id":"...","ext":"..."}` | **New in v1.3**, from `park`, right after the REFER dispatches successfully (synchronous acceptance only — same "not a promise about the async outcome" caveat as `blind_transfer`'s own `call_state`/`error` story, see that command's row and `cmd_park()`'s own comment in `ctrl_json.c`). `call_id` is the resolved call's real id (same convention as `call_state`/`tap_state`). `ext` is always the **pilot** extension the park request targeted (echoed back from the command), **never** a specific auto-assigned parking-lot slot number — genuinely not observable over plain SIP signaling this engine's call leg is party to (confirmed by reading how Asterisk's REFER handling and `Park()` interact here, not guessed — see `core/E2E-F1.md` "F5 park"); a future consumer that needs the *actual* assigned slot would need an AMI/ARI integration, out of scope for this SIP-only engine. See this file's own top-of-file v1.3/v1.4 status paragraphs for `park`'s current real-PBX e2e status — **v1.4**: confirmed landing in the lot end-to-end over udp/tcp (real SIP trace: REFER on the wire, `202 Accepted`, final NOTIFY sipfrag `SIP/2.0 200 OK`); over wss specifically, the REFER-progress-subscription issue remains (see "Planned"), but the failure now surfaces via a normal `error` event (`BEVENT_CALL_TRANSFER_FAILED`, `core/patches/0005-*`) instead of silently vanishing. |

## Changes from v0

v0 shipped with only `dial`/`answer`/`hangup`/`quit` and
`reg_state`/`call_state`/`error`. Everything above `hold` in the commands
table, and everything from `stats` on in the events table, is new. A few
things worth calling out explicitly for anyone who integrated against v0:

- **`hangup` no longer routes through `cmd_process_long()`.** v0's
  `hangup` (and `dial`/`answer`/`quit`, unchanged) went through baresip's
  long-command dispatch, which resolves "the current call" via the
  `menu` module's own private state (`menu_uacur()`/`menu.curcall`,
  updated on ringing/established/etc. bevents). Every new v1 command
  needing "the current call" instead resolves it via the public
  `ua_call()`/`uag_call_find()` API (see `resolve_call()` in
  `ctrl_json.c`), which is a *different* mechanism that could disagree
  with menu's in a 2-call scenario (exactly the shape attended transfer
  creates). Rather than ship two different, occasionally-disagreeing
  definitions of "current call" depending on which command a consumer
  happens to send, v1 moves `hangup` onto the same direct-API path as
  hold/resume/dtmf/mute/etc, for one consistent definition everywhere.
  Confirmed behavior-preserving for the single-call case (all of this
  repo's e2e testing exercises plain hangup repeatedly); `dial`/
  `answer`/`quit` are unaffected (they don't have a "which call"
  resolution question in the first place — see `ctrl_json.c`'s
  top-of-file comment for the full reasoning).
- **`call_id` is now on every call-scoped command and event**, optional
  on input (falls back to "current call"), always present on
  `call_state` (as both `id` and `call_id`) and the new `stats` event.
- **stdout is written with `fwrite()`+`fflush()`, not `write()`.** No
  behavior change (same immediate per-line flush), just drops the
  `<unistd.h>` dependency from the output path as part of this version's
  Windows-portability work.
- **`sip_verify_server no` (the default for this spike's self-signed/
  internal-CA PBX cert) used to mean "no certificate check at all" for
  the WSS connection.** `CENT_TLS_PIN` (see `core/BUILD.md` "TLS
  verification") now adds an independent SHA256 leaf-certificate pin
  check on top, so a connection can be rejected even with server
  verification otherwise disabled. Off (current behavior preserved) when
  the env var is unset.

## Changes from v1

v1.1 is additive and fully backward compatible — nothing a v1 consumer
already relies on changed shape or behavior. Everything below is new:

- **Per-command request/response correlation** (`id` on input, `result`
  on output — see "Commands"/"Events"). Implemented via
  `cmd.have_id`/`cmd.id` (decoded unconditionally in `cent_cmd_decode()`,
  before `cmd` itself is even inspected, so even a hard decode error or
  an unrecognised `cmd` value can still be correlated back to its
  caller) and a `g_error_seq` counter bumped by every `emit_error()`
  call, snapshotted immediately before and compared immediately after a
  command's dispatch in `process_line()` — a moved counter means some
  `emit_error()` fired *during that dispatch*, i.e. the command failed.
  This intentionally required **zero signature changes** to any existing
  `cmd_*`/`do_*` handler — every one of them is byte-for-byte unchanged
  by this feature; only `process_line()` and `emit_error()` itself grew
  the new plumbing. See `ctrl_json.c` `emit_result()`'s own comment for
  the full contract, including exactly what `ok:true` does and doesn't
  promise.
- **`devices`/`set_device`** (see "Commands"/"Events") — audio device
  enumeration and live/persistent selection. `set_device` applies
  *both* live (to whatever call is active right now, via baresip's
  `audio_set_source()`/`audio_set_player()` hot-swap API — confirmed by
  reading both implementations that they genuinely restart the running
  audio source/player against the same live call, no re-INVITE) *and*
  persistently (as the default for future calls) — the F1.1 task this
  shipped under asked to "investigate briefly" whether baresip supports
  hot-swap; it does, and this uses it, rather than only doing the
  "apply at next call" half.
- **`quality_stats`/`stats` enrichment**: `codec` (TX/encoder codec
  name) and `transport` (the call's own actual SIP transport, not the
  account's static config) — see "Events" `stats` row for the full
  field semantics and the pre-existing `rtt_us`-often-reads-`0` caveat
  (unchanged from v1, just written down more prominently here since
  it's easy to mistake for something the enrichment work broke).
- **stdout is pure NDJSON.** See "Framing" above for the full story —
  three independent leak sources (baresip's own startup banner +
  module-load logging, SIP trace, and a handful of unconditional
  `re_printf()`s in `re`'s SIP-over-WS transport code), three
  independent fixes (`core/patches/0003-*`/`0004-*`, see
  `core/BUILD.md` "Findings"), confirmed with `grep -cv '^{'` returning
  `0` against a real e2e run's captured stdout (`core/E2E-F1.md` "F3
  regression"), with and without `-s`.

## Changes from v1.1

v1.2 is additive and fully backward compatible — nothing a v1.1 consumer
already relies on changed shape or behavior. Everything below is new:

- **`tap_start`/`tap_stop`** (see "Commands"/"Events") — per-call audio
  tapping to two mono 16-bit PCM WAV files, the foundation for local
  transcription (F4/F5): each direction (remote/decoded vs.
  local/pre-encode) is a separate file by construction, so a future
  transcription pipeline gets 2-speaker diarization for free, no
  speaker-separation model needed on the consuming side.
- Implemented as a baresip `aufilt` (`core/modules/ctrl_json/audiotap.c`
  — adapted from baresip's own `modules/sndfile/sndfile.c` reference
  module for the encode/decode-frame plumbing, hand-rolling its own
  minimal WAV writer, `core/modules/ctrl_json/wav_writer.c`, instead of
  a new external dependency like libsndfile — see both files' own
  top-of-file comments for the full design reasoning, including why the
  filter attaches to every call unconditionally but a separate registry
  decides per-frame whether anything actually gets written) — both new
  files compiled into the *existing* `ctrl_json` module target (see
  `core/modules/ctrl_json/CMakeLists.txt`), not a new sibling
  `APP_MODULE` — no CI/build-config wiring beyond that one file's `SRCS`
  line, since `ctrl_json` was already built everywhere this engine is.
- A tap always targets a call that already exists when `tap_start` runs
  (`call_id` optional, falls back to "the current call" — same
  `resolve_call()` convention, and the same "no call ⇒ `error`, never an
  implicit arm-for-later" behavior, as every other call-scoped command
  in this file) — see `core/E2E-F1.md` "F4 audio tap" for why its e2e
  sequence dials, waits for the same ICE-settle window scenario (a)
  already established, *then* taps, rather than tapping before dialing.
- A tap is auto-finalized on call teardown (`BEVENT_CALL_CLOSED` →
  `audiotap_call_closed()`, see `event_handler()`) even without an
  explicit `tap_stop` — "never leave a corrupt WAV" holds on every path
  a call can end on, not just the happy path. Also force-finalized for
  any call still mid-tap at process shutdown (`ctrl_close()` →
  `audiotap_close()`).
- WAV headers are committed lazily, per direction, on that direction's
  first real audio frame — its actual sample rate, never a
  guessed/pre-negotiated value (see `wav_writer.h`) — same deliberate
  choice `sndfile.c` already made. A tap that is stopped/finalized
  having seen zero real frames in a direction (e.g. the call died
  immediately) still gets a syntactically valid, silent WAV in that
  direction rather than a headerless stub, using a documented fallback
  sample rate (`AUDIOTAP_FALLBACK_SRATE` in `audiotap.c` — this build's
  actual G.711 audio path, 8000 Hz).
- Output is always mono, 16-bit PCM, regardless of the source frame's
  own channel count or sample format (downmixed/converted first if
  needed, via `re`/`rem`'s own `auconv_to_s16()` — already a link-time
  dependency of this engine, not a new one) — this build's actual e2e
  testing only ever exercises the already-mono-S16LE fast path (G.711
  decodes to that natively); the conversion/downmix path is portability/
  correctness code for a future different codec, not something
  `core/E2E-F1.md` "F4 audio tap" itself exercises against the real PBX.
- A tap-side write failure (e.g. disk full) never disrupts the call
  itself: `encode()`/`decode()` always return success to baresip's own
  audio pipeline regardless of the tap's own I/O outcome; a failing
  writer logs exactly one `warning()` (not one per frame) via a sticky
  `wav_writer` error flag, then silently stops attempting further writes
  in that direction for the rest of the call — the WAV file, when
  finalized, ends up correctly headered for whatever it did manage to
  capture.

## Changes from v1.2

v1.3 is additive and fully backward compatible — nothing a v1.2 consumer
already relies on changed shape or behavior. Everything below is new (see
this file's own top-of-file v1.3 status paragraph for the real-PBX e2e
verification status of each, and `core/E2E-F1.md` "F5 presence_override"/
"F5 park" for the underlying evidence):

- **`answer` accepts an optional `call_id`** (see "Commands"/"answer") —
  a queue-aware caller (the receptionist console) can now answer a
  *specific* incoming call rather than only ever "the" incoming call.
  Implemented via baresip's own existing `accept <call-id>` long-command
  parameter (`modules/menu/static_menu.c` `cmd_answer()` already resolves
  it via `uag_call_find()`) — no new call-resolution code needed, just
  building that string when `call_id` is present. e2e-verified.
- **`park`** (see "Commands"/"Events") — parks a call by blind-transferring
  it to a parking lot's pilot extension. Implemented via the same
  `call_transfer()` mechanism `blind_transfer` already uses; the target
  URI is built the same way `blf_subscribe`'s already was (see
  `build_pbx_ext_uri()` in `ctrl_json.c`, a new shared helper factored out
  of what was previously `blf_subscribe`'s own inline URI-building code —
  `blf_subscribe`'s own behavior is unchanged, this is a pure refactor on
  that side). Real e2e surfaced an unresolved issue specific to this
  target (see top-of-file status and `core/E2E-F1.md` "F5 park") — not
  yet confirmed end-to-end.
- **`blf` gains two new `state` values, `held` and `dnd`**
  ("presence_override" — see "Events"/"blf" and
  `core/modules/ctrl_json/dialog_info.h`'s own header comment for the full
  parsing rules). `held` follows the RFC 4235/RFC 3840 standard hold
  indication; real e2e proved this engine's actual test PBX doesn't emit
  that signal for a locally-held call (a real, useful finding — documented
  as a regression-guard unit test, `test_dialog_info_real_capture_
  ext1000_confirmed_no_hold_signal()`, not treated as a bug in this parser).
  `dnd` is a best-effort, non-standard hook, not verified against a real
  Asterisk capture.
- **Security fix: `call_id` is sanitized before ever reaching a
  filesystem path** (`core/modules/ctrl_json/pathsafe.c`/`.h`, new files).
  Found during this version's own 4R risk review: `call_id(call)` — the
  value `tap_start` (v1.2) interpolates directly into
  `<call_id>-rx.wav`/`-tx.wav` — is baresip's own `struct call::id`, which
  for an *incoming* call is set verbatim from the SIP `Call-ID` header the
  *far end* sent (`src/call.c` `sipsess_accept_handler()` →
  `sip_dialog_callid()`), not an engine-generated value. RFC 3261's own
  `word` token grammar (`callid = word ["@" word]`) legally permits `/` in
  a Call-ID, so an unsanitized one is a real path-traversal vector, not a
  theoretical one — a crafted Call-ID could write a WAV file outside the
  caller-supplied `tap_start` `dir`. `pathsafe_component()` (whitelist-only:
  `[A-Za-z0-9._@-]`, everything else including `/`/`\` replaced with `_`,
  leading `.` runs also neutralized) is now applied to `call_id` in
  `audiotap.c`'s `audiotap_start()` before it reaches `path_build()` — the
  *only* place in this codebase that interpolates a call_id into a
  filesystem path. Nothing else changes: `call_id` on JSON events
  (`call_state`, `tap_state`, `park`, ...) is still the raw, unsanitized
  value from `call_id(call)` — a JSON string handles arbitrary bytes
  safely (no filesystem interpretation), so there was nothing to fix
  there; only the filesystem-path use site needed it. 16 new unit tests
  (`test_pathsafe_component()`, `core/modules/ctrl_json/test/test_main.c`)
  cover the charset whitelist, `../` / bare `..`/`.` neutralization,
  truncation, and NULL/zero-size edge cases; e2e-confirmed (dual-contact
  1000, real UUID-shaped call_id) that a normal call_id's filename is
  unaffected — see `core/E2E-F1.md` "F5 pathsafe regression".

## Changes from v1.3

v1.4 is additive and fully backward compatible — nothing a v1.3 consumer
already relies on changed shape or behavior (the one existing event this
version touches, plain `error`, still fires exactly as before for every
case it already covered — see both bullets below for what's *added*
alongside it). Everything below is new — see this file's own top-of-file
v1.4 status paragraph for the full "what was actually broken vs. what
already worked" writeup and the real e2e evidence behind both:

- **`audio_error`** (see "Commands"[none - event-only]/"Events") — a
  distinct, `call_id`-bearing event for `BEVENT_AUDIO_ERROR`
  (`ctrl_json.c`'s `emit_audio_error()`, called from `event_handler()`'s
  existing `BEVENT_AUDIO_ERROR` case alongside the unchanged `emit_error()`
  call already there). No baresip/re patch was needed for this half — by
  reading `src/call.c`'s `audio_error_handler()`, the bevent itself was
  already correctly emitted with the failing call still valid at that
  point; the gap was purely that this module's own JSON shape for it
  (the generic `error` event) carried no `call_id`, so a consumer with
  more than one call in play had no reliable way to tell which call's
  audio device just failed. Separately, and *not* fixed by this change:
  only `ausrc` (microphone/input) failures are wired to
  `BEVENT_AUDIO_ERROR` in baresip at all — see this file's own v1.4
  status paragraph for the `auplay` (speaker/output) gap, a real
  baresip-core API limitation, not something `ctrl_json.c` can route
  around.
- **`park`/`blind_transfer` subscription-establishment failures now
  surface** (`core/patches/0005-baresip-transfer-subscription-error-event.patch`,
  `src/call.c`'s `sipsub_close_handler()`) — this handler's `if (err)`
  branch (fired when the REFER-progress SUBSCRIBE itself fails to be
  established at all, e.g. the wss-specific `EDESTADDRREQ` finding under
  `park` above, *before* any SIP response with a status code is ever
  received) used to only `info()`-log the failure, with no
  `call_event_handler()`/bevent call at all — unlike the very next
  branch in the same function (`msg && msg->scode >= 300`, a *rejected*
  transfer), which already reported via `CALL_EVENT_TRANSFER_FAILED`.
  The fix mirrors that sibling branch exactly: reuses the existing
  `CALL_EVENT_TRANSFER_FAILED` → `BEVENT_CALL_TRANSFER_FAILED` path
  (`ua.c`, unchanged) → `ctrl_json.c`'s pre-existing
  `BEVENT_CALL_TRANSFER_FAILED` case (unchanged — reuses the plain
  `error` event shape, exactly like it already did for the rejection
  case). No new protocol event type; a caller that already watches for
  `error` events with `"transfer failed: "` in `message` now also
  catches this failure mode, where before it saw nothing at all for it.
  Real e2e evidence both directions (this session, real test PBX, `-s`
  SIP trace, dual-contact 1000 into pilot ext `700`): over **wss**, the
  `EDESTADDRREQ` failure reproduces and now correctly emits
  `{"event":"error","message":"transfer failed: Destination address
  required [39]"}`, with the original call confirmed still up/untouched
  afterward; over **udp**, no failure occurs at all — the REFER reaches
  the wire, gets a `202 Accepted`, and a final NOTIFY sipfrag body of
  `SIP/2.0 200 OK` confirms the call actually lands in the parking lot —
  narrowing "`park` doesn't work" (the v1.3 finding) to "`park` doesn't
  work specifically when the engine's own SIP signaling transport is
  wss" (see "Planned" below for the still-open wss-specific root cause).

## Changes from v1.4

v1.5 is additive and fully backward compatible — nothing a v1.4 consumer
already relies on changed shape or behavior. One new command:

- **`set_answer_mode`** (see "Commands" above) — `cmd.c`'s
  `cent_cmd_decode()` gets a new branch (required `mode`, restricted to
  `"auto"`/`"manual"`, same "reject anything else" shape as
  `set_device`'s `kind`), `struct cent_cmd` gets one new field
  (`answer_auto`, a `bool` rather than baresip's own `enum answermode` —
  kept `cmd.h` dependency-free, same reasoning as every other
  baresip-shaped-but-not-baresip-typed field in that struct), and
  `ctrl_json.c` gets `cmd_set_answer_mode()`: resolves the engine's one
  UA (`primary_ua()`) and its account (`ua_account(ua)`, erroring if
  either is missing — same two-step shape `blf_subscribe`/
  `attended_transfer` already use), then calls baresip's own
  `account_set_answermode(acc, mode)` directly — no new engine-side
  state, no new event type. Why that one call is sufficient (no
  additional flag/logic needed here): this build already loads the
  `menu` module (see `core/BUILD.md` "Module selection" — it's what
  `answer`/`dial`/`quit` dispatch through via `cmd_process_long()`), and
  `modules/menu/menu.c` already reads `account_answermode(acc)` on every
  `BEVENT_CALL_INCOMING`, auto-answering when it's `ANSWERMODE_AUTO` —
  that path has been live and unused (every account starts at the
  `ANSWERMODE_MANUAL` default) since v1; `set_answer_mode` is only
  wiring a way to flip it at runtime. Unit-tested end to end through the
  real `cent_cmd_decode()` pipeline (`test/test_main.c`
  `test_cmd_set_answer_mode()` — valid `auto`/`manual`, case-
  insensitivity, missing/invalid `mode` → `CENT_CMD_NONE`, and that no
  `call_id` is ever decoded for it), full suite (279 checks) passing
  under ASan (`-fsanitize=address`, `CENT_ASAN=ON`) — see
  `core/modules/ctrl_json/test/CMakeLists.txt`. Compiles clean (no new
  warnings) as part of a full `core/deps/baresip` engine build with the
  `ctrl_json` app module, this session, from a clean submodule checkout
  (`core/BUILD.md` steps 2-4, patches 0001-0005 applied). That original
  session verified the decode/dispatch/build path only, not a real
  incoming call actually auto-answering — **now e2e-verified against
  the real test PBX (2026-07-21)**, see `core/E2E-F1.md` "F7
  set_answer_mode" and this file's own top-of-file v1.5 status
  paragraph.

## Planned (still not in v1.5)

- `devices`'s device-name granularity is exactly baresip's own module
  set for this spike build (`ausine`/`aufile`, see `core/BUILD.md`
  "Module selection") — real per-device enumeration (actual microphone/
  speaker names) needs a real device-backend module (`coreaudio`,
  `alsa`, `wasapi`, ...) added to the build; `devices_add_driver()` in
  `ctrl_json.c` already walks each driver's real `dev_list` first and
  only falls back to "the driver itself" when that's empty, so this is
  a build-config change, not a protocol change, when it happens.
- Multi-account support. This engine still registers exactly one UA;
  `call_id`-based multi-*call* resolution (this version's work) and
  multi-*account* support are different problems — `primary_ua()` in
  `ctrl_json.c` (`uag_find_aor(NULL)`, "the first/only UA") would need
  to become a real per-account selector.
- Inbound DTMF-received events (`BEVENT_CALL_DTMF_START`/`_END` exist in
  baresip; only the *send*-DTMF command is wired up this version, since
  it's what the F1 spec asked for and nothing in this version's testing
  needed the receive side).
- `BEVENT_CALL_TRANSFER`/`BEVENT_CALL_REDIRECT` (the transfer-*target*-
  side perspective — receiving a REFER and acting on it) — not mapped;
  this engine only ever initiates transfers in every flow this repo
  exercises, never receives one.
- A mid-dialog "your call was replaced" event for the *far* end of an
  attended transfer's original call (party B in `core/E2E-F1.md`'s
  scenario) — confirmed working PBX-side (the channel visibly moves —
  see `core/E2E-F1.md`), but there's currently no *protocol* event on
  that call's own connection marking the moment its dialog got
  Replaces'd versus a normal established call continuing; only the
  transferor's own `complete_transfer` outcome is currently observable
  over the wire.
- Multi-pending attended transfers (`complete_transfer`'s `call_id`
  field is accepted but unused — see "Commands").
- CENT_TLS_PIN is one flat env var (single pin, checked for every
  secure connection this engine's http_client makes) — not host-keyed,
  and not a list (v1 Electron app's own `pinnedCertSha256` supports
  pin rotation via an array). Fine for this engine's actual one-PBX-host
  usage; a real multi-account/multi-host version would need more. See
  `core/patches/0002-re-tls-fingerprint-pin.patch`'s own comment.
- **Tap consumption is out of scope for this version** — `tap_start`/
  `tap_stop` only produce the two WAV files; nothing in this repo reads
  them back yet. The intended next consumer is
  `premium/crates/centinelo-transcribe` (whisper.cpp, per the workspace
  spec's F4 phase) — it should be able to treat `-rx.wav`/`-tx.wav` as
  two independent, already-speaker-separated mono streams and never need
  its own diarization step; both files use a canonical, spec-plain PCM
  WAV (see `wav_writer.c` — no `LIST`/`fact`/other optional chunks, no
  RF64/extensible-format header), so any standard WAV reader (Python's
  stdlib `wave` module, as used by `core/E2E-F1.md` "F4 audio tap"'s own
  verification, included) should read them without special-casing.
- The downmix-to-mono / non-S16LE-source conversion path in
  `audiotap.c`'s `write_frame()` (see "Changes from v1.1") is not
  exercised by this repo's e2e testing — this build's actual codec set
  (G.711) only ever produces already-mono-S16LE frames. Worth a
  synthetic/unit-level check (a fake multi-channel `struct auframe`) if
  a future build adds a stereo device or non-PCM-native codec ahead of
  it in the pipeline.
- No maximum tap duration/size enforcement — a very long tap on a very
  long call will eventually hit the ~4 GiB single-file ceiling any
  canonical (non-RF64) WAV file has (~37h continuous at this build's
  8kHz mono — see `wav_writer.c`'s own note on `data_bytes` wrapping).
  Not a concern for any call length this repo's e2e testing (or a real
  dental-office phone call) produces; would need an RF64/W64 header or a
  rollover-to-a-new-file policy if that ever changed.
- **`park` over wss specifically.** As of v1.4, `park` is confirmed
  end-to-end (real PBX, real SIP trace: REFER on the wire, `202
  Accepted`, final NOTIFY sipfrag `SIP/2.0 200 OK`) when the engine's
  own SIP signaling transport is **udp** (or, by the same mechanism,
  presumably `tcp`/`tls` — not separately re-verified this session, only
  `udp` and `wss` were). Over **wss** specifically, a client-side
  (baresip/`re`, not PBX-side) error in the REFER-progress-subscription
  tracking still reproduces every time (`call: subscription closed:
  Destination address required`, `errno` 39/`EDESTADDRREQ`) — this is
  now a *narrower*, more precise open question than v1.3's framing
  ("does `park` work at all against `Park()`") — see `core/E2E-F1.md`
  "F5 park" for the original v1.3 repro and this file's own v1.4 status
  paragraph for the transport-dependence finding. As of v1.4 the failure
  at least surfaces as a normal `error` event (`core/patches/0005-*`)
  instead of vanishing silently — see "Changes from v1.3" — but the
  underlying wss-specific cause is still unresolved. Next step: bisect
  whether this is `sipevent_drefer()`'s dialog-reuse path interacting
  differently with a WS-transport dialog specifically (needs a live WS
  connection/socket handle for address resolution, unlike a plain UDP
  socket's trivially-reusable remote address) versus a normal
  `Background()`/echo-app blind-transfer target over the same wss
  transport (which works fine, see "(b) blind_transfer" in
  `core/E2E-F1.md` — also wss); a `re`-level (not just `ctrl_json`-level)
  trace of `re/src/sipevent/subscribe.c`'s `sipsub_alloc()` specifically
  comparing its udp-transport and wss-transport code paths would be the
  most direct way to pin this down, not attempted this session (deep,
  `re`-internal, genuinely out of this task's bounded-patch scope — see
  this version's own scope decision in the v1.4 status paragraph).
- **`park`'s actual assigned parking slot is not observable over plain
  SIP** — see "Events" `park` row. Would need an AMI/ARI integration (a
  different, out-of-scope layer for this SIP-only engine) to report the
  real slot number rather than just the pilot extension the request
  targeted.
- **`dnd` (`blf` `state`) is unverified against a real PBX** — see
  "Events" `blf` and `dialog_info.h`'s `CENT_BLF_DND` comment. Testing it
  would need either toggling DND on a test extension via a feature code
  outside this repo's current pre-authorized safe-target list, or a
  different PBX/vendor that actually emits *some* distinguishing
  Event:dialog signal for it (standard Asterisk chan_pjsip, per this
  version's investigation, may not).
- **`held` (`blf` `state`) never fires against this engine's actual test
  PBX** — see "Events" `blf` and the v1.3 status paragraph at the top of
  this file. The parser rule is correct and unit-tested against the
  RFC-documented shape; the gap is entirely PBX-side (this Asterisk
  build doesn't emit the signal), not something a future protocol change
  here can fix without a different signal source.

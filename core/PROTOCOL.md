# core/ — ctrl_json wire protocol (v1)

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

**v1 status: every command below is implemented and e2e-verified against
the real test PBX** (FreePBX 17 / Asterisk 22 at `100.119.230.80`) — see
`core/E2E-F1.md` for the evidence. v0 (dial/answer/hangup/quit only,
reg_state/call_state/error events, no call_id, no BLF/transfer/DTMF/
hold/mute/stats) is superseded; see "Changes from v0" below for exactly
what moved and why, if you're a consumer that was coded against v0.

## Framing

One JSON object per line (`\n`-terminated; a trailing `\r` is tolerated).
No netstring/length-prefix framing — plain newline-delimited JSON
(NDJSON).

**stdout is not *pure* NDJSON.** Two things baresip itself prints land on
stdout before/alongside `ctrl_json`'s own lines:

1. Exactly one plain-text banner line (`baresip vX.Y.Z Copyright ...`),
   printed directly in `main()` before any module — including
   `ctrl_json` — loads. `log_enable_stdout(false)` (which `ctrl_json`
   calls on init) can't retroactively suppress this.
2. In practice, several more human-readable lines from modules that load
   *before* `ctrl_json` in the config's module order (network interface
   enumeration, each module's own `info("aucodec: ...")`-style
   registration line, `Populated 1 account`, ...) — `ctrl_json` is
   deliberately listed last in `run-spike.sh`'s generated config
   (`module_app` line last) specifically so it claims
   `log_enable_stdout(false)` as early as anything under this repo's
   control can, but earlier-loading stock modules still log to stdout
   before that point.
3. **New in v1, and worth calling out explicitly:** if the engine is run
   with `-s` (SIP trace, `CENT_BARESIP_ARGS="-s"`), the raw SIP messages
   also print to stdout — `uag_enable_sip_trace()`'s handler
   (`src/uag.c`) calls `re_printf()` directly, which is stdout, not
   `debug()`/`info()` (those go to stderr, unaffected). Confirmed while
   building this protocol version (needed the trace to debug a transfer
   failure — see `core/E2E-F1.md`). `-s` is a debugging aid, not
   something a normal consumer would set, but if you do, the "filter for
   `{`-prefixed lines" rule below still applies and still works (SIP
   trace lines never start with `{`).

**A consumer must treat stdout as: filter for lines whose first
non-whitespace character is `{`, and JSON-decode each of those
individually.** Everything else on stdout is human-readable log/trace
noise to ignore. `ctrl_json.c`'s own `emit()` writes each JSON line with
`fwrite()` + an explicit `fflush(stdout)` (not a raw `write()` — see
"Changes from v0" — same immediate-delivery guarantee, just POSIX-free).

stderr carries baresip's own human-readable debug/info/warning log
unaffected by any of the above, plus `run-spike.sh`'s own startup summary
lines. `-v`/`-s` (verbose / SIP trace, see `run-spike.sh`'s
`CENT_BARESIP_ARGS`) add detail there (except SIP trace bodies
themselves, per point 3 above).

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

| JSON | Effect |
|---|---|
| `{"cmd":"dial","uri":"sip:*43@host"}` | Dial `uri`. Unchanged from v0: `cmd_process_long(commands, "dial <uri>", ...)`, reusing the `menu` module's dial/UA-selection logic. |
| `{"cmd":"answer"}` | Answer the current incoming call. Unchanged from v0: maps to baresip's long command `accept`. |
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
| `{"cmd":"quality_stats","call_id":"..."}` | Emit a `stats` event (see "Events") for the call's current RTCP-derived counters. |
| `{"cmd":"blf_subscribe","ext":"510"}` | SIP SUBSCRIBE `Event: dialog` (RFC 4235) to `sip:<ext>@<same PBX host the account registered against>`, `Accept: application/dialog-info+xml`, refreshed automatically by `re`'s sipevent layer for as long as the subscription lives (no polling here). Emits `blf` events (see "Events") as NOTIFYs arrive, starting with the initial one. Errors if already subscribed to that `ext`. |
| `{"cmd":"blf_unsubscribe","ext":"510"}` | Cleanly unsubscribes (`Expires: 0`) and stops tracking `ext`. Errors if not currently subscribed. |

Unknown `cmd` values, a required field missing/wrong-typed (e.g. `dial`
without `uri`, `mute` without a real boolean `on`), or a baresip call
that returns an error, all produce an `error` event rather than crashing
or hanging; the connection stays up. The JSON-decoding + field-validation
half of this (everything except actually calling into baresip) is pure,
unit-tested code — see `core/modules/ctrl_json/cmd.c` and
`test/test_main.c`.

There is still no per-command request/response envelope (no `token`-style
echo like `ctrl_tcp` has) — success is observable via the resulting
event(s) documented per-command above, failure via `error`. Still listed
under "Planned" as a natural v2 addition.

## Events (stdout)

| JSON | When |
|---|---|
| `{"event":"ready"}` | Once, right after `ctrl_json` finishes initializing — the earliest point commands can safely be sent. Unchanged from v0. |
| `{"event":"reg_state","account":"...","state":"registering\|registered\|failed\|unregistered","transport":"udp\|tcp\|tls\|ws\|wss","reason":"..."}` | On every registration transition — now including transitions caused by the runtime `register`/`unregister` commands, not just process-start registration. `reason` present only on `failed`. Unchanged shape from v0. |
| `{"event":"call_state","state":"...","peer":"...","id":"...","call_id":"...","}` | **`call_id` is new in v1** (added alongside the original `id` field, same value — kept both so a v0 consumer reading `id` doesn't break; a future v2 may drop `id`). `state` values beyond v0's `incoming\|ringing\|established\|closed`: **`hold`/`resumed`** (fired both for this engine's own local hold/resume commands — synthetically, right at the command's own success path, since baresip has no bevent for *locally*-initiated hold/resume, only peer-initiated — and relayed from `BEVENT_CALL_HOLD`/`BEVENT_CALL_RESUME` for a *peer*-initiated hold/resume) and **`muted`/`unmuted`** (from `mute`). None of these correspond to baresip's own `CALL_STATE_*` lifecycle machine changing — hold/mute are attributes of an otherwise-established call, not lifecycle transitions — they're folded into `call_state` anyway rather than inventing a new event per attribute, since from a consumer's perspective they're all "something about this call just changed, here's its id". |
| `{"event":"error","message":"..."}` | Malformed/unparseable input line, unknown `cmd`, a required field missing/wrong-typed, a baresip command that returned an error, `BEVENT_AUDIO_ERROR`, or (new in v1) `BEVENT_CALL_TRANSFER_FAILED` (an async transfer failure reported by the far end after `blind_transfer`/`complete_transfer` already returned success synchronously — reuses this existing event/shape rather than inventing a transfer-specific one). |
| `{"event":"stats","call_id":"...","rtt_us":N,"tx_packets":N,"tx_lost":N,"tx_jitter_us":N,"rx_packets":N,"rx_lost":N,"rx_jitter_us":N}` | New in v1, from `quality_stats`. Sourced from `stream_rtcp_stats()` (`src/stream.c`) — **this reflects the most recently *received* RTCP Sender/Receiver Report, not a live per-packet counter.** Querying more often than the RTCP interval (empirically ~10-20s against the test PBX, see `core/E2E-F1.md`) returns identical numbers between reports; that's correct RTCP behavior, not a bug or a stale/broken reading. Query again after waiting a few RTCP intervals if you need fresher numbers. |
| `{"event":"blf","ext":"...","state":"idle\|ringing\|busy\|offline"}` | New in v1, from `blf_subscribe`. `idle`: no active dialog for that extension (either no `<dialog>` element in the NOTIFY body, *or* one present with `<state>terminated</state>` — both occur in practice, see `core/E2E-F1.md` for the real captured body, which uses the second shape). `ringing`: `<state>` is `early`/`proceeding`/`trying`. `busy`: `<state>confirmed</state>`. `offline`: the subscription itself failed/was rejected/expired before a NOTIFY could be parsed, *or* a `<dialog>` element was present with no parseable `<state>` — the "can't currently tell" bucket. Parsing is pure, tiny, and unit-tested against both synthetic bodies and the real capture — see `core/modules/ctrl_json/dialog_info.c` and `test/test_main.c`. |
| `{"event":"attended_transfer_started","source_call_id":"...","target_call_id":"..."}` | New in v1, from `attended_transfer`, right after the consultation call's dial succeeds. Lets a consumer correlate exactly which two `call_id`s a pending `complete_transfer`/`abort_transfer` will act on — there's no other way to learn `target_call_id` (it's a brand new call, not something the caller supplied). |

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

## Planned (still not in v1)

- Per-command request/response correlation (a `token` field echoed
  back, matching stock `ctrl_tcp`'s convention).
- A clean, exclusively-JSON stdout stream — real but explicitly deferred;
  see "Framing" above for the current filter-for-`{` workaround and why
  it's not fully closed off yet (the SIP-trace-on-stdout finding from
  this version adds one more source of non-JSON noise to that same
  category, not a new problem).
- `devices` — enumerate/select audio devices. Still hardcoded (`ausine`
  source / `aufile` player) at config-generation time in `run-spike.sh`.
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

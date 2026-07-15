# core/ — ctrl_json wire protocol

`ctrl_json` (`core/modules/ctrl_json/`) is a baresip "application" module
that turns the running engine into a sidecar controllable over stdio:
newline-delimited JSON **commands** in on stdin, newline-delimited JSON
**events** out on stdout. It is the F1 tracer-bullet version of the
protocol a future Tauri shell (or this spike's own test harness) speaks to
drive the engine — modeled on baresip's own `ctrl_tcp` module (JSON over
TCP+netstring) and `stdio` module (keyboard polling via `fd_listen`), with
the transport swapped for a plain stdio pipe and the JSON shape narrowed
to what F1 actually needs.

## Framing

One JSON object per line (`\n`-terminated; a trailing `\r` is tolerated).
No netstring/length-prefix framing — plain newline-delimited JSON
(NDJSON).

**stdout is not *pure* NDJSON in v0.** Two things baresip itself prints
land on stdout before/alongside `ctrl_json`'s own lines:

1. Exactly one plain-text banner line (`baresip vX.Y.Z Copyright ...`),
   printed directly in `main()` before any module — including
   `ctrl_json` — loads. `log_enable_stdout(false)` (which `ctrl_json`
   calls on init, see BUILD.md) can't retroactively suppress this.
2. In practice, several more human-readable lines from modules that load
   *before* `ctrl_json` in the config's module order (network interface
   enumeration, each module's own `info("aucodec: ...")`-style
   registration line, `Populated 1 account`, ...) — `ctrl_json` is
   deliberately listed last in `run-spike.sh`'s generated config
   (`module_app` line last) specifically so it claims
   `log_enable_stdout(false)` as early as anything under this repo's
   control can, but earlier-loading stock modules still log to stdout
   before that point. This was worse in practice than the single banner
   line originally assumed — see the real captured output in BUILD.md's
   "Findings" testing narrative.

**A consumer must treat stdout as: filter for lines whose first
non-whitespace character is `{`, and JSON-decode each of those
individually.** Everything else on stdout is human-readable log noise to
ignore (baresip's own diagnostic log, distinct from `ctrl_json`'s JSON
events). This spike's own evidence-gathering does exactly that (`grep
'^{'`). Cleaning this up properly (e.g. redirecting baresip's own log to
a file instead of stdout, or getting `log_enable_stdout(false)` applied
before any other module loads) is flagged as an F1-continuation
follow-up, not done here.

stderr carries baresip's own human-readable debug/info/warning log
unaffected by any of the above, plus `run-spike.sh`'s own startup summary
lines. `-v`/`-s` (verbose / SIP trace, see `run-spike.sh`'s
`CENT_BARESIP_ARGS`) add detail there, never to stdout.

## v0 commands (stdin)

| JSON | Effect |
|---|---|
| `{"cmd":"dial","uri":"sip:*43@host"}` | Dial `uri`. Internally: `cmd_process_long(commands, "dial <uri>", ...)` — the same long-command dispatch baresip's own `ctrl_tcp` uses, so this reuses the `menu` module's existing dial/UA-selection logic instead of reimplementing it. |
| `{"cmd":"answer"}` | Answer the current incoming call. Maps to baresip's long command `accept` (menu module's naming, not `answer` — `ctrl_json` does the translation). |
| `{"cmd":"hangup"}` | Hang up the current call. Maps to baresip long command `hangup`. |
| `{"cmd":"quit"}` | Clean shutdown. Maps to baresip's core long command `quit` (`cmd_quit`, `src/baresip.c` — not menu-dependent). Also triggered automatically if stdin is closed/EOF'd (the sidecar's parent process going away is treated the same as an explicit quit). |

Unknown `cmd` values, a `dial` missing its `uri`, or a baresip command
error (e.g. `hangup` with no active call) all produce an `error` event
(below) rather than crashing or hanging; the connection stays up.

There is no per-command acknowledgement/response object in v0 (no
`token`-style echo like `ctrl_tcp` has) — success is observable via the
resulting `reg_state`/`call_state` events, failure via `error`. A
request/response envelope (so a caller can correlate a specific command
with its outcome, matching `ctrl_tcp`'s `token` field) is a natural v1
addition, listed under "Planned" below.

## v0 events (stdout)

| JSON | When |
|---|---|
| `{"event":"ready"}` | Once, right after `ctrl_json` finishes initializing (stdin listener + event subscription both up) — the earliest point commands can safely be sent. |
| `{"event":"reg_state","account":"sip:1100@host:port","state":"registering\|registered\|failed\|unregistered","transport":"udp\|tcp\|tls\|ws\|wss","reason":"..."}` | On every `bevent_ev` registration transition (`BEVENT_REGISTERING`/`BEVENT_REGISTER_OK`/`BEVENT_REGISTER_FAIL`/`BEVENT_UNREGISTERING`). `transport` is read back from the account's own registrar URI (`account_luri()` + `uri_param_get(..., "transport", ...)`), so it reflects what was actually configured/attempted, not just an echo of a request field. `reason` is present only on `failed`, taken from `bevent_get_text()` (e.g. the transport-level error, an auth failure text, ...) — this is the field this spike used to capture *why* a transport attempt didn't register (task requirement 6b). `state` values beyond the task's literal `registered\|failed` example (`registering`, `unregistered`) are a deliberate v0 addition — the task's own wording ("...") left room for them, and both are real, useful, observed transitions. |
| `{"event":"call_state","state":"incoming\|ringing\|established\|closed","peer":"sip:...","id":"..."}` | On the corresponding `bevent_ev` (`CALL_INCOMING`; `CALL_RINGING` **and** `CALL_PROGRESS`, i.e. 180 and 183, both collapse to `"ringing"`; `CALL_ESTABLISHED`; `CALL_CLOSED`). `id` (`call_id()`) is a v0 addition beyond the task's literal schema, to let a caller correlate events belonging to the same call when a v1 protocol adds multi-call support. Other call-level `bevent_ev`s baresip emits (`CALL_OUTGOING`, `CALL_ANSWERED`, `CALL_HOLD`/`CALL_RESUME`, DTMF, transfer, RTCP, ...) are **not** mapped to a `call_state` event in v0 — see "Planned" below. |
| `{"event":"error","message":"..."}` | Malformed/unparseable input line, unknown `cmd`, a `dial` missing `uri`, a baresip command that returned an error, or `BEVENT_AUDIO_ERROR`. |

## Planned (not in v0)

Full command set this protocol is meant to grow into, per the F1 spec.
None of these are implemented by `core/modules/ctrl_json/ctrl_json.c` yet
— listed here so F1-continuation work has an agreed shape to build
against rather than inventing one per-command:

- `register` / `unregister` — v0 always registers on process start from
  the generated accounts file; no runtime re-registration or
  multi-account control yet.
- `hold` / (implicit) resume — baresip's re-INVITE hold exists at the
  core (`BEVENT_CALL_HOLD`/`BEVENT_CALL_RESUME`, and the v1 Electron app's
  README lists hold as a ported feature) but `ctrl_json` doesn't expose a
  command or emit an event for it yet.
- `blind_transfer` / `attended_transfer` — same story: baresip/menu has
  the underlying support (`BEVENT_CALL_TRANSFER`/`_REDIRECT`/`_FAILED`),
  not wired into this protocol.
- `dtmf` — `BEVENT_CALL_DTMF_START`/`_END` exist in baresip; no send-DTMF
  command or event mapping here yet.
- `mute` — no command; `ausine`'s always-on tone source has no concept of
  mute in this spike anyway (a real mic source would need this).
- `devices` — enumerate/select audio devices. v0 hardcodes `ausine`
  (source) / `aufile` (player) at config-generation time in
  `run-spike.sh`; no runtime device listing/switching.
- `blf_subscribe` — busy-lamp-field presence, a named feature of the v1
  Electron app (`BLF busy lamps`, `Event: dialog` SUBSCRIBE). Not touched
  in this engine spike.
- `quality_stats` — call/RTP quality numbers over the protocol itself.
  This spike instead verified real RTP the "outside" way for F1's e2e
  requirement (SSH + `asterisk -rx "pjsip show channelstats"`, see
  `BUILD.md`); baresip does have internal stats (`rtpstat.c`) that a v1
  protocol could expose as a command/event, not done here.
- Per-command request/response correlation (a `token` field echoed back,
  matching stock `ctrl_tcp`'s convention) — see "v0 commands" above.
- A clean, exclusively-JSON stdout stream (see "Framing" above) — real,
  but explicitly deferred rather than half-fixed in this pass.

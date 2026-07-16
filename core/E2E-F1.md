# core/ — F1 end-to-end verification (+ F3 regression, v1.1)

Real evidence, gathered by actually running the built engine against the
live test PBX (FreePBX 17 / Asterisk 22, `100.119.230.80`, Tailscale-only,
ext 1100 / secret from `~/Library/Application Support/Centinelo
Phone/settings.json`, never printed/committed). PBX-side verification was
SSH + read-only `asterisk -rx "... show ..."` commands only — no PBX
configuration was changed at any point in this work. Scenarios (a)-(d) +
"Additional verification"/"Memory safety" are the original F1 (v1)
evidence; "F3 regression" further down is the v1.1 protocol-hardening
follow-up (`core/PROTOCOL.md` "Changes from v1") against this same PBX.

Methodology: a small Python harness (not checked in — scratch tooling)
spoke the NDJSON protocol over `run-spike.sh`'s stdin/stdout, with a
background thread parsing `{`-prefixed lines as events (per
`PROTOCOL.md` "Framing") and a `wait_for(predicate, timeout)` primitive
so each check either observes the exact expected event or times out
loudly — no sleep-and-hope. PBX-side snapshots were taken immediately
before/after each protocol action for independent confirmation.

## (a) register → dial *43 → hold → resume → dtmf → hangup

**PASS.**

1. `register` (startup) → `{"event":"reg_state",...,"state":"registered","transport":"wss"}`.
2. `{"cmd":"dial","uri":"sip:*43@100.119.230.80"}` → `call_state`
   `established`.
3. **Finding — ICE settle time**: immediately after `established`,
   baresip's own periodic bitrate ticker (stderr) read `audio=0/0
   (bit/s)` for several seconds, and `pjsip show channelstats`
   PBX-side showed `Count 0` both directions. This is *not* a bug —
   the account has `medianat=ice;mediaenc=dtls_srtp` (required by the
   endpoint's `webrtc=yes`, see `BUILD.md` "Findings"), and this
   engine offers host ICE candidates across every local interface (LAN,
   Tailscale, IPv6 — six candidates in the captured SDP: `192.168.100.224`,
   two IPv6 ULAs, `192.168.100.225`, `100.93.223.113` (Tailscale), one
   more IPv6). ICE connectivity checks across that many candidate pairs
   take real time to settle. Waiting ~15-20s after `established` before
   relying on live RTP resolved it consistently across every run in this
   document. Confirmed via SIP trace (`-s`) that SIP-level `established`
   genuinely precedes working media by design (INVITE/200/ACK doesn't
   wait on ICE) — not a defect in `ctrl_json`.
4. After the ~18s settle: `quality_stats` → real, growing counters (see
   scenario (d) below for the exact numbers) and PBX `pjsip show
   channelstats` agreeing (both sides growing, `Lost 0 Pct 0`).
5. `{"cmd":"hold","call_id":...}` → `call_state` `hold`. PBX
   `channelstats` sampled 3s into the hold window: **+2 packets** over
   that window (essentially flat), vs. a steady-state rate of roughly
   45-50 packets/5s measured moments earlier — media visibly paused.
6. `{"cmd":"resume","call_id":...}` → `call_state` `resumed`. PBX
   `channelstats` sampled ~4-9s after resume: **+226 packets** — media
   visibly and immediately resumed at full rate.
7. `{"cmd":"dtmf","digits":"1234","call_id":...}` → no error event (2s
   drain, clean).
8. `{"cmd":"hangup","call_id":...}` → `call_state` `closed`. PBX `core
   show channels` back to `0 active channels` afterward.

Representative PBX evidence (one full run, edited for length — see raw
harness logs, not checked in, for the complete transcript):

```
$ asterisk -rx 'pjsip show channelstats'      (t=18s, before hold)
          1100-00000867      00:00:18 ulaw      773       0    0   0.000    774 ...
$ asterisk -rx 'pjsip show channelstats'      (t=22s, 3s into hold)
          1100-00000867      00:00:22 ulaw      775       0    0   0.000    776 ...   <- +2 over 4s
$ asterisk -rx 'pjsip show channelstats'      (after resume)
          1100-00000867      00:00:27 ulaw     1001       0    0   0.000   1002 ...   <- +226 total, media clearly resumed
```

## (b) blind_transfer

**PASS — full positive evidence, after root-causing a false start.**

### First attempt (blocked, root-caused, not a code bug)

`dial *43` → `{"cmd":"blind_transfer","uri":"sip:*97@100.119.230.80",...}`
initially failed: REFER was sent correctly and **accepted by Asterisk
(202 Accepted)**, but the implicit refer-progress subscription's next
NOTIFY carried `Subscription-State: terminated;reason=noresource` with
sipfrag body `SIP/2.0 400 Bad Request`. `ctrl_json`'s
`BEVENT_CALL_TRANSFER_FAILED` relay correctly surfaced this as
`{"event":"error","message":"transfer failed: 400 Bad Request"}` — the
code path is correct, something PBX-side rejected the actual transfer.

Root-caused two ways:

1. Read-only `asterisk -rx "dialplan show *97@from-internal"` +
   `/var/log/asterisk/full` (read-only) showed ext 1100 has no
   voicemail mailbox provisioned (`VMBOXEXISTSSTATUS=FAILED`,
   `VMCONTEXT=novm`) — dialing `*97` directly from 1100 answers then
   immediately hangs itself up. **PBX-config footnote, not an engine
   issue**: 1100 = novm. No PBX config was changed to investigate or
   work around this.
2. That alone didn't explain a *reverse*-direction failure
   (`*97`→`*43`, also `noresource`/400) or a same-target failure with
   `*60` (speaking clock — doesn't touch mailboxes at all) or even a
   self-transfer (`*43`→`*43`). All four additional combinations
   produced the byte-identical `Subscription-State:
   terminated;reason=noresource` / `SIP/2.0 400 Bad Request` sipfrag,
   captured via SIP trace. Conclusion: `*43`/`*97`/`*60`/`*65` are all
   single-party `Background()`/`Answer()`-driven demo/utility apps, not
   genuine 2-party bridges — Asterisk's native blind-transfer
   (`res_pjsip_refer` → bridge redirect) needs an actual bridge to
   redirect, which none of these feature-code apps are in. This is a
   property of the transfer *source* channel, independent of target.

### Working verification (dual-contact self-bridge)

`pjsip show aor 1100` confirmed `max_contacts: 2`. Two separate engine
instances (A, B) registered simultaneously as ext 1100 (two distinct
contacts, confirmed: `sip:1100@100.93.223.113:56994...` and
`...:56995...`). A dialed `sip:1100@100.119.230.80` (its own extension —
the dialplan allowed this and rang the other contact); B received
`call_state incoming` and answered. A and B reached `established` — a
**genuine 2-party bridge**, confirmed by both PBX channels sharing one
`BridgeID` in `core show channels verbose`.

From A: `{"cmd":"blind_transfer","uri":"sip:*43@100.119.230.80",...}` →
A's own call closed cleanly (`call_state closed` — the expected shape
for a *successful* transfer, since `call_replace_transfer`/
`call_transfer` success collapses the transferor's own leg, same as a
normal hangup — see `PROTOCOL.md` and `src/call.c`
`sipsub_notify_handler`'s 2xx-sipfrag branch). PBX evidence, before and
after:

```
$ asterisk -rx 'core show channels verbose'      (before transfer)
PJSIP/1100-00000870   from-internal                       1  Up  AppDial  (Outgoing Line)             BridgeID 9df79407-...
PJSIP/1100-0000086f   dialOne-with-exten   1100            2  Up  Dial     PJSIP/1100/sip:1100@100.9  BridgeID 9df79407-...

$ asterisk -rx 'core show channels verbose'      (after blind_transfer to *43)
PJSIP/1100-00000870   from-internal-xfer   *43             7  Up  BackGround   demo-echotest,,,app-echo-...
```

The surviving channel visibly moved into `from-internal-xfer` context,
extension `*43`, running the echo-test application — exactly the "call
lands in [target]" evidence the task asked for, using an authorized
alternate target (`*43`) once a genuine bridge existed.

## (c) blf_subscribe ext 510

**PASS.**

`{"cmd":"blf_subscribe","ext":"510"}` → SUBSCRIBE `Event: dialog`,
`Accept: application/dialog-info+xml` sent to `sip:510@100.119.230.80`
(digest-authenticated, 401→200 in trace). Initial NOTIFY received and
correctly parsed:

```
$ (raw NOTIFY body, captured via -s SIP trace, ext 510 idle/unregistered)
<?xml version="1.0" encoding="UTF-8"?>
<dialog-info xmlns="urn:ietf:params:xml:ns:dialog-info" version="0" state="full" entity="sip:510@100.119.230.80">
 <dialog id="510">
  <state>terminated</state>
 </dialog>
</dialog-info>
```

→ `{"event":"blf","ext":"510","state":"idle"}`.

**Finding**: the real server sends a *populated* `<dialog>` element with
`state=terminated` for "no active call", not an absent/empty `<dialog>`
element as `dialog_info.c`'s first version assumed before this capture.
Both shapes correctly resolve to `idle` per the parser's rules (a
`terminated` dialog state is explicitly one of them), so this didn't
require a code change — but the real body is now also a permanent
regression-test fixture (`test/test_main.c`
`test_dialog_info_real_capture_ext510_idle()`), not just synthetic
cases.

`{"cmd":"blf_unsubscribe","ext":"510"}` → clean SUBSCRIBE
`Expires: 0` → final `terminated` NOTIFY → subscription torn down, no
further `blf` events. No secrets in any of the captured traffic (SIP
digest nonces/responses are single-use challenge material, not
credentials).

## (d) quality_stats

**PASS**, on the same active `*43` call as scenario (a), after the ICE
settle window described there.

```
{"event":"stats","call_id":"...","rtt_us":0,"tx_packets":1049,"tx_lost":0,
 "tx_jitter_us":1000,"rx_packets":1042,"rx_lost":11,"rx_jitter_us":3375}
```

Non-zero `tx_packets`/`rx_packets`, consistent with the PBX-side
`channelstats` growth in the same window. `rtt_us` reading `0` across
every capture in this document — RTCP round-trip-time calculation
depends on a full SR/RR/DLSR round trip populating; not investigated
further, noted as a real, minor gap (the counters that matter for "is
media flowing and healthy" — packets/loss/jitter — are all correctly
non-zero and consistent with independent PBX evidence).

**Finding — RTCP reporting cadence**: querying `quality_stats` more than
once within a short window (a few seconds) returned byte-identical
numbers three times in a row across one test run, despite PBX-side
`channelstats` showing continuous packet growth in the same window. This
is correct behavior, not a bug: `stream_rtcp_stats()` (`src/stream.c`)
reflects the most recently *received* RTCP Sender/Receiver Report, not a
live per-packet counter, and this PBX's effective RTCP interval is
empirically on the order of 10-20+ seconds. Documented in `PROTOCOL.md`'s
`stats` event description so a consumer doesn't mistake a fast repeat
poll for a stuck reading.

## Additional verification (beyond the lettered scenarios)

Not explicitly one of a-d, but part of the F1 command set and verified
against the same live PBX:

- **`register`/`unregister` at runtime** (not just process-start): sent
  `unregister` mid-session → `reg_state unregistered`; sent `register`
  again → `reg_state registered`. Repeated 8x in the memory-safety run
  below with no issues.
- **`mute`/un-mute**: on an established `*43` call, `{"cmd":"mute","on":true}`
  → `call_state muted`; `{"on":false}` → `call_state unmuted`. No PBX-side
  media-direction check performed beyond the command round-tripping
  cleanly (the `audio_mute()` call it drives is a purely local flag on
  the outgoing tx path — see `PROTOCOL.md`).
- **`attended_transfer` + `complete_transfer`**: verified for real using
  the same dual-contact bridge as scenario (b). A+B bridged (as above);
  from A, `{"cmd":"attended_transfer","uri":"sip:*60@100.119.230.80",...}`
  → source held (`call_state hold`) → `attended_transfer_started` fired
  with correct `source_call_id`/`target_call_id` → consultation call to
  `*60` established. PBX showed 3 channels at that point (the held A-B
  leg + the new A-*60 consultation, both counted). `{"cmd":"complete_transfer"}`
  → A's source call closed cleanly; PBX afterward showed the surviving
  channel in context `sub-hr12format` running `SayUnixTime` — **B was
  successfully REFER-with-Replaces'd onto the speaking-clock call**,
  confirming `call_replace_transfer()` works end to end, not just
  `call_transfer()`.
- **`abort_transfer`**: exercised in isolated unit-level command-dispatch
  testing (no pending transfer → clean `error`); the full
  hold-then-abort-then-verify-resumed round trip against the live PBX
  was not separately captured as its own artifact (time-boxed — the
  underlying `uag_hold_resume()` call is the exact same one `resume`
  already verified working in scenario (a)).
- **`CENT_TLS_PIN`** (see `BUILD.md` "TLS leaf-certificate pinning"):
  independently confirmed the live cert's SHA256 fingerprint via
  `openssl s_client` matches the v1 app's stored `pinnedCertSha256`
  (`40:16:32:...:bd:c1`) before testing. Correct pin (colon-separated
  format) → `reg_state registered` normally. Deliberately wrong pin
  (`00` × 32) → `reg_state failed`, `reason: "Authentication error [80]"`
  — connection rejected cleanly before any SIP traffic, engine did not
  crash or hang. stderr confirmed the exact rejection path fired:
  `CENT_TLS_PIN: peer certificate fingerprint does not match the pinned
  value - rejecting this connection`.

## Memory safety

- **Unit tests under ASan** (`core/modules/ctrl_json/test/`, `-DCENT_ASAN=ON`):
  63/63 checks pass, 0 ASan findings.
- **Live engine under `leaks`** (macOS): ran the full new-command set
  (blf subscribe/unsubscribe, register/unregister, hold/resume, mute,
  dtmf, quality_stats, and malformed/unknown-command error paths) once,
  then again repeated 8x in a single process lifetime. Both runs: **1
  leak, 1024 bytes**, unchanged by the 8x repetition — i.e. a fixed-size,
  one-time allocation (not scaling with command traffic, so not
  attributable to any per-call/per-command code added in F1; most likely
  re/baresip core init or OpenSSL's own static state). `leaks` flagged
  the process as "not debuggable" (binary not signed with a
  `get-task-allow` entitlement), which blocked a full allocation-site
  stack trace for that one block — the repeat-count comparison was the
  practical way to gain confidence without it. See `BUILD.md` "Memory
  safety" for the full note.

## F3 regression (v1.1 protocol hardening)

Re-verification against the same live test PBX after the v1.1 changes
(`core/PROTOCOL.md` "Changes from v1": `id` request/response correlation,
`devices`/`set_device`, `quality_stats` codec/transport enrichment, pure
JSON stdout) — both to confirm every v1 scenario above still passes
byte-for-byte unchanged, *and* to gather fresh evidence for what's new.
Same methodology as (a)-(d) above: a small Python harness (not checked
in — scratch tooling), OS pipes for stdin/stdout (a real `subprocess`,
not the harness's own shell — `run-spike.sh`'s `fd_listen(STDIN_FILENO,
...)` needs a genuinely pollable fd, which a sandboxed shell's own stdin
redirection doesn't always provide; a subprocess pipe always is), a
background thread parsing `{`-prefixed lines into a queue, `wait_for()`
with a hard timeout, PBX-side snapshots via read-only `asterisk -rx`
before/during/after.

### (e) register → dial \*43 with `id` → `result` + call events → `quality_stats` (enriched) → `devices` → `set_device` → hangup with `id`

**PASS.**

```
-> {"cmd": "dial", "uri": "sip:*43@100.119.230.80", "id": "d1"}
<- {"event":"reg_state","account":"sip:1100@100.119.230.80:8089","state":"registered","transport":"wss"}
<- {"event":"result","id":"d1","ok":true}
<- {"event":"call_state","state":"established","peer":"sip:*43@100.119.230.80;transport=wss","id":"8832d603f43c4fd3","call_id":"8832d603f43c4fd3"}
```

1. `register` (startup, wss) → `reg_state registered` — unchanged from
   scenario (a).
2. `{"cmd":"dial","uri":"sip:*43@100.119.230.80","id":"d1"}` → both a
   correlated `result` (`id:"d1"`, `ok:true`) **and** the normal
   `call_state established` arrived (order between the two is not
   guaranteed by the protocol and wasn't fixed run to run — the harness
   collects both before proceeding). `id`/`result` is additive: nothing
   about the existing `call_state` event changed.
3. After the same ~20s ICE settle window as scenario (a):
   `{"cmd":"quality_stats","call_id":"...","id":"q1"}` →
   ```
   {"event":"stats","call_id":"...","rtt_us":0,"tx_packets":672,"tx_lost":0,
    "tx_jitter_us":2125,"rx_packets":671,"rx_lost":0,"rx_jitter_us":0,
    "codec":"PCMU","transport":"wss"}
   {"event":"result","id":"q1","ok":true,"rtt_us":0,"tx_packets":672,"tx_lost":0,
    "tx_jitter_us":2125,"rx_packets":671,"rx_lost":0,"rx_jitter_us":0,
    "codec":"PCMU","transport":"wss"}
   ```
   Both the standalone `stats` event *and* the `id`-correlated `result`
   carry the new `codec`/`transport` fields (`"PCMU"`/`"wss"` — matches
   the account's `audio_codecs=pcmu,pcma` and the wss registration) —
   confirms both the enrichment itself and the "command-specific fields
   merge onto `result`" design for `quality_stats`. `rtt_us:0` again
   (see scenario (d) — expected, not a regression).
4. `{"cmd":"devices","id":"dv1"}` →
   ```
   {"event":"devices","input":[{"name":"ausine,440","active":true},
    {"name":"aufile","active":false}],
    "output":[{"name":"aufile,/.../centinelo-spike.ZBQWss/rx.wav","active":true}]}
   {"event":"result","id":"dv1","ok":true,"input":[...],"output":[...]}
   ```
   (identical `input`/`output` arrays on both — confirmed). One real
   finding here: `input` lists *two* entries — `ausine,440` (the
   configured/active source) *and* `aufile` (`aufile` registers both an
   `ausrc` *and* an `auplay` driver, see `modules/aufile/aufile.c`, so it
   legitimately appears in `input` too, `active:false` since the account
   isn't sourcing from it) — correct behavior for this build's module
   set, not a bug; worth knowing before assuming `input.length` maps
   1:1 to "physical microphones" once a real device backend is added.
5. `{"cmd":"set_device","kind":"input","name":"ausine,440","id":"sd1"}`
   (the exact `name` string read back from step 4's own `devices`
   event — round-trip, as designed) → `{"event":"result","id":"sd1","ok":true}`.
   Applied to the *already-active* driver (idempotent stop+restart of
   the same `ausrc`), on a live, established call — no error, no
   observable disruption to the running call (confirmed by the
   subsequent hangup completing normally, next step).
6. `{"cmd":"hangup","call_id":"...","id":"h1"}` → `result id:"h1" ok:true`
   **and** `call_state closed` — same additive relationship as step 2.

### (f) PBX-side corroboration + `-s` stdout purity

**PASS.** A second, focused run (register → dial \*43 → hangup, quick,
`CENT_BARESIP_ARGS="-s"`) with `asterisk -rx "core show channels
concise"` snapshots around the call, independent of the harness's own
self-reported events:

```
PBX channels BEFORE:        []
PBX channels DURING call:   ['PJSIP/1100-0000087b!from-internal!*43!7!Up!BackGround!demo-echotest,,,app-echo-test-echo!1100!!!3!3!!1784157785.3767']
PBX channels AFTER hangup:  []
```

A real PBX channel exists exactly while the engine reports the call
`established`, running the expected `demo-echotest` application (safe
target, matches scenario (a)/(b)'s own `*43` usage), and is gone again
right after `hangup` — independent confirmation the JSON events reflect
real call state, not just internally-consistent self-reporting.

### (g) stdout purity — the actual acceptance test

**PASS, both scenarios (e) and (f).** Every stdout line captured by the
harness (everything the child process ever wrote to its stdout, not
just the ones that happened to parse as JSON) was checked with the
Python equivalent of `grep -cv '^{'`:

| Run | Total stdout lines | Non-JSON lines |
|---|---|---|
| (e) — full scenario, no `-s` | 12 | **0** |
| (f) — quick scenario, `CENT_BARESIP_ARGS="-s"` | 7 | **0** |

`-s` was confirmed to actually be *doing* something in run (f) — not
just silently absent — by grepping the run's stderr for SIP
INVITE/REGISTER occurrences: **31 matches**, i.e. the SIP trace machinery
genuinely fired repeatedly during this run and still produced zero
stdout leakage; this isn't "it passed because it was never exercised."

Getting to `0`/`0` took two rounds, both against this same real PBX, and
that gap between them is itself a real finding (see below): the first
round (`core/patches/0003-*` only — the baresip-side banner/log/
SIP-trace fix) brought a scenario-(e)-shaped run down from the v1
baseline to **3** non-JSON lines (`"websock: connecting to
'wss://100.119.230.80:8089/ws'"`, `"<...> WSS websock established to
100.119.230.80:8089"`, `"--> send"`) — all from unconditional
`re_printf()`s in `core/deps/re`'s own SIP-over-WS transport code
(`src/sip/transp.c`), a different submodule than 0003 touched, only
found by actually capturing and grepping a live run's stdout, not by
inspection. `core/patches/0004-*` fixed those (plus two adjacent error-
path `re_printf()`s in the same functions), and the second round of
scenario (e) is where the `0`/`12` numbers above came from. See
`core/BUILD.md` "Findings" for the full per-line breakdown, including
several *other* `re_printf()` call sites found during the same audit
that were deliberately left unpatched (dormant/unreachable for this
engine's actual usage — dead code, debug-gated-off, wrong protocol/no
module loaded, or a WS-server-only accept path this outbound-only client
never reaches).

## F4 audio tap

Re-verification against the same live test PBX for v1.2's new
`tap_start`/`tap_stop` (`core/PROTOCOL.md` "Changes from v1.1") — the
per-call audio-tap foundation for local transcription. Same harness
methodology as (e)-(g) above (a small Python harness, not checked in —
scratch tooling; background thread parsing `{`-prefixed lines,
`wait_for()` with a hard timeout, read-only `asterisk -rx` PBX
snapshots), plus a second, independent verification pass over the
resulting WAV files themselves using nothing but Python's stdlib `wave`
module (deliberately *not* reusing any of this engine's own WAV-writing
code — see "(i)" below).

Secret handling: the harness reads ext 1100's password from this
machine's local Centinelo Phone `settings.json` (per this workspace's
`CLAUDE.md`) straight into the child process's env dict, in Python
memory only — never on a command line, never logged, never written
anywhere this document (or the harness's own source) could leak it.

### (h) register → dial \*43 → tap_start → ~12s capture → tap_stop → hangup

**PASS**, run twice (two independent live calls, different `call_id`s —
see "(j)" below for the second run's own numbers and why it exists).
Full captured exchange, run 1 (`call_id` truncated to `2845d09c...` for
readability):

```
<- {"event":"ready"}
<- {"event":"reg_state","account":"sip:1100@100.119.230.80:8089","state":"registered","transport":"wss"}
-> {"cmd":"dial","uri":"sip:*43@100.119.230.80","id":"d1"}
<- {"event":"result","id":"d1","ok":true}
<- {"event":"call_state","state":"established","peer":"sip:*43@100.119.230.80;transport=wss","id":"2845d09c...","call_id":"2845d09c..."}
```

1. `register` (startup, wss) → `reg_state registered` — unchanged from
   scenario (a)/(e).
2. `dial *43` → `established`.
3. **Deliberate deviation from the F4 task's own prose ordering, called
   out explicitly**: the task description that specified this feature
   listed the e2e sequence as "register → tap_start → dial *43 → ...".
   This harness instead dials *first*, waits for the call to exist and
   settle, *then* taps — because `tap_start`'s own design (`call_id`
   optional, falling back to "the current call" via the same
   `resolve_call()` every other call-scoped command in this protocol
   uses) has no "arm for a call that doesn't exist yet" mode, by
   design: **every** call-scoped command in this protocol (`hold`,
   `mute`, `dtmf`, `blind_transfer`, `quality_stats`, and now
   `tap_start`/`tap_stop`) resolves to a real, already-existing call or
   fails with a plain `error` event — there is no precedent anywhere
   else in this protocol for a command that silently queues itself for
   a future call, and inventing one just for `tap_start` would have
   made it the one call-scoped command in the whole protocol that
   behaves differently from every other one. Confirmed empirically
   too, not just by design: sending `tap_start` before `dial` in an
   early manual test produced the expected, unsurprising `{"event":
   "error","message":"tap_start: call not found"}` — correct behavior,
   not a bug, given the design above.
4. Same ~15-20s ICE-settle window as scenario (a) — but **polled, not a
   fixed sleep**, using scenario (a)'s own "Summary of findings" #1
   recommendation for a future automated test ("drive it off
   `quality_stats` first reading non-zero instead of a fixed sleep"):
   the harness sent `quality_stats` every 3s and waited for both
   `tx_packets` and `rx_packets` to read non-zero. Real captured
   sequence, run 1 (three all-zero polls, ICE still settling, then real
   traffic):
   ```
   <- {"event":"stats","call_id":"2845d09c...","tx_packets":0,"rx_packets":0,...}
   <- {"event":"stats","call_id":"2845d09c...","tx_packets":0,"rx_packets":0,...}
   <- {"event":"stats","call_id":"2845d09c...","tx_packets":0,"rx_packets":0,"codec":"PCMU",...}
   <- {"event":"stats","call_id":"2845d09c...","tx_packets":172,"rx_packets":170,"codec":"PCMU",...}
   ```
   Settled at ~9s after `established` this run (three 3s polls) — inside
   scenario (a)'s documented 15-20s range, on the earlier side of it.
5. `{"cmd":"tap_start","dir":"<abs scratch dir>","call_id":"2845d09c...","id":"t1"}` →
   ```
   {"event":"tap_state","call_id":"2845d09c...","state":"started",
    "rx_path":".../2845d09c...-rx.wav","tx_path":".../2845d09c...-tx.wav"}
   {"event":"result","id":"t1","ok":true}
   ```
   Both paths exist on disk (confirmed via a separate `ls` right after
   this event — 0 bytes each at this instant, before either direction's
   first real frame commits a header — see `wav_writer.h`).
6. Harness slept 12s (audio flowing: our own `ausine,440` tone going
   out, the PBX's `*43` demo-echotest app's response coming back — see
   "(i)" below for exactly what came back).
7. `{"cmd":"tap_stop","call_id":"2845d09c...","id":"t2"}` →
   ```
   {"event":"tap_state","call_id":"2845d09c...","state":"stopped",
    "rx_path":".../2845d09c...-rx.wav","tx_path":".../2845d09c...-tx.wav",
    "rx_bytes":192000,"tx_bytes":192000,"rx_duration_ms":12000,"tx_duration_ms":12000}
   {"event":"result","id":"t2","ok":true}
   ```
   `192000 bytes / (8000 Hz × 2 bytes/sample) = 12.000s` exactly, both
   directions — matches the harness's own ~12s sleep almost exactly (see
   "(i)" for independent confirmation straight from the files
   themselves, not just this event's self-reported numbers).
8. `{"cmd":"hangup",...}` → `call_state closed`, then `quit` → clean
   process exit. No crash, no hang, no leftover PBX channel (confirmed —
   see "(j)").

### (i) WAV file verification (`python3` `wave` module)

**PASS.** Deliberately re-parsed both output files with nothing but
Python's stdlib `wave` module — not this engine's own `wav_writer.c`, so
this is checking the *bytes on disk* against the WAV spec independently,
not just that this engine agrees with itself:

| File (run 1, `call_id` `2845d09c...`) | channels | sample width | frame rate | frames | duration | RMS | peak |
|---|---|---|---|---|---|---|---|
| `-rx.wav` (remote/decoded) | 1 (mono) | 2 (16-bit) | 8000 Hz | 96000 | 12.000s | **3327.0** | 28028 |
| `-tx.wav` (local/pre-encode) | 1 (mono) | 2 (16-bit) | 8000 Hz | 96000 | 12.000s | **5791.9** | 8191 |

Both files: `actual file size on disk == 44 + (frames × 2 bytes)` exactly
(192044 bytes each) — confirms the header's own RIFF/data chunk-size
fields, patched at `tap_stop`, are byte-accurate, not just
plausible-looking. Duration from the *file itself* (`frames / 8000`)
matches the `tap_state stopped` event's own self-reported
`rx_duration_ms`/`tx_duration_ms` (12000ms both) exactly — the protocol
event isn't fabricating numbers independent of what actually landed on
disk.

**Non-silence**: both RMS values are unambiguous — silence/comfort noise
on this codec/path reads in the tens at most; thousands is real audio
content in both directions, satisfying the F4 task's own "non-silence
(RMS above a threshold)" requirement with a very wide margin either way.

**Bonus check — single-frequency (Goertzel) scan, not required by the
task but run anyway for extra confidence**: `-tx.wav` (sourced entirely
from this engine's own `ausine,440` config, zero PBX involvement) shows
an exact, dominant 440 Hz peak (magnitude 4095.5, next-nearest 20 Hz-step
bin at less than a tenth of that) — byte-accurate proof the encode-side
tap captures *exactly* the known source signal, not noise or silence
dressed up with a valid header. `-rx.wav` (the PBX's actual response) is
real, substantial audio (RMS 3327-3325 across both runs) but is
*spectrally more complex* than a single clean tone (magnitude at exactly
440 Hz: only 189.6, vs. a broader peak around 220 Hz) — most likely
because Asterisk's `demo-echotest`/`app-echo-test-echo` application
plays a spoken announcement/prompt rather than being a byte-for-byte RTP
echo the whole time (this repo's own prior scenario (f) already
identified the PBX-side app name; nothing here contradicts it — it's the
same app). This is a PBX test-application-behavior characteristic, not a
tap defect: the tap faithfully records whatever audio actually arrives
on each side, and the *tx* side (where this engine controls the ground
truth completely) proves that faithfulness byte-for-byte. Not re-run
with a numpy/scipy proper FFT (unavailable in this environment) — the
pure-Python Goertzel single-bin detector used here is exact for the one
frequency it targets (440 Hz falls on an exact bin at this window
size/sample rate, no spectral leakage), just not a full spectrum plot.

### (j) PBX-side corroboration + repeatability (run 2)

**PASS.** A second, independent full run (fresh call, `call_id`
`1ad0c76b...`) both cross-checks repeatability and adds a live PBX-side
channel snapshot taken *during* the tap window (same "independent
confirmation the JSON events reflect real call state" methodology as
scenario (f), read-only `asterisk -rx`, no PBX config touched):

```
PBX channels DURING tap: PJSIP/1100-0000088b!from-internal!*43!7!Up!BackGround!demo-echotest,,,app-echo-test-echo!1100!...
```

A real, live PBX channel exists, running the same `demo-echotest`/
`app-echo-test-echo` application scenario (f) already identified,
*while* the tap is actively capturing — independent confirmation this
isn't a local-loopback artifact. Run 2's own file-level numbers (again
independently re-parsed with `python3`'s `wave` module):

| File (run 2, `call_id` `1ad0c76b...`) | frames | duration | RMS | peak |
|---|---|---|---|---|
| `-rx.wav` | 100640 | 12.580s | 3325.0 | 28028 |
| `-tx.wav` | 100800 | 12.600s | 5791.9 | 8191 |

Consistent with run 1 within run-to-run timing noise (this run's harness
paused mid-capture to make the SSH corroboration call above, so the
sleep window ran ~0.6s long — both `tap_state stopped`'s own
`rx_duration_ms`/`tx_duration_ms` and the independently-reparsed file
durations agree on that *same* slightly-longer window, not just with
each other in the abstract). `tx.wav`'s RMS (5791.9) and peak (8191) are
bit-for-bit identical between both runs — expected and reassuring, since
`ausine,440` generates the exact same deterministic tone every time;
`rx.wav`'s RMS is within 0.06% run to run (3327.0 vs 3325.0) — real
network audio, not literally identical, but consistent. Also confirms
this feature is repeatable, not a one-off — two independent live calls,
two clean `tap_start`→capture→`tap_stop`→`hangup` cycles, zero crashes,
zero hangs, zero corrupt files.

**stdout purity regression check** (v1.1's own F3 acceptance test,
re-run here since `tap_state` is a new event type that could in
principle have introduced its own stray non-JSON output): both runs,
every stdout line captured by the harness, `grep -cv '^{'`-equivalent:

| Run | Total stdout lines | Non-JSON lines |
|---|---|---|
| (h)/(i) — run 1 | 19 | **0** |
| (j) — run 2 | 19 | **0** |

v1.2 does not regress v1.1's pure-NDJSON guarantee.

## Summary of findings (for future F-phases)

1. ICE needs real settle time here (~15-20s) before relying on live RTP
   — not a defect, but worth budgeting for in any future automated test
   or in shell-side UX (e.g. don't judge "is the call actually working"
   purely off the `established` event without a grace period, or drive
   it off `quality_stats` first reading non-zero instead of a fixed
   sleep).
2. `stream_rtcp_stats()`/`quality_stats` is RTCP-interval-limited
   (~10-20s here), not live-per-packet. Documented in `PROTOCOL.md`.
3. The real dialog-info NOTIFY shape for "idle" uses a populated
   `<dialog><state>terminated</state></dialog>`, not an absent
   `<dialog>` element — both are handled, but only the real capture
   proves which one this PBX actually sends. Added as a permanent
   regression fixture.
4. Ext 1100 has no voicemail mailbox (`VMCONTEXT=novm`) — PBX-config
   footnote for whoever sets up test extensions next, not an engine
   defect. No PBX config was touched to work around this.
5. Blind/attended transfer of a single-party `Background()`/`Answer()`
   demo-app channel (the safe feature-code test targets, by their
   nature) cannot succeed — Asterisk's native transfer needs a real
   bridge to redirect. A dual-contact self-bridge on the test extension
   (same extension, two simultaneous registrations, `max_contacts: 2`)
   is a safe (nothing rings in the clinic) way to get a genuine bridge
   for transfer testing without dialing a real extension.
6. **(F3)** A stdout-purity fix needs its acceptance test to actually
   *run*, not just be reasoned about: the first-pass baresip-only patch
   looked complete by inspection (every `info()`/`warning()`/`debug()`
   call site traced back to one gate) but missed a second submodule
   (`re`) entirely — its own unconditional `re_printf()`s in the SIP-
   over-WS transport code don't go through baresip's logging system at
   all. `grep -cv '^{'` against a real captured run is what actually
   caught it; a code-reading-only review of "every `info()`/`warning()`
   call site" would not have.
7. **(F3)** `aufile` registers as *both* an `ausrc` and an `auplay`
   driver (see `modules/aufile/aufile.c`), so it legitimately appears in
   `devices`' `"input"` array too (as `active:false`, alongside the
   real active source) even though nothing in this engine's config ever
   sources audio from it — correct, not a bug, but worth knowing before
   assuming a `devices` array's length maps 1:1 to physical
   microphones/speakers.
8. **(F3)** `audio_set_source()`/`audio_set_player()` (`src/audio.c`)
   are genuine live hot-swap APIs — confirmed both by reading their
   implementation (stop the running `ausrc_st`/`auplay_st`, allocate a
   fresh one against the same `struct audio`, no re-INVITE) and by
   exercising `set_device` against an already-established call in a
   real e2e run with no disruption to that call (it continued normally
   through to a clean `hangup` afterward).
9. **(F4)** A tap-scoped command with no "arm for a future call" mode is
   the *consistent* design, not a limitation worth working around — see
   "(h)" step 3 for the full reasoning. Worth remembering for any future
   command in this protocol that might be tempted to special-case
   "no call yet" into a queued/deferred behavior: nothing else here does
   that, and there's real value (one predictable failure mode,
   `resolve_call()` returning `NULL` → a plain `error`) in not being the
   first.
10. **(F4)** The PBX's `*43` demo-echotest app is *not* simply a
    byte-for-byte RTP echo the whole call — see "(i)"'s Goertzel
    single-frequency scan: this engine's own outgoing `ausine,440` tone
    (`tx.wav`) comes back spectrally different on the incoming side
    (`rx.wav`), most likely because the app plays a spoken announcement/
    prompt at some point rather than echoing continuously from the
    moment media starts. Both directions are still unambiguously
    non-silent (RMS in the thousands), so this doesn't block using `*43`
    as this repo's safe e2e audio target — but a *future* test that
    specifically needs to assert "the received audio is exactly the sent
    tone" (rather than just "real audio arrived") would need a different
    target or a longer capture window past the announcement, not `*43`
    used the way this document uses it.

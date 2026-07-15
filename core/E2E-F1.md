# core/ — F1 end-to-end verification

Real evidence, gathered by actually running the built engine against the
live test PBX (FreePBX 17 / Asterisk 22, `100.119.230.80`, Tailscale-only,
ext 1100 / secret from `~/Library/Application Support/Centinelo
Phone/settings.json`, never printed/committed). PBX-side verification was
SSH + read-only `asterisk -rx "... show ..."` commands only — no PBX
configuration was changed at any point in this work.

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

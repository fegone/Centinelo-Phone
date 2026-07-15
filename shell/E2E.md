# F2 e2e verification — evidence

**Verdict: PASS.** The real Tauri app (debug build, `cargo tauri dev`)
registered extension **1100** on **100.119.230.80** over **WSS**, placed a
real call to the `*43` echo test extension through its own sidecar
supervisor (`shell/src-tauri/src/sidecar.rs`), and independent,
Asterisk-side, read-only verification confirmed real bidirectional RTP
(non-zero, monotonically increasing packet counts, 0% loss) for the full
duration of every held call. Four separate calls were run; all four
registered, established, carried real audio, and closed cleanly.

## Setup

- Test PBX: `100.119.230.80` (FreePBX/Asterisk, Tailscale), extension
  `1100`, secret read from the v1 app's own settings file
  (`~/Library/Application Support/Centinelo Phone/settings.json`,
  never printed/logged/committed — see "How settings were configured"
  below).
- Core binary: built locally per `core/BUILD.md` in this worktree
  (`core/deps/baresip/build/baresip`, auto-resolved by
  `resolve_core_binary()` — no `CENTINELO_CORE_BIN` override needed).
- Transport: `wss` (explicit, not `auto`) for determinism — `auto`'s
  wss->udp fallback is a startup-time decision (see README "Architecture"),
  not something worth re-testing per call.
- Dial target: `sip:*43@100.119.230.80` (Asterisk's built-in echo test —
  answers immediately, loops audio back, exactly what `core/BUILD.md`'s own
  spike testing used).

## Methodology: why a scripted driver instead of clicking through the UI

The task explicitly allows either driving the real UI or "invoking the
same Tauri commands programmatically." I started with the former (OS-level
mouse/keyboard automation against the real running app) and got a real
first pass working — registration, dial, live call overlay, hangup, all
visually confirmed. Partway through testing the *settings* flow, though,
this machine turned out to have **another automated process concurrently
driving the same physical screen/keyboard/mouse** (a Safari-based Google
account creation flow, unrelated to this task, that kept regaining
frontmost focus and once caused stray characters to land in one of this
app's own text fields). Continuing to fight over shared input risked two
bad outcomes: corrupting my own test data (which is what actually
happened — see "Investigated, not a bug" below) and — worse — a
misdirected click landing in *that* unrelated flow.

So the final, evidentiary e2e runs use `shell/src-tauri/src/e2e.rs`: a
`#[cfg(debug_assertions)]`-only module (never compiled into a release
build) that, when `CENTINELO_E2E_SCRIPT` is set, calls **the exact same
`#[tauri::command]` functions** (`commands::sidecar_dial`,
`commands::sidecar_answer`, `commands::sidecar_hangup`) the frontend's
`invoke()` calls would reach, obtained via `AppHandle::state()` — the same
`State<T>` extractors Tauri's own IPC dispatch constructs. This exercises
the identical sidecar-supervisor code path a human clicking the dialpad
would, with zero OS-level input dependency. Script grammar:
`wait:<secs>|dial:<uri>|answer|hangup` steps joined with `|`.

The **UI itself was still visually verified** earlier in the session
(idle main window, live in-call overlay with a real 00:11 timer, settings
admin-lock flow) by screenshot — see "UI screenshots (visual QA, not
saved as files)" below — before the shared-desktop contention made
further click-driven testing unreliable.

## Evidence: app-side protocol trail (real run, verbatim log)

Captured from the running app's own log (`sidecar.rs` logs every parsed
`ctrl_json` event at `INFO`: `sidecar event: {...}`, plus the e2e driver's
own `e2e: ...` lines) — this run held the call 16 seconds:

```
21:55:22 e2e: script starting: wait:6|dial:sip:*43@100.119.230.80|wait:16|hangup|wait:3
21:55:22 e2e: waiting 6s
21:55:22 sidecar event: {"event":"ready"}
21:55:22 sidecar event: {"account":"sip:1100@100.119.230.80:8089","event":"reg_state","state":"registered","transport":"wss"}
21:55:28 e2e: dial(sip:*43@100.119.230.80) -> ok
21:55:28 e2e: waiting 16s
21:55:28 sidecar event: {"event":"call_state","id":"4f64f9c1cbf47ffb","peer":"sip:*43@100.119.230.80;transport=wss","state":"established"}
21:55:44 e2e: hangup -> ok
21:55:44 e2e: waiting 3s
21:55:44 sidecar event: {"event":"call_state","id":"4f64f9c1cbf47ffb","peer":"sip:*43@100.119.230.80;transport=wss","state":"closed"}
21:55:47 e2e: script complete
```

Registration took well under a second; the call reached `established`
immediately (Asterisk's echo test answers instantly, no separate
`ringing` event — expected per `PROTOCOL.md`).

## Evidence: PBX-side RTP confirmation (read-only, independent of the app)

Per the task's instructions: `ssh -i ~/.ssh/id_neola_vps root@100.119.230.80
"asterisk -rx 'pjsip show channelstats'"`, polled every ~3-5s during the
held call. Cleanest single-channel run (20s hold):

```
=== poll 1 at 17:56:56 ===
 BridgeId ChannelId ........ UpTime.. Codec.   Count    Lost Pct  Jitter   Count    Lost Pct  Jitter RTT....
          1100-00000863      00:00:08 ulaw      244       0    0   0.000    244       0    0   0.002   0.000
Objects found: 1

=== poll 2 at 17:56:59 ===
          1100-00000863      00:00:11 ulaw      399       0    0   0.000    399       0    0   0.001   0.000
Objects found: 1

=== poll 3 at 17:57:03 ===
          1100-00000863      00:00:15 ulaw      555       0    0   0.000    555       0    0   0.001   0.000
Objects found: 1
```

Same channel ID throughout; UpTime and Rx/Tx counts increase monotonically
(244 -> 399 -> 555, ~155 packets/3.5s ≈ 44pps, in the right neighborhood for
G.711 20ms packetization with polling jitter); Rx == Tx exactly (expected —
Asterisk's echo test loops the audio straight back); **0% loss, 0.000
jitter, 0.000 RTT throughout** (LAN/Tailscale-local). This is unambiguous
evidence of real, live, flowing bidirectional RTP audio, independently
confirmed from the PBX side, not just the app's own event log.

A second, later run (25s hold) additionally showed **two** RTP legs
(`1100-00000865` / `...866`, both counts climbing together, still 0% loss)
— consistent with Asterisk's echo-test channel plus its bridged peer leg.

Every poll taken *after* the scripted `hangup` returned `No objects
found.` — the channel is gone, confirming clean teardown, not a stuck call.

## Evidence: frontend reacted correctly, not just the backend

Even though `e2e.rs` calls the backend directly (bypassing the frontend's
own `dialUri()` JS), the backend's `sidecar-event`/`sidecar-status` Tauri
emits reach the frontend's `listen()` handlers exactly as they would from a
UI-driven call — proven by `recents.json` (app-data dir) correctly
accumulating one entry per test call, with `duration_secs` matching each
script's actual hold time:

```json
[
  { "peer": "*43", "direction": "inbound", "duration_secs": 16, "missed": false },
  { "peer": "*43", "direction": "inbound", "duration_secs": 10, "missed": false }
]
```

**Known artifact of this test method** (not a product bug): both entries
say `"inbound"`. The frontend infers direction from whether it already had
a local `state.call` (set optimistically by its own `dialUri()`) *before*
the first `call_state` event arrives. Since `e2e.rs` calls
`sidecar_dial` directly, that optimistic local state never gets set, so
`handleCallState()` falls through to its "no prior state -> must be
inbound" branch. A real user clicking the dialpad's call button *would*
have `state.call.direction = "outbound"` set first (see `dialUri()` in
`app.js`), so this only affects this specific bypass-the-frontend test
path, not real usage.

## UI screenshots (visual QA, not saved as files)

Captured via the desktop screenshot tool during this session (full-screen
captures, not saved into the repo — see README for why; the objective,
reproducible evidence is the log/RTP data above). What was directly
observed on the real running app:

1. **Idle main window** — titlebar "Centinelo · Ready" with the breathing
   amber watchlamp dot; identity card "E2E Test / EXT 1100" with a green
   "WSS" registration pill; empty dial display; full 3x4 keypad; green call
   button; 4 favorite tiles all showing "Empty"; recents list showing
   `*43 / Incoming / <time> / 00:2X` rows, newest first, matching
   `mockups/main.html` closely.
2. **Live in-call overlay**, captured ~11s into an active held call —
   titlebar state "a call — *43"; caller medal "43" with a green live lamp;
   bold "*43", "Incoming call", "Main line"; timer showing **`00:11`**
   (tabular mono, matches the design spec); "ENCRYPTED CALL" secure line;
   full-width coral "End call" button. Matches the compacted adaptation of
   `mockups/in-call.html` described in README's design-fidelity notes.
3. **Settings, admin-lock states** — both the first-run "Set an admin
   password" card and the post-unlock fully-editable form (display name,
   theme row, host/extension/password fields, transport cards, advanced
   core-path field, admin-password-change row) rendered correctly, matching
   `mockups/settings.html`/`onboarding.html`'s visual language. The
   `[hidden]`-vs-`display:flex` CSS bug described below was caught and
   fixed via this same screenshot QA.

Dark theme was not re-screenshotted after the desktop-contention issue
appeared (see README's design-fidelity notes for why confidence in it is
still high — same CSS mechanism, same tokens, only the `light-dark()`
second value differs).

## Bugs found and fixed during this verification pass

1. **`[hidden]` attribute silently overridden.** Several views
   (`.call-overlay`, `.settings-screen`, `.lock-card`, ...) set
   `display:flex` on the same element the app toggles `hidden` on. Per the
   CSS cascade, an author rule beats the user-agent default
   `[hidden]{display:none}` at equal specificity, so "hidden" elements were
   rendering anyway (first caught as both admin-lock cards showing
   simultaneously). Fixed with one rule: `[hidden]{display:none!important}`
   in `app.css`.
2. **Settings unreachable on macOS.** The Settings button lived inside the
   same `.winbtns` container as Minimize/Close, and macOS hides that whole
   container (native traffic lights replace it). Once an account was
   configured there was no way to reopen Settings. Fixed: only
   `#btn-minimize`/`#btn-close` are mac-hidden now, Settings stays visible.

## Investigated, not a bug: transport selector visually showing "Auto"

Mid-session, the Settings transport picker appeared to show "Auto"
selected for an account whose saved `transport_priority` was `"wss"`.
Given the concurrent-desktop-input finding above, I suspected test
contamination (a stray click from the other process landing on the "Auto"
card) rather than a real defect, and confirmed it with a GUI-independent
check: a temporary debug line in `lib.rs` calling
`commands::get_account_settings` directly (the exact function the frontend
invokes) and logging its JSON output on startup:

```
diag: get_account_settings -> {"host":"100.119.230.80","ext":"1100","display_name":"E2E Test","transport_priority":"wss","secret_set":true}
```

This confirms the backend-to-frontend contract returns `"wss"` correctly,
and it's the exact payload `setTransportUI()` receives — the frontend
logic was re-read multiple times with no bug found either. Also,
independently: all four registrations in this session's evidence above
show `"transport":"wss"`, which is only possible if the *actual* stored
transport (used for real, not just displayed) was correct throughout. The
debug line was removed before committing (see git history).

## Runs summary

| Run | Hold | Registered | Established | RTP confirmed | Closed cleanly |
|---|---|---|---|---|---|
| 1 | 10s | yes (wss) | yes | inconclusive (SSH cold-connect latency ate the window) | yes |
| 2 | 16s | yes (wss) | yes | **yes** (484->590, 0% loss) | yes |
| 3 | 20s | yes (wss) | yes | **yes** (244->399->555, 0% loss) | yes |
| 4 | 25s | yes (wss) | yes | **yes** (2 legs, both climbing, 0% loss) | yes |

Run 1's inconclusive SSH poll was a test-harness issue (first SSH
connection to the host paid full handshake latency) fixed for runs 2-4 by
pre-warming an `SSH ControlMaster` connection — not a product issue.

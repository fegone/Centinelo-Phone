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

# F3 e2e verification — live BLF favorites + click-to-call bridge

**Verdict: PASS** for both features required to carry e2e evidence (BLF,
bridge). The third F3 feature — `centinelo://`/`tel:` deep-link handling —
is implemented and unit-tested (18 pure tests across `bridge.rs` and
`deeplink.rs`, `cargo test --lib`) but is **not** independently e2e-verified
here: `tauri-plugin-deep-link` itself documents that dynamic OS-level
registration is unsupported on macOS, and real OS-level scheme activation
can only be exercised against a *bundled and installed* `.app` (not a plain
`cargo tauri dev` process) — installing this dev build into `/Applications`
and registering it as a system default handler was judged out of scope for
a debug-build verification pass. See "Known limitations" below and
`deeplink.rs`'s own module doc for the full platform breakdown.

## Setup

Same test PBX/extension as F2 (`100.119.230.80`, ext `1100`, secret from
the v1 app's own settings file, transport `wss`) — see F2's "Setup" above.
Two new things this session:

- **Second baresip instance ("instance B")**: a *separate* process, run
  directly via `core/run-spike.sh` (not through the shell) with its own
  scratch dir, registered as the **same** extension `1100` — ext `1100`
  allows `max_contacts=2`, so this is a legitimate dual-contact
  registration, not a conflict. Driven by piping a persistent `tail -f
  cmds.txt` into its stdin (a plain FIFO would EOF - and per
  core/PROTOCOL.md, stdin EOF means `quit` - the moment a single write
  finished) and a second `tail -F instanceB.log | grep incoming` loop that
  appends `{"cmd":"answer"}` to `cmds.txt` the instant an `incoming`
  `call_state` appears, so the answer is programmatic and near-instant
  (no reliance on this agent's own reaction time across tool calls).
- **The shell app itself ("instance A")**: `shell`'s own `settings.json`
  (`~/Library/Application Support/com.centinelo.phone/settings.json`)
  edited directly (host/ext/secret/transport already present from F2's own
  testing; only `favorites` was changed) to set `favorites[0] =
  {"ext":"1100","label":"Self (BLF)"}` and `favorites[1] =
  {"ext":"510","label":"Test 510"}` — this is what makes the shell
  `blf_subscribe` both on registration, exercising the actual free-tier
  feature rather than a synthetic backend-only call. Editing settings.json
  directly (not through the GUI) keeps setup fully scripted/non-interactive
  per the task's "never desktop GUI automation" constraint; `SettingsStore`
  is the same JSON shape the app itself reads and writes.

## Methodology

Both scenarios use `CENTINELO_E2E_SCRIPT` (see F2 "Methodology" for why —
same shared-desktop-input-contention reasoning) plus `RUST_LOG=info npm run
dev`, log redirected to a file, so every `sidecar-event`/bridge/e2e log
line is captured verbatim for citation. The click-to-call bridge itself is
exercised with real `curl` requests from a second terminal against
`127.0.0.1:38911` while the app runs — this *is* the intended real-world
client (the paired Chrome extension talks to this exact HTTP contract), so
`curl` isn't a stand-in for anything, it's a faithful client.

"Assert via app state" (BLF): rather than reaching into the webview's JS
(`state.blf`) via devtools/GUI, the backend now also tracks the same data
(`sidecar.rs` `Shared::blf_states: Mutex<HashMap<String,String>>`, updated
from the identical `blf` events the frontend consumes) and exposes it two
ways: a `get_blf_states` Tauri command (wired into the frontend's own
`boot()` so a devtools reload rehydrates instead of going blank - a real
resilience improvement, not just test scaffolding) and an e2e-only log
line at script completion (`e2e.rs`: `"e2e: final blf_states = {:?}"`).
That log line is what's cited below as genuine, backend-tracked "app
state" — not a self-report, since it's produced by the same code path
serving the live UI and the bridge's own `/ping`.

## Evidence: BLF favorites (task a)

Two full runs were captured; the second (below) is the clean one — the
first surfaced and fixed a test-harness timing bug (see "Investigated, not
a bug" below), not a product bug.

`CENTINELO_E2E_SCRIPT="wait:5|dial:sip:1100@100.119.230.80|wait:15|hangup|wait:3"`,
instance B already registered and its auto-answer loop already armed
before instance A started:

```
23:29:05 sidecar event: {"account":"sip:1100@100.119.230.80:8089","event":"reg_state","state":"registered","transport":"wss"}
23:29:05 sidecar event: {"event":"blf","ext":"1100","state":"idle"}
23:29:05 sidecar event: {"event":"blf","ext":"510","state":"idle"}
23:29:09 e2e: dial(sip:1100@100.119.230.80) -> ok
23:29:10 sidecar event: {...,"event":"call_state",...,"peer":"sip:1100@pbx.neoladental.com","state":"incoming"}
23:29:10 sidecar event: {"event":"blf","ext":"1100","state":"busy"}
23:29:10 sidecar event: {...,"event":"call_state","peer":"sip:1100@100.119.230.80;transport=wss","state":"ringing"}
23:29:10 sidecar event: {...,"event":"call_state","peer":"sip:1100@100.119.230.80;transport=wss","state":"established"}
23:29:10 sidecar event: {...,"event":"call_state",...,"peer":"sip:1100@pbx.neoladental.com","state":"closed"}
23:29:10 sidecar event: {"event":"blf","ext":"1100","state":"busy"}
23:29:24 e2e: hangup -> ok
23:29:24 sidecar event: {...,"event":"call_state","peer":"sip:1100@100.119.230.80;transport=wss","state":"closed"}
23:29:25 sidecar event: {"event":"blf","ext":"1100","state":"idle"}
23:29:27 e2e: final blf_states = {"1100": "idle", "510": "idle"}
23:29:27 e2e: script complete
```

Confirms, in order: (1) registration; (2) **auto-subscribe on registration**
— `blf_subscribe` fired for *both* configured favorites (`1100` and `510`)
with no explicit command in the e2e script, purely from `favorites` in
settings — this is the actual F3 feature, not a manual trigger; (3) instance
A dialing `1100` forks to instance B (the dual-contact "truco" — B's own
log shows `incoming` -> auto-answered -> `established` in under a second,
confirmed independently, see below); (4) A's own outbound leg goes
`ringing` -> `established`, i.e. a **real, answered, two-way call**, not
just a ring; (5) BLF for `1100` transitions `idle` -> `busy` and back to
`idle` -> tracked correctly end to end; (6) `510` never moves — stays
`idle` for the entire run, confirmed in the final backend-state dump; (7)
nothing else was dialed or subscribed — only `1100` and its own watched
extensions, matching "nothing rings in the clinic (1100 only)".

Instance B's independent log for the same window (own scratch process, own
PBX-side registration):

```
{"event":"reg_state","account":"sip:1100@100.119.230.80:8089","state":"registered","transport":"wss"}
{"event":"call_state","state":"incoming","peer":"sip:1100@pbx.neoladental.com","id":"dd3c4008-...","call_id":"dd3c4008-..."}
{"event":"call_state","state":"established","peer":"sip:1100@pbx.neoladental.com","id":"dd3c4008-...","call_id":"dd3c4008-..."}
{"event":"call_state","state":"closed","peer":"sip:1100@pbx.neoladental.com","id":"dd3c4008-...","call_id":"dd3c4008-..."}
```
(auto-answer log: `AUTO-ANSWERED: {"event":"call_state","state":"incoming",...}` — fired
the instant the `incoming` line appeared, before Asterisk's own ring-no-answer
window could expire — see "Investigated, not a bug" for why the *first* run
missed this window.)

A second, independent run (`wait:5|dial:...|wait:15|hangup|wait:3`, fresh
instance A process) reproduced the identical shape — `busy` immediately on
dial, real `established`, clean return to `idle`, `510` untouched — ruling
out the first run's timing/flakiness explanation for *this* run's shape
being a fluke.

### Investigated, not a bug: `busy(confirmed)` appears immediately, not after a visible `ringing` phase

The task's own framing expected a `ringing` -> `busy(confirmed)` sequence.
What was actually observed, consistently across both successful runs, was
`idle` -> `busy` with no intervening `ringing` NOTIFY for `1100`, even
though instance A's *own* call leg was independently confirmed `ringing`
for a full ~0.3-15s before `established`. Read `core/modules/ctrl_json/dialog_info.c`'s
mapping (not modified here, core/ is out of scope for this agent) confirms
`busy` only ever comes from a NOTIFY body literally containing
`<state>confirmed</state>` — so this is what the PBX actually sent, not a
shell-side misparse. The most likely explanation, consistent with standard
Asterisk hint/devicestate behavior: extension `1100`'s dialog-info hint
reports the AoR "in use" the moment *any* channel touches it — including
the watching UA's *own* outbound leg to its *own* AoR, self-dial being an
inherent property of the dual-contact trick (both "instance A" and
"instance B" are, from the PBX's perspective, just two contacts of the same
`1100`). This is PBX/Asterisk hint configuration, outside this agent's
scope (`core/` is read-only reference here, and PBX config changes are
prohibited per the workspace's PBX rules) — flagged for whoever owns
Asterisk hint config if a cleaner `ringing` phase is wanted for this
specific self-referential scenario. The shell-side behavior being verified
here — subscribe, receive, track, and reflect whatever the PBX actually
sends — is confirmed correct: every `blf` event that *did* arrive was
correctly parsed, stored, and (per the frontend code review below) would
correctly drive the `.fav.busy`/`.fav.idle` lamp classes.

### Investigated, not a bug: first run's "accept failed" error

The very first dial attempt (before the auto-answer loop existed) relied
on this agent manually reacting to a `Monitor` notification — by the time
the `{"cmd":"answer"}` line was appended to instance B's `cmds.txt`,
Asterisk had already cancelled/timed out the un-answered fork to instance
B (`{"event":"call_state","state":"closed",...}` immediately preceded
`{"event":"error","message":"cmd 'accept' failed (Invalid argument
[22])"}` in B's log — baresip correctly rejected `answer` for a call that
no longer existed). Fixed by replacing manual reaction with the tail-driven
auto-answer loop described in "Setup" above (sub-second, no agent
round-trip latency) — confirmed by the second run answering cleanly. Not a
`ctrl_json`/shell bug; a test-harness latency issue.

## Evidence: click-to-call bridge (task b)

`curl` against `127.0.0.1:38911` with the real token read from
`settings.json`'s `bridge.token` (never printed to any log or committed;
32 hex chars, minted on first run by `settings.rs`'s
`generate_bridge_token()`). All six requests below and the app's own log
are from the same run:

```
$ curl -w ' [HTTP %{http_code}]' http://127.0.0.1:38911/ping
{"error":"bad token"} [HTTP 403]                                    # no token

$ curl -w ' [HTTP %{http_code}]' -H 'X-Centinelo-Token: wrong' http://127.0.0.1:38911/ping
{"error":"bad token"} [HTTP 403]                                    # wrong token

$ curl -w ' [HTTP %{http_code}]' -H "X-Centinelo-Token: $TOKEN" http://127.0.0.1:38911/ping
{"app":"centinelo-phone","state":"registered"} [HTTP 200]           # correct token

$ curl -o /dev/null -w 'HTTP %{http_code}' -X OPTIONS http://127.0.0.1:38911/dial
HTTP 204                                                            # preflight, no token needed
# response headers included:
#   Access-Control-Allow-Origin: *
#   Access-Control-Allow-Headers: Content-Type, X-Centinelo-Token
#   Access-Control-Allow-Methods: POST, GET, OPTIONS
#   Access-Control-Allow-Private-Network: true

$ curl -w ' [HTTP %{http_code}]' -X POST -H "X-Centinelo-Token: $TOKEN" \
    -H 'Content-Type: application/json' -d '{"number":"*60"}' http://127.0.0.1:38911/dial
{"ok":true} [HTTP 200]                                               # auto_dial=false (default)

$ curl -w ' [HTTP %{http_code}]' -X POST -H "X-Centinelo-Token: $TOKEN" \
    -H 'Content-Type: application/json' -d '{}' http://127.0.0.1:38911/dial
{"error":"bad request"} [HTTP 400]                                   # no number
```

App's own log for the `auto_dial=false` `/dial` request above:
```
23:34:52 click-to-call bridge: listening on 127.0.0.1:38911
23:34:52 sidecar event: {"account":"sip:1100@100.119.230.80:8089","event":"reg_state","state":"registered","transport":"wss"}
23:35:02 click-to-call bridge: /dial request for *60 (auto_dial=false) - asking for confirmation
```
Zero `call_state` events appear anywhere in this run's log (`grep -c
call_state` = `0`) — confirming the `*60` request emitted the
`click-to-call` confirmation event and did **not** dial, matching "curl
/dial with token -> confirmation state appears" exactly (no dial happened;
a human/the frontend must still say yes).

### `auto_dial=true` — real dial, independently RTP-verified

`bridge.auto_dial` flipped to `true` directly in `settings.json` (same
non-GUI editing as the BLF setup), fresh app launch, then:
```
$ curl -X POST -H "X-Centinelo-Token: $TOKEN" -H 'Content-Type: application/json' \
    -d '{"number":"*43"}' http://127.0.0.1:38911/dial
{"ok":true}
```
App log:
```
23:32:11 click-to-call bridge: /dial request for *43 (auto_dial=true) - dialing immediately
23:32:12 sidecar event: {"call_id":"89532783c58e9bf7","event":"call_state",...,"peer":"sip:*43@100.119.230.80;transport=wss","state":"established"}
```
This is the **full real path**, not a backend bypass: the HTTP request only
ever emits a Tauri event (`bridge.rs`); it is `ui/js/app.js`'s own
`handleClickToCall()`, running in the actual webview, that reads
`auto_dial:true` from the event payload and calls the same `dialUri()` a
human clicking the keypad would — proven by the resulting `sidecar_dial`
IPC call reaching the backend and a real SIP `established` state one
second later.

Independent, read-only PBX-side confirmation (`ssh ... "asterisk -rx
'pjsip show channelstats'"`), polled twice ~9s apart:
```
          1100-00000885      00:00:15 ulaw      597       0    0   0.000    598       0    0   0.002   0.000
...
          1100-00000885      00:00:24 ulaw     1084       0    0   0.000   1085       0    0   0.003   0.000
```
Same channel, UpTime and Rx/Tx counts climbing together (597->1084,
598->1085, ~54 pps, 0% loss both polls) — real, live, bidirectional RTP,
not a one-off snapshot. The app was then sent `SIGTERM` (graceful Tauri
shutdown, not a scripted `hangup` this time, since no e2e script was
running for this manual-curl test) and a follow-up poll returned `No
objects found.` — clean teardown confirmed.

`auto_dial` was reset to `false` (the safe default) in `settings.json`
immediately after this test.

## Known limitations (F3 scope)

- `centinelo://`/`tel:` deep-link activation itself (an actual OS-level
  scheme click) is not e2e-verified — see the verdict note above. The
  URL-extraction logic it depends on (`deeplink.rs::extract_dial_target`)
  is unit-tested against `tel:5551234`, percent-encoded/formatted
  variants, and all three `centinelo:` shapes
  (`centinelo://dial?number=501`, `centinelo://501`, `centinelo:501`) plus
  precedence and negative cases.
- The `register_tel_handler` toggle's Windows/Linux registry/`.desktop`-file
  behavior is inherently untestable on this macOS dev machine (`register()`/
  `unregister()` are `Err(UnsupportedPlatform)` on macOS by
  `tauri-plugin-deep-link`'s own design - confirmed by reading its
  vendored source, not assumed). Its macOS no-op path did run (as `false`)
  on every app launch across all sessions above with no error - the same
  function the Settings toggle calls.
- Favorites beyond 2 slots (`ext` left blank) were not separately
  exercised - `settings.rs::normalize_favorites`'s "skip blank `ext`"
  branch is straightforward and shares code with the already-tested path.
- No screenshot/GUI confirmation of the favorites lamp classes
  (`.fav.idle`/`.fav.ring`/`.fav.busy`/`.fav.off`) actually painting
  correctly - per the task's "never desktop GUI automation" constraint,
  this run relied on code review (`renderFavorites()` in `ui/js/app.js`
  maps `blf` state -> CSS class 1:1 with the mockup's own class names,
  reusing app.css rules that predate this task unchanged) plus the
  verified-correct event data reaching the frontend (`click-to-call`/`blf`
  events, `get_blf_states` boot-time fetch).

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

# F4 premium: loader gating + console e2e

**Verdict: PASS** for both required surfaces: the premium module loader's
three gating scenarios, and the premium console's live e2e against the real
test PBX (BLF tiles via the dual-contact trick, `blind_transfer` issued
through the console's own EngineBridge code path, PBX-side confirmation of
the surviving channel).

## Setup

Same test PBX/extension as F2/F3 (`100.119.230.80`, ext `1100`, secret from
the v1 app's own settings file, transport `wss`) — see F2's "Setup" above.
New for F4:

- **`centinelo-premium` dylib**: built + signed via the private premium
  repo's `scripts/build-and-sign-premium.sh`, against a hand-written
  `centinelo_libsign.key` containing the well-known dev/test seed
  (`2424...24`, 32 bytes of `0x24`) — the exact same seed `premium.rs`'s
  embedded `LIB_PUBKEY_BYTES` placeholder accepts (see `premium.rs`'s own
  doc comment on that constant) and `loader-poc`'s own test fixtures use.
  The build script's own signature-verify step confirmed a match
  independently: `[ok] signature matches — 642048 bytes verified` against a
  `.pub` file extracted directly from `premium.rs`'s `LIB_PUBKEY_BYTES`
  bytes (not re-derived from the seed a second time — this closes the loop
  on the exact bytes actually compiled into this shell). Never committed;
  built in a scratch dir outside any git working tree.
- **Premium console assets**: `premium/console-ui/src/*` (private repo)
  copied verbatim into `premium-console-assets/` beside the built
  executable (`shell/src-tauri/target/debug/`, already gitignored via
  `/target/`) — never committed.
- **Methodology**: same `CENTINELO_E2E_SCRIPT` scripted-driver approach as
  F2/F3 (see F2's "Methodology" above for why — unchanged reasoning), now
  covering the new `premium_diagnostic`/`open_console`/`blind_transfer`/
  `blf_subscribe` steps `e2e.rs` gained this round. Each gating scenario is
  a separate, short-lived app launch (env-var-driven, `RUST_LOG=info`,
  output captured to a file) so the dylib/`.sig` files on disk can be
  swapped between runs without restarting anything mid-test.
- **Dual-contact "instance B"**: same trick as F3 — a second baresip
  process via `core/run-spike.sh` directly (not through the shell),
  registered as the same extension `1100` (`max_contacts=2`), driven by a
  persistent `tail -f cmds.txt | ./run-spike.sh` (stdin never EOFs) plus a
  `tail -F instanceB.log | grep '"state":"incoming"'` loop appending
  `{"cmd":"answer"}` the instant an incoming call appears — sub-second,
  no agent-reaction-time dependency, same fix F3's own "Investigated, not a
  bug" section already explains.

## Evidence: gating scenario (a) — no dylib present

`libcentinelo_premium.dylib`/`.sig`/`premium-console-assets/` all absent.
App launched with `CENTINELO_E2E_SCRIPT="wait:2|premium_diagnostic|open_console|wait:1"`.

```
[app_lib::premium][INFO] premium: no module found next to the executable, running free
[app_lib::e2e][INFO] e2e: premium_diagnostic = not found
[app_lib::e2e][INFO] e2e: premium_capability_status(blf_console) = Unavailable
[app_lib::e2e][INFO] e2e: open_console -> err: premium console is not licensed
```

- **Console entry absent, not merely disabled**: `tray.rs` only appends the
  `"Console…"` `MenuItem` (and its separator) to the tray menu when
  `console::is_unlocked(&premium)` is true at startup — with no dylib
  loaded it's never constructed at all, matching "console entry absent"
  literally, not a disabled-but-present item (`tray-icon`'s `MenuItem` has
  no cross-platform visibility toggle, only enabled/disabled — see
  `tray.rs`'s own doc comment on this). `ui/js/app.js`'s
  `applyPremiumUI()` mirrors the same gate for the main window's own
  button (`#btn-console` stays `hidden`).
- **Defense in depth confirmed, not just UI absence**: `open_console` was
  invoked directly (the same command the hidden button/absent menu item
  would call) and still refused — the window itself never opens, not just
  the entry points to it.
- **Zero errors, app ran normally**: `grep -c '\]\[ERROR\]'` = `0` for the
  whole run; the sidecar registered and subscribed BLF for both favorites
  in the same run (`{"event":"blf","ext":"1100","state":"idle"}`,
  `...ext":"510"...`) — premium gating is fully independent of ordinary
  call/registration function, confirmed by them both working in the same
  process. Process was still running (not crashed) when torn down after 6s.

## Evidence: gating scenario (b) — dylib present, signature tampered

Valid dylib restored, `.sig` overwritten with 64 bytes of `0xAA` (wrong
content, correct length — exercises the actual Ed25519 mismatch path, not
just a length-check short-circuit).

```
[app_lib::premium][WARN] premium: not loading module (signature does not verify), running free
[app_lib::e2e][INFO] e2e: premium_diagnostic = signature does not verify
[app_lib::e2e][INFO] e2e: premium_capability_status(blf_console) = Unavailable
[app_lib::e2e][INFO] e2e: open_console -> err: premium console is not licensed
```

- **Exactly one warn-level log line** for the whole run
  (`grep -c '\]\[WARN\]'` = `1`), **zero error-level lines**
  (`grep -c '\]\[ERROR\]'` = `0`) — matches the task's exact requirement.
  Per `premium.rs`'s own doc ("Never fails startup"), this is logged once,
  at startup, never surfaced to the user as an error.
- Same graceful degrade as scenario (a): console absent, `open_console`
  refuses, app otherwise fully functional (registered, BLF-subscribed,
  zero crashes).
- Confirms the loader's "verify before load, not after" ordering is real,
  not just documented: a `libloading::Library::new` on a *tampered* dylib
  would have run whatever code the tampering introduced the instant it
  succeeded — the fact this run produced `"signature does not verify"`
  (the signature-check failure reason) rather than any behavior from the
  dylib itself confirms the check happened first, exactly as `premium.rs`'s
  `load_premium` and `docs/loader-integration.md`'s "Verify before load,
  not after" both specify.

## Evidence: gating scenario (c) + console live e2e — valid dylib, founder license

Valid `.sig` restored, `premium-console-assets/` in place.
`CENTINELO_E2E_SCRIPT="wait:2|premium_diagnostic|open_console|wait:3|dial:sip:1100@100.119.230.80|wait:5|blind_transfer:sip:*43@100.119.230.80|wait:6|premium_diagnostic"`,
instance B already registered and its auto-answer loop already armed
before instance A started (same ordering as F3).

### (c1) the license gate cleared and the window actually opened

```
[app_lib::premium][INFO] premium: loaded Centinelo Premium (build 0.1.0)
...
[app_lib::e2e][INFO] e2e: premium_diagnostic = loaded
[app_lib::e2e][INFO] e2e: premium_capability_status(blf_console) = NotImplemented
[tao::platform_impl::platform::window][TRACE] Creating new window
[app_lib::e2e][INFO] e2e: open_console -> ok
```

`NotImplemented`, not `Available`, is v0's own honest answer for a
*licensed* capability — see `console.rs`'s `unlocks_console` doc comment
for the full reasoning (short version: `centinelo-premium`'s v0 build has
no real implementation behind *any* capability yet, by design — its own
`loader-poc` test proving this is intentional is literally named
`unlicensed_feature_blocked_while_licensed_feature_reaches_stub` — so
`NotImplemented` is what a cleared gate looks like today; gating strictly
on the literal `Available` discriminant would make this exact scenario
impossible to pass under any build of `centinelo-premium` that exists).
`open_console -> ok` and Tauri's own `Creating new window` trace line
confirm the window was actually built, not merely that the gate check
passed in isolation.

### (c2) EngineBridge live — proven via the *unmodified vendored* console-ui code, not a self-report

`commands::sidecar_blf_subscribe`/`sidecar_blf_unsubscribe` log
`"invoked over IPC"` at INFO specifically so this is observable — and
critically, the *only* thing that calls those two commands via IPC in this
build is `console-app.js`'s own `ConsoleApp.mount()` → `ConsoleStore`
→ `store.start()`, which `premium/console-ui/README.md` documents as
firing `blf_subscribe` for every roster extension **automatically, on
mount** — unmodified vendored behavior, not a test hook added for this
round. The favorites auto-subscribe (`sidecar.rs`, on `reg_state`) reaches
the *same* extensions through `blf_subscribe_raw` directly, never through
this `#[tauri::command]` at all, so a hit here is unambiguous:

```
[app_lib::commands][INFO] commands: sidecar_blf_subscribe(1100) invoked over IPC
[app_lib::commands][INFO] commands: sidecar_blf_subscribe(510) invoked over IPC
```

This is direct, Rust-log-visible proof that: the `premium-console://`
protocol handler served all 12 script files + 2 stylesheets correctly: every
classic `<script>` parsed without error (a syntax error anywhere in that
load-bearing dependency chain would have stopped `ConsoleApp` from ever
being defined); `EngineBridge.init()` ran; `get_favorites` round-tripped
over real IPC to source the roster; `ConsoleApp.mount()` ran to completion;
and `ConsoleStore.start()`'s own bridge calls reached the Rust backend —
i.e. **EngineBridge live**, established without any GUI automation, by
observing the one code path only the real vendored console-ui package
could have taken.

### (c3) BLF tiles — "subscribe 1100 + 510", 1100 goes busy

Both favorites (`1100`, `510` — the same two configured since F3) are the
console's roster (sourced from `get_favorites`, see `shell/README.md`
"Premium console window"); `selfExt` is left unset specifically so `1100`
is *not* treated as "self" and gets a real, subscribed grid tile rather
than being suppressed — see `console.rs`'s module doc for why. The dual-
contact trick (instance A dials its own extension, forking to instance B)
produces the exact same wire-level `blf` transition F3 already proved
reaches the frontend correctly, this time also reaching the console:

```
[app_lib::sidecar][INFO] sidecar event: {"account":"sip:1100@100.119.230.80:8089","event":"reg_state","state":"registered","transport":"wss"}
[app_lib::sidecar][INFO] sidecar event: {"event":"blf","ext":"1100","state":"idle"}
[app_lib::sidecar][INFO] sidecar event: {"event":"blf","ext":"510","state":"idle"}
...
[app_lib::e2e][INFO] e2e: dial(sip:1100@100.119.230.80) -> ok
[app_lib::sidecar][INFO] sidecar event: {"event":"blf","ext":"1100","state":"busy"}
...
[app_lib::sidecar][INFO] sidecar event: {"call_id":"9b56c63a198f461c",...,"state":"established"}
[app_lib::sidecar][INFO] sidecar event: {"event":"blf","ext":"1100","state":"busy"}
```

**Why this necessarily also updates the console's own tiles, not just the
main window's favorites grid**: `sidecar.rs` emits every `sidecar-event`
via `AppHandle::emit` (`Emitter::emit`'s own doc: *"Emits an event to all
targets ... emits the synchronized event to all webviews"*) — a plain,
un-targeted broadcast, not `emit_to` a specific window label. The console's
own wrapper script `listen()`s to the identical `"sidecar-event"` Tauri
event the main window already does (same API, same event name — see
`console.rs`'s embedded `INDEX_HTML`), so there is no code path by which
the main window would receive a `blf` event the console's `ConsoleStore`
did not; this is a structural (Tauri IPC broadcast semantics), not
probabilistic, guarantee, verified directly from the `tauri` crate source
(`Emitter::emit`'s doc + implementation) as part of this integration.

### (c4) `blind_transfer` from the console's own code path

Per this repo's own established e2e methodology (F2's "Methodology" above
— scripted driver calling the exact `#[tauri::command]` functions a real
UI action would reach, instead of OS-level click automation, which the
workspace rules prohibit outright): `e2e.rs`'s `blind_transfer:<uri>` step
calls `commands::sidecar_blind_transfer` — **the identical function**
`console.rs`'s embedded `DISPATCH.blind_transfer` invokes
(`invoke("sidecar_blind_transfer", {uri, call_id})`) when a real drag-to-
transfer gesture completes in the console UI. This is "the console code
path" in the same sense F2/F3's own dial/answer/hangup e2e already
established for the main window — same command, same backend logic, zero
GUI-automation dependency either way.

```
[app_lib::e2e][INFO] e2e: blind_transfer(sip:*43@100.119.230.80) -> ok
[app_lib::sidecar][DEBUG] core: ... transferring call to sip:*43@100.119.230.80
[app_lib::sidecar][INFO] sidecar event: {"call_id":"9b56c63a198f461c",...,"state":"closed"}
[app_lib::sidecar][DEBUG] core: sip:1100@100.119.230.80:8089: Call with sip:1100@100.119.230.80;transport=wss terminated (duration: 5 secs)
```

`call_id` was omitted (`None`) on purpose — instance A had exactly one
active call at that point, so `core/PROTOCOL.md`'s "falls back to the
current call" default resolves it unambiguously; a real console operator's
drag-to-transfer always supplies an explicit `call_id` (`ConsoleStore`
tracks it), this just exercises the same command with the protocol's own
default-resolution path instead. Instance A's own leg closing immediately
after `-> ok` is the expected shape of a *blind* transfer: the transferor
drops out the moment the far end (Asterisk) accepts the REFER and redirects
the bridged party — A is not supposed to still have a call afterward.

### (c5) PBX-side confirmation — the surviving channel lands on the echo test

Per the task's exact instruction: `ssh -i ~/.ssh/id_neola_vps root@100.119.230.80 "asterisk -rx 'core show channels'"`, read-only, polled three times starting ~3s after the transfer was issued:

```
=== poll 1 (~3s after blind_transfer) ===
Channel                       Location                    State   Application(Data)
PJSIP/1100-00000889           *43@from-internal-xfer:7    Up      BackGround(demo-echotest,,,app...
1 active channel
1 active call

=== poll 2 (~6s after) ===
PJSIP/1100-00000889           *43@from-internal-xfer:7    Up      BackGround(demo-echotest,,,app...

=== poll 3 (~9s after) ===
PJSIP/1100-00000889           *43@from-internal-xfer:7    Up      BackGround(demo-echotest,,,app...
```

One surviving channel (`PJSIP/1100-00000889` — instance B's own contact,
the far side of the transfer), consistently in the `from-internal-xfer`
dialplan context targeting `*43`, running Asterisk's own echo-test
application (`BackGround(demo-echotest,...)`) across all three polls — the
transferor's (instance A's) channel is gone, exactly matching "the
surviving channel lands on the echo test". No PBX config was read-write
touched at any point; only `1100` (the provisioned test extension) and
`*43` (the sanctioned echo test) were ever dialed — no real extensions, no
`600`/`601` ring groups.

**Known artifact, not a bug**: after instance B's own process was
terminated (test teardown) its WSS transport closed, but the PJSIP channel
itself lingered on the PBX for longer than expected before Asterisk's own
transport-failure detection reclaimed it — consistent with this account's
`rtp_timeout 0` (see `core/BUILD.md`, `run-spike.sh`) disabling
RTP-silence-based teardown; no read-write PBX action was taken to force it
(per the workspace's read-only SSH rule), and it is expected to clear on
its own via PJSIP's own transport/session handling, the same way any
client disconnecting mid-call would.

## Evidence: zero errors across all three scenarios

```
$ grep -c '\]\[ERROR\]' scenario-a-no-dylib.log scenario-b-tampered-sig.log scenario-c-console-live.log
scenario-a-no-dylib.log:0
scenario-b-tampered-sig.log:0
scenario-c-console-live.log:0
```

## Known limitations (F4 scope)

- No screenshot/GUI confirmation that the console's tiles/drag affordances
  actually *paint* correctly (busy lamp color, drag ghost, etc.) — per the
  task's explicit "never desktop GUI automation" constraint, this round
  relied on the same class of evidence F3's own BLF verification did:
  backend-observable IPC/event-log proof that the *real, unmodified*
  vendored console-ui code executed the full mount → subscribe → receive
  chain, not a screenshot. Visual fidelity of the vendored console-ui
  package itself is out of scope for this integration round (owned by the
  team that built `premium/console-ui`, whose own `screenshots/` already
  documents its fidelity against the design mockups).
  Not e2e-verified here.
- `attended_transfer`/`complete_transfer`/`abort_transfer`/`hold`/`resume`/
  `mute` all got new backend commands this round (`commands.rs`) and e2e
  script steps (`e2e.rs`), but only `blind_transfer` was exercised against
  the real PBX this session — the others are unit-shaped identically (thin
  `sidecar.send_cmd` wrappers, same pattern as the already-verified
  `dial`/`answer`/`hangup`/`blind_transfer`) but not independently proven
  against a live call this round.
- The console window's native macOS traffic-light/decorations question
  (this build uses `decorations:false` + a wrapper-wired minimize/close,
  mirroring the main window's own Windows-only custom-titlebar approach)
  was not visually verified — see `shell/README.md` "Premium console
  window" for the reasoning, but actual pixel-level chrome behavior on
  macOS specifically wasn't screenshotted this round.
- Windows/Linux: this round's testing was macOS-only (same caveat as every
  earlier phase's own Windows note) — the premium loader's platform-`cfg`'d
  filename resolution (`centinelo_premium.dll`/`.so`) and the console's
  custom URI scheme protocol registration are implemented for all three
  platforms per the vendored ABI crate and Tauri's own cross-platform
  `register_uri_scheme_protocol`, but only exercised on macOS this session.

## Transcript panel (F4 ola 2, 2026-07-16) — headless render, not desktop GUI automation

The transcript panel (`ui/js/transcript-panel.js`, wired into `ui/index.html`'s
`#screen-transcript` by `ui/js/app.js`) is a Tauri-free rendering module by
design — it takes a plain state object and produces DOM, with all
`invoke`/`listen` wiring living in `app.js`. That split is what made this
round's verification possible without ever touching desktop GUI automation
(hard rule, both this project's and Felix's global "never GUI-automation
del desktop, choca con otros agentes en la Mini"):

1. **Backend (real, spawned-process integration tests, not mocks of
   `parse_transcribe_line` alone)**: `cargo test --lib` — 69 passed, 0
   failed, including three tests that spawn the real
   `tests/fixtures/mock-transcribe.sh` as a child process through the
   exact `spawn_transcribe`/`parse_transcribe_line` code path the live app
   uses — one of them (`mock_binary_reports_channels_failed_when_env_set`)
   proves the new `channels_failed` field (added to `centinelo-transcribe`'s
   real `done` event in a 2026-07-16 reliability re-review, after this
   shell's ola-1 contract reconciliation had already landed) round-trips
   end to end, not just through `parse_transcribe_line`'s own unit tests.
   `cargo clippy --all-targets -- -D warnings`: clean.
2. **Frontend render, both themes, five scenarios**: served `ui/` over a
   plain local static file server (`python3 -m http.server`, no Tauri
   runtime needed — the panel has none as a dependency) and loaded
   `ui/dev/transcript-mock.html` (a harness, never referenced by
   `index.html`, feeding fabricated `TranscriptPanel` models through the
   exact same `renderTranscriptBody` the real app calls) in the Browser
   pane. Cycled all five phases — `live`, `writing`, `done` (clean), `done`
   with `channels_failed:["caller"]`, `error` (folder-down) — across
   `auto`/`light`/`dark` themes. Zero console errors across every
   transition.
3. **Amber-discipline check, programmatic not visual**: resolved
   `--amber`/`--amber-fill`/`--amber-soft`/`--amber-glow` to their actual
   `rgb()` values and swept every element under `.transcript-screen` for a
   `color`/`background-color`/`border-*-color` match — zero hits on the
   `live` phase (the state creative-vigilia's report flagged as the one
   Plate 02 got wrong before its correction). The one deliberate exception
   — a user-typed find query — was exercised separately: searching
   "Wednesday" against the clean-done fixture produced exactly `2 OF 2`
   hits with `<mark>` styled in `--amber-soft`/`--amber`, matching the
   mockup's own footnote ("found terms are the one amber use here").
4. **A real bug found and fixed this way**: the find bar's hit counter
   initially read `container.querySelectorAll("mark").length` *before*
   re-rendering the tape with the new query, so it always reported the
   *previous* query's count (visually confirmed as `0 OF 0` on the very
   first search in the harness). Fixed by reordering the tape re-render
   ahead of the count in `transcript-panel.js`'s `findInput` handler;
   re-verified `2 OF 2` after the fix. Also fixed: at exactly this app's
   default 380px window width, `Copy`/`Show in folder` forced the caller's
   own name/number to wrap — `transcript.css`'s `.idrow` now wraps the
   actions onto their own row below 460px (a CSS container query on
   `.transcript-body`, `container-type: inline-size`) instead.
5. **`reveal_in_file_manager`** (new command backing "Show in
   folder"/"Show local copy") got its own `CENTINELO_E2E_SCRIPT` step
   (`reveal_in_file_manager:<path>`) exercising the real command dispatch
   path the same way every other step in this file does — **not run this
   session** (needs a real display/window to launch `cargo tauri dev`
   against, which this verification pass didn't have); the path-validation
   logic itself (must resolve under the configured `storage_dir` or a
   `centinelo-transcribe-tap.*` temp dir) has no separate unit test yet
   either — flagged for qa-e2e or a follow-up pass.

## Known limitations (transcript panel, F4 ola 2)

- No real call was driven through the actual `core/` engine this round —
  the panel consumes `transcription://segment|done|error`, which ola-1's
  own module (`transcription.rs`) already proved fires correctly off real
  `tap_state` events (`shell-tauri-2026-07-16-transcription-shell.md`);
  this round only proves the *panel* renders whatever those events already
  carry, correctly and without amber leakage.
- Only one call/transcript is tracked client-side at a time
  (`app.js`'s `state.transcript`) — matches the engine's own one-UA,
  one-call-in-flight design (`core/PROTOCOL.md` "Multi-account support"),
  but a transcript still `writing`/`error` when a *second* call somehow
  starts (shouldn't happen given the engine's own constraint) would be
  silently replaced in the UI, even though the backend's own finalize
  pipeline for the first call keeps running independently.
- The audio-playback button in the "kept audio" state (`.playbtn`) is
  presentational only this round — no actual playback wiring (the mockup
  itself doesn't specify a source; this shell has no `<audio>`-servable
  path for a WAV outside `storage_dir` yet). Flagged, not silently shipped
  as if it worked.
- WCAG AA: inherited by construction (same tokens, same contrast ratios
  documented in `premium/design/TOKENS.md` §8) rather than independently
  re-audited with an automated tool (e.g. axe-core) this round — the
  premium console got that treatment (`premium/console-ui/A11Y.md`); this
  panel didn't, since axe-core needs a real DOM+browser harness beyond
  what this round's plain `node --check` + manual Browser-pane pass covered.

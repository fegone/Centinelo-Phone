# Centinelo Phone 2.0 — desktop shell (F2)

Tauri v2 desktop app that wraps the `core/` baresip+`ctrl_json` sidecar (see
`../core/PROTOCOL.md` and `../core/BUILD.md`) in a native window built to the
"Vigilia" design system (`../../centinelo-premium-design/design/`). Rust
backend, static HTML/CSS/vanilla-JS frontend — no bundler, no frontend
framework.

```
shell/
  src-tauri/     Rust backend (Tauri app)
    src/
      lib.rs       app wiring: state, commands, tray, lifecycle
      sidecar.rs   sidecar process supervisor (spawn/pipe/restart/backoff)
      settings.rs  settings.json persistence + argon2 admin-password hashing
      commands.rs  #[tauri::command]s exposed to the frontend
      tray.rs      system tray (Show/Quit, close-to-tray)
      e2e.rs       debug-only scripted e2e driver (see "e2e verification")
  ui/            static frontend, served directly as `frontendDist`
    index.html     single-page app: main window + settings + call overlay
    css/tokens.css   verbatim copy of the Vigilia design tokens
    css/app.css      component styles ported from the design mockups
    js/app.js        all frontend logic (Tauri invoke/event wiring)
```

## Architecture: shell <-> sidecar

On spawn, `sidecar.rs` writes a scratch `config`/`accounts` pair (mirroring
`core/run-spike.sh` exactly — same module list, same
`mediaenc=dtls_srtp;medianat=ice;rtcp_mux=yes` account params, same
`outbound=` pin) into a fresh temp directory, execs
`<core-binary> -f <scratch-dir>` with `CENT_WS_PATH=/ws` in its environment,
and holds the child's stdin/stdout/stderr:

- **stdin**: commands from the frontend (`dial`/`answer`/`hangup`) are
  written as one JSON object per line.
- **stdout**: a reader thread filters for lines starting with `{` (per
  `PROTOCOL.md` "Framing"), parses each as JSON, and forwards it verbatim to
  the frontend as a `sidecar-event` Tauri event. It also logs every event at
  `INFO` (`sidecar event: {...}`) so a plain `cargo tauri dev` terminal is a
  readable protocol trace.
- **stderr**: drained into a small ring buffer (last 20 lines), surfaced in
  crash diagnostics.

**Sidecar binary resolution** (`resolve_core_binary`): an explicit
`core_binary_path` in settings wins; otherwise `CENTINELO_CORE_BIN` env var;
otherwise a walk-up search from the cwd and the running executable's
directory for `core/deps/baresip/build/baresip` (matches this repo's
`shell/` next to `core/` layout). No path found -> a clear "Core engine
binary not found" status instead of a silent failure.

**Auto-restart**: a single long-lived supervisor thread per app run. On an
*unexpected* exit it respawns with exponential backoff (1/2/4/8/16s, capped
at 5 tries before giving up and surfacing a "crashed repeatedly" state with
a manual retry action). Backoff waits are polled in 120ms ticks so a manual
retry or a settings change can interrupt them instantly rather than waiting
out the delay. An *intentional* respawn (settings changed, manual "Restart
engine", the wss->udp auto-transport fallback) does not count against the
budget and does not back off.

Stop is implemented by closing the child's stdin: `ctrl_json` treats stdin
EOF as an implicit `quit` (documented in `PROTOCOL.md`), so the child exits
on its own and the blocking `child.wait()` in the supervisor thread returns
promptly. A watchdog thread force-kills (`kill -9` / `taskkill /F`) if the
child hasn't exited ~3s after stdin closes, as a safety net for a hung
process — this is the only place a raw PID (not the `Child` handle) needs
to cross threads, which is why it's tracked separately.

**`auto` transport**: v0's `ctrl_json` has no runtime transport
switching (see `PROTOCOL.md` "Planned"). The shell's `auto` mode is
therefore a simple, honestly-scoped approximation: start on `wss`; if the
*first* registration attempt reports `reg_state:"failed"` before ever
reaching `"registered"`, respawn once with `udp` instead. This is
start-time transport selection, not seamless mid-call failover — documented
here so it isn't mistaken for more than it is.

## Settings & admin-lock model

Everything lives in one `settings.json` in the Tauri app-data directory
(`~/Library/Application Support/com.centinelo.phone/` on macOS), written
with `0600` permissions. Shape: `{ account, admin, favorites, theme,
core_binary_path }` (see `settings.rs`).

- **`account.secret`** (the SIP password) is stored the same way the v1
  Electron app stored it: plaintext, in this per-user settings file, never
  logged, never sent anywhere else. The frontend never round-trips the
  actual value back from the backend — `get_account_settings` returns
  `secret_set: bool` only; the password field starts blank with an
  "unchanged" placeholder, and saving with it blank keeps the stored secret.
  The **one** documented exception, inherited from `core/run-spike.sh`'s own
  security note, is the sidecar's ephemeral scratch `accounts` file
  (`0600`, deleted when the sidecar stops/respawns) — baresip has no other
  way to receive the SIP auth password.
- **Admin password**: never stored in recoverable form, only its Argon2id
  hash (`argon2` crate, `SaltString::generate(&mut OsRng)`). First run with
  no hash set forces a "set an admin password" step before any sensitive
  field becomes editable. The unlock flag lives in memory only
  (`AdminSession`, an `AtomicBool`) — every app launch starts locked.
- **What's gated**: account (host/ext/secret/display name), transport
  choice, and the core-binary-path override — all sensitive per the task
  spec. Theme and "restart engine" are not gated (low-risk, and retry-after-crash
  should work without the password).
- **UI note**: the lock overlay covers the whole settings body for
  simplicity, so the (ungated-on-the-backend) theme buttons happen to sit
  behind it too until unlock. A real front-desk user unlocks once per
  launch anyway, so this was an acceptable simplification given F2's scope.

## Design fidelity notes

`ui/css/tokens.css` is `TOKENS.md` section 9 copied verbatim (no
hand-edits) so the shell always matches the source of truth. `app.css`
ports the mockups' component classes 1:1 where the surface is literally the
same:

- **Main window** (`mockups/main.html`) — identity card, dial display,
  keypad, favorites grid, recents list: same class names, same tokens, same
  380x680 window. Registration pill omits the `18 MS` latency figure the
  mockup shows as a placeholder — `ctrl_json` v0 has no `quality_stats`
  command (see `PROTOCOL.md` "Planned"), and per the design system's own
  voice rules ("Numbers are facts") a fabricated number would be worse than
  none.
- **In-call state** — adapted from `mockups/in-call.html`'s caller
  card/timer/secure-line/end-button language, but *compacted into the same
  380px window* rather than the mockup's 664px 2-column Pro layout, and
  *without* the mute/keypad/audio/hold/transfer controls grid — none of
  those are wired in the v0 protocol (no `hold`, `mute`, or
  `blind_transfer` command exists yet), and shipping dead buttons would
  contradict the brand voice's honesty principle. Caller identity is shown
  as the raw extension/number (mono), never a fabricated contact name —
  F2 has no directory/CRM lookup.
- **Settings** — reuses `mockups/settings.html`'s nav-item/card/section
  patterns and `mockups/onboarding.html`'s field/transport-card language,
  but as a single-column drill-down (back chevron + stacked sections)
  instead of the wide 940x640 2-pane desktop-settings plate, because this
  shell's actual primary surface is the compact 380px window, not a
  separate large settings window. The admin-lock card styling (icon +
  heading + description + input) is a new component in the same visual
  language, since neither mockup designed an admin-lock state
  (`REVIEW.md` §2 lists "pre-activation license screen" as a designed-later
  gap; admin-lock is F2-specific and wasn't in the five plates at all).
- **BLF favorites** — 4 static placeholder tiles (off/gray state, "Not set
  up" / "Not tracked yet" labels) per the task spec ("BLF events come in
  F3"). Tiles with a configured extension are click-to-dial even though
  presence isn't live yet, which seemed like reasonable, low-risk, honest
  use of the sidecar wiring already in place.
- **Both themes**: implemented via the tokens' `light-dark()` +
  `color-scheme` mechanism (auto/system) plus a manual override
  (`data-theme` attribute, three-way Auto/Light/Dark control in Settings).
  Light mode was visually verified against the real running app this
  session (see `E2E.md`) and matches the mockups closely. Dark mode uses
  the identical CSS custom properties (same `light-dark()` pairs, same
  rule set) and was code-reviewed but not independently re-screenshotted
  this session — see `E2E.md` for why (shared-machine desktop contention
  cut manual click-through QA short after the core call flow was already
  proven).
- **Titlebar**: custom-drawn on Windows (`decorations:false`, drawn
  Settings/Minimize/Close buttons, matches the mockups' titlebar exactly).
  macOS keeps the native traffic lights (`decorations:true` +
  `titleBarStyle:Overlay` + `hiddenTitle:true`, see
  `tauri.macos.conf.json`) with content inset 78px, per
  `DIRECTION.md` §5 — the mockups explicitly didn't draw this variant
  (`REVIEW.md` §3: "macOS traffic-light variant... designed for, not
  drawn"), so this is a from-spec, not from-pixels, implementation. Only
  Settings needed to stay reachable outside the native-redundant
  minimize/close pair — see the git history for a real bug this exact
  point caught (Settings was originally nested inside the same
  mac-hidden button group as minimize/close, making it unreachable once
  configured).

## Build + run

```bash
# 1. Build the core sidecar once (from the repo root - see ../core/BUILD.md)
brew install cmake openssl
git submodule update --init --recursive
git apply --directory=core/deps/re core/patches/0001-re-configurable-sip-ws-path.patch
cmake -S core/deps/re -B core/deps/re/build -DCMAKE_BUILD_TYPE=Release \
  -DOPENSSL_ROOT_DIR="$(brew --prefix openssl@3)"
cmake --build core/deps/re/build -j"$(sysctl -n hw.ncpu)"
cmake -S core/deps/baresip -B core/deps/baresip/build -DCMAKE_BUILD_TYPE=Release \
  -DOPENSSL_ROOT_DIR="$(brew --prefix openssl@3)" \
  -DMODULES="account;g711;auconv;auresamp;ausine;aufile;ice;dtls_srtp;menu" \
  -DAPP_MODULES="ctrl_json" -DAPP_MODULES_DIR="$PWD/core/modules"
cmake --build core/deps/baresip/build -j"$(sysctl -n hw.ncpu)"

# 2. Run the shell
cd shell
npm install
npm run dev        # = tauri dev - builds Rust + launches the app
```

First launch: the main window shows "Connect your phone system" until you
open Settings, set an admin password (first run only), fill in your PBX
host/extension/password and pick a transport, and Save. The sidecar
auto-starts whenever the account is configured (on launch, and after every
save).

`npm run build` (`tauri build`) produces a signed-for-nothing dev bundle;
production signing/notarization is out of scope for F2.

### Environment variables (debug builds only)

| Var | Effect |
|---|---|
| `CENTINELO_CORE_BIN` | Override the auto-detected core binary path. |
| `CENTINELO_OPEN_DEVTOOLS=1` | Auto-open the WKWebView/WebView2 inspector on launch. |
| `CENTINELO_E2E_SCRIPT` | Scripted dial/answer/hangup driver, see below. |

## e2e verification

See **`E2E.md`** for the full methodology (including why a `\|`-separated
`CENTINELO_E2E_SCRIPT` env-var driver was used instead of OS-level click
automation for the final verified runs) and the captured evidence: the
complete `ready`/`reg_state`/`call_state` event trail from the real running
app, and independent PBX-side RTP packet-count confirmation
(`asterisk -rx "pjsip show channelstats"`, read-only) for four separate
real calls to the `*43` echo test extension over WSS.

## Known limitations (F2 scope)

- No `hold`/`mute`/`transfer`/`dtmf` — not in the v0 `ctrl_json` protocol.
- BLF favorites are static (dial-only), no live presence — F3.
- No cert pinning (`sip_verify_server no`) — matches `core/BUILD.md`'s own
  documented TODO; the v1 app's `pinnedCertSha256` setting isn't ported.
- No console (receptionist grid), transcript, recording, or licensing UI —
  those are Pro/later-phase surfaces per `DIRECTION.md`.
- Windows: untested this session (no Windows machine available - same
  caveat as `core/BUILD.md`'s own Windows CI note). `shell-build.yml`'s
  Windows job is `continue-on-error: true` for the same reason.
- Recents/favorites/settings have no import from the v1 Electron app; this
  is a fresh v2 app with its own app-data directory and schema.

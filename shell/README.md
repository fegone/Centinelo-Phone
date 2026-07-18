# Centinelo Phone 2.0 — desktop shell (F2/F3)

Tauri v2 desktop app that wraps the `core/` baresip+`ctrl_json` sidecar (see
`../core/PROTOCOL.md` and `../core/BUILD.md`) in a native window built to the
"Vigilia" design system (`../../centinelo-premium-design/design/`). Rust
backend, static HTML/CSS/vanilla-JS frontend — no bundler, no frontend
framework.

```
shell/
  src-tauri/     Rust backend (Tauri app)
    centinelo-premium-abi/  vendored verbatim from the private premium repo
                             (see "Premium module loader" below) - the C ABI
                             contract only, no secrets, no feature logic
    src/
      lib.rs       app wiring: state, commands, tray, lifecycle, plugins
      sidecar.rs   sidecar process supervisor (spawn/pipe/restart/backoff)
      settings.rs  settings.json persistence + argon2 admin-password hashing
      commands.rs  #[tauri::command]s exposed to the frontend
      tray.rs      system tray (Show/Quit, close-to-tray, gated Console…)
      bridge.rs    click-to-call localhost HTTP bridge (F3)
      deeplink.rs  centinelo:// / tel: deep-link handling (F3)
      premium.rs   premium module loader (F4, see "Premium module loader")
      console.rs   premium console window (F4, see "Premium console window")
      transcription.rs  local transcription orchestration (F4, tap->transcribe->save)
      updater.rs   auto-updater download/install commands - the has_active_call() gate (see "Auto-updater")
      e2e.rs       debug-only scripted e2e driver (see "e2e verification")
  ui/            static frontend, served directly as `frontendDist` (bundled into every build - see "dev/" below for why the mock harness deliberately lives OUTSIDE this directory)
    index.html     single-page app: main window + settings + call overlay + transcript panel
    css/tokens.css   verbatim copy of the Vigilia design tokens
    css/app.css      component styles ported from the design mockups
    css/transcript.css  transcript panel styles (F4 ola 2, see "Transcript panel")
    js/app.js        all frontend logic (Tauri invoke/event wiring)
    js/transcript-panel.js  transcript panel rendering (pure - no Tauri dependency)
    js/transcript-panel.test.js  node:test coverage for the above (`npm test`)
    js/updater.js    auto-updater state machine + rendering (pure - no Tauri dependency, see "Auto-updater")
    js/updater.test.js  node:test coverage for the above (`npm test`)
  dev/           dev-only tooling, NEVER under ui/ so `frontendDist` never bundles it
    transcript-mock.html  standalone harness for screenshot verification (see "Transcript panel")
    updater-mock.html  standalone harness for screenshot verification (see "Auto-updater")
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

## Premium module loader (F4)

`premium.rs` looks for `centinelo_premium` directly beside this executable
at startup (`centinelo_premium_abi::expected_library_path`), verifies its
Ed25519 side-car `.sig` **before** ever calling `libloading::Library::new`
(so a tampered file's code never executes, not even briefly), and loads it
if the signature checks out. Missing, corrupt, or tampered all degrade to
ordinary free-mode operation - this can never fail app startup. Full design
writeup (threat model, why a dylib not a static link, why side-car not
appended bytes): `premium/docs/loader-integration.md` in the private
premium repo.

- **`centinelo-premium-abi/`** (vendored here) is the wire contract only -
  zero dependencies, no secrets, no feature logic. A Community build
  (`git clone` + `cargo build`, no private repo involved) compiles the exact
  same loader path as an official Pro build; the only difference is whether
  the installer step below happened to drop a dylib next to the executable.
  Re-syncing it when the private repo's copy changes is a plain
  `cp -r .../centinelo-premium-abi shell/src-tauri/centinelo-premium-abi`
  (see that crate's own doc comment, "Extending this enum"/"struct" for why
  changes there are additive, nothing to hand-merge).
- **Where the license check actually happens**: never in this shell. This
  file only ever asks the loaded dylib "what's the status of capability N"
  and relays the answer - `centinelo-license` never appears as a dependency
  here, not even transitively (see `premium.rs`'s own module doc for the
  full reasoning, and `centinelo-premium-abi`'s crate doc, "Why the split is
  a dylib").
- **Dev/test key**: `premium.rs`'s embedded `LIB_PUBKEY_BYTES` is a
  known, non-secret placeholder (`SigningKey::from_bytes(&[0x24; 32])`) so a
  dylib built and signed against that same well-known seed loads during
  development without Felix's real offline key. Before an official release:
  generate a real keypair with `premium-sign keygen` (private premium repo,
  offline, never in CI), replace `LIB_PUBKEY_BYTES` with the real public
  half, and re-sign the shipped dylib with the matching private half.
  Until that swap happens, an official installer built from this repo only
  loads a dylib signed by the well-known dev key - a safe default (free
  mode), not a broken one.
- **Building + signing a dylib for local testing**: from the private
  premium repo, `scripts/build-and-sign-premium.sh --key <path to a
  centinelo_libsign.key file containing the 64-hex-char dev seed
  `2424...24`>` produces `target/release/libcentinelo_premium.dylib(.sig)`
  (macOS naming; see that crate's `expected_library_filename` for
  Windows/Linux). Copy both next to this app's own built executable
  (`shell/src-tauri/target/debug/` for a plain `cargo build`/`cargo run`,
  or the `.app` bundle's `Contents/MacOS/` for a `tauri build`).

## Premium console window (F4)

The receptionist BLF console (`premium/console-ui` in the private repo,
drag-to-transfer grid, live tiles) opens as its own Tauri window
(`console.rs`), from the tray menu's "Console…" item or a titlebar button
next to Settings - **both only ever appear when the premium license gate
clears** (`premium_capability_status("blf_console")` resolves to
`available` or `not_implemented` - see `console.rs`'s `unlocks_console` doc
for why `not_implemented` counts too: `centinelo-premium`'s v0 build has no
real implementation behind *any* capability yet, by design, so a licensed
capability landing on `not_implemented` rather than `available` is what a
cleared gate looks like today - not a reason to hide a feature this shell
itself implements). `commands::open_console` re-checks the gate itself too,
independent of whether either button happened to be visible.

- **Premium UI assets never ship in this public repo.** `console.rs`
  registers a custom `premium-console://` URI scheme (not the bundled
  `frontendDist`) that serves a small, wholly-generic wrapper page
  (embedded directly in `console.rs` - zero console-ui-specific content,
  just `<script src>` tags in `premium/console-ui/dev/mock.html`'s own
  documented dependency order, plus the EngineBridge wiring
  `premium/console-ui/README.md` describes as "Option B": one Tauri command
  per protocol verb, matching this file's existing dial/answer/hangup
  convention rather than a single generic passthrough - see `commands.rs`'s
  `sidecar_hold`/`sidecar_blind_transfer`/etc.) plus every other file
  (`tokens.css`, `console.css`, `components/*.js`, `store/ConsoleStore.js`,
  `bridge/EngineBridge.js`, `console-app.js`) read live from a directory
  *beside the running executable* - the same "next to the exe" convention
  the dylib itself uses, so one packaging step drops both in the same
  place.
- **Populating that directory for local testing**: copy
  `premium/console-ui/src/*` (private repo) verbatim, preserving its
  internal `components/`/`store/`/`bridge/` structure, into
  `<exe dir>/premium-console-assets/` (or point
  `CENTINELO_PREMIUM_ASSETS_DIR` at wherever you put it instead - same
  override shape as `CENTINELO_CORE_BIN`). Nothing under this directory is
  ever committed to this repo; `target/` is already gitignored, which is
  where a plain `cargo build`'s executable (and therefore this directory)
  lives during development.
- **Official installer layout** (not yet built - out of scope this round,
  F5 in the product spec): a post-`tauri build` packaging step, owned by the
  private premium repo's release tooling, copies the signed dylib + `.sig`
  (see "Premium module loader" above) *and* a `premium-console-assets/`
  tree built from `premium/console-ui/src/*` into the bundle's output
  directory - macOS: `Centinelo Phone.app/Contents/MacOS/` (beside
  `centinelo-shell`); Windows: the install directory, beside
  `centinelo-shell.exe`.
- **Roster**: sourced from the operator's own configured favorites
  (`commands::get_favorites`) - the only extension directory this shell has
  (no CRM/directory lookup yet, same limitation the main window's favorites
  grid already has). Honest, not fabricated: every tile the console shows
  is a real, user-configured extension. `selfExt` is left unset so a
  favorite that happens to match the operator's own account extension still
  gets a live, subscribed BLF tile in the grid (rather than being
  suppressed as "self") - the "Your call" panel's own state comes from
  `call_state` events regardless.
- **`blf_subscribe`/`blf_unsubscribe` are idempotent** (`SidecarHandle`,
  `sidecar.rs`): the favorites auto-subscribe (on registration) and the
  console's own subscribe-on-mount can legitimately both want the same
  extension watched - `core/PROTOCOL.md`'s `blf_subscribe` errors on a
  literal duplicate, so this shell now tracks which extensions are already
  watched and no-ops a repeat request instead of forwarding it to the wire.
- **Window chrome**: `decorations: false`, native-titlebar-free - the
  console-ui package renders its own titlebar (minimize/close glyphs,
  intentionally inert in the vendored package, "Wired by the embedding
  shell" per its own doc) which this shell's wrapper script wires to the
  real window (`getCurrentWindow().minimize()/close()`) plus dragging (no
  `data-tauri-drag-region` attribute is possible on console-ui's
  JS-constructed DOM, so dragging goes through the explicit
  `startDragging()` API instead).

## Transcript panel (F4 ola 2)

The "four lives of a call" (`premium/design/mockups/transcript-panel.html`,
plate 07) — live while a call is up, writing right after hangup, the full
viewer once saved, or a calm folder-down card if the final save failed —
consuming the `transcription://segment`/`done`/`error` events `transcription.rs`
(ola 1, F4 "plomería") already emits. Unlike the premium console
(`console.rs`, above), this panel's UI is **public**: the task that built it
was scoped to `phone/shell/ui/` directly (not a `premium-console://`-style
runtime asset directory), matching how BLF favorites/click-to-call/deep
links already ship public with the *feature itself* gated by a license
check, not the UI code.

- **Full-screen overlay inside the real window, not a second Tauri
  window.** The mockup's own 680×648 standalone "win" doesn't fit this
  app's 380px default width (`tauri.conf.json`) — rather than open a
  second, differently-sized window (the console's own pattern, justified
  there by needing to serve premium-only assets from outside this repo),
  `#screen-transcript` adapts the settings-screen precedent (`#screen-settings`):
  a full-screen overlay (`z-index:25`, above the call overlay's `20`) using
  this window's own real titlebar/chrome. `transcript.css` scopes every
  selector under `.transcript-screen` specifically so generic mockup class
  names (`banner`, `plates`, ...) can't leak onto unrelated elements that
  happen to share the name elsewhere in `app.css`.
- **`ui/js/transcript-panel.js` has zero Tauri dependency on purpose** —
  it exports pure `renderTranscriptBody(container, model, handlers)` /
  `plainTextTranscript(model)` functions operating on a plain state object;
  `app.js` owns all `invoke`/`listen` wiring and hands the module a `model`
  built from real events. This is what let `dev/transcript-mock.html`
  (a harness, not referenced by `index.html`, never shipped/loaded by the
  real app) verify all five phases × both themes via a headless Browser
  pane instead of desktop GUI automation — see `E2E.md` "Transcript panel
  (F4 ola 2)".
- **Entry points**: a titlebar button (`#btn-transcript`, same "absent
  unless active" pattern as `#btn-console`) appears the instant a
  transcript starts tracking client-side, so a `live`-mode call can be
  watched mid-call; the panel also **auto-opens once** at the exact
  `established → closed` transition if a transcript was tracking (the
  "just ended - writing" moment, `app.js`'s `maybeTranscriptCallEnded`) -
  safe to auto-open there specifically because the call overlay has
  already disappeared by then, so nothing hides call controls.
  `activation == "manual"` calls get a small "Transcribe this call" ghost
  button in the call overlay's `.foot` instead of auto-starting
  (`#btn-transcribe-manual`, `renderManualTranscribeButton`).
- **`reveal_in_file_manager`** (new command, `commands.rs`) backs "Show in
  folder"/"Show local copy" — reveals a path in Finder/Explorer/`xdg-open`,
  but only after canonicalizing it and checking it resolves under either
  the configured `transcription.storage_dir` or a
  `centinelo-transcribe-tap.*` temp directory (the only two places this
  feature ever writes a transcript) — never an arbitrary frontend-supplied
  path, same "verify, don't assume" discipline `console.rs`'s own asset
  protocol handler documents for its path-traversal guard.
- **`channels_failed`** (added to `centinelo-transcribe`'s real `done`
  event in a 2026-07-16 reliability re-review, after ola-1's own contract
  reconciliation had already landed) is now parsed (`transcription.rs`'s
  `TranscribeLine::Done`) and threaded onto `transcription://done`'s
  payload and `PendingRetryView` (for a retried save) - the panel renders
  it as a calm, non-alarming notice ("Part of this call wasn't
  transcribed...") above the tape, never a full-looking transcript that's
  silently missing a channel.

### 4R fix pass (2026-07-16, same day)

The first 4R review on this panel FAILED all four lenses. Every finding
was closed in the same branch before merge - summarized here since it
changed real architecture, not just polish:

- **A previously-in-flight `state.call` could vanish before an `await`
  resolved** (`btn-transcribe-manual`'s click handler) - a `call_state:
  "closed"` racing `transcription_manual_start`'s own round trip nulled
  `state.call` synchronously mid-`await`, so resuming afterward threw
  reading `state.call.callId` (crashing as a raw JS error banner) *and*
  skipped `beginTranscript` - even though the backend had already
  accepted the tap and would transcribe the call regardless, silently
  orphaning every `transcription://` event for it. Fixed by capturing
  `callId`/`peer`/`direction` before the `await`, never re-reading
  `state.call` after it.
- **The find input's `value="${escapeHtml(query)}"` was an attribute-
  injection bug** - `escapeHtml` only ever escaped `&`/`<`/`>` (correct
  for text-node content, where this module also uses it), not `"`/`'`,
  which a re-render (any live segment arriving mid-typing) then
  interpolated straight into a `value="..."` attribute. Fixed by setting
  the DOM `value` property directly instead of ever building that
  attribute from user input as a string; added `escapeAttr` for the
  handful of remaining `data-*` attribute interpolations (the
  otherwise-safe `call_id`s in the pending-retries list, defense in
  depth since those are already whitelist-validated server-side).
- **A pending retry (NAS down, say) on call A used to vanish from the UI
  the instant call B started** - `beginTranscript` unconditionally
  overwrote `state.transcript`. Fixed with a real
  `state.pendingRetries` list, independent of whichever call is
  "current" - hydrated at `boot()` (so an app restart doesn't lose
  visibility either), refreshed after every relevant event, and rendered
  as an "other calls waiting to save" section (or, with nothing current
  at all, as the panel's own primary content -
  `renderPendingRetriesOnly`).
- **`reveal_in_file_manager` on Windows silently opened Explorer with
  nothing selected** for a `storage_dir` on a NAS share - `canonicalize()`
  returns the `\\?\UNC\...` extended-length form there, which
  `explorer /select,` doesn't resolve. Fixed with a pure, unit-tested
  `strip_windows_extended_prefix`. Its own path-validation logic
  (`reveal_path_is_allowed`) was extracted out of the `#[tauri::command]`
  for testability at the same time - its test suite caught a **second,
  real bug** in the process: the temp-tap-dir check compared an
  unresolved `std::env::temp_dir()` against an already-`canonicalize()`d
  candidate path, which never matched on macOS (`/var` -> `/private/var`)
  - meaning "Show local copy" would have silently failed on every
  developer's own Mac. Also added a symlink-escape test (a link inside
  `storage_dir` pointing outside it) proving `canonicalize()`-before-check
  closes that path.
- **A live call's tape re-rendered its ENTIRE history on every single new
  segment** - O(n) work n times over a call is O(n²) total, visibly janky
  on a long call-center call. Fixed by capping the *live* view to the most
  recent `LIVE_TAPE_MAX_TURNS` (50) turns (with a quiet note that it's
  truncated) - the `done`/`error` phases still always render every
  segment, since that render only ever happens once and is the
  authoritative saved transcript.
- **Zero automated test coverage** on `transcript-panel.js` despite
  exporting `__testables` for exactly that purpose, and zero coverage on
  `reveal_in_file_manager`'s entire security boundary. Closed with
  `ui/js/transcript-panel.test.js` (`node:test`, no new dependency -
  `escapeHtml` was rewritten to not need a live `document` so the whole
  module's pure half is testable in plain Node) and the
  `reveal_in_file_manager_tests` module in `commands.rs` (see above -
  this is also what caught the macOS temp-dir bug).
- **Readability**: the find-hit counter always read `N OF N` with no
  actual prev/next cursor to navigate between, implying a feature that
  doesn't exist - simplified to `N matches`. `onShowFolder`/`onShowLocal`
  were byte-identical closures - both are now the same
  `revealInFileManager` reference. The mock harness moved from `ui/dev/`
  to a sibling `shell/dev/` - `ui/` is bundled verbatim into every build
  via `frontendDist`, so anything under it ships in the release
  regardless of whether `index.html` links to it.

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
npm test           # node:test coverage for ui/js/*.test.js (frontend pure logic)
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
| `CENTINELO_E2E_SCRIPT` | Scripted call-control + premium/console driver, see below. |
| `CENTINELO_PREMIUM_ASSETS_DIR` | Override the auto-detected premium console-ui assets directory (see "Premium console window"). Not debug-only - same "next to the exe unless overridden" shape as `CENTINELO_CORE_BIN`. |

## e2e verification

See **`E2E.md`** for the full methodology (including why a `\|`-separated
`CENTINELO_E2E_SCRIPT` env-var driver was used instead of OS-level click
automation for the final verified runs) and the captured evidence: the
complete `ready`/`reg_state`/`call_state` event trail from the real running
app, and independent PBX-side RTP packet-count confirmation
(`asterisk -rx "pjsip show channelstats"`, read-only) for four separate
real calls to the `*43` echo test extension over WSS. `E2E.md`'s "F4
premium" section covers the premium loader's three gating scenarios
(missing dylib / tampered signature / valid dylib+license) and the premium
console's own live e2e (BLF tiles via the dual-contact trick,
`blind_transfer` from the console's own code path, PBX-side confirmation of
the surviving channel).

## F3 additions: live BLF favorites, click-to-call bridge, deep links

- **Live BLF favorites** — `favorites` (still 4 free-tier slots, `ext` +
  `label`) is now admin-gated like the account fields (`commands::
  save_favorites`, restarts the sidecar so a fresh process re-registers and
  re-subscribes to the new list). On every `reg_state:"registered"`,
  `sidecar.rs`'s stdout reader issues `blf_subscribe` once per configured
  extension (guarded against `regint`-driven re-REGISTERs re-firing it
  within the same process — `ctrl_json` errors on a duplicate subscribe).
  Incoming `blf` events update `ui/js/app.js`'s `state.blf` map, which
  `renderFavorites()` maps straight onto the mockup's own lamp classes —
  `idle`->`.fav.idle`, `ringing`->`.fav.ring` (ringing owns the one amber
  glow, per `DIRECTION.md`'s "one glow rule"), `busy`->`.fav.busy`,
  `offline`/unconfigured->`.fav.off` — no new CSS, `app.css` already had
  all four from F2's static placeholder tiles. Clicking a favorite always
  shows the "Call this number?" confirmation (shared with the bridge/deep
  links below) before dialing — favorites are real coworkers' extensions,
  not a number a keypad-happy click should silently ring. The backend also
  tracks the same per-extension state (`sidecar.rs` `Shared::blf_states`,
  exposed as `get_blf_states`) so a devtools reload repaints instead of
  going blank until the next NOTIFY.
- **Click-to-call bridge** (`bridge.rs`) — ported from v1's Electron bridge
  (`src/main/main.js`) onto a `tiny_http` listener on the same
  `127.0.0.1:38911`, same `X-Centinelo-Token` header, same `GET /ping` +
  `POST /dial` (JSON body `{"number":...}`) contract, same CORS headers
  including `Access-Control-Allow-Private-Network` — the v1 Chrome
  extension in `extension/` works against it unchanged. `token`/`number`
  are *also* accepted as query params purely for `curl`-based verification
  convenience; the extension itself never uses that path. One deliberate
  behavior change from v1: a dial request no longer dials silently — it
  raises the same "Call this number?" confirmation as favorites, unless
  `settings.bridge.auto_dial` is on (default off).
- **`centinelo://`/`tel:` deep links** (`deeplink.rs`, `tauri-plugin-deep-link`
  + `tauri-plugin-single-instance`) — `centinelo` is always claimed (this
  app's own scheme); `tel` is opt-in (`settings.bridge.register_tel_handler`,
  off by default), matching v1's own `registerTelHandler` setting. Both
  feed the exact same "click-to-call" event/confirmation flow as the
  bridge. Platform note, confirmed by reading the plugin's vendored source
  rather than assumed: dynamic `register()`/`unregister()` only work on
  Windows/Linux — macOS has no runtime API for this, so a *built and
  installed* Centinelo is always Info.plist-capable of `tel:` once
  installed, and the in-app toggle there instead gates whether an incoming
  `tel:` link is acted on at all.

## F4 addition: auto-provisioning

**`PROVISIONING.md`** has the full design (JSON config schema, the three
link forms, admin-lock carve-out, security notes, what's in/out of scope).
Short version: `provisioning.rs` + a new `#setup-prompt` paste field and
`#provision-confirm-overlay` confirmation screen (`ui/index.html`/
`ui/js/app.js`) let a fresh install go from "paste a link" to "registered"
without touching Settings by hand, per spec §5. Reuses the deep-link
plumbing `deeplink.rs` already had for `tel:`/`centinelo:` dial links —
`centinelo://provision?...` is routed away from that dial-target
extraction before it runs. The secret never round-trips to the frontend at
any point (two-step `provisioning_resolve`/`provisioning_apply`, see that
module's doc). QR is explicitly out of scope for this pass (webcam capture
specifically) — see `PROVISIONING.md` "QR" for what's left for a future
pass to build on.

## License activation (P3, activation-server plan)

Settings → License gains a serial paste field, an activation server URL
field (default **empty**, same "no internal hostname ships in this public
repo" rule the STT/provisioning endpoints already follow), and an
"Activate" button — the shell side of "generic signed serial in →
seat-counted, machine-bound, signed license out" (private premium repo:
`docs/SPEC-2026-07-17-activation-server-design.md`,
`docs/PLAN-2026-07-17-activation-server.md` §P3, read-only references —
the server half, `centinelo-activationd`, is a separate premium-repo
piece this shell only ever talks HTTP to).

All logic lives in `src/activation.rs`; `commands.rs`'s `activate_license`
is thin plumbing (admin-lock + `Result` mapping), same split every other
command in this file follows. The flow: validate the URL (`https://`
always, `http://127.0.0.1`/`http://localhost` only for local testing),
compute this machine's fingerprint, `POST {server}/activate`
`{serial, machine_fingerprint}` over `ureq` (reused, no new HTTP crate —
the same client `provisioning.rs`'s remote fetch already uses; the
plugin-only `reqwest` `tauri-plugin-updater` pulls in isn't a usable
direct dependency of this crate without adding it as one, so `ureq` is
the better fit for "reuse what's already here"), and on a `200` whose
returned license verifies locally against an embedded activation pubkey
**before** anything touches disk, write it atomically (`tmp` + rename,
`0600`, `settings::write_private_file` — the exact same helper
`settings.json` itself uses) to `license.json`, a new sibling of
`settings.json` in the app-data directory
(`SettingsStore::license_path`). Any failure at any step — bad URL,
network down, a non-2xx response, or a `200` whose signature does NOT
verify — leaves whatever license state already existed completely
untouched (spec §5.4: "a failed activation changes nothing"; the shell
never persists the serial itself, only the server URL preference).

**Errors cross the Tauri command boundary as short codes**
(`"seats_exhausted"`, `"serial_revoked"`, `"invalid_serial"`,
`"expired_serial"`, `"network"`, ...; see `activation::ActivationError::code`),
not prose — `ui/js/i18n.js`'s `activation.error.<code>` keys hold the
actual displayed copy, in all three of this product's real languages
(EN/PT-BR/ES; the ES wording matches the P3 task brief's exact spec).
This is a deliberate difference from `provisioning_apply`'s own
English-only backend error string (which never runs through i18n.js
today) — this feature routes through the real i18n system instead of
adding a second, inconsistent all-one-language error path, while this
repo's "UI text English" rule (`.claude/skills/shell-tauri/SKILL.md`) is
satisfied because what's hardcoded in Rust is a short identifier, never
displayed prose.

### Machine fingerprint and the signed-license envelope are duplicated, not imported

This crate never depends on the private `centinelo-license` crate — same
rule `premium.rs`'s own doc states for the dylib loader ("a public,
forkable repo... a fork could delete any gating logic that lived here").
`activation.rs`'s `machine_fingerprint()` and its minimal
`{payload, sig}` envelope verifier are hand-duplicated from
`centinelo-license/src/fingerprint.rs` and `.../src/container.rs`
(private repo, read-only references) — the same precedent
`centinelo-premium-abi/src/capability.rs` already sets for the
`FEATURE_*` name strings. `machine_fingerprint()` mirrors the private
crate's algorithm bit-for-bit (macOS `IOPlatformUUID` via `ioreg`,
Windows `MachineGuid` from the registry, `CENTINELO_MACHINE_ID` env
override for dev/test/CI) so a license this shell requests binds to the
same fingerprint a future real consumer of `license.json` would compute
independently for the same machine.

### Activation pubkey — dev/test placeholder, same pattern as the other two

`ACTIVATION_PUBKEY_BYTES` (`activation.rs`) is a third, distinct dev/test
Ed25519 keypair — `premium.rs`'s `LIB_PUBKEY_BYTES` authenticates the
`centinelo_premium` dylib *binary*; `centinelo-premium`'s own (private
repo) `DEV_TEST_LICENSE_SIGNING_SEED` gates its dev-only
`CENTINELO_PREMIUM_LICENSE_PATH` override; this one authenticates a
*license issued by an activation server*. **Before an official release**,
Felix generates a real activation keypair offline (same ceremony this
file's own "Dev signing key" section documents for the updater), replaces
`ACTIVATION_PUBKEY_BYTES` with the real public half, and the private half
becomes `centinelo-activationd`'s `ACTIVATIOND_KEY` (server-side, private
repo, never in this repo).

### The real gap this piece leaves open — flagged, not papered over

`activate_and_persist` writes a verified `license.json` — but **nothing
reads it back yet**. Confirmed by reading `centinelo-premium/src/
license.rs` (private repo, read-only reference, this repo's scope never
includes editing it): `active_license()` there is
`founder_license().or_else(license_from_override_env)`, and that
`or_else` arm is an explicit dev/test-only escape hatch
(`CENTINELO_PREMIUM_LICENSE_PATH`), documented in that file's own doc
comment as "not yet a real app-data-dir file read... out of scope for the
v0 loader mechanism this crate implements." This piece is code-complete
and tested end to end up to a correctly-verified `license.json` landing
on disk at `SettingsStore::license_path()`; making an activated license
actually change what `premium_capability_status` reports needs a
follow-up in `centinelo-premium` (licensing agent's ambit, not this
one) — teaching `active_license()` to also try reading from this shell's
real path, not only the dev env-var override. The UI's own success copy
(`settings.licenseActivatedStatus`) is worded to match what actually
happened today ("License saved for {customer}"), not a restart-to-apply
claim this shell can't back up yet.

## Auto-updater (roadmap debt fix)

Every build used to require a manual reinstall. `tauri-plugin-updater` +
`tauri-plugin-process` close that gap: GitHub Releases' static
`latest.json` as the sole endpoint (`plugins.updater.endpoints` in
`tauri.conf.json`) — no server of our own, matching this app's existing
"nothing phones home except what you explicitly configure" posture
(README's own `settings.aboutBody` string).

### Why the plugins aren't `import`ed like the docs show

Tauri's official docs assume a bundler (`import { check } from
"@tauri-apps/plugin-updater"`, resolved from `node_modules` by
Vite/webpack/etc.). This project has neither — `ui/` is served verbatim as
`frontendDist`, and `node_modules` isn't even inside it. `withGlobalTauri:
true` only injects the **core** API onto `window.__TAURI__` (`.core`,
`.event`, `.window`, `.app`, ...) — verified against `tauri`'s own
`scripts/bundle.global.js` (2.11.5): its `__TAURI_IIFE__` object literally
assigns `e.app=f, e.core=_, e.event=O, ...` with no `updater`/`process`
entries anywhere in the bundle. Plugin JS packages are NOT part of that
global — their own `dist-js/index.js` even `import`s `@tauri-apps/api/core`
as a bare specifier internally, which would fail to resolve here too.

So `ui/js/app.js`'s own "auto-updater" section calls `invoke("plugin:
updater|check", ...)` directly for the read-only check — confirmed against
the plugin's Rust `commands.rs` (exact argument names/casing) and JS
`dist-js/index.js`. `window.__TAURI__.core.Channel` (download progress) and
`.core.invoke` **are** part of the global bundle (same `core.js` module,
not curated) — verified the same way.

Download and install do **not** go through the plugin's own
`plugin:updater|download`/`|install` commands (2026-07-17 4R re-review,
RESILIENCE blocker — see the next section for why) — `src-tauri/src/
updater.rs`'s own `updater_download`/`updater_install` commands do,
calling the plugin's public `Update::download()`/`Update::install()`
Rust methods directly. `ui/js/updater.js` itself stays Tauri-free either
way (pure state machine + rendering, see its own header comment) — app.js
is the only file that knows any of this.

### Two-step download → install, and why install() is this app's OWN Rust command

This is a softphone. `install()` restarts the process — it must never fire
out from under an active call. The plugin's own combined
`downloadAndInstall()` doesn't leave a pause point between "downloaded" and
"installing" either way, so this shell always used the separate
`download()`/`install()` shape for a real "Update ready" phase where the
operator decides when to restart.

The FIRST version of this (shipped, then caught in the same-day 4R
re-review) gated that decision with `app.js` reading `state.call` —
`ui/js/app.js`'s own client-side mirror of call state, the SAME kind
`beginTranscript`'s doc elsewhere in that file already documents as able
to go stale mid-`await` (a `call_state:"closed"` racing it). That's an
acceptable risk for a UI affordance (a manual-transcribe button that's
merely disabled a beat late); it is not acceptable for the one action that
kills the entire process. `commands::provisioning_apply` had already hit
this exact bug class once (the R4 bug: a provisioning deep link racing an
active call, fixed by checking `sidecar.has_active_call()` — the
authoritative, call_id-tracked source — inside the Rust command itself,
not the frontend). The updater now follows the same shape:
`src-tauri/src/updater.rs`'s `updater_install` command checks
`sidecar.has_active_call()` as the very first thing it does, before even
looking up the update/bytes resources, and refuses with a clear message if
a call is active. `ui/js/updater.js`'s `canStartInstall` (reading
`state.call`) stays as UX only — it disables the button and saves a round
trip in the common case, but the Rust check is what actually decides, and
it isn't reachable to bypass from devtools the way a JS-only guard would
be.

**Why this couldn't be a thin wrapper around the plugin's own
`plugin:updater|install`**: that command (and the private resource type a
`bytes_rid` from `plugin:updater|download` actually points at,
`DownloadedBytes`) lives inside `tauri-plugin-updater`'s private `mod
commands` — not `pub mod`, and `lib.rs`'s `pub use updater::*` never
re-exports it. There is no type this crate could name to even resolve a
`bytes_rid` the plugin's own command minted, let alone forward to
`install()` with a check bolted on. What IS public is
`tauri_plugin_updater::Update` itself (`#[derive(Clone)] pub struct
Update`, `impl Resource for Update {}`, `pub async fn download`/`pub fn
install`, confirmed in 2.10.1's `updater.rs`) — the same object the
plugin's own commands call these exact methods on internally.
`updater_download`/`updater_install` call them directly instead, storing
the downloaded bytes under **this crate's own** resource type
(`DownloadedUpdateBytes`) so both commands agree on a type this crate
actually owns end to end. `Update::download()` verifies the update's
signature before ever returning bytes — not something either command
re-implements or could accidentally skip.

This closes most of the race between "an update is ready" and "a call is
active," but not all of it — a call that starts in the exact window
between `has_active_call()` returning `false` and `Update::install()`
actually tearing the process down isn't caught (there's no lock spanning
both). Documented, not silently accepted: see "Known limitations" in this
section for the two gaps this pass leaves open (this one, and the
Windows-specific one below).

### Dev signing key — replace before a real release

`tauri signer generate` needs a real keypair; the private half must never
touch this repo or CI (same discipline `PROTOCOL.md`'s TLS pinning and
`premium.rs`'s dev-vs-real ABI key already document). For **development**,
a throwaway keypair was generated once (`CI=true npx tauri signer generate
-w <path> -p "" --ci`) and only its **public** half is embedded in
`tauri.conf.json`'s `plugins.updater.pubkey` — the private half was written
to a scratch path outside this repo and is not recoverable from anything
checked in here.

**Before a real release, Felix replaces this dev key offline:**

1. `npx tauri signer generate -w /somewhere/outside/any/repo/centinelo-updater.key`
   (a real password recommended — `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` at
   sign time, never committed).
2. Copy the printed **public** key (a single base64 line, `dW50cnVzdGVk...`)
   into `tauri.conf.json`'s `plugins.updater.pubkey`, replacing the dev
   value below. This is the ONLY updater-related value that's real config,
   not a secret — it's meant to be public (it's how every installed copy of
   the app verifies a signature, the same direction TLS certs work).
3. The **private** key + its password become release-ci's signing
   secrets (`TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
   env vars at `tauri build` time — see the contract below), stored the same
   place the premium dylib's Ed25519 signing key already lives: offline
   with Felix, injected into CI as a secret, never in this repo.
4. Every install signed with the OLD (dev) key stops being able to verify
   an update signed with the NEW key — expected, matches "public key change
   = new trust root," not a bug to route around.

`tauri.conf.json`'s current `pubkey` corresponds to a well-known,
non-secret dev seed — anyone can regenerate the matching private key from
this repo's own git history and sign a fake update with it. This is
**exactly as safe as it sounds for a dev key** (same threat model
`premium.rs`'s own dev pubkey doc already accepts for the premium loader) —
it only matters once a real release actually ships with this pubkey still
in place, which step 2 above prevents.

### Windows: NSIS, not MSI, is the update artifact

`tauri-plugin-updater`'s Windows installer step (`updater.rs`,
`install_inner`) auto-detects NSIS vs. MSI from the downloaded bytes
(`infer::archive::is_msi`) and can drive either — but only one file can sit
behind `latest.json`'s `windows-x86_64.url`. **NSIS is the one this project
publishes there**, not the MSI `windows-installer.yml` also produces:

- `tauri.conf.json`'s `bundle.windows.nsis.installMode` is
  `"currentUser"` — a fresh install needs no admin elevation. NSIS updates
  keep that property; an MSI update can require elevation depending on how
  the original install was scoped, which would turn "restart to update"
  into a UAC prompt an operator didn't expect.
- `plugins.updater.windows.installMode: "passive"` maps to NSIS's `/P /R`
  flags (progress-bar-only, auto-restart-after-install) — the MSI path's
  `msiexec /passive` shows a native Windows Installer progress dialog,
  visually inconsistent with this app's own ink-toned, non-native chrome.
- The MSI (`bundle/msi/*.msi`) keeps shipping as a GitHub Release asset
  for anyone who explicitly wants Windows-Installer-based deployment (IT
  fleets, Group Policy) — it's just never referenced by `latest.json`, so
  the in-app updater never touches it.

### Known limitations (this feature's own, distinct from the shell-wide list at the bottom of this file)

Two gaps this pass leaves open, documented rather than glossed over
(2026-07-17 4R re-review, RESILIENCE):

1. **The has_active_call → install() race window.** `updater_install`
   checks `sidecar.has_active_call()` before doing anything else, which
   closes the window this feature's FIRST version left wide open (a
   client-side-only `state.call` check) — but a call that starts in the
   exact gap between that check returning `false` and `Update::install()`
   actually tearing the process down still isn't caught; there's no lock
   spanning both. Narrow (milliseconds) but real. Closing it fully would
   need the sidecar itself to refuse a new INVITE while an install is in
   flight, which is out of scope for this pass.

2. **On Windows, a failed installer handoff is silent — no error, no
   retry, the app just closes.** `tauri-plugin-updater` 2.10.1's Windows
   `install_inner` (`updater.rs`) calls `ShellExecuteW(...)` to launch the
   NSIS/MSI installer, **discards its return value** (no `let result =
   ...`, just a bare statement), and follows it **unconditionally** with
   `std::process::exit(0)`. If the handoff itself fails — UAC declined,
   SmartScreen or an AV quarantining the installer, a permissions issue —
   the app still exits immediately, with nothing surfaced to the operator
   and no retry path, because the whole process is gone before anything
   in `ui/js/app.js`'s own `catch` block (or even the Rust side of
   `updater_install`, mid-await) gets a chance to run. This is a real gap
   in the PLUGIN's own Windows install path (confirmed by reading
   `install_inner`'s source directly, not inferred) — not something this
   app's error handling failed to cover; there is nothing on this side of
   the IPC boundary that could intercept it. `startUpdateInstall`'s own
   doc comment (`ui/js/app.js`) flags this same limitation at the call
   site, specifically so a future edit doesn't reintroduce a comment
   implying Windows failures always surface (an earlier draft of that
   comment claimed exactly that, incorrectly — fixed in the same pass
   this README section was added).

### Periodic background re-check

`maybeCheckForUpdatesOnStartup()` only ever fires once, at launch — a
softphone can sit minimized in the tray for weeks, so a build shipped the
week after a launch would otherwise go unnoticed indefinitely
(2026-07-17 4R re-review, RESILIENCE #3, flagged as non-blocking but cheap
enough to just fix). `scheduleUpdatePeriodicRecheck()` (`app.js`, called
once from `boot()`) re-runs the same check every 24h, gated by the exact
same `check_on_startup` preference (one master switch for "does this app
ever check on its own," not two settings for what's really one decision).

The actual "is it safe to silently re-check right now" decision is
`ui/js/updater.js`'s pure, unit-tested `canRunBackgroundRecheck` — true
from `idle`/`up_to_date` **and from a check-origin error**, false from
anything that has real pending state to protect
(`available`/`downloading`/`ready`/`installing`, or a
download/install/restart-origin error). The check-origin carve-out is a
same-day 4R follow-up fix: the first version of this excluded every
`"error"` phase, including `errorOrigin: "check"` — but the only way OUT
of `"error"` is a successful check, and this timer is the only automatic
path back to one, so a launch whose FIRST check happens to fail (the
common case: `boot()` runs before the network is actually up) would have
permanently disabled its own recheck for the rest of the process's
life — defeating the exact "softphone sits in the tray for weeks" case
this feature exists for. A check-origin error has no pending resource to
protect (nothing was ever downloaded), unlike the other three error
origins.

### Contract for release-ci

Everything below is what `ui/js/app.js`'s updater code and
`tauri.conf.json`'s `plugins.updater.endpoints` already assume — the
publish pipeline (a separate release-ci task) needs to produce exactly
this, nothing here needs to change on the app side to match it.

**Endpoint** (already set): `https://github.com/fegone/Centinelo-Phone/releases/latest/download/latest.json`
— GitHub's "latest release" redirect, so this URL never changes across
releases; only the release itself does.

**`latest.json`** — uploaded as a release asset literally named
`latest.json`, on every release:

```json
{
  "version": "2.1.0",
  "notes": "What changed in this release.",
  "pub_date": "2026-08-01T12:00:00Z",
  "platforms": {
    "darwin-aarch64": {
      "signature": "<contents of the .sig file next to the .app.tar.gz>",
      "url": "https://github.com/fegone/Centinelo-Phone/releases/download/v2.1.0/Centinelo.Phone_2.1.0_aarch64.app.tar.gz"
    },
    "windows-x86_64": {
      "signature": "<contents of the .sig file next to the NSIS .exe>",
      "url": "https://github.com/fegone/Centinelo-Phone/releases/download/v2.1.0/Centinelo.Phone_2.1.0_x64-setup.exe"
    }
  }
}
```

- `version` must be valid SemVer and greater than the previous release's
  (`update.rs`'s default version comparator is a plain `>`, no downgrades).
- `pub_date` is RFC 3339.
- Only the platforms this release actually built need an entry — a build
  that skipped Windows this round (Windows CI red, say) just omits
  `windows-x86_64` rather than publishing a broken one; the app then
  reports "up to date" instead of "found nothing" for that platform's
  installs, matching `check()`'s own `Option<Metadata>` shape (see
  `withUpToDate` in `ui/js/updater.js` — nothing distinguishes "genuinely
  latest" from "no entry for this platform" today, a known, minor gap, not
  a build blocker).
- **macOS `darwin-x86_64`** isn't in the example above because
  `shell-build.yml`/`core-build.yml` currently only run `macos-latest`
  (Apple Silicon runners) — add it once an Intel or universal build exists.
  `darwin-universal` is also a valid single key covering both if release-ci
  moves to a universal build instead of two per-arch ones — either shape
  works, this app doesn't care which.

**Asset names**: don't hardcode an exact filename — `tauri build`'s own
bundler names these (productName + version + arch, per `tauri-plugin-
updater`'s own doc comment on `install_inner`: `[AppName]_[version]_x64-
setup.exe`, `[AppName]_[version]_x64.msi`, `[AppName]_[version]_
aarch64.app.tar.gz` are the documented *pattern* — confirm the exact
strings against real build output, same "verify, don't assume" discipline
`windows-installer.yml`'s own smoke test already applies via `Get-
ChildItem *.exe`/`*.msi` globs rather than fixed names). What matters is
only that:

1. The Windows asset `latest.json` points to is the **NSIS** `.exe`
   (`target/release/bundle/nsis/*.exe`), not the MSI.
2. The macOS asset is the **`.app.tar.gz`**
   (`target/release/bundle/macos/*.app.tar.gz`), not the `.dmg` — this is
   the file `createUpdaterArtifacts: true` (now set in `tauri.conf.json`)
   makes `tauri build` produce specifically for the updater; the `.dmg`
   keeps shipping too, for first installs.
3. Every asset `latest.json` references has a sibling `<same name>.sig`
   file, ALSO uploaded as its own release asset (not read from — its
   *contents*, a single base64 line, is what goes into `latest.json`'s
   `signature` field). `tauri build` produces these `.sig` files
   automatically, next to each updater artifact, the moment
   `TAURI_SIGNING_PRIVATE_KEY` (+ `..._PASSWORD` if the key has one) is set
   in the build environment — no separate signing step needed.

**Signing**: `TAURI_SIGNING_PRIVATE_KEY_PATH` (or `_KEY` for the raw
string) + `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` as CI secrets, injected only
into the `tauri build` step, sourced from wherever Felix's real offline key
ends up living (see "Dev signing key" above) — the same secret-handling
discipline `premium/docs/loader-integration.md`'s signing key already
follows, nothing new to invent here.

### How this was verified this pass

- `cargo build` / `cargo clippy --all-targets -- -D warnings` / `cargo
  test` all green with both plugins wired in, plus `src-tauri/src/
  updater.rs` (new, 2026-07-17 4R fix pass). This doubles as real
  verification that `capabilities/default.json`'s `updater:allow-check`/
  `process:allow-restart` and `tauri.conf.json`'s `plugins.updater` block
  are valid: Tauri's build script validates capability identifiers and
  plugin config shape against the linked plugins' own ACL manifests/
  `Config` deserializer at compile time, not just at runtime — and that
  `updater.rs` actually compiles against the plugin's PUBLIC API surface
  only (`tauri_plugin_updater::Update`, `impl Resource for Update`), which
  is the whole point of that module (see its own header comment) — if
  `Update`/its methods had stopped being public in some future plugin
  version, this build would fail loudly here, not silently at runtime.
  `updater::refuse_install_while_on_a_call_tests` (2 tests) pins the exact
  gate decision (refuses while a call is active, allows otherwise) as a
  pure function — the authoritative state it reads,
  `sidecar.has_active_call()`, already has exhaustive coverage of its own
  in `sidecar.rs`'s `call_phase_tests`.
- `npm test` (`node --test ui/js/*.test.js`) — 103 passing assertions
  across `updater.js`'s full state machine (`updater.test.js`): the happy
  path, every error origin (check/download/install/restart — the
  post-install-relaunch-failed case) and which ones reach the main-window
  banner vs. stay Settings-only, the stale-progress-event guard,
  `canStartInstall`'s call-safety gate, `canRunBackgroundRecheck`'s full
  phase×errorOrigin matrix (same-day follow-up fix — see "Periodic
  background re-check" above for the bug this closes), and
  `closePendingUpdateResources`' dependency-injected close-call contract
  (a counting mock `closeFn`, confirming BOTH the update-metadata AND the
  downloaded-bytes resource get closed, individually and together, and
  that one failing never stops the other — this is the regression test for
  the leak the same review pass caught: the previous inline version only
  ever closed the first one).
- **`dev/updater-mock.html`** (same precedent as `dev/transcript-
  mock.html`) — a standalone harness importing the real `renderUpdateBanner`/
  `renderUpdaterAboutStatus` from `ui/js/updater.js` against 15 fabricated
  states (13 from the first pass + 2 new: "installed, restart failed" with
  and without an active call), verified via a headless Browser pane across
  light/dark and all 3 locales — including, this pass, confirming the
  restart-failed state renders "Update installed" (never "failed") with a
  "Restart to update" button (never "Retry"), that the button disables the
  same way the `ready` phase's own does when a call is active, and that
  Settings' downloading status now shows a real percentage
  (`updater.aboutDownloading`, previously a dead i18n string never wired
  to anything — 4R minor #5). This is the sanctioned alternative to
  desktop GUI automation this project already uses (`shell-tauri`'s own
  rule against automating the real app window) — it verifies rendering,
  not the real network/IPC calls, which only run inside an actual Tauri
  webview (see "Why the plugins aren't `import`ed" above for why a plain
  browser tab can't reach `window.__TAURI__.updater` at all — there is no
  such namespace to reach, and this app's own `updater_download`/
  `updater_install` commands are equally unreachable from outside a real
  Tauri IPC context).
- **Not verified this pass**: a real `check()`/`download()`/`install()`
  round trip against a live mock HTTP endpoint from inside the actual
  running desktop app, and a real end-to-end proof that
  `updater_install` genuinely refuses while a live call is up (as opposed
  to the pure-function gate logic, which is verified). Both need a real
  Tauri webview (WindowServer connection) or a `tauri::test::mock_app`-
  style harness, neither of which this sandboxed environment has. What
  IS verified independently: the `latest.json` shape (against the
  plugin's own `Metadata`/config structs), the Rust-side plugin wiring
  (via `cargo build`'s ACL/config validation), and the gate's own decision
  logic (unit-tested). Flagged as a real gap, not glossed over: qa-e2e or
  a real machine run should confirm the live network round trip AND the
  has_active_call() refusal against an actual call once release-ci's
  pipeline produces a real `latest.json` to point at.

## Apple code signing & notarization — configure before a real macOS release

A real macOS release must be **codesigned with a Developer ID Application
certificate and notarized through Apple's notary service**, or macOS
Gatekeeper will refuse to open the `.app`/`.dmg` for anyone who downloads
it: the default "downloaded from the internet" quarantine flag plus the
hardened-runtime requirement make an unsigned build unlaunchable for end
users. This is a hard gate in release-ci, not a nice-to-have.

**This is `medium-blocked` right now:** Felix does not yet hold an Apple
Developer Program membership, so the `APPLE_*` secrets are not configured.
The `preflight` job has a dedicated guard (`Guard - Apple notarization
secrets must be configured`) that fails the whole run in seconds — before
any compile starts — listing exactly which of the six secrets are missing
and pointing here, rather than failing 20 minutes into a build with a
cryptic `codesign`/`notarytool` error. Once the secrets below are set, the
guard passes and `build-macos` signs + notarizes automatically. No code
change is needed at that point.

### Getting the certificate and the six secrets

An Apple Developer Program membership costs **$99/year** (individual or
organization) at https://developer.apple.com/programs/. The certificate
this app needs is a **Developer ID Application** certificate — NOT a
"Developer ID Installer" one (the Installer cert is for `.pkg`s; the
Application cert is for `.app`s and binaries). Once Felix has the account:

1. **Create the certificate** in the Apple Developer portal (Certificates,
   Identifiers & Profiles → Certificates → + → "Developer ID Application"),
   or via Keychain Access → Certificate Assistant → Request a Certificate
   from a Certificate Authority with a local CSR. Install it into Keychain
   Access on a Mac.
2. **Export it as a `.p12`** ("Personal Information Exchange") from
   Keychain Access: select the *private key* under the certificate →
   File → Export Items → save as `DeveloperID.p12`, choosing a strong
   password (this becomes `APPLE_CERTIFICATE_PASSWORD`). The `.p12`
   bundles the certificate + its private key into one portable file.
3. **Base64-encode the `.p12`** so it can live in a GitHub secret with no
   binary/escaping issues:
   ```
   base64 -i DeveloperID.p12 | pbcopy     # macOS: copies the base64 to the clipboard
   # or:  base64 -w 0 DeveloperID.p12     # GNU base64 (Linux) — single line
   ```
   Paste the result (one long line) into a new repository secret named
   **`APPLE_CERTIFICATE`**. The workflow decodes it back to a `.p12` at
   build time before importing it into a throwaway keychain.
4. The remaining five secrets are plain strings — add each as its own
   repository secret (Repo → Settings → Secrets and variables → Actions →
   New repository secret):
   - **`APPLE_CERTIFICATE_PASSWORD`** — the password chosen at step 2.
   - **`APPLE_SIGNING_IDENTITY`** — the certificate's Common Name exactly
     as `security find-identity` prints it, e.g.
     `"Developer ID Application: Felix Gonzalez (ABCD1234XY)"`. `codesign
     -s "$APPLE_SIGNING_IDENTITY"` resolves by this name.
   - **`APPLE_ID`** — the Apple ID (email) of the notarizing account.
   - **`APPLE_PASSWORD`** — an **app-specific password** (NOT the account
     password): create one at https://appleid.apple.com → Sign-In and
     Security → App-Specific Passwords. `notarytool` authenticates with
     this, never the account password.
   - **`APPLE_TEAM_ID`** — the 10-char Team ID shown in the Developer
     portal's Membership details (also the parenthesised suffix of the
     signing identity above). `notarytool` needs it to disambiguate the
     account.

No `APPLE_*` secret's VALUE is ever printed by the workflow — the guard
and the import/codesign/notarize steps only ever check *presence by name*
or feed the value straight into `security`/`codesign`/`notarytool`.

### What the build does with them (and why the order matters)

`package-official.sh` copies the core engine binary + baresip modules into
`.app/Contents/MacOS/` **after** `tauri build`, which would invalidate any
signature Tauri applied at build time. So signing is the **manual, last**
step in `build-macos`, in this exact order (see `.github/workflows/release.yml`,
job `build-macos`):

1. `tauri build --bundles app` builds the bare `.app` only (NOT the `.dmg` —
   a `.dmg` built now would wrap a core-engine-less `.app` and ship
   incomplete). `bundle.macOS.signingIdentity` is `null` so Tauri does
   **not** sign and never races our manual `codesign`.
2. `package-official.sh --target macos` injects `core-engine/` (baresip +
   modules) beside the executable.
3. The `.p12` is imported into a **temporary keychain** that is deleted
   (`if: always()`) at the end of the job — the cert never touches the
   runner's default keychain and never persists.
4. **Manual codesign, inner-to-outer** (no `--deep`): each embedded Mach-O
   is signed first — the `.so`/`.dylib` modules with hardened runtime, and
   the `core-engine/baresip` sidecar (a standalone process that opens the
   mic + SIP socket, so it gets the audio-input/network entitlements too) —
   then the `.app` bundle itself is signed last with `--options runtime
   --entitlements entitlements.plist` and a Developer ID timestamp.
5. The signed `.app` is zipped and submitted to `notarytool submit --wait`,
   then `stapler staple`d.
6. The **final** `.dmg` is built with `hdiutil` from the now-signed +
   stapled `.app`, then the `.dmg` itself is signed + notarized + stapled
   (a `.dmg` is a separate notarization target from the `.app` inside it).
7. The updater `.app.tar.gz` + `.sig` are regenerated from the signed
   `.app` (re-tar + `tauri signer sign`), so the update payload matches
   what ships inside the `.dmg`.
8. `codesign --verify --deep --strict`, `spctl -a -t exec -vv`, and
   `stapler validate` confirm the result before any asset is uploaded.

### `com.apple.security.cs.disable-library-validation` — deliberately absent

Hardened runtime's Library Validation requires every loaded dylib be
signed by Apple or the **same Team ID** as the main executable. The
core-engine/baresip modules and any premium dylib are all codesigned in
step 4 with the **same** Developer ID Application certificate (same Team
ID), so library validation passes *without* weakening it. The entitlement
that disables it (`com.apple.security.cs.disable-library-validation`) is
intentionally NOT in `entitlements.plist` — adding it preemptively weakens
the runtime for no benefit. It would only become necessary if a future
build loaded a dylib signed by a *different* team (e.g. a third-party
plugin); that change must be documented here, in the plist, and in the
codesign step at the same time.

Full walkthrough: `.github/workflows/release.yml`, job `build-macos`,
steps `Codesign the .app in depth` through `Verify codesign, Gatekeeper,
and notarization`.

## Known limitations (F2/F3/F4 scope)

- **License activation writes a verified `license.json`, but nothing
  reads it back yet** — see "License activation (P3, activation-server
  plan)" above, "The real gap this piece leaves open", for the full
  explanation. `centinelo-premium` (private repo) needs a follow-up to
  read from this shell's real app-data-dir path instead of only its
  dev-only `CENTINELO_PREMIUM_LICENSE_PATH` env override before an
  activated license changes what `premium_capability_status` reports.
  Also not built this pass (out of P3's scope per the plan): a real
  `centinelo-activationd` round trip from inside a running desktop app
  (this piece's own tests cover the mock-server flow only, see
  `activation.rs`) — that's P4's job, once P2 (the server) exists.
- No `hold`/`mute`/`transfer`/`dtmf` **in the main window's own UI** — F4
  added the backend commands (`sidecar_hold`/`sidecar_mute`/
  `sidecar_blind_transfer`/`sidecar_attended_transfer`/etc., see
  `commands.rs`) and wired them into the premium console, but the main
  window's dialpad/in-call overlay still has no buttons for them — a real
  gap for a Community-only user (free tier never sees a hold button even
  though the protocol and backend now support it), tracked as follow-up,
  not this round's scope (F4 was "integrate the premium module + console",
  not "redesign the free-tier call UI").
- Cert pinning (`CENT_TLS_PIN`, `core/BUILD.md` "TLS leaf-certificate
  pinning") is now wired end to end (`settings.rs` `AccountSettings.tls_pin_sha256`
  -> `sidecar.rs` spawn env, this session's auto-provisioning work) —
  v1's `pinnedCertSha256` setting *is* ported, functionally. What's still
  missing is a **manual-entry field for it in Settings**: the only way to
  set it today is through a provisioning config (`PROVISIONING.md`'s
  `tls_pin_sha256` field) — an operator can't paste a pin into the UI by
  hand. `sip_verify_server no` (self-signed-CA-friendly, unconditional) is
  unchanged, matching `core/BUILD.md`'s own note that pinning, not CA
  verification, is this engine's real trust boundary.
- Transcript/recording UI — still Pro/later-phase surfaces per
  `DIRECTION.md`, not part of F4 (F4 scope was specifically the loader +
  the `blf_console` capability/console window).
- Premium console's roster is favorites-only (see "Premium console window"
  above) — no real directory/CRM lookup, same limitation the main window's
  own favorites grid already has.
- Official installer packaging (dropping the signed dylib + console-ui
  assets into a `tauri build` bundle automatically) is not built — F5 in
  the product spec. Today both are placed manually for local testing (see
  "Premium module loader"/"Premium console window" above).
- Windows: untested this session (no Windows machine available - same
  caveat as `core/BUILD.md`'s own Windows CI note). `shell-build.yml`'s
  Windows job is `continue-on-error: true` for the same reason. The new
  `register_tel_handler` toggle's actual Windows-registry/Linux-`.desktop`
  behavior is therefore unverified on real hardware — see `shell/E2E.md`
  "F3 ... Known limitations".
- `centinelo://`/`tel:` activation itself (an OS-level scheme click, as
  opposed to the URL-parsing logic behind it, which is unit-tested) isn't
  e2e-verified this session — see `shell/E2E.md` "F3" for why.
- Recents/favorites/settings have no import from the v1 Electron app; this
  is a fresh v2 app with its own app-data directory and schema.

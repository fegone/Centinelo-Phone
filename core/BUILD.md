# core/ — Build

Tested on: macOS 26.5 (arm64/Apple Silicon), AppleClang 21, CMake 4.4.0,
Homebrew OpenSSL 3.6.3. The same sources are also built (best-effort, see
"Windows CI" below) on `windows-latest` in
`.github/workflows/core-build.yml`.

## 1. Toolchain

```bash
brew install cmake openssl
```

`re`/`baresip` themselves are **not** installed via brew — only build
tooling is. The engine builds from the pinned git submodules in
`core/deps/` via their own CMake build, so the exact same sources are used
on every platform (including Windows CI, where there is no brew).

## 2. Clone + submodules

```bash
git clone <this repo> && cd Centinelo-Phone
git checkout v2
git submodule update --init --recursive
```

`core/deps/re` and `core/deps/baresip` are pinned to matching tags
`v4.9.0` (re and baresip are developed in lockstep by the same upstream
team and released together; using the same version number for both is the
supported pairing — confirmed by baresip's own `CMakeLists.txt`, which
`find_package(re CONFIG REQUIRED HINTS ../re/cmake)`s a **sibling**
directory, i.e. it expects exactly the `core/deps/re` next to
`core/deps/baresip` layout used here).

## 3. Apply the local patches

```bash
git apply --directory=core/deps/re core/patches/0001-re-configurable-sip-ws-path.patch
git apply --directory=core/deps/re core/patches/0002-re-tls-fingerprint-pin.patch
git apply --directory=core/deps/baresip core/patches/0003-baresip-json-stdout-purity.patch
git apply --directory=core/deps/re core/patches/0004-re-json-stdout-purity.patch
```

These are the **only** local modifications to either submodule. They are
kept as patch files, applied on top of a clean pinned checkout, rather
than as dirty submodule commits — `core/deps/re` and `core/deps/baresip`
stay at their exact pinned upstream tag in git (`git -C core/deps/re
status` is clean after a fresh `submodule update`), so `git submodule
update` always gives you real, verifiable upstream source, and each fix
is a visible, reviewable diff. See "Findings" below for *why* patch 0001
exists (the WSS e2e test does not pass without it), "TLS verification"
below for patch 0002 (`CENT_TLS_PIN` cert pinning), and "`lg.enable_stdout`
defaults to `true`" below for patches 0003/0004 (pure-JSON-stdout, v1.1 —
0003 is the baresip-side fix: `main.c`'s new `CENT_JSON_STDOUT` gate +
`log.c`'s stderr fallback + `uag.c`'s SIP-trace redirect; 0004 is a
second, smaller re-side fix for a handful of unconditional
`re_printf()`s in `core/deps/re/src/sip/transp.c`'s WS-client connect/
send/close paths, found only by actually running the F3 e2e regression
against the real PBX after 0003 — see `core/E2E-F1.md` "F3 regression").

## 4. Build re, then baresip

```bash
# 4a. libre
cmake -S core/deps/re -B core/deps/re/build -DCMAKE_BUILD_TYPE=Release
cmake --build core/deps/re/build -j"$(sysctl -n hw.ncpu 2>/dev/null || nproc)"

# 4b. baresip - minimal, explicit module set (see "Module selection")
cmake -S core/deps/baresip -B core/deps/baresip/build \
  -DCMAKE_BUILD_TYPE=Release \
  -DMODULES="account;g711;auconv;auresamp;ausine;aufile;ice;dtls_srtp;menu" \
  -DAPP_MODULES="ctrl_json" \
  -DAPP_MODULES_DIR="$PWD/core/modules"
cmake --build core/deps/baresip/build -j"$(sysctl -n hw.ncpu 2>/dev/null || nproc)"
```

Output: `core/deps/baresip/build/baresip` (the engine binary) plus every
selected module symlinked flat into that same `build/` directory (e.g.
`ctrl_json.so`, `g711.so`, ...) — this is baresip's own CMake doing that
symlinking (see `core/deps/baresip/CMakeLists.txt`, the "Symlink modules
to build dir" post-build step, which explicitly covers both
`MODULES_DETECTED` and `APP_MODULES`), not something this repo added.
`run-spike.sh` points `module_path` at that directory, so no manual
copying/installing is needed.

On OpenSSL discovery: Homebrew's OpenSSL is keg-only and CMake does not
find it by default on a clean machine. If step 4a's configure fails with
an OpenSSL-not-found error, pass
`-DOPENSSL_ROOT_DIR="$(brew --prefix openssl@3)"` to the `cmake -S core/deps/re ...`
command. On this machine it was found automatically; the flag is included
in `.github/workflows/core-build.yml` for a clean-runner guarantee.

This whole sequence (steps 2-4) was run from a fully clean state (fresh
submodule clones, no prior build dirs) as part of this spike; no manual
fixups outside these commands were needed.

## 5. Run

```bash
CENT_EXT=1000 CENT_HOST=<pbx host> CENT_TRANSPORT=wss \
CENT_SECRET="$(python3 -c "import json;print(json.load(open('$HOME/Library/Application Support/Centinelo Phone/settings.json'))['password'])")" \
./core/run-spike.sh
```

See `core/run-spike.sh`'s header comment for every env var, and
`PROTOCOL.md` for the stdin/stdout wire format.

---

## Module selection

baresip's default `MODULES` CMake cache list enables ~80 modules, most of
which either no-op (`return()` early in their own `CMakeLists.txt`) when
an optional dependency isn't found, or are platform-specific
(`alsa`/`v4l2`/... on Linux, `wasapi`/`dshow` on Windows). For a
reproducible, minimal spike build this repo overrides `MODULES` explicitly
instead of relying on that default-everything list:

| Module | Why |
|---|---|
| `g711` | codec — matches the test endpoint's `allow=(opus\|ulaw)` (`asterisk -rx "pjsip show endpoint 1000"`), zero external deps |
| `auconv`, `auresamp` | audio format/rate glue baresip's own default config always loads |
| `ausine` | sine-wave audio **source** (`ausrc`) — no microphone / OS audio-permission needed, ideal for a headless/CI spike |
| `aufile` | writes received audio to a `.wav` as audio **player** (`auplay`) — no speaker needed |
| `ice`, `dtls_srtp` | **required**, not optional — see "webrtc=yes" finding below |
| `menu` | owns the `dial`/`accept`/`hangup` long-form commands (`modules/menu/static_menu.c`); `ctrl_json` drives these via `cmd_process_long()`, the same mechanism baresip's stock `ctrl_tcp` module uses, rather than reimplementing UA/call selection |
| `account` | loads the accounts file. **Must load after** `g711`/`ice`/`dtls_srtp`/`menu` in the config's module list — see "Findings" |
| `ctrl_json` (app module) | this repo's control channel, see `PROTOCOL.md` |

Explicitly *not* loaded: `stdio` (its keyboard/tty UI would fight
`ctrl_json` for stdin) and anything with an external media/GUI dependency
(`opus`, `gst`, `sdl`, `portaudio`, `coreaudio`, ...) — none of it is
needed for this spike, and leaving it out keeps the build free of brew/
system audio-library dependencies, which matters for Windows CI parity.

## Findings

These were all discovered by actually running the spike end-to-end
against the target PBX (FreePBX 17 / Asterisk 22 at `<pbx host>`),
not from reading docs — each one blocked a real run until fixed.

### `webrtc=yes` forces DTLS-SRTP + ICE, independent of SIP transport

`asterisk -rx "pjsip show endpoint 1000"` (read-only) shows
`webrtc: yes`, which in turn forces `media_encryption: dtls`,
`ice_support: true`, `use_avpf: true`, `rtcp_mux: true` **at the endpoint
level** — this applies to calls placed on that endpoint regardless of
which SIP signaling transport (wss or classic udp) carried the
REGISTER/INVITE. A plain RTP/AVP client without ICE/DTLS-SRTP would
register but fail to get real media, on *either* transport. So the
generated account always sets `mediaenc=dtls_srtp;medianat=ice;rtcp_mux=yes`
(see `run-spike.sh`), not only for the wss case. This matches a gotcha
already documented in the v1 app's own `README.md`: "WebRTC and SIP-UDP do
not coexist on one endpoint — hard-phone extensions need their own
endpoint decision." For v2 it means: which transport an account uses is a
signaling-layer choice, but the *media* requirements (DTLS-SRTP/ICE) here
are set by the endpoint's `webrtc` flag, not by the transport.

### `account` module must load after codec/mnat/menc modules

First run logged (to stderr):
```
account: audio codec not found: pcmu/8000/1
account: medianat not found: 'ice'
account: mediaenc not found: 'dtls_srtp'
```
`modules/account/account.c` validates the account's
`audio_codecs=`/`medianat=`/`mediaenc=` restrictions against whatever
codecs/mnat/menc are *already registered* at the moment it parses the
accounts file. Since baresip loads `module` lines in the order they
appear in the config file, `account` has to be listed **after**
`g711`/`ice`/`dtls_srtp` (which register those capabilities) or the
restrictions silently fail to bind. `run-spike.sh` generates the config
with `account` last (right before `module_app ctrl_json.so`).

### stock re/baresip hardcode the WSS/WS upgrade path to `"/"`

The actual blocker for 6a. `core/deps/re/src/sip/transp.c` builds the
outbound websocket URI as `"%s://%J/"` / `"%s://%j/"` — the path is
**always** `/`, with an upstream `/* TODO: ... http url path "test" is
temp, add config */` comment acknowledging the gap. Confirmed independent
of baresip with a raw probe:

```bash
$ printf 'GET / HTTP/1.1\r\nHost: <pbx host>:8089\r\n...' | openssl s_client -connect <pbx host>:8089 -quiet
HTTP/1.1 404 Not Found
Server: Asterisk/22.8.2
```
and read-only confirmation of the real mount point:
```
$ asterisk -rx "http show status"
Enabled URI's:
/metrics/... => Prometheus Metrics URI
/media/... => Media over Websocket
/ws => Asterisk HTTP WebSocket
```
Asterisk's `res_http_websocket` mounts at `/ws`, not `/`. TLS itself is
fine (raw `openssl s_client` completes the handshake and shows a normal,
if self-signed, cert from an internal CA — see "TLS leaf-certificate
pinning (CENT_TLS_PIN)" below, implemented in F1); the 404 is purely an
HTTP-routing mismatch, and it is not
something `sip_verify_server`/account params can route around, since the
request path isn't exposed through the account/config surface at all in
this baresip version.

**Fix**: `core/patches/0001-re-configurable-sip-ws-path.patch` reads an
optional `CENT_WS_PATH` env var at the exact point the URI is built,
defaulting to `"/"` (i.e. upstream behavior is unchanged if the var is
unset). `run-spike.sh` exports `CENT_WS_PATH=/ws` by default (override if
pointed at a server mounted elsewhere). This is a minimal, spike-stage
fix; the real fix belongs upstream (or as a proper `sip_ws_path` config
key threaded through `struct config`/`struct sip`, matching how
`sip_verify_server` is plumbed) — flagged for F1 continuation, see the
final report / repo issue.

### outbound calls need an explicit route back to the registered transport

Even after the path fix, `dial sip:*43@<pbx host>` (exactly the form
in this task's spec — no `;transport=` or `:port`) initially still failed:
```
websock: connecting to 'wss://<pbx host>:8089/ws'   <- REGISTER, OK
...
websock: connecting to 'wss://<pbx host>:443/ws'    <- the dial, wrong port
sip: websock connection closed (Protocol error [100])
```
Resolving a bare request-URI with no transport/port hint falls back to
the scheme's well-known port (`wss` → 443 — `re`'s
`sip_transp_port(SIP_TRANSP_WSS)` — see `core/deps/re/src/sip/transp.c`),
not the port the account actually registered on. Nothing listens on 443
here, so the call attempt silently redialed a dead port instead of
reusing the live registration connection. **Fix**: the generated account
also sets `outbound="sip:<host>:<port>;transport=<transport>"` (see
`run-spike.sh`), which pins an explicit proxy/route so a same-process
`dial sip:ext@host` — no transport params required, matching
`PROTOCOL.md`'s v0 command shape — routes over whichever transport is
under test. Confirmed fixed: dialing the bare `*43@<pbx host>` after
this reached `established` on both wss and udp.

### `lg.enable_stdout` defaults to `true` (v1) → pure JSON stdout (v1.1)

**v1 status (superseded by the v1.1 fix below, kept for history):**
stdout is the JSON channel (see `PROTOCOL.md`), but baresip's own
human-readable logger (`src/log.c`) defaults `enable_stdout=true` and is
only ever turned off by `-d`/daemon mode in `main.c` — which isn't usable
here (daemonizing forks/detaches, severing the stdio pipe `ctrl_json`
depends on). `ctrl_json`'s `ctrl_init()` calls `log_enable_stdout(false)`
(a public baresip API) as its first action. This helped, but did not
fully clean the stream: `ctrl_json` is always the *last* module loaded
(see "Module selection" above), so every earlier module's own
info()/debug() startup line, the banner (`main.c`, printed before any
module loads at all), and (with `-s`) raw SIP trace (`uag.c`, `re_printf()`
straight to stdout, bypassing `log.c` entirely) all still leaked onto
stdout ahead of/around `ctrl_json`'s own JSON — see the v1 `PROTOCOL.md`
"Framing" section (superseded, see current version) for the exact
"filter for lines starting with `{`" workaround this forced on every
consumer.

**v1.1 fix (patches 0003/0004, see "Apply the local patches" above):**
stdout is now *pure* NDJSON end to end, confirmed empirically (not just
by inspection) — `grep -cv '^{'` on stdout captured from a real,
full-length e2e run (register → dial → 20s ICE settle → quality_stats →
devices → set_device → hangup → quit) against the live test PBX returns
`0`, both with and without `-s` (SIP trace) — see `core/E2E-F1.md` "F3
regression" for the exact commands and captured output. Three
independent sources of stdout noise, three independent fixes, all in the
one pair of patches:

1. **The banner + all module-load logging** (`core/patches/0003-*`,
   `main.c`+`log.c`): `main.c` now checks a new env var,
   `CENT_JSON_STDOUT` (any non-empty value — `run-spike.sh` sets it by
   default, see that script's own header comment for how to opt back
   out), *before* printing the banner or doing anything else — when set,
   the banner goes to stderr instead, and `log_enable_stdout(false)`
   runs immediately after `libre_init()`, before `conf_configure()`/
   `conf_modules()` gets a chance to load a single module and log a line
   the old (stdout) way. This alone would have made every
   info()/warning()/debug() call in the *entire remaining process
   lifetime* go completely silent rather than just off-stdout — traced
   to `log.c`'s `vlog()` having no branch at all for `!enable_stdout`
   (nothing in this build's minimal module set, no `cons`/`syslog`, ever
   calls `log_register_handler()` to give it somewhere else to go) — so
   `log.c` is *also* patched: `!lg.enable_stdout` now routes to stderr
   (same color/formatting logic, different stream) instead of dropping
   the message. `ctrl_json.c`'s own `log_enable_stdout(false)` call (its
   only effect under v1) is unchanged and still runs, now typically a
   harmless no-op given `main.c` already flipped the switch — see that
   call site's own comment in `ctrl_json.c`.
2. **SIP trace with `-s`** (`core/patches/0003-*`, `uag.c`):
   `sip_trace_handler()`'s `re_printf(...)` (stdout, unconditional,
   bypasses `log.c` entirely — confirmed by reading
   `core/deps/re/src/fmt/print.c`'s `re_vprintf()` while investigating
   this) is now `re_fprintf(stderr, ...)`. Unconditional, not gated
   behind `CENT_JSON_STDOUT` — SIP trace was never a valid NDJSON stream
   on stdout to begin with, so there's no compatibility case to
   preserve, unlike the banner.
3. **The WS-client connect/send/close lines**
   (`core/patches/0004-re-json-stdout-purity.patch`,
   `core/deps/re/src/sip/transp.c`): found only by actually *running*
   the F3 e2e regression after 0003 — `grep -cv '^{'` on that first
   post-0003 run wasn't `0` yet. Three unconditional `re_printf()`s
   (`"websock: connecting to '...'"`, `"--> send"`,
   `"<...> ... websock established to ..."`) plus two more on adjacent
   error paths (`"websock_connect: %m"`, `"sip: websock connection
   closed (%m)"`) all fire during this engine's normal SIP-over-WSS
   *client* traffic (registration, every SIP message send, connection
   teardown) — all five now `re_fprintf(stderr, ...)`. A broader audit
   of every remaining `re_printf(` call site in `core/deps/re/src/`
   turned up several more (STUN/SIP message dump utilities with no
   automatic caller anywhere in the tree; H264 NAL parsing, unreachable
   with no video module loaded; two ICE/trice debug printers already
   gated behind `icem->conf.debug`/`.trace`, off by default; a PCP
   option-parsing note and a rare macOS-only TCP-ICE `EADDRINUSE` retry
   message, both real but narrow/network-topology-dependent edge cases
   this engine's actual test runs never hit; and the WS-*server* accept
   handler in the same `transp.c`, unreachable since this engine only
   ever makes outbound WS connections, never listens for inbound ones)
   — all confirmed dormant for this engine's actual usage (by reading
   each call site's guard/caller graph, not just grep) and deliberately
   left unpatched rather than growing patch scope for code this build
   never executes. See `core/patches/0004-*`'s own comments for the
   exact per-line rationale on the five that *were* patched.

### TLS verification

`sip_verify_server no` (config key, `run-spike.sh` sets it via
`CENT_VERIFY_SERVER`, default `no`) is required for this PBX's WSS
listener, which serves a self-signed cert issued by an internal CA
(CN `<pbx host>`). Confirmed via
`uag.c`: this key drives `tls_disable_verify_server()` on the dedicated
WSS TLS context (`uag.wss_tls`), independent of the plain-TLS-transport
context.

### TLS leaf-certificate pinning (`CENT_TLS_PIN`)

Implemented in F1 (`core/patches/0002-re-tls-fingerprint-pin.patch`,
`core/deps/re/src/http/client.c`). Optional env var, checked right after
the TLS handshake completes (`estab_handler()` in that file — confirmed
by reading `core/deps/re/src/tls/openssl/tls_tcp.c` while implementing
this: that handler only fires *after* `SSL_state(tc->ssl) == SSL_ST_OK`,
i.e. handshake done, cert chain check already run):

```bash
CENT_TLS_PIN="$(python3 -c "import json;print(json.load(open('$HOME/Library/Application Support/Centinelo Phone/settings.json'))['pinnedCertSha256'][0])")" \
CENT_EXT=1000 CENT_HOST=<pbx host> CENT_TRANSPORT=wss \
CENT_SECRET="..." ./core/run-spike.sh
```

- Format matches the v1 Electron app's `settings.pinnedCertSha256`
  entries: SHA256 of the leaf cert's DER bytes, hex, non-hex separators
  (`:`, spaces, ...) tolerated and stripped. `tls_peer_fingerprint()`
  (`re_tls.h`) computes the same digest baresip-side (`X509_digest()`
  over the DER encoding) that `pemToDerSha256()` in `src/main/main.js`
  computes on the Electron side — confirmed by reading both while
  implementing this, not assumed.
- **Independent of, and checked in addition to,** whatever
  `sip_verify_server`/`tls_set_verify_server()` chain-of-trust
  verification is otherwise configured — including when that's fully
  disabled (`sip_verify_server no`, this spike's default for the
  self-signed/internal-CA cert): `tls_set_verify_server()` no-ops
  completely (doesn't even call `SSL_set_verify()`) when
  `tls->verify_server` is false, so today *nothing else* checks the peer
  cert for that case without `CENT_TLS_PIN`.
- Unset (default): no-op, identical to pre-F1 behavior.
- Mismatch: the connection is rejected before any SIP traffic is sent
  over it (`try_next(conn, EAUTH)` in the patch) — surfaces as a normal
  `reg_state` `"failed"` event with `reason` containing `"Authentication
  error"`, not a crash or a hang. Verified live against the test PBX
  with both a correct pin (registers normally) and a deliberately wrong
  one (fails cleanly) — see `core/E2E-F1.md`.
- Scope: one flat env var, checked for every secure connection this
  engine's `http_client` makes — not host-keyed, not a list (unlike v1's
  array-of-pins). Sufficient for this engine's actual one-PBX-host usage;
  see `core/PROTOCOL.md` "Planned" for what a multi-host version would
  need.

## Unit tests (`cmd.c` / `dialog_info.c` / `wav_writer.c`)

`core/modules/ctrl_json/test/` is a **standalone** CMake project (own
`project()`, not part of baresip's own build tree — see that directory's
`CMakeLists.txt` for why), covering the three pieces of `ctrl_json` that
are pure/parseable-or-stdio-only without a running engine: JSON-command
decoding (`cmd.c`), dialog-info+xml parsing for BLF (`dialog_info.c`),
and the streaming WAV writer used by the v1.2 audio-tap feature
(`wav_writer.c`, added v1.2 — see `core/PROTOCOL.md` "Changes from
v1.1"). `audiotap.c`, the other new v1.2 file, is **not** here — it's
baresip-dependent throughout (aufilt registration, `struct call`/
`audio`), covered by `core/E2E-F1.md` "F4 audio tap" instead, same split
as `ctrl_json.c`'s own call-control commands. Requires `core/deps/re`
already built (step 4a above — links that exact `libre.a`, patches
included, so what's tested matches what ships).

```bash
cmake -S core/modules/ctrl_json/test -B core/modules/ctrl_json/test/build \
  -DCMAKE_BUILD_TYPE=Debug \
  -DOPENSSL_ROOT_DIR="$(brew --prefix openssl@3)"
cmake --build core/modules/ctrl_json/test/build
ctest --test-dir core/modules/ctrl_json/test/build --output-on-failure
```

With AddressSanitizer (`-DCENT_ASAN=ON`, same commands otherwise — this
is what `.github/workflows/core-build.yml`'s macOS job runs): clean, 0
findings, as of this version. Note: macOS's ASan build does **not**
support `detect_leaks` (LeakSanitizer isn't available on Darwin) —
`ASAN_OPTIONS=detect_leaks=1` will abort with "not supported on this
platform" rather than silently ignoring it; leak checking on macOS is
done separately, see "Memory safety" below.

203 checks across all three files as of this version (96 pre-v1.2 [63
pre-v1.1 + 33 v1.1's own `id` correlation / `devices`/`set_device`
tests] + 107 new in v1.2: 11 for `tap_start`/`tap_stop` decoding
(required `dir`, optional `call_id`, same shape as `dial`'s `uri`/every
other call-scoped command) + 96 for `wav_writer.c` — header field
correctness re-derived independently per test rather than trusted from
the writer's own output (magic bytes, chunk sizes, sample rate, and the
actual PCM sample bytes, byte-exact, including negative/extreme int16
values), close()-idempotence (both "close an already-finalized writer
again" and "close a writer that was never even `create()`'d"), the
"zero frames ever written" fallback-header path, and `create()`'s own
clean-failure behavior for bad inputs), including one fixture that's the
*real* dialog-info+xml body captured from the test PBX (see
`core/E2E-F1.md` scenario c) — not just synthetic ones. Two real bugs
were caught by these tests before any e2e run: the dialog-info parser
originally conflated "well-formed idle" with "unparseable garbage" (both
returned `idle`; fixed to require a `<dialog-info` root element before
concluding idle, garbage now correctly falls into the `offline`/"can't
tell" bucket), and a use-after-free in the `CENT_CMD_UNKNOWN` error path
(read the just-freed decoded JSON object to build the error message —
fixed by capturing the `cmd` string before freeing). v1.2 itself
introduced no new bugs caught this way — `wav_writer.c` passed its own
tests on the first ASan-clean run (see "Memory safety" below); the one
real mistake made while building v1.2 (a stray literal `*/` inside a
block comment in `audiotap.c`, closing it early and turning the next
line of prose into invalid C) was a *compile* error, not something this
test suite would have caught either way — caught immediately by the
real engine build (`cmake --build core/deps/baresip/build`), not by
`ctrl_json_test`.

## Memory safety

No ASan run for the *full* engine (re+baresip+ctrl_json all built
`-fsanitize=address` would be a much larger rebuild for marginal extra
coverage beyond the unit tests above, which already ASan-cover every
line ctrl_json.c added that's reachable without a live SIP stack).
Instead, the live engine process was checked with macOS's `leaks` tool
during real e2e runs against the test PBX, exercising every new command
(including repeating the full set — blf subscribe/unsubscribe,
register/unregister, hold/mute/dtmf, and the malformed/unknown-cmd error
paths — 8 times over in one process lifetime to distinguish a possible
per-call leak from a one-time allocation): consistently **1 leak, 1024
bytes, identical after 1 rep and after 8 reps** — i.e. a fixed-size,
one-time allocation (almost certainly re/baresip core init or OpenSSL's
own static state, given it doesn't scale with repeated command
traffic), not something introduced by this version's new code paths.
`leaks` itself flags the process as "not debuggable" (this binary isn't
signed with a `get-task-allow` entitlement), which limits it to
read-only introspection and blocks a full allocation-site stack trace
for that one block — the repeat-count comparison was the practical way
to get confidence without that.

**v1.2 addendum (audio tap)**: the unit-test-reachable half of this
feature (`wav_writer.c`, plus `cmd.c`'s `tap_start`/`tap_stop` decoding)
is covered by the same `ctrl_json_test` ASan run above — clean, 0
findings (203/203 checks, up from 96 pre-v1.2, see "Unit tests" above).
`audiotap.c` itself (the baresip-dependent half — aufilt registration,
the per-call tap registry) was **not** separately re-run under `leaks`
this pass; its memory ownership follows the exact same
`mem_zalloc()`/destructor/`list_append()`/`list_flush()` refcounting
shape as `blf_subs` above (already covered by the `leaks` run's own
repeat-count methodology, structurally, if not by re-running it
specifically for tap traffic) — see `audiotap.c`'s own top comment. What
*was* verified live: two full real-world runs against the real test PBX
(`core/E2E-F1.md` "F4 audio tap"), each a complete `tap_start` → ~12s
capture → `tap_stop` → `hangup` → `quit` cycle, both exiting the child
process cleanly (no crash, no hang, confirmed by the harness's own
`proc.wait()` completing) — real evidence against a crash/hang in the
live code path, just not a substitute for an actual `leaks`/ASan pass
against the *live* engine specifically exercising tap traffic, which
would be a reasonable next step before this feature carries production
weight beyond its current F4-foundation role.

## Windows CI

`.github/workflows/core-build.yml` builds `core/` on both `macos-latest`
and `windows-latest`. **The Windows job (`Windows (experimental)`) is
GATING as of 2026-07-16 — no longer `continue-on-error`.** Both jobs are
required checks on `v2`; do not regress either one. Its build log is
still uploaded as an artifact on every run (pass or fail) via
`actions/upload-artifact@v4` (`windows-core-build-logs`), useful for the
things that only show up in the raw CMake/MSVC output.

**Confirmed green run** (not "should work" — an actual passing run,
checked): [`29533068908`](https://github.com/fegone/Centinelo-Phone/actions/runs/29533068908)
(`db5f7e6`, `v2-winci` branch, 2026-07-16) — `macOS (supported)` in 1m6s,
`Windows (experimental)` in 4m34s, both ✓, with the full media module set
below (`ausine;aufile;ice;dtls_srtp;wasapi`) compiled in — confirmed from
the job's own log, not inferred: `MODULES_DETECTED=account;g711;auconv;
auresamp;ausine;aufile;ice;dtls_srtp;menu;wasapi;ctrl_json`. Prior run
([`29459035249`](https://github.com/fegone/Centinelo-Phone/actions/runs/29459035249),
`5be8dbf`) confirmed only the smaller pre-fix module set — see "Windows
media modules" below for what changed and why.

### What's actually different on Windows (read the workflow file, not this doc, for the literal commands — this section explains *why*)

1. **OpenSSL via Chocolatey, not brew**, and its install path is
   **detected at runtime, not hardcoded**. The `choco install openssl`
   package changed its deploy directory between versions — `v3.x` lands
   at `C:\Program Files\OpenSSL-Win64`, current `v4.x` at
   `C:\Program Files\OpenSSL` (no `-Win64` suffix). A prior run hardcoded
   the old path and failed (`find_package(OpenSSL)` → "missing:
   OPENSSL_CRYPTO_LIBRARY OPENSSL_INCLUDE_DIR"). The fix (this version):
   a `pwsh` step probes both candidate paths for
   `include\openssl\ssl.h` and exports whichever one exists to
   `$GITHUB_ENV` as `OPENSSL_ROOT_DIR`, which every later `cmake`
   invocation then forwards via `-DOPENSSL_ROOT_DIR="$OPENSSL_ROOT_DIR"`.
   This is genuinely Windows-only — there is no brew-path-drift
   equivalent to reproduce on macOS; the macOS job still uses
   `$(brew --prefix openssl)` directly, unchanged.

2. **`re` is built STATIC and explicitly `cmake --install`ed**, then
   handed to baresip's configure as explicit `-DRE_LIBRARY=...`/
   `-DRE_INCLUDE_DIR=...` flags, instead of relying on baresip's own
   `find_package(RE CONFIG REQUIRED HINTS ../re/cmake)` the way macOS
   does (see "Clone + submodules" above). Root cause: baresip's
   `cmake/FindRE.cmake` only searches `../re`, `../re/build`, and
   `../re/build/Debug` for the library — but MSVC's multi-config
   generator puts the `Release` build's `.lib` at `../re/build/Release`,
   a path `FindRE.cmake` never looks in, so the raw build-tree lookup
   failed with `Could NOT find RE (missing: RE_LIBRARY)`. Installing `re`
   to a known prefix (`cmake --install core/deps/re/build --config
   Release`) and pointing baresip straight at the installed
   `re-install-prefix/lib/re-static.lib` sidesteps `FindRE.cmake`'s
   search-path assumption entirely, rather than patching
   `FindRE.cmake` itself (upstream file, would need to survive the next
   `git submodule update`).

3. **`MODULES` set on Windows now matches macOS's media set, plus the
   Windows-native audio backend**: `account;g711;auconv;auresamp;ausine;
   aufile;ice;dtls_srtp;menu;wasapi`. See "Windows media modules" below
   for the full rationale (what was missing before 2026-07-16, why, and
   what a green run does/doesn't prove) — not repeated here.

4. **No runtime smoke test** — the Windows job only checks the artifacts
   exist (`test -x .../Release/baresip.exe`, `test -f
   .../re-static.lib`), it does not run `baresip.exe -h` the way the
   macOS job does. Reason: the static Windows build still links against
   OpenSSL's import libs, and the OpenSSL DLLs aren't on `PATH` at
   sanity-check time — actually invoking the binary would need that
   sorted out first. So a green Windows run today means "builds and
   links cleanly on MSVC", not "runs". The sanity step does additionally
   assert every module actually got compiled in, not just requested: for
   a `STATIC` build there's no per-module `.dll` file to `test -f` (they
   become `OBJECT` libraries baked into `baresip`'s own static lib and
   the generated `src/static.c` exports table — see
   `core/deps/baresip/CMakeLists.txt`'s `MODULES_DETECTED` handling), so
   the CI step instead greps the tee'd configure log for baresip's own
   `message("MODULES_DETECTED=...")` line and fails the job if any of
   `ausine`/`aufile`/`ice`/`dtls_srtp`/`wasapi` is missing from it — this
   is the mechanism that would have caught the pre-2026-07-16 state (or
   any future module silently dropping out via an early `return()` in its
   own `CMakeLists.txt`, e.g. an unsatisfied optional dependency) as a
   hard CI failure instead of a silently-smaller green build. The check
   is a token-exact match against the `;`-separated list (wraps both
   sides in `;` and matches `;name;`), not a plain substring check — a
   substring check would wrongly pass a module whose name is contained
   inside another present module's name.

### Windows media modules (fixed 2026-07-16)

Before this date the Windows CI `MODULES` list was
`account;g711;auconv;auresamp;menu` — a real functional gap, not a
cosmetic one: **no path to real call media at all** on Windows. Fixed by
adding `ausine;aufile;ice;dtls_srtp;wasapi` (now
`account;g711;auconv;auresamp;ausine;aufile;ice;dtls_srtp;menu;wasapi`,
matching macOS's media set plus the one genuinely-platform-specific
module).

- **`ice`, `dtls_srtp`, `ausine`, `aufile`**: read all four modules'
  source (`core/deps/baresip/modules/{ice,dtls_srtp,ausine,aufile}/*.c`)
  before enabling them — none has a `WIN32`/`_WIN32` guard in its
  `CMakeLists.txt` (unlike `wasapi`, see below), and none calls a raw
  POSIX function MSVC lacks (they use `re`'s own portable wrappers
  throughout — `pl_strcasecmp`, `re_snprintf`, `sys_msleep`, confirmed by
  grepping for `strcasecmp`/`usleep`/`alloca`/`gettimeofday`/... and
  finding none). `dtls_srtp`'s own gate (`if(NOT USE_OPENSSL) return()`)
  was already satisfiable on Windows before this fix too: baresip's
  `CMakeLists.txt` line 100 (`find_package(re CONFIG REQUIRED HINTS
  ../re/cmake)`) unconditionally re-includes
  `core/deps/re/cmake/re-config.cmake` — independent of the platform's
  `find_package(RE)` (module-mode, line 39, satisfied via the explicit
  `-DRE_LIBRARY`/`-DRE_INCLUDE_DIR` flags on Windows) — which derives
  `USE_OPENSSL` fresh from `find_package(OpenSSL)` using whatever
  `OPENSSL_ROOT_DIR` this job's own configure step passes. Since the
  Windows job already forwards `-DOPENSSL_ROOT_DIR="$OPENSSL_ROOT_DIR"`
  to the baresip configure (needed by `dtls_srtp` itself, and also by
  `re-config.cmake`'s own `find_package(OpenSSL)` call), `USE_OPENSSL`
  was already true there before this change — the module was simply
  never requested. Confirmed for real, not just by reading: the Windows
  CI run below shows `dtls_srtp` present in `MODULES_DETECTED`.
- **`wasapi`**: baresip v4.9.0 does not have a `winwave` module (checked
  `core/deps/baresip/modules/` directly — no such directory); `wasapi`
  (Windows Audio Session API, the modern backend) is the one Windows
  audio module that exists, gated `if(NOT WIN32) return()` in its own
  `CMakeLists.txt` (so requesting it on macOS/Linux would be a silent
  no-op — this is exactly the case the new `MODULES_DETECTED` assertion
  above does *not* check for, since the assertion only runs in the
  Windows job). Its four source files (`wasapi.c`, `play.c`, `src.c`,
  `util.c`) only include Windows SDK COM headers
  (`mmdeviceapi.h`/`audioclient.h`/...) plus `re`/`baresip`'s own — no
  extra CMake wiring was needed for linking either: baresip's top-level
  `CMakeLists.txt` already appends `ole32`/`oleaut32` (among others) to
  `LINKLIBS` whenever `WIN32` (line ~268), which is what WASAPI's COM
  interfaces need; the module doesn't touch MMCSS
  (`Avrt`/`AvSetMmThreadCharacteristics`), so no `avrt` link dependency
  either.
- **What this does *not* yet prove**: CI only proves these modules
  *compile and link* on MSVC (via the `MODULES_DETECTED` assertion) — it
  does not run `baresip.exe` at all (see point 4 above), so it cannot
  confirm a real WSS/ICE/DTLS-SRTP call actually completes, or that
  `wasapi` actually opens a real microphone/speaker, on real Windows
  hardware. No Windows machine was available this session (same
  constraint as "F1 status" below) — this is a CI-only, link-level
  verification, stated explicitly rather than implied.
- **Separate, real gap this does *not* close (shell-tauri's scope, not
  core-engine's)**: `shell/src-tauri/src/sidecar.rs`'s
  `write_config_file()` hardcodes `audio_source ausine,440` /
  `audio_player aufile,<scratch>/rx.wav` unconditionally, on **every**
  platform including macOS — i.e. the shell's real, shipped config never
  actually selects a real microphone/speaker device today, on any OS.
  Compiling `wasapi` in on Windows makes the module available; it does
  nothing by itself until `write_config_file()` is made
  platform-conditional (`wasapi,default` and an analogous mac path — no
  `coreaudio` module is enabled on macOS either, same file, same gap) to
  actually select it as `audio_source`/`audio_player`. Flagged to
  shell-tauri, not fixed here — out of `phone/core/` scope, and
  `shell/src-tauri/` isn't touched by this file or workflow.

**F1 status** (`ctrl_json.c`'s stdin path, still accurate, unchanged by
the above): the previously-flagged Windows blocker (`unistd.h`/`read()`/
`STDIN_FILENO`, POSIX-only) has a `_WIN32`-gated implementation (reader
thread + `fgets()` + `re_mqueue.h`, see `core/PROTOCOL.md` "Framing /
stdin" for the full design and rationale). No Windows machine was
available to run this engine interactively on real Windows hardware —
CI's static-link build is the only Windows signal that exists today.

Before pushing, the `_WIN32` branch was also sanity-checked locally with
a forced-macro syntax-only compile (no real MSVC available, but this
still parses the exact same C source with clang, catching real mistakes
in code that otherwise never gets compiled at all on a non-Windows dev
machine):

```bash
clang -fsyntax-only -D_WIN32 -Wall -Wextra \
  -I core/deps/re/include -I core/deps/baresip/include \
  core/modules/ctrl_json/ctrl_json.c
```

This caught two real bugs before they ever reached CI: a missing
`#include <stdlib.h>` (the `_WIN32` path's `malloc`/`free` calls were
implicitly declared, undetectable on the POSIX build since that path
never compiles `_WIN32`'s code at all), and `process_inbuf()` — written
as "shared" between both stdin paths — turned out to be POSIX-only in
practice (`fgets()` on the Windows side already delivers whole lines, so
it never calls the shared buffer-splitting helper), flagged as an
unused-function warning; fixed by scoping it under the same `#ifndef
_WIN32` as its only real caller and correcting the stale "shared"
comment. Neither of these would show up in the macOS job at all — worth
re-running this check after any future change to `ctrl_json.c`'s
`_WIN32` block, not just relying on the Windows CI job's own (slower,
`continue-on-error`) feedback loop. Re-run for v1.1 (this version added
no new code inside the `_WIN32` block itself, but a fair amount to the
shared, always-compiled parts of the file that block also sits in - same
command, clean, 0 warnings/errors).

There's also a real, deliberate memory-safety design point in that
`_WIN32` code worth flagging for review: the reader thread is never
`thrd_join()`'d (risk of blocking shutdown indefinitely if it's still
parked in `fgets()`), so it must never touch anything the main thread
might free first. It's built to take its own `mem_ref()`'d reference to
just the `mqueue` object (not the whole `ctrl_st`) precisely so a
concurrent teardown on the main thread can never race it into a
use-after-free — see the block comment above `stdin_thread_main()` in
`ctrl_json.c` for the full reasoning; this shape only exists because an
earlier draft got this wrong (thread held a raw, unrefcounted `ctrl_st*`)
and the syntax-check pass above doesn't catch use-after-free bugs, only
compile errors — this one was caught by re-reading the code, not
tooling.

Two risk points flagged in the previous version
of this doc remain genuinely open (unrelated to the stdin fix, not
addressed this version, still real per-item risk):
`getopt`/POSIX bits `re`/`baresip` conditionally compile around (both
projects support Windows upstream, so this is a tooling/generator
question more than a source-portability one), and whether `EXPORT_SYM`
(`__declspec(dllexport)`, see `core/deps/re/include/re_mod.h`) is
sufficient by itself for `ctrl_json.dll`'s `exports` symbol to be
discoverable via baresip's Windows module loader
(`core/deps/baresip/src/mod/...`) without also needing a `.def` file.

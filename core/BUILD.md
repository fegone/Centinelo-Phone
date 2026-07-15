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
```

These are the **only** local modifications to either submodule. They are
kept as patch files, applied on top of a clean pinned checkout, rather
than as dirty submodule commits — `core/deps/re` and `core/deps/baresip`
stay at their exact pinned upstream tag in git (`git -C core/deps/re
status` is clean after a fresh `submodule update`), so `git submodule
update` always gives you real, verifiable upstream source, and each fix
is a visible, reviewable diff. See "Findings" below for *why* patch 0001
exists (the WSS e2e test does not pass without it) and "TLS verification"
below for patch 0002 (`CENT_TLS_PIN` cert pinning).

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
CENT_EXT=1100 CENT_HOST=100.119.230.80 CENT_TRANSPORT=wss \
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
| `g711` | codec — matches the test endpoint's `allow=(opus\|ulaw)` (`asterisk -rx "pjsip show endpoint 1100"`), zero external deps |
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
against the target PBX (FreePBX 17 / Asterisk 22 at `100.119.230.80`),
not from reading docs — each one blocked a real run until fixed.

### `webrtc=yes` forces DTLS-SRTP + ICE, independent of SIP transport

`asterisk -rx "pjsip show endpoint 1100"` (read-only) shows
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
$ printf 'GET / HTTP/1.1\r\nHost: 100.119.230.80:8089\r\n...' | openssl s_client -connect 100.119.230.80:8089 -quiet
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
if self-signed, cert from `Neola Internal CA` — see "TODO: cert pinning"
below); the 404 is purely an HTTP-routing mismatch, and it is not
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

Even after the path fix, `dial sip:*43@100.119.230.80` (exactly the form
in this task's spec — no `;transport=` or `:port`) initially still failed:
```
websock: connecting to 'wss://100.119.230.80:8089/ws'   <- REGISTER, OK
...
websock: connecting to 'wss://100.119.230.80:443/ws'    <- the dial, wrong port
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
under test. Confirmed fixed: dialing the bare `*43@100.119.230.80` after
this reached `established` on both wss and udp.

### `lg.enable_stdout` defaults to `true`

stdout is the JSON channel (see `PROTOCOL.md`), but baresip's own
human-readable logger (`src/log.c`) defaults `enable_stdout=true` and is
only ever turned off by `-d`/daemon mode in `main.c` — which isn't usable
here (daemonizing forks/detaches, severing the stdio pipe `ctrl_json`
depends on). `ctrl_json`'s `ctrl_init()` calls `log_enable_stdout(false)`
(a public baresip API) as its first action. This helps, but does not
fully clean the stream — see `PROTOCOL.md` "Framing" for what's left and
why, and how a consumer should handle it (filter for lines starting with
`{`).

### TLS verification

`sip_verify_server no` (config key, `run-spike.sh` sets it via
`CENT_VERIFY_SERVER`, default `no`) is required for this PBX's WSS
listener, which serves a self-signed cert issued by an internal CA
("Neola Internal CA" / CN `neola-pbx.tail0fc359.ts.net`). Confirmed via
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
CENT_EXT=1100 CENT_HOST=100.119.230.80 CENT_TRANSPORT=wss \
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

## Unit tests (`cmd.c` / `dialog_info.c`)

`core/modules/ctrl_json/test/` is a **standalone** CMake project (own
`project()`, not part of baresip's own build tree — see that directory's
`CMakeLists.txt` for why), covering the two pieces of `ctrl_json` that
are pure/parseable without a running engine: JSON-command decoding
(`cmd.c`) and dialog-info+xml parsing for BLF (`dialog_info.c`). Requires
`core/deps/re` already built (step 4a above — links that exact
`libre.a`, patches included, so what's tested matches what ships).

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

63 checks across both files as of this version, including one fixture
that's the *real* dialog-info+xml body captured from the test PBX (see
`core/E2E-F1.md` scenario c) — not just synthetic ones. Two real bugs
were caught by these tests before any e2e run: the dialog-info parser
originally conflated "well-formed idle" with "unparseable garbage" (both
returned `idle`; fixed to require a `<dialog-info` root element before
concluding idle, garbage now correctly falls into the `offline`/"can't
tell" bucket), and a use-after-free in the `CENT_CMD_UNKNOWN` error path
(read the just-freed decoded JSON object to build the error message —
fixed by capturing the `cmd` string before freeing).

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

## Windows CI

`.github/workflows/core-build.yml` builds `core/` on both `macos-latest`
and `windows-latest`. The Windows job is marked
`continue-on-error: true` (allowed to fail) and its build log is uploaded
as an artifact either way — see that workflow file for the exact commands
(same submodule + both patches + CMake sequence as above).

**F1 status**: `ctrl_json.c`'s stdin path — the specific, previously-
flagged Windows blocker (`unistd.h`/`read()`/`STDIN_FILENO`, POSIX-only)
— now has a `_WIN32`-gated implementation (reader thread + `fgets()` +
`re_mqueue.h`, see `core/PROTOCOL.md` "Framing / stdin" for the full
design and rationale). No Windows machine was available to run this
engine on real Windows hardware, so this is **compile-verified via CI
only, not run-verified**; `continue-on-error` stays `true` until an
actual green `windows-latest` run is confirmed (check the Actions run
for this push before flipping it — do not flip it on the strength of
local reasoning alone).

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
`continue-on-error`) feedback loop.

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

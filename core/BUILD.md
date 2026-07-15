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

## 3. Apply the local patch

```bash
git apply --directory=core/deps/re core/patches/0001-re-configurable-sip-ws-path.patch
```

This is the **only** local modification to either submodule. It is kept
as a patch file, applied on top of a clean pinned checkout, rather than as
a dirty submodule commit — `core/deps/re` and `core/deps/baresip` stay at
their exact pinned upstream tag in git (`git -C core/deps/re status` is
clean after a fresh `submodule update`), so `git submodule update` always
gives you real, verifiable upstream source, and the fix is a visible,
reviewable diff. See "Findings" below for *why* this patch exists — it is
not cosmetic, the WSS e2e test in step 6a does not pass without it.

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

**TODO (cert pinning, not done in this spike)**: the v1 Electron app's
settings already carry a `pinnedCertSha256` for this exact cert; a real F2
implementation should pin that hash in the engine too (baresip's TLS
layer supports `tls_add_cafile_path`, so pinning the CA rather than
disabling verification entirely is realistic) instead of running with
verification off indefinitely.

## Windows CI

`.github/workflows/core-build.yml` builds `core/` on both `macos-latest`
and `windows-latest`. The Windows job is marked
`continue-on-error: true` (allowed to fail) and its build log is uploaded
as an artifact either way — see that workflow file for the exact commands
(same submodule + patch + CMake sequence as above, with
`-DSTATIC=ON` since baresip's own `CMakeLists.txt` defaults `STATIC` to
`ON` on `WIN32`, and MSVC-appropriate generator flags). This spike did not
have a Windows machine to validate the job locally before pushing; its
actual pass/fail state on `windows-latest` is unverified as of this
commit — that is the literal, explicit ask in task step 8 ("Windows may
legitimately fail at this stage"). Known likely Windows risk points, for
whoever picks this up: `getopt`/POSIX bits `re`/`baresip` conditionally
compile around (both project support Windows upstream, so this is a
tooling/generator question more than a source-portability one),
`unistd.h`/`read()`/`STDIN_FILENO` in `ctrl_json.c` (POSIX-only — a real
Windows build of `ctrl_json` needs a `ReadFile`/`_read` + fd_listen (or
equivalent) path; **not implemented in this v0** — flagged as a
Windows-specific follow-up, not covered by the `CENT_WS_PATH` patch or
anything else in this spike), and whether `EXPORT_SYM`
(`__declspec(dllexport)`, see `core/deps/re/include/re_mod.h`) is
sufficient by itself for `ctrl_json.dll`'s `exports` symbol to be
discoverable via baresip's Windows module loader (`core/deps/baresip/src/mod/...`)
without also needing a `.def` file — unverified here.

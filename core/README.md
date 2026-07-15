# core/ — Centinelo Phone v2 engine (F1 tracer bullet)

This directory is the start of the v2 monorepo layout: a native SIP engine
(C, [baresip](https://github.com/baresip/baresip)/[libre](https://github.com/baresip/re),
BSD-licensed) running as a sidecar process, controlled over a small
JSON-over-stdio protocol. A Tauri shell will eventually drive this sidecar
as its SIP backend (not built here — this is the engine spike only).

Everything under `core/` is new. The v1 Electron app at the repo root
(`src/`, `extension/`, `package.json`, ...) is untouched and keeps working
independently; this directory does not depend on it and nothing here is
wired into its build.

## Layout

```
core/
├── README.md          this file
├── BUILD.md            exact, from-clean-clone build steps + findings
├── PROTOCOL.md          the v0 JSON control protocol (implemented + planned)
├── run-spike.sh          launches baresip with a generated scratch config
├── deps/
│   ├── re/               git submodule, github.com/baresip/re, pinned v4.9.0
│   └── baresip/           git submodule, github.com/baresip/baresip, pinned v4.9.0
├── patches/
│   └── 0001-re-configurable-sip-ws-path.patch
│                          small, documented patch applied to deps/re after
│                          submodule checkout - see BUILD.md "Findings" for
│                          why it's needed (Asterisk's SIP-over-WSS listener
│                          isn't mounted at "/", which stock re hardcodes)
└── modules/
    └── ctrl_json/          our out-of-tree baresip "application" module:
                            newline-delimited JSON commands on stdin,
                            newline-delimited JSON events on stdout
```

## Why baresip/libre

- BSD-licensed, C, genuinely cross-platform (the same source builds on
  macOS/Linux/Windows), and it is the reference implementation many
  WebRTC-to-SIP gateways are built on — it natively supports SIP over
  UDP/TCP/TLS *and* over WSS (RFC 7118), plus ICE + DTLS-SRTP, which this
  PBX's endpoints require (see BUILD.md "Findings" — `webrtc=yes` forces
  `media_encryption=dtls` + `ice_support=yes` regardless of which SIP
  transport carries the signaling).
- Modules are a first-class, supported extension point: baresip's own
  CMake build has an `APP_MODULES`/`APP_MODULES_DIR` mechanism specifically
  for building an out-of-tree module (like `ctrl_json`) against the pinned
  submodule sources without forking/patching baresip's own module list.
  `core/modules/ctrl_json/CMakeLists.txt` uses exactly that mechanism.

## Quick start

See `BUILD.md` for the full, from-clean-clone build. Once built:

```bash
CENT_EXT=1100 \
CENT_HOST=100.119.230.80 \
CENT_TRANSPORT=wss \
CENT_SECRET="$(...)" \
./core/run-spike.sh
```

stdin/stdout speak the protocol in `PROTOCOL.md`. `run-spike.sh`'s own
`--help`-equivalent (its header comment) documents every env var.

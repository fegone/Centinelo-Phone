# core/ — Centinelo Phone v2 engine

This directory is the v2 monorepo layout: a native SIP engine
(C, [baresip](https://github.com/baresip/baresip)/[libre](https://github.com/baresip/re),
BSD-licensed) running as a sidecar process, controlled over a small
JSON-over-stdio protocol. A Tauri shell will eventually drive this sidecar
as its SIP backend (not built here — this is the engine only).

Everything under `core/` is new. The v1 Electron app at the repo root
(`src/`, `extension/`, `package.json`, ...) is untouched and keeps working
independently; this directory does not depend on it and nothing here is
wired into its build.

**Status: F1 complete** — full call control (dial/answer/hangup/hold/
resume/mute/DTMF/blind+attended transfer), BLF presence, RTCP quality
stats, runtime register/unregister, TLS cert pinning, and a
Windows-portable stdin path, all e2e-verified against the real test PBX.
See `PROTOCOL.md` (v1, the wire protocol) and `E2E-F1.md` (the
verification evidence).

## Layout

```
core/
├── README.md          this file
├── BUILD.md            exact, from-clean-clone build steps + findings
├── PROTOCOL.md          the v1 JSON control protocol (implemented + planned)
├── E2E-F1.md            F1 end-to-end verification evidence (real PBX)
├── run-spike.sh          launches baresip with a generated scratch config
├── deps/
│   ├── re/               git submodule, github.com/baresip/re, pinned v4.9.0
│   └── baresip/           git submodule, github.com/baresip/baresip, pinned v4.9.0
├── patches/
│   ├── 0001-re-configurable-sip-ws-path.patch
│   │                      small, documented patch applied to deps/re after
│   │                      submodule checkout - see BUILD.md "Findings" for
│   │                      why it's needed (Asterisk's SIP-over-WSS listener
│   │                      isn't mounted at "/", which stock re hardcodes)
│   └── 0002-re-tls-fingerprint-pin.patch
│                          adds CENT_TLS_PIN (optional leaf-cert SHA256
│                          pin, independent of chain-of-trust verification)
│                          to deps/re's http client - see BUILD.md "TLS
│                          leaf-certificate pinning"
└── modules/
    └── ctrl_json/          our out-of-tree baresip "application" module:
        ├── ctrl_json.c        newline-delimited JSON commands on stdin,
        │                      newline-delimited JSON events on stdout -
        │                      the module glue (init/close, event relay,
        │                      command dispatch, BLF/transfer state)
        ├── cmd.c / cmd.h        pure JSON-command decoding, no baresip/
        │                      SIP-stack dependency - unit tested
        ├── dialog_info.c / .h    pure dialog-info+xml (BLF) parsing, same
        │                      - unit tested, incl. against a real
        │                      captured NOTIFY body
        └── test/                standalone CMake project + ctest for the
                               two pure files above - see BUILD.md
                               "Unit tests"
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

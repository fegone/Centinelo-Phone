# Centinelo Phone

Centinelo Phone is a modern, **open-core** desktop softphone for Windows and
macOS. This repository's `v2` branch is a from-scratch rewrite of the original
Electron app: a **native SIP engine in C** (built on
[baresip](https://github.com/baresip/baresip)/[libre](https://github.com/baresip/re))
paired with a lightweight **Tauri v2 desktop shell** in Rust. No Electron, no
bundled browser — just a small, fast, memory-safe native binary.

> **Looking for the stable app?** The original v1 **Electron** softphone still
> lives on the [`main`](../../tree/main) branch, and its releases are the
> currently shipped installers. The `v2` branch is **active development** — see
> [Project status](#project-status) below for what works today.

---

## What Centinelo Phone is (honestly)

Centinelo Phone is a SIP softphone. It places and receives voice calls over a
SIP PBX, and aims to be a quiet, dependable tool for people who spend their day
on the phone. Some of the headline features are **already working end-to-end**;
others are **in progress** and are labeled as such — this repo does not ship
dead buttons or invented capability claims.

### Headline features

| Feature | Status |
|---|---|
| **Dual-transport SIP** — UDP/TCP/TLS (classic) **and** WSS ([RFC 7118](https://www.rfc-editor.org/rfc/rfc7118)), with automatic start-time transport fallback (WSS → UDP) | ✅ Working |
| **HD codecs** — Opus, G.722, G.711 (µ-law/A-law), negotiated with the PBX | ✅ Working |
| **Full call control** — dial / answer / hang up / hold / resume / mute / DTMF (RFC 2833) / blind + attended transfer | ✅ Working |
| **Call quality stats** — RTCP-derived packet loss, jitter, round-trip | ✅ Working |
| **TLS leaf-certificate pinning** — independent SHA-256 pin check (`CENT_TLS_PIN`) | ✅ Working |
| **BLF presence** — SUBSCRIBE `Event: dialog` (RFC 4235), dialog-info+xml parsing, idle/ringing/busy/offline lamps | 🚧 In progress (engine ready, shell wiring in progress) |
| **Receptionist console** (Pro) — multi-line BLF grid / call-park / transfer console | 🚧 In progress |
| **Local on-device AI transcription** (Pro) — via [whisper.cpp](https://github.com/ggerov/whisper.cpp), audio never leaves the machine | 🔜 Coming |
| **Offline license model** — Community edition is fully free and standalone; Pro features are unlocked by a local license, no cloud call-home | 🔜 Coming |

The SIP engine itself (the part under [`core/`](core/README.md)) is verified
end-to-end against a real SIP PBX for every feature marked ✅ — see
[`core/PROTOCOL.md`](core/PROTOCOL.md) (the wire protocol) and `core/E2E-F1.md`
(the captured verification evidence).

---

## Architecture

```
 ┌──────────────────────────────┐        newline-delimited JSON
 │  Tauri v2 shell  (Rust)      │  stdin  commands   ──────────────┐
 │  ┌────────────────────────┐  │ ◀────────────────────────────────┤
 │  │  native window (UI)    │  │                                   │
 │  │  settings, tray,       │  │  stdout events   ──────────────┐ │
 │  │  sidecar supervisor    │  │ ───────────────────────────────▶│
 │  └────────────────────────┘  │                                   │
 └──────────────┬───────────────┘                                   │
                │ spawns + supervises (auto-restart w/ backoff)     │
                ▼                                                   │
 ┌──────────────────────────────┐                                   │
 │  core  — native SIP engine   │   (baresip + libre, BSD-licensed) │
 │  UDP/TCP/TLS/WSS · ICE/DTLS  │                                   │
 │  Opus/G.722/G.711 · BLF      │                                   │
 └──────────────┬───────────────┘                                   │
                │  Premium modules (receptionist console,            │
                │  local transcription, licensing) ship only in      │
                │  Official builds — see editions below.             │
                ▼                                                   
        your SIP PBX ◀─────────── standard SIP/RTP/RTCP ─────────────┘
```

The shell and the engine are two separate processes that talk over a tiny
**JSON-over-stdio** protocol — one JSON command per line in, one JSON event per
line out. That clean seam is what lets the engine be built and tested
completely independently of any UI.

- **`core/`** — the native SIP engine. C99, baresip/libre, an out-of-tree
  `ctrl_json` application module that turns baresip into a stdin/stdout sidecar.
  See [`core/README.md`](core/README.md), [`core/PROTOCOL.md`](core/PROTOCOL.md),
  [`core/BUILD.md`](core/BUILD.md).
- **`shell/`** — the Tauri v2 desktop app (Rust backend, static HTML/CSS/JS
  frontend). It spawns the engine, pipes JSON to/from it, and renders the UI.
  See [`shell/README.md`](shell/README.md).

---

## Editions: Community vs Official/Pro

Centinelo Phone is **open-core**.

- **Community edition (this repo, free, MIT).** Cloning and building this
  repository as documented below produces the **full free Community edition** —
  a complete, standalone softphone with all the working features listed above.
  There is no feature flag, no time bomb, no phone-home.
- **Official / Pro edition.** "Official" builds are the signed, distributable
  binaries published by the project; they additionally bundle the **Pro
  modules**: the **receptionist console**, **local on-device AI transcription**,
  and **offline licensing**. Those modules are developed separately and are not
  part of this open-source repository. The engine and shell here are the same
  foundation both editions are built on.

There is no per-user pricing detail in this public repo — only the generic
free-Community / paid-Pro split above.

---

## Build quickstart

Centinelo Phone is a monorepo: build the C engine first, then the Tauri shell.
Full, from-clean-clone instructions (including the pinned submodules and the
two documented `re` patches) live in **[`core/BUILD.md`](core/BUILD.md)** and
**[`shell/README.md`](shell/README.md)**. The short version:

```bash
# 0. prerequisites (macOS shown; see core/BUILD.md for Windows)
brew install cmake openssl

# 1. clone + pinned submodules + patches
git clone https://github.com/fegone/Centinelo-Phone && cd Centinelo-Phone
git checkout v2
git submodule update --init --recursive
git apply --directory=core/deps/re core/patches/0001-re-configurable-sip-ws-path.patch
git apply --directory=core/deps/re core/patches/0002-re-tls-fingerprint-pin.patch

# 2. build the libre + baresip engine (see core/BUILD.md for the exact CMake flags)
cmake -S core/deps/re      -B core/deps/re/build      -DCMAKE_BUILD_TYPE=Release
cmake --build core/deps/re/build -j"$(sysctl -n hw.ncpu 2>/dev/null || nproc)"
cmake -S core/deps/baresip -B core/deps/baresip/build -DCMAKE_BUILD_TYPE=Release \
  -DMODULES="account;g711;auconv;auresamp;ausine;aufile;ice;dtls_srtp;menu" \
  -DAPP_MODULES="ctrl_json" -DAPP_MODULES_DIR="$PWD/core/modules"
cmake --build core/deps/baresip/build -j"$(sysctl -n hw.ncpu 2>/dev/null || nproc)"

# 3. run the shell
cd shell && npm install && npm run dev   # = tauri dev
```

Unit tests for the engine's pure JSON-command decoding and BLF dialog-info
parsing live in `core/modules/ctrl_json/test/` — see `core/BUILD.md`
("Unit tests") to run them.

---

## Project status

| Component | Status | Notes |
|---|---|---|
| **`core/` SIP engine** | ✅ Working end-to-end | Full call control, BLF SUBSCRIBE/NOTIFY parsing, RTCP stats, runtime register/unregister, TLS pinning — verified against a real PBX. Windows portability work done; Windows CI still resolving an upstream `re`/`baresip` CMake packaging issue (`continue-on-error`). |
| **`shell/` desktop app** | ✅ Working end-to-end | Spawn/supervise the sidecar, dial/answer/hangup, settings + admin-lock, tray, light/dark themes — verified with real calls. |
| **BLF presence UI** | 🚧 In progress | Engine emits live `blf` events; shell favorites are currently static/dial-only. |
| **Receptionist console** (Pro) | 🚧 In progress | Pro module, not in this repo. |
| **Local AI transcription** (Pro) | 🔵 Coming | whisper.cpp, fully on-device. |
| **Offline licensing** (Pro) | 🔵 Coming | Pro module, not in this repo. |

See `core/PROTOCOL.md` "Planned" for the engine's own tracked TODOs.

---

## Branches & history

| Branch | What it is |
|---|---|
| **`main`** | The original **v1 Electron** softphone — the currently stable, released app. Its `v*` tags are the published installers (the auto-update feed). |
| **`v2`** | **Active development** of the native rewrite described in this README. |
| **`v2-docs`** | Public docs (this README + `CONTRIBUTING.md`) for v2. |

The v1 Electron app (`src/`, `extension/`, `package.json`, ...) present at the
repo root is untouched by v2 and keeps working independently on `main`. The v2
code under `core/` and `shell/` does not depend on it.

---

## Credits

Centinelo Phone v2 stands on the shoulders of excellent open-source projects:

- **[baresip](https://github.com/baresip/baresip)** / **[libre](https://github.com/baresip/re)** — the modular SIP user-agent and its portable SIP/SDP/HTTP/TLS library (BSD-2-Clause). The v2 engine is a baresip application module built against pinned upstream sources.
- **[Tauri](https://tauri.app)** (v2) — the Rust desktop shell framework, MIT/Apache-2.0.
- **[whisper.cpp](https://github.com/ggerov/whisper.cpp)** — high-performance CPU inference for local on-device transcription (Pro, coming), MIT.

## License

[MIT](LICENSE) © Felix Gonzalez.

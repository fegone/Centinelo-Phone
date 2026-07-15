# Centinelo Phone

Native desktop softphone (SIP over WSS / WebRTC) for front-desk / small-office use. **Windows first, macOS next** — one codebase (Electron). The SIP engine is a battle-tested core ported from a production Chrome-extension softphone, freed from the extension constraints that made deployment fragile.

## Why native (vs the Chrome extension)

| Extension pain | Centinelo |
|---|---|
| Widget lost on `chrome://` tabs, popup fallback hacks | Own always-available window + tray |
| Dev-mode unpacked loading, Chrome Web Store account | Signed-per-release NSIS installer, auto-update |
| Extension ID differs per PC | Stable app identity |
| Mic permission onboarding page hack | One OS-level grant |
| No global hotkeys, no autostart | Both, first-class |

## Features

- **SIP over WSS** (SIP.js 0.21.2) with reconnection backoff + burst circuit breaker
- Dial / answer / hang up / mute / **hold (re-INVITE)** / DTMF (RFC 4733)
- **Blind + attended transfer**
- **BLF busy lamps** (4 favorites, SUBSCRIBE `Event: dialog`) — green/amber/red
- **Call waiting**: second incoming line with beep, answer+auto-hold, **swap**
- Redial + call history (30 days, local)
- **Audio device routing**: mic / speaker / **separate ringer device**, live hot-swap mid-call
- Echo cancellation / noise suppression / AGC toggles
- **DND**, opt-in **auto-answer** (headset workflows)
- **Global hotkeys**: answer, hang up, dial clipboard number
- **Click-to-call**: `centinelo://` + optional `tel:` handler + companion browser extension (`extension/`) that detects numbers on any page and dials via a token-guarded localhost bridge
- Patient lookup on incoming calls (optional HTTP lookup, degrades silently)
- Tray app: close = hide, start with Windows, always-on-top, missed-call badge
- Incoming/missed OS notifications

### Codecs

The WebRTC engine negotiates **Opus, G.722 (HD), G.711 (ulaw/alaw)**. **G.729**: browsers/Electron do not ship the codec — the correct place for it is the PBX. Asterisk transcodes the WebRTC leg (Opus/ulaw) to G.729 on trunks that require it (`codec_g729` module; the patent expired in 2017). On LAN/Tailscale legs Opus outperforms G.729 in every dimension, so nothing is lost client-side.

## Development

```bash
npm install
npm start          # run the app
npm test           # smoke checks (syntax, vendor bundle, build config)
```

## Build

```bash
npm run dist:win   # NSIS .exe (cross-compiles from macOS too)
npm run dist:mac   # DMG
```

CI (`.github/workflows/build.yml`): every push builds the Windows installer; pushing a `v*` tag publishes it to GitHub Releases (which is also the auto-update feed).

## Deploy notes

1. If your PBX WSS certificate is signed by a private/internal CA, install that CA cert into **Trusted Root Certification Authorities** on each PC. Fallback: pin the leaf cert sha256 in settings (`pinnedCertSha256`).
2. PBX endpoint needs `webrtc=yes`, `max_contacts=2`, `allow=opus,ulaw` (FreePBX 17 / Asterisk 22). ⚠️ WebRTC and SIP-UDP do not coexist on one endpoint — hard-phone extensions need their own endpoint decision.
3. Companion extension: load `extension/` (or pack it), paste the bridge token from Settings → Hotkeys & integration.

## Provenance

SIP engine, BLF, reconnect/circuit-breaker logic ported from a production Chrome-extension softphone, including its hard-won gotchas: UA identity guards against register/unregister loops, `sessionDescriptionHandlerOptions` constraints placement, `userAgentString` build signature for PBX-side version checks.

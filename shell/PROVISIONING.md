# Auto-provisioning

Spec §5: "paste URL or scan QR -> fetch config JSON -> registered in ~30s."
Answers Edgar's "easy for whoever installs it" - a front-desk operator
shouldn't have to hand-type a host/extension/secret from a sticky note.

Implementation: `src-tauri/src/provisioning.rs` (parsing/validation/fetch/
apply) + `src-tauri/src/deeplink.rs` (the `centinelo://provision` deep-link
entry point) + `ui/index.html`/`ui/js/app.js` (`#setup-prompt`'s paste field
and the `#provision-confirm-overlay` confirmation screen).

## The config JSON

```json
{
  "version": 1,
  "host": "pbx.example.test",
  "ext": "1001",
  "secret": "changeme",
  "display_name": "Front Desk",
  "transport_priority": "auto",
  "tls_pin_sha256": "AA:BB:CC:...:99"
}
```

| Field | Required | Notes |
|---|---|---|
| `version` | no (defaults `1`) | Schema version, for future evolution. |
| `host` | yes | Hostname or IP. Allowed characters: alnum, `.` `-` `:` `[` `]` (covers plain hostnames, IPv4, and bracketed IPv6). Max 253 chars. |
| `ext` | yes | The SIP extension/username. Allowed characters: alnum, `.` `-` `_` `*` `#` `+`. Max 64 chars. |
| `secret` | yes | The SIP auth password. No control characters, no `;`. Max 256 chars. Never sent to the frontend at any point (see "Security" below). |
| `display_name` | no | Shown in the UI (`me-name`/`me-plate`, favorites-style). No control characters. Max 128 chars. |
| `transport_priority` | no (defaults `auto`) | One of `auto` \| `wss` \| `classic` - the exact same three values `AccountSettings.transport_priority` already uses (`src-tauri/src/settings.rs`), matching the three transport cards in `premium/design/mockups/onboarding.html`'s "Step 2 - How calls travel": `auto` = WSS, falls back to classic UDP once if registration fails; `wss` = secure web only; `classic` = plain UDP (see `sidecar.rs` `choose_transport`). |
| `tls_pin_sha256` | no | Hex SHA-256 leaf-certificate fingerprint, colons optional (64 hex chars once colons are stripped). Applied to the sidecar as `CENT_TLS_PIN` - the exact env var `core/PROTOCOL.md` already documents ("CENT_TLS_PIN is one flat env var... checked for every TLS/WSS connection"). |

Everything not in this table is ignored (unknown fields are simply not
read) - a future field can be added to this schema without breaking older
shell builds parsing a newer config.

### Not supported yet

- **Custom CA / full certificate chain.** `core` only supports a single
  leaf-cert SHA-256 pin today (`core/PROTOCOL.md`'s TLS section) - there's
  no field for a CA bundle here because the engine has nothing to do with
  one yet. Adding a config field the engine can't act on would be a silent
  no-op, not a real feature. If a deployment needs full CA pinning, that's
  a request to file with core-engine, not something to fake at the config
  level.
- Any field is a **path** (a certificate file path, a save-location path,
  etc). Deliberate: this config can come from a URL an operator merely
  pasted, or from a `centinelo://provision?url=...` deep link someone else
  sent them - accepting a filesystem path from that source and having the
  shell read it would be a path-traversal/arbitrary-file-read primitive.
  See `provisioning.rs`'s module doc, "Why the config format has no
  file-path fields".

## The link forms

Three ways to hand the shell a config, all funneled through the same
`resolve_input()` (parse -> fetch if needed -> validate):

1. **A bare `https://` link** - the common case. Paste it into the
   onboarding field ("Paste your provisioning link"), or reach it via
   `centinelo://provision?url=<percent-encoded https url>`. Either way the
   shell does a plain `GET` and expects the response body to be the JSON
   config directly (any `Content-Type` - not required to be
   `application/json`). Response capped at 16 KiB, 8s timeout, no
   automatic redirect following (a redirect surfaces as an error asking
   for the final link instead of silently following it).
2. **`centinelo://provision?url=<https url>`** - same fetch as above,
   reached through the OS's `centinelo:` protocol handler (or a
   second-instance launch while the app is already running) instead of a
   manual paste. See `deeplink.rs` for the existing `tel:`/`centinelo:`
   dial-link plumbing this reuses; provisioning links are routed away from
   that dial-target extraction before it ever runs.
3. **`centinelo://provision?config=<base64url, no padding, JSON>`** - the
   config embedded directly in the link, no network fetch at all. Exists
   so this module's happy path is testable without a live server, and so
   a future QR code (see "QR" below) can encode something that works
   fully offline.

`http://` is rejected outright everywhere it could appear (the bare link
and the `url=` fetch target) - the response contains a SIP password, and a
plain HTTP fetch would put that on the wire in the clear the first time
it's used.

### Worked examples

```text
https://provision.example.test/front-desk.json

centinelo://provision?url=https%3A%2F%2Fprovision.example.test%2Ffront-desk.json

centinelo://provision?config=eyJob3N0IjoicGJ4LmV4YW1wbGUudGVzdCIsImV4dCI6IjEwMDEiLCJzZWNyZXQiOiJ4In0
```

(The third example's payload decodes to
`{"host":"pbx.example.test","ext":"1001","secret":"x"}` - a minimal valid
config with every optional field defaulted.)

## Flow

1. Operator pastes a link (`#prov-input` + Connect) **or** clicks a
   `centinelo://provision` link (OS handler / second-instance launch).
2. Shell resolves it: parses, fetches over https if it's a `url=`/bare-https
   link, validates every field. The **secret never reaches the frontend**
   at this step or any other - the resolved config is held server-side
   (`ProvisioningPending`, a session-only in-memory slot, same shape as
   `AdminSession`) and only a secret-free preview
   (`ProvisioningPreviewView`: host, ext, display_name, transport_priority,
   `has_tls_pin`) comes back to the UI.
3. UI shows the confirmation screen ("Connect to this phone system?") with
   the preview - explicitly *not* the secret, matching the mockup's
   "Treat it like a password" whisper line.
4. Operator confirms (`provisioning_apply`) or cancels
   (`provisioning_cancel`, discards the pending config). Applying writes
   the resolved config into `AccountSettings` (same store/file the manual
   Settings form writes to, `settings.json`, mode 600) and restarts the
   sidecar - registration typically completes within a few seconds after
   that, well inside the spec's "~30s" budget end to end (paste -> fetch
   -> confirm -> registered).

## Admin lock

Provisioning changes the account (a sensitive setting, same category as
the manual Settings form's host/extension/secret fields) - so it follows
the same admin-lock rule *except* for one carve-out:

- **First provisioning on a clean install** (no account configured yet):
  **not** admin-gated. A fresh install typically has no admin password set
  either (`AdminSettings.password_hash` is `None` until the operator
  explicitly sets one) - requiring unlock here would strand a new install
  behind a password screen it has no way to satisfy yet. This mirrors
  exactly how the app is usable at all before any admin password exists
  today (see `commands.rs` `require_unlocked`'s callers - none of them run
  before an account exists in the current UI either).
- **Re-provisioning an already-configured install**: admin-gated, exactly
  like a manual edit in Settings. Provisioning isn't a lower-privilege way
  to change the account than typing it in by hand - see
  `commands::provisioning_apply`'s doc comment for the exact check
  (`settings.snapshot().account.is_configured()`).

## Security notes

- **Account-line injection**: `sidecar.rs`'s `write_accounts_file`
  interpolates `host`/`ext`/`secret` unquoted into a single-line baresip
  accounts entry. `validate()` rejects `;` and control characters (including
  newlines) in `secret`, and a restrictive character allowlist for `host`/
  `ext` - specifically closing off breaking out of `auth_pass=...;` or
  injecting an extra accounts-file line. Covered by
  `provisioning.rs`'s `secret_with_semicolon_rejected_account_line_injection`/
  `secret_with_newline_rejected_account_line_injection`/`host_with_semicolon_rejected`/
  `ext_with_at_sign_rejected` tests.
- **https-only**, no redirect following, 16 KiB response cap, 8s timeout -
  see "The link forms" above.
- **No file-path fields** in the config at all - see "Not supported yet".
- The secret is validated, applied to `settings.json` (mode 600, same as
  every other account write), and handed to the sidecar as a scratch
  `accounts` file (mode 0700 dir) - never logged, never returned to the
  frontend, never included in a preview.

## QR

**Out of scope for this task**, per the spec's own priority ("the QR is
stretch (requires webcam, rare in reception)... prioritize URL/deep-link").
Not implemented at all - no webcam capture, no image-file decode.

What *is* in place for a future QR feature to build on: the
`centinelo://provision?config=<base64url json>` link form (see "The link
forms" #3) is exactly the kind of short, self-contained payload a QR code
would encode - a future QR feature only needs to decode an image into that
same link string and hand it to the existing `resolve_input()`/onboarding
flow, no new backend plumbing. If/when it's built, per the task's explicit
guidance it should decode a pasted/uploaded image file, not drive a live
webcam capture loop (out of this shell's scope either way, see `shell/E2E.md`
"never GUI-automation" for the same reasoning applied to testing - a
picker/webcam loop can't be driven by the scripted e2e driver either).

## e2e (scripted driver, see shell/E2E.md)

`CENTINELO_E2E_SCRIPT` steps (see `src-tauri/src/e2e.rs`):

- `provisioning_resolve:<link>` - resolves (embedded config needs no
  network; a `url=`/bare-https link does a real fetch). Logs the resolved
  preview or the error.
- `provisioning_apply` - applies whatever's currently pending.
- `provisioning_cancel` - discards whatever's currently pending.

Fully offline, deterministic example (no PBX/network needed - the embedded
`config=` form):

```text
provisioning_resolve:centinelo://provision?config=eyJob3N0IjogInBieC5leGFtcGxlLnRlc3QiLCAiZXh0IjogIjk5OTkiLCAic2VjcmV0IjogIngiLCAidmVyc2lvbiI6IDF9|provisioning_apply
```

(That `config=` payload decodes to
`{"host": "pbx.example.test", "ext": "9999", "secret": "x", "version": 1}` -
the same placeholder host/extension convention `settings.rs`'s own test
fixtures already use in this public repo, never this workspace's real test
PBX address, which never appears under `phone/`.)

To exercise the real `https://` fetch path end to end, stand up any static
file server serving a valid config JSON over https (a throwaway self-signed
cert works fine against a real client, same as this repo's PBX box) and
point a `provisioning_resolve:https://...` step at it - not automated here
since it needs a live server, but the code path is identical to the
embedded case past `fetch_remote()`'s network call (see
`provisioning.rs`'s `read_capped_body` unit tests for the byte-cap logic
that call exercises, tested without a live server).

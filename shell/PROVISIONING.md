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
- **A call in progress blocks the apply regardless of admin state**
  (2026-07-16 4R re-review, R4): applying restarts the sidecar
  unconditionally, which would otherwise drop whatever call is in
  progress with no warning - a real risk specifically because a
  `centinelo://provision` deep link can arrive via email/IM at any
  moment, not just from a deliberate "I'm between calls" trip to
  Settings. `commands::provisioning_apply` refuses with "You're on a
  call..." (surfaced in `#prov-confirm-error`, same slot every other
  apply-time error uses) whenever `SidecarHandle::has_active_call()` is
  true - incoming/calling/in-call, not just an established call. A fresh
  install can never be mid-call (the sidecar never even starts before an
  account exists), so this check runs unconditionally rather than only on
  the re-provision path.

## Security notes

- **Account-line injection**: `sidecar.rs`'s `write_accounts_file`
  interpolates `host`/`ext`/`secret` unquoted into a single-line baresip
  accounts entry. The character/length validation that closes this
  (`;`/control chars rejected in `secret`, restrictive allowlists for
  `host`/`ext`) lives in **`settings::validate_account_fields`**, not in
  this module - it's the single source of truth, called from every writer
  of an `AccountSettings` the sidecar will eventually spawn with:
  `commands::save_account_settings` (manual Settings entry),
  `provisioning::validate` (this module), **and** defensively again
  inside `write_accounts_file` itself right before it builds the line
  (2026-07-16 4R re-review, A1 - the first version of this check lived
  only in `provisioning.rs`, leaving the manual-entry path checking
  nothing but emptiness). Covered by `settings::validate_account_fields_tests`
  (the shared checks) and `provisioning::tests::secret_with_semicolon_rejected_account_line_injection`/
  `secret_with_newline_rejected_account_line_injection`/`host_with_semicolon_rejected`/
  `ext_with_at_sign_rejected` (provisioning's own required-field wrapping
  of them).
- **https-only**, no redirect following, 16 KiB response cap (checked on
  the *encoded* length before ever decoding the `config=` form - 2026-07-16
  4R re-review, B1), 8s timeout - see "The link forms" above.
- **SSRF/DNS-rebinding hardening on the fetch** (2026-07-16 4R re-review,
  M1): the `url=`/bare-https fetch uses a custom `ureq::Resolver`
  (`provisioning::ssrf_safe_resolve`) that resolves the host once and
  drops loopback/link-local (including the 169.254.169.254 cloud-metadata
  endpoint)/unspecified/multicast addresses from the result *before*
  `ureq` ever connects - there's no second, independent resolution
  afterward for a rebinding attacker to redirect, which is what actually
  defeats the attack (a "resolve, check, then let the client re-resolve
  and connect" sequence has a gap; this doesn't). **Deliberately does
  NOT block RFC 1918 private ranges or the RFC 6598 CGNAT range
  (100.64.0.0/10)** - blocking those would break this feature's primary
  use case, not just close an edge case: a real on-prem PBX or a
  Tailscale-hosted provisioning page legitimately lives there (this
  workspace's own test PBX is a CGNAT/Tailscale address). See
  `provisioning.rs`'s `ipv4_should_be_blocked` doc for the full reasoning
  - this is a deliberately scoped subset of "block everything private",
  chosen to close the vectors with zero legitimate use (metadata
  endpoints, loopback) while preserving the product's real deployment
  model. A stricter policy (full RFC1918/ULA block, or an installer-domain
  allowlist) is a product decision, not a purely technical one - flagged
  for Mario/Felix if the threat model should be tightened further.
- **A TLS pin doesn't survive a host change** (2026-07-16 4R re-review,
  M2): `commands::resolved_tls_pin` clears `tls_pin_sha256` whenever a
  manual Settings save changes the host - a pin is a fingerprint of ONE
  host's certificate, and silently carrying PBX A's pin over to PBX B
  would fail that connection for a reason invisible in this UI (no field
  here shows/clears the pin directly).
- **No file-path fields** in the config at all - see "Not supported yet".
- The secret is validated, applied to `settings.json` (mode 600, written
  atomically via write-to-temp-then-rename - 2026-07-16 4R re-review, R2,
  see settings.rs `write_private_file`'s doc for why a crash/full-disk
  mid-write used to be able to reset every setting on next launch) and
  handed to the sidecar as a scratch `accounts` file (mode 0700 dir) -
  never logged, never returned to the frontend, never included in a
  preview.
- **A failed apply doesn't lose the pending config** (2026-07-16 4R
  re-review, R1): `provisioning_apply` peeks at the pending config and
  only clears it once `update_account` has actually succeeded - if the
  disk write fails (NAS-mounted app-data dir gone, disk full), the
  operator sees the real error and can just hit Connect again instead of
  a confusing "Nothing pending" forcing a re-paste.
- **A cold-start deep link's confirmation screen can't get lost**
  (2026-07-16 4R re-review, R3): a `centinelo://provision?config=...`
  link resolves synchronously during Rust-side startup, well before the
  frontend has loaded and attached its `provisioning://preview` listener
  - Tauri doesn't queue/replay events for late listeners, so without a
  fix that preview (and the confirmation it should have shown) would
  simply vanish. `ui/js/app.js`'s `boot()` calls the non-consuming
  `provisioning_pending_preview` command once, right after attaching
  listeners - between "listeners attached" and "checked once", no timing
  gap remains where a preview could go unseen.

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
- `admin_set_password:<password>` - sets/changes the admin password and
  leaves the session unlocked (added 2026-07-16 4R re-review, to reach
  `provisioning_apply` on an already-configured account from a script,
  without a GUI to click through the unlock screen).

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

Real requests/responses (status handling, the size-capped body read
against a real streamed response, JSON parsing) are covered by
`provisioning::tests::fetch_via_agent_*` against a loopback `tiny_http`
server, not just the pure-parsing tests above - `fetch_via_agent` is
split out from `fetch_remote` specifically so it's testable that way
without a valid TLS certificate (see that function's doc, 2026-07-16 4R
re-review B3). The `https`-only enforcement and the SSRF-safe resolver
both live in `fetch_remote`/`build_agent`, one layer up, deliberately
outside of what those loopback tests exercise.

**R4 (a call in progress blocks `provisioning_apply`) - verification
note**: verified by code inspection (the same `CallPhase`/`match` pattern
`SidecarHandle::ping_state` already uses, and the check runs before any
mutation in `provisioning_apply`) plus e2e evidence for the *negative*
case (idle - no active call - correctly allows apply, run against a
throwaway clean-install fixture). The *positive* case (dial a real call,
then confirm `provisioning_apply` is refused mid-call) was attempted
against this workspace's real test PBX during the 2026-07-16 4R
re-review fix pass but not completed with clean evidence - the dial
itself didn't settle within the attempt's wait window, and continuing to
iterate against the shared real `settings.json` (which a *successful*
`provisioning_apply` overwrites) carried more risk than the remaining
verification gap justified once the account had already been restored
once from backup. Flagged here rather than silently claimed as fully
e2e-verified; qa-e2e or a follow-up session can complete it against the
real PBX with more time budget for the dial to settle.

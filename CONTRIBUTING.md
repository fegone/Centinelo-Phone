# Contributing to Centinelo Phone

Thanks for your interest in Centinelo Phone! This is a public, MIT-licensed,
open-core project. Contributions are welcome — please read this short guide
first. It covers the two things that matter most here: **how to build**, and
**how to be honest in your changes**.

---

## Branch model

| Branch | What it is |
|---|---|
| **`main`** | The original **v1 Electron** softphone — the stable, released app. Do **not** send v2 work here. |
| **`v2`** | **Active development** of the native rewrite (`core/` C engine + `shell/` Tauri app). New v2 work targets here. |
| **`v2-docs`** | Public v2 docs (this file + the root `README.md`). |

If your change is for the v1 Electron app, branch from `main`. For anything in
`core/` or `shell/`, branch from `v2`.

---

## Build prerequisites (recap)

Centinelo Phone v2 is a monorepo with two buildable components. The full,
from-clean-clone steps are in **[`core/BUILD.md`](core/BUILD.md)** and
**[`shell/README.md`](shell/README.md)** — read those for the exact commands
and the rationale. The recap:

- **Toolchain**
  - C build: `cmake`, an OpenSSL 3.x install (Homebrew keg-only — pass
    `-DOPENSSL_ROOT_DIR="$(brew --prefix openssl@3)"` if discovery fails).
  - Shell build: a recent Rust toolchain, Node.js/npm, and the Tauri v2
    prerequisites for your platform.
- **Pinned submodules.** `core/deps/re` and `core/deps/baresip` are pinned to
  matching `v4.9.0` tags. Always init them recursively
  (`git submodule update --init --recursive`) and **never** commit changes
  inside the submodule working trees — local fixes live as reviewable patch
  files under `core/patches/` and are applied on top of a clean checkout.
- **Engine → Shell order.** Build the C engine first
  (`core/deps/re`, then `core/deps/baresip`), then `cd shell && npm install &&
  npm run dev`.
- **Unit tests.** The engine's pure JSON-command decoding and BLF dialog-info
  parsing have standalone CMake/`ctest` unit tests under
  `core/modules/ctrl_json/test/` — run them before any `core/` change.

---

## Commit conventions

Use [Conventional Commits](https://www.conventionalcommits.org/) prefixes,
scoped to the component you touched:

| Prefix | Use for |
|---|---|
| `feat(core):` | New capability in the C SIP engine / `ctrl_json` module. |
| `feat(shell):` | New capability in the Tauri shell. |
| `fix(core):` / `fix(shell):` | Bug fixes, scoped accordingly. |
| `ci:` | GitHub Actions workflow changes (e.g. `.github/workflows/*.yml`). |
| `docs:` | Documentation only — `README.md`, `CONTRIBUTING.md`, `core/*.md`, `shell/*.md`. |
| `test(core):` / `test(shell):` | Test-only changes. |
| `refactor(core):` / `refactor(shell):` | Code restructuring with no behavior change. |
| `chore:` | Submodule bumps, dependency updates, housekeeping. |

Examples:

```
feat(core): add mid-dialog call-replaced event for attended transfer target
fix(shell): clear backoff budget on intentional respawn
ci: pass OPENSSL_ROOT_DIR on clean windows runner
docs: v2 README + CONTRIBUTING
```

Keep the subject line ≤ ~72 chars, imperative mood ("add", not "added"). Put the
*why* in the body when it isn't obvious.

---

## Code style

### `core/` (C)

- **C99**, matching the baresip/libre upstream conventions you're building
  against: lowercase `snake_case`, module-prefixed symbol names, an explicit
  `init`/`close` pair for anything that owns state.
- No C++ features, no GNU extensions that break MSVC/clang-on-Windows. The
  `_WIN32` code path is real — keep it compiling (`clang -fsyntax-only -D_WIN32`
  is a fast local check; see `core/BUILD.md` "Windows CI").
- Pure, testable logic (JSON decoding, XML parsing, anything that doesn't need
  a live SIP stack) goes in its own file and gets a unit test under
  `core/modules/ctrl_json/test/`. Don't mix parsing with baresip API calls.
- Never leave an unrefcounted pointer crossing a thread boundary — see the
  `stdin_thread_main` design note in `core/modules/ctrl_json/ctrl_json.c`.

### `shell/` (Rust)

- **`cargo fmt`** before every commit, and **`cargo clippy -- -D warnings`**
  must pass clean. No `unwrap()`/`expect()` in production paths unless the panic
  is genuinely unrecoverable and the message explains why.
- Static HTML/CSS/JS frontend — **no bundler, no frontend framework.** Don't
  introduce one without a separate discussion; the zero-build frontend is
  intentional.

---

## Testing — the honesty rule

This project has a strong "don't claim what you didn't verify" culture. Two
rules apply to *every* change:

1. **Engine (`core/`) changes need protocol-level evidence.** A unit test for
   pure logic is good and required where applicable, but a claim that a SIP
   feature *works* means it was driven end-to-end over the `ctrl_json` wire
   protocol (`core/PROTOCOL.md`) against a real SIP PBX, and the resulting
   event trail (e.g. `ready` → `reg_state` → `call_state` ...) was captured.
   If you can't produce that evidence, mark the feature **🚧 in progress**,
   not ✅, and say what's left.

2. **UI (`shell/`) claims need the scripted e2e driver.** The shell ships a
   debug-only scripted driver (`shell/src-tauri/src/e2e.rs`, driven by the
   `CENTINELO_E2E_SCRIPT` env var) precisely so UI claims can be reproduced
   without fragile OS-level click automation. A claim that a UI flow works
   means that driver ran it and the captured event trail is in `shell/E2E.md`.
   Screenshots are nice; the driver trace is the evidence.

**Do not** add a button/feature to the UI that isn't backed by a working
engine command, and do not flip a 🚧/🔜 status to ✅ without the evidence
above. Shipping dead controls contradicts the project's brand voice — see the
"don't fabricate numbers" notes in `shell/README.md` for the precedent.

---

## This is a PUBLIC repo — what never goes in

Centinelo Phone is open source. **Never** commit, paste into an issue/PR, or
leave in a code comment:

- IP addresses, hostnames, or Tailscale/mesh-network identifiers of any real
  deployment.
- PBX product/version details, server config, or admin credentials of a real
  system.
- Test extension numbers, SIP secrets/passwords, or sample account configs that
  correspond to a real installation.
- Any clinic, customer, or business-specific reference. Keep examples generic
  (`sip:ext@example`, `<your PBX host>`).
- Pricing tiers, sales strategy, or private business detail beyond the generic
  Community (free) / Pro (paid) split already described in the `README.md`.

If a test fixture or doc example needs a value, use an obviously-placeholder
one.

---

## Pull requests

- Branch from the right base (`main` for v1, `v2` for v2 engine/shell work,
  `v2-docs` for docs).
- One logical change per PR. Mix `feat` + `ci` + `refactor` and it's hard to
  review.
- In the PR description, state **what was verified and how** (the protocol trace
  for `core/`, the e2e driver run for `shell/`). If something is intentionally
  *not* verified yet, say so explicitly rather than implying it works.
- Keep diffs focused — don't reformat whole files incidentally. `cargo fmt` and
  the baresip style apply to the lines you actually changed.

Thanks for helping make Centinelo Phone better.

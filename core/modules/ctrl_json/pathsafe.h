/**
 * @file pathsafe.h  Centinelo Phone v2 - filesystem-path-component sanitizer
 *
 * v1.3 security fix (4R risk-lens finding, 2026-07-16): `call_id(call)`
 * (baresip's `struct call::id`) is **not** an engine-generated value for an
 * *incoming* call - `src/call.c`'s `sipsess_accept_handler()` sets it
 * verbatim from `sip_dialog_callid()`, i.e. the SIP `Call-ID` header the
 * *far end* sent in its own INVITE. RFC 3261's `word` token grammar
 * (`callid = word ["@" word]`) legally permits `/` (and `\`, quotes, and
 * most other punctuation) inside a Call-ID - a real SIP peer, not just a
 * hypothetically malicious one, could send one containing `../` sequences
 * and still be spec-compliant.
 *
 * `audiotap.c`'s `audiotap_start()` interpolates this same call_id
 * directly into a filesystem path (`<dir>/<call_id>-rx.wav`/`-tx.wav`,
 * see PROTOCOL.md "tap_start") - unsanitized, that's a real path-traversal
 * vector: a crafted Call-ID could write a WAV file outside the caller-
 * supplied `dir`. This module is the fix: every remote-controlled
 * identifier this engine ever interpolates into a filesystem path MUST be
 * passed through pathsafe_component() first (currently just the one call
 * site - see audiotap.c `audiotap_start()`).
 *
 * Deliberately pure (no baresip.h / re.h dependency beyond <stddef.h>) so
 * it's unit tested standalone, same pattern as cmd.c/dialog_info.c - see
 * core/modules/ctrl_json/test/test_main.c.
 *
 * Copyright (C) 2026 Neola Dental / Centinelo Phone
 */

#ifndef CENTINELO_CTRL_JSON_PATHSAFE_H
#define CENTINELO_CTRL_JSON_PATHSAFE_H

#include <stddef.h>

/**
 * Sanitizes `in` for safe use as a single filesystem path *component*
 * (never a full path - the caller still owns joining it with a directory
 * and a suffix, see audiotap.c `path_build()`).
 *
 * Whitelist-only (fails safe, not blacklist): only
 * `[A-Za-z0-9._@-]` bytes are copied through; every other byte -
 * including `/` and `\` (the two path-separator characters this actually
 * needs to stop, on POSIX and Windows respectively - see
 * core/PROTOCOL.md "Framing / stdin" for this engine's own POSIX+Windows
 * dual-platform scope), any control byte, and any non-ASCII byte - is
 * replaced with `_`. A leading run of `.` characters is additionally
 * neutralized (each leading `.` also replaced with `_`) so the output can
 * never itself *be* exactly `.`/`..`/`...` etc, even before a caller
 * appends its own suffix - defense in depth on top of `audiotap.c`'s own
 * "always appends a non-dot `-rx`/`-tx` suffix" behavior, which already
 * independently rules out a bare `..` reaching the filesystem from this
 * engine's one real call site.
 *
 * @param in       Input string (any bytes, NUL-terminated). NULL is
 *                 treated the same as "".
 * @param out      Output buffer. Always left NUL-terminated on return,
 *                 even for a NULL/empty `in` or `out_size` of 1 (which
 *                 yields an empty string - there's no room for anything
 *                 else).
 * @param out_size Size of `out` in bytes, including the NUL terminator.
 *                 A `NULL`/zero `out`/`out_size` is a no-op (nothing
 *                 written anywhere).
 */
void pathsafe_component(const char *in, char *out, size_t out_size);

#endif /* CENTINELO_CTRL_JSON_PATHSAFE_H */

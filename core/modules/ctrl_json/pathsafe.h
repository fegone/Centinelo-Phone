/**
 * @file pathsafe.h  Centinelo Phone v2 - filesystem-path-component sanitizer
 *
 * v1.3 security fix - see core/PROTOCOL.md "Changes from v1.2" for the
 * full story (call_id(call) is caller-controlled for an incoming call,
 * not engine-generated, and was reaching a filesystem path unsanitized
 * in audiotap.c's tap_start).
 *
 * Deliberately pure (no baresip.h / re.h dependency beyond <stddef.h>) so
 * it's unit tested standalone, same pattern as cmd.c/dialog_info.c - see
 * core/modules/ctrl_json/test/test_main.c.
 *
 * Copyright (C) 2026 Centinelo Phone
 */

#ifndef CENTINELO_CTRL_JSON_PATHSAFE_H
#define CENTINELO_CTRL_JSON_PATHSAFE_H

#include <stdbool.h>
#include <stddef.h>

/**
 * Sanitizes `in` for safe use as a single filesystem path *component*
 * (never a full path - the caller still owns joining it with a directory
 * and a suffix, see audiotap.c `path_build()`).
 *
 * Whitelist-only (fails safe, not blacklist): only `[A-Za-z0-9._@-]`
 * bytes are copied through; every other byte - including `/` and `\`,
 * any control byte, any non-ASCII byte - is replaced with `_`. A leading
 * run of `.` characters is additionally neutralized so the output can
 * never itself *be* `.`/`..`/etc.
 *
 * Many-to-one by construction (a whitelist can't also guarantee
 * uniqueness) - two different inputs can sanitize to the same output.
 * A caller for whom that collision matters (two concurrent taps writing
 * to the same path, say) should use pathsafe_unique_component() below
 * instead of calling this directly.
 *
 * @param in       Input string (any bytes, NUL-terminated). NULL is
 *                 treated the same as "".
 * @param out      Output buffer. Always left NUL-terminated on return,
 *                 even for a NULL/empty `in` or `out_size` of 1.
 * @param out_size Size of `out` in bytes, including the NUL terminator.
 *                 A `NULL`/zero `out`/`out_size` is a no-op.
 */
void pathsafe_component(const char *in, char *out, size_t out_size);

/**
 * pathsafe_component(), plus collision avoidance: if the plain sanitized
 * value is already "taken" (per the caller-supplied `is_taken`
 * predicate), retries with a `-2`, `-3`, ... suffix appended until a free
 * candidate is found or `max_attempts` is exhausted. See pathsafe.c's own
 * comment for why this exists (the many-to-one collision above is a real
 * silent-data-corruption risk for a caller like audiotap.c that opens
 * files at the sanitized path).
 *
 * Pure with respect to filesystem/registry state: `is_taken` is the
 * caller's own predicate (audiotap.c's is impure - checks a live
 * registry and the filesystem) - this function itself stays unit
 * testable with a fake one (see test/test_main.c
 * test_pathsafe_unique_component()).
 *
 * @param in           Raw input (see pathsafe_component()).
 * @param out          Output buffer.
 * @param out_size     Size of `out`, including the NUL terminator.
 * @param is_taken     Returns true if `candidate` (always exactly what's
 *                     currently in `out`) is already in use. NULL means
 *                     "nothing is ever taken" (behaves like
 *                     pathsafe_component()).
 * @param arg          Passed through to `is_taken` unchanged.
 * @param max_attempts Upper bound on suffixed retries *after* the plain
 *                     (attempt 0) candidate - a caller-chosen safety net,
 *                     not a value this function picks. If exhausted, `out`
 *                     is left at its last attempted value.
 *
 * @return true if `out` holds a candidate `is_taken` reported as free (or
 *         `is_taken` was NULL), false if `max_attempts` was exhausted.
 */
bool pathsafe_unique_component(const char *in, char *out, size_t out_size,
				bool (*is_taken)(const char *candidate,
						  void *arg),
				void *arg, unsigned max_attempts);

#endif /* CENTINELO_CTRL_JSON_PATHSAFE_H */

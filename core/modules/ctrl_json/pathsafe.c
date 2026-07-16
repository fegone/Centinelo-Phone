/**
 * @file pathsafe.c  Centinelo Phone v2 - filesystem-path-component sanitizer
 *
 * See pathsafe.h. Pure C99, no re/baresip dependency - so it links into
 * the standalone unit test binary (core/modules/ctrl_json/test/) with no
 * baresip build required, same as cmd.c/dialog_info.c.
 *
 * Copyright (C) 2026 Centinelo Phone
 */

#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include "pathsafe.h"


static bool is_safe_char(char c)
{
	return (c >= 'A' && c <= 'Z') ||
	       (c >= 'a' && c <= 'z') ||
	       (c >= '0' && c <= '9') ||
	       c == '.' || c == '_' || c == '@' || c == '-';
}


void pathsafe_component(const char *in, char *out, size_t out_size)
{
	size_t i, n;
	bool leading_dots = true;

	if (!out || !out_size)
		return;

	if (!in)
		in = "";

	n = out_size - 1;   /* leave room for the NUL terminator */

	for (i = 0; in[i] != '\0' && i < n; i++) {
		char c = in[i];

		if (c == '.' && leading_dots) {
			out[i] = '_';
			continue;
		}
		leading_dots = false;

		out[i] = is_safe_char(c) ? c : '_';
	}

	out[i] = '\0';
}


static void copy_truncated(char *dst, size_t dst_size, const char *src)
{
	size_t len;

	if (!dst || !dst_size)
		return;

	len = strlen(src);
	if (len > dst_size - 1)
		len = dst_size - 1;

	memcpy(dst, src, len);
	dst[len] = '\0';
}


/*
 * pathsafe_component() is many-to-one by construction - a fixed
 * whitelist can neutralize path traversal, but can't also guarantee two
 * different raw inputs produce two different outputs (e.g. "abc/def" and
 * "abc_def" both sanitize to "abc_def"). For a caller that then opens a
 * file at the sanitized path - audiotap.c's tap_start, whose call_id is
 * the far end's own SIP Call-ID header, not engine-generated - a
 * collision is a real, silent correctness+security problem: two
 * concurrent taps racing to fopen("wb") the same path both "succeed",
 * whichever closes last wins the header, the other's capture is
 * corrupted with no error either side ever sees, and the same peer that
 * can craft a Call-ID for path traversal can just as easily craft one
 * that collides with an already-active tap's sanitized name on purpose.
 * This function is the fix: try the plain sanitized value first (keeps
 * the common, no-collision case's filename exactly what it's always
 * been), then retry with a "-2", "-3", ... suffix until the caller's own
 * `is_taken` predicate reports a free one.
 *
 * `base` is capped to `out_size` minus room for the *largest* suffix this
 * call could ever produce ("-" + `max_attempts + 1`'s digit count),
 * computed up front - not truncated to the full `out_size - 1` first and
 * only then suffixed. Truncating to the full width first (this
 * function's own earlier bug - a 4R finding, see PROTOCOL.md/E2E-F1.md
 * "F5") left zero room for a long, peer-controlled Call-ID's suffix to
 * ever change the truncated result: every attempt produced the
 * byte-identical candidate, retries exhausted, and audiotap_start()
 * wrongly denied recording for a call that never genuinely needed a
 * fallback - a caller-visible availability regression, not a cosmetic
 * truncation quirk.
 */
bool pathsafe_unique_component(const char *in, char *out, size_t out_size,
				bool (*is_taken)(const char *candidate,
						  void *arg),
				void *arg, unsigned max_attempts)
{
	char base[256];
	char suffix_probe[16];
	size_t suffix_margin, base_cap;
	unsigned attempt;

	if (!out || !out_size)
		return false;

	(void)snprintf(suffix_probe, sizeof(suffix_probe), "-%u",
		       max_attempts + 1);
	suffix_margin = strlen(suffix_probe);

	base_cap = (out_size > suffix_margin) ? out_size - suffix_margin : 1;
	if (base_cap > sizeof(base))
		base_cap = sizeof(base);

	pathsafe_component(in, base, base_cap);
	copy_truncated(out, out_size, base);

	if (!is_taken || !is_taken(out, arg))
		return true;

	for (attempt = 0; attempt < max_attempts; attempt++) {
		char withsuf[sizeof(base) + 16];

		(void)snprintf(withsuf, sizeof(withsuf), "%s-%u", base,
			       attempt + 2);
		copy_truncated(out, out_size, withsuf);

		if (!is_taken(out, arg))
			return true;
	}

	return false;
}

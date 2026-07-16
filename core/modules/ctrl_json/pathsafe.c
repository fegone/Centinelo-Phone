/**
 * @file pathsafe.c  Centinelo Phone v2 - filesystem-path-component sanitizer
 *
 * See pathsafe.h. Pure C99, no re/baresip dependency - so it links into
 * the standalone unit test binary (core/modules/ctrl_json/test/) with no
 * baresip build required, same as cmd.c/dialog_info.c.
 *
 * Copyright (C) 2026 Neola Dental / Centinelo Phone
 */

#include <stdbool.h>
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

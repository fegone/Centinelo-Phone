/**
 * @file dialog_info.c  Centinelo Phone v2 - tiny dialog-info+xml parser
 *
 * See dialog_info.h. Uses re's own small regex helper (re_regex(), from
 * re_fmt.h) rather than a real XML parser or a hand-rolled scanner -
 * re_regex is already a proven dependency in this exact codebase (see
 * core/deps/baresip/modules/presence/subscriber.c, which parses
 * PIDF+XML presence bodies the same way for the sibling Event: presence
 * package) and is enough for the handful of fixed shapes a dialog-info
 * NOTIFY body actually takes - a real XML parser would be considerably
 * more code for no behavioural difference here.
 *
 * Copyright (C) 2026 Neola Dental / Centinelo Phone
 */

#include <re.h>
#include "dialog_info.h"


const char *cent_blf_state_name(enum cent_blf_state state)
{
	switch (state) {

	case CENT_BLF_IDLE:    return "idle";
	case CENT_BLF_RINGING: return "ringing";
	case CENT_BLF_BUSY:    return "busy";
	case CENT_BLF_OFFLINE: return "offline";
	default:               return "offline";
	}
}


enum cent_blf_state cent_blf_state_for_close(void)
{
	return CENT_BLF_OFFLINE;
}


enum cent_blf_state dialog_info_parse(const char *body, size_t len)
{
	struct pl state;

	if (!body || !len)
		return cent_blf_state_for_close();

	/*
	 * First confirm this is even a dialog-info document at all (root
	 * "<dialog-info" element present) - garbage/unrelated bodies
	 * should fall into "can't tell" (offline), not be silently
	 * misread as idle just because they happen not to contain the
	 * substring "<dialog".
	 */
	if (re_regex(body, len, "<dialog-info"))
		return cent_blf_state_for_close();

	/*
	 * Does a "<dialog" *element* (as opposed to the "<dialog-info
	 * ...>" root element just confirmed above) appear at all? The
	 * required single delimiter character after "dialog" - one of
	 * space/tab/'>' - is what excludes matching the '-' of
	 * "<dialog-info": re_regex's "[ \t>]1" means "exactly one char
	 * from this class", so "<dialog-info" (next char '-') does not
	 * match, but "<dialog id=..." / "<dialog>" / "<dialog\t..." do.
	 *
	 * RFC 4235 allows state="full" with zero <dialog> children -
	 * that's the normal "no active calls for this resource" shape,
	 * i.e. idle, and it is the common case this checks for first.
	 */
	if (re_regex(body, len, "<dialog[ \t>]1", NULL))
		return CENT_BLF_IDLE;

	/* Found a <dialog> element - it must carry a <state> to mean
	 * anything to us. Tolerate optional attributes on the tag itself
	 * ("[^>]*" before the closing '>') and optional whitespace before
	 * the value, since nothing in RFC 4235 rules either out; capture
	 * only the value itself (third group). */
	if (re_regex(body, len, "<state[^>]*>[ \t\r\n]*[a-zA-Z]+",
		     NULL, NULL, &state)) {
		return cent_blf_state_for_close();
	}

	if (!pl_strcasecmp(&state, "confirmed"))
		return CENT_BLF_BUSY;

	if (!pl_strcasecmp(&state, "early") ||
	    !pl_strcasecmp(&state, "proceeding") ||
	    !pl_strcasecmp(&state, "trying"))
		return CENT_BLF_RINGING;

	if (!pl_strcasecmp(&state, "terminated"))
		return CENT_BLF_IDLE;   /* a dialog that just ended */

	/* Unrecognised <state> value - fail into the same "can't
	 * currently tell" bucket as a missing/unparseable one, rather
	 * than guessing. */
	return cent_blf_state_for_close();
}

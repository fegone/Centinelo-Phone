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
	case CENT_BLF_HELD:    return "held";
	case CENT_BLF_DND:     return "dnd";
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
	 * v1.3 presence_override - DND (see dialog_info.h's header comment
	 * for the full rationale/verification status - non-standard,
	 * best-effort, no real Asterisk capture has ever produced either
	 * pattern against this engine's test PBX). Checked before the "no
	 * <dialog> element -> idle" fallback right below, since an
	 * idle-but-DND'd extension has zero <dialog> elements either way -
	 * without this check first, it would silently fall into idle.
	 */
	if (!re_regex(body, len, "<dnd>true</dnd>", NULL) ||
	    !re_regex(body, len, "dnd=\"true\"", NULL))
		return CENT_BLF_DND;

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

	if (!pl_strcasecmp(&state, "confirmed")) {
		/*
		 * v1.3 presence_override - HELD: the RFC 4235/RFC 3840
		 * standard hold indication, a <target> "+sip.rendering"
		 * param with pvalue="no" (see dialog_info.h's header
		 * comment). Deliberately not scoped to the single
		 * <dialog>...</dialog> block just matched above - this was
		 * never a real XML parser (see this file's top-of-file
		 * comment) and every real capture so far carries exactly one
		 * <dialog> per NOTIFY anyway (see core/E2E-F1.md), so a
		 * whole-body search is equivalent in practice and keeps this
		 * consistent with how the <state> extraction above already
		 * works (first match anywhere in the body).
		 *
		 * Two independent substring checks, not one combined
		 * pattern: re_regex (see this file's top-of-file comment on
		 * it being a small hand-rolled matcher, not a real regex
		 * engine) has no backtracking, so a single pattern like
		 * "+sip.rendering\"[^>]*pvalue=\"no\"" fails in practice -
		 * "[^>]*" greedily consumes right through "pvalue=\"no\""
		 * itself (nothing stops it before the *next* real '>', which
		 * is well past it), leaving nothing left for the literal
		 * "pvalue=\"no\"" that follows in the pattern to match
		 * against - caught by this file's own unit tests (see
		 * test/test_main.c test_dialog_info_held()) before this ever
		 * reached e2e. "+sip.rendering" and "pvalue=\"no\"" both
		 * appearing anywhere in the body is a strong enough signal on
		 * its own for the simple, single-dialog bodies this parser
		 * actually sees (see core/E2E-F1.md) - no real dialog-info
		 * body has any other reason to contain literal
		 * "pvalue=\"no\"" text.
		 */
		if (!re_regex(body, len, "+sip.rendering", NULL) &&
		    !re_regex(body, len, "pvalue=\"no\"", NULL))
			return CENT_BLF_HELD;

		return CENT_BLF_BUSY;
	}

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

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
 * Copyright (C) 2026 Centinelo Phone
 */

#include <ctype.h>
#include <string.h>
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


/*
 * Case-insensitive byte-for-byte compare of exactly `n` bytes - re_regex()
 * (used everywhere else in this file) already matches tag names
 * case-insensitively (see test_dialog_info_busy()'s "loose" fixture:
 * "<DIALOG-INFO><DIALOG...<STATE>...</STATE></DIALOG></DIALOG-INFO>" is
 * expected to parse the same as its lowercase equivalent), so the new
 * byte-scanning helpers below - is_dialog_open()/find_str() - need to
 * preserve that, not silently become case-sensitive. No strncasecmp()
 * here: it's POSIX, not C99/MSVC (Windows has `_strnicmp` instead, a
 * different name) - see the "no memmem()" reasoning on find_str() below
 * for why this file avoids libc functions the Windows build doesn't
 * already carry.
 */
static bool ci_eq(const char *a, const char *b, size_t n)
{
	size_t i;

	for (i = 0; i < n; i++) {
		if (tolower((unsigned char)a[i]) != tolower((unsigned char)b[i]))
			return false;
	}

	return true;
}


/*
 * v1.7 fix: true iff `p` (which must have at least one byte remaining -
 * callers always check `p < end` first) points at the start of a real
 * "<dialog" *element*, as opposed to the "<dialog-info" root element -
 * i.e. the character right after the literal "<dialog" is one of the
 * delimiters RFC 4235's grammar actually allows there (space/tab/'>').
 * Same distinction the old single whole-body regex ("<dialog[ \t>]1")
 * relied on, just as a byte scan now that this file walks each <dialog>
 * element individually instead of matching once across the whole
 * document - see dialog_info_parse()'s own comment for why.
 */
static bool is_dialog_open(const char *p, const char *end)
{
	static const char tag[] = "<dialog";
	size_t taglen = sizeof(tag) - 1;

	if ((size_t)(end - p) <= taglen)
		return false;

	if (!ci_eq(p, tag, taglen))
		return false;

	switch (p[taglen]) {

	case ' ':
	case '\t':
	case '>':
		return true;

	default:
		return false;
	}
}


/*
 * Length-aware, case-insensitive literal substring search - no memmem()
 * here, it's not part of C99/MSVC's runtime and this file otherwise has
 * no libc dependency this project's Windows build doesn't already carry
 * (see core/BUILD.md). `hay` is not assumed NUL-terminated (it's a raw
 * NOTIFY body slice, see dialog_info_parse()'s caller in ctrl_json.c).
 * Returns NULL if `needle` doesn't occur.
 */
static const char *find_str(const char *hay, size_t haylen,
			     const char *needle)
{
	size_t nlen = str_len(needle);
	size_t i;

	if (!nlen || nlen > haylen)
		return NULL;

	for (i = 0; i + nlen <= haylen; i++) {
		if (ci_eq(hay + i, needle, nlen))
			return hay + i;
	}

	return NULL;
}


/*
 * Resolves a single <dialog>...</dialog> element's own state, exactly
 * the same rules dialog_info_parse() used to apply to the *whole* body -
 * see that function's own comment for why this is now scoped to just
 * `dialog` (one element's byte range) rather than the full document.
 */
static enum cent_blf_state dialog_state_of(const struct pl *dialog)
{
	struct pl state;

	/* Found a <dialog> element - it must carry a <state> to mean
	 * anything to us. Tolerate optional attributes on the tag itself
	 * ("[^>]*" before the closing '>') and optional whitespace before
	 * the value, since nothing in RFC 4235 rules either out; capture
	 * only the value itself (third group). */
	if (re_regex(dialog->p, dialog->l,
		     "<state[^>]*>[ \t\r\n]*[a-zA-Z]+",
		     NULL, NULL, &state)) {
		return cent_blf_state_for_close();
	}

	if (!pl_strcasecmp(&state, "confirmed")) {
		/*
		 * v1.3 presence_override - HELD (RFC 4235/3840
		 * "+sip.rendering" pvalue="no" - see dialog_info.h). Two
		 * independent substring checks, not one combined pattern -
		 * re_regex has no backtracking, so "+sip.rendering\"[^>]*
		 * pvalue=\"no\"" fails (the [^>]* greedily eats past
		 * pvalue="no" itself) - see PROTOCOL.md "Changes from
		 * v1.2" and core/E2E-F1.md "F5" for the full story,
		 * including why two whole-*dialog* (not whole-*body*,
		 * v1.7 - see dialog_info_parse()'s comment) checks are
		 * precise enough here.
		 */
		if (!re_regex(dialog->p, dialog->l, "+sip.rendering", NULL) &&
		    !re_regex(dialog->p, dialog->l, "pvalue=\"no\"", NULL))
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


enum cent_blf_state dialog_info_parse(const char *body, size_t len)
{
	const char *p, *end;
	bool have_dialog = false;
	bool any_ringing = false, any_busy = false, any_held = false;
	bool any_terminated = false;

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
	 * v1.7 fix: walk each <dialog>...</dialog> element in the
	 * document independently, instead of the pre-v1.7 approach of
	 * running dialog_state_of()'s regexes (in particular the HELD
	 * markers) once across the *entire* body. RFC 4235 allows more
	 * than one <dialog> child per dialog-info document - a monitored
	 * extension mid-attended-transfer (one call held, a second one
	 * ringing/talking) is the realistic shape - and a whole-body
	 * search had no way to tell which <dialog> a HELD marker actually
	 * belonged to: with two <dialog> elements, a held marker
	 * belonging to the *second* one could get attributed to the
	 * *first* one's (unheld) <state>, since re_regex only ever
	 * reported the first "<state...>" match in the whole document
	 * while checking hold markers everywhere. Scoping every check to
	 * one element's own byte range (`block` below) closes that.
	 *
	 * Once every dialog's own state is known, they're combined by
	 * priority - a receptionist's console cares more about "is this
	 * extension free to receive a transfer *right now*" than about
	 * exhaustively enumerating every dialog: any ringing dialog wins
	 * outright (an incoming call must never be hidden behind an
	 * existing busy/held line on the same extension), then any plain
	 * busy dialog, then held, then idle/terminated - matching the
	 * same visual urgency order the BLF console already renders
	 * (ringing > busy/held > idle).
	 */
	for (p = body, end = body + len; p < end; ) {

		const char *close;
		struct pl block;
		enum cent_blf_state st;

		if (!is_dialog_open(p, end)) {
			p++;
			continue;
		}

		have_dialog = true;

		close = find_str(p, (size_t)(end - p), "</dialog>");
		block.p = p;
		block.l = close ? (size_t)(close - p) : (size_t)(end - p);

		st = dialog_state_of(&block);

		switch (st) {

		case CENT_BLF_RINGING: any_ringing    = true; break;
		case CENT_BLF_BUSY:    any_busy       = true; break;
		case CENT_BLF_HELD:    any_held       = true; break;
		case CENT_BLF_IDLE:    any_terminated = true; break;
		default: break;   /* unrecognised/no <state> - ignored,
				   * same as before v1.7 */
		}

		/* Resume scanning right after this element's own close
		 * tag (or stop - no closing tag found, e.g. a malformed
		 * or self-closing "<dialog .../>" body with no <state> at
		 * all, matching the pre-v1.7 "no <state> found -> offline"
		 * outcome for that shape, see test_dialog_info_
		 * terminated_and_unknown()'s "no_state" fixture). */
		p = close ? close + str_len("</dialog>") : end;
	}

	if (!have_dialog) {
		/*
		 * RFC 4235 allows state="full" with zero <dialog>
		 * children - that's the normal "no active calls for this
		 * resource" shape, i.e. idle, and it is the common case
		 * this checks for first.
		 *
		 * v1.3 presence_override - DND (see dialog_info.h's
		 * header comment - non-standard, best-effort, no real
		 * Asterisk capture has ever produced either pattern).
		 * Deliberately scoped to *only* this "no <dialog> element
		 * at all" branch, not the whole function: an extension
		 * with a genuinely active dialog (confirmed/early/etc,
		 * handled above) is never overridden by a stray DND
		 * marker elsewhere in the body - only the "would otherwise
		 * be idle" case can become "dnd".
		 */
		if (!re_regex(body, len, "<dnd>true</dnd>", NULL) ||
		    !re_regex(body, len, "dnd=\"true\"", NULL))
			return CENT_BLF_DND;

		return CENT_BLF_IDLE;
	}

	if (any_ringing)
		return CENT_BLF_RINGING;

	if (any_busy)
		return CENT_BLF_BUSY;

	if (any_held)
		return CENT_BLF_HELD;

	if (any_terminated)
		return CENT_BLF_IDLE;

	/* At least one <dialog> element was present, but none of them had
	 * a <state> this parser recognises (or any <state> at all). */
	return cent_blf_state_for_close();
}

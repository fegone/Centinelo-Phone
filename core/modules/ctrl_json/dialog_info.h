/**
 * @file dialog_info.h  Centinelo Phone v2 - tiny dialog-info+xml parser
 *
 * BLF (busy-lamp-field) presence is delivered as a SIP SUBSCRIBE/NOTIFY
 * Event: dialog subscription (RFC 4235), body content-type
 * application/dialog-info+xml. baresip has no dialog-info parser (its
 * bundled `presence` module only speaks Event: presence / PIDF+XML, see
 * core/deps/baresip/modules/presence/subscriber.c) so this is a small,
 * hand-rolled, tolerant parser: it looks for a "<dialog" element and,
 * inside it, a "<state>...</state>" value - it does not attempt to be a
 * general XML parser (no namespaces, no entity decoding, no nesting
 * beyond what real dialog-info bodies use).
 *
 * Deliberately has zero baresip.h / SIP-stack dependency - only re's
 * string helpers - so it links into a small standalone test binary (see
 * core/modules/ctrl_json/test/) and is unit tested without a running
 * engine or a live SUBSCRIBE dialog.
 *
 * See core/PROTOCOL.md ("blf_subscribe") for the wire event this feeds,
 * and core/E2E-F1.md for a real NOTIFY body captured against the test PBX
 * (Asterisk chan_pjsip hint dialog-info).
 *
 * Copyright (C) 2026 Neola Dental / Centinelo Phone
 */

#ifndef CENTINELO_CTRL_JSON_DIALOG_INFO_H
#define CENTINELO_CTRL_JSON_DIALOG_INFO_H

#include <stddef.h>

/**
 * BLF line state, matching PROTOCOL.md's
 * {"event":"blf",...,"state":"idle|ringing|busy|held|dnd|offline"}.
 *
 * CENT_BLF_HELD/CENT_BLF_DND are v1.3 ("presence_override" - see
 * PROTOCOL.md "blf" and dialog_info_parse()'s own header comment below for
 * exactly what triggers each).
 */
enum cent_blf_state {
	CENT_BLF_IDLE = 0,
	CENT_BLF_RINGING,
	CENT_BLF_BUSY,
	CENT_BLF_HELD,     /**< v1.3 - a confirmed dialog whose NOTIFY body
			     *   also carries the RFC 4235/3840 standard hold
			     *   indication (a <target> "+sip.rendering" param,
			     *   pvalue="no") - see dialog_info_parse(). */
	CENT_BLF_DND,      /**< v1.3 - best-effort: see dialog_info_parse()'s
			     *   header comment for exactly what triggers this
			     *   and its real-PBX verification status (not
			     *   RFC 4235 - dialog-info is fundamentally a
			     *   *dialog* package, DND is a device-config
			     *   state, not a dialog). */
	CENT_BLF_OFFLINE,
};

const char *cent_blf_state_name(enum cent_blf_state state);

/**
 * Parse a NOTIFY body for the "Event: dialog" package
 * (application/dialog-info+xml, RFC 4235) into a BLF line state.
 *
 * Rules (see core/E2E-F1.md for the real captured body this was tuned
 * against):
 *   - NULL/empty body, or no "<dialog-info" root element at all (not a
 *     dialog-info document)                            -> CENT_BLF_OFFLINE
 *   - (v1.3, checked first, see below) a "<dnd>true</dnd>" element or
 *     "dnd=" attribute anywhere in the body              -> CENT_BLF_DND
 *   - a "<dialog-info" document with no "<dialog" element (state="full",
 *     zero dialogs - the idle/no-active-call case)      -> CENT_BLF_IDLE
 *   - a "<dialog" element whose "<state>" is "trying"/"proceeding"/
 *     "early"                                           -> CENT_BLF_RINGING
 *   - "<state>confirmed</state>", target "+sip.rendering"
 *     pvalue="no" ALSO present (v1.3, see below)         -> CENT_BLF_HELD
 *   - "<state>confirmed</state>", no rendering=no found  -> CENT_BLF_BUSY
 *   - "<state>terminated</state>" (a dialog that just ended - back to
 *     no active dialogs)                                -> CENT_BLF_IDLE
 *   - a "<dialog" element with no parseable "<state>"    -> CENT_BLF_OFFLINE
 *     (best-effort "can't tell" bucket - see also
 *     cent_blf_state_for_close(), the sibling case where the
 *     subscription itself failed/was rejected before any NOTIFY body
 *     existed to parse at all).
 *
 * v1.3 "presence_override" (see PROTOCOL.md "blf"/"presence_override" and
 * core/E2E-F1.md "F5 presence_override" for the real-PBX verification
 * status of each):
 *
 *   - CENT_BLF_HELD is the RFC 4235/RFC 3840 *standard* hold indication:
 *     a dialog's <local>/<remote> <target> element carrying a
 *     <param pname="+sip.rendering" pvalue="no"/> means that side isn't
 *     rendering the other's media, i.e. the dialog is on hold. Checked
 *     only once a dialog is already known "confirmed" (a ringing/idle
 *     dialog has no rendering param to speak of). The regex is
 *     deliberately not scoped to a single <dialog>...</dialog> block
 *     (this parser was never a real XML parser, see this file's
 *     top-of-file comment) - matches the same "grab the first match
 *     anywhere in the body" simplicity <state> extraction already uses;
 *     fine for this engine's actual usage (one watched dialog per NOTIFY
 *     in every real capture so far, see core/E2E-F1.md).
 *     **Real-PBX finding (see core/E2E-F1.md "F5 presence_override"):
 *     this engine's test PBX (FreePBX 17.0.30 / Asterisk 22.8.2,
 *     chan_pjsip) does NOT emit this param when a dialog is put on local
 *     hold** - a real NOTIFY captured mid-hold (dual-contact 1100 trick,
 *     one side sending this engine's own `hold` command) came back
 *     byte-for-byte the same `<state>confirmed</state>`, no
 *     `<local>`/`<remote>`/`<target>` elements at all, as the plain busy
 *     case - confirmed across 3 separate NOTIFYs spanning the hold
 *     window (dialog-info's own incrementing `version=` attribute proves
 *     they're distinct NOTIFYs, not a stale capture). So this parser
 *     rule is implemented correctly to the RFC-documented shape (unit
 *     tested against synthetic RFC-shaped fixtures - see
 *     test_dialog_info_held() in test/test_main.c) and will fire the
 *     moment a NOTIFY actually carries the param, but **on this
 *     engine's actual test PBX, a held call currently still reports
 *     "busy", not "held"** - this is the PBX's own dialog-info
 *     implementation choice, not a bug in this parser (nothing to fix
 *     here without a different signal source, e.g. AMI/ARI, which is
 *     out of scope for this SIP-only engine - see PROTOCOL.md
 *     "Planned").
 *   - CENT_BLF_DND is a **best-effort, forward-compatible hook, NOT
 *     confirmed against this engine's real test PBX**: standard Asterisk
 *     chan_pjsip hints have no dedicated Event:dialog XML element for
 *     "this extension is in DND" (dialog-info is fundamentally a
 *     *dialog* package - DND is a device-config state, not a dialog: an
 *     idle-but-DND'd extension has zero active dialogs either way, so
 *     the real captured body for that case, per core/E2E-F1.md's own
 *     idle/unregistered capture, is indistinguishable from plain idle
 *     without something extra in the XML). This parser looks for a
 *     non-standard "<dnd>true</dnd>" element or a "dnd=" attribute
 *     (checked before the idle fallback, so it can override "idle" when
 *     present) in case a given PBX/vendor adds one; testing it against
 *     this repo's real test PBX would require toggling DND on the test
 *     extension via a feature code outside this repo's pre-authorized
 *     safe-target list (see core/E2E-F1.md), so that verification is
 *     explicitly **pending**, not claimed - see PROTOCOL.md "Planned".
 *
 * @param body Raw NOTIFY body bytes (NOT necessarily NUL-terminated -
 *             pass the exact length).
 * @param len  Length of body in bytes.
 *
 * @return The parsed state.
 */
enum cent_blf_state dialog_info_parse(const char *body, size_t len);

/**
 * State to report when a BLF subscription's underlying SIP transaction
 * failed/was rejected/expired (sipsub close_handler with an error, no
 * usable NOTIFY body) - always CENT_BLF_OFFLINE, split out as its own
 * named function (rather than inlining CENT_BLF_OFFLINE at the call
 * site) purely so that meaning is grep-able/documented at both use sites
 * (dialog_info_parse()'s fallback and the subscription-failure path in
 * ctrl_json.c).
 */
enum cent_blf_state cent_blf_state_for_close(void);

#endif /* CENTINELO_CTRL_JSON_DIALOG_INFO_H */

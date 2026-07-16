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
 * See core/PROTOCOL.md ("blf_subscribe", "Changes from v1.2") for the wire
 * event this feeds and the full presence_override (held/dnd) contract, and
 * core/E2E-F1.md ("F5") for real captured NOTIFY bodies and this engine's
 * actual real-PBX verification status for each state.
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
 * PROTOCOL.md "Changes from v1.2" and dialog_info_parse()'s own doc
 * comment below for exactly what triggers each and their real-PBX
 * verification status).
 */
enum cent_blf_state {
	CENT_BLF_IDLE = 0,
	CENT_BLF_RINGING,
	CENT_BLF_BUSY,
	CENT_BLF_HELD,     /**< v1.3 - a confirmed dialog whose NOTIFY body
			     *   also carries the RFC 4235/3840 standard hold
			     *   indication (a <target> "+sip.rendering" param,
			     *   pvalue="no"). Not observed on this engine's
			     *   real test PBX - see PROTOCOL.md/E2E-F1.md. */
	CENT_BLF_DND,      /**< v1.3 - best-effort, non-standard, only
			     *   overrides what would otherwise be "idle"
			     *   (no <dialog> element) - never a genuinely
			     *   active dialog. Unverified against a real
			     *   PBX - see PROTOCOL.md/E2E-F1.md. */
	CENT_BLF_OFFLINE,
};

const char *cent_blf_state_name(enum cent_blf_state state);

/**
 * Parse a NOTIFY body for the "Event: dialog" package
 * (application/dialog-info+xml, RFC 4235) into a BLF line state.
 *
 * Rules (see core/E2E-F1.md for the real captured bodies this was tuned
 * against, and PROTOCOL.md "Changes from v1.2" for the full
 * presence_override design/verification write-up):
 *   - NULL/empty body, or no "<dialog-info" root element at all (not a
 *     dialog-info document)                            -> CENT_BLF_OFFLINE
 *   - no "<dialog" element at all (state="full", zero dialogs) AND a
 *     "<dnd>true</dnd>" element/"dnd=" attribute is present (v1.3,
 *     best-effort, non-standard - never overrides a real dialog state
 *     below, only this "would otherwise be idle" case) -> CENT_BLF_DND
 *   - no "<dialog" element, no dnd marker              -> CENT_BLF_IDLE
 *   - a "<dialog" element whose "<state>" is "trying"/"proceeding"/
 *     "early"                                           -> CENT_BLF_RINGING
 *   - "<state>confirmed</state>", RFC 4235/3840 "+sip.rendering"
 *     pvalue="no" hold signal ALSO present (v1.3)        -> CENT_BLF_HELD
 *   - "<state>confirmed</state>", no hold signal found  -> CENT_BLF_BUSY
 *   - "<state>terminated</state>" (a dialog that just ended - back to
 *     no active dialogs)                                -> CENT_BLF_IDLE
 *   - a "<dialog" element with no parseable "<state>"    -> CENT_BLF_OFFLINE
 *     (best-effort "can't tell" bucket - see also
 *     cent_blf_state_for_close(), the sibling case where the
 *     subscription itself failed/was rejected before any NOTIFY body
 *     existed to parse at all).
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

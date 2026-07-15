/**
 * @file cmd.h  Centinelo Phone v2 - pure JSON command decoding
 *
 * Turns an already-decoded {"cmd":...} odict (see json_decode_odict() in
 * re_json.h) into a typed, engine-agnostic struct cent_cmd. Deliberately
 * has zero dependency on baresip.h / the SIP stack / the event loop -
 * only re's odict/string types - so it links into a small standalone test
 * binary and is unit tested without a running engine (see
 * core/modules/ctrl_json/test/). ctrl_json.c is the only real caller; it
 * does the part that genuinely can't be unit tested without a live
 * engine: taking a decoded struct cent_cmd and actually driving baresip.
 *
 * See core/PROTOCOL.md for the wire protocol these map to.
 *
 * Copyright (C) 2026 Neola Dental / Centinelo Phone
 */

#ifndef CENTINELO_CTRL_JSON_CMD_H
#define CENTINELO_CTRL_JSON_CMD_H

#include <stdbool.h>

struct odict;   /* re_odict.h - forward declared so this header stays
                 * dependency-free; every real caller already has re.h. */

enum cent_cmd_type {
	CENT_CMD_NONE = 0,      /**< hard decode error, see *errmsg        */
	CENT_CMD_UNKNOWN,       /**< valid JSON, unrecognised 'cmd' value  */
	CENT_CMD_DIAL,
	CENT_CMD_ANSWER,
	CENT_CMD_HANGUP,
	CENT_CMD_QUIT,
	CENT_CMD_REGISTER,
	CENT_CMD_UNREGISTER,
	CENT_CMD_HOLD,
	CENT_CMD_RESUME,
	CENT_CMD_DTMF,
	CENT_CMD_MUTE,
	CENT_CMD_BLIND_TRANSFER,
	CENT_CMD_ATTENDED_TRANSFER,
	CENT_CMD_COMPLETE_TRANSFER,
	CENT_CMD_ABORT_TRANSFER,
	CENT_CMD_QUALITY_STATS,
	CENT_CMD_BLF_SUBSCRIBE,
	CENT_CMD_BLF_UNSUBSCRIBE,
};

enum {
	CENT_URI_SIZE  = 512,   /**< dial / blind_transfer / attended_transfer uri */
	CENT_ID_SIZE   = 128,   /**< call_id */
	CENT_DTMF_SIZE = 64,    /**< dtmf digit string */
	CENT_EXT_SIZE  = 32,    /**< blf ext */
};

/**
 * A decoded command. Only the fields relevant to .type are meaningful;
 * everything else is zeroed by cent_cmd_decode().
 */
struct cent_cmd {
	enum cent_cmd_type type;

	char uri[CENT_URI_SIZE];
	char call_id[CENT_ID_SIZE];
	bool have_call_id;         /**< call_id is optional on every command
				     *   that carries it - this says whether
				     *   the caller supplied one or the
				     *   engine should fall back to "the
				     *   current call". */
	char digits[CENT_DTMF_SIZE];
	char ext[CENT_EXT_SIZE];
	bool mute_on;
};

/**
 * Decode a command out of an already JSON-parsed odict.
 *
 * @param out    Zeroed, then filled in. Always safe to read after any
 *               return value (CENT_CMD_NONE/UNKNOWN leave it zeroed
 *               except ->type).
 * @param od     Decoded {"cmd": ..., ...} object (od itself, not a
 *               top-level array/string/etc - see json_decode_odict()).
 * @param errmsg Set to a static, human-readable string when the return
 *               value is CENT_CMD_NONE (a required field was missing or
 *               the wrong JSON type). Untouched otherwise. Never left
 *               NULL after a CENT_CMD_NONE return.
 *
 * @return The decoded command type - CENT_CMD_NONE for a hard decode
 *         error (see *errmsg), CENT_CMD_UNKNOWN for a syntactically fine
 *         object whose 'cmd' string isn't one this protocol knows, or one
 *         of the concrete CENT_CMD_* values with `out` populated.
 */
enum cent_cmd_type cent_cmd_decode(struct cent_cmd *out,
				    const struct odict *od,
				    const char **errmsg);

#endif /* CENTINELO_CTRL_JSON_CMD_H */

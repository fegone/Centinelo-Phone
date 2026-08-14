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
 * Copyright (C) 2026 Centinelo Phone
 */

#ifndef CENTINELO_CTRL_JSON_CMD_H
#define CENTINELO_CTRL_JSON_CMD_H

#include <stdbool.h>
#include <stdint.h>

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
	CENT_CMD_DEVICES,
	CENT_CMD_SET_DEVICE,
	CENT_CMD_TAP_START,    /**< v1.2 - see PROTOCOL.md "tap_start" */
	CENT_CMD_TAP_STOP,     /**< v1.2 - see PROTOCOL.md "tap_stop" */
	CENT_CMD_PARK,         /**< v1.3 - see PROTOCOL.md "park" */
	CENT_CMD_SET_ANSWER_MODE, /**< v1.5 - see PROTOCOL.md "set_answer_mode" */
	CENT_CMD_CODECS,       /**< v1.6 - see PROTOCOL.md "codecs" */
	CENT_CMD_SET_CODECS,   /**< v1.6 - see PROTOCOL.md "set_codecs" */
};

enum {
	CENT_URI_SIZE  = 512,   /**< dial / blind_transfer / attended_transfer uri */
	CENT_ID_SIZE   = 128,   /**< call_id, and (v1.1) the request/response
				  *  correlation "id" - same size, same kind
				  *  of opaque caller-supplied token, no
				  *  reason for the two to differ. */
	CENT_DTMF_SIZE = 64,    /**< dtmf digit string */
	CENT_EXT_SIZE  = 32,    /**< blf ext */
	CENT_DEVICE_KIND_SIZE = 16,    /**< set_device "kind": "input"/"output" */
	CENT_DEVICE_NAME_SIZE = 192,   /**< set_device "name": see PROTOCOL.md
					 *   "devices" - big enough for the
					 *   composite "<module>,<device>"
					 *   shape that command's own "name"
					 *   fields use (config_audio's
					 *   src_mod/play_mod are 16 bytes,
					 *   src_dev/play_dev are 128, see
					 *   baresip.h struct config_audio -
					 *   16 + 1 + 128 with room to
					 *   spare). */
	CENT_DIR_SIZE  = 512,   /**< tap_start "dir": an absolute filesystem
				  *  path - same size class as CENT_URI_SIZE
				  *  (a directory path can legitimately be
				  *  long; no protocol reason to cap it
				  *  tighter). See PROTOCOL.md "tap_start". */

	CENT_CODEC_NAME_SIZE = 32,  /**< set_codecs: one codec name, e.g.
				      *  "opus"/"pcmu"/"pcma" - baresip's own
				      *  aucodec names are short ASCII
				      *  identifiers (see modules/g711/g711.c,
				      *  modules/opus/opus.c); 32 is generous
				      *  headroom, not a tight fit. */
	CENT_MAX_CODECS = 16,       /**< set_codecs: matches baresip's own
				      *  struct account::acv[16] cap (see
				      *  core/deps/baresip/src/core.h) - the
				      *  account-level codec-restriction list
				      *  literally cannot hold more than this
				      *  many entries, so this protocol layer
				      *  rejects an oversized list up front
				      *  with a clear error instead of letting
				      *  account_set_audio_codecs() silently
				      *  truncate it. */
};   /* close the anonymous CENT_* enum above */

/**
 * A resolved audio codec's identity - the three fields set_codecs needs
 * from a real baresip struct aucodec to build the fully-pinned
 * "name/srate/ch" string account_set_audio_codecs()/audio_codecs_decode()
 * resolves unambiguously. Dependency-free (no baresip.h) so the builder
 * (cent_build_codecs_string(), below) is unit-testable in the standalone
 * test binary; ctrl_json.c's cmd_set_codecs() adapts each resolved
 * struct aucodec into one of these before calling the builder.
 *
 * Why srate/ch live here and not just .name: a BARE name ("opus") handed
 * to account_set_audio_codecs() is a real, silent-fallback bug - opus
 * registers at 48000/2, not the parser's 8000/1 bare-name default, so the
 * bare name never resolves and account_aucodecl() then falls back to the
 * ENTIRE compiled-in catalog (see ctrl_json.c cmd_set_codecs()'s top
 * comment + core/BUILD.md "Findings"). Pinning the suffix here from the
 * resolved aucodec closes that hole by construction - which is exactly
 * what the unit test for cent_build_codecs_string() guards against a
 * regression of.
 */
struct cent_codec_spec {
	const char *name;   /**< codec name, e.g. "opus"/"pcmu"/"pcma"      */
	uint32_t   srate;   /**< sample rate, e.g. opus 48000 / g711 8000   */
	uint8_t    ch;      /**< channel count, e.g. opus 2 / g711 1        */
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
				     *   current call". v1.3: now also decoded
				     *   for "answer" (see PROTOCOL.md
				     *   "answer") - retrocompatible, an
				     *   answer with no call_id keeps
				     *   targeting "the current incoming
				     *   call", same as v1/v1.1/v1.2. */
	char digits[CENT_DTMF_SIZE];
	char ext[CENT_EXT_SIZE];   /**< blf_subscribe/blf_unsubscribe's
				     *   watched extension, and (v1.3) park's
				     *   target parking-lot pilot extension -
				     *   same "bare extension on the account's
				     *   own PBX host" shape in both cases, so
				     *   this field is shared rather than
				     *   growing a second one - see
				     *   PROTOCOL.md "park". */
	bool mute_on;

	bool answer_auto;   /**< set_answer_mode: true = "auto" (this
			     *   account answers every incoming INVITE with 200
			     *   OK, via baresip's per-account answermode), false
			     *   = "manual" (the default - wait for the "answer"
			     *   command). v1.5 - see PROTOCOL.md
			     *   "set_answer_mode". Stored as a bool, not the
			     *   baresip enum, to keep this header dependency-free;
			     *   ctrl_json.c maps it to enum answermode. */

	/** v1.1 request/response correlation (see PROTOCOL.md) - unlike
	 * every other field above, valid regardless of .type: decoded
	 * unconditionally before the 'cmd' field itself is even inspected,
	 * so it's populated even for a CENT_CMD_NONE/CENT_CMD_UNKNOWN
	 * decode (a malformed or unrecognised command can still be
	 * correlated back to whoever sent it - see
	 * ctrl_json.c process_line()). */
	char id[CENT_ID_SIZE];
	bool have_id;

	char device_kind[CENT_DEVICE_KIND_SIZE]; /**< set_device: "input" or
						    *  "output", already
						    *  validated by
						    *  cent_cmd_decode(). */
	char device_name[CENT_DEVICE_NAME_SIZE]; /**< set_device: target
						    *  device name - opaque
						    *  here, see PROTOCOL.md
						    *  "devices"/"set_device"
						    *  for the "<module>[,
						    *  <device>]" shape
						    *  ctrl_json.c parses. */

	char dir[CENT_DIR_SIZE];   /**< tap_start: absolute directory the
				     *   engine writes <call_id>-rx.wav /
				     *   -tx.wav into - see PROTOCOL.md
				     *   "tap_start". Required (unlike
				     *   call_id) - a missing/empty dir is a
				     *   CENT_CMD_NONE decode error, same
				     *   treatment as dial's "uri". */

	/** set_codecs: ordered codec name list, decoded from the "codecs"
	 * JSON array - see PROTOCOL.md "set_codecs". Everything this layer
	 * can validate *without* a running engine (non-empty array, each
	 * entry a non-empty string that fits CENT_CODEC_NAME_SIZE, no more
	 * than CENT_MAX_CODECS entries, no case-insensitive duplicate) is
	 * rejected here as a CENT_CMD_NONE decode error; whether each name
	 * is actually a codec *this build* has compiled in is a separate,
	 * impure check only ctrl_json.c's cmd_set_codecs() can make (it
	 * needs baresip_aucodecl(), the live registered-codec list - see
	 * that function's own comment). */
	char codecs[CENT_MAX_CODECS][CENT_CODEC_NAME_SIZE];
	size_t codecs_len;
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

/**
 * Build the comma-separated "name/srate/ch,name/srate/ch,..." string
 * account_set_audio_codecs() consumes, from already-resolved codec specs.
 *
 * Every srate/ch in the output is taken from the passed-in specs (which
 * ctrl_json.c fills from the real struct aucodec it resolved via
 * aucodec_find()) - NEVER a bare requested name and NEVER the 8000/1
 * bare-name default audio_codecs_decode() would otherwise assume. That is
 * the whole point of this function: it is the unit-tested guard against
 * the silent-fallback bug (see struct cent_codec_spec's own comment).
 *
 * @param specs  Resolved codecs (each .name/.srate/.ch from a real struct
 *               aucodec); may be NULL only if n == 0.
 * @param n      Number of entries in specs (0 .. CENT_MAX_CODECS).
 * @param buf    Output buffer; always NUL-terminated on return.
 * @param sz     Size of buf in bytes.
 *
 * @return 0 on success, -1 if the result would not fit (treated by the
 *         only caller as an internal/buffer error - the inputs are already
 *         length-capped at this layer, so overflow is a bug, not bad input).
 */
int cent_build_codecs_string(const struct cent_codec_spec *specs, size_t n,
			     char *buf, size_t sz);

#endif /* CENTINELO_CTRL_JSON_CMD_H */

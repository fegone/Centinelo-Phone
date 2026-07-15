/**
 * @file cmd.c  Centinelo Phone v2 - pure JSON command decoding
 *
 * See cmd.h. Pure string/odict-field extraction, no SIP/engine state, no
 * I/O - so core/modules/ctrl_json/test/test_main.c can exercise this
 * without a running baresip.
 *
 * Copyright (C) 2026 Neola Dental / Centinelo Phone
 */

#include <re.h>
#include <string.h>
#include "cmd.h"


/* Copies a required string field. Returns false (and sets *errmsg) if
 * missing/empty; true and copies (truncating, never overflowing) if
 * present. `what` is only used to build *errmsg. */
static bool require_str(const struct odict *od, const char *key,
			 char *dst, size_t dst_size,
			 const char *cmdname, const char **errmsg)
{
	const char *v = odict_string(od, key);
	static char msg[160];

	if (!v || !v[0]) {
		(void)re_snprintf(msg, sizeof(msg), "%s: missing '%s' field",
				   cmdname, key);
		*errmsg = msg;
		return false;
	}

	str_ncpy(dst, v, dst_size);
	return true;
}


/* Optional call_id, shared by most call-scoped commands. */
static void optional_call_id(const struct odict *od, struct cent_cmd *out)
{
	const char *v = odict_string(od, "call_id");

	if (v && v[0]) {
		str_ncpy(out->call_id, v, sizeof(out->call_id));
		out->have_call_id = true;
	}
}


/* Optional "id" - v1.1 request/response correlation (see PROTOCOL.md).
 * Unlike call_id (only meaningful on call-scoped commands), id applies to
 * every command, so this is called unconditionally, before 'cmd' itself
 * is even inspected - see cent_cmd_decode() below - not from inside each
 * per-command branch the way optional_call_id() is. */
static void optional_id(const struct odict *od, struct cent_cmd *out)
{
	const char *v = odict_string(od, "id");

	if (v && v[0]) {
		str_ncpy(out->id, v, sizeof(out->id));
		out->have_id = true;
	}
}


enum cent_cmd_type cent_cmd_decode(struct cent_cmd *out,
				    const struct odict *od,
				    const char **errmsg)
{
	static const char *fallback_errmsg = "decode error";
	const char *cmd;

	if (!errmsg)
		return CENT_CMD_NONE;
	*errmsg = fallback_errmsg;

	if (!out || !od)
		return CENT_CMD_NONE;

	memset(out, 0, sizeof(*out));

	/* Unconditional, before 'cmd' is even looked at - see
	 * optional_id()'s comment: every command MAY carry an id, including
	 * ones that go on to decode as CENT_CMD_NONE/CENT_CMD_UNKNOWN. */
	optional_id(od, out);

	cmd = odict_string(od, "cmd");
	if (!cmd || !cmd[0]) {
		*errmsg = "missing 'cmd' field";
		return CENT_CMD_NONE;
	}

	if (!str_casecmp(cmd, "dial")) {
		if (!require_str(od, "uri", out->uri, sizeof(out->uri),
				  "dial", errmsg))
			return CENT_CMD_NONE;
		out->type = CENT_CMD_DIAL;
	}
	else if (!str_casecmp(cmd, "answer")) {
		out->type = CENT_CMD_ANSWER;
	}
	else if (!str_casecmp(cmd, "hangup")) {
		optional_call_id(od, out);
		out->type = CENT_CMD_HANGUP;
	}
	else if (!str_casecmp(cmd, "quit")) {
		out->type = CENT_CMD_QUIT;
	}
	else if (!str_casecmp(cmd, "register")) {
		out->type = CENT_CMD_REGISTER;
	}
	else if (!str_casecmp(cmd, "unregister")) {
		out->type = CENT_CMD_UNREGISTER;
	}
	else if (!str_casecmp(cmd, "hold")) {
		optional_call_id(od, out);
		out->type = CENT_CMD_HOLD;
	}
	else if (!str_casecmp(cmd, "resume")) {
		optional_call_id(od, out);
		out->type = CENT_CMD_RESUME;
	}
	else if (!str_casecmp(cmd, "dtmf")) {
		if (!require_str(od, "digits", out->digits,
				  sizeof(out->digits), "dtmf", errmsg))
			return CENT_CMD_NONE;
		optional_call_id(od, out);
		out->type = CENT_CMD_DTMF;
	}
	else if (!str_casecmp(cmd, "mute")) {
		bool on;

		if (!odict_get_boolean(od, &on, "on")) {
			*errmsg = "mute: missing/invalid 'on' field"
				  " (want a JSON boolean)";
			return CENT_CMD_NONE;
		}
		out->mute_on = on;
		optional_call_id(od, out);
		out->type = CENT_CMD_MUTE;
	}
	else if (!str_casecmp(cmd, "blind_transfer")) {
		if (!require_str(od, "uri", out->uri, sizeof(out->uri),
				  "blind_transfer", errmsg))
			return CENT_CMD_NONE;
		optional_call_id(od, out);
		out->type = CENT_CMD_BLIND_TRANSFER;
	}
	else if (!str_casecmp(cmd, "attended_transfer")) {
		if (!require_str(od, "uri", out->uri, sizeof(out->uri),
				  "attended_transfer", errmsg))
			return CENT_CMD_NONE;
		optional_call_id(od, out);
		out->type = CENT_CMD_ATTENDED_TRANSFER;
	}
	else if (!str_casecmp(cmd, "complete_transfer")) {
		optional_call_id(od, out);
		out->type = CENT_CMD_COMPLETE_TRANSFER;
	}
	else if (!str_casecmp(cmd, "abort_transfer")) {
		out->type = CENT_CMD_ABORT_TRANSFER;
	}
	else if (!str_casecmp(cmd, "quality_stats")) {
		optional_call_id(od, out);
		out->type = CENT_CMD_QUALITY_STATS;
	}
	else if (!str_casecmp(cmd, "blf_subscribe")) {
		if (!require_str(od, "ext", out->ext, sizeof(out->ext),
				  "blf_subscribe", errmsg))
			return CENT_CMD_NONE;
		out->type = CENT_CMD_BLF_SUBSCRIBE;
	}
	else if (!str_casecmp(cmd, "blf_unsubscribe")) {
		if (!require_str(od, "ext", out->ext, sizeof(out->ext),
				  "blf_unsubscribe", errmsg))
			return CENT_CMD_NONE;
		out->type = CENT_CMD_BLF_UNSUBSCRIBE;
	}
	else if (!str_casecmp(cmd, "devices")) {
		out->type = CENT_CMD_DEVICES;
	}
	else if (!str_casecmp(cmd, "set_device")) {
		const char *kind = odict_string(od, "kind");

		if (!kind || (str_casecmp(kind, "input") &&
			      str_casecmp(kind, "output"))) {
			*errmsg = "set_device: 'kind' must be \"input\" or"
				  " \"output\"";
			return CENT_CMD_NONE;
		}
		str_ncpy(out->device_kind, kind, sizeof(out->device_kind));

		if (!require_str(od, "name", out->device_name,
				  sizeof(out->device_name), "set_device",
				  errmsg))
			return CENT_CMD_NONE;

		out->type = CENT_CMD_SET_DEVICE;
	}
	else {
		out->type = CENT_CMD_UNKNOWN;
	}

	return out->type;
}

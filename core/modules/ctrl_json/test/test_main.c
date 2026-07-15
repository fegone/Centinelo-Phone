/**
 * @file test_main.c  Centinelo Phone v2 - ctrl_json unit tests
 *
 * Exercises the two pieces of ctrl_json that are pure/parseable and so
 * don't need a running baresip engine or a live SIP dialog:
 *   - cmd.c        JSON command line -> struct cent_cmd
 *   - dialog_info.c  dialog-info+xml NOTIFY body -> BLF state
 *
 * Standalone: links only libre (for json_decode_odict()/odict_*, used
 * exactly like the real ctrl_json.c stdin path so these tests exercise
 * the actual production decode pipeline, not a re-implementation of it)
 * plus this module's own cmd.c/dialog_info.c - no baresip, no network, no
 * event loop beyond libre_init()/libre_close() (matching
 * core/deps/re/test/main.c's own convention).
 *
 * Run via ctest (see test/CMakeLists.txt) or directly:
 *   ./ctrl_json_test
 *
 * Copyright (C) 2026 Neola Dental / Centinelo Phone
 */

#include <re.h>
#include <stdio.h>
#include <string.h>
#include "../cmd.h"
#include "../dialog_info.h"


static int failures = 0;
static int checks   = 0;


#define CHECK(cond, desc)                                                   \
	do {                                                                 \
		++checks;                                                     \
		if (!(cond)) {                                                \
			++failures;                                           \
			(void)fprintf(stderr,                                 \
				      "FAIL %s:%d: %s\n",                     \
				      __FILE__, __LINE__, (desc));            \
		}                                                             \
	} while (0)

#define CHECK_STREQ(a, b, desc)                                              \
	CHECK(0 == str_cmp((a), (b)), desc)


/* Decodes `json` through the real json_decode_odict() -> cent_cmd_decode()
 * pipeline, exactly as ctrl_json.c's process_line() does. */
static enum cent_cmd_type decode(const char *json, struct cent_cmd *out,
				  const char **errmsg)
{
	struct odict *od = NULL;
	enum cent_cmd_type type;
	static const char *no_errmsg = NULL;

	if (!errmsg)
		errmsg = &no_errmsg;

	if (json_decode_odict(&od, 8, json, str_len(json), 8)) {
		CHECK(false, "test bug: fixture JSON itself failed to parse");
		return CENT_CMD_NONE;
	}

	type = cent_cmd_decode(out, od, errmsg);

	mem_deref(od);

	return type;
}


static void test_cmd_dial(void)
{
	struct cent_cmd cmd;
	const char *err = NULL;

	CHECK(CENT_CMD_DIAL ==
	      decode("{\"cmd\":\"dial\",\"uri\":\"sip:*43@host\"}",
		     &cmd, &err), "dial: type");
	CHECK_STREQ(cmd.uri, "sip:*43@host", "dial: uri");
	CHECK(!cmd.have_call_id, "dial: no call_id by default");

	CHECK(CENT_CMD_NONE == decode("{\"cmd\":\"dial\"}", &cmd, &err),
	      "dial: missing uri -> NONE");
	CHECK(err != NULL && strstr(err, "uri") != NULL,
	      "dial: missing uri -> errmsg mentions 'uri'");
}


static void test_cmd_simple_noargs(void)
{
	struct cent_cmd cmd;

	CHECK(CENT_CMD_ANSWER == decode("{\"cmd\":\"answer\"}", &cmd, NULL),
	      "answer: type");
	CHECK(CENT_CMD_QUIT == decode("{\"cmd\":\"quit\"}", &cmd, NULL),
	      "quit: type");
	CHECK(CENT_CMD_REGISTER == decode("{\"cmd\":\"register\"}", &cmd, NULL),
	      "register: type");
	CHECK(CENT_CMD_UNREGISTER ==
	      decode("{\"cmd\":\"unregister\"}", &cmd, NULL),
	      "unregister: type");
	CHECK(CENT_CMD_ABORT_TRANSFER ==
	      decode("{\"cmd\":\"abort_transfer\"}", &cmd, NULL),
	      "abort_transfer: type");

	/* case-insensitive cmd name, matching v0's dial/answer/hangup/quit */
	CHECK(CENT_CMD_ANSWER == decode("{\"cmd\":\"ANSWER\"}", &cmd, NULL),
	      "answer: cmd name is case-insensitive");
}


static void test_cmd_call_id_optional(void)
{
	struct cent_cmd cmd;

	CHECK(CENT_CMD_HANGUP ==
	      decode("{\"cmd\":\"hangup\",\"call_id\":\"abc123\"}",
		     &cmd, NULL), "hangup: type");
	CHECK(cmd.have_call_id, "hangup: call_id present -> have_call_id");
	CHECK_STREQ(cmd.call_id, "abc123", "hangup: call_id value");

	CHECK(CENT_CMD_HANGUP == decode("{\"cmd\":\"hangup\"}", &cmd, NULL),
	      "hangup: type (no call_id)");
	CHECK(!cmd.have_call_id, "hangup: no call_id -> have_call_id false");

	CHECK(CENT_CMD_HOLD ==
	      decode("{\"cmd\":\"hold\",\"call_id\":\"x1\"}", &cmd, NULL),
	      "hold: type");
	CHECK_STREQ(cmd.call_id, "x1", "hold: call_id value");

	CHECK(CENT_CMD_RESUME == decode("{\"cmd\":\"resume\"}", &cmd, NULL),
	      "resume: type");
	CHECK(!cmd.have_call_id, "resume: no call_id -> have_call_id false");
}


static void test_cmd_dtmf(void)
{
	struct cent_cmd cmd;
	const char *err = NULL;

	CHECK(CENT_CMD_DTMF ==
	      decode("{\"cmd\":\"dtmf\",\"digits\":\"123#\"}", &cmd, &err),
	      "dtmf: type");
	CHECK_STREQ(cmd.digits, "123#", "dtmf: digits value");

	CHECK(CENT_CMD_NONE == decode("{\"cmd\":\"dtmf\"}", &cmd, &err),
	      "dtmf: missing digits -> NONE");
	CHECK(err != NULL && strstr(err, "digits") != NULL,
	      "dtmf: missing digits -> errmsg mentions 'digits'");
}


static void test_cmd_mute(void)
{
	struct cent_cmd cmd;
	const char *err = NULL;

	CHECK(CENT_CMD_MUTE ==
	      decode("{\"cmd\":\"mute\",\"on\":true}", &cmd, &err),
	      "mute on=true: type");
	CHECK(cmd.mute_on, "mute on=true: mute_on is true");

	CHECK(CENT_CMD_MUTE ==
	      decode("{\"cmd\":\"mute\",\"on\":false}", &cmd, &err),
	      "mute on=false: type");
	CHECK(!cmd.mute_on, "mute on=false: mute_on is false");

	CHECK(CENT_CMD_NONE == decode("{\"cmd\":\"mute\"}", &cmd, &err),
	      "mute: missing 'on' -> NONE");

	/* 'on' must be a real JSON boolean, not the string "true" - this
	 * guards against a shell client accidentally stringifying it. */
	CHECK(CENT_CMD_NONE ==
	      decode("{\"cmd\":\"mute\",\"on\":\"true\"}", &cmd, &err),
	      "mute: 'on' as a JSON string (not boolean) -> NONE");
}


static void test_cmd_transfer(void)
{
	struct cent_cmd cmd;
	const char *err = NULL;

	CHECK(CENT_CMD_BLIND_TRANSFER ==
	      decode("{\"cmd\":\"blind_transfer\",\"uri\":\"sip:*97@host\"}",
		     &cmd, &err), "blind_transfer: type");
	CHECK_STREQ(cmd.uri, "sip:*97@host", "blind_transfer: uri");

	CHECK(CENT_CMD_NONE ==
	      decode("{\"cmd\":\"blind_transfer\"}", &cmd, &err),
	      "blind_transfer: missing uri -> NONE");

	CHECK(CENT_CMD_ATTENDED_TRANSFER ==
	      decode("{\"cmd\":\"attended_transfer\",\"uri\":\"sip:510@host\","
		     "\"call_id\":\"c1\"}", &cmd, &err),
	      "attended_transfer: type");
	CHECK_STREQ(cmd.uri, "sip:510@host", "attended_transfer: uri");
	CHECK_STREQ(cmd.call_id, "c1", "attended_transfer: call_id");

	CHECK(CENT_CMD_COMPLETE_TRANSFER ==
	      decode("{\"cmd\":\"complete_transfer\"}", &cmd, &err),
	      "complete_transfer: type");
}


static void test_cmd_quality_stats_and_blf(void)
{
	struct cent_cmd cmd;
	const char *err = NULL;

	CHECK(CENT_CMD_QUALITY_STATS ==
	      decode("{\"cmd\":\"quality_stats\",\"call_id\":\"c1\"}",
		     &cmd, &err), "quality_stats: type");
	CHECK(cmd.have_call_id, "quality_stats: have_call_id");

	CHECK(CENT_CMD_BLF_SUBSCRIBE ==
	      decode("{\"cmd\":\"blf_subscribe\",\"ext\":\"510\"}",
		     &cmd, &err), "blf_subscribe: type");
	CHECK_STREQ(cmd.ext, "510", "blf_subscribe: ext value");

	CHECK(CENT_CMD_NONE ==
	      decode("{\"cmd\":\"blf_subscribe\"}", &cmd, &err),
	      "blf_subscribe: missing ext -> NONE");

	CHECK(CENT_CMD_BLF_UNSUBSCRIBE ==
	      decode("{\"cmd\":\"blf_unsubscribe\",\"ext\":\"510\"}",
		     &cmd, &err), "blf_unsubscribe: type");
}


static void test_cmd_unknown_and_malformed(void)
{
	struct cent_cmd cmd;
	const char *err = NULL;

	CHECK(CENT_CMD_UNKNOWN == decode("{\"cmd\":\"levitate\"}", &cmd, &err),
	      "unknown cmd value -> CENT_CMD_UNKNOWN");

	CHECK(CENT_CMD_NONE == decode("{}", &cmd, &err),
	      "missing 'cmd' field -> CENT_CMD_NONE");
	CHECK(err != NULL, "missing 'cmd' field -> errmsg set");
}


/*
 * v1.1: request/response correlation ("id" - see PROTOCOL.md "result").
 * id is decoded unconditionally, before 'cmd' is even inspected (see
 * cmd.c cent_cmd_decode()/optional_id()), so - unlike call_id, which
 * only matters on call-scoped commands - it must survive every decode
 * outcome: a normal command, a command with no id at all, and even the
 * two "failed to fully decode" outcomes (CENT_CMD_NONE/CENT_CMD_UNKNOWN)
 * - the whole point being that a caller can correlate a rejected/
 * unrecognised command back to itself too, not just a successful one.
 */
static void test_cmd_id_correlation(void)
{
	struct cent_cmd cmd;
	const char *err = NULL;

	CHECK(CENT_CMD_ANSWER ==
	      decode("{\"cmd\":\"answer\",\"id\":\"req-1\"}", &cmd, &err),
	      "answer+id: type");
	CHECK(cmd.have_id, "answer+id: have_id true");
	CHECK_STREQ(cmd.id, "req-1", "answer+id: id value");

	CHECK(CENT_CMD_ANSWER == decode("{\"cmd\":\"answer\"}", &cmd, &err),
	      "answer without id: type");
	CHECK(!cmd.have_id, "answer without id: have_id false");

	CHECK(CENT_CMD_HOLD ==
	      decode("{\"cmd\":\"hold\",\"call_id\":\"c1\",\"id\":\"req-2\"}",
		     &cmd, &err), "hold+call_id+id: type");
	CHECK(cmd.have_call_id, "hold+call_id+id: have_call_id true");
	CHECK_STREQ(cmd.call_id, "c1", "hold+call_id+id: call_id value");
	CHECK(cmd.have_id, "hold+call_id+id: have_id true");
	CHECK_STREQ(cmd.id, "req-2", "hold+call_id+id: id value - id and"
		    " call_id are independent fields, not aliases");

	/* id survives even a hard decode error (missing 'cmd') ... */
	CHECK(CENT_CMD_NONE ==
	      decode("{\"id\":\"req-3\"}", &cmd, &err),
	      "id with missing 'cmd' field -> CENT_CMD_NONE");
	CHECK(cmd.have_id, "missing 'cmd' but id present -> have_id true");
	CHECK_STREQ(cmd.id, "req-3", "missing 'cmd' but id present -> id"
		    " value still decoded");
	CHECK(err != NULL, "missing 'cmd' field -> errmsg still set");

	/* ... and an unrecognised cmd value. */
	CHECK(CENT_CMD_UNKNOWN ==
	      decode("{\"cmd\":\"levitate\",\"id\":\"req-4\"}", &cmd, &err),
	      "unknown cmd + id -> CENT_CMD_UNKNOWN");
	CHECK(cmd.have_id, "unknown cmd + id -> have_id true");
	CHECK_STREQ(cmd.id, "req-4", "unknown cmd + id -> id value");

	/* a required-field failure (dial with no uri) also keeps id. */
	CHECK(CENT_CMD_NONE ==
	      decode("{\"cmd\":\"dial\",\"id\":\"req-5\"}", &cmd, &err),
	      "dial missing uri + id -> CENT_CMD_NONE");
	CHECK(cmd.have_id, "dial missing uri + id -> have_id true");
	CHECK_STREQ(cmd.id, "req-5", "dial missing uri + id -> id value");
}


/*
 * v1.1: "devices" (no fields) and "set_device" (see PROTOCOL.md
 * "devices"/"set_device"). cmd.c's job here is purely mechanical field
 * extraction + 'kind' validation - the actual module/device name
 * splitting (the "<module>[,<device>]" convention) happens in
 * ctrl_json.c's cmd_set_device(), out of reach of this standalone test
 * binary (see this file's own top-of-file comment) - so device_name is
 * checked here only as the opaque string cent_cmd_decode() is supposed
 * to copy verbatim, same as every other free-form string field (uri,
 * digits, ext, ...).
 */
static void test_cmd_devices_and_set_device(void)
{
	struct cent_cmd cmd;
	const char *err = NULL;

	CHECK(CENT_CMD_DEVICES == decode("{\"cmd\":\"devices\"}", &cmd, &err),
	      "devices: type");

	CHECK(CENT_CMD_SET_DEVICE ==
	      decode("{\"cmd\":\"set_device\",\"kind\":\"input\","
		     "\"name\":\"ausine,440\"}", &cmd, &err),
	      "set_device input: type");
	CHECK_STREQ(cmd.device_kind, "input", "set_device input: kind value");
	CHECK_STREQ(cmd.device_name, "ausine,440",
		    "set_device input: name value verbatim (module,device)");

	CHECK(CENT_CMD_SET_DEVICE ==
	      decode("{\"cmd\":\"set_device\",\"kind\":\"output\","
		     "\"name\":\"aufile\"}", &cmd, &err),
	      "set_device output: type");
	CHECK_STREQ(cmd.device_kind, "output",
		    "set_device output: kind value");
	CHECK_STREQ(cmd.device_name, "aufile",
		    "set_device output: name value verbatim (no comma)");

	/* case-insensitive kind, matching cmd's own case-insensitivity */
	CHECK(CENT_CMD_SET_DEVICE ==
	      decode("{\"cmd\":\"set_device\",\"kind\":\"INPUT\","
		     "\"name\":\"x\"}", &cmd, &err),
	      "set_device: kind is case-insensitive");

	CHECK(CENT_CMD_NONE ==
	      decode("{\"cmd\":\"set_device\",\"kind\":\"sideways\","
		     "\"name\":\"x\"}", &cmd, &err),
	      "set_device: invalid kind -> CENT_CMD_NONE");
	CHECK(err != NULL && strstr(err, "kind") != NULL,
	      "set_device: invalid kind -> errmsg mentions 'kind'");

	CHECK(CENT_CMD_NONE ==
	      decode("{\"cmd\":\"set_device\",\"name\":\"x\"}", &cmd, &err),
	      "set_device: missing kind -> CENT_CMD_NONE");

	CHECK(CENT_CMD_NONE ==
	      decode("{\"cmd\":\"set_device\",\"kind\":\"input\"}",
		     &cmd, &err),
	      "set_device: missing name -> CENT_CMD_NONE");
	CHECK(err != NULL && strstr(err, "name") != NULL,
	      "set_device: missing name -> errmsg mentions 'name'");
}


static void test_dialog_info_idle(void)
{
	static const char idle[] =
		"<?xml version=\"1.0\"?>\n"
		"<dialog-info xmlns=\"urn:ietf:params:xml:ns:dialog-info\" "
		"version=\"0\" state=\"full\" "
		"entity=\"sip:510@pbx.example.com\"/>\n";

	CHECK(CENT_BLF_IDLE ==
	      dialog_info_parse(idle, str_len(idle)),
	      "dialog_info: state=full, no <dialog> child -> idle");

	/* Regression guard: "<dialog-info" itself must never be mistaken
	 * for a "<dialog" *element* (see dialog_info.c's comment on the
	 * exact delimiter this relies on). */
	CHECK(CENT_BLF_IDLE == dialog_info_parse("<dialog-info></dialog-info>",
			str_len("<dialog-info></dialog-info>")),
	      "dialog_info: bare <dialog-info> root only -> idle, "
	      "not misread as a <dialog> element");
}


static void test_dialog_info_ringing(void)
{
	static const char early[] =
		"<dialog-info state=\"full\">"
		"<dialog id=\"abc\"><state>early</state></dialog>"
		"</dialog-info>";
	static const char trying[] =
		"<dialog-info state=\"full\">"
		"<dialog id=\"abc\"><state>trying</state></dialog>"
		"</dialog-info>";
	static const char proceeding[] =
		"<dialog-info state=\"full\">"
		"<dialog id=\"abc\"><state>proceeding</state></dialog>"
		"</dialog-info>";

	CHECK(CENT_BLF_RINGING == dialog_info_parse(early, str_len(early)),
	      "dialog_info: <state>early</state> -> ringing");
	CHECK(CENT_BLF_RINGING == dialog_info_parse(trying, str_len(trying)),
	      "dialog_info: <state>trying</state> -> ringing");
	CHECK(CENT_BLF_RINGING ==
	      dialog_info_parse(proceeding, str_len(proceeding)),
	      "dialog_info: <state>proceeding</state> -> ringing");
}


static void test_dialog_info_busy(void)
{
	static const char confirmed[] =
		"<dialog-info state=\"full\">"
		"<dialog id=\"abc\" direction=\"recipient\">"
		"<state>confirmed</state>"
		"</dialog>"
		"</dialog-info>";

	CHECK(CENT_BLF_BUSY ==
	      dialog_info_parse(confirmed, str_len(confirmed)),
	      "dialog_info: <state>confirmed</state> -> busy");

	/* case-insensitivity + surrounding whitespace inside the tags -
	 * re_regex matches literal chars case-insensitively, and the
	 * value capture stops at the first non-letter so trailing
	 * whitespace before "</state>" doesn't leak into the match. */
	{
		static const char loose[] =
			"<DIALOG-INFO><DIALOG id=\"x\">"
			"<STATE>  Confirmed  </STATE>"
			"</DIALOG></DIALOG-INFO>";

		CHECK(CENT_BLF_BUSY == dialog_info_parse(loose, str_len(loose)),
		      "dialog_info: mixed-case tags + inner whitespace"
		      " -> still busy");
	}
}


static void test_dialog_info_terminated_and_unknown(void)
{
	static const char terminated[] =
		"<dialog-info state=\"full\">"
		"<dialog id=\"abc\"><state>terminated</state></dialog>"
		"</dialog-info>";
	static const char no_state[] =
		"<dialog-info state=\"full\">"
		"<dialog id=\"abc\"/>"
		"</dialog-info>";
	static const char garbage[] = "not even xml";

	CHECK(CENT_BLF_IDLE ==
	      dialog_info_parse(terminated, str_len(terminated)),
	      "dialog_info: <state>terminated</state> -> idle"
	      " (dialog just ended)");

	CHECK(CENT_BLF_OFFLINE ==
	      dialog_info_parse(no_state, str_len(no_state)),
	      "dialog_info: <dialog> present but no <state> -> offline"
	      " (can't tell)");

	CHECK(CENT_BLF_OFFLINE == dialog_info_parse(garbage, str_len(garbage)),
	      "dialog_info: unparseable body -> offline");

	CHECK(CENT_BLF_OFFLINE == dialog_info_parse(NULL, 0),
	      "dialog_info: NULL body -> offline (fail closed)");
	CHECK(CENT_BLF_OFFLINE == dialog_info_parse("", 0),
	      "dialog_info: zero-length body -> offline (fail closed)");
}


/*
 * Real body captured against the test PBX (Asterisk chan_pjsip hint
 * dialog-info, FPBX-17.0.30/22.8.2) subscribing to ext 510 while idle -
 * see core/E2E-F1.md scenario (c). Verbatim, no secrets in it. Notable:
 * the real server sends a *populated* <dialog> element with
 * state=terminated for "no active call", not an empty/absent <dialog>
 * as this parser's first version assumed before this body was captured
 * - both shapes correctly resolve to CENT_BLF_IDLE (see
 * dialog_info.c's rules), but only the real capture proves the server
 * actually uses this shape in practice.
 */
static void test_dialog_info_real_capture_ext510_idle(void)
{
	static const char real_body[] =
		"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n"
		"<dialog-info xmlns=\"urn:ietf:params:xml:ns:dialog-info\""
		" version=\"0\" state=\"full\""
		" entity=\"sip:510@100.119.230.80\">\r\n"
		" <dialog id=\"510\">\r\n"
		"  <state>terminated</state>\r\n"
		" </dialog>\r\n"
		"</dialog-info>\r\n";

	CHECK(CENT_BLF_IDLE ==
	      dialog_info_parse(real_body, str_len(real_body)),
	      "dialog_info: real ext-510 capture (state=full,"
	      " dialog state=terminated) -> idle");
}


static void test_blf_state_name(void)
{
	CHECK_STREQ(cent_blf_state_name(CENT_BLF_IDLE), "idle", "name: idle");
	CHECK_STREQ(cent_blf_state_name(CENT_BLF_RINGING), "ringing",
		    "name: ringing");
	CHECK_STREQ(cent_blf_state_name(CENT_BLF_BUSY), "busy", "name: busy");
	CHECK_STREQ(cent_blf_state_name(CENT_BLF_OFFLINE), "offline",
		    "name: offline");
}


int main(void)
{
	int err;

	err = libre_init();
	if (err) {
		(void)fprintf(stderr, "libre_init failed: %d\n", err);
		return 2;
	}

	test_cmd_dial();
	test_cmd_simple_noargs();
	test_cmd_call_id_optional();
	test_cmd_dtmf();
	test_cmd_mute();
	test_cmd_transfer();
	test_cmd_quality_stats_and_blf();
	test_cmd_unknown_and_malformed();
	test_cmd_id_correlation();
	test_cmd_devices_and_set_device();

	test_dialog_info_idle();
	test_dialog_info_ringing();
	test_dialog_info_busy();
	test_dialog_info_terminated_and_unknown();
	test_dialog_info_real_capture_ext510_idle();
	test_blf_state_name();

	libre_close();

	if (failures) {
		(void)fprintf(stderr, "%d/%d checks FAILED\n",
			      failures, checks);
		return 1;
	}

	(void)printf("ctrl_json_test: %d/%d checks passed\n",
		     checks, checks);
	return 0;
}

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
 * Copyright (C) 2026 Centinelo Phone
 */

#include <errno.h>
#include <re.h>
#include <stdio.h>
#include <string.h>
#include "../cmd.h"
#include "../dialog_info.h"
#include "../pathsafe.h"
#include "../wav_writer.h"


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


/* v1.3: "answer" now optionally decodes call_id (see PROTOCOL.md
 * "answer") - retrocompatible, byte-for-byte unchanged when omitted. */
static void test_cmd_answer_call_id(void)
{
	struct cent_cmd cmd;
	const char *err = NULL;

	CHECK(CENT_CMD_ANSWER == decode("{\"cmd\":\"answer\"}", &cmd, &err),
	      "answer: type (no call_id)");
	CHECK(!cmd.have_call_id,
	      "answer: no call_id -> have_call_id false (v1/v1.1/v1.2"
	      " behavior unchanged)");

	CHECK(CENT_CMD_ANSWER ==
	      decode("{\"cmd\":\"answer\",\"call_id\":\"c42\"}", &cmd, &err),
	      "answer+call_id: type");
	CHECK(cmd.have_call_id, "answer+call_id: have_call_id true");
	CHECK_STREQ(cmd.call_id, "c42", "answer+call_id: call_id value");

	/* quit never decodes call_id, even though it shares the same
	 * process_line() switch case in ctrl_json.c as answer. */
	CHECK(CENT_CMD_QUIT ==
	      decode("{\"cmd\":\"quit\",\"call_id\":\"ignored\"}", &cmd, &err),
	      "quit: type (call_id field, if present, is simply not"
	      " decoded for quit)");
	CHECK(!cmd.have_call_id,
	      "quit: call_id never decoded, regardless of input");
}


/* v1.3: "park" (see PROTOCOL.md "park") - required ext, same shape as
 * blf_subscribe's, optional call_id, same shape as blind_transfer's. */
static void test_cmd_park(void)
{
	struct cent_cmd cmd;
	const char *err = NULL;

	CHECK(CENT_CMD_PARK ==
	      decode("{\"cmd\":\"park\",\"ext\":\"70\"}", &cmd, &err),
	      "park: type");
	CHECK_STREQ(cmd.ext, "70", "park: ext value");
	CHECK(!cmd.have_call_id, "park: no call_id -> have_call_id false");

	CHECK(CENT_CMD_PARK ==
	      decode("{\"cmd\":\"park\",\"ext\":\"70\",\"call_id\":\"c1\"}",
		     &cmd, &err), "park+call_id: type");
	CHECK_STREQ(cmd.ext, "70", "park+call_id: ext value");
	CHECK(cmd.have_call_id, "park+call_id: have_call_id true");
	CHECK_STREQ(cmd.call_id, "c1", "park+call_id: call_id value");

	CHECK(CENT_CMD_NONE == decode("{\"cmd\":\"park\"}", &cmd, &err),
	      "park: missing ext -> NONE");
	CHECK(err != NULL && strstr(err, "ext") != NULL,
	      "park: missing ext -> errmsg mentions 'ext'");

	CHECK(CENT_CMD_NONE ==
	      decode("{\"cmd\":\"park\",\"ext\":\"\"}", &cmd, &err),
	      "park: empty ext -> NONE (require_str treats empty as missing,"
	      " same as blf_subscribe's ext)");
}


/*
 * v1.5: "set_answer_mode" (see PROTOCOL.md "set_answer_mode") - not
 * call-scoped (no call_id, unlike park/mute/etc above), required "mode"
 * restricted to "auto"/"manual", same shape as set_device's "kind"
 * validation (test_cmd_devices_and_set_device() above).
 */
static void test_cmd_set_answer_mode(void)
{
	struct cent_cmd cmd;
	const char *err = NULL;

	CHECK(CENT_CMD_SET_ANSWER_MODE ==
	      decode("{\"cmd\":\"set_answer_mode\",\"mode\":\"auto\"}",
		     &cmd, &err), "set_answer_mode auto: type");
	CHECK(cmd.answer_auto, "set_answer_mode auto: answer_auto true");

	CHECK(CENT_CMD_SET_ANSWER_MODE ==
	      decode("{\"cmd\":\"set_answer_mode\",\"mode\":\"manual\"}",
		     &cmd, &err), "set_answer_mode manual: type");
	CHECK(!cmd.answer_auto, "set_answer_mode manual: answer_auto false");

	/* case-insensitive mode value, matching set_device's 'kind' and
	 * cmd's own case-insensitivity. */
	CHECK(CENT_CMD_SET_ANSWER_MODE ==
	      decode("{\"cmd\":\"set_answer_mode\",\"mode\":\"AUTO\"}",
		     &cmd, &err), "set_answer_mode: mode is case-insensitive");
	CHECK(cmd.answer_auto,
	      "set_answer_mode: 'AUTO' still sets answer_auto true");

	CHECK(CENT_CMD_NONE ==
	      decode("{\"cmd\":\"set_answer_mode\"}", &cmd, &err),
	      "set_answer_mode: missing mode -> CENT_CMD_NONE");
	CHECK(err != NULL && strstr(err, "mode") != NULL,
	      "set_answer_mode: missing mode -> errmsg mentions 'mode'");

	CHECK(CENT_CMD_NONE ==
	      decode("{\"cmd\":\"set_answer_mode\",\"mode\":\"sideways\"}",
		     &cmd, &err),
	      "set_answer_mode: invalid mode -> CENT_CMD_NONE");
	CHECK(err != NULL && strstr(err, "mode") != NULL,
	      "set_answer_mode: invalid mode -> errmsg mentions 'mode'");

	/* no call_id decoded for this command at all - it's a per-account
	 * setting, not call-scoped (see cmd.h's answer_auto comment). */
	CHECK(!cmd.have_call_id,
	      "set_answer_mode: never decodes call_id (not call-scoped)");
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


/*
 * v1.2: "tap_start" (required "dir", optional call_id - like dial's
 * required "uri") and "tap_stop" (call_id only, like hold/resume). See
 * PROTOCOL.md "tap_start"/"tap_stop" and audiotap.h - the actual
 * aufilt/WAV-writer mechanics this decode feeds are out of reach of this
 * standalone test binary (baresip-dependent - see this file's own
 * top-of-file comment), covered by core/E2E-F1.md "F4 audio tap"
 * instead; this is purely the JSON -> struct cent_cmd field extraction.
 */
static void test_cmd_tap(void)
{
	struct cent_cmd cmd;
	const char *err = NULL;

	CHECK(CENT_CMD_TAP_START ==
	      decode("{\"cmd\":\"tap_start\",\"dir\":\"/tmp/calls\"}",
		     &cmd, &err), "tap_start: type");
	CHECK_STREQ(cmd.dir, "/tmp/calls", "tap_start: dir value");
	CHECK(!cmd.have_call_id, "tap_start: no call_id -> have_call_id false");

	CHECK(CENT_CMD_TAP_START ==
	      decode("{\"cmd\":\"tap_start\",\"dir\":\"/tmp/calls\","
		     "\"call_id\":\"c1\"}", &cmd, &err),
	      "tap_start+call_id: type");
	CHECK_STREQ(cmd.dir, "/tmp/calls", "tap_start+call_id: dir value");
	CHECK(cmd.have_call_id, "tap_start+call_id: have_call_id true");
	CHECK_STREQ(cmd.call_id, "c1", "tap_start+call_id: call_id value");

	CHECK(CENT_CMD_NONE == decode("{\"cmd\":\"tap_start\"}", &cmd, &err),
	      "tap_start: missing dir -> NONE");
	CHECK(err != NULL && strstr(err, "dir") != NULL,
	      "tap_start: missing dir -> errmsg mentions 'dir'");

	CHECK(CENT_CMD_NONE ==
	      decode("{\"cmd\":\"tap_start\",\"dir\":\"\"}", &cmd, &err),
	      "tap_start: empty dir -> NONE (require_str treats empty as"
	      " missing, same as dial's uri)");

	CHECK(CENT_CMD_TAP_STOP == decode("{\"cmd\":\"tap_stop\"}", &cmd, &err),
	      "tap_stop: type");
	CHECK(!cmd.have_call_id, "tap_stop: no call_id -> have_call_id false");

	CHECK(CENT_CMD_TAP_STOP ==
	      decode("{\"cmd\":\"tap_stop\",\"call_id\":\"c1\"}", &cmd, &err),
	      "tap_stop+call_id: type");
	CHECK(cmd.have_call_id, "tap_stop+call_id: have_call_id true");
	CHECK_STREQ(cmd.call_id, "c1", "tap_stop+call_id: call_id value");
}


/*
 * v1.3 security fix - pathsafe_component() (see ../pathsafe.h's own top
 * comment for the full story: audiotap.c's tap_start interpolates
 * call_id(call) - the raw SIP Call-ID header for an incoming call,
 * caller-controlled, not engine-generated - directly into a filesystem
 * path; this is the fix, and these are its tests).
 */
static void test_pathsafe_component(void)
{
	char out[64];

	pathsafe_component("abc123", out, sizeof(out));
	CHECK_STREQ(out, "abc123", "pathsafe: plain alnum passes through"
		    " unchanged");

	pathsafe_component("a1b2c3@pbx.example.com", out, sizeof(out));
	CHECK_STREQ(out, "a1b2c3@pbx.example.com",
		    "pathsafe: '@'/'.' (legal Call-ID chars) pass through");

	pathsafe_component("../../etc/passwd", out, sizeof(out));
	CHECK(NULL == strchr(out, '/'),
	      "pathsafe: '/' never survives - no traversal separator left"
	      " in the output, regardless of what else is in it");
	CHECK(NULL == strchr(out, '\\'),
	      "pathsafe: '\\' never survives either (Windows separator)");

	pathsafe_component("..", out, sizeof(out));
	CHECK(0 != str_cmp(out, ".."),
	      "pathsafe: bare '..' input is never returned verbatim"
	      " (leading dots are neutralized)");
	CHECK_STREQ(out, "__", "pathsafe: '..' -> '__' exactly");

	pathsafe_component(".", out, sizeof(out));
	CHECK_STREQ(out, "_", "pathsafe: bare '.' -> '_'");

	pathsafe_component("...hidden", out, sizeof(out));
	CHECK_STREQ(out, "___hidden",
		    "pathsafe: leading dot RUN is neutralized, not just the"
		    " first one");

	pathsafe_component("call.id.with.dots", out, sizeof(out));
	CHECK_STREQ(out, "call.id.with.dots",
		    "pathsafe: non-leading '.' characters are legal and"
		    " pass through (a real Call-ID commonly has these,"
		    " e.g. an '@' domain suffix)");

	pathsafe_component("weird;`$(rm -rf /)`", out, sizeof(out));
	CHECK(NULL == strchr(out, '/') && NULL == strchr(out, ' ') &&
	      NULL == strchr(out, '('),
	      "pathsafe: shell-metacharacter-laden input is fully"
	      " neutralized (not a shell-injection fix per se - this engine"
	      " never shells out with this string - but confirms the"
	      " whitelist is a whitelist, not an ad-hoc blacklist)");

	/* Truncation: output never exceeds out_size (including NUL), and
	 * is always NUL-terminated even when the input is longer than the
	 * buffer. */
	{
		char in[256];
		char small[8];
		size_t i;

		for (i = 0; i < sizeof(in) - 1; i++)
			in[i] = 'a';
		in[sizeof(in) - 1] = '\0';

		pathsafe_component(in, small, sizeof(small));
		CHECK(str_len(small) == sizeof(small) - 1,
		      "pathsafe: output truncated to out_size - 1 chars");
		CHECK(small[sizeof(small) - 1] == '\0',
		      "pathsafe: output always NUL-terminated");
	}

	/* NULL/empty input, and the out_size edge cases - never crashes,
	 * always leaves `out` in a defined, NUL-terminated state. */
	pathsafe_component(NULL, out, sizeof(out));
	CHECK_STREQ(out, "", "pathsafe: NULL input -> empty string");

	pathsafe_component("", out, sizeof(out));
	CHECK_STREQ(out, "", "pathsafe: empty input -> empty string");

	out[0] = 'X';
	pathsafe_component("abc", out, 1);
	CHECK(out[0] == '\0',
	      "pathsafe: out_size == 1 -> just the NUL terminator, no"
	      " overflow");

	/* No crash with a NULL/zero-size `out` - a defensive no-op, same
	 * convention as this codebase's other pure decode functions
	 * (e.g. cent_cmd_decode() with a NULL `out`). */
	pathsafe_component("abc", NULL, 0);
	pathsafe_component("abc", out, 0);
}


struct fake_taken_ctx {
	const char **taken;
	size_t count;
};


static bool fake_is_taken(const char *candidate, void *arg)
{
	const struct fake_taken_ctx *ctx = arg;
	size_t i;

	for (i = 0; i < ctx->count; i++) {
		if (0 == str_cmp(ctx->taken[i], candidate))
			return true;
	}
	return false;
}


/*
 * v1.3 4R finding (R1) regression guard: pathsafe_component() is
 * many-to-one by construction ("abc/def" and "abc_def" both sanitize to
 * "abc_def") - audiotap.c's real collision-avoidance (checking the live
 * tap registry + filesystem) isn't itself pure/unit-testable, but the
 * retry/suffix algorithm it's built on (pathsafe_unique_component()) is
 * - tested here with a fake is_taken predicate.
 */
static void test_pathsafe_unique_component(void)
{
	char out[64];
	bool ok;

	ok = pathsafe_unique_component("abc123", out, sizeof(out), NULL,
					NULL, 10);
	CHECK(ok, "pathsafe_unique: no is_taken -> always succeeds");
	CHECK_STREQ(out, "abc123",
		    "pathsafe_unique: no collision -> filename unchanged"
		    " from plain pathsafe_component() (common case stays"
		    " byte-identical)");

	{
		const char *taken[] = { "abc_def" };
		struct fake_taken_ctx ctx = { taken, 1 };

		/* R1: "abc/def" sanitizes to "abc_def", same as a
		 * DIFFERENT raw call_id "abc_def" would - simulate that
		 * second one already being active. */
		ok = pathsafe_unique_component("abc/def", out, sizeof(out),
						fake_is_taken, &ctx, 10);
		CHECK(ok, "pathsafe_unique: one collision -> still succeeds");
		CHECK(0 != str_cmp(out, "abc_def"),
		      "pathsafe_unique: R1 regression - a taken candidate is"
		      " never returned as-is");
		CHECK_STREQ(out, "abc_def-2",
			    "pathsafe_unique: first retry is '-2'");
	}

	{
		const char *taken[] = { "x", "x-2", "x-3" };
		struct fake_taken_ctx ctx = { taken, 3 };

		ok = pathsafe_unique_component("x", out, sizeof(out),
						fake_is_taken, &ctx, 10);
		CHECK(ok, "pathsafe_unique: 3 chained collisions -> still"
		      " succeeds within max_attempts");
		CHECK_STREQ(out, "x-4",
			    "pathsafe_unique: skips every already-taken"
			    " suffix in order");
	}

	{
		const char *taken[] = { "y", "y-2", "y-3" };
		struct fake_taken_ctx ctx = { taken, 3 };

		ok = pathsafe_unique_component("y", out, sizeof(out),
						fake_is_taken, &ctx, 2);
		CHECK(!ok, "pathsafe_unique: max_attempts exhausted -> false,"
		      " never a silent false-success");
	}

	ok = pathsafe_unique_component(NULL, out, sizeof(out), NULL, NULL, 5);
	CHECK(ok, "pathsafe_unique: NULL input, no is_taken -> succeeds");
	CHECK_STREQ(out, "", "pathsafe_unique: NULL input -> empty string");

	CHECK(!pathsafe_unique_component("abc", NULL, 0, NULL, NULL, 5),
	      "pathsafe_unique: NULL out -> false, no crash");
}


/*
 * v1.3 4R finding (F2, resilience) regression guard: pathsafe_unique_
 * component()'s first fix truncated its sanitized `base` to the FULL
 * out_size-1 width before ever appending a "-N" suffix - for an input
 * long enough to fill the whole output buffer on its own (a SIP Call-ID
 * has no length cap this engine enforces before this point, and the far
 * end controls it), every suffixed retry truncated right back down to
 * the identical bytes, so a genuinely resolvable collision looked
 * unresolvable and a real caller (audiotap_start()) would have wrongly
 * denied recording. Fixed by reserving room for the largest possible
 * suffix in `base` up front - these tests exercise inputs at/over
 * out_size to prove retries now actually change the result.
 */
static void test_pathsafe_unique_component_long_input(void)
{
	char out[128];
	char long_in[200];
	size_t i;
	bool ok;

	for (i = 0; i < sizeof(long_in) - 1; i++)
		long_in[i] = 'a';
	long_in[sizeof(long_in) - 1] = '\0';

	ok = pathsafe_unique_component(long_in, out, sizeof(out), NULL, NULL,
					5);
	CHECK(ok, "pathsafe_unique: long input (> out_size), no collision"
	      " -> succeeds");
	CHECK(str_len(out) < sizeof(out) - 1,
	      "pathsafe_unique: F2 fix - the plain candidate leaves room"
	      " for a suffix (shorter than the full buffer), rather than"
	      " filling it completely the way the pre-fix truncation did");

	/* Force a collision on that exact candidate - the retry MUST
	 * produce something different, not the same truncated bytes again. */
	{
		char first_candidate[128];
		const char *taken1[1];
		struct fake_taken_ctx ctx;

		str_ncpy(first_candidate, out, sizeof(first_candidate));
		taken1[0] = first_candidate;
		ctx.taken = taken1;
		ctx.count = 1;

		ok = pathsafe_unique_component(long_in, out, sizeof(out),
						fake_is_taken, &ctx, 5);
		CHECK(ok, "pathsafe_unique: long input, one collision ->"
		      " still succeeds (pre-fix: exhausted every retry on"
		      " the identical truncated candidate, returned false)");
		CHECK(0 != str_cmp(out, first_candidate),
		      "pathsafe_unique: F2 regression guard - retry produces"
		      " a DIFFERENT candidate even for an input long enough"
		      " to fill the entire output buffer on its own");
	}

	/* Two distinct long call_ids sharing a common prefix long enough to
	 * collide once naively truncated (the real-world shape this bug
	 * actually denies recording for) must still resolve to two
	 * different final paths once a real is_taken predicate reports the
	 * first one active. */
	{
		char long_in_b[200];
		char out_a[128], out_b[128];
		const char *taken1[1];
		struct fake_taken_ctx ctx;

		memcpy(long_in_b, long_in, sizeof(long_in_b));
		long_in_b[sizeof(long_in_b) - 2] = 'b';

		ok = pathsafe_unique_component(long_in, out_a, sizeof(out_a),
						NULL, NULL, 5);
		CHECK(ok, "pathsafe_unique: long input A -> succeeds");

		taken1[0] = out_a;
		ctx.taken = taken1;
		ctx.count = 1;

		ok = pathsafe_unique_component(long_in_b, out_b,
						sizeof(out_b), fake_is_taken,
						&ctx, 5);
		CHECK(ok, "pathsafe_unique: long input B colliding with A's"
		      " candidate -> still succeeds");
		CHECK(0 != str_cmp(out_a, out_b),
		      "pathsafe_unique: two long, distinct call_ids that"
		      " collide after truncation end up with two DIFFERENT"
		      " final paths, not silently the same one (the real"
		      " bug this finding describes: silent denial of"
		      " recording for a legitimate call)");
	}

	/* max_attempts genuinely exhausted (every candidate the retry loop
	 * could ever try, including the suffixed one, reported taken)
	 * still fails cleanly - the fix doesn't turn a real exhaustion into
	 * an infinite loop or a false success. Learn what the base (attempt
	 * 0) and "-2" (attempt 1, the only retry max_attempts=1 allows)
	 * candidates actually are first, rather than guessing their exact
	 * bytes. */
	{
		char base_cand[128], suffixed_cand[128];
		const char *taken1[1];
		struct fake_taken_ctx ctx;

		ok = pathsafe_unique_component(long_in, base_cand,
						sizeof(base_cand), NULL, NULL,
						0);
		CHECK(ok, "pathsafe_unique: setup - learn the base candidate"
		      " (max_attempts=0, no retries possible)");

		taken1[0] = base_cand;
		ctx.taken = taken1;
		ctx.count = 1;

		ok = pathsafe_unique_component(long_in, suffixed_cand,
						sizeof(suffixed_cand),
						fake_is_taken, &ctx, 1);
		CHECK(ok, "pathsafe_unique: setup - learn the '-2' candidate"
		      " (base taken, exactly 1 retry allowed, must succeed)");
		CHECK(0 != str_cmp(base_cand, suffixed_cand),
		      "pathsafe_unique: setup - the two learned candidates"
		      " are themselves distinct (sanity check for this"
		      " sub-test, not a new assertion about the fix)");

		/* Now both are taken, with max_attempts=1 (still only one
		 * retry allowed) - genuinely exhausted, must report false. */
		{
			const char *taken2[2];

			taken2[0] = base_cand;
			taken2[1] = suffixed_cand;
			ctx.taken = taken2;
			ctx.count = 2;

			ok = pathsafe_unique_component(long_in, out,
							sizeof(out),
							fake_is_taken, &ctx,
							1);
			CHECK(!ok, "pathsafe_unique: long input, real"
			      " exhaustion (both reachable candidates taken,"
			      " max_attempts=1) -> false, not a false"
			      " success");
		}
	}
}


/*
 * v1.2: wav_writer.c - see wav_writer.h's own top comment for why this
 * is unit tested (pure C99 stdio, no baresip/re) unlike audiotap.c (the
 * caller that actually feeds it real call audio - covered by
 * core/E2E-F1.md "F4 audio tap" instead). These tests do real file I/O
 * against the current working directory (wherever ctest/the binary runs
 * from - core/BUILD.md's own build-dir convention), cleaning up after
 * themselves with remove().
 */

static uint32_t get_u32le(const uint8_t *p)
{
	return (uint32_t)p[0] | ((uint32_t)p[1] << 8) |
	       ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}


static uint16_t get_u16le(const uint8_t *p)
{
	return (uint16_t)(p[0] | (p[1] << 8));
}


/* Re-opens `path` independently (plain fopen/fread, no wav_writer.h API
 * at all) and checks the canonical 44-byte PCM header field-by-field,
 * plus that the file is exactly 44 + `want_data_bytes` bytes long. This
 * is deliberately an independent reader from wav_writer.c's own
 * build_header() - checking wav_writer's *output* against the WAV
 * spec's actual byte layout, not just against itself. */
static void check_wav_header(const char *path, uint32_t want_srate,
			      uint32_t want_data_bytes, const char *desc)
{
	uint8_t hdr[44];
	FILE *fp;
	long total;
	char msg[192];

	fp = fopen(path, "rb");
	CHECK(fp != NULL, desc);
	if (!fp)
		return;

	CHECK(fread(hdr, 1, sizeof(hdr), fp) == sizeof(hdr),
	      "wav header: could read 44 bytes");

	CHECK(0 == memcmp(hdr + 0, "RIFF", 4), "wav header: 'RIFF' magic");
	CHECK(get_u32le(hdr + 4) == 36 + want_data_bytes,
	      "wav header: RIFF chunk size == 36 + data_bytes");
	CHECK(0 == memcmp(hdr + 8, "WAVE", 4), "wav header: 'WAVE' magic");
	CHECK(0 == memcmp(hdr + 12, "fmt ", 4), "wav header: 'fmt ' magic");
	CHECK(get_u32le(hdr + 16) == 16, "wav header: fmt chunk size == 16");
	CHECK(get_u16le(hdr + 20) == 1, "wav header: audio format == 1 (PCM)");
	CHECK(get_u16le(hdr + 22) == 1, "wav header: channels == 1 (mono)");
	(void)re_snprintf(msg, sizeof(msg), "%s: srate", desc);
	CHECK(get_u32le(hdr + 24) == want_srate, msg);
	CHECK(get_u32le(hdr + 28) == want_srate * 1 * 2,
	      "wav header: byte_rate == srate * channels * bytes/sample");
	CHECK(get_u16le(hdr + 32) == 2,
	      "wav header: block_align == channels * bytes/sample");
	CHECK(get_u16le(hdr + 34) == 16, "wav header: bits_per_sample == 16");
	CHECK(0 == memcmp(hdr + 36, "data", 4), "wav header: 'data' magic");
	CHECK(get_u32le(hdr + 40) == want_data_bytes,
	      "wav header: data chunk size == want_data_bytes");

	if (fseek(fp, 0, SEEK_END) == 0) {
		total = ftell(fp);
		CHECK(total == (long)(44 + want_data_bytes),
		      "wav header: total file size == 44 + data_bytes"
		      " (no extra trailing bytes)");
	}

	(void)fclose(fp);
}


static void test_wav_writer_basic(void)
{
	static const char *path = "wav_writer_test_basic.wav";
	static const int16_t samples[4] = { 100, -100, 32767, -32768 };
	struct wav_writer w;
	uint8_t raw[8];
	FILE *fp;

	CHECK(0 == wav_writer_create(&w, path), "wav_writer: create");
	CHECK(0 == wav_writer_write(&w, 8000, samples, 4),
	      "wav_writer: write 4 samples @ 8000Hz");
	CHECK(wav_writer_bytes(&w) == 8,
	      "wav_writer: bytes() == 8 (4 samples * 2 bytes) after write,"
	      " before close");
	CHECK(0 == wav_writer_close(&w, 8000), "wav_writer: close");
	CHECK(wav_writer_bytes(&w) == 8,
	      "wav_writer: bytes() still 8 after close");

	check_wav_header(path, 8000, 8, "wav_writer basic: header");

	/* Sample data itself, byte-exact - independent of build_header(),
	 * this is checking wav_writer_write()'s own raw fwrite() path. */
	fp = fopen(path, "rb");
	CHECK(fp != NULL, "wav_writer basic: re-open for sample data check");
	if (fp) {
		CHECK(fseek(fp, 44, SEEK_SET) == 0,
		      "wav_writer basic: seek past header");
		CHECK(fread(raw, 1, sizeof(raw), fp) == sizeof(raw),
		      "wav_writer basic: read 8 data bytes");
		CHECK(get_u16le(raw + 0) == (uint16_t)100,
		      "wav_writer basic: sample 0 == 100");
		CHECK(get_u16le(raw + 2) == (uint16_t)-100,
		      "wav_writer basic: sample 1 == -100 (two's complement)");
		CHECK(get_u16le(raw + 4) == (uint16_t)32767,
		      "wav_writer basic: sample 2 == 32767");
		CHECK(get_u16le(raw + 6) == (uint16_t)-32768,
		      "wav_writer basic: sample 3 == -32768");
		(void)fclose(fp);
	}

	/* Idempotence: a second close() must not corrupt the already-
	 * finalized file (see wav_writer.h's own "idempotent" contract). */
	CHECK(0 == wav_writer_close(&w, 8000),
	      "wav_writer: second close() is a safe no-op (returns 0)");
	check_wav_header(path, 8000, 8,
			 "wav_writer basic: header unchanged after 2nd close");

	(void)remove(path);
}


static void test_wav_writer_multi_write(void)
{
	static const char *path = "wav_writer_test_multiwrite.wav";
	static const int16_t chunk_a[2] = { 1, 2 };
	static const int16_t chunk_b[3] = { 3, 4, 5 };
	struct wav_writer w;

	CHECK(0 == wav_writer_create(&w, path), "wav_writer multi: create");
	CHECK(0 == wav_writer_write(&w, 16000, chunk_a, 2),
	      "wav_writer multi: first write (commits header @ 16000Hz)");
	CHECK(0 == wav_writer_write(&w, 16000, chunk_b, 3),
	      "wav_writer multi: second write (header already committed -"
	      " srate arg ignored)");
	CHECK(wav_writer_bytes(&w) == 10,
	      "wav_writer multi: bytes() == 10 ((2+3) samples * 2 bytes)");
	CHECK(0 == wav_writer_close(&w, 16000), "wav_writer multi: close");

	check_wav_header(path, 16000, 10, "wav_writer multi: header");

	(void)remove(path);
}


/* The "zero frames ever written" edge case (F4 task design: "never leave
 * a corrupt WAV") - create() then close() with no write() in between at
 * all must still leave a syntactically valid, silent WAV using the
 * fallback srate. */
static void test_wav_writer_never_written(void)
{
	static const char *path = "wav_writer_test_neverwritten.wav";
	struct wav_writer w;

	CHECK(0 == wav_writer_create(&w, path),
	      "wav_writer never-written: create");
	CHECK(wav_writer_bytes(&w) == 0,
	      "wav_writer never-written: bytes() == 0 before close");
	CHECK(0 == wav_writer_close(&w, 8000),
	      "wav_writer never-written: close (commits fallback header)");
	CHECK(wav_writer_bytes(&w) == 0,
	      "wav_writer never-written: bytes() still 0 after close");

	check_wav_header(path, 8000, 0,
			 "wav_writer never-written: fallback header, 0 data"
			 " bytes, still a syntactically valid WAV");

	(void)remove(path);
}


/* wav_writer_close()/wav_writer_bytes() on a writer that was never even
 * create()'d (an all-zero struct, matching how a fresh struct
 * audiotap_reg's rx_w/tx_w start out - see audiotap.c) must be safe
 * no-ops, not a crash - belt-and-suspenders alongside
 * test_wav_writer_basic()'s "close an already-closed writer" case. */
static void test_wav_writer_uninitialized_close(void)
{
	struct wav_writer w;

	memset(&w, 0, sizeof(w));

	CHECK(0 == wav_writer_close(&w, 8000),
	      "wav_writer: close() on a never-create()'d (all-zero) writer"
	      " is a safe no-op");
	CHECK(wav_writer_bytes(&w) == 0,
	      "wav_writer: bytes() on a never-create()'d writer == 0");
}


/* wav_writer_create() itself must fail cleanly (not crash) for the
 * obviously-invalid inputs cmd.c's own require_str() already screens
 * "dir" for at the JSON layer (see test_cmd_tap()) - this is the
 * defense-in-depth layer under that one, exercised directly. */
static void test_wav_writer_create_errors(void)
{
	struct wav_writer w;

	CHECK(EINVAL == wav_writer_create(&w, NULL),
	      "wav_writer: create(NULL path) -> EINVAL");
	CHECK(EINVAL == wav_writer_create(&w, ""),
	      "wav_writer: create(\"\") -> EINVAL");
	CHECK(EINVAL == wav_writer_create(NULL, "x.wav"),
	      "wav_writer: create(NULL writer) -> EINVAL");

	/* A directory that doesn't exist: fopen()'s own ENOENT, propagated
	 * verbatim - confirms wav_writer_create() doesn't swallow/remap the
	 * real errno (matters for audiotap.c's own "bad 'dir'?" error
	 * message being accurate). */
	CHECK(0 != wav_writer_create(&w, "/no/such/directory/x.wav"),
	      "wav_writer: create() under a nonexistent directory fails"
	      " (nonzero, not necessarily ENOENT on every OS/libc)");
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
		" entity=\"sip:510@192.0.2.10\">\r\n"
		" <dialog id=\"510\">\r\n"
		"  <state>terminated</state>\r\n"
		" </dialog>\r\n"
		"</dialog-info>\r\n";

	CHECK(CENT_BLF_IDLE ==
	      dialog_info_parse(real_body, str_len(real_body)),
	      "dialog_info: real ext-510 capture (state=full,"
	      " dialog state=terminated) -> idle");
}


/*
 * v1.3 presence_override - real capture, mid-hold (see core/E2E-F1.md
 * "F5 presence_override" and dialog_info.h's own header comment on
 * CENT_BLF_HELD for the full story). Captured via SIP trace (-s) while
 * ext 1000 (this engine's own test account, dual-contact trick) had a
 * live bridged call on local hold (this engine's own `hold` command) -
 * this exact body (only `version=` differs across the 3 NOTIFYs actually
 * captured spanning the hold window - all byte-identical otherwise) is
 * what a real FreePBX 17.0.30 / Asterisk 22.8.2 chan_pjsip hint sends:
 * plain `<state>confirmed</state>`, no rendering param, no
 * <local>/<remote>/<target> at all - indistinguishable from a plain busy
 * call at this layer. This is a *regression guard*, not a bug report:
 * proves this parser correctly reads what the real PBX actually sends
 * (busy, not a false "held") rather than what RFC 4235 merely allows a
 * compliant implementation to send.
 */
static void test_dialog_info_real_capture_ext1000_confirmed_no_hold_signal(void)
{
	static const char real_body[] =
		"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n"
		"<dialog-info xmlns=\"urn:ietf:params:xml:ns:dialog-info\""
		" version=\"2\" state=\"full\""
		" entity=\"sip:1000@192.0.2.10\">\r\n"
		" <dialog id=\"1000\">\r\n"
		"  <state>confirmed</state>\r\n"
		" </dialog>\r\n"
		"</dialog-info>\r\n";

	CHECK(CENT_BLF_BUSY ==
	      dialog_info_parse(real_body, str_len(real_body)),
	      "dialog_info: real ext-1000 capture, mid-hold (state=full,"
	      " dialog state=confirmed, NO rendering param) -> busy, NOT"
	      " held - this real PBX doesn't signal hold via dialog-info"
	      " (see dialog_info.h's CENT_BLF_HELD comment)");
}


/*
 * v1.3 presence_override - HELD: RFC 4235/RFC 3840 "+sip.rendering"
 * pvalue="no" target param on a confirmed dialog (see dialog_info.h's
 * header comment for the full rationale). Synthetic fixtures, built
 * strictly to the RFC 4235/3840 documented shape - see
 * core/E2E-F1.md "F5 presence_override" for whether/how this was also
 * confirmed against a real captured NOTIFY body from this repo's test
 * PBX (update this comment - and swap in the real body as an additional
 * fixture, matching test_dialog_info_real_capture_ext510_idle()'s own
 * precedent - the moment that capture exists).
 */
static void test_dialog_info_held(void)
{
	static const char confirmed_local_held[] =
		"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n"
		"<dialog-info xmlns=\"urn:ietf:params:xml:ns:dialog-info\""
		" version=\"1\" state=\"partial\""
		" entity=\"sip:ext@pbx.example.com\">\r\n"
		" <dialog id=\"1\">\r\n"
		"  <state>confirmed</state>\r\n"
		"  <local>\r\n"
		"   <target uri=\"sip:ext@pbx.example.com\">\r\n"
		"    <param pname=\"+sip.rendering\" pvalue=\"no\"/>\r\n"
		"   </target>\r\n"
		"  </local>\r\n"
		" </dialog>\r\n"
		"</dialog-info>\r\n";
	static const char remote_target[] =
		"<dialog-info state=\"full\">"
		"<dialog id=\"abc\"><state>confirmed</state>"
		"<remote><target uri=\"sip:peer@host\">"
		"<param pname=\"+sip.rendering\" pvalue=\"no\"/>"
		"</target></remote>"
		"</dialog></dialog-info>";
	static const char rendering_yes[] =
		"<dialog-info state=\"full\">"
		"<dialog id=\"abc\"><state>confirmed</state>"
		"<local><target uri=\"sip:x@host\">"
		"<param pname=\"+sip.rendering\" pvalue=\"yes\"/>"
		"</target></local>"
		"</dialog></dialog-info>";

	CHECK(CENT_BLF_HELD ==
	      dialog_info_parse(confirmed_local_held,
				 str_len(confirmed_local_held)),
	      "dialog_info: confirmed + local target rendering=no -> held");
	CHECK(CENT_BLF_HELD ==
	      dialog_info_parse(remote_target, str_len(remote_target)),
	      "dialog_info: confirmed + remote target rendering=no -> held");
	CHECK(CENT_BLF_BUSY ==
	      dialog_info_parse(rendering_yes, str_len(rendering_yes)),
	      "dialog_info: confirmed + rendering=yes (not held) -> busy,"
	      " not held");
}


/*
 * v1.3 presence_override - DND: best-effort, non-standard hook, NOT
 * confirmed against a real Asterisk capture (see dialog_info.h). Scope
 * is deliberately narrow (v1.3 4R finding R1... R4 - see dialog_info.c's
 * own comment): only overrides what would otherwise be "idle" (no
 * <dialog> element at all) - never a genuinely active dialog, including
 * one that's merely "terminated" (still a real <dialog> element, handled
 * by the normal <state> parsing path, unaffected by dnd).
 */
static void test_dialog_info_dnd(void)
{
	static const char dnd_element[] =
		"<dialog-info state=\"full\" entity=\"sip:510@host\">"
		"<dnd>true</dnd>"
		"</dialog-info>";
	static const char dnd_attr[] =
		"<dialog-info state=\"full\" entity=\"sip:510@host\""
		" dnd=\"true\">"
		"</dialog-info>";
	static const char dnd_with_terminated_dialog[] =
		"<dialog-info state=\"full\">"
		"<dnd>true</dnd>"
		"<dialog id=\"abc\"><state>terminated</state></dialog>"
		"</dialog-info>";
	static const char dnd_with_confirmed_dialog[] =
		"<dialog-info state=\"full\">"
		"<dnd>true</dnd>"
		"<dialog id=\"abc\"><state>confirmed</state></dialog>"
		"</dialog-info>";
	static const char dnd_with_early_dialog[] =
		"<dialog-info state=\"full\">"
		"<dnd>true</dnd>"
		"<dialog id=\"abc\"><state>early</state></dialog>"
		"</dialog-info>";

	CHECK(CENT_BLF_DND ==
	      dialog_info_parse(dnd_element, str_len(dnd_element)),
	      "dialog_info: <dnd>true</dnd> element, no <dialog> -> dnd,"
	      " overrides the idle fallback");
	CHECK(CENT_BLF_DND ==
	      dialog_info_parse(dnd_attr, str_len(dnd_attr)),
	      "dialog_info: dnd=\"true\" attribute -> dnd");

	/* R4 regression guard: a dnd marker must NEVER override a real
	 * active dialog state - only the "no <dialog> at all" case. */
	CHECK(CENT_BLF_IDLE ==
	      dialog_info_parse(dnd_with_terminated_dialog,
				 str_len(dnd_with_terminated_dialog)),
	      "dialog_info: dnd marker + a real (terminated) <dialog>"
	      " element -> idle, NOT dnd (a terminated dialog is a real"
	      " <dialog>, handled by the normal <state> path)");
	CHECK(CENT_BLF_BUSY ==
	      dialog_info_parse(dnd_with_confirmed_dialog,
				 str_len(dnd_with_confirmed_dialog)),
	      "dialog_info: dnd marker + <state>confirmed</state> -> busy,"
	      " NOT dnd - dnd never overrides a genuinely active dialog");
	CHECK(CENT_BLF_RINGING ==
	      dialog_info_parse(dnd_with_early_dialog,
				 str_len(dnd_with_early_dialog)),
	      "dialog_info: dnd marker + <state>early</state> -> ringing,"
	      " NOT dnd - same guarantee for the ringing case");
}


static void test_blf_state_name(void)
{
	CHECK_STREQ(cent_blf_state_name(CENT_BLF_IDLE), "idle", "name: idle");
	CHECK_STREQ(cent_blf_state_name(CENT_BLF_RINGING), "ringing",
		    "name: ringing");
	CHECK_STREQ(cent_blf_state_name(CENT_BLF_BUSY), "busy", "name: busy");
	CHECK_STREQ(cent_blf_state_name(CENT_BLF_HELD), "held", "name: held");
	CHECK_STREQ(cent_blf_state_name(CENT_BLF_DND), "dnd", "name: dnd");
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
	test_cmd_answer_call_id();
	test_cmd_park();
	test_cmd_set_answer_mode();
	test_cmd_dtmf();
	test_cmd_mute();
	test_cmd_transfer();
	test_cmd_quality_stats_and_blf();
	test_cmd_unknown_and_malformed();
	test_cmd_id_correlation();
	test_cmd_devices_and_set_device();
	test_cmd_tap();

	test_pathsafe_component();
	test_pathsafe_unique_component();
	test_pathsafe_unique_component_long_input();

	test_wav_writer_basic();
	test_wav_writer_multi_write();
	test_wav_writer_never_written();
	test_wav_writer_uninitialized_close();
	test_wav_writer_create_errors();

	test_dialog_info_idle();
	test_dialog_info_ringing();
	test_dialog_info_busy();
	test_dialog_info_terminated_and_unknown();
	test_dialog_info_real_capture_ext510_idle();
	test_dialog_info_held();
	test_dialog_info_real_capture_ext1000_confirmed_no_hold_signal();
	test_dialog_info_dnd();
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

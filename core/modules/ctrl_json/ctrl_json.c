/**
 * @file ctrl_json.c  Centinelo Phone v2 - JSON control protocol over stdio
 *
 * Reads newline-delimited JSON commands from stdin and writes
 * newline-delimited JSON events to stdout, so a parent process (a Tauri
 * shell, a test harness, ...) can drive baresip as a sidecar over a plain
 * pipe.
 *
 * Adapted from two stock baresip modules:
 *   - modules/ctrl_tcp: JSON-over-TCP/netstring control channel and its
 *     bevent -> JSON event relay (here: bevent -> our own compact
 *     schema instead of the generic ctrl_tcp wire format).
 *   - modules/stdio: polls STDIN_FILENO via fd_listen() instead of
 *     opening a socket (ctrl_tcp's approach) or a raw tty (stdio's
 *     approach - we deliberately stay in normal buffered/line mode, no
 *     tcsetattr, since our peer is a pipe, not a human at a keyboard).
 *     Windows has no fd_listen()-compatible way to poll a console/pipe
 *     stdin handle, so _WIN32 uses a dedicated reader thread + re's
 *     thread-safe mqueue instead - see "stdin - Windows" below.
 *
 * Call-control commands (hold/resume/dtmf/mute/transfer/quality_stats)
 * are driven directly against baresip's public call_*, ua_*, uag_* API
 * (include/baresip.h) rather than through cmd_process_long()'s
 * long-command text dispatch that v0's dial/answer/hangup/quit used and
 * still use for dial/answer: those two commands have no "which call"
 * ambiguity to begin with (dial always creates a new call; answer always
 * targets the one incoming/ringing call, which the menu module already
 * tracks correctly via bevents - reimplementing that tracking here would
 * gain nothing). Every other call-scoped command, however, needs a
 * single, consistent notion of "the current call" when a JSON command
 * omits "call_id" - see resolve_call() - and menu's own long commands
 * resolve "current" through menu-internal state (menu_uacur()/
 * menu.curcall, private to modules/menu/, not reachable from an
 * out-of-tree module like this one) which is a *different* mechanism
 * than the public uag_call_find()/ua_call() used here. Mixing the two
 * per-command would let "hold" (no call_id) and "hangup" (no call_id)
 * silently disagree about which call is "current" in a 2-call scenario
 * (exactly the attended-transfer shape this file implements) - so V1
 * moves hangup onto the same direct-API + resolve_call() path as
 * everything else, for one consistent definition of "current call"
 * across the whole protocol. See PROTOCOL.md's changelog for this
 * decision spelled out for protocol consumers.
 *
 * v1.1 adds (see PROTOCOL.md's own changelog for the full list):
 * per-command request/response correlation via an optional "id" field
 * (see cmd.have_id, emit_result(), and process_line()'s
 * g_error_seq-based ok/fail tracking - no existing handler's signature
 * changed to support this); a "devices"/"set_device" pair for audio
 * device enumeration/selection; codec/transport enrichment on
 * quality_stats' "stats" event; and stdout is now *pure* NDJSON end to
 * end - the baresip banner, module-load log lines, and (with -s) SIP
 * trace, all previously leaking onto stdout ahead of/around this file's
 * own JSON (see the old "Framing" section this replaced), now land on
 * stderr instead, via a small baresip patch (core/patches/0003-*, see
 * BUILD.md) rather than anything in this file - ctrl_json.c's own
 * log_enable_stdout(false) call in ctrl_init() below still runs, but is
 * now typically a no-op by the time it's reached (see that call site's
 * own comment).
 *
 * See core/PROTOCOL.md for the wire protocol (v1.1).
 *
 * Copyright (C) 2026 Neola Dental / Centinelo Phone
 */

#include <re.h>
#include <baresip.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include <errno.h>
#include "cmd.h"
#include "dialog_info.h"
#include "audiotap.h"

#ifndef _WIN32
#include <unistd.h>
#endif


/**
 * @defgroup ctrl_json ctrl_json
 *
 * Newline-delimited JSON control channel on stdin/stdout.
 *
 * Only one instance is supported (single baresip process per Centinelo
 * Phone session), matching modules/ctrl_tcp's "one instance" rule - all
 * state below (the stdin reader, the BLF subscription list, the pending
 * attended-transfer pair) is process-wide static state on that same
 * assumption, not stashed inside struct ctrl_st per-instance.
 */


enum {
	INBUF_SIZE      = 8192,
	BLF_EXPIRES     = 3600,   /* seconds; sipevent_subscribe() refreshes
				   * this automatically before it lapses -
				   * see core/deps/re/src/sipevent/subscribe.c
				   * tmr_handler - no manual re-SUBSCRIBE loop
				   * needed here. */
};

struct ctrl_st {
#ifdef _WIN32
	thrd_t stdin_thr;
	struct mqueue *mq;
#else
	struct re_fhs *fhs;
	uint8_t inbuf[INBUF_SIZE];
	size_t  inlen;
#endif
};

static struct ctrl_st *ctrl = NULL;   /* allow only one instance */

/* Attended-transfer state: F1 supports exactly one pending attended
 * transfer at a time, matching baresip's own
 * modules/menu/dynamic_menu.c single-slot xfer_call/xfer_targ design
 * (private to that module, not reachable here - this is our own
 * equivalent). xfer_source is the original, held call (REFER is sent on
 * it); xfer_target is the consultation call we dial out in
 * attended_transfer. Both are borrowed pointers into baresip's own
 * ua->calls list (see resolve_call()) - never mem_deref'd from this
 * file, only forgotten (xfer_reset()) when the transfer completes,
 * aborts, or either call closes first (see event_handler's
 * BEVENT_CALL_CLOSED case). */
static struct call *xfer_source;
static struct call *xfer_target;

/* One SIP SUBSCRIBE;Event:dialog per watched extension - see
 * blf_subscribe()/blf_unsubscribe() below. */
struct blf_sub {
	struct le le;
	char ext[CENT_EXT_SIZE];
	struct sipsub *sub;
};
static struct list blf_subs;


static int print_handler(const char *p, size_t size, void *arg)
{
	struct mbuf *mb = arg;

	return mbuf_write_mem(mb, (const uint8_t *)p, size);
}


/*
 * Serialize an odict as one compact JSON line and write it to stdout.
 *
 * Uses buffered stdio (fwrite + an explicit fflush) rather than a raw
 * write(STDOUT_FILENO, ...) - the previous v0 implementation - purely so
 * this function has no POSIX-only dependency at all (no <unistd.h>,
 * matching the portability goal driving this file's other _WIN32
 * changes), not for any behavioural reason: the explicit fflush()
 * preserves v0's actual behaviour (each JSON line is flushed to the pipe
 * immediately, not left sitting in a libc stdio buffer) - dropping it
 * would be a real regression for a sidecar protocol whose parent process
 * is waiting on each event as it happens.
 */
static void emit(struct odict *od)
{
	struct mbuf *mb = mbuf_alloc(512);
	struct re_printf pf = {print_handler, mb};
	int err;

	if (!mb)
		return;

	err = json_encode_odict(&pf, od);
	if (err) {
		warning("ctrl_json: failed to encode JSON (%m)\n", err);
		goto out;
	}

	(void)fwrite(mb->buf, 1, mb->end, stdout);
	(void)fwrite("\n", 1, 1, stdout);
	(void)fflush(stdout);

 out:
	mem_deref(mb);
}


static void emit_ready(void)
{
	struct odict *od = NULL;

	if (odict_alloc(&od, 4))
		return;

	(void)odict_entry_add(od, "event", ODICT_STRING, "ready");

	emit(od);
	mem_deref(od);
}


/*
 * v1.1 request/response correlation (see PROTOCOL.md "result"): every
 * emit_error()/emit_errorf() call - i.e. every place in this file that
 * already, under v1, signals "this command failed" - also records its
 * message here and bumps g_error_seq. process_line() snapshots
 * g_error_seq immediately before dispatching a command and compares it
 * again immediately after: if it moved, some emit_error() fired *during
 * that command's own synchronous dispatch*, so the command failed - see
 * process_line() for why this is a safe, race-free way to learn a
 * handler's outcome without changing any handler's own signature/return
 * type (every cmd_* / do_* function below is completely unmodified by
 * v1.1; this is the only new plumbing request/response correlation
 * needed). g_last_error is then the exact same text an "error" event
 * would show (or already did, if have_id is also false) - single
 * source of truth, no duplicated message-building.
 */
static char g_last_error[256];
static uint32_t g_error_seq;


static void emit_error(const char *message)
{
	struct odict *od = NULL;

	str_ncpy(g_last_error, message ? message : "", sizeof(g_last_error));
	++g_error_seq;

	if (odict_alloc(&od, 4))
		return;

	(void)odict_entry_add(od, "event", ODICT_STRING, "error");
	(void)odict_entry_add(od, "message", ODICT_STRING,
			       message ? message : "");

	emit(od);
	mem_deref(od);
}


/* printf-style convenience wrapper around emit_error() - most call
 * sites below need to fold a baresip %m errno or a dynamic detail
 * (call_id, ext, ...) into the message. */
static void emit_errorf(const char *fmt, ...)
{
	char buf[256];
	va_list ap;

	va_start(ap, fmt);
	(void)re_vsnprintf(buf, sizeof(buf), fmt, ap);
	va_end(ap);

	emit_error(buf);
}


static const char *transp_name(enum sip_transp tp)
{
	switch (tp) {

	case SIP_TRANSP_UDP:  return "udp";
	case SIP_TRANSP_TCP:  return "tcp";
	case SIP_TRANSP_TLS:  return "tls";
	case SIP_TRANSP_WS:   return "ws";
	case SIP_TRANSP_WSS:  return "wss";
	case SIP_TRANSP_NONE: return "none";
	default:              return "udp";
	}
}


/*
 * Best-effort: read back the ";transport=" URI param of the account's
 * registrar URI, so reg_state events can report which of the dual
 * transports (WSS vs classic UDP/TCP/TLS) actually registered. This is
 * how we tell the two halves of the F1 e2e test (6a vs 6b) apart in the
 * emitted JSON without the caller having to already know the answer.
 */
static const char *account_transp(struct account *acc)
{
	static const struct pl pl_transport = PL("transport");
	struct uri *uri;
	struct pl val;

	if (!acc)
		return "udp";

	uri = account_luri(acc);
	if (!uri)
		return "udp";

	if (uri_param_get(&uri->params, &pl_transport, &val))
		pl_set_str(&val, "udp");

	return transp_name(sip_transp_decode(&val));
}


static void emit_reg_state(struct ua *ua, const char *state,
			    const char *reason)
{
	struct odict *od = NULL;
	struct account *acc = ua ? ua_account(ua) : NULL;

	if (odict_alloc(&od, 8))
		return;

	(void)odict_entry_add(od, "event", ODICT_STRING, "reg_state");
	(void)odict_entry_add(od, "account", ODICT_STRING,
			       acc ? account_aor(acc) : "");
	(void)odict_entry_add(od, "state", ODICT_STRING, state);
	(void)odict_entry_add(od, "transport", ODICT_STRING,
			       account_transp(acc));

	/* v0 addition (see PROTOCOL.md): SIP failure reason, when known -
	 * useful evidence for "why did this transport not register". */
	if (reason && str_isset(reason))
		(void)odict_entry_add(od, "reason", ODICT_STRING, reason);

	emit(od);
	mem_deref(od);
}


/*
 * call_state carries both "id" (v0's original field name) and the new
 * "call_id" (matching every new command's own "call_id" field name, and
 * the letter of the F1 spec: "events always carry call_id") - kept as
 * two keys with the same value, rather than renamed outright, so a v0
 * consumer reading "id" does not silently break under v1. See
 * PROTOCOL.md's changelog.
 *
 * "state" values beyond v0's incoming/ringing/established/closed:
 * "hold"/"resumed" (fired both for our own local hold/resume commands,
 * synthetically - see cmd_hold()/cmd_resume() - and for a *peer*-
 * initiated hold/resume, relayed from BEVENT_CALL_HOLD/_RESUME - see
 * event_handler()) and "muted"/"unmuted" (fired from cmd_mute()). None
 * of these change call_state()'s own CALL_STATE_* machine (hold/mute are
 * orthogonal to the incoming/ringing/established/closed lifecycle), they
 * are simply everything this protocol currently considers "observable
 * about a call" funnelled through one event type rather than inventing a
 * new event per attribute.
 */
static void emit_call_state(struct call *call, const char *state)
{
	struct odict *od = NULL;
	const char *id = call ? call_id(call) : "";

	if (odict_alloc(&od, 8))
		return;

	(void)odict_entry_add(od, "event", ODICT_STRING, "call_state");
	(void)odict_entry_add(od, "state", ODICT_STRING, state);
	(void)odict_entry_add(od, "peer", ODICT_STRING,
			       call ? call_peeruri(call) : "");
	(void)odict_entry_add(od, "id", ODICT_STRING, id);
	(void)odict_entry_add(od, "call_id", ODICT_STRING, id);

	emit(od);
	mem_deref(od);
}


/*
 * v1.1 enrichment (see PROTOCOL.md "stats"): adds "codec" (the TX/
 * encoder side - see audio_codec()'s `tx` param; for every flow this
 * repo exercises the rx side negotiates the same codec, SDP offer/
 * answer being symmetric, so one field covers "the" codec in practice)
 * and "transport" (call_transp() - the *call's own* actual SIP
 * transport, the same per-call accessor baresip itself uses
 * internally, not a guess derived from the account - reuses
 * transp_name(), already used by reg_state, for the same udp/tcp/tls/
 * ws/wss vocabulary). Both are omitted entirely (not emitted as an
 * empty string) when not yet known - e.g. "codec" before SDP
 * negotiation completes - so a consumer can tell "not known yet" apart
 * from a real value, matching this file's existing convention for
 * optional fields (see emit_reg_state()'s "reason").
 *
 * rtt_us (rs->rtt, unchanged v1 field/name and unchanged RTCP source)
 * is frequently 0 in practice against a real PBX - see
 * core/E2E-F1.md scenario (d): RTCP round-trip-time needs a full SR/
 * RR/DLSR round trip to populate, which this engine's test PBX
 * empirically never completed in any capture in that document, even
 * though tx/rx packet/loss/jitter (also RTCP-sourced, same
 * struct rtcp_stats) were consistently non-zero and independently
 * PBX-confirmed in the same window. A 0 rtt_us is not, by itself,
 * evidence this field is broken - see also the RTCP-cadence caveat
 * below (also unchanged from v1: querying faster than the PBX's own
 * RTCP interval - ~10-20s, measured empirically - returns identical
 * numbers, not fresh ones, for every field this function emits).
 */
static void fill_stats_fields(struct odict *od, struct call *call,
			       const struct rtcp_stats *rs)
{
	const struct aucodec *ac = audio_codec(call_audio(call), true);

	(void)odict_entry_add(od, "rtt_us", ODICT_INT, (int64_t)rs->rtt);
	(void)odict_entry_add(od, "tx_packets", ODICT_INT,
			       (int64_t)rs->tx.sent);
	(void)odict_entry_add(od, "tx_lost", ODICT_INT, (int64_t)rs->tx.lost);
	(void)odict_entry_add(od, "tx_jitter_us", ODICT_INT,
			       (int64_t)rs->tx.jit);
	(void)odict_entry_add(od, "rx_packets", ODICT_INT,
			       (int64_t)rs->rx.sent);
	(void)odict_entry_add(od, "rx_lost", ODICT_INT, (int64_t)rs->rx.lost);
	(void)odict_entry_add(od, "rx_jitter_us", ODICT_INT,
			       (int64_t)rs->rx.jit);

	if (ac)
		(void)odict_entry_add(od, "codec", ODICT_STRING, ac->name);

	(void)odict_entry_add(od, "transport", ODICT_STRING,
			       transp_name(call_transp(call)));
}


static void emit_stats(struct call *call)
{
	const struct rtcp_stats *rs;
	struct odict *od = NULL;

	if (!call) {
		emit_error("quality_stats: call not found");
		return;
	}

	rs = stream_rtcp_stats(audio_strm(call_audio(call)));
	if (!rs) {
		emit_error("quality_stats: call has no audio stream");
		return;
	}

	if (odict_alloc(&od, 16))
		return;

	(void)odict_entry_add(od, "event", ODICT_STRING, "stats");
	(void)odict_entry_add(od, "call_id", ODICT_STRING, call_id(call));
	fill_stats_fields(od, call, rs);

	emit(od);
	mem_deref(od);
}


static void emit_blf(const char *ext, enum cent_blf_state state)
{
	struct odict *od = NULL;

	if (odict_alloc(&od, 8))
		return;

	(void)odict_entry_add(od, "event", ODICT_STRING, "blf");
	(void)odict_entry_add(od, "ext", ODICT_STRING, ext);
	(void)odict_entry_add(od, "state", ODICT_STRING,
			       cent_blf_state_name(state));

	emit(od);
	mem_deref(od);
}


/*
 * v1.2 (see PROTOCOL.md "tap_start"/"tap_stop"): reports an audio tap's
 * lifecycle. `state` is "started" (from tap_start - rx_path/tx_path
 * only, byte/duration fields omitted, nothing's been written yet) or
 * "stopped" (from tap_stop *or* BEVENT_CALL_CLOSED auto-finalizing a
 * tap that outlived its tap_stop - see event_handler() - carries the
 * final byte/duration counts too). call_id is always the resolved
 * call's real id, regardless of whether the triggering command supplied
 * one - same convention as emit_call_state().
 */
static void emit_tap_state(struct call *call, const char *state,
			    const struct audiotap_result *res)
{
	struct odict *od = NULL;
	const char *id = call ? call_id(call) : "";

	if (odict_alloc(&od, 16))
		return;

	(void)odict_entry_add(od, "event", ODICT_STRING, "tap_state");
	(void)odict_entry_add(od, "call_id", ODICT_STRING, id);
	(void)odict_entry_add(od, "state", ODICT_STRING, state);
	(void)odict_entry_add(od, "rx_path", ODICT_STRING, res->rx_path);
	(void)odict_entry_add(od, "tx_path", ODICT_STRING, res->tx_path);

	if (!str_casecmp(state, "stopped")) {
		(void)odict_entry_add(od, "rx_bytes", ODICT_INT,
				       (int64_t)res->rx_bytes);
		(void)odict_entry_add(od, "tx_bytes", ODICT_INT,
				       (int64_t)res->tx_bytes);
		(void)odict_entry_add(od, "rx_duration_ms", ODICT_INT,
				       (int64_t)res->rx_duration_ms);
		(void)odict_entry_add(od, "tx_duration_ms", ODICT_INT,
				       (int64_t)res->tx_duration_ms);
	}

	emit(od);
	mem_deref(od);
}


static void emit_attended_transfer_started(struct call *source,
					    struct call *target)
{
	struct odict *od = NULL;

	if (odict_alloc(&od, 8))
		return;

	(void)odict_entry_add(od, "event", ODICT_STRING,
			       "attended_transfer_started");
	(void)odict_entry_add(od, "source_call_id", ODICT_STRING,
			       call_id(source));
	(void)odict_entry_add(od, "target_call_id", ODICT_STRING,
			       call_id(target));

	emit(od);
	mem_deref(od);
}


/*
 * v1.3 (see PROTOCOL.md "park"): confirms a park request's own
 * *synchronous* dispatch (the REFER was accepted and sent) - same
 * "ok:true is not a promise about the async outcome" caveat as
 * blind_transfer's own call_state "closed"/error story (see
 * cmd_blind_transfer()'s comment): the far end's own eventual REFER
 * NOTIFY outcome, and which specific parking-lot slot Asterisk's
 * Park() app actually auto-assigns, are not observable over plain SIP
 * signaling this engine's call leg is party to - see PROTOCOL.md
 * "park" for the full explanation of why `ext` here is the pilot
 * extension the park request targeted, not a specific auto-assigned
 * slot number.
 */
static void emit_park(struct call *call, const char *ext)
{
	struct odict *od = NULL;
	const char *id = call ? call_id(call) : "";

	if (odict_alloc(&od, 8))
		return;

	(void)odict_entry_add(od, "event", ODICT_STRING, "park");
	(void)odict_entry_add(od, "call_id", ODICT_STRING, id);
	(void)odict_entry_add(od, "ext", ODICT_STRING, ext);

	emit(od);
	mem_deref(od);
}


/* ------------------------------------------------------------------- */
/* Devices (v1.1 - see PROTOCOL.md "devices"/"set_device")             */

/*
 * Appends one array entry per real device in `driver_devs` (a
 * struct ausrc/struct auplay's own dev_list - see baresip.h struct
 * ausrc/struct auplay) to `arr`, named "<driver_name>,<device name>" -
 * the same "module,device" shape baresip's own audio_source/
 * audio_player config-file syntax uses (see run-spike.sh's generated
 * config), so a client can round-trip a "devices" event's "name"
 * straight into "set_device"'s own "name" field unmodified (see
 * cmd_set_device() below, which splits on the first comma the same way
 * this builds it).
 *
 * If `driver_devs` is empty - true for every driver in this spike's
 * actual minimal module set (BUILD.md "Module selection": ausine/
 * aufile, no coreaudio/alsa/wasapi/... - confirmed by reading both
 * modules' source, neither ever calls mediadev_add()) - falls back to
 * reporting the driver itself as the one selectable pseudo-device for
 * its direction, so "devices" is never an empty, useless array in this
 * build. A future real device-backend module plugs in with no change
 * here: its dev_list stops being empty and this naturally starts
 * emitting one real entry per device instead of the one driver-level
 * fallback entry.
 */
static void devices_add_driver(struct odict *arr, const char *driver_name,
				const struct list *driver_devs,
				const char *cfg_mod, const char *cfg_dev)
{
	struct le *le;
	bool any = false;

	for (le = list_head(driver_devs); le; le = le->next) {
		const struct mediadev *dev = le->data;
		struct odict *entry = NULL;
		char name[CENT_DEVICE_NAME_SIZE];
		bool active;

		if (odict_alloc(&entry, 4))
			continue;

		(void)re_snprintf(name, sizeof(name), "%s,%s", driver_name,
				   dev->name);
		active = !str_casecmp(driver_name, cfg_mod) &&
			 !str_casecmp(dev->name, cfg_dev);

		(void)odict_entry_add(entry, "name", ODICT_STRING, name);
		(void)odict_entry_add(entry, "active", ODICT_BOOL, active);
		(void)odict_entry_add(arr, "device", ODICT_OBJECT, entry);
		mem_deref(entry);
		any = true;
	}

	if (!any) {
		struct odict *entry = NULL;
		char name[CENT_DEVICE_NAME_SIZE];

		if (odict_alloc(&entry, 4))
			return;

		if (str_isset(cfg_dev) && !str_casecmp(driver_name, cfg_mod))
			(void)re_snprintf(name, sizeof(name), "%s,%s",
					   driver_name, cfg_dev);
		else
			str_ncpy(name, driver_name, sizeof(name));

		(void)odict_entry_add(entry, "name", ODICT_STRING, name);
		(void)odict_entry_add(entry, "active", ODICT_BOOL,
				       !str_casecmp(driver_name, cfg_mod));
		(void)odict_entry_add(arr, "device", ODICT_OBJECT, entry);
		mem_deref(entry);
	}
}


/* Shared by emit_devices() (the standalone "devices" event) and
 * emit_result()'s CENT_CMD_DEVICES enrichment (see PROTOCOL.md
 * "result") - adds "input"/"output" ODICT_ARRAY entries directly onto
 * whatever odict is passed in, so both call sites walk the driver
 * lists exactly once each, same code, same output shape. */
static void fill_devices_fields(struct odict *od)
{
	struct config *cfg = conf_config();
	struct odict *input = NULL;
	struct odict *output = NULL;
	struct le *le;

	if (!cfg)
		return;

	if (odict_alloc(&input, 8) || odict_alloc(&output, 8)) {
		mem_deref(input);
		mem_deref(output);
		return;
	}

	for (le = list_head(baresip_ausrcl()); le; le = le->next) {
		const struct ausrc *as = le->data;

		devices_add_driver(input, as->name, &as->dev_list,
				    cfg->audio.src_mod, cfg->audio.src_dev);
	}

	for (le = list_head(baresip_auplayl()); le; le = le->next) {
		const struct auplay *ap = le->data;

		devices_add_driver(output, ap->name, &ap->dev_list,
				    cfg->audio.play_mod, cfg->audio.play_dev);
	}

	(void)odict_entry_add(od, "input", ODICT_ARRAY, input);
	(void)odict_entry_add(od, "output", ODICT_ARRAY, output);
	mem_deref(input);
	mem_deref(output);
}


static void emit_devices(void)
{
	struct odict *od = NULL;

	if (odict_alloc(&od, 4))
		return;

	(void)odict_entry_add(od, "event", ODICT_STRING, "devices");
	fill_devices_fields(od);

	emit(od);
	mem_deref(od);
}


/* ------------------------------------------------------------------- */
/* Call/UA resolution helpers                                          */

/* The one UA this engine registers (see run-spike.sh: one CENT_EXT
 * account). uag_find_aor(NULL) is the established idiom for "the first/
 * only UA" already used elsewhere in baresip (see
 * modules/presence/subscriber.c's subscribe(), same call, same
 * comment). */
static struct ua *primary_ua(void)
{
	return uag_find_aor(NULL);
}


/* Resolves a command's optional call_id to a call: uag_call_find()
 * (searches every UA's call list by id) when the caller supplied one,
 * otherwise ua_call(primary_ua()) - the "current" call of the one UA
 * this engine runs, i.e. the same call a single-call session has always
 * unambiguously meant. See this file's top-of-file comment for why every
 * call-scoped command resolves "current" this same way. */
static struct call *resolve_call(bool have_call_id, const char *call_id)
{
	if (have_call_id)
		return uag_call_find(call_id);

	return ua_call(primary_ua());
}


/*
 * v1.3: builds "sip:<ext>@<host>" against the same PBX host/port `ua`'s
 * account registered against - shared by blf_subscribe() (watches ext's
 * dialog state) and cmd_park() (blind-transfers to the parking lot's
 * pilot ext) since both need exactly the same "bare extension on this
 * account's own PBX" address shape (see PROTOCOL.md "blf_subscribe" and
 * "park") - factored out here rather than duplicated a second time.
 *
 * @return 0 on success (`uri` filled in), an error code otherwise. Never
 *         emits its own error event - callers add their own
 *         command-specific message (see call sites).
 */
static int build_pbx_ext_uri(struct ua *ua, const char *ext,
			      char *uri, size_t uri_size)
{
	struct account *acc;
	struct uri *aor;

	if (!ua)
		return ENOENT;

	acc = ua_account(ua);
	aor = account_luri(acc);
	if (!aor)
		return ENOENT;

	if (re_snprintf(uri, uri_size, "sip:%s@%r", ext, &aor->host) < 0)
		return EOVERFLOW;

	return 0;
}


/*
 * v1.1 request/response correlation (see PROTOCOL.md "result"). Only
 * ever called from process_line(), and only when the input command
 * carried an "id" - see cmd.have_id. `ok` reflects whether this
 * command's own *synchronous* dispatch succeeded - i.e. whether
 * emit_error()/emit_errorf() fired during it (see g_error_seq) - not
 * the eventual outcome of anything asynchronous: a blind_transfer that
 * gets "result ok:true" can still later fail far-end, exactly like v1's
 * existing BEVENT_CALL_TRANSFER_FAILED -> error-event convention for
 * that same command (see event_handler()); "ok:true" here means
 * "accepted and dispatched without a synchronous validation/API
 * failure", not a guarantee about what happens next - watch the normal
 * call_state/reg_state/stats/blf/... events for that, same as always.
 *
 * `type`/`cmd` decide the two cases that also get "command-specific
 * fields" merged onto a successful result: quality_stats and devices
 * are both "query" commands whose entire purpose is the data they
 * return, so an "ok:true" with no data would be nearly useless without
 * a second stats/devices event to correlate by hand - every other
 * command is a pure action, where ok/error is the complete story (a
 * consumer that wants more detail already has call_id/ext/etc from its
 * own request to correlate against the normal broadcast events).
 */
static void emit_result(const struct cent_cmd *cmd, enum cent_cmd_type type,
			 bool ok, const char *error)
{
	struct odict *od = NULL;

	if (odict_alloc(&od, 16))
		return;

	(void)odict_entry_add(od, "event", ODICT_STRING, "result");
	(void)odict_entry_add(od, "id", ODICT_STRING, cmd->id);
	(void)odict_entry_add(od, "ok", ODICT_BOOL, ok);

	if (!ok) {
		(void)odict_entry_add(od, "error", ODICT_STRING,
				       error ? error : "");
	}
	else if (type == CENT_CMD_QUALITY_STATS) {
		struct call *call = resolve_call(cmd->have_call_id,
						  cmd->call_id);
		const struct rtcp_stats *rs = call ?
			stream_rtcp_stats(audio_strm(call_audio(call))) :
			NULL;

		if (rs)
			fill_stats_fields(od, call, rs);
	}
	else if (type == CENT_CMD_DEVICES) {
		fill_devices_fields(od);
	}

	emit(od);
	mem_deref(od);
}


/* ------------------------------------------------------------------- */
/* BLF (Event: dialog) subscriptions                                   */

static struct blf_sub *blf_find(const char *ext)
{
	struct le *le;

	for (le = list_head(&blf_subs); le; le = le->next) {
		struct blf_sub *b = le->data;

		if (!str_casecmp(b->ext, ext))
			return b;
	}

	return NULL;
}


static void blf_destructor(void *arg)
{
	struct blf_sub *b = arg;

	list_unlink(&b->le);
	b->sub = mem_deref(b->sub);
}


static void blf_notify_handler(struct sip *sip, const struct sip_msg *msg,
				void *arg)
{
	struct blf_sub *b = arg;
	const struct sip_hdr *type_hdr;
	enum cent_blf_state state;

	/* Same defensive Content-Type check as
	 * modules/presence/subscriber.c's notify_handler() for the sibling
	 * Event: presence package - reject anything that isn't the body
	 * type we asked for (Accept: application/dialog-info+xml, see
	 * blf_subscribe()) rather than feeding it to dialog_info_parse()
	 * and guessing. */
	type_hdr = sip_msg_hdr(msg, SIP_HDR_CONTENT_TYPE);
	if (!type_hdr ||
	    pl_strcasecmp(&type_hdr->val, "application/dialog-info+xml")) {

		if (type_hdr)
			warning("ctrl_json: blf %s: unsupported"
				" content-type: '%r'\n", b->ext,
				&type_hdr->val);

		sip_treplyf(NULL, NULL, sip, msg, false,
			    415, "Unsupported Media Type",
			    "Accept: application/dialog-info+xml\r\n"
			    "Content-Length: 0\r\n\r\n");
		return;
	}

	(void)sip_treply(NULL, sip, msg, 200, "OK");

	state = dialog_info_parse((const char *)mbuf_buf(msg->mb),
				   mbuf_get_left(msg->mb));
	emit_blf(b->ext, state);
}


static void blf_close_handler(int err, const struct sip_msg *msg,
			       const struct sipevent_substate *substate,
			       void *arg)
{
	struct blf_sub *b = arg;
	char ext[CENT_EXT_SIZE];

	(void)err;
	(void)msg;
	(void)substate;

	/* Copy the ext before freeing b (the destructor unlinks/derefs it,
	 * and b->sub is what's calling us right now via its own
	 * destructor-driven "terminate" path - see subscribe.c's
	 * terminate() swapping in internal handlers before a *locally*
	 * initiated mem_deref(b->sub) - so this handler only ever runs for
	 * a genuine remote-side failure/expiry, never for our own
	 * blf_unsubscribe(), but b is about to go away either way). */
	str_ncpy(ext, b->ext, sizeof(ext));
	b->sub = NULL;   /* already being torn down by its own destructor */
	mem_deref(b);

	emit_blf(ext, cent_blf_state_for_close());

	/* No auto-retry (unlike modules/presence/subscriber.c's
	 * wait_fail()/tmr_handler() loop) - out of scope for F1: a client
	 * that wants the watch back after a failure sends blf_subscribe
	 * again. See PROTOCOL.md "Planned". */
}


static int blf_auth_handler(char **username, char **password,
			     const char *realm, void *arg)
{
	return account_auth(arg, username, password, realm);
}


static void blf_subscribe(const char *ext)
{
	struct ua *ua = primary_ua();
	struct account *acc;
	struct blf_sub *b;
	const char *routev[1];
	char uri[256];
	int err;

	if (!ua) {
		emit_error("blf_subscribe: no UA configured");
		return;
	}

	if (blf_find(ext)) {
		emit_errorf("blf_subscribe: already subscribed to ext '%s'",
			    ext);
		return;
	}

	acc = ua_account(ua);

	/* Target: same PBX host/port this account registered against,
	 * different user part - matches how run-spike.sh builds the
	 * account URI itself (CENT_EXT@CENT_HOST). */
	err = build_pbx_ext_uri(ua, ext, uri, sizeof(uri));
	if (err == EOVERFLOW) {
		emit_error("blf_subscribe: uri too long");
		return;
	}
	else if (err) {
		emit_error("blf_subscribe: could not resolve PBX host from"
			   " the account");
		return;
	}

	b = mem_zalloc(sizeof(*b), blf_destructor);
	if (!b) {
		emit_error("blf_subscribe: out of memory");
		return;
	}
	str_ncpy(b->ext, ext, sizeof(b->ext));

	routev[0] = ua_outbound(ua);

	err = sipevent_subscribe(&b->sub, uag_sipevent_sock(), uri, NULL,
				  account_aor(acc), "dialog", NULL,
				  BLF_EXPIRES, ua_cuser(ua), routev,
				  routev[0] ? 1 : 0,
				  blf_auth_handler, acc, true, NULL,
				  blf_notify_handler, blf_close_handler, b,
				  "Accept: application/dialog-info+xml\r\n");
	if (err) {
		emit_errorf("blf_subscribe: %m", err);
		mem_deref(b);
		return;
	}

	list_append(&blf_subs, &b->le, b);
}


static void blf_unsubscribe(const char *ext)
{
	struct blf_sub *b = blf_find(ext);

	if (!b) {
		emit_errorf("blf_unsubscribe: not subscribed to ext '%s'",
			    ext);
		return;
	}

	/* mem_deref(b->sub) here runs sipsub's own terminate() path (see
	 * core/deps/re/src/sipevent/subscribe.c), which swaps in internal
	 * notify/close handlers *before* sending the final Expires: 0
	 * SUBSCRIBE - blf_close_handler() above is not re-entered for this
	 * intentional teardown. */
	mem_deref(b);
}


/* ------------------------------------------------------------------- */
/* Attended transfer                                                    */

static void xfer_reset(void)
{
	xfer_source = NULL;
	xfer_target = NULL;
}


static void cmd_attended_transfer(const struct cent_cmd *cmd)
{
	struct ua *ua = primary_ua();
	struct call *source;
	struct call *target = NULL;
	int err;

	if (!ua) {
		emit_error("attended_transfer: no UA configured");
		return;
	}

	if (xfer_source) {
		emit_error("attended_transfer: another attended transfer is"
			   " already pending (complete_transfer or"
			   " abort_transfer it first)");
		return;
	}

	source = resolve_call(cmd->have_call_id, cmd->call_id);
	if (!source) {
		emit_error("attended_transfer: call not found");
		return;
	}

	if (!call_supported(source, REPLACES)) {
		emit_error("attended_transfer: peer does not support the"
			   " Replaces extension, cannot complete an attended"
			   " transfer");
		return;
	}

	err = call_hold(source, true);
	if (err) {
		emit_errorf("attended_transfer: hold failed (%m)", err);
		return;
	}
	emit_call_state(source, "hold");

	err = ua_connect(ua, &target, NULL, cmd->uri, VIDMODE_OFF);
	if (err) {
		emit_errorf("attended_transfer: dial to '%s' failed (%m)",
			    cmd->uri, err);
		(void)uag_hold_resume(source);   /* best-effort: don't strand
						   * the source call on hold
						   * for a consultation call
						   * that never started */
		return;
	}

	xfer_source = source;
	xfer_target = target;

	emit_attended_transfer_started(source, target);
	/* target's own ringing/established/closed lifecycle is already
	 * covered by the normal event_handler() -> call_state relay below,
	 * no special-casing needed here. */
}


static void cmd_complete_transfer(void)
{
	int err;

	if (!xfer_source || !xfer_target) {
		emit_error("complete_transfer: no attended transfer"
			   " pending");
		return;
	}

	err = call_replace_transfer(xfer_source, xfer_target);
	if (err) {
		emit_errorf("complete_transfer failed (%m)", err);
		return;
	}

	/* Result arrives async, the same way blind_transfer's does: on
	 * success both legs get a normal call_state "closed" (see
	 * src/call.c sipsub_notify_handler()'s 2xx-sipfrag branch, which
	 * raises CALL_EVENT_CLOSED through the exact same path a
	 * peer-initiated hangup does - no extra cleanup needed from us
	 * beyond forgetting the pair here); on failure,
	 * BEVENT_CALL_TRANSFER_FAILED is relayed as an error event (see
	 * event_handler()). */
	xfer_reset();
}


static void cmd_abort_transfer(void)
{
	if (!xfer_source || !xfer_target) {
		emit_error("abort_transfer: no attended transfer pending");
		return;
	}

	(void)uag_hold_resume(xfer_source);
	xfer_reset();
}


/* ------------------------------------------------------------------- */
/* Command handlers                                                     */

/* Named do_register()/do_unregister(), not cmd_register()/
 * cmd_unregister(): those exact names are already baresip core API
 * (baresip.h - registering/unregistering a *command table*, unrelated
 * to SIP registration) - reusing them here would collide. */
static void do_register(void)
{
	struct ua *ua = primary_ua();
	int err;

	if (!ua) {
		emit_error("register: no UA configured");
		return;
	}

	err = ua_register(ua);
	if (err)
		emit_errorf("register failed (%m)", err);

	/* success is observable via the resulting reg_state events,
	 * matching the existing v0 convention (see PROTOCOL.md). */
}


static void do_unregister(void)
{
	struct ua *ua = primary_ua();

	if (!ua) {
		emit_error("unregister: no UA configured");
		return;
	}

	ua_unregister(ua);
}


static void cmd_hangup(const struct cent_cmd *cmd)
{
	struct call *call = resolve_call(cmd->have_call_id, cmd->call_id);

	if (!call) {
		emit_error("hangup: call not found");
		return;
	}

	/*
	 * Must be ua_hangup(), not the lower-level call_hangup(): raw
	 * call_hangup()/call_hangupf() (src/call.c) tears down the SIP
	 * session (sends BYE) and marks the call CALL_STATE_TERMINATED,
	 * but does *not* itself raise CALL_EVENT_CLOSED - the call object
	 * would otherwise sit in the UA's call list forever (a real leak)
	 * with no call_state "closed" ever emitted. ua_hangup()/
	 * ua_hangupf() (src/ua.c) is what additionally fires
	 * BEVENT_CALL_CLOSED and mem_deref()s the call - confirmed by
	 * reading both call.c and ua.c while implementing this (matches
	 * exactly what modules/menu/static_menu.c's own cmd_hangup() does
	 * for the CLI/menu path). event_handler() below relays that bevent
	 * as call_state "closed" - no separate emit needed here.
	 */
	ua_hangup(call_get_ua(call), call, 0, NULL);
}


static void cmd_hold(const struct cent_cmd *cmd)
{
	struct call *call = resolve_call(cmd->have_call_id, cmd->call_id);
	int err;

	if (!call) {
		emit_error("hold: call not found");
		return;
	}

	err = call_hold(call, true);
	if (err) {
		emit_errorf("hold failed (%m)", err);
		return;
	}

	/* A *local* hold has no corresponding bevent - BEVENT_CALL_HOLD is
	 * only raised for a detected *peer*-initiated hold (see src/call.c
	 * detect_hold_resume(): it only inspects offers/answers coming
	 * from the remote side). Emit our own call_state so v0's "success
	 * is observable via the resulting call_state event" convention
	 * still holds for a locally-initiated hold. */
	emit_call_state(call, "hold");
}


static void cmd_resume(const struct cent_cmd *cmd)
{
	struct call *call = resolve_call(cmd->have_call_id, cmd->call_id);
	int err;

	if (!call) {
		emit_error("resume: call not found");
		return;
	}

	/* uag_hold_resume(), not a raw call_hold(call, false): it also
	 * holds whatever *other* call is currently active first (so two
	 * calls are never both off-hold at once), matching
	 * modules/menu/dynamic_menu.c's cmd_call_resume() - relevant the
	 * moment there are 2 calls (attended transfer). */
	err = uag_hold_resume(call);
	if (err) {
		emit_errorf("resume failed (%m)", err);
		return;
	}

	emit_call_state(call, "resumed");
}


static void cmd_dtmf(const struct cent_cmd *cmd)
{
	struct call *call = resolve_call(cmd->have_call_id, cmd->call_id);
	size_t i;
	int err = 0;

	if (!call) {
		emit_error("dtmf: call not found");
		return;
	}

	/* One call_send_digit() per digit, then a KEYCODE_REL to mark the
	 * last one released - matches modules/menu/dynamic_menu.c's
	 * send_code(), which audio_send_digit() (src/audio.c) depends on
	 * for its start/end RFC2833 event pairing (it tracks "the
	 * previous key" and emits that one's *end* event on the next
	 * call, so a trailing release call is required to end the final
	 * digit). */
	for (i = 0; cmd->digits[i] && !err; i++)
		err = call_send_digit(call, cmd->digits[i]);

	if (!err)
		err = call_send_digit(call, KEYCODE_REL);

	if (err)
		emit_errorf("dtmf failed (%m)", err);
}


static void cmd_mute(const struct cent_cmd *cmd)
{
	struct call *call = resolve_call(cmd->have_call_id, cmd->call_id);
	struct audio *audio;

	if (!call) {
		emit_error("mute: call not found");
		return;
	}

	audio = call_audio(call);
	if (!audio) {
		emit_error("mute: call has no audio");
		return;
	}

	audio_mute(audio, cmd->mute_on);
	emit_call_state(call, cmd->mute_on ? "muted" : "unmuted");
}


static void cmd_blind_transfer(const struct cent_cmd *cmd)
{
	struct call *call = resolve_call(cmd->have_call_id, cmd->call_id);
	int err;

	if (!call) {
		emit_error("blind_transfer: call not found");
		return;
	}

	err = call_transfer(call, cmd->uri);
	if (err)
		emit_errorf("blind_transfer failed (%m)", err);

	/* Result arrives async via the REFER subscription's sipfrag
	 * NOTIFYs: success collapses the call closed (call_state "closed",
	 * same path described in cmd_complete_transfer()'s comment),
	 * failure surfaces as BEVENT_CALL_TRANSFER_FAILED, relayed as an
	 * error event - see event_handler(). */
}


/*
 * v1.3 "park" (see PROTOCOL.md "park"): parks a call by blind-transferring
 * it (REFER, same call_transfer() mechanism as cmd_blind_transfer() right
 * above - park IS a blind transfer, just to a specific kind of PBX
 * resource) to `cmd->ext` - the target parking lot's *pilot* extension
 * (e.g. this engine's own test PBX's "70", confirmed read-only via
 * `asterisk -rx "parking show"`/`"dialplan show 70@from-internal"` to run
 * `Park()` - see core/E2E-F1.md "F5 park" for the exact commands/output).
 * `ext` is required (see cmd.c) rather than defaulted to any particular
 * value - a parking lot's pilot extension is per-PBX configuration, not
 * a protocol constant this engine should guess at (this repo is public;
 * baking in this deployment's own "70" would also embed one test PBX's
 * config into shipped code for no protocol reason - see PROTOCOL.md
 * "park" for the full explanation, including why the resulting
 * confirmation event's `ext` field is the *pilot* extension the park
 * request targeted, not a specific auto-assigned parking slot number -
 * that's genuinely not observable over plain SIP signaling this engine's
 * call leg is party to, confirmed by reading how Asterisk's REFER
 * handling and Park() app interact here, not guessed).
 */
static void cmd_park(const struct cent_cmd *cmd)
{
	struct call *call = resolve_call(cmd->have_call_id, cmd->call_id);
	char uri[256];
	int err;

	if (!call) {
		emit_error("park: call not found");
		return;
	}

	err = build_pbx_ext_uri(primary_ua(), cmd->ext, uri, sizeof(uri));
	if (err == EOVERFLOW) {
		emit_error("park: uri too long");
		return;
	}
	else if (err) {
		emit_error("park: could not resolve PBX host from the"
			   " account");
		return;
	}

	err = call_transfer(call, uri);
	if (err) {
		emit_errorf("park failed (%m)", err);
		return;
	}

	/* Confirms the REFER was dispatched, not the eventual parked
	 * outcome - see emit_park()'s own comment. Async failure, same as
	 * blind_transfer, surfaces as BEVENT_CALL_TRANSFER_FAILED -> a
	 * plain error event, see event_handler(). */
	emit_park(call, cmd->ext);
}


/*
 * v1.1 "set_device" (see PROTOCOL.md "devices"/"set_device"). Splits
 * cmd->device_name back into module/device the same way
 * devices_add_driver() built it (see that function's header comment) -
 * a bare name with no comma is treated as a module with no specific
 * device string (dev "" -> NULL below), matching how this spike's own
 * ausine/aufile drivers are reported today (see
 * devices_add_driver()'s "no real per-device enumeration" fallback).
 *
 * Two effects, both real, not just one-or-the-other:
 *
 *  1. Persists the choice into conf_config()->audio.{src,play}_{mod,dev}
 *     - the exact same fields run-spike.sh's generated config seeds at
 *     process start (see BUILD.md) - so it becomes the default for any
 *     call started *after* this command, same as "at next call" in the
 *     task this version implements.
 *
 *  2. ALSO applied live, immediately, to whatever call is active right
 *     now (if any) via audio_set_source()/audio_set_player()
 *     (src/audio.c) - investigated briefly while building this: both
 *     are real hot-swap APIs, confirmed by reading their implementation
 *     - they mem_deref() the running ausrc_st/auplay_st and
 *     ausrc_alloc()/aurecv_start_player() a fresh one against the SAME
 *     struct audio, no re-INVITE, no call restart. So "live if baresip
 *     supports hot-swap" - it does, and this uses it.
 *
 * Scope note: like every other no-call_id command in this file (see
 * resolve_call()), "the current call" means ua_call(primary_ua()) - a
 * second, concurrent call (the attended-transfer consultation leg) is
 * deliberately NOT touched here; a future multi-call-aware version
 * would need this command to grow its own optional call_id, same as
 * the rest already have.
 */
static void cmd_set_device(const struct cent_cmd *cmd)
{
	struct config *cfg = conf_config();
	char mod[CENT_DEVICE_KIND_SIZE + CENT_DEVICE_NAME_SIZE] = "";
	char dev[CENT_DEVICE_NAME_SIZE] = "";
	const char *comma;
	struct call *call;
	bool is_input;
	int err;

	if (!cfg) {
		emit_error("set_device: no config");
		return;
	}

	is_input = !str_casecmp(cmd->device_kind, "input");

	comma = strchr(cmd->device_name, ',');
	if (comma) {
		size_t modlen = (size_t)(comma - cmd->device_name);

		if (modlen >= sizeof(mod)) {
			emit_error("set_device: module name too long");
			return;
		}
		memcpy(mod, cmd->device_name, modlen);
		mod[modlen] = '\0';
		str_ncpy(dev, comma + 1, sizeof(dev));
	}
	else {
		str_ncpy(mod, cmd->device_name, sizeof(mod));
	}

	if (is_input) {
		str_ncpy(cfg->audio.src_mod, mod, sizeof(cfg->audio.src_mod));
		str_ncpy(cfg->audio.src_dev, dev, sizeof(cfg->audio.src_dev));
	}
	else {
		str_ncpy(cfg->audio.play_mod, mod,
			 sizeof(cfg->audio.play_mod));
		str_ncpy(cfg->audio.play_dev, dev,
			 sizeof(cfg->audio.play_dev));
	}

	call = ua_call(primary_ua());
	if (!call) {
		/* Persisted default above still applies to the next call -
		 * nothing live to update right now is not itself an error
		 * (a client can already tell from "devices"' own "active"
		 * flag whether anything is live). */
		return;
	}

	if (is_input)
		err = audio_set_source(call_audio(call), mod,
					str_isset(dev) ? dev : NULL);
	else
		err = audio_set_player(call_audio(call), mod,
					str_isset(dev) ? dev : NULL);

	if (err)
		emit_errorf("set_device: saved as the default for future"
			    " calls, but live swap on the current call"
			    " failed (%m)", err);
}


/*
 * v1.2 "tap_start"/"tap_stop" (see PROTOCOL.md, audiotap.h). Same
 * resolve_call()-then-act shape as every other call-scoped command in
 * this file (cmd_hold()/cmd_mute()/... above) - the actual aufilt/WAV-
 * writer mechanics live in audiotap.c, not here; this function's whole
 * job is decoding a struct cent_cmd into a resolved call and turning
 * audiotap_start()'s outcome into the right event.
 */
static void cmd_tap_start(const struct cent_cmd *cmd)
{
	struct call *call = resolve_call(cmd->have_call_id, cmd->call_id);
	struct audiotap_result res;
	const char *errmsg = NULL;

	if (!call) {
		emit_error("tap_start: call not found");
		return;
	}

	if (audiotap_start(call, cmd->dir, &res, &errmsg)) {
		emit_errorf("%s", errmsg ? errmsg : "tap_start failed");
		return;
	}

	emit_tap_state(call, "started", &res);
}


static void cmd_tap_stop(const struct cent_cmd *cmd)
{
	struct call *call = resolve_call(cmd->have_call_id, cmd->call_id);
	struct audiotap_result res;
	const char *errmsg = NULL;

	if (!call) {
		emit_error("tap_stop: call not found");
		return;
	}

	if (audiotap_stop(call, &res, &errmsg)) {
		emit_errorf("%s", errmsg ? errmsg : "tap_stop failed");
		return;
	}

	emit_tap_state(call, "stopped", &res);
}


/* ------------------------------------------------------------------- */
/* Command dispatch                                                     */

/*
 * Decode + dispatch one JSON command line. cent_cmd_decode() (cmd.c) is
 * the pure, unit-tested part (see core/modules/ctrl_json/test/); this
 * function is the (inherently untestable without a live engine) part
 * that turns a decoded command into real baresip calls.
 *
 * dial/answer/quit still go through cmd_process_long() - see this file's
 * top-of-file comment for why those three (and not the rest) stay on
 * that path.
 *
 * v1.1: if the decoded command carried an "id" (cmd.have_id), a
 * correlated "result" event is emitted after dispatch - see
 * emit_result()'s own header comment for the full contract. This wraps
 * the *entire* switch below (every existing case, completely
 * unmodified) rather than touching each handler individually: ok is
 * derived from whether g_error_seq moved during dispatch (see
 * emit_error()), so no handler needed a signature change for this.
 */
static void process_line(const char *line, size_t len)
{
	struct odict *od = NULL;
	struct cent_cmd cmd;
	enum cent_cmd_type type;
	const char *errmsg = NULL;
	char cmd_name[64] = "";
	uint32_t error_seq_before;

	if (json_decode_odict(&od, 8, line, len, 8)) {
		emit_error("invalid JSON command line");
		return;
	}

	/* Captured before mem_deref(od) below purely for the
	 * CENT_CMD_UNKNOWN error message - everything cent_cmd_decode()
	 * itself needs from `od`, it copies out into `cmd` and `errmsg`
	 * before returning, so `od` is safe to free right after this call
	 * in every other case. */
	if (odict_string(od, "cmd"))
		str_ncpy(cmd_name, odict_string(od, "cmd"), sizeof(cmd_name));

	type = cent_cmd_decode(&cmd, od, &errmsg);
	mem_deref(od);

	error_seq_before = g_error_seq;

	switch (type) {

	case CENT_CMD_NONE:
		emit_error(errmsg);
		break;

	case CENT_CMD_UNKNOWN:
		emit_errorf("unknown cmd '%s'", cmd_name);
		break;

	case CENT_CMD_DIAL: {
		char buf[CENT_URI_SIZE + 16];
		struct mbuf *resp = mbuf_alloc(256);
		struct re_printf pf = {print_handler, resp};
		int err;

		if (!resp)
			break;

		(void)re_snprintf(buf, sizeof(buf), "dial %s", cmd.uri);
		err = cmd_process_long(baresip_commands(), buf, str_len(buf),
					&pf, NULL);
		if (err)
			emit_errorf("cmd 'dial' failed (%m)", err);
		mem_deref(resp);
		break;
	}

	case CENT_CMD_ANSWER:
	case CENT_CMD_QUIT: {
		struct mbuf *resp = mbuf_alloc(256);
		struct re_printf pf = {print_handler, resp};
		char buf[16 + CENT_ID_SIZE];
		int err;

		if (!resp)
			break;

		/*
		 * v1.3: an explicit call_id (see PROTOCOL.md "answer") rides
		 * as baresip's own "accept <call-id>" long-command parameter
		 * - modules/menu/static_menu.c's cmd_answer() already
		 * resolves that exact shape via uag_call_find() (confirmed
		 * by reading it, not assumed - see core/BUILD.md's own
		 * "read the source, don't guess" precedent), so this needed
		 * no new resolve-call path here, just building the right
		 * string. No call_id keeps the plain "accept"/"quit" this
		 * case has always sent - byte-for-byte unchanged for a
		 * caller that never sends call_id, and CENT_CMD_QUIT never
		 * decodes one (see cmd.c) so it's unaffected either way.
		 */
		if (type == CENT_CMD_ANSWER && cmd.have_call_id)
			(void)re_snprintf(buf, sizeof(buf), "accept %s",
					   cmd.call_id);
		else
			str_ncpy(buf, (type == CENT_CMD_ANSWER) ? "accept" :
				 "quit", sizeof(buf));

		err = cmd_process_long(baresip_commands(), buf, str_len(buf),
					&pf, NULL);
		if (err)
			emit_errorf("cmd '%s' failed (%m)", buf, err);
		mem_deref(resp);
		break;
	}

	case CENT_CMD_REGISTER:
		do_register();
		break;

	case CENT_CMD_UNREGISTER:
		do_unregister();
		break;

	case CENT_CMD_HANGUP:
		cmd_hangup(&cmd);
		break;

	case CENT_CMD_HOLD:
		cmd_hold(&cmd);
		break;

	case CENT_CMD_RESUME:
		cmd_resume(&cmd);
		break;

	case CENT_CMD_DTMF:
		cmd_dtmf(&cmd);
		break;

	case CENT_CMD_MUTE:
		cmd_mute(&cmd);
		break;

	case CENT_CMD_BLIND_TRANSFER:
		cmd_blind_transfer(&cmd);
		break;

	case CENT_CMD_ATTENDED_TRANSFER:
		cmd_attended_transfer(&cmd);
		break;

	case CENT_CMD_COMPLETE_TRANSFER:
		cmd_complete_transfer();
		break;

	case CENT_CMD_ABORT_TRANSFER:
		cmd_abort_transfer();
		break;

	case CENT_CMD_QUALITY_STATS:
		emit_stats(resolve_call(cmd.have_call_id, cmd.call_id));
		break;

	case CENT_CMD_BLF_SUBSCRIBE:
		blf_subscribe(cmd.ext);
		break;

	case CENT_CMD_BLF_UNSUBSCRIBE:
		blf_unsubscribe(cmd.ext);
		break;

	case CENT_CMD_DEVICES:
		emit_devices();
		break;

	case CENT_CMD_SET_DEVICE:
		cmd_set_device(&cmd);
		break;

	case CENT_CMD_TAP_START:
		cmd_tap_start(&cmd);
		break;

	case CENT_CMD_TAP_STOP:
		cmd_tap_stop(&cmd);
		break;

	case CENT_CMD_PARK:
		cmd_park(&cmd);
		break;

	default:
		emit_error("internal error: unhandled command type");
		break;
	}

	if (cmd.have_id) {
		bool ok = (g_error_seq == error_seq_before);

		emit_result(&cmd, type, ok, ok ? NULL : g_last_error);
	}
}


/*
 * Relay UA/call events -> JSON events (see core/PROTOCOL.md).
 *
 * v0 covered: reg_state transitions, and incoming/ringing/established/
 * closed call_state. v1 adds:
 *   - BEVENT_CALL_HOLD/_RESUME: a *peer*-initiated hold/resume (see
 *     cmd_hold()'s comment for why local hold/resume are instead emitted
 *     synthetically, right at the call site).
 *   - BEVENT_CALL_TRANSFER_FAILED: relayed as a plain error event -
 *     reuses the existing error schema rather than inventing a new one,
 *     since "cmd X failed" is exactly what this already communicates for
 *     any other command's synchronous failure, and this is a transfer's
 *     *asynchronous* one.
 * Still not mapped (see PROTOCOL.md "Planned"): BEVENT_CALL_TRANSFER/
 * _REDIRECT (the transfer-*target*-side perspective - this account never
 * plays that role in any tested flow), DTMF-received, RTCP/VU-meter,
 * local SDP/menc details.
 */
static void event_handler(enum bevent_ev ev, struct bevent *event, void *arg)
{
	struct ua *ua     = bevent_get_ua(event);
	struct call *call = bevent_get_call(event);
	(void)arg;

	switch (ev) {

	case BEVENT_REGISTERING:
		emit_reg_state(ua, "registering", NULL);
		break;

	case BEVENT_REGISTER_OK:
		emit_reg_state(ua, "registered", NULL);
		break;

	case BEVENT_REGISTER_FAIL:
		emit_reg_state(ua, "failed", bevent_get_text(event));
		break;

	case BEVENT_UNREGISTERING:
		emit_reg_state(ua, "unregistered", NULL);
		break;

	case BEVENT_CALL_INCOMING:
		emit_call_state(call, "incoming");
		break;

	case BEVENT_CALL_RINGING:
	case BEVENT_CALL_PROGRESS:
		emit_call_state(call, "ringing");
		break;

	case BEVENT_CALL_ESTABLISHED:
		emit_call_state(call, "established");
		break;

	case BEVENT_CALL_CLOSED: {
		/* v1.2: auto-finalize an audio tap that outlived its
		 * tap_stop (peer hangup, failed dial, ... - the "call-end"
		 * trigger from PROTOCOL.md "tap_start"/audiotap.h's own doc
		 * comment) - "never leave a corrupt WAV" needs this to run
		 * on *every* path a call can end on, not just an explicit
		 * tap_stop. No-op (false, nothing emitted) for the common
		 * case of a call that was never tapped. */
		struct audiotap_result tap_res;

		if (audiotap_call_closed(call, &tap_res))
			emit_tap_state(call, "stopped", &tap_res);

		/* If either half of a pending attended transfer just
		 * closed on its own (peer hangup, failed dial, ...) before
		 * complete_transfer/abort_transfer ran, don't leave the
		 * other half stranded on hold forever - mirrors
		 * modules/menu/menu.c's own cleanup on the same bevent for
		 * its xfer_call/xfer_targ pair. */
		if (call == xfer_source || call == xfer_target) {
			if (call == xfer_target && xfer_source)
				(void)uag_hold_resume(xfer_source);
			xfer_reset();
		}
		emit_call_state(call, "closed");
		break;
	}

	case BEVENT_CALL_HOLD:
		emit_call_state(call, "hold");
		break;

	case BEVENT_CALL_RESUME:
		emit_call_state(call, "resumed");
		break;

	case BEVENT_CALL_TRANSFER_FAILED:
		emit_errorf("transfer failed: %s", bevent_get_text(event));
		break;

	case BEVENT_AUDIO_ERROR:
		emit_error(bevent_get_text(event));
		break;

	default:
		/* not part of the v1 schema - ignored on purpose */
		break;
	}
}


#ifndef _WIN32

/* ------------------------------------------------------------------- */
/* stdin - POSIX line buffering                                         */

/*
 * Split the accumulated stdin buffer on '\n' and process each complete
 * line. Partial trailing data (no newline yet) is kept for the next
 * read(). POSIX-only: the raw read() below can hand back a chunk
 * containing zero, one, or several lines, so this buffer/split step is
 * needed. The _WIN32 path (see "stdin - Windows" further down) doesn't
 * need an equivalent - fgets() already delivers one complete line at a
 * time on its own.
 */
static void process_inbuf(uint8_t *inbuf, size_t *inlen)
{
	size_t start = 0, i;

	for (i = 0; i < *inlen; i++) {

		if (inbuf[i] != '\n')
			continue;

		if (i > start) {
			size_t len = i - start;

			/* tolerate CRLF line endings too */
			if (len > 0 && inbuf[start + len - 1] == '\r')
				--len;

			if (len > 0)
				process_line((const char *)&inbuf[start],
					     len);
		}

		start = i + 1;
	}

	if (start > 0) {
		*inlen -= start;
		memmove(inbuf, &inbuf[start], *inlen);
	}
}


/* ------------------------------------------------------------------- */
/* stdin - POSIX (unchanged from v0)                                    */

static void stdin_handler(int flags, void *arg)
{
	struct ctrl_st *st = arg;
	ssize_t n;

	if (!(flags & FD_READ))
		return;

	if (st->inlen >= sizeof(st->inbuf)) {
		/* pathological: no newline for a whole buffer's worth of
		 * input - drop it rather than growing unbounded. */
		emit_error("input line too long, buffer reset");
		st->inlen = 0;
	}

	n = read(STDIN_FILENO, st->inbuf + st->inlen,
		 sizeof(st->inbuf) - st->inlen);
	if (n < 0) {
		if (errno == EAGAIN || errno == EINTR)
			return;
		n = 0; /* treat other read errors like EOF */
	}

	if (n == 0) {
		/* stdin closed - the controlling process is gone, so shut
		 * down the same way a "quit" command would. */
		info("ctrl_json: stdin closed, shutting down\n");
		st->fhs = fd_close(st->fhs);
		ua_stop_all(false);
		return;
	}

	st->inlen += (size_t)n;

	process_inbuf(st->inbuf, &st->inlen);
}


static int stdin_start(struct ctrl_st *st)
{
	return fd_listen(&st->fhs, STDIN_FILENO, FD_READ, stdin_handler, st);
}


static void stdin_stop(struct ctrl_st *st)
{
	st->fhs = fd_close(st->fhs);
}


#else /* _WIN32 */

/* ------------------------------------------------------------------- */
/* stdin - Windows                                                      */
/*
 * fd_listen()/STDIN_FILENO/POSIX read() (the v0/POSIX implementation
 * above) has no Windows equivalent for a console or piped stdin handle -
 * re's fd_listen() is a socket-readiness primitive (WSAAsyncSelect-
 * backed on win32), not usable on an arbitrary file HANDLE. The portable
 * fix used throughout re/baresip for "bring a blocking OS primitive into
 * the re_main() event loop" is a dedicated thread (re_thread.h's
 * thrd_t/thrd_create - a cross-platform C11-style wrapper, POSIX pthread
 * on POSIX, no #ifdef needed for the thread API itself) that does the
 * blocking work and hands results to the main/re thread via re_mqueue.h,
 * which is explicitly documented as thread-safe for exactly this
 * (core/deps/re/include/re_mqueue.h: "communicate between two threads
 * ... receiving thread must run the re_main() loop").
 *
 * fgets() on stdin (not raw ReadFile on the HANDLE) is deliberate: the
 * real deployment shape is a piped child process (a Tauri shell spawning
 * this binary), not an interactive console, and fgets() already gives
 * line-buffered reads - exactly this protocol's NDJSON framing - for
 * free, portably, without touching Win32 HANDLE/console-mode APIs at
 * all.
 *
 * The reader thread is intentionally never thrd_join()'d: it only ever
 * exits via EOF/read-error (pushing MQ_EOF first) or the process ending,
 * and joining from ctrl_close() risks blocking indefinite shutdown if
 * ctrl_close() runs for a reason *other* than stdin EOF (e.g. a "quit"
 * command) while the thread is still blocked in fgets() waiting on a
 * peer that hasn't closed its write end yet.
 *
 * Because it's never joined, the thread can still be blocked in fgets()
 * *after* ctrl_close()/stdin_stop() has already run on the main thread
 * (e.g. shutdown triggered by "quit" rather than stdin EOF) - so it must
 * never touch anything ctrl_close() may have already freed. That's why
 * this thread's entry point takes a `struct mqueue *` directly (with its
 * own mem_ref() taken in stdin_start(), released in stdin_thread_main()
 * right before it returns) rather than `struct ctrl_st *`: it never
 * dereferences ctrl_st at all, only its own independently-refcounted
 * mqueue handle, so a concurrent mem_deref() of ctrl_st on the main
 * thread can never race it into a use-after-free. Whichever side (main
 * thread via stdin_stop(), or this thread when it finally returns) drops
 * its reference last is what actually frees the mqueue/closes its pipe;
 * until then mqueue_push() from this thread remains valid even though
 * nothing is reading the other end any more post-shutdown (a harmless,
 * process-lifetime-bounded no-op in that case, not a crash) - and by the
 * time ctrl_close() runs, baresip's own module-shutdown ordering means
 * re_main() has already stopped polling anyway (see main.c), so a late
 * push is never actually delivered to mqueue_handler() below regardless.
 */

enum { MQ_LINE = 1, MQ_EOF = 2 };

struct win_line {
	char  *buf;
	size_t len;
};

static int stdin_thread_main(void *arg)
{
	struct mqueue *mq = arg;
	char line[INBUF_SIZE];

	while (fgets(line, sizeof(line), stdin)) {

		size_t len = strlen(line);
		struct win_line *wl;

		/* tolerate CRLF/LF, matching process_inbuf()'s POSIX-path
		 * behaviour */
		while (len > 0 && (line[len - 1] == '\n' ||
				   line[len - 1] == '\r'))
			--len;

		if (!len)
			continue;

		wl = malloc(sizeof(*wl));
		if (!wl)
			continue;   /* drop this line under OOM, keep reading */

		wl->buf = malloc(len);
		if (!wl->buf) {
			free(wl);
			continue;
		}
		memcpy(wl->buf, line, len);
		wl->len = len;

		if (mqueue_push(mq, MQ_LINE, wl)) {
			free(wl->buf);
			free(wl);
		}
	}

	/* fgets() returned NULL: EOF or a read error on stdin - either way
	 * the controlling process is gone. */
	(void)mqueue_push(mq, MQ_EOF, NULL);

	mem_deref(mq);   /* release this thread's own reference - see the
			  * block comment above */

	return 0;
}


static void mqueue_handler(int id, void *data, void *arg)
{
	(void)arg;   /* unused: nothing here needs ctrl_st, see the "never
		      * touch ctrl_st from the reader thread" note above -
		      * kept out of this handler too for the same reason,
		      * even though it only ever runs on the main thread. */

	switch (id) {

	case MQ_LINE: {
		struct win_line *wl = data;

		process_line(wl->buf, wl->len);
		free(wl->buf);
		free(wl);
		break;
	}

	case MQ_EOF:
		info("ctrl_json: stdin closed, shutting down\n");
		ua_stop_all(false);
		break;

	default:
		break;
	}
}


static int stdin_start(struct ctrl_st *st)
{
	struct mqueue *mq_for_thread;

	if (mqueue_alloc(&st->mq, mqueue_handler, NULL))
		return ENOMEM;

	/* The reader thread gets its own reference, independent of
	 * ctrl_st/st->mq's lifetime from the main thread's point of view -
	 * see the big comment above stdin_thread_main(). */
	mq_for_thread = mem_ref(st->mq);

	if (thrd_create(&st->stdin_thr, stdin_thread_main, mq_for_thread) !=
	    thrd_success) {
		mem_deref(mq_for_thread);    /* the ref meant for the thread
					      * that never started */
		st->mq = mem_deref(st->mq);  /* our own */
		return ENOMEM;
	}

	return 0;
}


static void stdin_stop(struct ctrl_st *st)
{
	/* Deliberately not thrd_join()'d - see the block comment above
	 * stdin_thread_main(). Only drops *this* side's reference; if the
	 * thread is still blocked in fgets(), the mqueue/pipe stays alive
	 * via its own reference until the thread returns. */
	st->mq = mem_deref(st->mq);
}

#endif /* _WIN32 */


/* ------------------------------------------------------------------- */
/* Module init/close                                                    */

static void ctrl_destructor(void *arg)
{
	struct ctrl_st *st = arg;

	stdin_stop(st);
}


static int ctrl_init(void)
{
	int err;

	/*
	 * stdout is the JSON event channel. Under v1, this call was the
	 * *only* thing that ever turned off baresip's own human-readable
	 * stdout logger (lg.enable_stdout, default true - see src/log.c),
	 * and only from here on: ctrl_json is always the last module
	 * loaded (see BUILD.md "Module selection"), so every earlier
	 * module's own info()/debug() line during startup - account
	 * population, codec registration, network interface enumeration,
	 * ... - had already gone to stdout by the time this line ran.
	 *
	 * v1.1 (core/patches/0003-*, see BUILD.md/PROTOCOL.md) fixes this
	 * at the source instead: main.c now flips
	 * log_enable_stdout(false) immediately, before any config/module
	 * loading, whenever CENT_JSON_STDOUT is set in the environment
	 * (run-spike.sh always sets it) - and log.c now routes
	 * !enable_stdout output to *stderr* rather than dropping it
	 * silently (the v1 code path had no third option: stdout or
	 * nowhere - every info()/warning()/debug() call in this file
	 * itself, e.g. the warning() calls in blf_notify_handler() below,
	 * would otherwise have gone completely dark for the rest of the
	 * process the moment this line ran, not just quieted on stdout -
	 * confirmed by reading log.c's vlog() while investigating this).
	 *
	 * This call therefore stays, but is now typically a harmless no-op
	 * by the time it's reached (already false): it's still the *only*
	 * thing enforcing stdout purity when CENT_JSON_STDOUT is unset
	 * (e.g. someone builds this exact baresip tree standalone, outside
	 * run-spike.sh) - dropping it would silently regress that case
	 * back to v1's original module-load-noise-on-stdout behavior.
	 * debug()/DEBUG_* (re_dbg.h) output is unaffected either way -
	 * libre always sends that to stderr, never stdout, independent of
	 * this flag - see BUILD.md "Findings" for the SIP-trace log
	 * exception to that (also fixed by the same 0003 patch, in
	 * src/uag.c, not here).
	 */
	log_enable_stdout(false);

	ctrl = mem_zalloc(sizeof(*ctrl), ctrl_destructor);
	if (!ctrl)
		return ENOMEM;

	err = stdin_start(ctrl);
	if (err) {
		warning("ctrl_json: stdin listener setup failed (%m)\n", err);
		ctrl = mem_deref(ctrl);
		return err;
	}

	err = bevent_register(event_handler, ctrl);
	if (err) {
		ctrl = mem_deref(ctrl);
		return err;
	}

	/* v1.2: registers the audio-tap filter globally (attaches to every
	 * call from here on - see audiotap.h's own top comment for why
	 * that doesn't mean every call gets tapped). Deliberately last,
	 * after every fallible step above: aufilt_register() itself can't
	 * fail, but this ordering means it's a no-op invariant that
	 * audiotap_init() ran if and only if ctrl_init() is about to
	 * return success - so ctrl_close() (only ever called by baresip's
	 * module loader for a module that finished init()'ing successfully)
	 * calling audiotap_close() is always a matched pair, never a leak
	 * of a filter registration whose owning ctrl_init() call actually
	 * failed further down. */
	audiotap_init();

	/* Signal the controlling process that commands can now be sent. */
	emit_ready();

	return 0;
}


static int ctrl_close(void)
{
	bevent_unregister(event_handler);
	audiotap_close();
	list_flush(&blf_subs);
	xfer_reset();
	ctrl = mem_deref(ctrl);

	return 0;
}


EXPORT_SYM const struct mod_export DECL_EXPORTS(ctrl_json) = {
	"ctrl_json",
	"application",
	ctrl_init,
	ctrl_close
};

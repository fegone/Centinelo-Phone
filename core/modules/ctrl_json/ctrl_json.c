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
 * See core/PROTOCOL.md for the wire protocol (v1).
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


static void emit_error(const char *message)
{
	struct odict *od = NULL;

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

	case SIP_TRANSP_UDP: return "udp";
	case SIP_TRANSP_TCP: return "tcp";
	case SIP_TRANSP_TLS: return "tls";
	case SIP_TRANSP_WS:  return "ws";
	case SIP_TRANSP_WSS: return "wss";
	default:             return "udp";
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
	struct uri *aor;
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
	aor = account_luri(acc);
	if (!aor) {
		emit_error("blf_subscribe: could not resolve PBX host from"
			   " the account");
		return;
	}

	/* Target: same PBX host/port this account registered against,
	 * different user part - matches how run-spike.sh builds the
	 * account URI itself (CENT_EXT@CENT_HOST). */
	if (re_snprintf(uri, sizeof(uri), "sip:%s@%r", ext, &aor->host) < 0) {
		emit_error("blf_subscribe: uri too long");
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
 */
static void process_line(const char *line, size_t len)
{
	struct odict *od = NULL;
	struct cent_cmd cmd;
	enum cent_cmd_type type;
	const char *errmsg = NULL;
	char cmd_name[64] = "";

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
		const char *buf = (type == CENT_CMD_ANSWER) ? "accept" : "quit";
		int err;

		if (!resp)
			break;

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

	default:
		emit_error("internal error: unhandled command type");
		break;
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

	case BEVENT_CALL_CLOSED:
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
	 * stdout is the JSON event channel - baresip's own human-readable
	 * logger defaults lg.enable_stdout=true (see src/log.c) and is only
	 * ever turned off by main.c in daemon (-d) mode, which we don't use
	 * (daemonizing would fork/detach and sever the very stdio pipe this
	 * module depends on). Claim stdout exclusively for JSON here instead.
	 * debug()/re_dbg output is unaffected - libre always sends that to
	 * stderr, never stdout.
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

	/* Signal the controlling process that commands can now be sent. */
	emit_ready();

	return 0;
}


static int ctrl_close(void)
{
	bevent_unregister(event_handler);
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

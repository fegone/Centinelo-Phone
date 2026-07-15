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
 *     bevent -> JSON event relay (here: bevent -> our own compact v0
 *     schema instead of the generic ctrl_tcp wire format).
 *   - modules/stdio: polls STDIN_FILENO via fd_listen() instead of
 *     opening a socket (ctrl_tcp's approach) or a raw tty (stdio's
 *     approach - we deliberately stay in normal buffered/line mode, no
 *     tcsetattr, since our peer is a pipe, not a human at a keyboard).
 *
 * See core/PROTOCOL.md for the wire protocol (v0).
 *
 * Copyright (C) 2026 Neola Dental / Centinelo Phone
 */

#include <re.h>
#include <baresip.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>


/**
 * @defgroup ctrl_json ctrl_json
 *
 * Newline-delimited JSON control channel on stdin/stdout.
 *
 * Only one instance is supported (single baresip process per Centinelo
 * Phone spike/session), matching modules/ctrl_tcp's "one instance" rule.
 */


enum { INBUF_SIZE = 8192 };

struct ctrl_st {
	struct re_fhs *fhs;
	uint8_t inbuf[INBUF_SIZE];
	size_t  inlen;
};

static struct ctrl_st *ctrl = NULL;   /* allow only one instance */


static int print_handler(const char *p, size_t size, void *arg)
{
	struct mbuf *mb = arg;

	return mbuf_write_mem(mb, (const uint8_t *)p, size);
}


/* Serialize an odict as one compact JSON line and write it to stdout. */
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

	(void)write(STDOUT_FILENO, mb->buf, mb->end);
	(void)write(STDOUT_FILENO, "\n", 1);

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


static void emit_call_state(struct call *call, const char *state)
{
	struct odict *od = NULL;

	if (odict_alloc(&od, 8))
		return;

	(void)odict_entry_add(od, "event", ODICT_STRING, "call_state");
	(void)odict_entry_add(od, "state", ODICT_STRING, state);
	(void)odict_entry_add(od, "peer", ODICT_STRING,
			       call ? call_peeruri(call) : "");

	/* v0 addition: call-id, handy to correlate events for one call. */
	(void)odict_entry_add(od, "id", ODICT_STRING,
			       call ? call_id(call) : "");

	emit(od);
	mem_deref(od);
}


/*
 * Relay UA/call events -> v0 JSON events (see core/PROTOCOL.md).
 *
 * Deliberately narrower than ctrl_tcp's bevent_odict_encode() passthrough:
 * only the events named in the v0 schema are emitted. Everything else
 * (hold/resume/dtmf/transfer/rtcp/vu-meter/...) is silently ignored here
 * and left for a later protocol version (see PROTOCOL.md "planned").
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
		emit_call_state(call, "closed");
		break;

	case BEVENT_AUDIO_ERROR:
		emit_error(bevent_get_text(event));
		break;

	default:
		/* not part of the v0 schema - ignored on purpose */
		break;
	}
}


/*
 * Command dispatch: translate a decoded {"cmd":...} JSON line into a
 * baresip long-form command and hand it to the shared command processor
 * (the same mechanism modules/ctrl_tcp uses), so dial/answer/hangup/quit
 * reuse baresip's own (menu-module) call-control logic instead of
 * re-implementing UA/call selection here.
 */
static void process_line(const char *line, size_t len)
{
	struct odict *od = NULL;
	struct mbuf *resp = mbuf_alloc(256);
	struct re_printf pf = {print_handler, resp};
	const char *cmd, *uri;
	char buf[1024];
	int err;

	if (!resp)
		return;

	if (json_decode_odict(&od, 8, line, len, 8)) {
		emit_error("invalid JSON command line");
		goto out;
	}

	cmd = odict_string(od, "cmd");
	if (!cmd) {
		emit_error("missing 'cmd' field");
		goto out;
	}

	if (!str_casecmp(cmd, "dial")) {
		uri = odict_string(od, "uri");
		if (!uri) {
			emit_error("dial: missing 'uri' field");
			goto out;
		}
		(void)re_snprintf(buf, sizeof(buf), "dial %s", uri);
	}
	else if (!str_casecmp(cmd, "answer")) {
		str_ncpy(buf, "accept", sizeof(buf));
	}
	else if (!str_casecmp(cmd, "hangup")) {
		str_ncpy(buf, "hangup", sizeof(buf));
	}
	else if (!str_casecmp(cmd, "quit")) {
		str_ncpy(buf, "quit", sizeof(buf));
	}
	else {
		char m[128];
		(void)re_snprintf(m, sizeof(m), "unknown cmd '%s'", cmd);
		emit_error(m);
		goto out;
	}

	err = cmd_process_long(baresip_commands(), buf, str_len(buf),
				&pf, NULL);
	if (err) {
		char m[256];
		(void)re_snprintf(m, sizeof(m), "cmd '%s' failed (%m)",
				   cmd, err);
		emit_error(m);
	}

 out:
	mem_deref(resp);
	mem_deref(od);
}


/*
 * Split the accumulated stdin buffer on '\n' and process each complete
 * line. Partial trailing data (no newline yet) is kept for the next
 * read().
 */
static void process_inbuf(struct ctrl_st *st)
{
	size_t start = 0, i;

	for (i = 0; i < st->inlen; i++) {

		if (st->inbuf[i] != '\n')
			continue;

		if (i > start) {
			size_t len = i - start;

			/* tolerate CRLF line endings too */
			if (len > 0 && st->inbuf[start + len - 1] == '\r')
				--len;

			if (len > 0)
				process_line(
					(const char *)&st->inbuf[start],
					len);
		}

		start = i + 1;
	}

	if (start > 0) {
		st->inlen -= start;
		memmove(st->inbuf, &st->inbuf[start], st->inlen);
	}
}


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

	process_inbuf(st);
}


static void ctrl_destructor(void *arg)
{
	struct ctrl_st *st = arg;

	st->fhs = fd_close(st->fhs);
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

	err = fd_listen(&ctrl->fhs, STDIN_FILENO, FD_READ,
			stdin_handler, ctrl);
	if (err) {
		warning("ctrl_json: fd_listen(stdin) failed (%m)\n", err);
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
	ctrl = mem_deref(ctrl);

	return 0;
}


EXPORT_SYM const struct mod_export DECL_EXPORTS(ctrl_json) = {
	"ctrl_json",
	"application",
	ctrl_init,
	ctrl_close
};

/**
 * @file audiotap.c  Centinelo Phone v2 - per-call audio tap (F4 foundation)
 *
 * See audiotap.h. Baresip-dependent (aufilt registration, struct
 * call/audio, the mem_ and list_ APIs) - unlike wav_writer.c, this isn't
 * unit tested standalone; core/E2E-F1.md "F4 audio tap" is its test coverage,
 * matching how ctrl_json.c's own call-control commands are e2e-verified
 * rather than unit tested - see cmd.h's top comment for that same split
 * applied to the JSON-decode side of this feature).
 *
 * Two independent lifetimes worth being explicit about, since they're
 * easy to conflate at a glance:
 *
 *   1. The aufilt per-call filter state (struct audiotap_enc/_dec below)
 *      - one instance per call, per direction, allocated by
 *      encode_update()/decode_update() the first time baresip sets up
 *      that call's audio filter chain (see src/audio.c aufilt_setup()),
 *      freed automatically (via baresip's own mem_deref graph) whenever
 *      that call's audio pipeline tears down. This engine has no control
 *      over exactly when that first setup happens relative to
 *      tap_start/tap_stop traffic - it's baresip's own timing - so this
 *      state is attached to *every* call unconditionally (cheap: a
 *      handful of bytes, no I/O, no file ever opened here) and never
 *      itself decides whether to write anything.
 *
 *   2. The tap registry (struct audiotap_reg, audiotap_regl) - one
 *      instance per call *that has an active or just-finished tap*,
 *      entirely owned by audiotap_start()/audiotap_stop()/
 *      audiotap_call_closed(). This is where the real state lives: the
 *      two wav_writer instances, the paths, the byte counts.
 *
 * The two are joined, per-frame, by a single fresh lookup
 * (find_reg(au) - a linear search of audiotap_regl by the struct audio*
 * pointer baresip hands both sides) rather than a cached pointer from
 * one into the other. This is deliberate, not just simplicity: baresip's
 * event loop is single-threaded (re_main() - see PROTOCOL.md "Framing"
 * and ctrl_json.c's own top comment), so there's no actual *race*
 * between a tap_stop freeing a struct audiotap_reg and an in-flight
 * encode()/decode() call using it - but a fresh lookup means there's
 * also no *class* of dangling-pointer bug to reason about here at all,
 * which is worth the linear search's cost (trivially cheap - this
 * engine registers one UA and this build's e2e testing never exceeds a
 * handful of concurrent calls, see PROTOCOL.md's attended-transfer
 * scenario for the realistic ceiling).
 *
 * Copyright (C) 2026 Neola Dental / Centinelo Phone
 */

#include <re.h>
#include <rem.h>
#include <baresip.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include "audiotap.h"
#include "cmd.h"          /* CENT_ID_SIZE - reuse the same call_id sizing
			    * the rest of this protocol already uses */
#include "wav_writer.h"


enum {
	/* This build's actual audio path (see run-spike.sh's account
	 * `audio_codecs=pcmu,pcma` - G.711, always 8000 Hz mono - see
	 * core/BUILD.md "Module selection"). Only ever used by
	 * finalize_reg() as wav_writer_close()'s fallback: a direction
	 * that saw zero real frames (header never committed with a real
	 * srate) still needs *some* value to leave a syntactically valid
	 * WAV rather than a headerless stub - see wav_writer.h. A future
	 * build with a different codec (e.g. opus) would still produce a
	 * *correct* header for any tap that actually saw audio (real
	 * frames always carry their own true af->srate - see
	 * write_frame()); this constant only ever affects the all-silence
	 * edge case's header, never real data.
	 */
	AUDIOTAP_FALLBACK_SRATE = 8000,
};


/* ------------------------------------------------------------------- */
/* Tap registry - see this file's own top comment for the two-lifetimes
 * split this and the aufilt per-call state below are each one half of. */

struct audiotap_reg {
	struct le le;                 /* member of audiotap_regl */
	struct audio *au;              /* identity key - borrowed, never
					 * dereferenced as a live object,
					 * only ever pointer-compared (see
					 * find_reg()) */
	char call_id[CENT_ID_SIZE];
	bool active;                    /* between audiotap_start() and
					 * audiotap_stop()/
					 * audiotap_call_closed()/
					 * audiotap_close() */

	struct wav_writer rx_w;         /* decode direction - remote party */
	struct wav_writer tx_w;         /* encode direction - local party */
	char rx_path[AUDIOTAP_PATH_SIZE];
	char tx_path[AUDIOTAP_PATH_SIZE];

	/* Per-tap scratch buffers for write_frame()'s format-conversion/
	 * downmix slow path (see that function) - grown on demand, freed
	 * in reg_destructor(). Kept per-registry-entry (not, say, static/
	 * global) so two concurrent taps (attended transfer) never share
	 * or fight over one buffer. */
	int16_t *rx_scratch;
	size_t   rx_scratch_cap;
	int16_t *tx_scratch;
	size_t   tx_scratch_cap;
};

static struct list audiotap_regl;


static struct audiotap_reg *find_reg(const struct audio *au)
{
	struct le *le;

	for (le = list_head(&audiotap_regl); le; le = le->next) {
		struct audiotap_reg *r = le->data;

		if (r->au == au)
			return r;
	}

	return NULL;
}


static void reg_destructor(void *arg)
{
	struct audiotap_reg *r = arg;

	/* Idempotent (see wav_writer_close()) - the common case is these
	 * are already closed by the time this runs (audiotap_stop()/
	 * audiotap_call_closed() both finalize *before* mem_deref()'ing
	 * the entry); the case this destructor call is the *first* close
	 * for is audiotap_close()'s process-shutdown sweep (a tap that
	 * was still active when `quit` tore everything down at once). */
	(void)wav_writer_close(&r->rx_w, AUDIOTAP_FALLBACK_SRATE);
	(void)wav_writer_close(&r->tx_w, AUDIOTAP_FALLBACK_SRATE);

	free(r->rx_scratch);
	free(r->tx_scratch);

	list_unlink(&r->le);
}


static void path_build(char *out, size_t out_sz, const char *dir,
			const char *call_id, const char *suffix)
{
	(void)re_snprintf(out, out_sz, "%s/%s-%s.wav", dir, call_id, suffix);
}


/*
 * Finalizes both writers of an active entry and fills *res - shared by
 * audiotap_stop() and audiotap_call_closed(), which differ only in how
 * they find `r` and whether "no active tap" is an error or a silent
 * no-op. Does NOT free/unlink `r` - both callers mem_deref() it
 * themselves right after, once (see reg_destructor() for what that
 * triggers).
 */
static void finalize_reg(struct audiotap_reg *r, struct audiotap_result *res)
{
	(void)wav_writer_close(&r->rx_w, AUDIOTAP_FALLBACK_SRATE);
	(void)wav_writer_close(&r->tx_w, AUDIOTAP_FALLBACK_SRATE);

	str_ncpy(res->rx_path, r->rx_path, sizeof(res->rx_path));
	str_ncpy(res->tx_path, r->tx_path, sizeof(res->tx_path));
	res->rx_bytes = wav_writer_bytes(&r->rx_w);
	res->tx_bytes = wav_writer_bytes(&r->tx_w);

	/* duration_ms = data_bytes / (srate * bytes_per_sample) * 1000,
	 * integer math (rounds down) - mono/16-bit throughout, matching
	 * this writer's own fixed output format (see wav_writer.c). r->
	 * {rx,tx}_w.srate is always valid post-close() - wav_writer_close()
	 * itself commits a (fallback-srate) header first if one was never
	 * written, so there's no "0 frames ever arrived" case left to
	 * special-case here; srate is only genuinely 0 if `r` was somehow
	 * never opened at all, which audiotap_start() never leaves this
	 * registry entry in (see that function - it fails and frees `r`
	 * before list_append() if either wav_writer_create() fails). */
	res->rx_duration_ms = r->rx_w.srate ?
		(uint32_t)(((uint64_t)res->rx_bytes * 1000) /
			   (r->rx_w.srate * (uint32_t)sizeof(int16_t))) : 0;
	res->tx_duration_ms = r->tx_w.srate ?
		(uint32_t)(((uint64_t)res->tx_bytes * 1000) /
			   (r->tx_w.srate * (uint32_t)sizeof(int16_t))) : 0;

	r->active = false;
}


/* ------------------------------------------------------------------- */
/* aufilt - per-call filter state. See this file's own top comment for
 * why these never touch audiotap_regl beyond find_reg()'s read-only
 * lookup, and always attach/succeed unconditionally (sndfile.c's own
 * encode_update()/decode_update() do too - the "should this call be
 * tapped" decision lives entirely in encode()/decode() below, per
 * frame, not here). */

struct audiotap_enc {
	struct aufilt_enc_st af;   /* base class - MUST be first (every
				     * aufilt module relies on this same
				     * cast, e.g. sndfile.c's sndfile_enc) */
	struct audio *au;
};

struct audiotap_dec {
	struct aufilt_dec_st af;   /* base class - MUST be first */
	struct audio *au;
};


static void enc_destructor(void *arg)
{
	struct audiotap_enc *st = arg;

	list_unlink(&st->af.le);
}


static void dec_destructor(void *arg)
{
	struct audiotap_dec *st = arg;

	list_unlink(&st->af.le);
}


static int encode_update(struct aufilt_enc_st **stp, void **ctx,
			  const struct aufilt *af, struct aufilt_prm *prm,
			  const struct audio *au)
{
	struct audiotap_enc *st;
	(void)ctx;
	(void)af;
	(void)prm;

	if (!stp || !au)
		return EINVAL;

	st = mem_zalloc(sizeof(*st), enc_destructor);
	if (!st)
		return ENOMEM;

	/* `au` is only ever used as an opaque identity key from here on
	 * (pointer comparison in find_reg()), never dereferenced - the
	 * cast away from aufilt_encupd_h's `const` just matches
	 * audiotap_reg::au / call_audio()'s own non-const return type at
	 * the audiotap_start() call site, so find_reg()'s single pointer-
	 * equality check compares like types both directions. */
	st->au = (struct audio *)au;
	*stp   = (struct aufilt_enc_st *)st;

	return 0;
}


static int decode_update(struct aufilt_dec_st **stp, void **ctx,
			  const struct aufilt *af, struct aufilt_prm *prm,
			  const struct audio *au)
{
	struct audiotap_dec *st;
	(void)ctx;
	(void)af;
	(void)prm;

	if (!stp || !au)
		return EINVAL;

	st = mem_zalloc(sizeof(*st), dec_destructor);
	if (!st)
		return ENOMEM;

	st->au = (struct audio *)au;
	*stp   = (struct aufilt_dec_st *)st;

	return 0;
}


/*
 * Converts one audio frame to mono S16LE and writes it to `w`. Fast path
 * (af->fmt == AUFMT_S16LE && af->ch == 1): zero-copy, straight into
 * wav_writer_write() - this is the *only* path this engine's actual e2e
 * testing exercises (G.711 decodes to mono S16LE - see
 * AUDIOTAP_FALLBACK_SRATE's own comment). The slow path (anything else -
 * a different codec, or a future stereo device) converts into `*scratch`
 * (grown on demand, owned by the caller's struct audiotap_reg) via
 * re/rem's own auconv_to_s16() - not a new dependency, already part of
 * libre this engine links unconditionally - then downmixes to mono with
 * a simple integer average, clamped by construction (an average of N
 * int16 values, accumulated in int32, can never itself overflow int16).
 *
 * `scratch` layout when the slow path runs: the first af->sampc int16s
 * hold the S16LE-converted (still multi-channel) samples, the next
 * af->sampc/af->ch hold the downmixed mono result - one allocation
 * covers both stages.
 */
static int write_frame(struct wav_writer *w, const struct auframe *af,
			int16_t **scratch, size_t *scratch_cap)
{
	size_t frames, need, i, c;
	int16_t *s16, *mono;

	if (!af || !af->sampc || !af->sampv)
		return 0;

	if (af->fmt == AUFMT_S16LE && af->ch == 1)
		return wav_writer_write(w, af->srate, af->sampv, af->sampc);

	if (!af->ch)
		return 0;   /* malformed frame - ignore, never crash the
			     * call over a tap-side format surprise */

	frames = af->sampc / af->ch;
	need   = af->sampc + frames;

	if (*scratch_cap < need) {
		int16_t *grown = realloc(*scratch, need * sizeof(int16_t));

		if (!grown)
			return ENOMEM;

		*scratch     = grown;
		*scratch_cap = need;
	}

	s16  = *scratch;
	mono = *scratch + af->sampc;

	if (af->fmt == AUFMT_S16LE)
		memcpy(s16, af->sampv, af->sampc * sizeof(int16_t));
	else
		auconv_to_s16(s16, af->fmt, af->sampv, af->sampc);

	if (af->ch == 1) {
		mono = s16;   /* already mono post-conversion - skip the
			       * downmix pass entirely */
	}
	else {
		for (i = 0; i < frames; i++) {
			int32_t sum = 0;

			for (c = 0; c < af->ch; c++)
				sum += s16[i * af->ch + c];

			mono[i] = (int16_t)(sum / (int32_t)af->ch);
		}
	}

	return wav_writer_write(w, af->srate, mono, frames);
}


static int encode(struct aufilt_enc_st *stp, struct auframe *af)
{
	struct audiotap_enc *st = (struct audiotap_enc *)stp;
	struct audiotap_reg *r;
	bool was_ok;

	if (!st || !af)
		return EINVAL;

	r = find_reg(st->au);
	if (!r || !r->active)
		return 0;   /* no tap requested for this call right now -
			     * passthrough, exactly like every frame before
			     * the first tap_start / after a tap_stop */

	was_ok = !r->tx_w.err;

	(void)write_frame(&r->tx_w, af, &r->tx_scratch, &r->tx_scratch_cap);

	if (was_ok && r->tx_w.err) {
		warning("audiotap: tx (local/encode) write failed for call"
			" %s (%m) - tap continues capturing rx only\n",
			r->call_id, r->tx_w.err);
	}

	/* Always 0: a tap write failure (e.g. disk full) must never
	 * disrupt the actual phone call - see this file's own top comment
	 * and wav_writer's sticky ->err (one warning() above, then silent
	 * for the rest of this call, no repeated failing I/O every
	 * frame). */
	return 0;
}


static int decode(struct aufilt_dec_st *stp, struct auframe *af)
{
	struct audiotap_dec *st = (struct audiotap_dec *)stp;
	struct audiotap_reg *r;
	bool was_ok;

	if (!st || !af)
		return EINVAL;

	r = find_reg(st->au);
	if (!r || !r->active)
		return 0;

	was_ok = !r->rx_w.err;

	(void)write_frame(&r->rx_w, af, &r->rx_scratch, &r->rx_scratch_cap);

	if (was_ok && r->rx_w.err) {
		warning("audiotap: rx (remote/decode) write failed for call"
			" %s (%m) - tap continues capturing tx only\n",
			r->call_id, r->rx_w.err);
	}

	return 0;
}


static struct aufilt audiotap_filt = {
	.name    = "audiotap",
	.encupdh = encode_update,
	.ench    = encode,
	.decupdh = decode_update,
	.dech    = decode,
};


void audiotap_init(void)
{
	aufilt_register(baresip_aufiltl(), &audiotap_filt);
}


void audiotap_close(void)
{
	aufilt_unregister(&audiotap_filt);

	/* Frees every remaining entry via reg_destructor() (list_flush()
	 * mem_deref()s each - see core/deps/re/src/list/list.c), which
	 * force-finalizes (idempotently - see that destructor's own
	 * comment) any tap that was still active - "never leave a corrupt
	 * WAV" holds even for `quit` racing an unfinished tap_stop. */
	list_flush(&audiotap_regl);
}


int audiotap_start(struct call *call, const char *dir,
		    struct audiotap_result *res, const char **errmsg)
{
	static const char *e_no_call  = "tap_start: call not found";
	static const char *e_no_audio = "tap_start: call has no audio";
	static const char *e_no_dir   = "tap_start: missing 'dir'";
	static const char *e_running  =
		"tap_start: tap already running for this call";
	static const char *e_open     =
		"tap_start: could not open output file(s)"
		" (bad 'dir'? not writable?)";
	static const char *e_nomem    = "tap_start: out of memory";
	struct audio *au;
	struct audiotap_reg *r;
	const char *cid;

	if (!errmsg)
		return EINVAL;
	*errmsg = NULL;

	if (!res)
		return EINVAL;
	memset(res, 0, sizeof(*res));

	if (!call) {
		*errmsg = e_no_call;
		return ENOENT;
	}

	if (!dir || !dir[0]) {
		*errmsg = e_no_dir;
		return EINVAL;
	}

	au = call_audio(call);
	if (!au) {
		*errmsg = e_no_audio;
		return EINVAL;
	}

	r = find_reg(au);
	if (r && r->active) {
		*errmsg = e_running;
		return EEXIST;
	}
	if (r) {
		/* Stale, already-inactive entry from a prior tap on this
		 * same call (tap_start -> tap_stop -> tap_start again) -
		 * drop it; a fresh tap gets a fresh registry entry. This
		 * path is never hot (at most once per call). */
		mem_deref(r);
	}

	r = mem_zalloc(sizeof(*r), reg_destructor);
	if (!r) {
		*errmsg = e_nomem;
		return ENOMEM;
	}

	r->au = au;
	cid = call_id(call);
	str_ncpy(r->call_id, cid ? cid : "", sizeof(r->call_id));

	path_build(r->rx_path, sizeof(r->rx_path), dir, r->call_id, "rx");
	path_build(r->tx_path, sizeof(r->tx_path), dir, r->call_id, "tx");

	if (wav_writer_create(&r->rx_w, r->rx_path) ||
	    wav_writer_create(&r->tx_w, r->tx_path)) {
		*errmsg = e_open;
		mem_deref(r);
		return EIO;
	}

	r->active = true;
	list_append(&audiotap_regl, &r->le, r);

	str_ncpy(res->rx_path, r->rx_path, sizeof(res->rx_path));
	str_ncpy(res->tx_path, r->tx_path, sizeof(res->tx_path));

	return 0;
}


int audiotap_stop(struct call *call, struct audiotap_result *res,
		   const char **errmsg)
{
	static const char *e_no_call     = "tap_stop: call not found";
	static const char *e_no_audio    = "tap_stop: call has no audio";
	static const char *e_not_running =
		"tap_stop: no tap running for this call";
	struct audio *au;
	struct audiotap_reg *r;

	if (!errmsg)
		return EINVAL;
	*errmsg = NULL;

	if (!res)
		return EINVAL;
	memset(res, 0, sizeof(*res));

	if (!call) {
		*errmsg = e_no_call;
		return ENOENT;
	}

	au = call_audio(call);
	if (!au) {
		*errmsg = e_no_audio;
		return EINVAL;
	}

	r = find_reg(au);
	if (!r || !r->active) {
		*errmsg = e_not_running;
		return ENOENT;
	}

	finalize_reg(r, res);
	mem_deref(r);   /* -> reg_destructor(): idempotent re-close (no-op,
			 * already closed by finalize_reg() above) + unlink +
			 * scratch-buffer free */

	return 0;
}


bool audiotap_call_closed(struct call *call, struct audiotap_result *res)
{
	struct audio *au;
	struct audiotap_reg *r;

	if (!call || !res)
		return false;

	au = call_audio(call);
	if (!au)
		return false;

	r = find_reg(au);
	if (!r || !r->active)
		return false;

	memset(res, 0, sizeof(*res));
	finalize_reg(r, res);
	mem_deref(r);

	return true;
}

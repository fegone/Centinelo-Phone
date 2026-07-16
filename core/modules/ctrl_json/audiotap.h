/**
 * @file audiotap.h  Centinelo Phone v2 - per-call audio tap (F4 foundation)
 *
 * Taps a call's audio to two mono 16-bit PCM WAV files, one per
 * direction - decode (the remote party's decoded-from-the-wire voice)
 * and encode (the local party's mic audio, pre-encode) - so a future
 * local-transcription pipeline (see PROTOCOL.md v1.2 changelog) gets
 * 2-speaker diarization for free: each side was already a separate
 * stream by construction, no speaker-separation model needed. Driven by
 * ctrl_json.c's tap_start/tap_stop commands (see PROTOCOL.md); this file
 * owns the baresip aufilt plumbing and the per-call registry, not the
 * JSON decode/emit side (see cmd.h/cmd.c for decode, ctrl_json.c's
 * emit_tap_state() for the wire event this drives).
 *
 * Adapted from baresip's own modules/sndfile/sndfile.c (an audio-dumping
 * aufilt module) for the actual encode/decode-frame plumbing, but with
 * three deliberate differences worth calling out up front (see
 * audiotap.c's own top comment for the full reasoning on each):
 *   1. sndfile is config-file-driven and always-on for every call from
 *      process start; this is *command*-driven (tap_start/tap_stop can
 *      fire, or not, at any point in a call's life) - so the aufilt
 *      itself is still attached to every call unconditionally (cheap,
 *      like sndfile), but a *separate* per-call registry (audiotap_regl
 *      in audiotap.c), owned entirely by tap_start/tap_stop/
 *      auto-finalize, decides whether each frame actually gets written
 *      anywhere.
 *   2. sndfile links libsndfile; this hand-rolls its own WAV writer (see
 *      wav_writer.h) - no new external dependency, per the F4 task
 *      design.
 *   3. sndfile writes exactly the source frame's own channel count;
 *      this always writes mono (downmixing if the source ever isn't -
 *      see audiotap.c's write_frame()), per the F4 task design ("writes
 *      TWO mono ... WAV files").
 *
 * Copyright (C) 2026 Neola Dental / Centinelo Phone
 */

#ifndef CENTINELO_CTRL_JSON_AUDIOTAP_H
#define CENTINELO_CTRL_JSON_AUDIOTAP_H

#include <stdbool.h>
#include <stdint.h>

struct call;

enum {
	/* dir (CENT_DIR_SIZE=512, see cmd.h) + "/" + call_id
	 * (CENT_ID_SIZE=128) + "-rx.wav"/"-tx.wav" (8) + slack - see
	 * path_build() in audiotap.c for the exact format string. */
	AUDIOTAP_PATH_SIZE = 700,
};

/** Outcome of a start/stop/auto-finalize - see this file's functions
 *  below for which fields are meaningful when (audiotap_start() only
 *  ever fills the two paths; byte/duration counts are always 0 there -
 *  nothing has been written yet). */
struct audiotap_result {
	char rx_path[AUDIOTAP_PATH_SIZE];
	char tx_path[AUDIOTAP_PATH_SIZE];
	uint32_t rx_bytes;          /**< PCM data bytes, WAV header excluded */
	uint32_t tx_bytes;
	uint32_t rx_duration_ms;
	uint32_t tx_duration_ms;
};

/** Registers the tap aufilt globally (attaches to every call's audio
 *  pipeline from here on - see this file's own top comment for why that
 *  doesn't mean every call gets tapped). Call once, from ctrl_init(). */
void audiotap_init(void);

/** Unregisters the aufilt and force-finalizes (cleanly closes, patching
 *  final WAV headers on) every tap still active across every call,
 *  however many there are - belt-and-suspenders for process shutdown
 *  (`quit`) racing an unfinished tap_stop/hangup, so "never leave a
 *  corrupt WAV" holds even then. Call once, from ctrl_close(). */
void audiotap_close(void);

/**
 * Starts tapping `call`'s audio to two new WAV files under `dir` (an
 * absolute, already-existing, writable directory - this function
 * doesn't create it, matching how every other path-shaped input in this
 * protocol works). The files exist on disk (0 bytes) as soon as this
 * returns 0; each one's real WAV header is committed on that
 * direction's first actual audio frame (see wav_writer.h) - typically
 * within one ptime interval (~20ms in this build) of an already-flowing
 * call, but not synchronously with this call returning.
 *
 * @param call   Already-resolved target call (see ctrl_json.c
 *               resolve_call() - this function itself does no call_id
 *               lookup, matching every other cmd_*() in that file).
 * @param dir    Absolute directory to write into. Required - NULL/empty
 *               fails (mirrors cmd.c's own "dir" field validation,
 *               which is actually the *first* line of defense - this
 *               check only matters for a caller of this function that
 *               isn't ctrl_json.c's cmd_tap_start(), e.g. a future unit
 *               test).
 * @param res    Zeroed, then filled with the two paths on success.
 * @param errmsg Set to a static, human-readable string on failure.
 *
 * @return 0 on success, an errno-style code otherwise: ENOENT (`call` is
 *         NULL), EINVAL (`call` has no audio yet, or `dir` is missing),
 *         EEXIST (a tap is already running for this call - stop it
 *         first), EIO (couldn't open the output file(s) - bad `dir`?
 *         not writable?), ENOMEM.
 */
int audiotap_start(struct call *call, const char *dir,
		    struct audiotap_result *res, const char **errmsg);

/**
 * Stops a running tap, finalizing both WAV headers (correct final
 * RIFF/data chunk sizes).
 *
 * @return 0 on success (`res` filled with final paths + byte/duration
 *         counts), an errno-style code otherwise: ENOENT (`call` is
 *         NULL, or no tap is currently running for it), EINVAL (`call`
 *         has no audio).
 */
int audiotap_stop(struct call *call, struct audiotap_result *res,
		   const char **errmsg);

/**
 * Auto-finalize hook for BEVENT_CALL_CLOSED (see ctrl_json.c
 * event_handler()) - "on ... call-end" from the F4 task design, so a
 * tap never outlives the call it was tapping even if nobody sent
 * tap_stop first. Silently does nothing (returns false, `res`
 * untouched) if `call` has no active tap - most calls don't have one,
 * and that's not an error, unlike audiotap_stop()'s explicit-command
 * version of the same "no tap running" case.
 *
 * @return true (with `res` filled in exactly like audiotap_stop()) if a
 *         tap was in fact running and just got finalized - so the
 *         caller (ctrl_json.c) knows whether to also emit a tap_state
 *         "stopped" event.
 */
bool audiotap_call_closed(struct call *call, struct audiotap_result *res);

#endif /* CENTINELO_CTRL_JSON_AUDIOTAP_H */

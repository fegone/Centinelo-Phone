/**
 * @file wav_writer.h  Centinelo Phone v2 - minimal streaming mono 16-bit
 *                     PCM WAV writer
 *
 * Deliberately has zero dependency on baresip.h / re.h / the SIP stack -
 * only the C standard library (<stdio.h>/<stdint.h>) - so it links into
 * the same small standalone test binary cmd.c/dialog_info.c already do
 * (see cmd.h's own top comment for why that split exists) and is unit
 * tested without a running engine. audiotap.c (baresip-dependent: the
 * aufilt plumbing, the per-call registry, sample-format conversion) is
 * the only real caller - see that file for how a call's audio frames
 * become calls into this one.
 *
 * No new external dependency (no libsndfile, per the F4 task design) -
 * a canonical 44-byte PCM WAVE header is small enough to hand-roll
 * correctly, and doing so keeps this engine's dependency footprint
 * exactly what it was before F4.
 *
 * Two-phase, streaming: wav_writer_create() opens the file on disk
 * *immediately* (0 bytes, no header yet) - so a caller asking "does the
 * tap's output file exist" right after a successful tap_start gets a
 * real answer without waiting on the first audio frame. The header
 * itself (which needs a real sample rate) is committed lazily, on the
 * first wav_writer_write() call, using *that* call's own `srate` - never
 * a guessed/pre-negotiated value - the same deliberate choice baresip's
 * own modules/sndfile/sndfile.c reference module makes (see that file's
 * encode()/decode(): they open their SNDFILE* there, not in
 * encode_update()/decode_update(), for the same reason). If a writer is
 * closed having never seen a single frame (e.g. a tap started right
 * before the call died), wav_writer_close() still commits a minimal
 * valid (silent, 0-byte data) header using its `fallback_srate`
 * argument, rather than leaving a headerless stub - see that function's
 * own comment.
 *
 * All I/O is plain buffered <stdio.h> FILE* (fopen/fwrite/fseek/fclose) -
 * portable C99, no POSIX-only calls, no _WIN32 branch needed anywhere in
 * this pair of files (see core/CLAUDE.md-adjacent workspace rule "C99,
 * no POSIX-only calls without _WIN32 alternatives" - this module simply
 * never needs one). Writes are buffered by the C library as normal; this
 * file never calls fflush()/fsync() per frame (see wav_writer_write()) -
 * only once, at wav_writer_close() - per the F4 task design ("buffer
 * writes, don't fsync per frame").
 *
 * Copyright (C) 2026 Centinelo Phone
 */

#ifndef CENTINELO_CTRL_JSON_WAV_WRITER_H
#define CENTINELO_CTRL_JSON_WAV_WRITER_H

#include <stdint.h>
#include <stdio.h>

/**
 * Streaming mono 16-bit PCM WAV writer. Plain public struct (like this
 * module's cmd.h's own struct cent_cmd) - callers may read (never write)
 * ->srate / ->data_bytes / ->err after an operation; only ->fp is
 * "internal" (by convention, not enforcement).
 */
struct wav_writer {
	FILE *fp;               /**< NULL when not open - see
				   * wav_writer_close()'s idempotence and
				   * wav_writer_create()'s zero-init. */
	int header_written;     /**< the 44-byte header has been committed
				   * to `fp` (see wav_writer_write()). */
	uint32_t srate;          /**< sample rate the committed header
				   * claims - valid only once
				   * header_written is set. */
	uint32_t data_bytes;     /**< running total of PCM data bytes
				   * written so far (header excluded) -
				   * valid regardless of header_written
				   * (0 either way if it isn't). */
	int err;                 /**< sticky first I/O error (errno-style),
				   * 0 if none yet - see wav_writer_write():
				   * once set, every subsequent write is a
				   * cheap no-op that keeps returning it,
				   * rather than retrying failing I/O every
				   * single audio frame. */
};

/**
 * Opens `path` for writing, truncating any existing file, immediately -
 * `*w` is zeroed first, so this doubles as the writer's initializer (no
 * separate "init" step). The path exists on disk (0 bytes) as soon as
 * this returns 0; no header is written yet (see this file's own top
 * comment for why that's deferred).
 *
 * @return 0 on success, an errno-style code (e.g. ENOENT/EACCES from the
 *         underlying fopen()) otherwise. EINVAL for a NULL/empty `path`
 *         or NULL `w`.
 */
int wav_writer_create(struct wav_writer *w, const char *path);

/**
 * Appends `sampc` mono int16 samples (`sampc * 2` raw bytes, host/little-
 * endian - correct as-is for a WAV file on every platform this engine
 * targets, macOS arm64/x86_64 and Windows x86_64, all little-endian - no
 * byte-swap needed). Commits the 44-byte header first if this is the
 * first call on this writer, using `srate` - see this file's own top
 * comment. A writer that already hit a sticky error (->err) or was never
 * successfully create()'d is a no-op that keeps returning that same
 * error, not a re-attempt.
 *
 * @return 0 on success, an errno-style code otherwise (also stored in
 *         ->err - see struct wav_writer).
 */
int wav_writer_write(struct wav_writer *w, uint32_t srate,
		      const int16_t *sampv, size_t sampc);

/**
 * Finalizes: patches the RIFF chunk size and data chunk size fields to
 * their real final values (see build_header() in wav_writer.c - same
 * field layout, just re-written with the true `data_bytes` instead of
 * the 0 wav_writer_write() committed at header time), flushes, closes.
 *
 * If no frame was ever written (header never committed - e.g. a tap that
 * started and was immediately stopped/orphaned by a call that never
 * produced audio), commits a header now using `fallback_srate` first, so
 * the file left behind is always a syntactically valid WAV - silent (0
 * data bytes) in that case, never a headerless stub. See
 * audiotap.c/AUDIOTAP_FALLBACK_SRATE for what this engine actually
 * passes and why.
 *
 * Idempotent: a second (or Nth) call on an already-closed writer, or a
 * call on a writer that was never create()'d at all (an all-zero
 * struct), is a safe no-op that returns 0 - see core/E2E-F1.md "F4 audio
 * tap" / the unit tests in test/test_main.c for exactly what this
 * covers (tap_stop followed by the same call's auto-finalize-on-hangup
 * hitting the same writer a second time is the real scenario this
 * matters for - see audiotap.c's own comment on that path).
 *
 * @return 0 on success (including every no-op case above), an
 *         errno-style code if the final header patch itself fails
 *         (rare - the file is still closed either way, best-effort).
 */
int wav_writer_close(struct wav_writer *w, uint32_t fallback_srate);

/**
 * @return Total PCM data bytes written so far (0 if never begun or `w`
 *         is NULL) - valid before *or* after close().
 */
uint32_t wav_writer_bytes(const struct wav_writer *w);

#endif /* CENTINELO_CTRL_JSON_WAV_WRITER_H */

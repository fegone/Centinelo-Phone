/**
 * @file wav_writer.c  Centinelo Phone v2 - minimal streaming mono 16-bit
 *                     PCM WAV writer
 *
 * See wav_writer.h. Pure C99 stdio, no re/baresip - so
 * core/modules/ctrl_json/test/test_main.c can exercise this without a
 * running baresip engine (same reasoning as cmd.c/dialog_info.c).
 *
 * Copyright (C) 2026 Centinelo Phone
 */

#include <errno.h>
#include <string.h>
#include "wav_writer.h"

enum {
	WAV_HEADER_SIZE      = 44,
	WAV_CHANNELS         = 1,   /* tap output is always mono - see
				      * audiotap.c's downmix comment; this
				      * writer itself has no opinion on the
				      * *source* channel count, only on what
				      * it writes out. */
	WAV_BITS_PER_SAMPLE  = 16,
	WAV_BYTES_PER_SAMPLE = WAV_BITS_PER_SAMPLE / 8,
};


static void put_u32le(uint8_t *p, uint32_t v)
{
	p[0] = (uint8_t)(v);
	p[1] = (uint8_t)(v >> 8);
	p[2] = (uint8_t)(v >> 16);
	p[3] = (uint8_t)(v >> 24);
}


static void put_u16le(uint8_t *p, uint16_t v)
{
	p[0] = (uint8_t)(v);
	p[1] = (uint8_t)(v >> 8);
}


/*
 * Builds the canonical 44-byte canonical PCM WAVE header (RIFF/WAVE,
 * one "fmt " sub-chunk of 16 bytes describing linear PCM, one "data"
 * sub-chunk) into `buf`. Explicit little-endian field writes throughout
 * (put_u32le()/put_u16le()) rather than casting `buf` to a packed struct
 * and writing through it directly - avoids any dependency on the host
 * compiler's struct-packing/alignment/endianness behavior (this is the
 * one function in this file where getting a byte offset wrong would
 * silently produce a WAV file every real player rejects, so it's worth
 * the few extra lines of explicit byte-at-a-time writes over a
 * `#pragma pack` struct that "should" work).
 *
 * `data_bytes` is the *current* data sub-chunk size - called with 0 by
 * wav_writer_write() (header committed before any samples are known)
 * and again with the true final size by wav_writer_close() (same byte
 * offsets, just re-written) - exactly one place knows this layout.
 */
static void build_header(uint8_t buf[WAV_HEADER_SIZE], uint32_t srate,
			  uint32_t data_bytes)
{
	uint32_t byte_rate   = srate * WAV_CHANNELS * WAV_BYTES_PER_SAMPLE;
	uint16_t block_align = (uint16_t)(WAV_CHANNELS * WAV_BYTES_PER_SAMPLE);

	memcpy(buf + 0, "RIFF", 4);
	put_u32le(buf + 4, 36 + data_bytes);    /* RIFF chunk size:
						  * everything after this
						  * field = 4 ("WAVE") + 24
						  * ("fmt " sub-chunk incl.
						  * its own header) + 8
						  * ("data" sub-chunk header)
						  * + data_bytes = 36 +
						  * data_bytes. */
	memcpy(buf + 8, "WAVE", 4);

	memcpy(buf + 12, "fmt ", 4);
	put_u32le(buf + 16, 16);                 /* fmt sub-chunk size (16
						   * = PCM, no extra fields) */
	put_u16le(buf + 20, 1);                  /* audio format: 1 = PCM */
	put_u16le(buf + 22, WAV_CHANNELS);
	put_u32le(buf + 24, srate);
	put_u32le(buf + 28, byte_rate);
	put_u16le(buf + 32, block_align);
	put_u16le(buf + 34, WAV_BITS_PER_SAMPLE);

	memcpy(buf + 36, "data", 4);
	put_u32le(buf + 40, data_bytes);
}


/* Commits the header using `w->srate` - see wav_writer_write()/
 * wav_writer_close() for the two call sites and why each passes what it
 * does. Private: nothing outside this file needs to commit a header
 * without also either being about to write samples (wav_writer_write())
 * or finalizing (wav_writer_close()) - see wav_writer.h's own "two-phase"
 * comment. */
static int wav_writer_begin(struct wav_writer *w, uint32_t srate)
{
	uint8_t header[WAV_HEADER_SIZE];

	if (!w || !w->fp)
		return EINVAL;

	if (w->header_written)
		return 0;   /* already committed - no-op, never re-stamp a
			     * header once real data may follow it */

	w->srate = srate;
	build_header(header, srate, 0);

	if (fwrite(header, 1, sizeof(header), w->fp) != sizeof(header)) {
		w->err = EIO;
		return EIO;
	}

	w->header_written = 1;

	return 0;
}


int wav_writer_create(struct wav_writer *w, const char *path)
{
	if (!w)
		return EINVAL;

	memset(w, 0, sizeof(*w));

	if (!path || !path[0])
		return EINVAL;

	w->fp = fopen(path, "wb");
	if (!w->fp)
		return errno ? errno : EIO;

	return 0;
}


int wav_writer_write(struct wav_writer *w, uint32_t srate,
		      const int16_t *sampv, size_t sampc)
{
	size_t nbytes;
	int err;

	if (!w || !w->fp)
		return EINVAL;

	if (w->err)
		return w->err;   /* sticky - see struct wav_writer's own
				  * comment; don't retry failing I/O every
				  * frame */

	if (!w->header_written) {
		err = wav_writer_begin(w, srate);
		if (err)
			return err;   /* wav_writer_begin() already set
					* ->err on the write-failure path */
	}

	if (!sampc || !sampv)
		return 0;

	nbytes = sampc * sizeof(int16_t);

	if (fwrite(sampv, 1, nbytes, w->fp) != nbytes) {
		w->err = EIO;
		return EIO;
	}

	w->data_bytes += (uint32_t)nbytes;   /* NOTE: like any canonical
					       * (non-RF64) WAV file, this
					       * writer's own uint32
					       * data_bytes field - and the
					       * on-disk header field it
					       * mirrors - wrap at 4GiB of a
					       * single tap's data (~37 hours
					       * continuous at 8kHz mono
					       * 16-bit - see PROTOCOL.md/
					       * BUILD.md for this build's
					       * actual sample rate). Not a
					       * defect specific to this
					       * writer; no call in this
					       * engine's e2e testing gets
					       * remotely close. */

	return 0;
}


int wav_writer_close(struct wav_writer *w, uint32_t fallback_srate)
{
	uint8_t header[WAV_HEADER_SIZE];

	if (!w || !w->fp)
		return 0;   /* never created, or already closed - see this
			     * function's own "idempotent" doc comment */

	if (!w->header_written) {
		int err = wav_writer_begin(w, fallback_srate);

		if (err) {
			(void)fclose(w->fp);
			w->fp = NULL;
			return err;
		}
	}

	build_header(header, w->srate, w->data_bytes);

	if (fseek(w->fp, 0, SEEK_SET) != 0 ||
	    fwrite(header, 1, sizeof(header), w->fp) != sizeof(header)) {
		(void)fclose(w->fp);
		w->fp = NULL;
		return EIO;
	}

	(void)fflush(w->fp);
	(void)fclose(w->fp);
	w->fp = NULL;

	return 0;
}


uint32_t wav_writer_bytes(const struct wav_writer *w)
{
	return w ? w->data_bytes : 0;
}

/*
 * main.c - mediadec's entry point, FFmpeg's log, and what the shim's files
 * share.
 *
 * The program starts in C, as every program on musl does; main() hands it
 * to the Rust half. FFmpeg's messages go to the system log (the process's
 * standard error), its warnings and errors only, and not without end: a
 * damaged file can make a decoder complain at every frame.
 */

#include <stdarg.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include <dav1d/dav1d.h>
#include <libavutil/avutil.h>
#include <libavutil/mem.h>

#include "vmd_internal.h"

/* The Rust half's view of these (src/ffi.rs) must match. */
_Static_assert(sizeof(struct vmd_io) == 32, "struct vmd_io changed: update src/ffi.rs");
_Static_assert(sizeof(struct vmd_info) == 584, "struct vmd_info changed: update src/ffi.rs");
_Static_assert(sizeof(struct vmd_stream) == 384, "struct vmd_stream changed: update src/ffi.rs");
_Static_assert(sizeof(struct vmd_packet_info) == 40, "struct vmd_packet_info changed: update src/ffi.rs");
_Static_assert(sizeof(struct vmd_frame_info) == 48, "struct vmd_frame_info changed: update src/ffi.rs");

/* Messages FFmpeg may log before only one in LOG_EVERY more gets through. */
#define LOG_FREELY 200
#define LOG_EVERY 1000

static atomic_uint logged;

static void log_line(void *avcl, int level, const char *fmt, va_list vl)
{
	if (level > AV_LOG_WARNING)
		return;
	unsigned n = atomic_fetch_add(&logged, 1);
	if (n >= LOG_FREELY && n % LOG_EVERY)
		return;
	char line[512];
	int print_prefix = 1;
	int len = av_log_format_line2(avcl, level, fmt, vl, line, sizeof line, &print_prefix);
	if (len <= 0)
		return;
	if ((size_t)len >= sizeof line)
		len = sizeof line - 1;
	/* One write, so that threads' lines do not mix. */
	char out[sizeof line + 16];
	int m = snprintf(out, sizeof out, "ffmpeg: %.*s%s", len, line, line[len - 1] == '\n' ? "" : "\n");
	if (m > 0)
		(void)!write(2, out, (size_t)m < sizeof out ? (size_t)m : sizeof out - 1);
}

void vmd_setup(uint64_t max_alloc)
{
	av_log_set_level(AV_LOG_WARNING);
	av_log_set_callback(log_line);
	if (max_alloc)
		av_max_alloc((size_t)max_alloc);
}

const char *vmd_build(void)
{
	static char build[96];
	if (!build[0])
		snprintf(build, sizeof build, "FFmpeg %s, dav1d %s", av_version_info(), dav1d_version());
	return build;
}

int32_t vmd_error(int e)
{
	switch (e) {
	case AVERROR(ENOMEM):
		return VMD_ENOMEM;
	case AVERROR_INVALIDDATA:
		return VMD_ECORRUPT;
	case AVERROR_DECODER_NOT_FOUND:
	case AVERROR_DEMUXER_NOT_FOUND:
	case AVERROR_STREAM_NOT_FOUND:
	case AVERROR_PATCHWELCOME:
	case AVERROR(ENOSYS):
		return VMD_EUNSUPPORTED;
	case AVERROR(EIO):
	case AVERROR_EOF:
		return VMD_EIO;
	case AVERROR_EXIT:
		return VMD_EEXIT;
	case AVERROR(EINVAL):
		return VMD_EINVALID;
	case AVERROR(ERANGE):
		return VMD_ETOOLARGE;
	default:
		return VMD_EINTERNAL;
	}
}

int64_t vmd_time(int64_t t, AVRational base)
{
	if (t == AV_NOPTS_VALUE || base.num <= 0 || base.den <= 0)
		return VMD_NO_TIME;
	return av_rescale_q(t, base, AV_TIME_BASE_Q);
}

void vmd_copy_text(char *dst, size_t cap, const char *src)
{
	size_t n = strlen(src);
	if (n >= cap) {
		n = cap - 1;
		/* Not inside a character: back to its first byte. */
		while (n > 0 && ((unsigned char)src[n] & 0xC0) == 0x80)
			n--;
	}
	memcpy(dst, src, n);
	dst[n] = 0;
}

int main(void)
{
	return mediadec_main();
}

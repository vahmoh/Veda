/*
 * vmd_internal.h - what the shim's files share: the objects behind vmd.h's
 * opaque types, and small helpers.
 */

#ifndef VMD_INTERNAL_H
#define VMD_INTERNAL_H

#include <stddef.h>

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>

#include "vmd.h"

struct vmd_source {
	struct vmd_io io;
	AVFormatContext *fmt;
	AVIOContext *avio;
	/* The container's first time (µs), and the streams in use. */
	int64_t start_us;
	int video;
	int audio;
	uint32_t max_width;
	uint32_t max_height;
};

struct vmd_decoder {
	AVCodecContext *ctx;
	AVFrame *frame;
};

/* An AVERROR as a VMD_E* code. */
int32_t vmd_error(int averror);

/* A time in `base` units as microseconds (VMD_NO_TIME stays so). */
int64_t vmd_time(int64_t t, AVRational base);

/* `src` into `dst` (`cap` bytes, always terminated), cut at a UTF-8
 * character's boundary. */
void vmd_copy_text(char *dst, size_t cap, const char *src);

#endif

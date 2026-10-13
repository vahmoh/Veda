/*
 * SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * decode.c - decoders (libavcodec; AV1 through dav1d).
 *
 * The Rust half feeds them packets and takes their frames one call at a
 * time, between which it answers the player.
 */

#include <string.h>

#include <libavutil/pixdesc.h>

#include "vmd_internal.h"

static void set_skip(AVCodecContext *c, uint32_t level)
{
	c->skip_loop_filter = level >= 1 ? AVDISCARD_ALL : AVDISCARD_DEFAULT;
	c->skip_frame = level >= 3 ? AVDISCARD_NONKEY : level >= 2 ? AVDISCARD_NONREF : AVDISCARD_DEFAULT;
}

int32_t vmd_decoder_open(struct vmd_source *src, uint32_t stream, uint32_t threads, uint32_t flags,
			 struct vmd_decoder **out)
{
	*out = NULL;
	if (stream >= src->fmt->nb_streams)
		return VMD_EINVALID;
	const AVStream *st = src->fmt->streams[stream];
	const AVCodec *codec = avcodec_find_decoder(st->codecpar->codec_id);
	if (!codec)
		return VMD_EUNSUPPORTED;
	struct vmd_decoder *dec = av_mallocz(sizeof *dec);
	if (!dec)
		return VMD_ENOMEM;
	dec->ctx = avcodec_alloc_context3(codec);
	dec->frame = av_frame_alloc();
	if (!dec->ctx || !dec->frame) {
		vmd_decoder_close(dec);
		return VMD_ENOMEM;
	}
	int r = avcodec_parameters_to_context(dec->ctx, st->codecpar);
	if (r < 0) {
		vmd_decoder_close(dec);
		return vmd_error(r);
	}
	AVCodecContext *c = dec->ctx;
	c->pkt_timebase = st->time_base;
	c->thread_count = (int)threads;
	c->thread_type = FF_THREAD_FRAME | FF_THREAD_SLICE;
	/* Pictures beyond the player's limit are refused (dav1d too: it takes
	 * this as its frame size limit). */
	if (src->max_width && src->max_height)
		c->max_pixels = (int64_t)src->max_width * src->max_height;
	c->max_samples = 1 << 20;
	if (flags & VMD_DECODE_BITEXACT)
		c->flags |= AV_CODEC_FLAG_BITEXACT;
	set_skip(c, (flags >> 8) & 3);
	r = avcodec_open2(c, codec, NULL);
	if (r < 0) {
		vmd_decoder_close(dec);
		return vmd_error(r);
	}
	*out = dec;
	return VMD_OK;
}

void vmd_decoder_close(struct vmd_decoder *dec)
{
	if (!dec)
		return;
	avcodec_free_context(&dec->ctx);
	av_frame_free(&dec->frame);
	av_free(dec);
}

int32_t vmd_send(struct vmd_decoder *dec, const struct vmd_packet *packet)
{
	int r = avcodec_send_packet(dec->ctx, (const AVPacket *)packet);
	if (r == AVERROR(EAGAIN))
		return VMD_AGAIN;
	if (r == AVERROR_EOF)
		return VMD_END;
	return r < 0 ? vmd_error(r) : VMD_OK;
}

int32_t vmd_receive(struct vmd_decoder *dec, struct vmd_frame **out, struct vmd_frame_info *info)
{
	*out = NULL;
	int r = avcodec_receive_frame(dec->ctx, dec->frame);
	if (r == AVERROR(EAGAIN))
		return VMD_AGAIN;
	if (r == AVERROR_EOF)
		return VMD_END;
	if (r < 0)
		return vmd_error(r);
	AVFrame *f = av_frame_alloc();
	if (!f) {
		av_frame_unref(dec->frame);
		return VMD_ENOMEM;
	}
	av_frame_move_ref(f, dec->frame);
	AVRational base = dec->ctx->pkt_timebase;
	int64_t pts = f->best_effort_timestamp != AV_NOPTS_VALUE ? f->best_effort_timestamp : f->pts;
	info->pts_us = vmd_time(pts, base);
	info->duration_us = f->duration > 0 ? vmd_time(f->duration, base) : 0;
	info->width = f->width > 0 ? (uint32_t)f->width : 0;
	info->height = f->height > 0 ? (uint32_t)f->height : 0;
	info->key = (f->flags & AV_FRAME_FLAG_KEY) != 0;
	info->corrupt = (f->flags & AV_FRAME_FLAG_CORRUPT) || f->decode_error_flags;
	info->samples = f->nb_samples > 0 ? (uint32_t)f->nb_samples : 0;
	info->rate = f->sample_rate > 0 ? (uint32_t)f->sample_rate : 0;
	info->channels = f->ch_layout.nb_channels > 0 ? (uint32_t)f->ch_layout.nb_channels : 0;
	*out = (struct vmd_frame *)f;
	return VMD_OK;
}

void vmd_frame_free(struct vmd_frame *frame)
{
	AVFrame *f = (AVFrame *)frame;
	av_frame_free(&f);
}

void vmd_flush(struct vmd_decoder *dec)
{
	avcodec_flush_buffers(dec->ctx);
}

int32_t vmd_skip(struct vmd_decoder *dec, uint32_t level)
{
	/* dav1d takes what to decode once, when opened (its settings). */
	if (!strcmp(dec->ctx->codec->name, "libdav1d"))
		return VMD_AGAIN;
	set_skip(dec->ctx, level);
	return VMD_OK;
}

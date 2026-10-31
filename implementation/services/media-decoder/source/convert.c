/*
 * SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * convert.c - pictures to the screen's pixels (libswscale), sound to the
 * player's samples (libswresample), and pictures' fingerprints.
 *
 * Pictures become opaque 0xAARRGGBB (BGRA in memory) at the size the
 * player shows them, from the matrix and range the stream says (guessed
 * from the size where it says nothing, as players do). Standard dynamic
 * range keeps its transfer and primaries, as every player shows it; high
 * dynamic range (PQ, HLG) and wide gamuts are mapped to the screen's,
 * where the scaler can.
 */

#include <libavutil/channel_layout.h>
#include <libavutil/imgutils.h>
#include <libavutil/md5.h>
#include <libavutil/pixdesc.h>
#include <libswresample/swresample.h>
#include <libswscale/swscale.h>

#include "vmd_internal.h"

struct vmd_scaler {
	SwsContext *sws;
	uint32_t flags;
	/* The source with its colours filled in where the stream says nothing;
	 * the destination over the caller's pixels. */
	AVFrame *in;
	AVFrame *out;
};

struct vmd_resampler {
	SwrContext *swr;
	AVChannelLayout layout;
	int format;
	int rate_in;
	uint32_t rate_out;
};

static int is_sdr_transfer(enum AVColorTransferCharacteristic t)
{
	return t != AVCOL_TRC_SMPTE2084 && t != AVCOL_TRC_ARIB_STD_B67 && t != AVCOL_TRC_LINEAR &&
	       t != AVCOL_TRC_SMPTE428;
}

static int is_screen_gamut(enum AVColorPrimaries p)
{
	return p == AVCOL_PRI_BT709 || p == AVCOL_PRI_BT470BG || p == AVCOL_PRI_SMPTE170M || p == AVCOL_PRI_SMPTE240M;
}

/* Fills in what the stream leaves unsaid: high-definition pictures are
 * BT.709, others BT.601, in limited range. */
static void fill_colours(AVFrame *f)
{
	int hd = f->height >= 720 || f->width >= 1280;
	if (f->colorspace == AVCOL_SPC_UNSPECIFIED)
		f->colorspace = hd ? AVCOL_SPC_BT709 : AVCOL_SPC_SMPTE170M;
	if (f->color_primaries == AVCOL_PRI_UNSPECIFIED)
		f->color_primaries = hd ? AVCOL_PRI_BT709 : AVCOL_PRI_SMPTE170M;
	if (f->color_trc == AVCOL_TRC_UNSPECIFIED)
		f->color_trc = AVCOL_TRC_BT709;
	if (f->color_range == AVCOL_RANGE_UNSPECIFIED)
		f->color_range = AVCOL_RANGE_MPEG;
}

static int new_context(struct vmd_scaler *s, uint32_t flags)
{
	sws_free_context(&s->sws);
	s->sws = sws_alloc_context();
	if (!s->sws)
		return VMD_ENOMEM;
	s->sws->scaler = (flags & VMD_PICTURE_FAST) ? SWS_SCALE_BILINEAR : SWS_SCALE_BICUBIC;
	s->sws->dither = SWS_DITHER_AUTO;
	s->sws->threads = 4;
	if (flags & VMD_PICTURE_BITEXACT)
		s->sws->flags |= SWS_ACCURATE_RND | SWS_BITEXACT;
	s->flags = flags;
	return VMD_OK;
}

int32_t vmd_picture(struct vmd_scaler **scaler, const struct vmd_frame *frame, uint32_t *dst, uint32_t stride,
		    uint32_t width, uint32_t height, uint32_t flags)
{
	const AVFrame *f = (const AVFrame *)frame;
	if (!f || !f->width || !width || !height || stride < width || width > 16384 || height > 16384)
		return VMD_EINVALID;
	struct vmd_scaler *s = *scaler;
	if (!s) {
		s = av_mallocz(sizeof *s);
		if (!s)
			return VMD_ENOMEM;
		s->in = av_frame_alloc();
		s->out = av_frame_alloc();
		*scaler = s;
		if (!s->in || !s->out)
			return VMD_ENOMEM;
		int r = new_context(s, flags);
		if (r < 0)
			return r;
	} else if (s->flags != flags) {
		int r = new_context(s, flags);
		if (r < 0)
			return r;
	}
	int r = av_frame_ref(s->in, f);
	if (r < 0)
		return vmd_error(r);
	fill_colours(s->in);
	AVFrame *out = s->out;
	av_frame_unref(out);
	out->format = AV_PIX_FMT_BGRA;
	out->width = (int)width;
	out->height = (int)height;
	out->data[0] = (uint8_t *)dst;
	out->linesize[0] = (int)(stride * 4);
	out->color_range = AVCOL_RANGE_JPEG;
	out->colorspace = AVCOL_SPC_RGB;
	out->color_primaries = is_screen_gamut(s->in->color_primaries) ? s->in->color_primaries : AVCOL_PRI_BT709;
	out->color_trc = is_sdr_transfer(s->in->color_trc) ? s->in->color_trc : AVCOL_TRC_BT709;
	r = sws_scale_frame(s->sws, out, s->in);
	if (r < 0 && (out->color_primaries != s->in->color_primaries || out->color_trc != s->in->color_trc)) {
		/* A mapping the scaler cannot do: the picture as it is, not
		 * nothing. */
		out->color_primaries = s->in->color_primaries;
		out->color_trc = s->in->color_trc;
		r = sws_scale_frame(s->sws, out, s->in);
	}
	/* The caller's pixels, never the frame's to free. */
	out->data[0] = NULL;
	av_frame_unref(s->in);
	return r < 0 ? vmd_error(r) : VMD_OK;
}

void vmd_scaler_free(struct vmd_scaler *scaler)
{
	if (!scaler)
		return;
	sws_free_context(&scaler->sws);
	av_frame_free(&scaler->in);
	if (scaler->out)
		scaler->out->data[0] = NULL;
	av_frame_free(&scaler->out);
	av_free(scaler);
}

int32_t vmd_sound(struct vmd_resampler **resampler, const struct vmd_frame *frame, int64_t frame_pts_us,
		  uint32_t rate, int16_t *dst, uint32_t capacity, uint32_t *frames, int64_t *pts_us)
{
	const AVFrame *f = (const AVFrame *)frame;
	*frames = 0;
	*pts_us = VMD_NO_TIME;
	struct vmd_resampler *r = *resampler;
	uint8_t *out = (uint8_t *)dst;
	if (!f) {
		/* The end: what the resampler holds back. */
		if (!r || !r->swr)
			return VMD_OK;
		int n = swr_convert(r->swr, &out, (int)capacity, NULL, 0);
		if (n < 0)
			return vmd_error(n);
		*frames = (uint32_t)n;
		return VMD_OK;
	}
	if (!f->nb_samples || f->ch_layout.nb_channels <= 0 || f->sample_rate <= 0 || rate < 8000 || rate > 192000)
		return VMD_EINVALID;
	if (!r) {
		r = av_mallocz(sizeof *r);
		if (!r)
			return VMD_ENOMEM;
		*resampler = r;
	}
	if (!r->swr || r->format != f->format || r->rate_in != f->sample_rate || r->rate_out != rate ||
	    av_channel_layout_compare(&r->layout, &f->ch_layout)) {
		/* A stream that changes on the way (rare, but allowed). */
		swr_free(&r->swr);
		av_channel_layout_uninit(&r->layout);
		AVChannelLayout stereo = AV_CHANNEL_LAYOUT_STEREO;
		int e = swr_alloc_set_opts2(&r->swr, &stereo, AV_SAMPLE_FMT_S16, (int)rate, &f->ch_layout,
					    (enum AVSampleFormat)f->format, f->sample_rate, 0, NULL);
		if (e >= 0)
			e = swr_init(r->swr);
		if (e >= 0)
			e = av_channel_layout_copy(&r->layout, &f->ch_layout);
		if (e < 0) {
			swr_free(&r->swr);
			return vmd_error(e);
		}
		r->format = f->format;
		r->rate_in = f->sample_rate;
		r->rate_out = rate;
	}
	/* What it holds back is heard before this frame's first sample. */
	int64_t delay_us = swr_get_delay(r->swr, 1000000);
	int n = swr_convert(r->swr, &out, (int)capacity, (const uint8_t **)f->extended_data, f->nb_samples);
	if (n < 0)
		return vmd_error(n);
	*frames = (uint32_t)n;
	if (frame_pts_us != VMD_NO_TIME)
		*pts_us = frame_pts_us - delay_us;
	return VMD_OK;
}

void vmd_resampler_free(struct vmd_resampler *resampler)
{
	if (!resampler)
		return;
	swr_free(&resampler->swr);
	av_channel_layout_uninit(&resampler->layout);
	av_free(resampler);
}

int32_t vmd_frame_md5(const struct vmd_frame *frame, uint8_t md5[16])
{
	const AVFrame *f = (const AVFrame *)frame;
	const AVPixFmtDescriptor *d = f ? av_pix_fmt_desc_get(f->format) : NULL;
	if (!d || !f->width || !f->height || (d->flags & AV_PIX_FMT_FLAG_HWACCEL))
		return VMD_EINVALID;
	struct AVMD5 *ctx = av_md5_alloc();
	if (!ctx)
		return VMD_ENOMEM;
	av_md5_init(ctx);
	int planes = av_pix_fmt_count_planes(f->format);
	for (int p = 0; p < planes; p++) {
		/* The visible bytes of each row, as libvpx and dav1d hash them. */
		int row = av_image_get_linesize(f->format, f->width, p);
		int rows = (p == 1 || p == 2) ? AV_CEIL_RSHIFT(f->height, d->log2_chroma_h) : f->height;
		for (int y = 0; y < rows && row > 0; y++)
			av_md5_update(ctx, f->data[p] + (ptrdiff_t)y * f->linesize[p], (size_t)row);
	}
	av_md5_final(ctx, md5);
	av_free(ctx);
	return VMD_OK;
}

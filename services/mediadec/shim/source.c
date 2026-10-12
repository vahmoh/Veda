/*
 * source.c - opening a source and reading its packets (libavformat).
 *
 * FFmpeg reads through the Rust half's callbacks (struct vmd_io), so the
 * decoder never opens a file: the source is whatever channel the player
 * sent. Only the demuxers built in are allowed, every time is turned into
 * microseconds here, and the display matrix into a rotation.
 */

#include <math.h>
#include <string.h>

#include <libavformat/avformat.h>
#include <libavutil/display.h>
#include <libavutil/pixdesc.h>

#include "vmd_internal.h"

/* The demuxers mediadec is built with (ports/build.sh). */
static const char FORMATS[] = "mov,mp4,m4a,3gp,3g2,mj2,matroska,webm,ogg,avi,mpegts,mpeg";

/* AVIO's buffer. */
#define IO_BUFFER 65536

static int read_packet(void *opaque, uint8_t *buf, int size)
{
	struct vmd_source *src = opaque;
	int32_t n = src->io.read(src->io.opaque, buf, size);
	if (n == 0)
		return AVERROR_EOF;
	return n < 0 ? AVERROR(EIO) : n;
}

static int64_t seek_packet(void *opaque, int64_t offset, int whence)
{
	struct vmd_source *src = opaque;
	if (whence & AVSEEK_SIZE)
		return src->io.seek(src->io.opaque, 0, VMD_SEEK_SIZE);
	int64_t at = src->io.seek(src->io.opaque, offset, whence & ~AVSEEK_FORCE);
	return at < 0 ? AVERROR(EIO) : at;
}

static int interrupted(void *opaque)
{
	struct vmd_source *src = opaque;
	return src->io.interrupted(src->io.opaque) != 0;
}

int32_t vmd_open(const struct vmd_io *io, const char *name, uint32_t max_width, uint32_t max_height,
		 struct vmd_source **out)
{
	*out = NULL;
	struct vmd_source *src = av_mallocz(sizeof *src);
	if (!src)
		return VMD_ENOMEM;
	src->io = *io;
	src->max_width = max_width;
	src->max_height = max_height;
	uint8_t *buffer = av_malloc(IO_BUFFER);
	src->avio = buffer ? avio_alloc_context(buffer, IO_BUFFER, 0, src, read_packet, NULL, seek_packet) : NULL;
	if (!src->avio) {
		av_free(buffer);
		av_free(src);
		return VMD_ENOMEM;
	}
	AVFormatContext *fmt = avformat_alloc_context();
	if (!fmt) {
		vmd_close(src);
		return VMD_ENOMEM;
	}
	fmt->pb = src->avio;
	fmt->flags |= AVFMT_FLAG_CUSTOM_IO;
	fmt->interrupt_callback.callback = interrupted;
	fmt->interrupt_callback.opaque = src;
	fmt->format_whitelist = av_strdup(FORMATS);
	/* Enough of a file to find every stream of an ordinary one, and no
	 * more streams than the protocol lists. */
	fmt->probesize = 5 << 20;
	fmt->max_analyze_duration = 5 * AV_TIME_BASE;
	fmt->max_streams = 64;
	int r = avformat_open_input(&fmt, name, NULL, NULL);
	if (r < 0) {
		/* avformat_open_input frees the context when it fails. */
		vmd_close(src);
		return vmd_error(r);
	}
	src->fmt = fmt;
	r = avformat_find_stream_info(fmt, NULL);
	if (r < 0) {
		vmd_close(src);
		return vmd_error(r);
	}
	src->start_us = fmt->start_time == AV_NOPTS_VALUE ? 0 : fmt->start_time;
	src->video = -1;
	src->audio = -1;
	*out = src;
	return VMD_OK;
}

void vmd_close(struct vmd_source *src)
{
	if (!src)
		return;
	if (src->fmt)
		avformat_close_input(&src->fmt);
	if (src->avio) {
		av_freep(&src->avio->buffer);
		avio_context_free(&src->avio);
	}
	av_free(src);
}

static void copy_tag(char *dst, size_t cap, AVDictionary *meta, const char *key)
{
	const AVDictionaryEntry *e = av_dict_get(meta, key, NULL, 0);
	vmd_copy_text(dst, cap, e ? e->value : "");
}

int32_t vmd_info(struct vmd_source *src, struct vmd_info *out)
{
	AVFormatContext *fmt = src->fmt;
	memset(out, 0, sizeof *out);
	out->duration_us = fmt->duration == AV_NOPTS_VALUE ? VMD_NO_TIME : fmt->duration;
	out->start_us = src->start_us;
	out->bit_rate = fmt->bit_rate > 0 ? (uint64_t)fmt->bit_rate : 0;
	out->streams = fmt->nb_streams;
	out->chapters = fmt->nb_chapters;
	out->seekable = (fmt->pb->seekable & AVIO_SEEKABLE_NORMAL) && !(fmt->ctx_flags & AVFMTCTX_UNSEEKABLE);
	vmd_copy_text(out->container, sizeof out->container, fmt->iformat->name);
	/* "mov,mp4,m4a,..." names the family: its first name says enough. */
	char *comma = strchr(out->container, ',');
	if (comma)
		*comma = 0;
	copy_tag(out->title, sizeof out->title, fmt->metadata, "title");
	copy_tag(out->artist, sizeof out->artist, fmt->metadata, "artist");
	return VMD_OK;
}

/* The clockwise rotation and the mirroring that show a picture upright, as
 * FFmpeg's own `ffmpeg` turns them into filters (fftools/ffmpeg_filter.c:
 * get_rotation, then transpose, hflip and vflip). Angles other than right
 * angles are left alone. */
static void orientation(const int32_t *m, uint32_t *rotation, uint32_t *mirrored)
{
	*rotation = 0;
	*mirrored = 0;
	if (!m)
		return;
	double theta = -round(av_display_rotation_get(m));
	theta -= 360 * floor(theta / 360 + 0.9 / 360);
	if (fabs(theta - 90) < 1.0) {
		*rotation = 90;
		*mirrored = m[3] > 0;
	} else if (fabs(theta - 180) < 1.0) {
		/* An upside-down picture is flipped both ways; flipped one way
		 * only, it is mirrored. */
		if (m[0] < 0 && m[4] < 0)
			*rotation = 180;
		else if (m[0] < 0)
			*mirrored = 1;
		else if (m[4] < 0) {
			*rotation = 180;
			*mirrored = 1;
		}
	} else if (fabs(theta - 270) < 1.0) {
		*rotation = 270;
		*mirrored = m[3] < 0;
	} else if (fabs(theta) < 1.0 && m[4] < 0) {
		*rotation = 180;
		*mirrored = 1;
	}
}

int32_t vmd_stream(struct vmd_source *src, uint32_t index, struct vmd_stream *out)
{
	AVFormatContext *fmt = src->fmt;
	if (index >= fmt->nb_streams)
		return VMD_EINVALID;
	const AVStream *st = fmt->streams[index];
	const AVCodecParameters *par = st->codecpar;
	memset(out, 0, sizeof *out);
	out->index = index;
	switch (par->codec_type) {
	case AVMEDIA_TYPE_VIDEO:
		out->kind = VMD_KIND_VIDEO;
		break;
	case AVMEDIA_TYPE_AUDIO:
		out->kind = VMD_KIND_AUDIO;
		break;
	case AVMEDIA_TYPE_SUBTITLE:
		out->kind = VMD_KIND_SUBTITLE;
		break;
	default:
		out->kind = VMD_KIND_OTHER;
		break;
	}
	vmd_copy_text(out->codec, sizeof out->codec, avcodec_get_name(par->codec_id));
	copy_tag(out->language, sizeof out->language, st->metadata, "language");
	copy_tag(out->title, sizeof out->title, st->metadata, "title");
	out->decodable = avcodec_find_decoder(par->codec_id) != NULL;
	out->is_default = (st->disposition & AV_DISPOSITION_DEFAULT) != 0;
	out->cover = (st->disposition & AV_DISPOSITION_ATTACHED_PIC) != 0;
	if (out->kind == VMD_KIND_VIDEO) {
		out->width = par->width > 0 ? (uint32_t)par->width : 0;
		out->height = par->height > 0 ? (uint32_t)par->height : 0;
		AVRational sar = av_guess_sample_aspect_ratio(fmt, (AVStream *)st, NULL);
		if (sar.num > 0 && sar.den > 0) {
			out->sar_num = (uint32_t)sar.num;
			out->sar_den = (uint32_t)sar.den;
		}
		AVRational fps = st->avg_frame_rate.num > 0 ? st->avg_frame_rate : st->r_frame_rate;
		if (fps.num > 0 && fps.den > 0) {
			out->fps_num = (uint32_t)fps.num;
			out->fps_den = (uint32_t)fps.den;
		}
		const AVPacketSideData *sd =
			av_packet_side_data_get(par->coded_side_data, par->nb_coded_side_data, AV_PKT_DATA_DISPLAYMATRIX);
		orientation(sd && sd->size >= 9 * sizeof(int32_t) ? (const int32_t *)sd->data : NULL, &out->rotation,
			    &out->mirrored);
		const AVPixFmtDescriptor *desc = av_pix_fmt_desc_get(par->format);
		out->bit_depth = desc ? (uint32_t)desc->comp[0].depth : 8;
		out->primaries = par->color_primaries;
		out->transfer = par->color_trc;
		out->matrix = par->color_space;
		out->full_range = par->color_range == AVCOL_RANGE_JPEG;
	} else if (out->kind == VMD_KIND_AUDIO) {
		out->rate = par->sample_rate > 0 ? (uint32_t)par->sample_rate : 0;
		out->channels = par->ch_layout.nb_channels > 0 ? (uint32_t)par->ch_layout.nb_channels : 0;
	}
	return VMD_OK;
}

int32_t vmd_chapter(struct vmd_source *src, uint32_t index, int64_t *start_us, char title[256])
{
	AVFormatContext *fmt = src->fmt;
	if (index >= fmt->nb_chapters)
		return VMD_EINVALID;
	const AVChapter *c = fmt->chapters[index];
	*start_us = av_rescale_q(c->start, c->time_base, AV_TIME_BASE_Q);
	copy_tag(title, 256, c->metadata, "title");
	return VMD_OK;
}

int32_t vmd_best(struct vmd_source *src, uint32_t kind)
{
	AVFormatContext *fmt = src->fmt;
	if (kind == VMD_KIND_VIDEO) {
		/* A video, not a cover picture: the file's default, else the
		 * first that can be decoded. */
		int first = -1;
		for (unsigned i = 0; i < fmt->nb_streams; i++) {
			const AVStream *st = fmt->streams[i];
			if (st->codecpar->codec_type != AVMEDIA_TYPE_VIDEO || (st->disposition & AV_DISPOSITION_ATTACHED_PIC) ||
			    !avcodec_find_decoder(st->codecpar->codec_id))
				continue;
			if (st->disposition & AV_DISPOSITION_DEFAULT)
				return (int32_t)i;
			if (first < 0)
				first = (int)i;
		}
		return first >= 0 ? first : VMD_EUNSUPPORTED;
	}
	if (kind == VMD_KIND_AUDIO) {
		int r = av_find_best_stream(fmt, AVMEDIA_TYPE_AUDIO, -1, src->video, NULL, 0);
		if (r >= 0 && avcodec_find_decoder(fmt->streams[r]->codecpar->codec_id))
			return r;
		return VMD_EUNSUPPORTED;
	}
	return VMD_EINVALID;
}

int32_t vmd_use(struct vmd_source *src, int32_t video, int32_t audio)
{
	AVFormatContext *fmt = src->fmt;
	if (video >= (int32_t)fmt->nb_streams || audio >= (int32_t)fmt->nb_streams)
		return VMD_EINVALID;
	for (unsigned i = 0; i < fmt->nb_streams; i++)
		fmt->streams[i]->discard = ((int32_t)i == video || (int32_t)i == audio) ? AVDISCARD_DEFAULT : AVDISCARD_ALL;
	src->video = video;
	src->audio = audio;
	return VMD_OK;
}

int32_t vmd_read(struct vmd_source *src, struct vmd_packet **out, struct vmd_packet_info *info)
{
	*out = NULL;
	AVPacket *pkt = av_packet_alloc();
	if (!pkt)
		return VMD_ENOMEM;
	for (;;) {
		int r = av_read_frame(src->fmt, pkt);
		if (r < 0) {
			av_packet_free(&pkt);
			return r == AVERROR_EOF ? VMD_END : vmd_error(r);
		}
		if (pkt->stream_index == src->video || pkt->stream_index == src->audio)
			break;
		av_packet_unref(pkt);
	}
	const AVStream *st = src->fmt->streams[pkt->stream_index];
	info->stream = (uint32_t)pkt->stream_index;
	info->size = (uint32_t)pkt->size;
	info->pts_us = vmd_time(pkt->pts, st->time_base);
	info->dts_us = vmd_time(pkt->dts, st->time_base);
	info->duration_us = pkt->duration > 0 ? vmd_time(pkt->duration, st->time_base) : 0;
	info->key = (pkt->flags & AV_PKT_FLAG_KEY) != 0;
	*out = (struct vmd_packet *)pkt;
	return VMD_OK;
}

void vmd_packet_free(struct vmd_packet *packet)
{
	AVPacket *pkt = (AVPacket *)packet;
	av_packet_free(&pkt);
}

int32_t vmd_seek(struct vmd_source *src, int64_t target_us)
{
	/* The last key frame at or before the target, in AV_TIME_BASE. */
	int r = avformat_seek_file(src->fmt, -1, INT64_MIN, target_us, target_us, 0);
	if (r < 0)
		/* Some formats only seek forward from the target: the start. */
		r = avformat_seek_file(src->fmt, -1, INT64_MIN, src->start_us, src->start_us, 0);
	return r < 0 ? vmd_error(r) : VMD_OK;
}

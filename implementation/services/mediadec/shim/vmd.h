/*
 * SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * vmd.h - mediadec's shim: the only code of Veda's that calls FFmpeg.
 *
 * mediadec is a Rust program in C's world (a static ELF program on musl):
 * its Rust half (../src) speaks the media protocol, fills the shared frame
 * pool and sound ring and schedules the work; this shim opens, demuxes,
 * decodes (AV1 through dav1d), converts and fingerprints with FFmpeg,
 * behind plain integers and opaque objects. No floating-point value
 * crosses it: the Rust half is built for the architecture's bare target
 * (<architecture>-unknown-none), which may pass them otherwise than GCC
 * (`cargo orchestration` refuses `float` and `double` here).
 *
 * Times are microseconds. Objects are made by the shim and freed by it.
 */

#ifndef VMD_H
#define VMD_H

#include <stdint.h>

#define VMD_ABI 1

/* Results: not negative on success; errors are vproto::media::MediaError's
 * codes, negated. */
#define VMD_OK 0
/* Nothing now: receive first (send), or send more (receive). */
#define VMD_AGAIN 1
/* The end of the stream. */
#define VMD_END 2
#define VMD_EINVALID (-1)
#define VMD_ESTATE (-2)
#define VMD_EUNSUPPORTED (-3)
#define VMD_ECORRUPT (-4)
#define VMD_EIO (-5)
#define VMD_ENOMEM (-6)
#define VMD_ETOOLARGE (-7)
#define VMD_EINTERNAL (-8)
/* Interrupted: the player went away. */
#define VMD_EEXIT (-10)

/* A time not known. */
#define VMD_NO_TIME INT64_MIN

/* How FFmpeg reads: callbacks into the Rust half. */
struct vmd_io {
	void *opaque;
	/* Up to `size` bytes into `buf`: how many, 0 at the end, < 0 failed. */
	int32_t (*read)(void *opaque, uint8_t *buf, int32_t size);
	/* Moves to `offset` (whence: 0 from the start, 1 from here, 2 from the
	 * end) and returns where it is; with whence VMD_SEEK_SIZE, returns the
	 * size. < 0 failed. */
	int64_t (*seek)(void *opaque, int64_t offset, int32_t whence);
	/* Not 0: give up what is being done (the player went away). */
	int32_t (*interrupted)(void *opaque);
};

#define VMD_SEEK_SIZE 0x10000

/* Once, before anything else: logging, and how much FFmpeg may allocate
 * at once. */
void vmd_setup(uint64_t max_alloc);

/* What it is built from ("FFmpeg 9.0.2, dav1d 1.5.4"). */
const char *vmd_build(void);

/* ---- sources ------------------------------------------------------------ */

struct vmd_source;

/* Opens a source read through `io` (`name`: its file name, a hint to the
 * format; pictures larger than max_width x max_height are refused). */
int32_t vmd_open(const struct vmd_io *io, const char *name, uint32_t max_width, uint32_t max_height,
		 struct vmd_source **out);
void vmd_close(struct vmd_source *src);

struct vmd_info {
	/* VMD_NO_TIME when not known. */
	int64_t duration_us;
	/* The container's first time, which the Rust half subtracts from
	 * every other. */
	int64_t start_us;
	uint64_t bit_rate;
	uint32_t streams;
	uint32_t chapters;
	uint32_t seekable;
	char container[32];
	char title[256];
	char artist[256];
};

int32_t vmd_info(struct vmd_source *src, struct vmd_info *out);

#define VMD_KIND_OTHER 0
#define VMD_KIND_VIDEO 1
#define VMD_KIND_AUDIO 2
#define VMD_KIND_SUBTITLE 3

struct vmd_stream {
	uint32_t index;
	uint32_t kind;
	char codec[32];
	char language[16];
	char title[256];
	uint32_t width;
	uint32_t height;
	uint32_t sar_num;
	uint32_t sar_den;
	uint32_t fps_num;
	uint32_t fps_den;
	/* Clockwise, to show it upright: 0, 90, 180 or 270. */
	uint32_t rotation;
	uint32_t mirrored;
	uint32_t bit_depth;
	/* H.273 code points. */
	uint32_t primaries;
	uint32_t transfer;
	uint32_t matrix;
	uint32_t full_range;
	uint32_t rate;
	uint32_t channels;
	uint32_t decodable;
	uint32_t is_default;
	/* A still picture (cover art), not a video. */
	uint32_t cover;
};

int32_t vmd_stream(struct vmd_source *src, uint32_t index, struct vmd_stream *out);
int32_t vmd_chapter(struct vmd_source *src, uint32_t index, int64_t *start_us, char title[256]);
/* The stream of `kind` to show: its index, or < 0. */
int32_t vmd_best(struct vmd_source *src, uint32_t kind);
/* Reads these streams only (< 0: none of that kind). */
int32_t vmd_use(struct vmd_source *src, int32_t video, int32_t audio);

struct vmd_packet;

struct vmd_packet_info {
	uint32_t stream;
	uint32_t size;
	int64_t pts_us;
	int64_t dts_us;
	int64_t duration_us;
	uint32_t key;
};

/* The next packet of a stream in use: VMD_OK, VMD_END or an error. */
int32_t vmd_read(struct vmd_source *src, struct vmd_packet **out, struct vmd_packet_info *info);
void vmd_packet_free(struct vmd_packet *packet);
/* Moves to the key frame at or before `target_us` (in the container's
 * times). */
int32_t vmd_seek(struct vmd_source *src, int64_t target_us);

/* ---- decoders ----------------------------------------------------------- */

struct vmd_decoder;
struct vmd_frame;

#define VMD_DECODE_BITEXACT 1
/* What to leave out from the start (see vmd_skip), in bits 8 to 9. */
#define VMD_DECODE_SKIP(level) ((uint32_t)(level) << 8)

int32_t vmd_decoder_open(struct vmd_source *src, uint32_t stream, uint32_t threads, uint32_t flags,
			 struct vmd_decoder **out);
void vmd_decoder_close(struct vmd_decoder *dec);
/* Sends a packet (NULL: there are no more): VMD_OK, VMD_AGAIN (receive
 * first) or an error. The packet stays the caller's. */
int32_t vmd_send(struct vmd_decoder *dec, const struct vmd_packet *packet);

struct vmd_frame_info {
	int64_t pts_us;
	int64_t duration_us;
	uint32_t width;
	uint32_t height;
	uint32_t key;
	uint32_t corrupt;
	/* Sound. */
	uint32_t samples;
	uint32_t rate;
	uint32_t channels;
};

/* The next frame: VMD_OK, VMD_AGAIN (send more) or VMD_END (no more). */
int32_t vmd_receive(struct vmd_decoder *dec, struct vmd_frame **out, struct vmd_frame_info *info);
void vmd_frame_free(struct vmd_frame *frame);
/* Forgets what it holds (after a seek). */
void vmd_flush(struct vmd_decoder *dec);
/* What to leave out: 0 nothing, 1 the loop filter, 2 frames nothing
 * refers to, 3 all but key frames. VMD_OK when it does so from the next
 * packet, VMD_AGAIN when the decoder takes it only when opened (dav1d): it
 * must be opened again, at a key frame. */
int32_t vmd_skip(struct vmd_decoder *dec, uint32_t level);

/* ---- conversion --------------------------------------------------------- */

struct vmd_scaler;
struct vmd_resampler;

#define VMD_PICTURE_BITEXACT 1
/* Bilinear, for when the decoder falls behind. */
#define VMD_PICTURE_FAST 2

/* Converts `frame` to `width` x `height` opaque 0xAARRGGBB pixels at `dst`
 * (`stride` pixels to a row), in sRGB. `*scaler` is made on first use. */
int32_t vmd_picture(struct vmd_scaler **scaler, const struct vmd_frame *frame, uint32_t *dst, uint32_t stride,
		    uint32_t width, uint32_t height, uint32_t flags);
void vmd_scaler_free(struct vmd_scaler *scaler);

/* Converts `frame`, heard from `frame_pts_us` (NULL: what is held back
 * at the end), to interleaved stereo `int16_t` at `rate`, into `dst` (room
 * for `capacity` frames; what does not fit is held back): `*frames`
 * written, the first heard at `*pts_us` (VMD_NO_TIME when draining or not
 * known). `*resampler` is made on first use. */
int32_t vmd_sound(struct vmd_resampler **resampler, const struct vmd_frame *frame, int64_t frame_pts_us,
		  uint32_t rate, int16_t *dst, uint32_t capacity, uint32_t *frames, int64_t *pts_us);
void vmd_resampler_free(struct vmd_resampler *resampler);

/* The MD5 of a picture's visible planes as decoded. */
int32_t vmd_frame_md5(const struct vmd_frame *frame, uint8_t md5[16]);

/* ---- the program -------------------------------------------------------- */

/* The Rust half: the program after main() set the shim up. */
int32_t mediadec_main(void);

#endif

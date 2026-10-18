/* SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * What engine.c and platform.c share: a view as the engine keeps it, and
 * the platform's side of it. Everything here is the engine's thread's, but
 * a view's frame, which the browser's thread reads (engine_frame_lock), and
 * the browser's sound streams, which media.c's sink uses from GStreamer's.
 */

#ifndef BROWSER_ENGINE_INTERNAL_H
#define BROWSER_ENGINE_INTERNAL_H

#include <glib.h>
#include <wpe/webkit.h>
#include <wpe/wpe-platform.h>

#include "engine.h"

/* What WebKit drew of a view last, which the browser shows. */
struct frame {
	GMutex lock;
	uint32_t *pixels;
	int width, height;
	/* What changed since the browser last took it: x, y, width, height. */
	int damage[4];
	uint64_t serial;
	/* The browser has been told of a change it has not taken yet. */
	gboolean told;
	/* The serial of the frame whose buffer WebKit waits to hear was shown
	 * (0: none). */
	uint64_t waiting;
};

struct view {
	engine_view id;
	WebKitWebView *web_view;
	WPEView *wpe_view;
	WPEToplevel *toplevel;
	struct frame *frame;
	gboolean ephemeral;
	int width, height;
	double scale;
	gboolean shown, focused, fullscreen;
	/* The buffer drawn last, which WebKit waits to hear was shown, and the
	 * time after which it hears anyway. */
	WPEBuffer *shown_buffer;
	guint shown_timeout;
	/* The link under the pointer, as the browser was last told. */
	char *hovered;
	/* What finding finds. */
	char *finding;
};

/* --- platform.c: WebKit's platform on Veda ---------------------------- */

/* The display: one screen of `width` x `height` logical pixels at
 * `scale`, showing `refresh_rate` frames a second. */
WPEDisplay *platform_display_new(int width, int height, double scale, int refresh_rate);
/* Ties a view's WPE objects to `view`, which they tell of what WebKit asks
 * (frames, cursors, the whole screen). */
void platform_view_attach(WPEView *wpe_view, struct view *view);
void platform_view_detach(WPEView *wpe_view);
/* The keyboard's modifiers, as the latest event had them. */
void platform_set_modifiers(WPEDisplay *display, WPEModifiers modifiers);
/* The system's clipboard text, for the next paste. */
void platform_set_clipboard(WPEDisplay *display, const char *text);

/* --- engine.c: what the platform tells -------------------------------- */

/* WebKit drew `buffer` for `view` (`damage`: what changed, `n` rectangles;
 * none: all of it). */
void engine_buffer_rendered(struct view *view, WPEBuffer *buffer, const WPERectangle *damage, guint n);
void engine_cursor(struct view *view, const char *name);
void engine_fullscreen_requested(struct view *view, gboolean fullscreen);
/* A page copied `text`. */
void engine_copied(const char *text);

/* --- the browser's (src/sound.rs): its streams of the audio service ---- */

/* What media.c's sink plays through, any thread at a time. */
struct browser_sound;

/* A stream of `channels` (1 or 2) interleaved 16-bit samples at `rate`,
 * with room for about `buffer_frames` frames; NULL if none can be opened. */
struct browser_sound *browser_sound_open(uint32_t rate, uint32_t channels, uint32_t buffer_frames);
/* Writes what fits of `frames` frames: how many it wrote. */
size_t browser_sound_write(const struct browser_sound *sound, const int16_t *samples, size_t frames);
/* Waits until `frames` frames fit, or `timeout_ms` passes. */
void browser_sound_wait(const struct browser_sound *sound, uint32_t frames, uint32_t timeout_ms);
/* The frames written that have not been heard yet. */
uint32_t browser_sound_delay(const struct browser_sound *sound);
/* Pauses it (or resumes it), keeping what it holds. */
void browser_sound_pause(const struct browser_sound *sound, int paused);
/* Drops what it holds that has not been heard. */
void browser_sound_flush(const struct browser_sound *sound);
/* Closes it (NULL: nothing). */
void browser_sound_close(struct browser_sound *sound);

#endif

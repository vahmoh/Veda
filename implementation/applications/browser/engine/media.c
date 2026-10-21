/* SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * Pages' media, which WebKit's web processes play with GStreamer: its
 * plugins, linked in, and the sink that plays their sound through Veda's
 * audio service.
 *
 * GStreamer's core, built for static programs, calls
 * gst_init_static_plugins() as it starts (gst_init, which WebKit calls the
 * first time a page has media): it registers the plugins of the web
 * engine's GStreamer (gstreamer-full-1.0, which `cargo automation web`
 * builds) and the sink, whose rank makes it the one autoaudiosink, WebKit's
 * audio sink, chooses.
 *
 * The sink is a GstAudioSink: its thread writes the segments of its ring
 * buffer as they come, each into the audio service's stream (src/sound.rs,
 * opened as the first is written), which is its device, waiting while that
 * is full; the clock is the samples heard, which the service tells.
 * Pausing, stopping and flushing (a seek) end a write that waits, and pause
 * or empty the stream. Without a stream (no audio service, or no more
 * streams), the sound goes unheard at its pace, so that the pictures still
 * play at theirs.
 */

#include <gst/audio/audio.h>
#include <gst/gst.h>

#include "internal.h"

/* The plugins of the web engine's GStreamer, linked in. */
void gst_full_register_plugins(void);
/* What GStreamer's core calls as it starts. */
void gst_init_static_plugins(void);

/* How long a write waits at most for room before it looks whether it is
 * to stop. */
#define WRITE_WAIT_MS 20

/* What the audio service and the sound card hold ahead of what is heard,
 * at most (µs): the service mixes two periods ahead, the card plays from
 * six (twelve, once it has run short a few times). The sink's ring is
 * this much longer than buffer-time, which is the latency its samples are
 * placed with: a live source's samples (Web Audio's) are written into the
 * ring that far ahead of what is heard, and must still be ahead of what
 * the ring has written into the stream, which is as far ahead as the
 * service and the card hold, and the stream itself. Else they come after
 * it, and are dropped: the sound cuts out. */
#define DEVICE_US 120000

#define BROWSER_TYPE_AUDIO_SINK (browser_audio_sink_get_type())
G_DECLARE_FINAL_TYPE(BrowserAudioSink, browser_audio_sink, BROWSER, AUDIO_SINK, GstAudioSink)

struct _BrowserAudioSink {
	GstAudioSink parent;
	/* The stream, once the first samples are written (NULL before, and if
	 * none could be opened), under `lock`: only the ring buffer's thread
	 * opens it, and it is closed once that thread has ended (unprepare). */
	GMutex lock;
	struct browser_sound *sound;
	gboolean opened;
	/* The format, from prepare. */
	int rate, channels, bpf;
	/* Whether the log said that the sound comes late (once a stream). */
	gboolean said_late;
	/* Bumped to end a write that waits for room (pause, stop, reset). */
	gint interrupts;
};

G_DEFINE_FINAL_TYPE(BrowserAudioSink, browser_audio_sink, GST_TYPE_AUDIO_SINK)

/* What the audio service plays: 16-bit samples, mono or stereo. */
#define SINK_CAPS                                                                                                  \
	"audio/x-raw, format = (string) S16LE, layout = (string) interleaved, rate = (int) [ 8000, 192000 ], "      \
	"channels = (int) 1; "                                                                                     \
	"audio/x-raw, format = (string) S16LE, layout = (string) interleaved, rate = (int) [ 8000, 192000 ], "      \
	"channels = (int) 2, channel-mask = (bitmask) 0x3"

static GstStaticPadTemplate sink_template =
	GST_STATIC_PAD_TEMPLATE("sink", GST_PAD_SINK, GST_PAD_ALWAYS, GST_STATIC_CAPS(SINK_CAPS));

static gboolean sink_open(GstAudioSink *base)
{
	(void)base;
	return TRUE;
}

static gboolean sink_close(GstAudioSink *base)
{
	(void)base;
	return TRUE;
}

static gboolean sink_prepare(GstAudioSink *base, GstAudioRingBufferSpec *spec)
{
	BrowserAudioSink *sink = BROWSER_AUDIO_SINK(base);
	if (spec->type != GST_AUDIO_RING_BUFFER_FORMAT_TYPE_RAW ||
	    GST_AUDIO_INFO_FORMAT(&spec->info) != GST_AUDIO_FORMAT_S16LE)
		return FALSE;
	sink->rate = GST_AUDIO_INFO_RATE(&spec->info);
	sink->channels = GST_AUDIO_INFO_CHANNELS(&spec->info);
	sink->bpf = GST_AUDIO_INFO_BPF(&spec->info);
	/* Segments of latency-time, as many as buffer-time and what the
	 * service and the card hold ahead hold (DEVICE_US). */
	guint64 frames = gst_util_uint64_scale_int(spec->latency_time, sink->rate, G_USEC_PER_SEC);
	spec->segsize = (gint)MAX(frames, 64) * sink->bpf;
	spec->segtotal = MAX((spec->buffer_time + DEVICE_US) / MAX(spec->latency_time, 1), 2);
	return TRUE;
}

static gboolean sink_unprepare(GstAudioSink *base)
{
	BrowserAudioSink *sink = BROWSER_AUDIO_SINK(base);
	g_mutex_lock(&sink->lock);
	browser_sound_close(sink->sound);
	sink->sound = NULL;
	sink->opened = FALSE;
	sink->said_late = FALSE;
	g_mutex_unlock(&sink->lock);
	return TRUE;
}

/* Says once in the log if what the sink has written is so far ahead of
 * what is heard that the samples it is given come after it (DEVICE_US):
 * GStreamer drops them, and the sound cuts out. A sample is written into
 * the ring the latency ahead of what is heard (seglatency's segments); the
 * ring has written as far ahead as is queued below it. */
static void check_late(BrowserAudioSink *sink)
{
	if (sink->said_late)
		return;
	GstAudioRingBufferSpec *spec = &GST_AUDIO_BASE_SINK(sink)->ringbuffer->spec;
	guint segment = (guint)(spec->segsize / sink->bpf);
	guint latency = (guint)spec->seglatency * segment;
	guint queued = browser_sound_delay(sink->sound);
	if (queued + segment <= latency)
		return;
	sink->said_late = TRUE;
	g_printerr("browser: a page's sound comes late: %u ms of it are queued below the sink, which places "
		   "samples %u ms ahead of what is heard: some are dropped\n",
		   queued * 1000 / (guint)sink->rate, latency * 1000 / (guint)sink->rate);
}

static gint sink_write(GstAudioSink *base, gpointer data, guint length)
{
	BrowserAudioSink *sink = BROWSER_AUDIO_SINK(base);
	size_t frames = length / sink->bpf, done = 0;
	gint interrupts = g_atomic_int_get(&sink->interrupts);
	if (!sink->opened) {
		/* The stream is the sink's device, whose buffer is small (about
		 * 20 ms), as a GstAudioSink's writes wait for room in it: it is
		 * part of how far the ring writes ahead of what is heard, which a
		 * live source's samples must stay ahead of (DEVICE_US). */
		struct browser_sound *sound = browser_sound_open(sink->rate, sink->channels, sink->rate / 50);
		g_mutex_lock(&sink->lock);
		sink->sound = sound;
		sink->opened = TRUE;
		g_mutex_unlock(&sink->lock);
	}
	if (!sink->sound) {
		g_usleep(gst_util_uint64_scale_int(frames, G_USEC_PER_SEC, sink->rate));
		return length;
	}
	const int16_t *samples = data;
	while (done < frames && g_atomic_int_get(&sink->interrupts) == interrupts) {
		done += browser_sound_write(sink->sound, samples + done * sink->channels, frames - done);
		if (done < frames)
			browser_sound_wait(sink->sound, (uint32_t)(frames - done), WRITE_WAIT_MS);
	}
	check_late(sink);
	/* What a pause, stop or flush cut short is dropped, as the rest of the
	 * segment is. */
	return length;
}

/* Frames written that are still to be heard: the clock's lag. */
static guint sink_delay(GstAudioSink *base)
{
	BrowserAudioSink *sink = BROWSER_AUDIO_SINK(base);
	g_mutex_lock(&sink->lock);
	guint delay = sink->sound ? browser_sound_delay(sink->sound) : 0;
	g_mutex_unlock(&sink->lock);
	return delay;
}

/* Ends a write that waits for room, and pauses or empties the stream. */
static void interrupt(BrowserAudioSink *sink, int paused, gboolean flush)
{
	g_atomic_int_inc(&sink->interrupts);
	g_mutex_lock(&sink->lock);
	if (sink->sound) {
		if (paused >= 0)
			browser_sound_pause(sink->sound, paused);
		if (flush)
			browser_sound_flush(sink->sound);
	}
	g_mutex_unlock(&sink->lock);
}

static void sink_pause(GstAudioSink *base)
{
	interrupt(BROWSER_AUDIO_SINK(base), 1, FALSE);
}

static void sink_resume(GstAudioSink *base)
{
	BrowserAudioSink *sink = BROWSER_AUDIO_SINK(base);
	g_mutex_lock(&sink->lock);
	if (sink->sound)
		browser_sound_pause(sink->sound, 0);
	g_mutex_unlock(&sink->lock);
}

static void sink_stop(GstAudioSink *base)
{
	interrupt(BROWSER_AUDIO_SINK(base), -1, TRUE);
}

static void sink_reset(GstAudioSink *base)
{
	interrupt(BROWSER_AUDIO_SINK(base), -1, TRUE);
}

static void sink_finalize(GObject *object)
{
	BrowserAudioSink *sink = BROWSER_AUDIO_SINK(object);
	browser_sound_close(sink->sound);
	g_mutex_clear(&sink->lock);
	G_OBJECT_CLASS(browser_audio_sink_parent_class)->finalize(object);
}

static void browser_audio_sink_init(BrowserAudioSink *sink)
{
	g_mutex_init(&sink->lock);
	sink->sound = NULL;
	sink->opened = FALSE;
	sink->rate = sink->channels = sink->bpf = 0;
	sink->said_late = FALSE;
	sink->interrupts = 0;
}

static void browser_audio_sink_class_init(BrowserAudioSinkClass *klass)
{
	G_OBJECT_CLASS(klass)->finalize = sink_finalize;
	GstElementClass *element = GST_ELEMENT_CLASS(klass);
	gst_element_class_set_static_metadata(element, "Veda's audio service", "Sink/Audio",
					      "Plays sound through Veda's audio service", "Veda");
	gst_element_class_add_static_pad_template(element, &sink_template);
	GstAudioSinkClass *sink = GST_AUDIO_SINK_CLASS(klass);
	sink->open = sink_open;
	sink->prepare = sink_prepare;
	sink->unprepare = sink_unprepare;
	sink->close = sink_close;
	sink->write = sink_write;
	sink->delay = sink_delay;
	sink->pause = sink_pause;
	sink->resume = sink_resume;
	sink->stop = sink_stop;
	sink->reset = sink_reset;
}

void gst_init_static_plugins(void)
{
	gst_full_register_plugins();
	gst_element_register(NULL, "vedaaudiosink", GST_RANK_PRIMARY, BROWSER_TYPE_AUDIO_SINK);
}

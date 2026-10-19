/* SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * The engine's thread, which is WebKit's main thread: it runs GLib's
 * default main context, where WebKit's work and the browser's commands
 * run. Commands come from the browser's thread in a queue, run in order;
 * events go back in another, and the browser is woken (its `wake`) when
 * the first comes. Each view's page is a subsurface of the browser's window,
 * which this thread presents WebKit's frames to; the browser's thread asks
 * only which subsurface a view's is (engine_view_subsurface).
 *
 * What a page asks that waits for an answer (a dialog, a permission, a
 * menu, a download's destination) is a request: the engine keeps what
 * WebKit gave for it until the browser answers, or the view goes, which
 * gives it the answer that refuses.
 */

#include <errno.h>
#include <fontconfig/fontconfig.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>

#include "internal.h"

/* The engine thread's stack: a main thread's (as glibc gives every thread). */
#define ENGINE_STACK (8u << 20)

/* glib-networking's modules for TLS and proxies, linked in: they register
 * here what GIO would load from files. */
void g_io_openssl_load(GIOModule *module);
void g_io_environmentproxy_load(GIOModule *module);

/* WebKit's own resources, in GResource bundles linked in, which WebCore
 * looks up in the web processes: the pictures it draws (a broken image's
 * icon, a text area's resizer, the media controls' icons), Web Audio's
 * spatial sound (HRTF), and the PDF viewer (PDF.js, and the style and
 * script WebKit adds to it), in whose frame a PDF is shown. Each registers
 * itself as a process starts (its object's constructor, as WebKit's
 * library does as it loads), but a static link leaves out an object
 * nothing refers to: these do. */
GResource *WebKitResourcesGResourceBundle_get_resource(void);
GResource *ModernMediaControlsGResourceBundle_get_resource(void);
GResource *PdfJSGResourceBundle_get_resource(void);
GResource *PdfJSGResourceBundleExtras_get_resource(void);

/* WebKit's processes on Veda: this program, started again as one. */
int webkit_veda_auxiliary_process_main(int argc, char **argv);

/* What every process of the engine's GIO has, which no file brings: TLS
 * (the network process's connections) and proxies from the environment. */
static void load_gio_modules(void)
{
	g_io_openssl_load(NULL);
	g_io_environmentproxy_load(NULL);
}

/* Keeps WebKit's resources in the program, and registers them now rather
 * than at their first look-up. */
static void register_resources(void)
{
	WebKitResourcesGResourceBundle_get_resource();
	ModernMediaControlsGResourceBundle_get_resource();
	PdfJSGResourceBundle_get_resource();
	PdfJSGResourceBundleExtras_get_resource();
}

int engine_auxiliary_main(int argc, char **argv)
{
	if (!getenv("WEBKIT_PROCESS_TYPE"))
		return -1;
	load_gio_modules();
	register_resources();
	return webkit_veda_auxiliary_process_main(argc, argv);
}

/* Each request's kind. */
enum request_kind {
	REQUEST_DIALOG = 1,
	REQUEST_PERMISSION,
	REQUEST_AUTH,
	REQUEST_FILES,
	REQUEST_OPTION_MENU,
	REQUEST_CONTEXT_MENU,
	REQUEST_TLS,
	REQUEST_DOWNLOAD,
};

struct request {
	uint64_t id;
	enum request_kind kind;
	/* Its view (0 for a download, which outlives it). */
	engine_view view;
	/* What WebKit gave, referenced: a WebKitScriptDialog (boxed), or a
	 * GObject (a permission, an authentication or file chooser request,
	 * an option menu, a download, a certificate). */
	gpointer object;
	/* A context menu's items' actions (NULL for a separator) and their
	 * targets. */
	GPtrArray *actions, *targets;
	/* A TLS error's address and host. */
	char *uri, *host;
	/* A download's last progress told, and whether it failed. */
	gint64 told_at;
	double told;
	gboolean failed;
	/* The browser answered; the page closed its menu itself. */
	gboolean answered, closed;
};

struct command {
	void (*run)(struct command *c);
	engine_view view;
	uint64_t request;
	int64_t n[4];
	double d[4];
	char *s1, *s2;
	char **strv;
};

static struct {
	/* The thread's, once it runs. */
	struct engine_config config;
	char *data_dir, *cache_dir, *downloads_dir, *temporary_dir, *application;
	pthread_t thread;
	/* The thread runs (until engine_stop). */
	int running;
	GMainLoop *loop;
	WPEDisplay *display;
	WebKitWebContext *context;
	WebKitNetworkSession *session, *private_session;
	WebKitSettings *settings;
	/* The browser's window, whose subsurfaces the pages are (0: not yet
	 * known). */
	uint32_t window;
	/* engine_view -> struct view. */
	GHashTable *views;
	/* uint64_t * -> struct request. */
	GHashTable *requests;
	uint64_t next_request;

	/* Any thread's, under `lock`. */
	GMutex lock;
	GQueue commands;
	gboolean draining;
	GQueue events;
	/* engine_view -> its page's subsurface (see engine_view_subsurface). */
	GHashTable *subsurfaces;
	engine_view next_view;
	/* The thread is ready (1) or failed (-errno): engine_start waits. */
	GCond started;
	int ready;
} E;

/* --- events ----------------------------------------------------------- */

static struct engine_event *event_new(uint32_t kind, engine_view view)
{
	struct engine_event *e = g_new0(struct engine_event, 1);
	e->kind = kind;
	e->view = view;
	return e;
}

static void emit(struct engine_event *e)
{
	g_mutex_lock(&E.lock);
	gboolean first = g_queue_is_empty(&E.events);
	g_queue_push_tail(&E.events, e);
	g_mutex_unlock(&E.lock);
	if (first && E.config.wake)
		E.config.wake(E.config.wake_context);
}

static void emit_text(uint32_t kind, engine_view view, const char *text)
{
	struct engine_event *e = event_new(kind, view);
	e->text = g_strdup(text);
	emit(e);
}

static void emit_number(uint32_t kind, engine_view view, int64_t n)
{
	struct engine_event *e = event_new(kind, view);
	e->number[0] = n;
	emit(e);
}

int engine_next_event(struct engine_event *event)
{
	g_mutex_lock(&E.lock);
	struct engine_event *e = g_queue_pop_head(&E.events);
	g_mutex_unlock(&E.lock);
	if (!e)
		return 0;
	*event = *e;
	g_free(e);
	return 1;
}

void engine_event_clear(struct engine_event *event)
{
	g_free(event->text);
	g_free(event->text2);
	g_free(event->text3);
	g_free(event->data);
	memset(event, 0, sizeof *event);
}

/* --- commands --------------------------------------------------------- */

static struct command *command_new(void (*run)(struct command *), engine_view view)
{
	struct command *c = g_new0(struct command, 1);
	c->run = run;
	c->view = view;
	return c;
}

static void command_free(struct command *c)
{
	g_free(c->s1);
	g_free(c->s2);
	g_strfreev(c->strv);
	g_free(c);
}

/* Runs the commands that wait, in order. */
static gboolean drain(gpointer data)
{
	(void)data;
	for (;;) {
		g_mutex_lock(&E.lock);
		struct command *c = g_queue_pop_head(&E.commands);
		if (!c)
			E.draining = FALSE;
		g_mutex_unlock(&E.lock);
		if (!c)
			return G_SOURCE_REMOVE;
		c->run(c);
		command_free(c);
	}
}

/* Gives the engine's thread `c`, which it runs after those before it. */
static void post(struct command *c)
{
	g_mutex_lock(&E.lock);
	g_queue_push_tail(&E.commands, c);
	gboolean schedule = !E.draining;
	E.draining = TRUE;
	g_mutex_unlock(&E.lock);
	if (schedule) {
		/* Attached from here, it runs on the thread that runs the
		 * context: the engine's. */
		GSource *source = g_idle_source_new();
		g_source_set_priority(source, G_PRIORITY_DEFAULT);
		g_source_set_callback(source, drain, NULL, NULL);
		g_source_set_name(source, "browser engine commands");
		g_source_attach(source, NULL);
		g_source_unref(source);
	}
}

static struct view *view_get(engine_view id)
{
	return id ? g_hash_table_lookup(E.views, GUINT_TO_POINTER(id)) : NULL;
}

/* --- requests --------------------------------------------------------- */

static struct request *request_add(enum request_kind kind, engine_view view, gpointer object)
{
	struct request *r = g_new0(struct request, 1);
	r->id = ++E.next_request;
	r->kind = kind;
	r->view = view;
	r->object = object;
	g_hash_table_insert(E.requests, &r->id, r);
	return r;
}

static struct request *request_get(uint64_t id, enum request_kind kind)
{
	struct request *r = g_hash_table_lookup(E.requests, &id);
	return r && r->kind == kind ? r : NULL;
}

static void on_option_menu_close(WebKitOptionMenu *menu, gpointer data);

/* Lets what a request kept go; one not answered gets the answer that
 * refuses. */
static void request_free(gpointer data)
{
	struct request *r = data;
	switch (r->kind) {
	case REQUEST_DIALOG:
		/* An answer is set before: closing gives it. */
		webkit_script_dialog_close(r->object);
		webkit_script_dialog_unref(r->object);
		break;
	case REQUEST_PERMISSION:
		if (!r->answered)
			webkit_permission_request_deny(r->object);
		g_object_unref(r->object);
		break;
	case REQUEST_AUTH:
		if (!r->answered)
			webkit_authentication_request_cancel(r->object);
		g_object_unref(r->object);
		break;
	case REQUEST_FILES:
		if (!r->answered)
			webkit_file_chooser_request_cancel(r->object);
		g_object_unref(r->object);
		break;
	case REQUEST_OPTION_MENU:
		g_signal_handlers_disconnect_by_func(r->object, on_option_menu_close, r);
		if (!r->closed)
			webkit_option_menu_close(r->object);
		g_object_unref(r->object);
		break;
	case REQUEST_DOWNLOAD:
		g_signal_handlers_disconnect_by_data(r->object, r);
		g_object_unref(r->object);
		break;
	case REQUEST_TLS:
		g_clear_object(&r->object);
		break;
	case REQUEST_CONTEXT_MENU:
		break;
	}
	if (r->actions)
		g_ptr_array_unref(r->actions);
	if (r->targets)
		g_ptr_array_unref(r->targets);
	g_free(r->uri);
	g_free(r->host);
	g_free(r);
}

static void unref_object(gpointer object)
{
	if (object)
		g_object_unref(object);
}

static void unref_variant(gpointer variant)
{
	if (variant)
		g_variant_unref(variant);
}

/* Ends a request, answered or not (see request_free). */
static void request_end(struct request *r)
{
	g_hash_table_remove(E.requests, &r->id);
}

/* Ends `view`'s requests: it is going. */
static void requests_end_for(engine_view view)
{
	GHashTableIter it;
	gpointer value;
	g_hash_table_iter_init(&it, E.requests);
	while (g_hash_table_iter_next(&it, NULL, &value)) {
		if (((struct request *)value)->view == view)
			g_hash_table_iter_remove(&it);
	}
}

/* --- pages ------------------------------------------------------------ */

/* How many of WebKit's rectangles of damage a frame takes: more are their
 * bounds. How many rectangles of what changed it presents: more are merged
 * with the last. */
#define DAMAGE_MAX 16
#define CHANGES_MAX 32

/* What came of presenting a frame. */
enum presented {
	PRESENTED,
	/* The view has no subsurface: the window is not known yet, or its
	 * subsurface went to make room for others'. */
	NO_SUBSURFACE,
	/* The window system refused it (and the log says why). */
	REFUSED,
};

/* Tells the browser's thread the view's subsurface (engine_view_subsurface),
 * and the browser that it changed. */
static void publish_subsurface(struct view *view)
{
	g_mutex_lock(&E.lock);
	if (view->subsurface)
		g_hash_table_insert(E.subsurfaces, GUINT_TO_POINTER(view->id), GUINT_TO_POINTER(view->subsurface));
	else
		g_hash_table_remove(E.subsurfaces, GUINT_TO_POINTER(view->id));
	g_mutex_unlock(&E.lock);
	emit(event_new(ENGINE_EVENT_FRAME, view->id));
}

uint32_t engine_view_subsurface(engine_view view)
{
	g_mutex_lock(&E.lock);
	uint32_t subsurface = GPOINTER_TO_UINT(g_hash_table_lookup(E.subsurfaces, GUINT_TO_POINTER(view)));
	g_mutex_unlock(&E.lock);
	return subsurface;
}

/* A buffer WebKit and the engine have done with: the window system has done
 * with it too. */
static void page_buffer_gone(gpointer data)
{
	struct page_buffer *b = data;
	if (b->view) {
		g_ptr_array_remove_fast(b->view->buffers, b);
		if (b->view->presented == b)
			b->view->presented = NULL;
		if (b->subsurface)
			browser_subsurface_detach(b->subsurface, b->name);
	}
	g_free(b);
}

/* Gives the view a subsurface if it has none and the window is known:
 * whether it has one. */
static gboolean page_subsurface(struct view *view)
{
	if (view->subsurface)
		return TRUE;
	if (!E.window)
		return FALSE;
	view->subsurface = browser_subsurface_create(E.window);
	if (!view->subsurface)
		return FALSE;
	publish_subsurface(view);
	return TRUE;
}

/* Takes the view's subsurface away, and the window system's hold of its
 * buffers: its frame shown is its pending one if `keep`, and WebKit's
 * again if not; those it had yet to give back are WebKit's again. */
static void page_drop(struct view *view, gboolean keep)
{
	if (!view->subsurface)
		return;
	struct page_buffer *shown = view->presented;
	view->presented = NULL;
	browser_subsurface_destroy(view->subsurface);
	view->subsurface = 0;
	for (guint i = 0; i < view->buffers->len; i++) {
		struct page_buffer *b = g_ptr_array_index(view->buffers, i);
		if (b->held && b != shown)
			wpe_view_buffer_released(view->wpe_view, b->buffer);
		b->subsurface = 0;
		b->name = 0;
		b->held = FALSE;
	}
	if (shown && keep && !view->pending)
		view->pending = g_object_ref(shown->buffer);
	else if (shown)
		wpe_view_buffer_released(view->wpe_view, shown->buffer);
}

/* The window system has done with buffer `name` of the view's subsurface:
 * WebKit has it back. */
static void page_give_back(struct view *view, uint32_t name)
{
	for (guint i = 0; i < view->buffers->len; i++) {
		struct page_buffer *b = g_ptr_array_index(view->buffers, i);
		if (b->subsurface == view->subsurface && b->name == name) {
			if (b->held && b != view->presented) {
				b->held = FALSE;
				wpe_view_buffer_released(view->wpe_view, b->buffer);
			}
			return;
		}
	}
}

/* Makes room in the window system for `view`'s buffers: the subsurfaces of
 * the views not shown go, each keeping its latest frame to present as it is
 * shown again. Whether any went. */
static gboolean page_trim(struct view *view)
{
	GHashTableIter it;
	gpointer value;
	gboolean any = FALSE;
	g_hash_table_iter_init(&it, E.views);
	while (g_hash_table_iter_next(&it, NULL, &value)) {
		struct view *other = value;
		if (other != view && !other->shown && other->subsurface) {
			page_drop(other, TRUE);
			publish_subsurface(other);
			any = TRUE;
		}
	}
	return any;
}

/* Logs `what` the first few times, then every hundredth. */
static void page_problem(const char *what)
{
	static guint count;
	if (count < 3 || count % 100 == 0)
		g_printerr("browser: a page's frame cannot be shown: %s\n", what);
	count++;
}

/* `buffer` as attached to the view's subsurface (it is attached first if it
 * is not): NULL if the window system does not take it. */
static struct page_buffer *page_attach(struct view *view, WPEBuffer *buffer)
{
	struct page_buffer *b = wpe_buffer_get_user_data(buffer);
	if (b && b->view == view && b->subsurface == view->subsurface && b->name)
		return b;
	int fd = GPOINTER_TO_INT(g_object_get_data(G_OBJECT(buffer), "wpe-veda-memory-file")) - 1;
	if (!WPE_IS_BUFFER_SHM(buffer) || fd < 0 ||
	    wpe_buffer_shm_get_format(WPE_BUFFER_SHM(buffer)) != WPE_PIXEL_FORMAT_ARGB8888) {
		page_problem("it is not in a memory file WebKit names");
		return NULL;
	}
	if (!b) {
		b = g_new0(struct page_buffer, 1);
		b->buffer = buffer;
		b->view = view;
		g_ptr_array_add(view->buffers, b);
		wpe_buffer_set_user_data(buffer, b, page_buffer_gone);
	} else if (b->view != view) {
		page_problem("it is another page's");
		return NULL;
	}
	int width = wpe_buffer_get_width(buffer), height = wpe_buffer_get_height(buffer);
	int stride = (int)wpe_buffer_shm_get_stride(WPE_BUFFER_SHM(buffer));
	if (++view->next_name == 0)
		view->next_name = 1;
	int error = browser_subsurface_attach(view->subsurface, view->next_name, fd, width, height, stride);
	/* The buffers of the pages not in view make room for it. */
	if (error == ENOMEM && page_trim(view))
		error = browser_subsurface_attach(view->subsurface, view->next_name, fd, width, height, stride);
	if (error) {
		page_problem(error == ENOMEM ? "the window system has no room for it" : "the window system refuses it");
		return NULL;
	}
	b->subsurface = view->subsurface;
	b->name = view->next_name;
	return b;
}

/* WebKit's damage of `buffer`, `n` rectangles (none: all of it), cut to the
 * buffer, into `out` (x, y, width, height each): how many (none: nothing
 * changed). Too many to keep are their bounds. */
static unsigned page_damage(WPEBuffer *buffer, const WPERectangle *damage, guint n, int *out)
{
	int width = wpe_buffer_get_width(buffer), height = wpe_buffer_get_height(buffer);
	if (n == 0) {
		out[0] = 0, out[1] = 0, out[2] = width, out[3] = height;
		return 1;
	}
	unsigned count = 0;
	int bounds[4] = { 0, 0, 0, 0 };
	for (guint i = 0; i < n; i++) {
		int x = MAX(damage[i].x, 0), y = MAX(damage[i].y, 0);
		int w = MIN(damage[i].x + damage[i].width, width) - x;
		int h = MIN(damage[i].y + damage[i].height, height) - y;
		if (w <= 0 || h <= 0)
			continue;
		if (count < DAMAGE_MAX) {
			int *r = out + 4 * count++;
			r[0] = x, r[1] = y, r[2] = w, r[3] = h;
		}
		int x1 = bounds[2] ? MIN(bounds[0], x) : x, y1 = bounds[3] ? MIN(bounds[1], y) : y;
		int x2 = MAX(bounds[0] + bounds[2], x + w), y2 = MAX(bounds[1] + bounds[3], y + h);
		bounds[0] = x1, bounds[1] = y1, bounds[2] = x2 - x1, bounds[3] = y2 - y1;
	}
	if (n > DAMAGE_MAX) {
		memcpy(out, bounds, sizeof bounds);
		count = 1;
	}
	return count;
}

/* A buffer's pixels as the engine has them mapped, and the bytes from one
 * row to the next: NULL unless they hold all of its rows. */
static const uint8_t *page_pixels(WPEBuffer *buffer, guint *stride)
{
	if (!WPE_IS_BUFFER_SHM(buffer))
		return NULL;
	GBytes *bytes = wpe_buffer_shm_get_data(WPE_BUFFER_SHM(buffer));
	gsize size = 0;
	const uint8_t *data = bytes ? g_bytes_get_data(bytes, &size) : NULL;
	int width = wpe_buffer_get_width(buffer), height = wpe_buffer_get_height(buffer);
	*stride = wpe_buffer_shm_get_stride(WPE_BUFFER_SHM(buffer));
	gboolean whole = width > 0 && height > 0 && *stride >= (guint)width * 4 && size >= (gsize)*stride * (gsize)height;
	return data && whole ? data : NULL;
}

static inline uint64_t load64(const uint8_t *p)
{
	uint64_t v;
	memcpy(&v, p, sizeof v);
	return v;
}

static inline uint32_t load32(const uint8_t *p)
{
	uint32_t v;
	memcpy(&v, p, sizeof v);
	return v;
}

/* The first pixel at which rows `a` and `b` of `n` pixels differ (-1: none),
 * and the last (`last`): compared two at a time from either end, so that a
 * row costs as far as its first and last change, as the window's own
 * frames are compared (gfx::Damage). */
static int row_change(const uint8_t *a, const uint8_t *b, int n, int *last)
{
	int i = 0;
	while (i + 2 <= n && load64(a + 4 * i) == load64(b + 4 * i))
		i += 2;
	while (i < n && load32(a + 4 * i) == load32(b + 4 * i))
		i++;
	if (i == n)
		return -1;
	/* One past the last that may differ. */
	int j = n;
	while (j - 2 > i && load64(a + 4 * (j - 2)) == load64(b + 4 * (j - 2)))
		j -= 2;
	while (j - 1 > i && load32(a + 4 * (j - 1)) == load32(b + 4 * (j - 1)))
		j--;
	*last = j - 1;
	return i;
}

/* Adds x, y, w, h to the `*count` rectangles at `out`: the last grows to
 * take it once there are CHANGES_MAX. */
static void changes_add(int *out, unsigned *count, int x, int y, int w, int h)
{
	if (*count < CHANGES_MAX) {
		int *r = out + 4 * (*count)++;
		r[0] = x, r[1] = y, r[2] = w, r[3] = h;
		return;
	}
	int *r = out + 4 * (CHANGES_MAX - 1);
	int x1 = MIN(r[0], x), y1 = MIN(r[1], y);
	int x2 = MAX(r[0] + r[2], x + w), y2 = MAX(r[1] + r[3], y + h);
	r[0] = x1, r[1] = y1, r[2] = x2 - x1, r[3] = y2 - y1;
}

/* What did change of WebKit's `n` rectangles of damage (`damage`) between
 * the frame in `before`, which the window system shows, and the one in
 * `now`: each band of rows that changed as one rectangle as wide as its
 * changes, into `out` (CHANGES_MAX rectangles at most): how many (none:
 * nothing did). Scrolling text moves its lines, not the space between
 * them: the window system then draws what moved alone. Without a frame of
 * the same size to compare with, the damage. */
static unsigned page_changes(WPEBuffer *before, WPEBuffer *now, const int *damage, unsigned n, int *out)
{
	guint stride = 0, before_stride = 0;
	const uint8_t *b = before ? page_pixels(before, &before_stride) : NULL;
	const uint8_t *a = page_pixels(now, &stride);
	if (!a || !b || before_stride != stride || wpe_buffer_get_width(before) != wpe_buffer_get_width(now) ||
	    wpe_buffer_get_height(before) != wpe_buffer_get_height(now)) {
		memcpy(out, damage, 4 * n * sizeof *damage);
		return n;
	}
	unsigned count = 0;
	for (unsigned i = 0; i < n; i++) {
		const int *r = damage + 4 * i;
		/* The band being gathered: its first row and columns. */
		int top = -1, left = 0, right = 0;
		for (int y = r[1]; y <= r[1] + r[3]; y++) {
			int first = -1, last = 0;
			if (y < r[1] + r[3]) {
				gsize at = (gsize)y * stride + (gsize)r[0] * 4;
				first = row_change(b + at, a + at, r[2], &last);
			}
			if (first >= 0 && top < 0) {
				top = y, left = first, right = last;
			} else if (first >= 0) {
				left = MIN(left, first), right = MAX(right, last);
			} else if (top >= 0) {
				changes_add(out, &count, r[0] + left, top, right - left + 1, y - top);
				top = -1;
			}
		}
	}
	return count;
}

/* Presents `buffer`, the view's page's frame, `damage` (`n` rectangles; none:
 * all of it) having changed since the one presented before, of which what did
 * change. The buffer shown before goes back to WebKit, which may draw into it
 * again. */
static enum presented page_present(struct view *view, WPEBuffer *buffer, const WPERectangle *damage, guint n)
{
	if (!page_subsurface(view))
		return NO_SUBSURFACE;
	struct page_buffer *b = page_attach(view, buffer);
	if (!b)
		return REFUSED;
	int damaged[4 * DAMAGE_MAX], rects[4 * CHANGES_MAX];
	unsigned n_damaged = page_damage(buffer, damage, n, damaged);
	unsigned count = page_changes(view->presented ? view->presented->buffer : NULL, buffer, damaged, n_damaged, rects);
	if (count == 0) {
		/* Nothing changed (the frame is presented all the same: WebKit
		 * waits to hear it was shown): an empty rectangle says so. */
		memset(rects, 0, 4 * sizeof *rects);
		count = 1;
	}
	int64_t released = browser_subsurface_present(view->subsurface, b->name, rects, count);
	if (released == -2) {
		/* The window went with its subsurfaces (the browser closes): the
		 * pages have none from now on. */
		E.window = 0;
		page_drop(view, TRUE);
		publish_subsurface(view);
		return NO_SUBSURFACE;
	}
	if (released < 0) {
		page_problem("the window system refuses to show it");
		return REFUSED;
	}
	view->presented = b;
	b->held = TRUE;
	/* The buffer shown before, unless the GPU may still read it, which the
	 * window system then says (run_subsurface_released). */
	if (released > 0)
		page_give_back(view, (uint32_t)released);
	return PRESENTED;
}

/* The view's pending frame goes to its subsurface, if it can have one. */
static void page_restore(struct view *view)
{
	WPEBuffer *buffer = view->pending;
	if (!buffer)
		return;
	view->pending = NULL;
	switch (page_present(view, buffer, NULL, 0)) {
	case PRESENTED:
		break;
	case NO_SUBSURFACE:
		/* Kept again; what the window showed, older, goes back (the
		 * window went as it was presented). */
		if (view->pending)
			wpe_view_buffer_released(view->wpe_view, view->pending);
		g_set_object(&view->pending, buffer);
		break;
	case REFUSED:
		wpe_view_buffer_released(view->wpe_view, buffer);
		break;
	}
	g_object_unref(buffer);
}

static void run_set_window(struct command *c)
{
	E.window = (uint32_t)c->n[0];
	GHashTableIter it;
	gpointer value;
	g_hash_table_iter_init(&it, E.views);
	while (g_hash_table_iter_next(&it, NULL, &value))
		page_restore(value);
}

void engine_set_window(uint32_t window)
{
	struct command *c = command_new(run_set_window, 0);
	c->n[0] = window;
	post(c);
}

/* WebKit hears that the frame it drew last was shown: it draws the next
 * when there is one. */
static void finish_shown(struct view *view)
{
	if (view->shown_timeout) {
		g_source_remove(view->shown_timeout);
		view->shown_timeout = 0;
	}
	WPEBuffer *buffer = view->shown_buffer;
	if (!buffer)
		return;
	view->shown_buffer = NULL;
	wpe_view_buffer_rendered(view->wpe_view, buffer);
	g_object_unref(buffer);
}

static void run_subsurface_shown(struct command *c)
{
	GHashTableIter it;
	gpointer value;
	g_hash_table_iter_init(&it, E.views);
	while (g_hash_table_iter_next(&it, NULL, &value)) {
		struct view *view = value;
		if (view->subsurface == (uint32_t)c->n[0]) {
			finish_shown(view);
			return;
		}
	}
}

void engine_subsurface_shown(uint32_t subsurface)
{
	struct command *c = command_new(run_subsurface_shown, 0);
	c->n[0] = subsurface;
	post(c);
}

static void run_subsurface_released(struct command *c)
{
	GHashTableIter it;
	gpointer value;
	g_hash_table_iter_init(&it, E.views);
	while (g_hash_table_iter_next(&it, NULL, &value)) {
		struct view *view = value;
		if (view->subsurface == (uint32_t)c->n[0]) {
			page_give_back(view, (uint32_t)c->n[1]);
			return;
		}
	}
}

void engine_subsurface_released(uint32_t subsurface, uint32_t buffer)
{
	struct command *c = command_new(run_subsurface_released, 0);
	c->n[0] = subsurface;
	c->n[1] = buffer;
	post(c);
}

/* A frame the window system does not show in time (the window cannot be
 * seen, or the view is not the one in it) is as good as shown: WebKit goes
 * on, slowly. */
static gboolean shown_late(gpointer data)
{
	struct view *view = data;
	view->shown_timeout = 0;
	finish_shown(view);
	return G_SOURCE_REMOVE;
}

/* How long a frame waits to be shown before WebKit hears it was (ms). */
#define SHOWN_LATE_MS 100

void engine_buffer_rendered(struct view *view, WPEBuffer *buffer, const WPERectangle *damage, guint n)
{
	enum presented presented = page_present(view, buffer, damage, n);
	switch (presented) {
	case PRESENTED:
		/* The window system has the view's latest frame. */
		if (view->pending) {
			wpe_view_buffer_released(view->wpe_view, view->pending);
			g_clear_object(&view->pending);
		}
		break;
	case NO_SUBSURFACE:
		/* Kept, to present once the view has a subsurface. */
		if (view->pending)
			wpe_view_buffer_released(view->wpe_view, view->pending);
		g_set_object(&view->pending, buffer);
		break;
	case REFUSED:
		wpe_view_buffer_released(view->wpe_view, buffer);
		break;
	}
	/* WebKit hears the frame was shown once it is
	 * (engine_subsurface_shown), or at once if it will not be. */
	finish_shown(view);
	view->shown_buffer = g_object_ref(buffer);
	view->shown_timeout = g_timeout_add(SHOWN_LATE_MS, shown_late, view);
	if (!view->shown || presented != PRESENTED)
		finish_shown(view);
}

/* --- what the platform tells ------------------------------------------ */

void engine_cursor(struct view *view, const char *name)
{
	emit_text(ENGINE_EVENT_CURSOR, view->id, name);
}

void engine_fullscreen_requested(struct view *view, gboolean fullscreen)
{
	emit_number(ENGINE_EVENT_FULLSCREEN, view->id, fullscreen ? 1 : 0);
}

void engine_copied(const char *text)
{
	emit_text(ENGINE_EVENT_CLIPBOARD, 0, text);
}

/* --- what a page says ------------------------------------------------- */

static void tell_history(struct view *view)
{
	struct engine_event *e = event_new(ENGINE_EVENT_HISTORY, view->id);
	e->number[0] = webkit_web_view_can_go_back(view->web_view);
	e->number[1] = webkit_web_view_can_go_forward(view->web_view);
	emit(e);
}

static void tell_security(struct view *view)
{
	const char *uri = webkit_web_view_get_uri(view->web_view);
	const char *scheme = uri ? g_uri_peek_scheme(uri) : NULL;
	GTlsCertificate *certificate = NULL;
	GTlsCertificateFlags errors = 0;
	int security = ENGINE_SECURITY_LOCAL;
	if (webkit_web_view_get_tls_info(view->web_view, &certificate, &errors))
		security = errors ? ENGINE_SECURITY_BROKEN : ENGINE_SECURITY_SECURE;
	else if (scheme && (!strcmp(scheme, "http") || !strcmp(scheme, "https") || !strcmp(scheme, "ws")))
		security = ENGINE_SECURITY_INSECURE;
	emit_number(ENGINE_EVENT_SECURITY, view->id, security);
}

static void on_title(WebKitWebView *web_view, GParamSpec *spec, gpointer data)
{
	(void)spec;
	emit_text(ENGINE_EVENT_TITLE, ((struct view *)data)->id, webkit_web_view_get_title(web_view));
}

static void on_uri(WebKitWebView *web_view, GParamSpec *spec, gpointer data)
{
	(void)spec;
	emit_text(ENGINE_EVENT_URI, ((struct view *)data)->id, webkit_web_view_get_uri(web_view));
}

static void on_progress(WebKitWebView *web_view, GParamSpec *spec, gpointer data)
{
	(void)spec;
	struct engine_event *e = event_new(ENGINE_EVENT_PROGRESS, ((struct view *)data)->id);
	e->value = webkit_web_view_get_estimated_load_progress(web_view);
	emit(e);
}

static void on_load_changed(WebKitWebView *web_view, WebKitLoadEvent load, gpointer data)
{
	(void)web_view;
	struct view *view = data;
	int what = 0;
	switch (load) {
	case WEBKIT_LOAD_STARTED:
		what = ENGINE_LOAD_STARTED;
		break;
	case WEBKIT_LOAD_REDIRECTED:
		what = ENGINE_LOAD_REDIRECTED;
		break;
	case WEBKIT_LOAD_COMMITTED:
		what = ENGINE_LOAD_COMMITTED;
		tell_security(view);
		break;
	case WEBKIT_LOAD_FINISHED:
		what = ENGINE_LOAD_FINISHED;
		break;
	}
	emit_number(ENGINE_EVENT_LOAD, view->id, what);
	if (load == WEBKIT_LOAD_COMMITTED || load == WEBKIT_LOAD_FINISHED)
		tell_history(view);
}

static gboolean on_load_failed(WebKitWebView *web_view, WebKitLoadEvent load, const char *uri, GError *error,
			       gpointer data)
{
	(void)web_view;
	(void)load;
	struct view *view = data;
	int why = ENGINE_FAILED_OTHER;
	if (g_error_matches(error, WEBKIT_NETWORK_ERROR, WEBKIT_NETWORK_ERROR_CANCELLED))
		why = ENGINE_FAILED_CANCELLED;
	else if (g_error_matches(error, WEBKIT_POLICY_ERROR, WEBKIT_POLICY_ERROR_FRAME_LOAD_INTERRUPTED_BY_POLICY_CHANGE) ||
		 g_error_matches(error, WEBKIT_POLICY_ERROR, WEBKIT_POLICY_ERROR_CANNOT_SHOW_MIME_TYPE))
		why = ENGINE_FAILED_CONTENT;
	else if (g_error_matches(error, G_RESOLVER_ERROR, G_RESOLVER_ERROR_NOT_FOUND))
		why = ENGINE_FAILED_HOST;
	else if (error->domain == WEBKIT_NETWORK_ERROR || error->domain == G_IO_ERROR ||
		 error->domain == G_RESOLVER_ERROR)
		why = ENGINE_FAILED_NETWORK;
	struct engine_event *e = event_new(ENGINE_EVENT_LOAD_FAILED, view->id);
	e->text = g_strdup(uri);
	e->text2 = g_strdup(error->message);
	e->number[0] = why;
	emit(e);
	/* The browser shows what failed: no page of WebKit's own. */
	return TRUE;
}

static gboolean on_tls_error(WebKitWebView *web_view, const char *uri, GTlsCertificate *certificate,
			     GTlsCertificateFlags errors, gpointer data)
{
	(void)web_view;
	struct view *view = data;
	struct request *r = request_add(REQUEST_TLS, view->id, g_object_ref(certificate));
	GUri *parsed = g_uri_parse(uri, G_URI_FLAGS_NONE, NULL);
	r->uri = g_strdup(uri);
	r->host = g_strdup(parsed ? g_uri_get_host(parsed) : NULL);
	if (parsed)
		g_uri_unref(parsed);
	struct engine_event *e = event_new(ENGINE_EVENT_TLS_ERROR, view->id);
	e->request = r->id;
	e->text = g_strdup(uri);
	e->text2 = g_strdup(r->host);
	g_object_get(certificate, "subject-name", &e->text3, NULL);
	static const struct {
		GTlsCertificateFlags flag;
		int ours;
	} flags[] = {
		{ G_TLS_CERTIFICATE_UNKNOWN_CA, ENGINE_TLS_UNKNOWN_CA },
		{ G_TLS_CERTIFICATE_BAD_IDENTITY, ENGINE_TLS_BAD_IDENTITY },
		{ G_TLS_CERTIFICATE_NOT_ACTIVATED, ENGINE_TLS_NOT_ACTIVATED },
		{ G_TLS_CERTIFICATE_EXPIRED, ENGINE_TLS_EXPIRED },
		{ G_TLS_CERTIFICATE_REVOKED, ENGINE_TLS_REVOKED },
		{ G_TLS_CERTIFICATE_INSECURE, ENGINE_TLS_INSECURE },
		{ G_TLS_CERTIFICATE_GENERIC_ERROR, ENGINE_TLS_GENERIC },
	};
	for (size_t i = 0; i < G_N_ELEMENTS(flags); i++) {
		if (errors & flags[i].flag)
			e->number[0] |= flags[i].ours;
	}
	emit(e);
	return TRUE;
}

static gboolean on_decide_policy(WebKitWebView *web_view, WebKitPolicyDecision *decision, WebKitPolicyDecisionType type,
				 gpointer data)
{
	(void)web_view;
	(void)data;
	if (type != WEBKIT_POLICY_DECISION_TYPE_RESPONSE)
		return FALSE;
	WebKitResponsePolicyDecision *response = WEBKIT_RESPONSE_POLICY_DECISION(decision);
	/* What cannot be shown is downloaded, if it is the page's own. */
	if (!webkit_response_policy_decision_is_mime_type_supported(response) &&
	    webkit_response_policy_decision_is_main_frame_main_resource(response)) {
		webkit_policy_decision_download(decision);
		return TRUE;
	}
	return FALSE;
}

static void on_page_icons(WebKitWebView *web_view, GParamSpec *spec, gpointer data)
{
	(void)spec;
	struct view *view = data;
	WebKitImageList *icons = webkit_web_view_get_page_icons(web_view);
	gsize count = icons ? webkit_image_list_get_length(icons) : 0;
	/* The smallest one at least 32 pixels wide, or else the largest. */
	WebKitImage *best = NULL;
	for (gsize i = 0; i < count; i++) {
		WebKitImage *image = webkit_image_list_get(icons, i);
		int w = webkit_image_get_width(image), bw = best ? webkit_image_get_width(best) : 0;
		if (w <= 0 || w > 512)
			continue;
		if (!best || (bw < 32 && w > bw) || (w >= 32 && w < bw))
			best = image;
	}
	struct engine_event *e = event_new(ENGINE_EVENT_ICON, view->id);
	if (best) {
		int w = webkit_image_get_width(best), h = webkit_image_get_height(best);
		guint stride = webkit_image_get_stride(best);
		GBytes *bytes = webkit_image_as_bytes(best);
		gsize size = 0;
		const uint8_t *src = bytes ? g_bytes_get_data(bytes, &size) : NULL;
		if (src && h > 0 && h <= 512 && stride >= (guint)w * 4 && size >= (gsize)stride * (gsize)h) {
			e->data = g_malloc((gsize)w * (gsize)h * 4);
			e->data_len = (gsize)w * (gsize)h * 4;
			for (int row = 0; row < h; row++)
				memcpy(e->data + (gsize)row * (gsize)w * 4, src + (gsize)row * stride, (gsize)w * 4);
			e->number[0] = w;
			e->number[1] = h;
		}
	}
	emit(e);
}

static void on_mouse_target(WebKitWebView *web_view, WebKitHitTestResult *hit, guint modifiers, gpointer data)
{
	(void)web_view;
	(void)modifiers;
	struct view *view = data;
	const char *link = webkit_hit_test_result_context_is_link(hit) ? webkit_hit_test_result_get_link_uri(hit) : NULL;
	if (g_strcmp0(link, view->hovered) == 0)
		return;
	g_free(view->hovered);
	view->hovered = g_strdup(link);
	emit_text(ENGINE_EVENT_HOVER, view->id, link);
}

static struct view *view_make(engine_view id, gboolean ephemeral, struct view *related);

static WebKitWebView *on_create(WebKitWebView *web_view, WebKitNavigationAction *action, gpointer data)
{
	(void)web_view;
	(void)action;
	struct view *opener = data;
	g_mutex_lock(&E.lock);
	engine_view id = ++E.next_view;
	g_mutex_unlock(&E.lock);
	struct view *view = view_make(id, opener->ephemeral, opener);
	emit_number(ENGINE_EVENT_NEW_VIEW, id, opener->id);
	return view->web_view;
}

static void on_close(WebKitWebView *web_view, gpointer data)
{
	(void)web_view;
	emit(event_new(ENGINE_EVENT_CLOSE, ((struct view *)data)->id));
}

static void on_terminated(WebKitWebView *web_view, WebKitWebProcessTerminationReason reason, gpointer data)
{
	(void)web_view;
	int why = ENGINE_CRASH_CRASHED;
	if (reason == WEBKIT_WEB_PROCESS_EXCEEDED_MEMORY_LIMIT)
		why = ENGINE_CRASH_MEMORY;
	else if (reason == WEBKIT_WEB_PROCESS_TERMINATED_BY_API)
		why = ENGINE_CRASH_TERMINATED;
	emit_number(ENGINE_EVENT_CRASHED, ((struct view *)data)->id, why);
}

static void on_insecure_content(WebKitWebView *web_view, WebKitInsecureContentEvent event, gpointer data)
{
	(void)web_view;
	(void)event;
	emit(event_new(ENGINE_EVENT_INSECURE_CONTENT, ((struct view *)data)->id));
}

static gboolean on_script_dialog(WebKitWebView *web_view, WebKitScriptDialog *dialog, gpointer data)
{
	(void)web_view;
	struct view *view = data;
	int kind = ENGINE_DIALOG_ALERT;
	switch (webkit_script_dialog_get_dialog_type(dialog)) {
	case WEBKIT_SCRIPT_DIALOG_ALERT:
		kind = ENGINE_DIALOG_ALERT;
		break;
	case WEBKIT_SCRIPT_DIALOG_CONFIRM:
		kind = ENGINE_DIALOG_CONFIRM;
		break;
	case WEBKIT_SCRIPT_DIALOG_PROMPT:
		kind = ENGINE_DIALOG_PROMPT;
		break;
	case WEBKIT_SCRIPT_DIALOG_BEFORE_UNLOAD_CONFIRM:
		kind = ENGINE_DIALOG_LEAVE;
		break;
	}
	struct request *r = request_add(REQUEST_DIALOG, view->id, webkit_script_dialog_ref(dialog));
	struct engine_event *e = event_new(ENGINE_EVENT_DIALOG, view->id);
	e->request = r->id;
	e->number[0] = kind;
	e->text = g_strdup(webkit_script_dialog_get_message(dialog));
	if (kind == ENGINE_DIALOG_PROMPT)
		e->text2 = g_strdup(webkit_script_dialog_prompt_get_default_text(dialog));
	emit(e);
	return TRUE;
}

static gboolean on_permission(WebKitWebView *web_view, WebKitPermissionRequest *request, gpointer data)
{
	struct view *view = data;
	int kind = ENGINE_PERMISSION_OTHER;
	if (WEBKIT_IS_GEOLOCATION_PERMISSION_REQUEST(request))
		kind = ENGINE_PERMISSION_GEOLOCATION;
	else if (WEBKIT_IS_NOTIFICATION_PERMISSION_REQUEST(request))
		kind = ENGINE_PERMISSION_NOTIFICATIONS;
	else if (WEBKIT_IS_WEBSITE_DATA_ACCESS_PERMISSION_REQUEST(request))
		kind = ENGINE_PERMISSION_STORAGE;
	else if (WEBKIT_IS_DEVICE_INFO_PERMISSION_REQUEST(request))
		kind = ENGINE_PERMISSION_DEVICE_INFO;
	else if (WEBKIT_IS_USER_MEDIA_PERMISSION_REQUEST(request) || WEBKIT_IS_MEDIA_KEY_SYSTEM_PERMISSION_REQUEST(request))
		kind = ENGINE_PERMISSION_MEDIA;
	struct request *r = request_add(REQUEST_PERMISSION, view->id, g_object_ref(request));
	struct engine_event *e = event_new(ENGINE_EVENT_PERMISSION, view->id);
	e->request = r->id;
	e->number[0] = kind;
	/* The site that asks: the page's. */
	const char *uri = webkit_web_view_get_uri(web_view);
	GUri *parsed = uri ? g_uri_parse(uri, G_URI_FLAGS_NONE, NULL) : NULL;
	e->text = g_strdup(parsed && g_uri_get_host(parsed) ? g_uri_get_host(parsed) : uri);
	if (parsed)
		g_uri_unref(parsed);
	emit(e);
	return TRUE;
}

static gboolean on_authenticate(WebKitWebView *web_view, WebKitAuthenticationRequest *request, gpointer data)
{
	(void)web_view;
	struct view *view = data;
	struct request *r = request_add(REQUEST_AUTH, view->id, g_object_ref(request));
	struct engine_event *e = event_new(ENGINE_EVENT_AUTH, view->id);
	e->request = r->id;
	e->text = g_strdup(webkit_authentication_request_get_host(request));
	e->text2 = g_strdup(webkit_authentication_request_get_realm(request));
	e->number[0] = webkit_authentication_request_is_for_proxy(request);
	emit(e);
	return TRUE;
}

static gboolean on_file_chooser(WebKitWebView *web_view, WebKitFileChooserRequest *request, gpointer data)
{
	(void)web_view;
	struct view *view = data;
	struct request *r = request_add(REQUEST_FILES, view->id, g_object_ref(request));
	struct engine_event *e = event_new(ENGINE_EVENT_FILE_CHOOSER, view->id);
	e->request = r->id;
	e->number[0] = webkit_file_chooser_request_get_select_multiple(request);
	const char *const *types = webkit_file_chooser_request_get_mime_types(request);
	if (types && *types)
		e->text = g_strjoinv("\n", (char **)types);
	emit(e);
	return TRUE;
}

/* Labels, '\n' between them, and a byte of ENGINE_ITEM_* each. */
struct items {
	GString *labels;
	GByteArray *flags;
};

static void items_add(struct items *items, const char *label, uint8_t flags)
{
	if (items->flags->len)
		g_string_append_c(items->labels, '\n');
	/* No line ends within a label. */
	for (const char *p = label ? label : ""; *p; p++)
		g_string_append_c(items->labels, *p == '\n' || *p == '\r' ? ' ' : *p);
	g_byte_array_append(items->flags, &flags, 1);
}

static void items_into(struct items *items, struct engine_event *e)
{
	e->text = g_string_free(items->labels, FALSE);
	e->data_len = items->flags->len;
	e->data = g_byte_array_free(items->flags, FALSE);
}

static void on_option_menu_close(WebKitOptionMenu *menu, gpointer data)
{
	(void)menu;
	struct request *r = data;
	struct engine_event *e = event_new(ENGINE_EVENT_MENU_CLOSED, r->view);
	e->request = r->id;
	emit(e);
	r->closed = TRUE;
	request_end(r);
}

static gboolean on_option_menu(WebKitWebView *web_view, WebKitOptionMenu *menu, WebKitRectangle *where, gpointer data)
{
	(void)web_view;
	struct view *view = data;
	struct request *r = request_add(REQUEST_OPTION_MENU, view->id, g_object_ref(menu));
	g_signal_connect(menu, "close", G_CALLBACK(on_option_menu_close), r);
	struct engine_event *e = event_new(ENGINE_EVENT_OPTION_MENU, view->id);
	e->request = r->id;
	struct items items = { g_string_new(NULL), g_byte_array_new() };
	guint n = webkit_option_menu_get_n_items(menu);
	for (guint i = 0; i < n; i++) {
		WebKitOptionMenuItem *item = webkit_option_menu_get_item(menu, i);
		uint8_t flags = 0;
		if (webkit_option_menu_item_is_enabled(item))
			flags |= ENGINE_ITEM_ENABLED;
		if (webkit_option_menu_item_is_selected(item))
			flags |= ENGINE_ITEM_SELECTED;
		if (webkit_option_menu_item_is_group_label(item))
			flags |= ENGINE_ITEM_GROUP_LABEL;
		if (webkit_option_menu_item_is_group_child(item))
			flags |= ENGINE_ITEM_GROUP_CHILD;
		items_add(&items, webkit_option_menu_item_get_label(item), flags);
	}
	items_into(&items, e);
	if (where) {
		e->number[0] = where->x;
		e->number[1] = where->y;
		e->number[2] = where->width;
		e->number[3] = where->height;
	}
	emit(e);
	return TRUE;
}

static gboolean on_context_menu(WebKitWebView *web_view, WebKitContextMenu *menu, WebKitHitTestResult *hit,
				gpointer data)
{
	(void)web_view;
	struct view *view = data;
	/* The menu is emptied once this returns; the actions its items take
	 * are kept, which WebKit sets up for the page then. */
	struct request *r = request_add(REQUEST_CONTEXT_MENU, view->id, NULL);
	r->actions = g_ptr_array_new_with_free_func(unref_object);
	r->targets = g_ptr_array_new_with_free_func(unref_variant);
	struct engine_event *e = event_new(ENGINE_EVENT_CONTEXT_MENU, view->id);
	e->request = r->id;
	struct items items = { g_string_new(NULL), g_byte_array_new() };
	guint n = webkit_context_menu_get_n_items(menu);
	for (guint i = 0; i < n; i++) {
		WebKitContextMenuItem *item = webkit_context_menu_get_item_at_position(menu, i);
		GAction *action = webkit_context_menu_item_is_separator(item) ? NULL : webkit_context_menu_item_get_gaction(item);
		GVariant *target = webkit_context_menu_item_get_gaction_target(item);
		/* Submenus (input methods, spelling) are left out. */
		if (!webkit_context_menu_item_is_separator(item) && (!action || webkit_context_menu_item_get_submenu(item)))
			continue;
		uint8_t flags = 0;
		if (!action)
			flags |= ENGINE_ITEM_SEPARATOR;
		else if (g_action_get_enabled(action))
			flags |= ENGINE_ITEM_ENABLED;
		g_ptr_array_add(r->actions, action ? g_object_ref(action) : NULL);
		g_ptr_array_add(r->targets, target ? g_variant_ref(target) : NULL);
		items_add(&items, action ? webkit_context_menu_item_get_title(item) : "", flags);
	}
	items_into(&items, e);
	if (webkit_hit_test_result_context_is_link(hit))
		e->text2 = g_strdup(webkit_hit_test_result_get_link_uri(hit));
	if (webkit_hit_test_result_context_is_image(hit))
		e->text3 = g_strdup(webkit_hit_test_result_get_image_uri(hit));
	emit(e);
	return TRUE;
}

static void on_found(WebKitFindController *finder, guint matches, gpointer data)
{
	(void)finder;
	emit_number(ENGINE_EVENT_FOUND, ((struct view *)data)->id, matches);
}

static void on_not_found(WebKitFindController *finder, gpointer data)
{
	(void)finder;
	emit_number(ENGINE_EVENT_FOUND, ((struct view *)data)->id, 0);
}

static void on_history_changed(WebKitBackForwardList *list, WebKitBackForwardListItem *added, gpointer removed,
			       gpointer data)
{
	(void)list;
	(void)added;
	(void)removed;
	tell_history(data);
}

/* --- downloads -------------------------------------------------------- */

static gboolean on_decide_destination(WebKitDownload *download, const char *suggested, gpointer data)
{
	struct request *r = data;
	struct engine_event *e = event_new(ENGINE_EVENT_DOWNLOAD, 0);
	e->request = r->id;
	WebKitURIRequest *request = webkit_download_get_request(download);
	e->text = g_strdup(request ? webkit_uri_request_get_uri(request) : NULL);
	e->text2 = g_strdup(suggested);
	WebKitURIResponse *response = webkit_download_get_response(download);
	guint64 length = response ? webkit_uri_response_get_content_length(response) : 0;
	e->number[0] = length ? (int64_t)length : -1;
	emit(e);
	/* The browser decides where it goes (engine_download_reply). */
	return TRUE;
}

static void on_received(WebKitDownload *download, guint64 length, gpointer data)
{
	(void)length;
	struct request *r = data;
	gint64 now = g_get_monotonic_time();
	double fraction = webkit_download_get_estimated_progress(download);
	/* Ten times a second, at most. */
	if (now - r->told_at < G_USEC_PER_SEC / 10 && fraction - r->told < 0.05)
		return;
	r->told_at = now;
	r->told = fraction;
	struct engine_event *e = event_new(ENGINE_EVENT_DOWNLOAD_PROGRESS, 0);
	e->request = r->id;
	e->number[0] = (int64_t)webkit_download_get_received_data_length(download);
	e->value = fraction;
	emit(e);
}

static void on_download_failed(WebKitDownload *download, GError *error, gpointer data)
{
	(void)download;
	struct request *r = data;
	r->failed = TRUE;
	struct engine_event *e = event_new(ENGINE_EVENT_DOWNLOAD_DONE, 0);
	e->request = r->id;
	e->number[0] = g_error_matches(error, WEBKIT_DOWNLOAD_ERROR, WEBKIT_DOWNLOAD_ERROR_CANCELLED_BY_USER) ? 2 : 1;
	e->text = g_strdup(error->message);
	emit(e);
}

static void on_download_finished(WebKitDownload *download, gpointer data)
{
	struct request *r = data;
	if (!r->failed) {
		struct engine_event *e = event_new(ENGINE_EVENT_DOWNLOAD_DONE, 0);
		e->request = r->id;
		e->number[0] = 0;
		e->value = 1;
		e->text = g_strdup(webkit_download_get_destination(download));
		emit(e);
	}
	request_end(r);
}

static void on_download_started(WebKitNetworkSession *session, WebKitDownload *download, gpointer data)
{
	(void)session;
	(void)data;
	struct request *r = request_add(REQUEST_DOWNLOAD, 0, g_object_ref(download));
	webkit_download_set_allow_overwrite(download, FALSE);
	g_signal_connect(download, "decide-destination", G_CALLBACK(on_decide_destination), r);
	g_signal_connect(download, "received-data", G_CALLBACK(on_received), r);
	g_signal_connect(download, "failed", G_CALLBACK(on_download_failed), r);
	g_signal_connect(download, "finished", G_CALLBACK(on_download_finished), r);
}

/* --- views ------------------------------------------------------------ */

/* The session that keeps nothing, made with the first private view (and
 * again with the first after the last has gone). */
static WebKitNetworkSession *private_session(void)
{
	if (!E.private_session) {
		E.private_session = webkit_network_session_new_ephemeral();
		webkit_network_session_set_itp_enabled(E.private_session, TRUE);
		webkit_cookie_manager_set_accept_policy(webkit_network_session_get_cookie_manager(E.private_session),
							E.config.block_third_party_cookies
								? WEBKIT_COOKIE_POLICY_ACCEPT_NO_THIRD_PARTY
								: WEBKIT_COOKIE_POLICY_ACCEPT_ALWAYS);
		g_signal_connect(E.private_session, "download-started", G_CALLBACK(on_download_started), NULL);
	}
	return E.private_session;
}

static struct view *view_make(engine_view id, gboolean ephemeral, struct view *related)
{
	struct view *view = g_new0(struct view, 1);
	view->id = id;
	view->buffers = g_ptr_array_new();
	view->scale = 1;
	if (related) {
		view->web_view = WEBKIT_WEB_VIEW(g_object_new(WEBKIT_TYPE_WEB_VIEW, "display", E.display, "related-view",
							      related->web_view, "settings", E.settings, NULL));
	} else {
		view->web_view = WEBKIT_WEB_VIEW(g_object_new(
			WEBKIT_TYPE_WEB_VIEW, "display", E.display, "web-context", E.context, "network-session",
			ephemeral ? private_session() : E.session, "settings", E.settings, NULL));
	}
	view->ephemeral = webkit_network_session_is_ephemeral(webkit_web_view_get_network_session(view->web_view));
	view->wpe_view = webkit_web_view_get_wpe_view(view->web_view);
	view->toplevel = wpe_view_get_toplevel(view->wpe_view);
	platform_view_attach(view->wpe_view, view);
	g_hash_table_insert(E.views, GUINT_TO_POINTER(id), view);

	WebKitWebView *w = view->web_view;
	g_signal_connect(w, "notify::title", G_CALLBACK(on_title), view);
	g_signal_connect(w, "notify::uri", G_CALLBACK(on_uri), view);
	g_signal_connect(w, "notify::estimated-load-progress", G_CALLBACK(on_progress), view);
	g_signal_connect(w, "notify::page-icons", G_CALLBACK(on_page_icons), view);
	g_signal_connect(w, "load-changed", G_CALLBACK(on_load_changed), view);
	g_signal_connect(w, "load-failed", G_CALLBACK(on_load_failed), view);
	g_signal_connect(w, "load-failed-with-tls-errors", G_CALLBACK(on_tls_error), view);
	g_signal_connect(w, "decide-policy", G_CALLBACK(on_decide_policy), view);
	g_signal_connect(w, "mouse-target-changed", G_CALLBACK(on_mouse_target), view);
	g_signal_connect(w, "create", G_CALLBACK(on_create), view);
	g_signal_connect(w, "close", G_CALLBACK(on_close), view);
	g_signal_connect(w, "web-process-terminated", G_CALLBACK(on_terminated), view);
	g_signal_connect(w, "insecure-content-detected", G_CALLBACK(on_insecure_content), view);
	g_signal_connect(w, "script-dialog", G_CALLBACK(on_script_dialog), view);
	g_signal_connect(w, "permission-request", G_CALLBACK(on_permission), view);
	g_signal_connect(w, "authenticate", G_CALLBACK(on_authenticate), view);
	g_signal_connect(w, "run-file-chooser", G_CALLBACK(on_file_chooser), view);
	g_signal_connect(w, "show-option-menu", G_CALLBACK(on_option_menu), view);
	g_signal_connect(w, "context-menu", G_CALLBACK(on_context_menu), view);
	WebKitFindController *finder = webkit_web_view_get_find_controller(w);
	g_signal_connect(finder, "found-text", G_CALLBACK(on_found), view);
	g_signal_connect(finder, "failed-to-find-text", G_CALLBACK(on_not_found), view);
	g_signal_connect(webkit_web_view_get_back_forward_list(w), "changed", G_CALLBACK(on_history_changed), view);
	return view;
}

static void view_free(struct view *view)
{
	requests_end_for(view->id);
	g_signal_handlers_disconnect_by_data(view->web_view, view);
	g_signal_handlers_disconnect_by_data(webkit_web_view_get_find_controller(view->web_view), view);
	g_signal_handlers_disconnect_by_data(webkit_web_view_get_back_forward_list(view->web_view), view);
	finish_shown(view);
	/* Its page goes from the window system, and its buffers back to
	 * WebKit, before the view does. */
	page_drop(view, FALSE);
	if (view->pending) {
		wpe_view_buffer_released(view->wpe_view, view->pending);
		g_clear_object(&view->pending);
	}
	for (guint i = 0; i < view->buffers->len; i++)
		((struct page_buffer *)g_ptr_array_index(view->buffers, i))->view = NULL;
	g_ptr_array_unref(view->buffers);
	g_mutex_lock(&E.lock);
	g_hash_table_remove(E.subsurfaces, GUINT_TO_POINTER(view->id));
	g_mutex_unlock(&E.lock);
	platform_view_detach(view->wpe_view);
	g_object_unref(view->web_view);
	g_free(view->hovered);
	g_free(view->finding);
	g_free(view);
}

static void run_create(struct command *c)
{
	view_make(c->view, c->n[0], view_get((engine_view)c->n[1]));
}

engine_view engine_view_create(int ephemeral, engine_view related)
{
	g_mutex_lock(&E.lock);
	engine_view id = ++E.next_view;
	g_mutex_unlock(&E.lock);
	struct command *c = command_new(run_create, id);
	c->n[0] = ephemeral;
	c->n[1] = related;
	post(c);
	return id;
}

/* No private view is left: the private session goes, with what it kept
 * (cookies, storage, cache), and the next private view starts afresh. */
static void private_session_end_if_unused(void)
{
	GHashTableIter it;
	gpointer value;
	if (!E.private_session)
		return;
	g_hash_table_iter_init(&it, E.views);
	while (g_hash_table_iter_next(&it, NULL, &value))
		if (((struct view *)value)->ephemeral)
			return;
	g_clear_object(&E.private_session);
}

static void run_destroy(struct command *c)
{
	struct view *view = view_get(c->view);
	if (!view)
		return;
	gboolean ephemeral = view->ephemeral;
	g_hash_table_remove(E.views, GUINT_TO_POINTER(c->view));
	view_free(view);
	if (ephemeral)
		private_session_end_if_unused();
}

void engine_view_destroy(engine_view view)
{
	post(command_new(run_destroy, view));
}

static void run_load(struct command *c)
{
	struct view *view = view_get(c->view);
	if (view)
		webkit_web_view_load_uri(view->web_view, c->s1);
}

void engine_view_load(engine_view view, const char *uri)
{
	struct command *c = command_new(run_load, view);
	c->s1 = g_strdup(uri);
	post(c);
}

static void run_reload(struct command *c)
{
	struct view *view = view_get(c->view);
	if (!view)
		return;
	if (c->n[0])
		webkit_web_view_reload_bypass_cache(view->web_view);
	else
		webkit_web_view_reload(view->web_view);
}

void engine_view_reload(engine_view view, int bypass_cache)
{
	struct command *c = command_new(run_reload, view);
	c->n[0] = bypass_cache;
	post(c);
}

static void run_stop(struct command *c)
{
	struct view *view = view_get(c->view);
	if (view)
		webkit_web_view_stop_loading(view->web_view);
}

void engine_view_stop(engine_view view)
{
	post(command_new(run_stop, view));
}

static void run_go(struct command *c)
{
	struct view *view = view_get(c->view);
	if (!view)
		return;
	if (c->n[0] < 0)
		webkit_web_view_go_back(view->web_view);
	else if (c->n[0] > 0)
		webkit_web_view_go_forward(view->web_view);
}

void engine_view_go(engine_view view, int steps)
{
	struct command *c = command_new(run_go, view);
	c->n[0] = steps;
	post(c);
}

/* The page's window: as large as the page area, active while the window
 * has the focus, full screen as the browser made it. */
static void update_toplevel(struct view *view, gboolean active)
{
	if (!view->toplevel)
		return;
	WPEToplevelState state = WPE_TOPLEVEL_STATE_NONE;
	if (active)
		state |= WPE_TOPLEVEL_STATE_ACTIVE;
	if (view->fullscreen)
		state |= WPE_TOPLEVEL_STATE_FULLSCREEN;
	if (wpe_toplevel_get_state(view->toplevel) != state)
		wpe_toplevel_state_changed(view->toplevel, state);
}

static void run_resize(struct command *c)
{
	struct view *view = view_get(c->view);
	if (!view)
		return;
	int width = MAX((int)c->n[0], 1), height = MAX((int)c->n[1], 1);
	double scale = c->d[0] > 0 ? c->d[0] : 1;
	if (view->toplevel) {
		if (scale != view->scale)
			wpe_toplevel_scale_changed(view->toplevel, scale);
		wpe_toplevel_resized(view->toplevel, width, height);
	}
	if (width != view->width || height != view->height)
		wpe_view_resized(view->wpe_view, width, height);
	view->width = width;
	view->height = height;
	view->scale = scale;
}

void engine_view_resize(engine_view view, int width, int height, double scale)
{
	struct command *c = command_new(run_resize, view);
	c->n[0] = width;
	c->n[1] = height;
	c->d[0] = scale;
	post(c);
}

static void run_show(struct command *c)
{
	struct view *view = view_get(c->view);
	if (!view || view->shown == (gboolean)c->n[0])
		return;
	view->shown = c->n[0];
	if (view->shown) {
		wpe_view_map(view->wpe_view);
		/* Its page's subsurface went to make room for others': its latest
		 * frame comes back with a new one. */
		page_restore(view);
	} else {
		wpe_view_unmap(view->wpe_view);
		finish_shown(view);
	}
}

void engine_view_show(engine_view view, int shown)
{
	struct command *c = command_new(run_show, view);
	c->n[0] = shown ? 1 : 0;
	post(c);
}

static void run_focus(struct command *c)
{
	struct view *view = view_get(c->view);
	if (!view)
		return;
	update_toplevel(view, c->n[0]);
	gboolean focused = c->n[1];
	if (focused != view->focused) {
		view->focused = focused;
		if (focused)
			wpe_view_focus_in(view->wpe_view);
		else
			wpe_view_focus_out(view->wpe_view);
	}
}

void engine_view_focus(engine_view view, int active, int focused)
{
	struct command *c = command_new(run_focus, view);
	c->n[0] = active ? 1 : 0;
	c->n[1] = focused ? 1 : 0;
	post(c);
}

static void run_zoom(struct command *c)
{
	struct view *view = view_get(c->view);
	if (view && c->d[0] > 0)
		webkit_web_view_set_zoom_level(view->web_view, c->d[0]);
}

void engine_view_zoom(engine_view view, double level)
{
	struct command *c = command_new(run_zoom, view);
	c->d[0] = level;
	post(c);
}

static void run_fullscreen(struct command *c)
{
	struct view *view = view_get(c->view);
	if (!view)
		return;
	view->fullscreen = c->n[0];
	update_toplevel(view, view->toplevel && (wpe_toplevel_get_state(view->toplevel) & WPE_TOPLEVEL_STATE_ACTIVE));
}

void engine_view_fullscreen(engine_view view, int fullscreen)
{
	struct command *c = command_new(run_fullscreen, view);
	c->n[0] = fullscreen ? 1 : 0;
	post(c);
}

/* --- input ------------------------------------------------------------ */

static WPEModifiers keyboard_modifiers(uint32_t modifiers)
{
	WPEModifiers m = 0;
	if (modifiers & ENGINE_SHIFT)
		m |= WPE_MODIFIER_KEYBOARD_SHIFT;
	if (modifiers & ENGINE_CONTROL)
		m |= WPE_MODIFIER_KEYBOARD_CONTROL;
	if (modifiers & ENGINE_ALT)
		m |= WPE_MODIFIER_KEYBOARD_ALT;
	if (modifiers & ENGINE_META)
		m |= WPE_MODIFIER_KEYBOARD_META;
	if (modifiers & ENGINE_CAPS_LOCK)
		m |= WPE_MODIFIER_KEYBOARD_CAPS_LOCK;
	return m;
}

static guint32 event_time(void)
{
	return (guint32)(g_get_monotonic_time() / 1000);
}

/* The buttons held, as WPE's modifiers say them. */
static WPEModifiers buttons_held;
static double last_x, last_y;

static WPEModifiers button_modifier(uint32_t button)
{
	switch (button) {
	case 1:
		return WPE_MODIFIER_POINTER_BUTTON1;
	case 2:
		return WPE_MODIFIER_POINTER_BUTTON2;
	case 3:
		return WPE_MODIFIER_POINTER_BUTTON3;
	case 8:
		return WPE_MODIFIER_POINTER_BUTTON4;
	case 9:
		return WPE_MODIFIER_POINTER_BUTTON5;
	default:
		return 0;
	}
}

static void run_pointer(struct command *c)
{
	struct view *view = view_get(c->view);
	if (!view)
		return;
	uint32_t kind = (uint32_t)c->n[0], button = (uint32_t)c->n[1], clicks = (uint32_t)c->n[2];
	double x = c->d[0], y = c->d[1];
	WPEModifiers keys = keyboard_modifiers((uint32_t)c->n[3]);
	platform_set_modifiers(E.display, keys);
	WPEEvent *event = NULL;
	switch (kind) {
	case ENGINE_POINTER_MOVE:
		event = wpe_event_pointer_move_new(WPE_EVENT_POINTER_MOVE, view->wpe_view, WPE_INPUT_SOURCE_MOUSE,
						   event_time(), keys | buttons_held, x, y, x - last_x, y - last_y);
		break;
	case ENGINE_POINTER_DOWN:
		buttons_held |= button_modifier(button);
		event = wpe_event_pointer_button_new(WPE_EVENT_POINTER_DOWN, view->wpe_view, WPE_INPUT_SOURCE_MOUSE,
						     event_time(), keys | buttons_held, button, x, y, MAX(clicks, 1));
		break;
	case ENGINE_POINTER_UP:
		buttons_held &= ~button_modifier(button);
		event = wpe_event_pointer_button_new(WPE_EVENT_POINTER_UP, view->wpe_view, WPE_INPUT_SOURCE_MOUSE,
						     event_time(), keys | buttons_held, button, x, y, 0);
		break;
	case ENGINE_POINTER_LEAVE:
		event = wpe_event_pointer_move_new(WPE_EVENT_POINTER_LEAVE, view->wpe_view, WPE_INPUT_SOURCE_MOUSE,
						   event_time(), keys | buttons_held, x, y, 0, 0);
		break;
	}
	last_x = x;
	last_y = y;
	if (event) {
		wpe_view_event(view->wpe_view, event);
		wpe_event_unref(event);
	}
}

void engine_view_pointer(engine_view view, uint32_t kind, double x, double y, uint32_t button, uint32_t clicks,
			 uint32_t modifiers)
{
	struct command *c = command_new(run_pointer, view);
	c->n[0] = kind;
	c->n[1] = button;
	c->n[2] = clicks;
	c->n[3] = modifiers;
	c->d[0] = x;
	c->d[1] = y;
	post(c);
}

static void run_scroll(struct command *c)
{
	struct view *view = view_get(c->view);
	if (!view)
		return;
	WPEModifiers keys = keyboard_modifiers((uint32_t)c->n[0]);
	WPEEvent *event = wpe_event_scroll_new(view->wpe_view, WPE_INPUT_SOURCE_MOUSE, event_time(), keys | buttons_held,
					       c->d[2], c->d[3], FALSE, FALSE, c->d[0], c->d[1]);
	wpe_view_event(view->wpe_view, event);
	wpe_event_unref(event);
}

void engine_view_scroll(engine_view view, double x, double y, double dx, double dy, uint32_t modifiers)
{
	struct command *c = command_new(run_scroll, view);
	c->n[0] = modifiers;
	c->d[0] = x;
	c->d[1] = y;
	c->d[2] = dx;
	c->d[3] = dy;
	post(c);
}

static void run_key(struct command *c)
{
	struct view *view = view_get(c->view);
	if (!view)
		return;
	WPEModifiers keys = keyboard_modifiers((uint32_t)c->n[3]);
	platform_set_modifiers(E.display, keys);
	WPEEvent *event =
		wpe_event_keyboard_new(c->n[0] ? WPE_EVENT_KEYBOARD_KEY_DOWN : WPE_EVENT_KEYBOARD_KEY_UP, view->wpe_view,
				       WPE_INPUT_SOURCE_KEYBOARD, event_time(), keys, (guint)c->n[1], (guint)c->n[2]);
	wpe_view_event(view->wpe_view, event);
	wpe_event_unref(event);
}

void engine_view_key(engine_view view, int pressed, uint32_t keycode, uint32_t keysym, uint32_t modifiers)
{
	struct command *c = command_new(run_key, view);
	c->n[0] = pressed ? 1 : 0;
	c->n[1] = keycode;
	c->n[2] = keysym;
	c->n[3] = modifiers;
	post(c);
}

/* --- what the browser asks of a page --------------------------------- */

static void run_find(struct command *c)
{
	struct view *view = view_get(c->view);
	if (!view)
		return;
	WebKitFindController *finder = webkit_web_view_get_find_controller(view->web_view);
	g_free(view->finding);
	view->finding = g_strdup(c->s1);
	if (!c->s1 || !*c->s1) {
		webkit_find_controller_search_finish(finder);
		return;
	}
	guint32 options = WEBKIT_FIND_OPTIONS_WRAP_AROUND;
	if (!(c->n[0] & ENGINE_FIND_CASE_SENSITIVE))
		options |= WEBKIT_FIND_OPTIONS_CASE_INSENSITIVE;
	if (c->n[0] & ENGINE_FIND_BACKWARDS)
		options |= WEBKIT_FIND_OPTIONS_BACKWARDS;
	webkit_find_controller_search(finder, c->s1, options, G_MAXUINT);
}

void engine_view_find(engine_view view, const char *text, uint32_t flags)
{
	struct command *c = command_new(run_find, view);
	c->s1 = g_strdup(text);
	c->n[0] = flags;
	post(c);
}

static void run_find_next(struct command *c)
{
	struct view *view = view_get(c->view);
	if (!view || !view->finding)
		return;
	WebKitFindController *finder = webkit_web_view_get_find_controller(view->web_view);
	if (c->n[0])
		webkit_find_controller_search_previous(finder);
	else
		webkit_find_controller_search_next(finder);
}

void engine_view_find_next(engine_view view, int backwards)
{
	struct command *c = command_new(run_find_next, view);
	c->n[0] = backwards ? 1 : 0;
	post(c);
}

struct script {
	engine_view view;
	uint64_t request;
};

static void script_done(GObject *source, GAsyncResult *result, gpointer data)
{
	struct script *s = data;
	GError *error = NULL;
	JSCValue *value = webkit_web_view_evaluate_javascript_finish(WEBKIT_WEB_VIEW(source), result, &error);
	struct engine_event *e = event_new(ENGINE_EVENT_SCRIPT_RESULT, s->view);
	e->request = s->request;
	if (value) {
		e->text = jsc_value_to_json(value, 0);
		/* What JSON cannot say (undefined, a function) is null. */
		if (!e->text)
			e->text = g_strdup("null");
		g_object_unref(value);
	} else {
		e->text2 = g_strdup(error ? error->message : "the script failed");
		g_clear_error(&error);
	}
	emit(e);
	g_free(s);
}

static void run_script(struct command *c)
{
	struct view *view = view_get(c->view);
	struct script *s = g_new0(struct script, 1);
	s->view = c->view;
	s->request = c->request;
	if (!view) {
		struct engine_event *e = event_new(ENGINE_EVENT_SCRIPT_RESULT, c->view);
		e->request = c->request;
		e->text2 = g_strdup("the page is gone");
		emit(e);
		g_free(s);
		return;
	}
	webkit_web_view_evaluate_javascript(view->web_view, c->s1, -1, NULL, NULL, NULL, script_done, s);
}

void engine_view_run_script(engine_view view, uint64_t request, const char *script)
{
	struct command *c = command_new(run_script, view);
	c->request = request;
	c->s1 = g_strdup(script);
	post(c);
}

static void run_clipboard(struct command *c)
{
	platform_set_clipboard(E.display, c->s1);
}

void engine_clipboard(const char *text)
{
	struct command *c = command_new(run_clipboard, 0);
	c->s1 = g_strdup(text);
	post(c);
}

static void data_cleared(GObject *source, GAsyncResult *result, gpointer data)
{
	(void)data;
	webkit_website_data_manager_clear_finish(WEBKIT_WEBSITE_DATA_MANAGER(source), result, NULL);
}

static void run_clear_data(struct command *c)
{
	uint32_t what = (uint32_t)c->n[0];
	WebKitWebsiteDataTypes types = 0;
	if (what & ENGINE_DATA_COOKIES)
		types |= WEBKIT_WEBSITE_DATA_COOKIES | WEBKIT_WEBSITE_DATA_HSTS_CACHE | WEBKIT_WEBSITE_DATA_ITP;
	if (what & ENGINE_DATA_CACHE)
		types |= WEBKIT_WEBSITE_DATA_MEMORY_CACHE | WEBKIT_WEBSITE_DATA_DISK_CACHE | WEBKIT_WEBSITE_DATA_DOM_CACHE;
	if (what & ENGINE_DATA_STORAGE)
		types |= WEBKIT_WEBSITE_DATA_LOCAL_STORAGE | WEBKIT_WEBSITE_DATA_SESSION_STORAGE |
			 WEBKIT_WEBSITE_DATA_INDEXEDDB_DATABASES | WEBKIT_WEBSITE_DATA_OFFLINE_APPLICATION_CACHE |
			 WEBKIT_WEBSITE_DATA_SERVICE_WORKER_REGISTRATIONS | WEBKIT_WEBSITE_DATA_DEVICE_ID_HASH_SALT;
	GTimeSpan since = c->n[1] > 0 ? c->n[1] * G_TIME_SPAN_SECOND : 0;
	WebKitWebsiteDataManager *manager = webkit_network_session_get_website_data_manager(E.session);
	if (types)
		webkit_website_data_manager_clear(manager, types, since, NULL, data_cleared, NULL);
	if (what & ENGINE_DATA_ICONS) {
		WebKitFaviconDatabase *icons = webkit_website_data_manager_get_favicon_database(manager);
		if (icons)
			webkit_favicon_database_clear(icons);
	}
}

void engine_clear_data(uint32_t what, int64_t seconds)
{
	struct command *c = command_new(run_clear_data, 0);
	c->n[0] = what;
	c->n[1] = seconds;
	post(c);
}

/* --- answers ---------------------------------------------------------- */

static void run_dialog_reply(struct command *c)
{
	struct request *r = request_get(c->request, REQUEST_DIALOG);
	if (!r)
		return;
	WebKitScriptDialog *dialog = r->object;
	switch (webkit_script_dialog_get_dialog_type(dialog)) {
	case WEBKIT_SCRIPT_DIALOG_CONFIRM:
	case WEBKIT_SCRIPT_DIALOG_BEFORE_UNLOAD_CONFIRM:
		webkit_script_dialog_confirm_set_confirmed(dialog, c->n[0]);
		break;
	case WEBKIT_SCRIPT_DIALOG_PROMPT:
		/* Cancelled, a prompt gives null. */
		if (c->n[0])
			webkit_script_dialog_prompt_set_text(dialog, c->s1 ? c->s1 : "");
		break;
	case WEBKIT_SCRIPT_DIALOG_ALERT:
		break;
	}
	/* request_free closes it. */
	request_end(r);
}

void engine_dialog_reply(uint64_t request, int confirmed, const char *text)
{
	struct command *c = command_new(run_dialog_reply, 0);
	c->request = request;
	c->n[0] = confirmed ? 1 : 0;
	c->s1 = g_strdup(text);
	post(c);
}

static void run_permission_reply(struct command *c)
{
	struct request *r = request_get(c->request, REQUEST_PERMISSION);
	if (!r)
		return;
	if (c->n[0]) {
		webkit_permission_request_allow(r->object);
		r->answered = TRUE;
	}
	request_end(r);
}

void engine_permission_reply(uint64_t request, int allow)
{
	struct command *c = command_new(run_permission_reply, 0);
	c->request = request;
	c->n[0] = allow ? 1 : 0;
	post(c);
}

static void run_auth_reply(struct command *c)
{
	struct request *r = request_get(c->request, REQUEST_AUTH);
	if (!r)
		return;
	if (c->s1) {
		WebKitCredential *credential =
			webkit_credential_new(c->s1, c->s2 ? c->s2 : "", WEBKIT_CREDENTIAL_PERSISTENCE_FOR_SESSION);
		webkit_authentication_request_authenticate(r->object, credential);
		webkit_credential_free(credential);
		r->answered = TRUE;
	}
	request_end(r);
}

void engine_auth_reply(uint64_t request, const char *user, const char *password)
{
	struct command *c = command_new(run_auth_reply, 0);
	c->request = request;
	c->s1 = g_strdup(user);
	c->s2 = g_strdup(password);
	post(c);
}

static void run_files_reply(struct command *c)
{
	struct request *r = request_get(c->request, REQUEST_FILES);
	if (!r)
		return;
	if (c->strv && c->strv[0]) {
		webkit_file_chooser_request_select_files(r->object, (const char *const *)c->strv);
		r->answered = TRUE;
	}
	request_end(r);
}

void engine_files_reply(uint64_t request, const char *const *paths, size_t count)
{
	struct command *c = command_new(run_files_reply, 0);
	c->request = request;
	c->strv = g_new0(char *, count + 1);
	for (size_t i = 0; i < count; i++)
		c->strv[i] = g_strdup(paths[i]);
	post(c);
}

static void run_menu_reply(struct command *c)
{
	struct request *r = g_hash_table_lookup(E.requests, &c->request);
	if (!r)
		return;
	int index = (int)c->n[0];
	if (r->kind == REQUEST_OPTION_MENU) {
		if (index >= 0 && (guint)index < webkit_option_menu_get_n_items(r->object))
			webkit_option_menu_activate_item(r->object, (guint)index);
	} else if (r->kind == REQUEST_CONTEXT_MENU) {
		if (index >= 0 && (guint)index < r->actions->len) {
			GAction *action = g_ptr_array_index(r->actions, index);
			if (action && g_action_get_enabled(action))
				g_action_activate(action, g_ptr_array_index(r->targets, index));
		}
	} else {
		return;
	}
	request_end(r);
}

void engine_menu_reply(uint64_t request, int index)
{
	struct command *c = command_new(run_menu_reply, 0);
	c->request = request;
	c->n[0] = index;
	post(c);
}

static void run_tls_reply(struct command *c)
{
	struct request *r = request_get(c->request, REQUEST_TLS);
	if (!r)
		return;
	struct view *view = view_get(r->view);
	if (c->n[0] && view && r->host && r->object) {
		webkit_network_session_allow_tls_certificate_for_host(webkit_web_view_get_network_session(view->web_view),
								      r->object, r->host);
		webkit_web_view_load_uri(view->web_view, r->uri);
	}
	request_end(r);
}

void engine_tls_reply(uint64_t request, int proceed)
{
	struct command *c = command_new(run_tls_reply, 0);
	c->request = request;
	c->n[0] = proceed ? 1 : 0;
	post(c);
}

static void run_download_reply(struct command *c)
{
	struct request *r = request_get(c->request, REQUEST_DOWNLOAD);
	if (!r)
		return;
	if (c->s1)
		webkit_download_set_destination(r->object, c->s1);
	else
		webkit_download_cancel(r->object);
}

void engine_download_reply(uint64_t request, const char *path)
{
	struct command *c = command_new(run_download_reply, 0);
	c->request = request;
	c->s1 = g_strdup(path);
	post(c);
}

static void run_download_cancel(struct command *c)
{
	struct request *r = request_get(c->request, REQUEST_DOWNLOAD);
	if (r)
		webkit_download_cancel(r->object);
}

void engine_download_cancel(uint64_t request)
{
	struct command *c = command_new(run_download_cancel, 0);
	c->request = request;
	post(c);
}

/* --- the engine ------------------------------------------------------- */

/* Turns one of WebKit's features (WebKitFeature) on or off. */
static void set_feature(WebKitSettings *settings, const char *identifier, gboolean enabled)
{
	WebKitFeatureList *all = webkit_settings_get_all_features();
	for (gsize i = 0; i < webkit_feature_list_get_length(all); i++) {
		WebKitFeature *feature = webkit_feature_list_get(all, i);
		if (!strcmp(webkit_feature_get_identifier(feature), identifier))
			webkit_settings_set_feature_enabled(settings, feature, enabled);
	}
	webkit_feature_list_unref(all);
}

/* WebKit's settings for every view: what a browser on Veda shows. */
static WebKitSettings *settings_new(void)
{
	WebKitSettings *settings = webkit_settings_new();
	const char *slash = E.application ? strchr(E.application, '/') : NULL;
	if (slash) {
		char *name = g_strndup(E.application, (gsize)(slash - E.application));
		webkit_settings_set_user_agent_with_application_details(settings, name, slash + 1);
		g_free(name);
	} else if (E.application) {
		webkit_settings_set_user_agent_with_application_details(settings, E.application, NULL);
	}
	if (E.config.accelerated) {
		/* Layers composited on the GPU, with OpenGL ES, by WebKit's
		 * TextureMapper (whose few simple shaders virgl takes as they are,
		 * rather than by Skia's GPU back end); their contents painted on the
		 * CPU by Skia (see engine_start), canvases too. */
		webkit_settings_set_hardware_acceleration_policy(settings, WEBKIT_HARDWARE_ACCELERATION_POLICY_ALWAYS);
		set_feature(settings, "UseSkiaForComposition", FALSE);
		webkit_settings_set_enable_2d_canvas_acceleration(settings, FALSE);
	} else {
		/* Each page painted on the CPU, by Skia, into the shared memory the
		 * frames come in. */
		webkit_settings_set_hardware_acceleration_policy(settings, WEBKIT_HARDWARE_ACCELERATION_POLICY_NEVER);
	}
	webkit_settings_set_enable_developer_extras(settings, FALSE);
	webkit_settings_set_javascript_can_open_windows_automatically(settings, FALSE);
	webkit_settings_set_enable_smooth_scrolling(settings, TRUE);
	webkit_settings_set_enable_page_cache(settings, TRUE);
	/* The fonts fontconfig knows (/system/etc/fonts): the system's. */
	webkit_settings_set_default_font_family(settings, "sans-serif");
	webkit_settings_set_sans_serif_font_family(settings, "sans-serif");
	webkit_settings_set_serif_font_family(settings, "serif");
	webkit_settings_set_monospace_font_family(settings, "monospace");
	return settings;
}

static gpointer engine_main(gpointer data)
{
	(void)data;
	load_gio_modules();
	/* The default context is this thread's from now on: commands posted to
	 * it run here, never where they are posted. */
	g_main_context_acquire(NULL);

	E.display = platform_display_new(E.config.screen_width, E.config.screen_height,
					 E.config.scale > 0 ? E.config.scale : 1, E.config.refresh_rate);
	GError *error = NULL;
	if (!wpe_display_connect(E.display, &error)) {
		g_warning("the display: %s", error ? error->message : "?");
		g_clear_error(&error);
		g_mutex_lock(&E.lock);
		E.ready = -EIO;
		g_cond_signal(&E.started);
		g_mutex_unlock(&E.lock);
		return NULL;
	}
	wpe_display_set_primary(E.display);

	E.context = webkit_web_context_new();
	webkit_web_context_set_cache_model(E.context, WEBKIT_CACHE_MODEL_WEB_BROWSER);
	E.session = webkit_network_session_new(E.data_dir, E.cache_dir);
	webkit_network_session_set_itp_enabled(E.session, TRUE);
	webkit_network_session_set_tls_errors_policy(E.session, WEBKIT_TLS_ERRORS_POLICY_FAIL);
	WebKitCookieManager *cookies = webkit_network_session_get_cookie_manager(E.session);
	webkit_cookie_manager_set_accept_policy(cookies, E.config.block_third_party_cookies
								 ? WEBKIT_COOKIE_POLICY_ACCEPT_NO_THIRD_PARTY
								 : WEBKIT_COOKIE_POLICY_ACCEPT_ALWAYS);
	if (E.data_dir) {
		char *path = g_build_filename(E.data_dir, "cookies.sqlite", NULL);
		webkit_cookie_manager_set_persistent_storage(cookies, path, WEBKIT_COOKIE_PERSISTENT_STORAGE_SQLITE);
		g_free(path);
	}
	webkit_website_data_manager_set_favicons_enabled(webkit_network_session_get_website_data_manager(E.session),
							 TRUE);
	g_signal_connect(E.session, "download-started", G_CALLBACK(on_download_started), NULL);
	E.settings = settings_new();

	E.loop = g_main_loop_new(NULL, FALSE);
	g_mutex_lock(&E.lock);
	E.ready = 1;
	g_cond_signal(&E.started);
	g_mutex_unlock(&E.lock);

	/* fontconfig's cache of the system's fonts (/tmp/fontconfig), which the
	 * web processes, sandboxed, only read: made here if it is not there
	 * yet, before the loop starts the first of them. */
	FcInit();

	g_main_loop_run(E.loop);

	/* The views are gone (engine_stop ran last); the rest goes too. */
	g_hash_table_remove_all(E.requests);
	g_clear_object(&E.settings);
	g_clear_object(&E.private_session);
	g_clear_object(&E.session);
	g_clear_object(&E.context);
	g_clear_object(&E.display);
	g_main_loop_unref(E.loop);
	E.loop = NULL;
	g_main_context_release(NULL);
	return NULL;
}

int engine_start(const struct engine_config *config)
{
	if (E.running)
		return EBUSY;
	E.config = *config;
	E.data_dir = g_strdup(config->data_dir);
	E.cache_dir = g_strdup(config->cache_dir);
	E.downloads_dir = g_strdup(config->downloads_dir);
	E.temporary_dir = g_strdup(config->temporary_dir);
	E.application = g_strdup(config->application);
	E.config.data_dir = E.data_dir;
	E.config.cache_dir = E.cache_dir;
	E.config.downloads_dir = E.downloads_dir;
	E.config.temporary_dir = E.temporary_dir;
	E.config.application = E.application;
	/* Pages are painted on the CPU (Skia's own rasterizer, on WebKit's
	 * painting threads), whatever composites them: WebKit's processes, which
	 * this environment goes to, read it as they start. */
	if (E.config.accelerated)
		g_setenv("WEBKIT_SKIA_ENABLE_CPU_RENDERING", "1", TRUE);
	/* The services the web processes reach, sandboxed (WebKit's
	 * WebProcess::initializeSandbox on Veda), which the browser's own code
	 * in them needs: the audio service, which media.c's sink plays pages'
	 * sound through, and the GPU's, while pages are composited. */
	g_setenv("WEBKIT_VEDA_WEB_PROCESS_SERVICES", E.config.accelerated ? "audio,gpu" : "audio", TRUE);
	/* The user's files the network process reaches, sandboxed (WebKit's
	 * NetworkProcess::initializeSandbox on Veda), each to read and change:
	 * the engine's data and cache, the downloads, and the temporary files,
	 * where it keeps its own (TMPDIR, which every process of the engine
	 * has) and the browser copies the files pages are to upload. A page
	 * loaded from the user's files lends it its folder too, to read
	 * (webkit_web_view_load_uri, WebKit's UIProcess/veda/FileLoansVeda.cpp). */
	const char *places[] = { E.data_dir, E.cache_dir, E.downloads_dir, E.temporary_dir };
	GString *files = g_string_new(NULL);
	for (size_t i = 0; i < G_N_ELEMENTS(places); i++) {
		if (places[i] && *places[i])
			g_string_append_printf(files, "%s%s", files->len ? ":" : "", places[i]);
	}
	g_setenv("WEBKIT_VEDA_NETWORK_PROCESS_FILES", files->str, TRUE);
	g_string_free(files, TRUE);
	if (E.temporary_dir && *E.temporary_dir)
		g_setenv("TMPDIR", E.temporary_dir, TRUE);
	/* Pages' media buffer in memory, as much as GStreamer's queues hold,
	 * never in a file of their own: the web processes write no files, and
	 * a file would keep what a page played where other programs read. */
	g_setenv("WPE_SHELL_DISABLE_MEDIA_DISK_CACHE", "1", TRUE);
	g_mutex_init(&E.lock);
	g_cond_init(&E.started);
	g_queue_init(&E.commands);
	g_queue_init(&E.events);
	E.views = g_hash_table_new(NULL, NULL);
	E.subsurfaces = g_hash_table_new(NULL, NULL);
	E.requests = g_hash_table_new_full(g_int64_hash, g_int64_equal, NULL, request_free);
	E.ready = 0;
	/* WebKit runs on this thread as on a program's main thread, and so
	 * does JavaScript (the results of pages' scripts are this process's):
	 * it has a main thread's stack, which JavaScriptCore needs room in
	 * (musl's threads have 128 KiB). Its pages are used as it grows. */
	pthread_attr_t attr;
	pthread_attr_init(&attr);
	pthread_attr_setstacksize(&attr, ENGINE_STACK);
	int r = pthread_create(&E.thread, &attr, engine_main, NULL);
	pthread_attr_destroy(&attr);
	if (r)
		return r;
	E.running = 1;
	g_mutex_lock(&E.lock);
	while (!E.ready)
		g_cond_wait(&E.started, &E.lock);
	int ready = E.ready;
	g_mutex_unlock(&E.lock);
	if (ready < 0) {
		pthread_join(E.thread, NULL);
		E.running = 0;
		return -ready;
	}
	return 0;
}

static void run_stop_engine(struct command *c)
{
	(void)c;
	GHashTableIter it;
	gpointer value;
	g_hash_table_iter_init(&it, E.views);
	while (g_hash_table_iter_next(&it, NULL, &value)) {
		g_hash_table_iter_steal(&it);
		view_free(value);
	}
	g_main_loop_quit(E.loop);
}

void engine_stop(void)
{
	if (!E.running)
		return;
	post(command_new(run_stop_engine, 0));
	pthread_join(E.thread, NULL);
	E.running = 0;
}

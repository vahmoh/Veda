/* SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * WebKit's platform on Veda: WPE's display, views, windows, screen,
 * keymap and clipboard, as the browser's window is to WebKit.
 *
 * Each view is a tab's page and has a window (a WPEToplevel) of its own,
 * which is the browser's window as that page sees it: as large as the page
 * area, active while the window has the focus, full screen when the page
 * asked and the browser made it so. Frames come in shared memory (they are
 * WPEBufferSHM's): WebKit paints them there on the processor, or composites
 * them with OpenGL ES and reads them back into it (EGL's surfaceless
 * display has no buffers to share). It hands each to the view
 * (render_buffer), which gives it to the engine.
 *
 * Key events come with their key symbols, which the browser works out
 * from what its window system says (no XKB keymap is loaded): the keymap
 * only knows the modifiers held.
 */

#include <string.h>

#include "internal.h"

/* --- the screen ------------------------------------------------------- */

#define BROWSER_TYPE_SCREEN (browser_screen_get_type())
G_DECLARE_FINAL_TYPE(BrowserScreen, browser_screen, BROWSER, SCREEN, WPEScreen)

struct _BrowserScreen {
	WPEScreen parent;
};

G_DEFINE_FINAL_TYPE(BrowserScreen, browser_screen, WPE_TYPE_SCREEN)

static void browser_screen_init(BrowserScreen *screen)
{
	(void)screen;
}

static void browser_screen_class_init(BrowserScreenClass *klass)
{
	(void)klass;
}

/* --- the keymap ------------------------------------------------------- */

#define BROWSER_TYPE_KEYMAP (browser_keymap_get_type())
G_DECLARE_FINAL_TYPE(BrowserKeymap, browser_keymap, BROWSER, KEYMAP, WPEKeymap)

struct _BrowserKeymap {
	WPEKeymap parent;
	WPEModifiers modifiers;
};

G_DEFINE_FINAL_TYPE(BrowserKeymap, browser_keymap, WPE_TYPE_KEYMAP)

/* Key symbols come with each event: there is nothing to look up. */
static gboolean keymap_entries_for_keyval(WPEKeymap *keymap, guint keyval, WPEKeymapEntry **entries, guint *n)
{
	(void)keymap;
	(void)keyval;
	*entries = NULL;
	*n = 0;
	return FALSE;
}

static gboolean keymap_translate(WPEKeymap *keymap, guint keycode, WPEModifiers modifiers, int group, guint *keyval,
				 int *effective_group, int *level, WPEModifiers *consumed)
{
	(void)keymap;
	(void)keycode;
	(void)modifiers;
	(void)group;
	(void)keyval;
	(void)effective_group;
	(void)level;
	(void)consumed;
	return FALSE;
}

static WPEModifiers keymap_modifiers(WPEKeymap *keymap)
{
	return BROWSER_KEYMAP(keymap)->modifiers;
}

static void browser_keymap_init(BrowserKeymap *keymap)
{
	keymap->modifiers = 0;
}

static void browser_keymap_class_init(BrowserKeymapClass *klass)
{
	WPEKeymapClass *keymap = WPE_KEYMAP_CLASS(klass);
	keymap->get_entries_for_keyval = keymap_entries_for_keyval;
	keymap->translate_keyboard_state = keymap_translate;
	keymap->get_modifiers = keymap_modifiers;
}

/* --- the clipboard ---------------------------------------------------- */

/* What pages copy goes to the system's clipboard (through the browser);
 * what they paste is the system's, as the browser last gave it. */
#define BROWSER_TYPE_CLIPBOARD (browser_clipboard_get_type())
G_DECLARE_FINAL_TYPE(BrowserClipboard, browser_clipboard, BROWSER, CLIPBOARD, WPEClipboard)

struct _BrowserClipboard {
	WPEClipboard parent;
	/* The system's text. */
	char *text;
};

G_DEFINE_FINAL_TYPE(BrowserClipboard, browser_clipboard, WPE_TYPE_CLIPBOARD)

static void clipboard_changed(WPEClipboard *clipboard, GPtrArray *formats, gboolean local, WPEClipboardContent *content)
{
	WPE_CLIPBOARD_CLASS(browser_clipboard_parent_class)->changed(clipboard, formats, local, content);
	const char *text = local && content ? wpe_clipboard_content_get_text(content) : NULL;
	if (text)
		engine_copied(text);
}

static GBytes *clipboard_read(WPEClipboard *clipboard, const char *format)
{
	BrowserClipboard *self = BROWSER_CLIPBOARD(clipboard);
	if (!self->text || !g_str_has_prefix(format, "text/plain"))
		return NULL;
	return g_bytes_new(self->text, strlen(self->text));
}

static void clipboard_finalize(GObject *object)
{
	g_free(BROWSER_CLIPBOARD(object)->text);
	G_OBJECT_CLASS(browser_clipboard_parent_class)->finalize(object);
}

static void browser_clipboard_init(BrowserClipboard *clipboard)
{
	clipboard->text = NULL;
}

static void browser_clipboard_class_init(BrowserClipboardClass *klass)
{
	G_OBJECT_CLASS(klass)->finalize = clipboard_finalize;
	WPEClipboardClass *clipboard = WPE_CLIPBOARD_CLASS(klass);
	clipboard->changed = clipboard_changed;
	clipboard->read = clipboard_read;
}

/* --- views ------------------------------------------------------------ */

#define BROWSER_TYPE_VIEW (browser_view_get_type())
G_DECLARE_FINAL_TYPE(BrowserView, browser_view, BROWSER, VIEW, WPEView)

struct _BrowserView {
	WPEView parent;
	/* The engine's view, once it is made (NULL before, and after it). */
	struct view *owner;
};

G_DEFINE_FINAL_TYPE(BrowserView, browser_view, WPE_TYPE_VIEW)

static gboolean view_render_buffer(WPEView *wpe_view, WPEBuffer *buffer, const WPERectangle *damage, guint n,
				   GError **error)
{
	(void)error;
	struct view *view = BROWSER_VIEW(wpe_view)->owner;
	if (view) {
		engine_buffer_rendered(view, buffer, damage, n);
	} else {
		/* Nobody to show it: as if it was shown. */
		wpe_view_buffer_rendered(wpe_view, buffer);
		wpe_view_buffer_released(wpe_view, buffer);
	}
	return TRUE;
}

static void view_set_cursor_from_name(WPEView *wpe_view, const char *name)
{
	struct view *view = BROWSER_VIEW(wpe_view)->owner;
	if (view)
		engine_cursor(view, name);
}

/* A cursor of the page's own pictures: the system's pointer stands in. */
static void view_set_cursor_from_bytes(WPEView *wpe_view, GBytes *bytes, guint width, guint height, guint stride,
				       guint hotspot_x, guint hotspot_y)
{
	(void)bytes;
	(void)width;
	(void)height;
	(void)stride;
	(void)hotspot_x;
	(void)hotspot_y;
	view_set_cursor_from_name(wpe_view, "default");
}

/* The pointer goes where it goes: Veda has no pointer lock yet. */
static gboolean view_lock_pointer(WPEView *view)
{
	(void)view;
	return FALSE;
}

static gboolean view_can_be_mapped(WPEView *view)
{
	(void)view;
	return TRUE;
}

static void browser_view_init(BrowserView *view)
{
	view->owner = NULL;
}

static void browser_view_class_init(BrowserViewClass *klass)
{
	WPEViewClass *view = WPE_VIEW_CLASS(klass);
	view->render_buffer = view_render_buffer;
	view->set_cursor_from_name = view_set_cursor_from_name;
	view->set_cursor_from_bytes = view_set_cursor_from_bytes;
	view->lock_pointer = view_lock_pointer;
	view->unlock_pointer = view_lock_pointer;
	view->can_be_mapped = view_can_be_mapped;
}

/* --- windows ---------------------------------------------------------- */

#define BROWSER_TYPE_TOPLEVEL (browser_toplevel_get_type())
G_DECLARE_FINAL_TYPE(BrowserToplevel, browser_toplevel, BROWSER, TOPLEVEL, WPEToplevel)

struct _BrowserToplevel {
	WPEToplevel parent;
	struct view *owner;
};

G_DEFINE_FINAL_TYPE(BrowserToplevel, browser_toplevel, WPE_TYPE_TOPLEVEL)

/* The browser sizes its window, not the pages in it. */
static gboolean toplevel_resize(WPEToplevel *toplevel, int width, int height)
{
	(void)toplevel;
	(void)width;
	(void)height;
	return FALSE;
}

/* The page asks for the whole screen, or gives it back: the browser
 * decides, and says what came of it (engine_view_fullscreen). */
static gboolean toplevel_set_fullscreen(WPEToplevel *toplevel, gboolean fullscreen)
{
	struct view *view = BROWSER_TOPLEVEL(toplevel)->owner;
	if (!view)
		return FALSE;
	engine_fullscreen_requested(view, fullscreen);
	return TRUE;
}

static gboolean toplevel_set_maximized(WPEToplevel *toplevel, gboolean maximized)
{
	(void)toplevel;
	(void)maximized;
	return FALSE;
}

static gboolean toplevel_set_minimized(WPEToplevel *toplevel)
{
	(void)toplevel;
	return FALSE;
}

static WPEScreen *toplevel_get_screen(WPEToplevel *toplevel)
{
	WPEDisplay *display = wpe_toplevel_get_display(toplevel);
	return display ? wpe_display_get_screen(display, 0) : NULL;
}

static void browser_toplevel_init(BrowserToplevel *toplevel)
{
	toplevel->owner = NULL;
}

static void browser_toplevel_class_init(BrowserToplevelClass *klass)
{
	WPEToplevelClass *toplevel = WPE_TOPLEVEL_CLASS(klass);
	toplevel->resize = toplevel_resize;
	toplevel->set_fullscreen = toplevel_set_fullscreen;
	toplevel->set_maximized = toplevel_set_maximized;
	toplevel->set_minimized = toplevel_set_minimized;
	toplevel->get_screen = toplevel_get_screen;
}

/* --- the display ------------------------------------------------------ */

#define BROWSER_TYPE_DISPLAY (browser_display_get_type())
G_DECLARE_FINAL_TYPE(BrowserDisplay, browser_display, BROWSER, DISPLAY, WPEDisplay)

struct _BrowserDisplay {
	WPEDisplay parent;
	WPEScreen *screen;
	WPEKeymap *keymap;
	WPEClipboard *clipboard;
};

G_DEFINE_FINAL_TYPE(BrowserDisplay, browser_display, WPE_TYPE_DISPLAY)

static gboolean display_connect(WPEDisplay *display, GError **error)
{
	(void)display;
	(void)error;
	return TRUE;
}

static WPEView *display_create_view(WPEDisplay *display)
{
	return WPE_VIEW(g_object_new(BROWSER_TYPE_VIEW, "display", display, NULL));
}

static WPEToplevel *display_create_toplevel(WPEDisplay *display, guint max_views)
{
	(void)max_views;
	return WPE_TOPLEVEL(g_object_new(BROWSER_TYPE_TOPLEVEL, "display", display, NULL));
}

/* No EGL: WebKit draws on the processor, into shared memory. */
static gpointer display_get_egl_display(WPEDisplay *display, GError **error)
{
	(void)display;
	g_set_error_literal(error, WPE_DISPLAY_ERROR, WPE_DISPLAY_ERROR_NOT_SUPPORTED, "Veda has no EGL");
	return NULL;
}

static WPEKeymap *display_get_keymap(WPEDisplay *display)
{
	return BROWSER_DISPLAY(display)->keymap;
}

static WPEClipboard *display_get_clipboard(WPEDisplay *display)
{
	return BROWSER_DISPLAY(display)->clipboard;
}

static guint display_get_n_screens(WPEDisplay *display)
{
	(void)display;
	return 1;
}

static WPEScreen *display_get_screen(WPEDisplay *display, guint index)
{
	return index == 0 ? BROWSER_DISPLAY(display)->screen : NULL;
}

static void display_dispose(GObject *object)
{
	BrowserDisplay *display = BROWSER_DISPLAY(object);
	g_clear_object(&display->screen);
	g_clear_object(&display->keymap);
	g_clear_object(&display->clipboard);
	G_OBJECT_CLASS(browser_display_parent_class)->dispose(object);
}

static void browser_display_init(BrowserDisplay *display)
{
	display->screen = NULL;
	display->keymap = NULL;
	display->clipboard = NULL;
}

static void browser_display_class_init(BrowserDisplayClass *klass)
{
	G_OBJECT_CLASS(klass)->dispose = display_dispose;
	WPEDisplayClass *display = WPE_DISPLAY_CLASS(klass);
	display->connect = display_connect;
	display->create_view = display_create_view;
	display->create_toplevel = display_create_toplevel;
	display->get_egl_display = display_get_egl_display;
	display->get_keymap = display_get_keymap;
	display->get_clipboard = display_get_clipboard;
	display->get_n_screens = display_get_n_screens;
	display->get_screen = display_get_screen;
}

WPEDisplay *platform_display_new(int width, int height, double scale, int refresh_rate)
{
	BrowserDisplay *display = g_object_new(BROWSER_TYPE_DISPLAY, NULL);
	display->screen = WPE_SCREEN(g_object_new(BROWSER_TYPE_SCREEN, "id", 1, NULL));
	wpe_screen_set_size(display->screen, width, height);
	/* At about 96 pixels an inch: what is known of the screen. */
	wpe_screen_set_physical_size(display->screen, (int)(width * scale * 25.4 / 96),
				     (int)(height * scale * 25.4 / 96));
	wpe_screen_set_scale(display->screen, scale);
	wpe_screen_set_refresh_rate(display->screen, refresh_rate * 1000);
	display->keymap = WPE_KEYMAP(g_object_new(BROWSER_TYPE_KEYMAP, NULL));
	display->clipboard = WPE_CLIPBOARD(g_object_new(BROWSER_TYPE_CLIPBOARD, "display", display, NULL));
	return WPE_DISPLAY(display);
}

void platform_view_attach(WPEView *wpe_view, struct view *view)
{
	BROWSER_VIEW(wpe_view)->owner = view;
	WPEToplevel *toplevel = wpe_view_get_toplevel(wpe_view);
	if (toplevel)
		BROWSER_TOPLEVEL(toplevel)->owner = view;
}

void platform_view_detach(WPEView *wpe_view)
{
	BROWSER_VIEW(wpe_view)->owner = NULL;
	WPEToplevel *toplevel = wpe_view_get_toplevel(wpe_view);
	if (toplevel)
		BROWSER_TOPLEVEL(toplevel)->owner = NULL;
}

void platform_set_modifiers(WPEDisplay *display, WPEModifiers modifiers)
{
	BROWSER_KEYMAP(BROWSER_DISPLAY(display)->keymap)->modifiers = modifiers;
}

void platform_set_clipboard(WPEDisplay *display, const char *text)
{
	WPEClipboard *clipboard = BROWSER_DISPLAY(display)->clipboard;
	BrowserClipboard *self = BROWSER_CLIPBOARD(clipboard);
	if (!text || (self->text && !strcmp(self->text, text)))
		return;
	/* A page's own copy is no news to give back to it. */
	const char *mine = wpe_clipboard_get_content(clipboard)
				   ? wpe_clipboard_content_get_text(wpe_clipboard_get_content(clipboard))
				   : NULL;
	if (mine && !strcmp(mine, text))
		return;
	g_free(self->text);
	self->text = g_strdup(text);
	/* The system's content, as the window system's would be. */
	GPtrArray *formats = g_ptr_array_new();
	g_ptr_array_add(formats, (gpointer)g_intern_static_string("text/plain"));
	g_ptr_array_add(formats, (gpointer)g_intern_static_string("text/plain;charset=utf-8"));
	g_ptr_array_add(formats, NULL);
	WPE_CLIPBOARD_GET_CLASS(clipboard)->changed(clipboard, formats, FALSE, NULL);
	g_ptr_array_unref(formats);
}

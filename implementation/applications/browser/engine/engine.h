/* SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * The browser's web engine: WPE WebKit on a thread of its own, behind
 * commands, which any thread may give, and events, which the browser takes
 * as they come. WebKit draws each view's page into memory kept here (its
 * frame), which the browser shows. What to make of a request, a download
 * or an error is the browser's to decide: the engine only says what
 * happened and does what it is told.
 *
 * Each view is a page as a tab shows it: its own history, in the network
 * session of every view or, for a private one, in one that keeps nothing.
 * Its sizes and positions are in logical pixels (its frame is `scale` of
 * them); its key codes are XKB's (Linux's input codes, plus 8), its key
 * symbols X11's.
 *
 * engine.c is the engine's thread and its commands, events and requests;
 * platform.c is WebKit's platform on Veda (WPE's display, views and
 * windows: the browser's window as WebKit sees it).
 */

#ifndef BROWSER_ENGINE_H
#define BROWSER_ENGINE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* A view: one page. Never 0. */
typedef uint32_t engine_view;

struct engine_config {
	/* Where the session keeps what it keeps (cookies, storage, icons),
	 * and its cache. */
	const char *data_dir;
	const char *cache_dir;
	/* Where downloads go, and the engine's temporary files (TMPDIR): the
	 * files pages are to upload, copied where the network process reads
	 * them, and its own. The network process, sandboxed, reaches these and
	 * the two above, and no other of the user's files but the folders of
	 * the pages the browser loads from them (engine_view_load). */
	const char *downloads_dir;
	const char *temporary_dir;
	/* What the user agent adds of the browser's own ("Name/version"). */
	const char *application;
	/* Cookies from sites other than the page's are refused. */
	int block_third_party_cookies;
	/* Pages are composited with OpenGL ES (Veda's, which the program links:
	 * the egl library), on the GPU: scrolling, animations and CSS's 3D
	 * transforms and filters as the web has them. A web process the GPU
	 * gives no context paints its pages on the CPU, as OpenGL ES renders
	 * nothing in software, which would take far more of the processor than
	 * painting. Otherwise the CPU paints each page as one picture. */
	int accelerated;
	/* The screen: its size and scale, and how many frames a second it
	 * shows (for animations' timing). */
	int screen_width, screen_height, refresh_rate;
	double scale;
	/* Called on the engine's thread when events come where there were
	 * none: the browser then takes them all (engine_next_event). */
	void (*wake)(void *context);
	void *wake_context;
};

/* Runs this process as one of WebKit's (its web content or network
 * process), if the engine started it as one: its exit code; -1 for the
 * browser itself. The program's main() asks first. */
int engine_auxiliary_main(int argc, char **argv);

/* Starts the engine: its thread, WebKit, the sessions. 0, or an errno
 * value. */
int engine_start(const struct engine_config *config);

/* Ends it, once its views are gone and what they keep is written. */
void engine_stop(void);

/* --- views ----------------------------------------------------------- */

/* A new view, empty; in a session that keeps nothing if `ephemeral`; in
 * `related`'s process and session if it is not 0 (a page opened by
 * another). 0 if it cannot be. */
engine_view engine_view_create(int ephemeral, engine_view related);
void engine_view_destroy(engine_view view);

/* Loads `uri`. A page from the user's files (file://) lends the network
 * process its folder, to read, as the load starts: the browser's loads are
 * the user's (an address typed, a bookmark, a file opened), where a page's
 * own links and scripts lend nothing. */
void engine_view_load(engine_view view, const char *uri);
void engine_view_reload(engine_view view, int bypass_cache);
void engine_view_stop(engine_view view);
/* Back (-1) or forward (1) in its history. */
void engine_view_go(engine_view view, int steps);

/* Its size in logical pixels, and how many device pixels each is. */
void engine_view_resize(engine_view view, int width, int height, double scale);
/* Whether it is shown (its tab is the window's); whether its window has
 * the focus (`active`), and whether the page has the keyboard in it
 * (`focused`, not while the browser's own controls have it). */
void engine_view_show(engine_view view, int shown);
void engine_view_focus(engine_view view, int active, int focused);
/* The page's zoom: 1 is its own size. */
void engine_view_zoom(engine_view view, double level);
/* The window became full screen, or stopped being (as the page asked:
 * ENGINE_EVENT_FULLSCREEN). */
void engine_view_fullscreen(engine_view view, int fullscreen);

/* --- input ----------------------------------------------------------- */

enum engine_modifier {
	ENGINE_SHIFT = 1 << 0,
	ENGINE_CONTROL = 1 << 1,
	ENGINE_ALT = 1 << 2,
	ENGINE_META = 1 << 3,
	ENGINE_CAPS_LOCK = 1 << 4,
};

enum engine_pointer {
	ENGINE_POINTER_MOVE = 1,
	ENGINE_POINTER_DOWN,
	ENGINE_POINTER_UP,
	ENGINE_POINTER_LEAVE,
};

/* Buttons: 1 primary, 2 middle, 3 secondary, 8 back, 9 forward. `clicks`
 * counts a press's rapid predecessors and itself (2: a double click). */
void engine_view_pointer(engine_view view, uint32_t kind, double x, double y, uint32_t button, uint32_t clicks,
			 uint32_t modifiers);
/* Wheel turns: `dy` up and `dx` left are positive, in notches. */
void engine_view_scroll(engine_view view, double x, double y, double dx, double dy, uint32_t modifiers);
void engine_view_key(engine_view view, int pressed, uint32_t keycode, uint32_t keysym, uint32_t modifiers);

/* --- what the browser asks of a page -------------------------------- */

enum engine_find {
	ENGINE_FIND_CASE_SENSITIVE = 1 << 0,
	ENGINE_FIND_BACKWARDS = 1 << 1,
};

/* Finds `text` in the page (NULL: stops, and unmarks what it found);
 * ENGINE_EVENT_FOUND says how many there are. */
void engine_view_find(engine_view view, const char *text, uint32_t flags);
void engine_view_find_next(engine_view view, int backwards);
/* Runs `script` in the page; ENGINE_EVENT_SCRIPT_RESULT answers `request`
 * (the browser's number) with its value as JSON. */
void engine_view_run_script(engine_view view, uint64_t request, const char *script);

/* The system's clipboard text, for the next paste in a page. */
void engine_clipboard(const char *text);

enum engine_data {
	ENGINE_DATA_COOKIES = 1 << 0,
	ENGINE_DATA_CACHE = 1 << 1,
	/* Local storage, IndexedDB and the like. */
	ENGINE_DATA_STORAGE = 1 << 2,
	ENGINE_DATA_ICONS = 1 << 3,
};

/* Clears what the session keeps of `what` from the last `seconds` (0:
 * all of it). */
void engine_clear_data(uint32_t what, int64_t seconds);

/* --- answers to requests (events with a `request`) ------------------ */

/* An alert, a confirmation or a prompt: `confirmed` (OK) and the text
 * typed. */
void engine_dialog_reply(uint64_t request, int confirmed, const char *text);
void engine_permission_reply(uint64_t request, int allow);
/* Credentials, or none (user NULL) to give up. */
void engine_auth_reply(uint64_t request, const char *user, const char *password);
/* The files chosen, `count` of them (0: none, given up). */
void engine_files_reply(uint64_t request, const char *const *paths, size_t count);
/* The item chosen of an option or context menu, or -1. */
void engine_menu_reply(uint64_t request, int index);
/* Go on to a page whose certificate is not to be trusted, which its host
 * is then allowed in this session. */
void engine_tls_reply(uint64_t request, int proceed);
/* Where a download goes (a file that does not exist yet), or NULL: none,
 * it is cancelled. */
void engine_download_reply(uint64_t request, const char *path);
void engine_download_cancel(uint64_t request);

/* --- events ---------------------------------------------------------- */

enum engine_event_kind {
	/* The view's frame changed. */
	ENGINE_EVENT_FRAME = 1,
	/* text: its title. */
	ENGINE_EVENT_TITLE,
	/* text: its address. */
	ENGINE_EVENT_URI,
	/* number[0]: ENGINE_LOAD_*. */
	ENGINE_EVENT_LOAD,
	/* value: how far a load is, 0 to 1. */
	ENGINE_EVENT_PROGRESS,
	/* number[0]: whether it can go back, number[1]: forward. */
	ENGINE_EVENT_HISTORY,
	/* text: the address that failed, text2: why, number[0]: ENGINE_FAILED_*. */
	ENGINE_EVENT_LOAD_FAILED,
	/* A request: text: the address, text2: its host, text3: the
	 * certificate's subject, number[0]: what is wrong with it
	 * (ENGINE_TLS_*). */
	ENGINE_EVENT_TLS_ERROR,
	/* number[0]: ENGINE_SECURITY_*, as the page committed. */
	ENGINE_EVENT_SECURITY,
	/* text: the pointer's CSS name ("default", "pointer", "text"...). */
	ENGINE_EVENT_CURSOR,
	/* The page's icon: data, number[0] x number[1] pixels as frames are;
	 * no data: none. */
	ENGINE_EVENT_ICON,
	/* text: the link under the pointer, NULL when there is none. */
	ENGINE_EVENT_HOVER,
	/* A view the engine made for a page (window.open, a link to a new
	 * window): number[0] is the view that opened it. */
	ENGINE_EVENT_NEW_VIEW,
	/* The page asks to be closed. */
	ENGINE_EVENT_CLOSE,
	/* Its page's process ended: number[0]: ENGINE_CRASH_*. */
	ENGINE_EVENT_CRASHED,
	/* number[0]: 1 when the page asks for the whole screen, 0 when it
	 * gives it back. */
	ENGINE_EVENT_FULLSCREEN,
	/* A request: number[0]: ENGINE_DIALOG_*, text: the message, text2:
	 * a prompt's text to begin with. */
	ENGINE_EVENT_DIALOG,
	/* A request: number[0]: ENGINE_PERMISSION_*, text: the site. */
	ENGINE_EVENT_PERMISSION,
	/* A request: text: the host, text2: the realm, number[0]: 1 for a
	 * proxy. */
	ENGINE_EVENT_AUTH,
	/* A request: number[0]: 1 if several files may be chosen, text: the
	 * types accepted, '\n' between them (NULL: any). */
	ENGINE_EVENT_FILE_CHOOSER,
	/* A request: the options of a <select>: text: their labels, '\n'
	 * between them, data: one byte of ENGINE_ITEM_* each; number[0..3]:
	 * where the select is (x, y, width, height). */
	ENGINE_EVENT_OPTION_MENU,
	/* A request: the page's context menu, as an option menu's but at
	 * number[0], number[1]; text2: the link there, text3: the image
	 * there. */
	ENGINE_EVENT_CONTEXT_MENU,
	/* The page closed the menu of `request`. */
	ENGINE_EVENT_MENU_CLOSED,
	/* A request: a download: text: its address, text2: the name it
	 * suggests, number[0]: its size if known (else -1). */
	ENGINE_EVENT_DOWNLOAD,
	/* `request`'s download has number[0] bytes (value: the fraction). */
	ENGINE_EVENT_DOWNLOAD_PROGRESS,
	/* `request`'s download ended: number[0]: 0 done, 1 failed (text: why),
	 * 2 cancelled. */
	ENGINE_EVENT_DOWNLOAD_DONE,
	/* number[0]: how many matches finding found (0: none). */
	ENGINE_EVENT_FOUND,
	/* `request`'s script: text: its value as JSON, or text2: why it
	 * failed. */
	ENGINE_EVENT_SCRIPT_RESULT,
	/* text: what a page copied. */
	ENGINE_EVENT_CLIPBOARD,
	/* The page shows content that came unencrypted. */
	ENGINE_EVENT_INSECURE_CONTENT,
};

enum engine_load {
	ENGINE_LOAD_STARTED = 1,
	ENGINE_LOAD_REDIRECTED,
	ENGINE_LOAD_COMMITTED,
	ENGINE_LOAD_FINISHED,
};

enum engine_failed {
	ENGINE_FAILED_NETWORK = 1,
	/* The user, or another load, stopped it: nothing to show. */
	ENGINE_FAILED_CANCELLED,
	/* No such host. */
	ENGINE_FAILED_HOST,
	/* What it is cannot be shown (it was downloaded instead). */
	ENGINE_FAILED_CONTENT,
	ENGINE_FAILED_OTHER,
};

enum engine_security {
	/* Not a network page (about:, data:, a file). */
	ENGINE_SECURITY_LOCAL = 1,
	/* Unencrypted. */
	ENGINE_SECURITY_INSECURE,
	/* Encrypted, its certificate trusted. */
	ENGINE_SECURITY_SECURE,
	/* Encrypted, its certificate not trusted but allowed (engine_tls_reply). */
	ENGINE_SECURITY_BROKEN,
};

enum engine_tls {
	ENGINE_TLS_UNKNOWN_CA = 1 << 0,
	ENGINE_TLS_BAD_IDENTITY = 1 << 1,
	ENGINE_TLS_NOT_ACTIVATED = 1 << 2,
	ENGINE_TLS_EXPIRED = 1 << 3,
	ENGINE_TLS_REVOKED = 1 << 4,
	ENGINE_TLS_INSECURE = 1 << 5,
	ENGINE_TLS_GENERIC = 1 << 6,
};

enum engine_crash {
	ENGINE_CRASH_CRASHED = 1,
	ENGINE_CRASH_MEMORY,
	ENGINE_CRASH_TERMINATED,
};

enum engine_dialog {
	ENGINE_DIALOG_ALERT = 1,
	ENGINE_DIALOG_CONFIRM,
	ENGINE_DIALOG_PROMPT,
	/* Leaving a page that asks whether to. */
	ENGINE_DIALOG_LEAVE,
};

enum engine_permission {
	ENGINE_PERMISSION_GEOLOCATION = 1,
	ENGINE_PERMISSION_NOTIFICATIONS,
	/* A site's storage where it is another's page's part (a frame). */
	ENGINE_PERMISSION_STORAGE,
	ENGINE_PERMISSION_DEVICE_INFO,
	ENGINE_PERMISSION_MEDIA,
	ENGINE_PERMISSION_OTHER,
};

enum engine_item {
	ENGINE_ITEM_ENABLED = 1 << 0,
	ENGINE_ITEM_SELECTED = 1 << 1,
	ENGINE_ITEM_SEPARATOR = 1 << 2,
	ENGINE_ITEM_GROUP_LABEL = 1 << 3,
	ENGINE_ITEM_GROUP_CHILD = 1 << 4,
};

struct engine_event {
	uint32_t kind;
	engine_view view;
	uint64_t request;
	int64_t number[4];
	double value;
	/* UTF-8, or NULL. */
	char *text, *text2, *text3;
	uint8_t *data;
	size_t data_len;
};

/* Takes the next event: 1, or 0 when there is none. Its strings and data
 * are the caller's until engine_event_clear. */
int engine_next_event(struct engine_event *event);
void engine_event_clear(struct engine_event *event);

/* --- frames ---------------------------------------------------------- */

struct engine_frame {
	/* Premultiplied 0xAARRGGBB, `stride` pixels from row to row. */
	const uint32_t *pixels;
	int width, height, stride;
	/* What changed since the frame was last taken: x, y, width, height
	 * (all 0: nothing). */
	int damage[4];
	/* Each frame WebKit draws has the next one. */
	uint64_t serial;
	/* The engine's, for engine_frame_unlock. */
	void *held;
};

/* The view's frame, which stays put until engine_frame_unlock, even if the
 * view goes meanwhile: 1, or 0 when it has none yet (nothing is held
 * then). */
int engine_frame_lock(engine_view view, struct engine_frame *frame);
/* Lets the frame engine_frame_lock gave go; `presented` is the serial of
 * the frame that reached the screen, or 0: WebKit draws the next once it
 * has. */
void engine_frame_unlock(const struct engine_frame *frame, uint64_t presented);

#ifdef __cplusplus
}
#endif

#endif

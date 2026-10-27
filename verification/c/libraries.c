/* SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * libraries — checks of shared libraries and of loading them at run time
 * (<dlfcn.h>), through the C library's dynamic linker. Like the other C
 * test programs it is built linked statically, at a fixed address and
 * position-independent, and dynamically (with -rdynamic, so that the
 * libraries it loads use what it exports): a static program checks that
 * loading fails as it says it does; the dynamic one checks the rest, with
 * the libraries of verification/c/shared in /system/tests/c/lib (a C++ one
 * among them, which brings the C++ library in), and the Rust one of
 * verification/rust/shared in /system/tests/rust/lib, when the image has
 * Rust's programs.
 *
 * Each check prints "ok - NAME" or "not ok - NAME: REASON"; the program
 * exits with the number of failed checks (0: all passed). `systest` runs
 * it inside Veda in a scratch directory, with its standard output on a
 * pipe.
 *
 * `libraries child MODE` is the program the checks start again: `destroy`
 * loads libplugin and ends, its destructor writing as it does; `search`
 * loads libbase by its name alone, as LD_LIBRARY_PATH lets it; `sandbox`
 * gives up new executable memory, then tries to load a library. */

#define _GNU_SOURCE
#include <dlfcn.h>
#include <elf.h>
#include <errno.h>
#include <link.h>
#include <pthread.h>
#include <spawn.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/wait.h>
#include <unistd.h>
#include <veda/sandbox.h>

#define LIB "/system/tests/c/lib"
#define RUST_LIB "/system/tests/rust/lib/librustplugin.so"
#define DYNAMIC_LINKER "/system/lib/libc.so"

extern char **environ;

static int failures;
static char program[4096];

static void report(const char *name, int ok, const char *fmt, ...)
{
	if (ok) {
		printf("ok - %s\n", name);
		return;
	}
	failures++;
	printf("not ok - %s: ", name);
	va_list ap;
	va_start(ap, fmt);
	vprintf(fmt, ap);
	va_end(ap);
	printf("\n");
}

#define CHECK(name, cond, ...) report(name, (cond), __VA_ARGS__)

/* What dlerror says, or "(nothing)": it says it once. */
static const char *said(void)
{
	const char *e = dlerror();
	return e ? e : "(nothing)";
}

/* Whether `text` holds `part`. */
static int says(const char *text, const char *part)
{
	return text && strstr(text, part);
}

/* What this program exports for the libraries it loads (-rdynamic):
   libhost calls it. */
int host_value(void)
{
	return 21;
}

/* Starts this program again as `child MODE`, with `env` its environment;
   its exit status, with its output, or -1. */
static int run_child(const char *mode, char *const *env, char *out, size_t size)
{
	int p[2];
	if (pipe(p))
		return -1;
	posix_spawn_file_actions_t fa;
	posix_spawn_file_actions_init(&fa);
	posix_spawn_file_actions_adddup2(&fa, p[1], 1);
	posix_spawn_file_actions_addclose(&fa, p[0]);
	char *argv[] = { "libraries", "child", (char *)mode, 0 };
	pid_t pid;
	int r = posix_spawn(&pid, program, &fa, 0, argv, env);
	posix_spawn_file_actions_destroy(&fa);
	close(p[1]);
	size_t n = 0;
	ssize_t got;
	while (n + 1 < size && (got = read(p[0], out + n, size - 1 - n)) > 0)
		n += got;
	out[n] = 0;
	close(p[0]);
	int status;
	if (r || waitpid(pid, &status, 0) != pid)
		return -1;
	return WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
}

static int child(const char *mode)
{
	if (!strcmp(mode, "destroy")) {
		/* The library's destructor writes after this, as the program
		   ends. */
		void *h = dlopen(LIB "/libplugin.so", RTLD_NOW);
		printf("child: %s\n", h ? "loaded" : dlerror());
		fflush(stdout);
		return h ? 0 : 1;
	}
	if (!strcmp(mode, "search")) {
		void *h = dlopen("libbase.so", RTLD_NOW);
		printf("child: %s\n", h ? "found" : dlerror());
		return h ? 0 : 1;
	}
	if (!strcmp(mode, "sandbox")) {
		if (veda_process_restrict(VEDA_RESTRICT_EXECUTE))
			return 2;
		void *h = dlopen(LIB "/libplugin.so", RTLD_NOW);
		printf("child: %s\n", h ? "loaded" : dlerror());
		return h ? 1 : 0;
	}
	return 3;
}

/* Linked statically, the program loads nothing, and says why. */
static void check_static(void)
{
	CHECK("static: no dynamic linker", getauxval(AT_BASE) == 0, "AT_BASE %#lx", getauxval(AT_BASE));
	void *h = dlopen(LIB "/libplugin.so", RTLD_NOW);
	const char *e = said();
	CHECK("static: dlopen fails, saying why", !h && says(e, "linked statically"), "%s", e);
	CHECK("static: no handle of the program either", !dlopen(0, RTLD_NOW), "had one");
	said();
}

/* The dynamic linker: the C library's shared object, where the program
   says it is. */
static void check_dynamic_linker(void)
{
	unsigned long base = getauxval(AT_BASE);
	CHECK("the dynamic linker is loaded", base && !memcmp((void *)base, ELFMAG, SELFMAG), "AT_BASE %#lx", base);
	Dl_info info = { 0 };
	int in_libc = dladdr((void *)printf, &info) && info.dli_fname && !strcmp(info.dli_fname, DYNAMIC_LINKER) &&
		      (unsigned long)info.dli_fbase == base;
	CHECK("the C library is the dynamic linker's object", in_libc, "printf in %s at %p",
	      info.dli_fname ? info.dli_fname : "nothing", info.dli_fbase);
	void *self = dlopen(0, RTLD_NOW);
	int (*value)(void) = self ? (int (*)(void))dlsym(self, "host_value") : 0;
	CHECK("the program's handle finds what it exports", value == host_value, "found %p", (void *)value);
}

/* What dl_iterate_phdr says of an object: whether it is there, and how many
   are. */
struct objects {
	const char *suffix;
	int found;
	int count;
	unsigned long long adds;
};

static int count_object(struct dl_phdr_info *info, size_t size, void *data)
{
	struct objects *o = data;
	size_t n = strlen(info->dlpi_name), m = strlen(o->suffix);
	if (n >= m && !strcmp(info->dlpi_name + n - m, o->suffix))
		o->found = 1;
	o->count++;
	o->adds = info->dlpi_adds;
	(void)size;
	return 0;
}

static struct objects objects(const char *suffix)
{
	struct objects o = { suffix, 0, 0, 0 };
	dl_iterate_phdr(count_object, &o);
	return o;
}

/* Thread-local storage of a library loaded after a thread began: the
   thread has it too, at its initial value. */
struct waiting {
	pthread_mutex_t lock;
	pthread_cond_t cond;
	/* Set once the library is loaded, or `given_up` if it is not. */
	int (*count)(void);
	const char *(*name)(const char *);
	int given_up;
	int calls;
	char seen[16];
};

static void *early_thread(void *p)
{
	struct waiting *w = p;
	pthread_mutex_lock(&w->lock);
	while (!w->count && !w->given_up)
		pthread_cond_wait(&w->cond, &w->lock);
	pthread_mutex_unlock(&w->lock);
	if (w->given_up)
		return 0;
	w->count();
	w->calls = w->count();
	snprintf(w->seen, sizeof w->seen, "%s", w->name(0));
	w->name("early");
	return 0;
}

/* Lets the early thread go on, with the library's functions or without
   them, and waits for it. */
static void release(struct waiting *w, pthread_t t, int (*count)(void), const char *(*name)(const char *))
{
	pthread_mutex_lock(&w->lock);
	w->name = name;
	w->count = count;
	w->given_up = !count;
	pthread_cond_signal(&w->cond);
	pthread_mutex_unlock(&w->lock);
	pthread_join(t, 0);
}

static void *late_thread(void *p)
{
	struct waiting *w = p;
	w->calls = w->count();
	snprintf(w->seen, sizeof w->seen, "%s", w->name(0));
	return 0;
}

static void check_loading(void)
{
	struct objects before = objects("/libplugin.so");
	CHECK("not loaded until asked for", !before.found && !dlopen("libplugin.so", RTLD_NOW | RTLD_NOLOAD),
	      "loaded");
	said();
	/* A thread that runs before the library is loaded. */
	struct waiting early = { .lock = PTHREAD_MUTEX_INITIALIZER, .cond = PTHREAD_COND_INITIALIZER };
	pthread_t t;
	int started = pthread_create(&t, 0, early_thread, &early) == 0;

	void *h = dlopen(LIB "/libplugin.so", RTLD_NOW);
	const char *e = said();
	CHECK("dlopen by path", h != 0, "%s", e);
	if (!h) {
		if (started)
			release(&early, t, 0, 0);
		return;
	}
	int (*twice)(int) = (int (*)(int))dlsym(h, "plugin_twice");
	int (*base)(void) = (int (*)(void))dlsym(h, "plugin_base");
	int *answer = dlsym(h, "plugin_answer");
	int *saw_base = dlsym(h, "plugin_saw_base");
	CHECK("a function", twice && twice(21) == 42, "plugin_twice %p", (void *)twice);
	CHECK("data", answer && *answer == 42, "plugin_answer %p", (void *)answer);
	CHECK("a dependency, found beside it ($ORIGIN)", base && base() == 41, "plugin_base %p", (void *)base);
	CHECK("dependencies constructed first", saw_base && *saw_base == 1, "plugin_saw_base %p", (void *)saw_base);
	void *dep = dlopen("libbase.so", RTLD_NOW | RTLD_NOLOAD);
	CHECK("a dependency is loaded, and known by its name", dep != 0, "%s", said());
	CHECK("loading again gives the same handle", dlopen(LIB "/libplugin.so", RTLD_LAZY) == h, "another");

	/* What it defines is its own, unless it is loaded RTLD_GLOBAL. */
	CHECK("RTLD_LOCAL keeps its symbols to it", !dlsym(RTLD_DEFAULT, "plugin_twice"), "found");
	said();
	CHECK("RTLD_GLOBAL makes them everyone's",
	      dlopen(LIB "/libplugin.so", RTLD_NOW | RTLD_GLOBAL) == h && dlsym(RTLD_DEFAULT, "plugin_twice") == twice,
	      "%s", said());

	Dl_info info = { 0 };
	int named = twice && dladdr((void *)twice, &info) && info.dli_fname &&
		    !strcmp(info.dli_fname, LIB "/libplugin.so") && info.dli_sname &&
		    !strcmp(info.dli_sname, "plugin_twice") && info.dli_saddr == (void *)twice;
	CHECK("dladdr names a library's symbol", named, "%s %s", info.dli_fname ? info.dli_fname : "-",
	      info.dli_sname ? info.dli_sname : "-");
	struct objects after = objects("/libplugin.so");
	CHECK("dl_iterate_phdr lists what is loaded",
	      after.found && objects("/libbase.so").found && after.count >= before.count + 2 && after.adds > before.adds,
	      "%d objects, %llu added", after.count, after.adds);

	/* Thread-local storage: each thread's own, from the library's initial
	   values; this thread's through dlsym too. */
	int (*count)(void) = (int (*)(void))dlsym(h, "plugin_count");
	const char *(*name)(const char *) = (const char *(*)(const char *))dlsym(h, "plugin_name");
	int *calls = dlsym(h, "plugin_thread_calls");
	if (!count || !name || !calls) {
		CHECK("thread-local storage", 0, "no plugin_count, plugin_name or plugin_thread_calls");
		if (started)
			release(&early, t, 0, 0);
		return;
	}
	count();
	CHECK("thread-local storage", count() == 2 && *calls == 2 && !strcmp(name("main"), "main"), "calls %d", *calls);
	if (started)
		release(&early, t, count, name);
	CHECK("in a thread begun before it was loaded", started && early.calls == 2 && !strcmp(early.seen, "unset"),
	      "calls %d, name %s", early.calls, early.seen);
	struct waiting late = { .count = count, .name = name };
	int joined = pthread_create(&t, 0, late_thread, &late) == 0 && pthread_join(t, 0) == 0;
	CHECK("in a thread begun after", joined && late.calls == 1 && !strcmp(late.seen, "unset"), "calls %d, name %s",
	      late.calls, late.seen);
	CHECK("each thread's apart", *calls == 2 && !strcmp(name(0), "main"), "calls %d, name %s", *calls, name(0));

	/* A library that uses what the program exports. */
	void *host = dlopen(LIB "/libhost.so", RTLD_NOW);
	int (*host_twice)(void) = host ? (int (*)(void))dlsym(host, "host_twice") : 0;
	CHECK("a library uses the program's symbols", host_twice && host_twice() == 42, "%s", said());

	/* Libraries stay loaded: closing a handle is accepted. */
	CHECK("dlclose", dlclose(h) == 0 && twice(1) == 2, "%s", said());
}

static void check_errors(void)
{
	void *h = dlopen(LIB "/libnothing.so", RTLD_NOW);
	const char *e = said();
	CHECK("a library that is not there", !h && says(e, "libnothing.so") && says(e, "No such file"), "%s", e);
	CHECK("dlerror says it once", !dlerror(), "said again");
	h = dlopen("libnothing.so", RTLD_NOW);
	e = said();
	CHECK("nor found by its name", !h && says(e, "libnothing.so"), "%s", e);
	h = dlopen("/system/etc/passwd", RTLD_NOW);
	e = said();
	CHECK("a file that is no library", !h && says(e, "passwd"), "%s", e);
	h = dlopen("/system/tests/c/hello", RTLD_NOW);
	e = said();
	CHECK("a program linked statically", !h, "%s", e);
	void *self = dlopen(0, RTLD_NOW);
	CHECK("a symbol that is not there", self && !dlsym(self, "no_such_symbol") && says(said(), "no_such_symbol"),
	      "found");
	/* By its name alone, a library is looked for where the system's are,
	   then the packages': libhost, loaded by its path, is in neither. */
	CHECK("a library by its name alone, only among the system's and the packages'", !dlopen("libhost.so", RTLD_NOW),
	      "found libhost.so");
	said();
}

static void check_children(void)
{
	char out[512];
	char *empty[] = { 0 };
	int status = run_child("destroy", empty, out, sizeof out);
	CHECK("a library's destructor runs as the program ends",
	      status == 0 && says(out, "child: loaded\nplugin: destroyed\n"), "status %d: %s", status, out);
	status = run_child("search", empty, out, sizeof out);
	CHECK("without LD_LIBRARY_PATH, a library by name is not found", status == 1 && says(out, "libbase.so"),
	      "status %d: %s", status, out);
	char *path[] = { "LD_LIBRARY_PATH=/nowhere:" LIB, 0 };
	status = run_child("search", path, out, sizeof out);
	CHECK("LD_LIBRARY_PATH", status == 0 && says(out, "child: found"), "status %d: %s", status, out);
	status = run_child("sandbox", empty, out, sizeof out);
	CHECK("no library without new executable memory (sandboxed)", status == 0 && says(out, "libplugin.so"),
	      "status %d: %s", status, out);
}

/* A thread begun before a C++ library is loaded, which then throws and
   catches inside it, with the C++ library's thread-local storage that came
   with it. */
struct cxx_waiting {
	pthread_mutex_t lock;
	pthread_cond_t cond;
	/* Set once the library is loaded, or `given_up` if it is not. */
	int (*round_trip)(int);
	int given_up;
	int length;
};

static void *early_cxx_thread(void *p)
{
	struct cxx_waiting *w = p;
	pthread_mutex_lock(&w->lock);
	while (!w->round_trip && !w->given_up)
		pthread_cond_wait(&w->cond, &w->lock);
	pthread_mutex_unlock(&w->lock);
	w->length = w->given_up ? -1 : w->round_trip(7);
	return 0;
}

/* A C++ library (verification/c/shared/thrower.cc), from C: the program has
   no C++ library until it loads one, which brings the system's C++ library
   and GCC's unwinder (libstdc++.so.6, libgcc_s.so.1); its static objects,
   and its exceptions, thrown and caught inside it, in this thread and in
   one begun before it was loaded. */
static void check_cxx_library(void)
{
	CHECK("no C++ library until one is loaded", !objects("/libstdc++.so.6").found, "loaded already");
	struct cxx_waiting early = { .lock = PTHREAD_MUTEX_INITIALIZER, .cond = PTHREAD_COND_INITIALIZER };
	pthread_t t;
	int started = pthread_create(&t, 0, early_cxx_thread, &early) == 0;
	void *h = dlopen(LIB "/libthrower.so", RTLD_NOW);
	const char *e = said();
	int (*round_trip)(int) = h ? (int (*)(int))dlsym(h, "thrower_round_trip") : 0;
	const char *(*name)(void) = h ? (const char *(*)(void))dlsym(h, "thrower_name") : 0;
	int runtime = objects("/system/lib/libstdc++.so.6").found && objects("/system/lib/libgcc_s.so.1").found;
	CHECK("a C++ library, with the C++ library and the unwinder", round_trip && name && runtime, "%s",
	      h ? "not all of them" : e);
	CHECK("a C++ library's static objects", name && !strcmp(name(), "thrower"), "%s", name ? name() : "-");
	int length = round_trip ? round_trip(42) : -1;
	CHECK("a C++ library's exceptions, inside it", length == (int)strlen("thrown in the library: 42"), "length %d",
	      length);
	if (started) {
		pthread_mutex_lock(&early.lock);
		early.round_trip = round_trip;
		early.given_up = !round_trip;
		pthread_cond_signal(&early.cond);
		pthread_mutex_unlock(&early.lock);
		pthread_join(t, 0);
	}
	CHECK("... in a thread begun before it was loaded",
	      started && early.length == (int)strlen("thrown in the library: 7"), "length %d", early.length);
}

/* A Rust library (verification/rust/shared/rustplugin.rs), from C, if the
   image has it. */
static void check_rust_library(void)
{
	if (access(RUST_LIB, F_OK)) {
		printf("ok - a Rust library # SKIP the image has no Rust programs\n");
		return;
	}
	void *h = dlopen(RUST_LIB, RTLD_NOW);
	int (*add)(int, int) = h ? (int (*)(int, int))dlsym(h, "rust_plugin_add") : 0;
	const char *(*greeting)(void) = h ? (const char *(*)(void))dlsym(h, "rust_plugin_greeting") : 0;
	int (*caught)(void) = h ? (int (*)(void))dlsym(h, "rust_plugin_caught_panic") : 0;
	CHECK("a Rust library", add && add(2, 40) == 42 && greeting && !strcmp(greeting(), "Hello from Rust"),
	      "%s", h ? "wrong answers" : said());
	CHECK("a Rust library's panic, caught inside it", caught && caught() == 1, "caught %d", caught ? caught() : -1);
}

int main(int argc, char **argv)
{
	const char *self = (const char *)getauxval(AT_EXECFN);
	snprintf(program, sizeof program, "%s", self && *self == '/' ? self : "/system/tests/c/libraries");
	if (argc >= 3 && !strcmp(argv[1], "child"))
		return child(argv[2]);
	setvbuf(stdout, 0, _IOLBF, 0);
	int dynamic = getauxval(AT_BASE) != 0;
	printf("libraries: %s, linked %s\n", program, dynamic ? "dynamically" : "statically");
	if (!dynamic) {
		check_static();
	} else {
		check_dynamic_linker();
		check_loading();
		check_errors();
		check_children();
		check_cxx_library();
		check_rust_library();
	}
	printf("%s: %d failed\n", failures ? "FAIL" : "PASS", failures);
	return failures;
}

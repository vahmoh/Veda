/* SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * posix — checks of Veda's POSIX layer through the C library.
 *
 * Each check prints "ok - NAME" or "not ok - NAME: REASON"; the program
 * exits with the number of failed checks (0: all passed). `systest` runs it
 * inside Veda in a scratch directory, with its standard output on a pipe.
 *
 * `posix child MODE [ARGUMENT]` is the program run by the spawning checks
 * (`posix child sandbox`, sandboxed, checks its own); `posix host ADDRESS
 * TCP-PORT UDP-PORT` runs the checks with the host machine's servers too
 * (see check_network). */

#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <pthread.h>
#include <setjmp.h>
#include <signal.h>
#include <spawn.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/timerfd.h>
#include <sys/uio.h>
#include <sys/un.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <veda/ipc.h>
#include <veda/memory.h>
#include <veda/sandbox.h>
#include <veda/thread.h>

/* The architecture's expectations
 * (verification/c/architectures/<architecture>/). */
#include "machine.h"

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
	printf(" (errno %d: %s)\n", errno, strerror(errno));
}

/* The condition first: the message may show what it computed. */
#define CHECK(name, cond, ...)                  \
	do {                                    \
		int ok_ = (cond);               \
		report(name, ok_, __VA_ARGS__); \
	} while (0)

/* The order of two ints, for qsort and bsearch. */
static int compare_ints(const void *a, const void *b)
{
	int x = *(const int *)a, y = *(const int *)b;
	return (x > y) - (x < y);
}

static void check_stdio_and_strings(void)
{
	char buf[128];
	int n = snprintf(buf, sizeof buf, "%d %s %.3f %x %c", -42, "veda", 3.14159, 255, 'Z');
	CHECK("snprintf", n == 19 && !strcmp(buf, "-42 veda 3.142 ff Z"), "got '%s' (%d)", buf, n);
	double d = strtod("2.5e3", 0);
	CHECK("strtod", d == 2500.0, "got %f", d);
	long l = strtol("-0x7f", 0, 16);
	CHECK("strtol", l == -127, "got %ld", l);
	int v[] = { 5, 3, 9, 1, 7, -2, 9 };
	qsort(v, 7, sizeof v[0], compare_ints);
	int key = 7;
	const int *found = bsearch(&key, v, 7, sizeof v[0], compare_ints);
	CHECK("qsort and bsearch",
	      v[0] == -2 && v[1] == 1 && v[2] == 3 && v[3] == 5 && v[4] == 7 && v[5] == 9 && v[6] == 9 && found == &v[4],
	      "sorted to %d %d %d %d %d %d %d, 7 at %p", v[0], v[1], v[2], v[3], v[4], v[5], v[6], (const void *)found);
	struct utsname u;
	CHECK("uname", uname(&u) == 0 && !strcmp(u.sysname, "Veda") && !strcmp(u.machine, VEDA_MACHINE),
	      "sysname '%s' machine '%s'", u.sysname, u.machine);
}

static void check_heap(void)
{
	void *small[1000];
	for (int i = 0; i < 1000; i++) {
		small[i] = malloc(16 + i);
		memset(small[i], i, 16 + i);
	}
	int ok = 1;
	for (int i = 0; i < 1000; i++) {
		if (((unsigned char *)small[i])[15 + i] != (unsigned char)i)
			ok = 0;
		free(small[i]);
	}
	CHECK("malloc small blocks", ok, "contents changed");
	char *big = malloc(64 << 20);
	CHECK("malloc 64 MiB", big != 0, "malloc failed");
	if (big) {
		big[0] = 1;
		big[(64 << 20) - 1] = 2;
		char *more = realloc(big, 96 << 20);
		CHECK("realloc", more && more[0] == 1 && more[(64 << 20) - 1] == 2, "contents lost");
		free(more ? more : big);
	}
	int *z = calloc(1000, sizeof(int));
	int zero = 1;
	for (int i = 0; z && i < 1000; i++)
		if (z[i])
			zero = 0;
	CHECK("calloc", z && zero, "not zero");
	free(z);
}

extern void _start(void);

/* What the loader put on the stack beside the arguments and environment. */
static void check_startup(void)
{
	const char *execfn = (const char *)getauxval(AT_EXECFN);
	const char *platform = (const char *)getauxval(AT_PLATFORM);
	CHECK("auxiliary vector", getauxval(AT_PAGESZ) == VEDA_PAGE_SIZE && sysconf(_SC_PAGESIZE) == VEDA_PAGE_SIZE &&
	      getauxval(AT_PHDR) && getauxval(AT_PHNUM) && getauxval(AT_RANDOM) && !getauxval(AT_SECURE) &&
	      execfn && *execfn == '/' && platform && !strcmp(platform, VEDA_MACHINE), "missing or wrong entries");
	/* Where the program was loaded: right for this program's own code. */
	CHECK("entry point", getauxval(AT_ENTRY) == (unsigned long)_start, "AT_ENTRY %#lx, _start %p",
	      getauxval(AT_ENTRY), (void *)_start);
}

static void check_mmap(void)
{
	const size_t page = VEDA_PAGE_SIZE;
	size_t len = 3 * page;
	unsigned char *p = mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	CHECK("mmap anonymous", p != MAP_FAILED && p[0] == 0 && p[len - 1] == 0, "map failed or not zero");
	if (p == MAP_FAILED)
		return;
	memset(p, 0xab, len);
	CHECK("mprotect part of a mapping", mprotect(p + page, page, PROT_READ) == 0, "mprotect failed");
	CHECK("munmap part of a mapping", munmap(p + 2 * page, page) == 0 && p[0] == 0xab, "munmap failed");
	CHECK("munmap", munmap(p, 2 * page) == 0, "munmap failed");
	void *none = mmap(0, 1 << 20, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	CHECK("mmap reservation", none != MAP_FAILED && mprotect(none, page, PROT_READ | PROT_WRITE) == 0,
	      "reserve then commit failed");
	if (none != MAP_FAILED) {
		char *r = none;
		r[100] = 7;
		/* Giving pages back, as allocators do: they read as zeros. */
		CHECK("madvise MADV_DONTNEED", madvise(r, page, MADV_DONTNEED) == 0 && r[100] == 0, "not dropped");
		CHECK("decommit a reservation",
		      mprotect(r, page, PROT_NONE) == 0 && madvise(r, 1 << 20, MADV_DONTNEED) == 0,
		      "madvise failed");
		munmap(none, 1 << 20);
	}
	/* Shared memory has nowhere to take its contents back from. */
	char *shared = mmap(0, page, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
	CHECK("mmap shared", shared != MAP_FAILED, "map failed");
	if (shared != MAP_FAILED) {
		shared[0] = 5;
		errno = 0;
		CHECK("MADV_DONTNEED keeps shared memory",
		      madvise(shared, page, MADV_DONTNEED) == -1 && errno == EINVAL && shared[0] == 5,
		      "contents dropped");
		munmap(shared, page);
	}
}

/* The system image's files map as its own pages where a mapping does not
 * write them: the bytes a read gives, zeros past the end of the last page,
 * and never writable. A mapping that writes is a copy of its own. */
static void check_system_files(void)
{
	const size_t page = VEDA_PAGE_SIZE;
	char text[256];
	int fd = open("/system/etc/version", O_RDONLY | O_CLOEXEC);
	ssize_t n = fd >= 0 ? read(fd, text, sizeof text) : -1;
	CHECK("a file of the system image", fd >= 0 && n > 0 && n < (ssize_t)sizeof text, "open %d, read %zd", fd, n);
	if (fd < 0 || n <= 0)
		return;
	unsigned char *p = mmap(0, page, PROT_READ, MAP_PRIVATE, fd, 0);
	CHECK("mmap a file of the system image", p != MAP_FAILED, "mmap: %s", strerror(errno));
	if (p != MAP_FAILED) {
		int zeros = 1;
		for (size_t i = n; i < page; i++)
			zeros &= p[i] == 0;
		CHECK("its pages are the file's", !memcmp(p, text, n) && zeros, "contents differ");
		errno = 0;
		CHECK("its pages are never writable", mprotect(p, page, PROT_READ | PROT_WRITE) == -1 && errno == EACCES,
		      "mprotect: %s", strerror(errno));
		munmap(p, page);
	}
	unsigned char *w = mmap(0, page, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
	char again[256];
	int copy = w != MAP_FAILED && !memcmp(w, text, n);
	if (w != MAP_FAILED) {
		w[0] ^= 1;
		munmap(w, page);
	}
	CHECK("a mapping that writes is a copy", copy && pread(fd, again, n, 0) == n && !memcmp(again, text, n),
	      "contents differ");
	close(fd);
}

/* A memory file: memory with a descriptor, as WebKit's processes share
 * their buffers: its mappings are the memory, ftruncate sizes it, seals
 * fix its size, and a read-only descriptor of it cannot write it; its
 * memory as a handle for a Veda service (<veda/memory.h>), as the browser
 * gives the window system a page's frames, with the descriptor's rights. */
static void check_memfd(void)
{
	const size_t page = VEDA_PAGE_SIZE, len = 4 * VEDA_PAGE_SIZE;
	int fd = memfd_create("check", MFD_CLOEXEC | MFD_ALLOW_SEALING);
	CHECK("memfd_create", fd >= 0, "%s", strerror(errno));
	if (fd < 0)
		return;
	struct stat st;
	CHECK("ftruncate a memory file", ftruncate(fd, len) == 0 && fstat(fd, &st) == 0 && st.st_size == (off_t)len,
	      "size %lld", (long long)st.st_size);
	char *a = mmap(0, len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	char *b = mmap(0, len, PROT_READ, MAP_SHARED, fd, 0);
	CHECK("map a memory file twice", a != MAP_FAILED && b != MAP_FAILED, "mmap: %s", strerror(errno));
	char back[8] = { 0 };
	if (a != MAP_FAILED && b != MAP_FAILED) {
		strcpy(a + page, "shared");
		CHECK("its mappings are its memory",
		      !strcmp(b + page, "shared") && pread(fd, back, 6, page) == 6 && !strcmp(back, "shared"),
		      "read \"%s\"", back);
	}
	errno = 0;
	CHECK("seals fix its size",
	      fcntl(fd, F_ADD_SEALS, F_SEAL_GROW | F_SEAL_SHRINK) == 0 &&
		      (fcntl(fd, F_GET_SEALS) & (F_SEAL_GROW | F_SEAL_SHRINK)) == (F_SEAL_GROW | F_SEAL_SHRINK) &&
		      ftruncate(fd, 2 * len) == -1 && errno == EPERM,
	      "%s", strerror(errno));
	/* As /proc/self/fd/N on Linux. */
	char path[32];
	snprintf(path, sizeof path, "/dev/fd/%d", fd);
	int ro = open(path, O_RDONLY | O_CLOEXEC);
	CHECK("a read-only descriptor of a memory file", ro >= 0, "open %s: %s", path, strerror(errno));
	if (ro >= 0) {
		errno = 0;
		void *w = mmap(0, len, PROT_READ | PROT_WRITE, MAP_SHARED, ro, 0);
		CHECK("it cannot map the memory to write", w == MAP_FAILED && errno == EACCES, "mmap: %s", strerror(errno));
		char *r = mmap(0, len, PROT_READ, MAP_SHARED, ro, 0);
		CHECK("it maps the memory to read", r != MAP_FAILED && a != MAP_FAILED && !strcmp(r + page, "shared"),
		      "contents differ");
		if (r != MAP_FAILED)
			munmap(r, len);
		veda_handle_t vmo = VEDA_INVALID_HANDLE;
		void *v = 0;
		int got = veda_memory_file(ro, &vmo);
		int refused = got == 0 ? veda_vmo_map(vmo, 0, len, VEDA_MAP_READ | VEDA_MAP_WRITE, &v) : 0;
		CHECK("its memory's handle cannot be mapped to write", got == 0 && refused == -VEDA_EACCESS_DENIED,
		      "veda_memory_file %d, veda_vmo_map %d", got, refused);
		if (vmo != VEDA_INVALID_HANDLE)
			veda_close(vmo);
		close(ro);
	}
	veda_handle_t vmo = VEDA_INVALID_HANDLE;
	void *v = 0;
	int got = veda_memory_file(fd, &vmo);
	int mapped = got == 0 ? veda_vmo_map(vmo, 0, len, VEDA_MAP_READ, &v) : -1;
	CHECK("a memory file's memory, as a handle", got == 0 && mapped == 0 && !strcmp((char *)v + page, "shared"),
	      "veda_memory_file %d, veda_vmo_map %d", got, mapped);
	if (mapped == 0)
		veda_vmo_unmap(v, len);
	if (vmo != VEDA_INVALID_HANDLE)
		veda_close(vmo);
	int other = eventfd(0, EFD_CLOEXEC);
	CHECK("only a memory file's", veda_memory_file(other, &vmo) == EINVAL && veda_memory_file(-1, &vmo) == EBADF,
	      "another descriptor's memory");
	close(other);
	if (a != MAP_FAILED)
		munmap(a, len);
	if (b != MAP_FAILED)
		munmap(b, len);
	close(fd);
}

static atomic_long spins;
static atomic_int spinner;
static atomic_int stop_spinning;

static void *spin(void *arg)
{
	(void)arg;
	atomic_store(&spinner, gettid());
	while (!atomic_load(&stop_spinning))
		atomic_fetch_add(&spins, 1);
	return 0;
}

/* Stopping another thread and reading its registers, as JavaScriptCore's
 * garbage collector does (<veda/thread.h>), and where the main thread's
 * stack is (pthread_getattr_np); the memory the process holds
 * (<veda/memory.h>). */
static void check_thread_suspension(void)
{
	pthread_t t;
	if (pthread_create(&t, 0, spin, 0)) {
		CHECK("a thread to stop", 0, "pthread_create failed");
		return;
	}
	while (!atomic_load(&spinner))
		sched_yield();
	pid_t tid = atomic_load(&spinner);
	pthread_attr_t attr;
	void *stack = 0;
	size_t size = 0;
	if (!pthread_getattr_np(t, &attr)) {
		pthread_attr_getstack(&attr, &stack, &size);
		pthread_attr_destroy(&attr);
	}
	int r = veda_thread_suspend(tid);
	long before = atomic_load(&spins);
	usleep(20000);
	long after = atomic_load(&spins);
	CHECK("a suspended thread stays put", r == 0 && before == after, "suspend %d, %ld then %ld spins", r, before, after);
	mcontext_t mc;
	r = veda_thread_registers(tid, &mc);
	unsigned long sp = VEDA_STACK_POINTER(mc);
	CHECK("its registers", r == 0 && VEDA_PROGRAM_COUNTER(mc) && sp >= (unsigned long)stack &&
	      sp < (unsigned long)stack + size, "returned %d, sp %#lx, stack %p+%zu", r, sp, stack, size);
	r = veda_thread_resume(tid);
	usleep(20000);
	CHECK("a resumed thread goes on", r == 0 && atomic_load(&spins) != after, "resume %d", r);
	CHECK("only a suspended thread is resumed", veda_thread_resume(tid) == EINVAL, "resumed twice");
	CHECK("a thread does not suspend itself", veda_thread_suspend(gettid()) == EINVAL, "suspended itself");
	atomic_store(&stop_spinning, 1);
	pthread_join(t, 0);
	CHECK("an ended thread is not suspended", veda_thread_suspend(tid) == ESRCH, "suspended");

	/* The main thread's stack, as the loader made it. */
	int local = 0;
	stack = 0;
	size = 0;
	r = pthread_getattr_np(pthread_self(), &attr);
	if (!r) {
		pthread_attr_getstack(&attr, &stack, &size);
		pthread_attr_destroy(&attr);
	}
	CHECK("the main thread's stack",
	      !r && (char *)&local >= (char *)stack && (char *)&local < (char *)stack + size && size >= 8 << 20,
	      "%p+%zu, a local at %p", stack, size, (void *)&local);

	size_t held = veda_process_memory();
	const size_t more = 8 << 20;
	char *touched = malloc(more);
	/* Through a volatile pointer: memory written and freed unread is no
	 * memory at all to the compiler, which may make neither. */
	if (touched)
		for (size_t i = 0; i < more; i += VEDA_PAGE_SIZE)
			((volatile char *)touched)[i] = 1;
	size_t now = veda_process_memory();
	CHECK("the memory the process holds", held > 0 && touched && now >= held + more / 2, "%zu, then %zu", held, now);
	free(touched);
}

static void check_files(void)
{
	FILE *f = fopen("notes.txt", "w");
	CHECK("fopen for writing", f != 0, "fopen failed");
	if (!f)
		return;
	for (int i = 0; i < 1000; i++)
		fprintf(f, "line %d\n", i);
	CHECK("fclose", fclose(f) == 0, "fclose failed");

	struct stat st;
	CHECK("stat", stat("notes.txt", &st) == 0 && S_ISREG(st.st_mode) && st.st_size == 8890,
	      "size %lld", (long long)st.st_size);
	struct statx stx;
	CHECK("statx",
	      statx(AT_FDCWD, "notes.txt", 0, STATX_BASIC_STATS, &stx) == 0 && (stx.stx_mask & STATX_SIZE) &&
		      stx.stx_size == 8890 && S_ISREG(stx.stx_mode) && stx.stx_ino == st.st_ino,
	      "%s", strerror(errno));
	f = fopen("notes.txt", "r");
	char line[64];
	int lines = 0, last = -1;
	while (f && fgets(line, sizeof line, f)) {
		lines++;
		sscanf(line, "line %d", &last);
	}
	CHECK("fgets", lines == 1000 && last == 999, "%d lines, last %d", lines, last);
	CHECK("fseek and ftell", f && fseek(f, 7, SEEK_SET) == 0 && ftell(f) == 7 && fgetc(f) == 'l',
	      "position wrong");
	if (f)
		fclose(f);

	int fd = open("raw.bin", O_RDWR | O_CREAT | O_TRUNC, 0644);
	CHECK("open", fd >= 0, "open failed");
	char block[100000];
	for (size_t i = 0; i < sizeof block; i++)
		block[i] = (char)(i * 7);
	CHECK("write large", write(fd, block, sizeof block) == (ssize_t)sizeof block, "short write");
	CHECK("lseek end", lseek(fd, 0, SEEK_END) == (off_t)sizeof block, "wrong size");
	char back[100000];
	CHECK("pread", pread(fd, back, sizeof back, 0) == (ssize_t)sizeof back && !memcmp(back, block, sizeof back),
	      "contents differ");
	CHECK("ftruncate", ftruncate(fd, 10) == 0 && fstat(fd, &st) == 0 && st.st_size == 10, "size %lld",
	      (long long)st.st_size);
	int fd2 = dup(fd);
	lseek(fd, 3, SEEK_SET);
	CHECK("dup shares the offset", lseek(fd2, 0, SEEK_CUR) == 3, "offset %lld", (long long)lseek(fd2, 0, SEEK_CUR));
	close(fd2);

	int ex = open("raw.bin", O_WRONLY | O_CREAT | O_EXCL, 0644);
	CHECK("O_EXCL", ex < 0 && errno == EEXIST, "opened an existing file");
	int ap = open("raw.bin", O_WRONLY | O_APPEND);
	write(ap, "tail", 4);
	close(ap);
	CHECK("O_APPEND", fstat(fd, &st) == 0 && st.st_size == 14, "size %lld", (long long)st.st_size);

	/* A file removed while open stays readable. */
	CHECK("unlink while open", unlink("raw.bin") == 0 && access("raw.bin", F_OK) != 0, "still there");
	char head[4] = { 0 };
	CHECK("read after unlink", pread(fd, head, 3, 0) == 3 && head[1] == block[1], "unreadable");
	close(fd);

	CHECK("missing file", open("no-such-file", O_RDONLY) < 0 && errno == ENOENT, "wrong error");

	f = fopen("a.txt", "w");
	fputs("A", f);
	fclose(f);
	f = fopen("b.txt", "w");
	fputs("B", f);
	fclose(f);
	CHECK("rename over a file", rename("a.txt", "b.txt") == 0, "rename failed");
	f = fopen("b.txt", "r");
	CHECK("renamed contents", f && fgetc(f) == 'A' && access("a.txt", F_OK) != 0, "old contents");
	if (f)
		fclose(f);

	struct stat prog;
	CHECK("programs are executable", stat(program, &prog) == 0 && (prog.st_mode & S_IXUSR) &&
	      access(program, X_OK) == 0, "mode %o", prog.st_mode);
	CHECK("text is not executable", access("b.txt", X_OK) != 0, "b.txt executable");
	CHECK("inodes differ", stat("b.txt", &st) == 0 && st.st_ino != prog.st_ino, "same inode");

	fd = open("/dev/null", O_RDWR);
	CHECK("/dev/null", fd >= 0 && write(fd, "x", 1) == 1 && read(fd, line, 1) == 0, "not a null device");
	close(fd);
	fd = open("/dev/urandom", O_RDONLY);
	unsigned char r[32] = { 0 };
	int nonzero = 0;
	if (fd >= 0 && read(fd, r, sizeof r) == sizeof r)
		for (int i = 0; i < 32; i++)
			nonzero |= r[i];
	CHECK("/dev/urandom", nonzero, "no random bytes");
	close(fd);
}

static void check_directories(void)
{
	char cwd[4096];
	CHECK("mkdir", mkdir("sub", 0755) == 0 && mkdir("sub/deeper", 0755) == 0, "mkdir failed");
	CHECK("mkdir existing", mkdir("sub", 0755) != 0 && errno == EEXIST, "made it twice");
	FILE *f = fopen("sub/one", "w");
	fclose(f);
	DIR *d = opendir("sub");
	int seen = 0, dot = 0;
	struct dirent *e;
	while (d && (e = readdir(d))) {
		if (!strcmp(e->d_name, "one") && e->d_type == DT_REG)
			seen |= 1;
		if (!strcmp(e->d_name, "deeper") && e->d_type == DT_DIR)
			seen |= 2;
		if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, ".."))
			dot++;
	}
	if (d)
		closedir(d);
	CHECK("readdir", seen == 3 && dot == 2, "seen %d, dots %d", seen, dot);
	char *before = getcwd(cwd, sizeof cwd);
	CHECK("chdir", before && chdir("sub/deeper") == 0, "chdir failed");
	char now[4096];
	CHECK("getcwd", getcwd(now, sizeof now) && strlen(now) > strlen(cwd) &&
	      !strcmp(now + strlen(now) - 10, "sub/deeper"), "now '%s'", now);
	CHECK("relative ..", access("../one", F_OK) == 0 && chdir("../..") == 0, "can't go back");
	CHECK("rmdir non-empty", rmdir("sub") != 0 && errno == ENOTEMPTY, "removed");
	CHECK("rmdir", unlink("sub/one") == 0 && rmdir("sub/deeper") == 0 && rmdir("sub") == 0, "rmdir failed");
}

static void check_pipes(void)
{
	int p[2];
	CHECK("pipe", pipe(p) == 0, "pipe failed");
	const char msg[] = "through the pipe";
	CHECK("pipe write", write(p[1], msg, sizeof msg) == sizeof msg, "short write");
	char buf[64];
	CHECK("pipe read", read(p[0], buf, sizeof buf) == sizeof msg && !strcmp(buf, msg), "got '%s'", buf);
	close(p[1]);
	CHECK("pipe end of file", read(p[0], buf, sizeof buf) == 0, "no EOF");
	close(p[0]);

	CHECK("pipe2 nonblocking", pipe2(p, O_NONBLOCK | O_CLOEXEC) == 0 && read(p[0], buf, 1) < 0 && errno == EAGAIN,
	      "read did not fail with EAGAIN");
	CHECK("close-on-exec", fcntl(p[0], F_GETFD) == FD_CLOEXEC, "flag missing");
	signal(SIGPIPE, SIG_IGN);
	close(p[0]);
	CHECK("EPIPE", write(p[1], "x", 1) < 0 && errno == EPIPE, "write to a closed pipe worked");
	close(p[1]);
	signal(SIGPIPE, SIG_DFL);
}

static __thread int tls_value = 5;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cond = PTHREAD_COND_INITIALIZER;
static int counter, ready;
static atomic_int atomic_counter;

static void *worker(void *arg)
{
	tls_value += (int)(intptr_t)arg;
	for (int i = 0; i < 10000; i++) {
		pthread_mutex_lock(&lock);
		counter++;
		pthread_mutex_unlock(&lock);
		atomic_fetch_add(&atomic_counter, 1);
	}
	pthread_mutex_lock(&lock);
	ready++;
	pthread_cond_signal(&cond);
	pthread_mutex_unlock(&lock);
	return (void *)(intptr_t)tls_value;
}

static void check_threads(void)
{
	pthread_t t[8];
	int started = 1;
	for (int i = 0; i < 8; i++)
		if (pthread_create(&t[i], 0, worker, (void *)(intptr_t)(i + 1)))
			started = 0;
	CHECK("pthread_create", started, "creation failed");
	pthread_mutex_lock(&lock);
	while (ready < 8)
		pthread_cond_wait(&cond, &lock);
	pthread_mutex_unlock(&lock);
	int tls_ok = 1;
	for (int i = 0; i < 8; i++) {
		void *r;
		pthread_join(t[i], &r);
		if ((intptr_t)r != 5 + i + 1)
			tls_ok = 0;
	}
	CHECK("mutexes", counter == 80000, "counter %d", counter);
	CHECK("atomics", atomic_counter == 80000, "counter %d", atomic_counter);
	CHECK("thread-local storage", tls_ok && tls_value == 5, "values mixed up");
	pthread_t d;
	CHECK("detached thread", pthread_create(&d, 0, worker, 0) == 0 && pthread_detach(d) == 0, "failed");
	struct timespec ts = { 0, 50 * 1000 * 1000 };
	nanosleep(&ts, 0);
}

static void check_time(void)
{
	struct timespec a, b;
	clock_gettime(CLOCK_MONOTONIC, &a);
	struct timespec ts = { 0, 20 * 1000 * 1000 };
	nanosleep(&ts, 0);
	clock_gettime(CLOCK_MONOTONIC, &b);
	long ms = (b.tv_sec - a.tv_sec) * 1000 + (b.tv_nsec - a.tv_nsec) / 1000000;
	CHECK("nanosleep", ms >= 19 && ms < 2000, "slept %ld ms", ms);
	time_t now = time(0);
	CHECK("time", now > 1700000000, "time %lld", (long long)now);
	struct timeval tv;
	CHECK("gettimeofday", gettimeofday(&tv, 0) == 0 && tv.tv_sec >= now - 2, "behind time()");
	/* Without TZ, local time is the system's: its offset from UTC, named
	   as the tz database names a zone of that offset alone ("+02"). */
	char *tz = getenv("TZ");
	unsetenv("TZ");
	tzset();
	struct tm local, utc;
	localtime_r(&now, &local);
	gmtime_r(&now, &utc);
	long apart = (long)(mktime(&local) - timegm(&utc));
	int named = local.tm_gmtoff == 0 ? !strcmp(tzname[0], "UTC") : tzname[0][0] == '+' || tzname[0][0] == '-';
	CHECK("the system's time zone", named && local.tm_gmtoff == -timezone && local.tm_gmtoff % 60 == 0 && !apart,
	      "%s, %ld s east, %ld s apart", tzname[0], local.tm_gmtoff, apart);
	/* TZ names another. */
	setenv("TZ", "<-03>3", 1);
	tzset();
	localtime_r(&now, &local);
	CHECK("TZ's time zone", local.tm_gmtoff == -3 * 3600 && !strcmp(tzname[0], "-03"), "%s, %ld s east", tzname[0],
	      local.tm_gmtoff);
	if (tz)
		setenv("TZ", tz, 1);
	else
		unsetenv("TZ");
	tzset();
}

static volatile sig_atomic_t got_signal;
static jmp_buf jump;

static void on_signal(int s)
{
	got_signal = s;
}

static void check_signals(void)
{
	signal(SIGUSR1, on_signal);
	raise(SIGUSR1);
	CHECK("raise", got_signal == SIGUSR1, "handler not run");
	sigset_t set, old;
	sigemptyset(&set);
	sigaddset(&set, SIGUSR2);
	signal(SIGUSR2, on_signal);
	got_signal = 0;
	sigprocmask(SIG_BLOCK, &set, &old);
	raise(SIGUSR2);
	int blocked = got_signal == 0;
	sigprocmask(SIG_SETMASK, &old, 0);
	CHECK("blocked signals wait", blocked && got_signal == SIGUSR2, "delivered at the wrong time");
	volatile int jumped = 0;
	if (setjmp(jump) == 0)
		longjmp(jump, 1);
	else
		jumped = 1;
	CHECK("setjmp/longjmp", jumped, "no jump");
}

/* Starts `program child MODE` with its output on a pipe; returns what it
   wrote and stores its wait status. */
static int spawn_child_with(const char *mode, const char *arg, char *out, size_t cap, int *status)
{
	int p[2];
	if (pipe(p))
		return -1;
	posix_spawn_file_actions_t fa;
	posix_spawn_file_actions_init(&fa);
	posix_spawn_file_actions_adddup2(&fa, p[1], 1);
	posix_spawn_file_actions_addclose(&fa, p[0]);
	char *argv[] = { "posix", "child", (char *)mode, (char *)arg, 0 };
	char *envp[] = { "VEDA_TEST=from-parent", 0 };
	pid_t pid;
	int r = posix_spawn(&pid, program, &fa, 0, argv, envp);
	posix_spawn_file_actions_destroy(&fa);
	close(p[1]);
	if (r) {
		close(p[0]);
		errno = r;
		return -1;
	}
	size_t n = 0;
	ssize_t got;
	while (n + 1 < cap && (got = read(p[0], out + n, cap - 1 - n)) > 0)
		n += got;
	out[n] = 0;
	close(p[0]);
	return waitpid(pid, status, 0) == pid ? 0 : -1;
}

/* Runs `posix child MODE` with its standard output in OUT: 0, or -1. */
static int spawn_child(const char *mode, char *out, size_t cap, int *status)
{
	return spawn_child_with(mode, 0, out, cap, status);
}

static void check_processes(void)
{
	char out[4096];
	int status = 0;
	int r = spawn_child("echo", out, sizeof out, &status);
	CHECK("posix_spawn", r == 0, "spawn failed");
	CHECK("child output", !strcmp(out, "child says hello from-parent\n"), "got '%s'", out);
	CHECK("child exit status", WIFEXITED(status) && WEXITSTATUS(status) == 7, "status %#x", status);
	r = spawn_child("abort", out, sizeof out, &status);
	CHECK("child killed by a signal", r == 0 && WIFSIGNALED(status) && WTERMSIG(status) == SIGABRT,
	      "status %#x", status);
	CHECK("child sees the directory", spawn_child("cwd", out, sizeof out, &status) == 0 &&
	      WEXITSTATUS(status) == 0, "child status %#x: '%s'", status, out);
	FILE *p = popen("missing-program", "r");
	CHECK("popen of a missing shell", p == 0 || pclose(p) != 0, "succeeded");
	CHECK("waitpid without children", waitpid(-1, &status, WNOHANG) < 0 && errno == ECHILD, "no ECHILD");
	char *e = getenv("HOME");
	CHECK("environment", e && *e == '/', "HOME is '%s'", e ? e : "(unset)");
}

/* Veda's own interfaces (<veda/ipc.h>): the program's startup handles,
 * and objects of its own. */
static void check_veda_interfaces(void)
{
	veda_handle_t h = VEDA_INVALID_HANDLE;
	/* systest starts it with descriptors and the registry only. */
	int r = veda_startup_handle(VEDA_ROLE_USER, &h);
	CHECK("no startup handle of an application's role", r == -VEDA_ENOT_FOUND, "returned %d (handle %u)", r, h);
	r = veda_startup_handle(16, &h);
	CHECK("the layer's startup handles stay its own", r == -VEDA_EINVALID_ARGS, "returned %d", r);
	veda_handle_t event;
	r = veda_event_create(&event);
	CHECK("event", r == 0, "returned %d", r);
	if (r == 0) {
		struct veda_wait_item item = { .handle = event, .signals = VEDA_SIGNALED };
		r = veda_wait(&item, 1, 0);
		CHECK("an event not signalled yet", r == 0, "wait returned %d", r);
		veda_object_signal(event, 0, VEDA_SIGNALED);
		r = veda_wait(&item, 1, 0);
		CHECK("a signalled event", r == 1 && (item.observed & VEDA_SIGNALED), "wait returned %d, observed %#x", r,
		      item.observed);
		veda_close(event);
	}
}

/* An IPv4 socket address. */
static struct sockaddr_in ipv4(in_addr_t addr, int port)
{
	return (struct sockaddr_in){ .sin_family = AF_INET, .sin_port = htons(port), .sin_addr.s_addr = addr };
}

/* TCP and UDP on the loopback interface, names, and, when systest names
 * them, the host machine's servers: a web server that answers GET /hello
 * with "hello from the host", and a UDP echo. */
static void check_network(const char *host, int tcp_port, int udp_port)
{
	int one = 1;
	char buf[256] = { 0 };
	struct sockaddr_in addr = ipv4(htonl(INADDR_LOOPBACK), 0);
	socklen_t len = sizeof addr;
	int server = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
	CHECK("socket", server >= 0 && setsockopt(server, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one) == 0,
	      "socket %d", server);
	CHECK("bind and listen",
	      bind(server, (struct sockaddr *)&addr, sizeof addr) == 0 && listen(server, 4) == 0 &&
		      getsockname(server, (struct sockaddr *)&addr, &len) == 0 && addr.sin_port != 0,
	      "port %d", ntohs(addr.sin_port));
	/* A connection made without waiting: in progress, then writable. */
	int client = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
	int r = connect(client, (struct sockaddr *)&addr, sizeof addr);
	CHECK("connect without waiting", r == 0 || errno == EINPROGRESS, "connect returned %d", r);
	struct pollfd pfd = { .fd = client, .events = POLLOUT };
	int err = -1;
	socklen_t errlen = sizeof err;
	CHECK("poll for the connection",
	      poll(&pfd, 1, 5000) == 1 && (pfd.revents & POLLOUT) &&
		      getsockopt(client, SOL_SOCKET, SO_ERROR, &err, &errlen) == 0 && err == 0,
	      "revents %#x, error %d", pfd.revents, err);
	struct sockaddr_in peer, mine;
	socklen_t peerlen = sizeof peer, minelen = sizeof mine;
	int conn = accept4(server, (struct sockaddr *)&peer, &peerlen, SOCK_CLOEXEC);
	CHECK("accept", conn >= 0 && getsockname(client, (struct sockaddr *)&mine, &minelen) == 0 &&
	      peer.sin_port == mine.sin_port && peer.sin_addr.s_addr == htonl(INADDR_LOOPBACK),
	      "connection %d from port %d", conn, ntohs(peer.sin_port));
	CHECK("TCP_NODELAY", setsockopt(conn, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one) == 0, "refused");
	/* Both ways, through poll and the message calls. */
	const char hello[] = "hello, network";
	CHECK("send", send(client, hello, sizeof hello, MSG_NOSIGNAL) == sizeof hello, "short send");
	pfd = (struct pollfd){ .fd = conn, .events = POLLIN };
	int pending = 0;
	CHECK("poll for data", poll(&pfd, 1, 5000) == 1 && (pfd.revents & POLLIN) &&
	      ioctl(conn, FIONREAD, &pending) == 0 && pending == sizeof hello, "revents %#x, %d bytes",
	      pfd.revents, pending);
	CHECK("recv", recv(conn, buf, sizeof buf, 0) == sizeof hello && !strcmp(buf, hello), "got '%s'", buf);
	struct iovec iov[2] = { { "back", 4 }, { " again", 6 } };
	struct msghdr mh = { .msg_iov = iov, .msg_iovlen = 2 };
	CHECK("sendmsg", sendmsg(conn, &mh, 0) == 10, "short sendmsg");
	pfd = (struct pollfd){ .fd = client, .events = POLLIN };
	memset(buf, 0, sizeof buf);
	CHECK("read", poll(&pfd, 1, 5000) == 1 && read(client, buf, sizeof buf) == 10 && !strcmp(buf, "back again"),
	      "got '%s'", buf);
	CHECK("nothing more without waiting", recv(client, buf, sizeof buf, MSG_DONTWAIT) < 0 && errno == EAGAIN,
	      "a read did not fail");
	/* The end of the stream. */
	CHECK("shutdown", shutdown(client, SHUT_WR) == 0, "failed");
	CHECK("end of stream", recv(conn, buf, sizeof buf, 0) == 0, "no end");
	close(conn);
	close(client);
	/* A port nobody listens on refuses. */
	close(server);
	int refused = socket(AF_INET, SOCK_STREAM, 0);
	CHECK("connection refused", connect(refused, (struct sockaddr *)&addr, sizeof addr) < 0 && errno == ECONNREFUSED,
	      "connected");
	close(refused);

	/* Datagrams. */
	int u1 = socket(AF_INET, SOCK_DGRAM, 0), u2 = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
	struct sockaddr_in a1 = ipv4(htonl(INADDR_LOOPBACK), 0), a2 = a1, from;
	socklen_t l1 = sizeof a1, l2 = sizeof a2, fromlen = sizeof from;
	CHECK("bind datagram sockets",
	      bind(u1, (struct sockaddr *)&a1, sizeof a1) == 0 && bind(u2, (struct sockaddr *)&a2, sizeof a2) == 0 &&
		      getsockname(u1, (struct sockaddr *)&a1, &l1) == 0 && getsockname(u2, (struct sockaddr *)&a2, &l2) == 0,
	      "ports %d %d", ntohs(a1.sin_port), ntohs(a2.sin_port));
	CHECK("sendto", sendto(u1, "datagram", 8, 0, (struct sockaddr *)&a2, sizeof a2) == 8, "short sendto");
	memset(buf, 0, sizeof buf);
	r = recvfrom(u2, buf, sizeof buf, 0, (struct sockaddr *)&from, &fromlen);
	CHECK("recvfrom", r == 8 && !strcmp(buf, "datagram") && from.sin_port == a1.sin_port &&
	      fromlen == sizeof from, "got %d bytes '%s' from port %d", r, buf, ntohs(from.sin_port));
	/* A datagram cut short says so. */
	CHECK("sendto", sendto(u2, "a longer datagram", 17, 0, (struct sockaddr *)&a1, sizeof a1) == 17, "short");
	char small[4];
	struct iovec piece = { small, sizeof small };
	struct msghdr m2 = { .msg_name = &from, .msg_namelen = sizeof from, .msg_iov = &piece, .msg_iovlen = 1 };
	r = recvmsg(u1, &m2, 0);
	CHECK("recvmsg of a longer datagram", r == 4 && (m2.msg_flags & MSG_TRUNC) && !memcmp(small, "a lo", 4) &&
	      from.sin_port == a2.sin_port, "got %d, flags %#x", r, m2.msg_flags);
	close(u1);
	close(u2);

	/* Names. */
	struct addrinfo hints = { .ai_socktype = SOCK_STREAM }, *res = 0;
	int gai = getaddrinfo("localhost", "80", &hints, &res);
	struct sockaddr_in *first = res ? (struct sockaddr_in *)res->ai_addr : 0;
	CHECK("getaddrinfo", gai == 0 && first && res->ai_family == AF_INET &&
	      first->sin_addr.s_addr == htonl(INADDR_LOOPBACK) && ntohs(first->sin_port) == 80, "%s", gai_strerror(gai));
	if (res)
		freeaddrinfo(res);
	res = 0;
	gai = getaddrinfo("no such host", 0, &hints, &res);
	CHECK("getaddrinfo of no name", gai == EAI_NONAME, "%s", gai_strerror(gai));
	if (res)
		freeaddrinfo(res);
	char name[64];
	struct sockaddr_in lo = ipv4(htonl(INADDR_LOOPBACK), 0);
	CHECK("getnameinfo", getnameinfo((struct sockaddr *)&lo, sizeof lo, name, sizeof name, 0, 0, NI_NUMERICHOST) == 0 &&
	      !strcmp(name, "127.0.0.1"), "got '%s'", name);

	/* The host machine's servers. */
	if (!host) {
		printf("# the host: no servers were named\n");
		return;
	}
	struct sockaddr_in web = ipv4(inet_addr(host), tcp_port), echo = ipv4(inet_addr(host), udp_port);
	/* The network comes up after the system starts (its card is the
	   driver VM's): until it does, a connection fails at once. */
	int s = -1;
	for (int i = 0; i < 240; i++) {
		s = socket(AF_INET, SOCK_STREAM, 0);
		if (connect(s, (struct sockaddr *)&web, sizeof web) == 0)
			break;
		int down = errno == ENETDOWN || errno == ENETUNREACH || errno == EHOSTUNREACH;
		close(s);
		s = -1;
		if (!down)
			break;
		usleep(250000);
	}
	CHECK("connect to the host", s >= 0, "%s:%d", host, tcp_port);
	if (s < 0)
		return;
	char request[128];
	int n = snprintf(request, sizeof request, "GET /hello HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n", host);
	CHECK("request to the host", write(s, request, n) == n, "short write");
	char response[1024] = { 0 };
	size_t got = 0;
	ssize_t k;
	while (got + 1 < sizeof response && (k = read(s, response + got, sizeof response - 1 - got)) > 0)
		got += k;
	CHECK("the host's answer", !strncmp(response, "HTTP/1.1 200 OK", 15) && strstr(response, "\r\n\r\nhello from the host\n"),
	      "got '%s'", response);
	close(s);
	int u = socket(AF_INET, SOCK_DGRAM, 0);
	struct timeval tv = { .tv_sec = 2 };
	setsockopt(u, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);
	r = -1;
	/* A datagram may be lost: a few tries. */
	for (int i = 0; i < 5 && r < 0; i++) {
		sendto(u, "echo from C", 11, 0, (struct sockaddr *)&echo, sizeof echo);
		fromlen = sizeof from;
		r = recvfrom(u, buf, sizeof buf, 0, (struct sockaddr *)&from, &fromlen);
	}
	CHECK("the host's echo", r == 11 && !memcmp(buf, "echo from C", 11) && from.sin_addr.s_addr == echo.sin_addr.s_addr &&
	      from.sin_port == echo.sin_port, "got %d bytes", r);
	close(u);
}

/* Writes to an eventfd after a moment, from another thread. */
static void *write_later(void *arg)
{
	usleep(50000);
	uint64_t one = 1;
	if (write(*(int *)arg, &one, 8) != 8)
		return (void *)1;
	return 0;
}

/* Waits up to a second for one event of `ep`: its data, or -1. */
static long long one_event(int ep, uint32_t *events)
{
	struct epoll_event out[4];
	if (epoll_wait(ep, out, 4, 1000) != 1)
		return -1;
	*events = out[0].events;
	return (long long)out[0].data.u64;
}

/* Milliseconds since `t0` (monotonic). */
static long ms_since(const struct timespec *t0)
{
	struct timespec t1;
	clock_gettime(CLOCK_MONOTONIC, &t1);
	return (t1.tv_sec - t0->tv_sec) * 1000 + (t1.tv_nsec - t0->tv_nsec) / 1000000;
}

/* timerfd: disarmed, one-shot, periodic and absolute timers, what
 * timerfd_gettime and timerfd_settime's old value say, poll and epoll
 * waking at an expiration, and a read that waits for one. */
static void check_timerfd(void)
{
	uint64_t n = 0;
	int t = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC | TFD_NONBLOCK);
	CHECK("timerfd_create", t >= 0 && fcntl(t, F_GETFD) == FD_CLOEXEC, "%s", strerror(errno));
	struct itimerspec now = { 0 };
	int disarmed = read(t, &n, 8) < 0 && errno == EAGAIN && timerfd_gettime(t, &now) == 0 &&
		       now.it_value.tv_sec == 0 && now.it_value.tv_nsec == 0;
	CHECK("a disarmed timer", disarmed, "%s", strerror(errno));
	CHECK("a timer is not written to", write(t, &n, 8) < 0 && errno == EINVAL, "%s", strerror(errno));

	/* Once, 30 ms from now. */
	struct itimerspec once = { .it_value = { .tv_nsec = 30000000 } };
	struct timespec t0;
	clock_gettime(CLOCK_MONOTONIC, &t0);
	int armed = timerfd_settime(t, 0, &once, 0) == 0 && timerfd_gettime(t, &now) == 0 && now.it_value.tv_sec == 0 &&
		    now.it_value.tv_nsec > 0 && now.it_value.tv_nsec <= 30000000;
	CHECK("a one-shot timer", armed, "%ld ns left", now.it_value.tv_nsec);
	struct pollfd pfd = { .fd = t, .events = POLLIN };
	int polled = poll(&pfd, 1, 1000) == 1 && (pfd.revents & POLLIN);
	long ms = ms_since(&t0);
	CHECK("poll wakes as it expires", polled && ms >= 25 && ms < 1000, "after %ld ms", ms);
	CHECK("its one expiration", read(t, &n, 8) == 8 && n == 1 && read(t, &n, 8) < 0 && errno == EAGAIN,
	      "read %llu", (unsigned long long)n);

	/* Every 10 ms. */
	struct itimerspec every = { .it_value = { .tv_nsec = 10000000 }, .it_interval = { .tv_nsec = 10000000 } };
	struct itimerspec old = { 0 };
	CHECK("a periodic timer", timerfd_settime(t, 0, &every, &old) == 0 && old.it_value.tv_sec == 0 &&
		  old.it_value.tv_nsec == 0, "old value %ld ns", old.it_value.tv_nsec);
	usleep(55000);
	CHECK("its expirations add up", read(t, &n, 8) == 8 && n >= 4 && n <= 8, "read %llu",
	      (unsigned long long)n);
	CHECK("timerfd_gettime of a periodic timer", timerfd_gettime(t, &now) == 0 && now.it_interval.tv_nsec == 10000000 &&
		  now.it_value.tv_nsec > 0 && now.it_value.tv_nsec <= 10000000, "%ld ns left", now.it_value.tv_nsec);
	struct itimerspec off = { 0 };
	int stopped = timerfd_settime(t, 0, &off, &old) == 0 && old.it_interval.tv_nsec == 10000000 &&
		      timerfd_gettime(t, &now) == 0 && now.it_value.tv_nsec == 0 && now.it_interval.tv_nsec == 0;
	CHECK("disarming it", stopped, "%ld ns left", now.it_value.tv_nsec);

	/* An absolute time already past: expired at once. */
	struct itimerspec past = { .it_value = { .tv_sec = 0, .tv_nsec = 1 } };
	CHECK("an absolute time already past",
	      timerfd_settime(t, TFD_TIMER_ABSTIME, &past, 0) == 0 && read(t, &n, 8) == 8 && n == 1, "read %llu",
	      (unsigned long long)n);

	/* epoll, waking as it expires. */
	int ep = epoll_create1(EPOLL_CLOEXEC);
	struct epoll_event ev = { .events = EPOLLIN, .data.u64 = 13 };
	uint32_t events = 0;
	clock_gettime(CLOCK_MONOTONIC, &t0);
	int woke = ep >= 0 && epoll_ctl(ep, EPOLL_CTL_ADD, t, &ev) == 0 && timerfd_settime(t, 0, &once, 0) == 0 &&
		   one_event(ep, &events) == 13 && (events & EPOLLIN);
	ms = ms_since(&t0);
	CHECK("epoll wakes as it expires", woke && ms >= 25, "after %ld ms", ms);
	close(ep);
	close(t);

	/* A read that waits, on a realtime timer. */
	int w = timerfd_create(CLOCK_REALTIME, 0);
	clock_gettime(CLOCK_MONOTONIC, &t0);
	struct itimerspec soon = { .it_value = { .tv_nsec = 20000000 } };
	int waited = w >= 0 && timerfd_settime(w, 0, &soon, 0) == 0 && read(w, &n, 8) == 8 && n == 1;
	ms = ms_since(&t0);
	CHECK("a read that waits for it", waited && ms >= 15, "read %llu after %ld ms", (unsigned long long)n, ms);
	close(w);
	CHECK("an unknown clock", timerfd_create(CLOCK_PROCESS_CPUTIME_ID, 0) < 0 && errno == EINVAL, "made");
}

/* eventfd and epoll: an eventfd's counts and its semaphore mode, level- and
 * edge-triggered and one-shot registrations of pipes, socket pairs, TCP
 * sockets and eventfds, changing and removing them, a wait woken by
 * another thread, time-outs. */
static void check_epoll(void)
{
	uint64_t v = 0;
	int e = eventfd(3, EFD_CLOEXEC | EFD_NONBLOCK);
	CHECK("eventfd", e >= 0 && read(e, &v, 8) == 8 && v == 3 && read(e, &v, 8) < 0 && errno == EAGAIN,
	      "read %llu", (unsigned long long)v);
	v = 2;
	int added = write(e, &v, 8) == 8 && write(e, &v, 8) == 8 && read(e, &v, 8) == 8 && v == 4;
	CHECK("eventfd adds", added, "read %llu", (unsigned long long)v);
	CHECK("eventfd's short read", read(e, &v, 4) < 0 && errno == EINVAL, "a 4-byte read worked");
	int sem = eventfd(2, EFD_SEMAPHORE | EFD_NONBLOCK);
	int semaphore = read(sem, &v, 8) == 8 && v == 1 && read(sem, &v, 8) == 8 && v == 1 &&
			read(sem, &v, 8) < 0 && errno == EAGAIN;
	CHECK("eventfd as a semaphore", semaphore, "read %llu", (unsigned long long)v);
	close(sem);
	struct pollfd pfd = { .fd = e, .events = POLLIN | POLLOUT };
	CHECK("poll of an eventfd", poll(&pfd, 1, 0) == 1 && pfd.revents == POLLOUT, "revents %#x", pfd.revents);

	int ep = epoll_create1(EPOLL_CLOEXEC);
	int p[2];
	CHECK("epoll_create1", ep >= 0 && pipe2(p, O_NONBLOCK) == 0 && fcntl(ep, F_GETFD) == FD_CLOEXEC, "failed");
	struct epoll_event ev = { .events = EPOLLIN, .data.u64 = 7 }, out[4];
	CHECK("epoll_ctl", epoll_ctl(ep, EPOLL_CTL_ADD, p[0], &ev) == 0, "failed");
	CHECK("adding twice", epoll_ctl(ep, EPOLL_CTL_ADD, p[0], &ev) < 0 && errno == EEXIST, "added again");
	struct timespec t0, t1;
	clock_gettime(CLOCK_MONOTONIC, &t0);
	int n = epoll_wait(ep, out, 4, 50);
	clock_gettime(CLOCK_MONOTONIC, &t1);
	long ms = (t1.tv_sec - t0.tv_sec) * 1000 + (t1.tv_nsec - t0.tv_nsec) / 1000000;
	CHECK("epoll_wait's time-out", n == 0 && ms >= 45 && ms < 2000, "%d events after %ld ms", n, ms);
	char buf[16];
	uint32_t events = 0;
	/* Level-triggered: reported while readable. */
	CHECK("a write", write(p[1], "x", 1) == 1, "failed");
	int level = one_event(ep, &events) == 7 && (events & EPOLLIN) && one_event(ep, &events) == 7;
	CHECK("level-triggered", level, "events %#x", events);
	CHECK("read, not ready", read(p[0], buf, sizeof buf) == 1 && epoll_wait(ep, out, 4, 0) == 0, "still ready");
	/* Edge-triggered: once, until it is ready anew. */
	ev = (struct epoll_event){ .events = EPOLLIN | EPOLLET, .data.u64 = 8 };
	CHECK("EPOLL_CTL_MOD", epoll_ctl(ep, EPOLL_CTL_MOD, p[0], &ev) == 0, "failed");
	CHECK("a write", write(p[1], "ab", 2) == 2, "failed");
	int edge = one_event(ep, &events) == 8 && epoll_wait(ep, out, 4, 0) == 0;
	CHECK("edge-triggered", edge, "events %#x", events);
	CHECK("drained", read(p[0], buf, sizeof buf) == 2 && read(p[0], buf, sizeof buf) < 0 && errno == EAGAIN, "?");
	CHECK("a write", write(p[1], "c", 1) == 1, "failed");
	CHECK("edge-triggered, anew", one_event(ep, &events) == 8, "no event");
	/* The end of a pipe's input. */
	close(p[1]);
	CHECK("hang-up", one_event(ep, &events) == 8 && (events & EPOLLHUP), "events %#x", events);
	int deleted = epoll_ctl(ep, EPOLL_CTL_DEL, p[0], 0) == 0 && epoll_ctl(ep, EPOLL_CTL_DEL, p[0], 0) < 0 &&
		      errno == ENOENT;
	CHECK("EPOLL_CTL_DEL", deleted, "failed");
	close(p[0]);

	/* Each write to an eventfd is an event, read or not: what async
	   runtimes' wakers count on. */
	ev = (struct epoll_event){ .events = EPOLLIN | EPOLLET, .data.u64 = 9 };
	v = 1;
	int edges = epoll_ctl(ep, EPOLL_CTL_ADD, e, &ev) == 0 && epoll_wait(ep, out, 4, 0) == 0 &&
		    write(e, &v, 8) == 8 && one_event(ep, &events) == 9 && epoll_wait(ep, out, 4, 0) == 0 &&
		    write(e, &v, 8) == 8 && one_event(ep, &events) == 9;
	CHECK("an eventfd's writes", edges, "events %#x", events);
	pthread_t writer;
	CHECK("a wait woken by another thread",
	      pthread_create(&writer, 0, write_later, &e) == 0 && one_event(ep, &events) == 9 &&
		      pthread_join(writer, 0) == 0,
	      "no event");
	epoll_ctl(ep, EPOLL_CTL_DEL, e, 0);

	/* One-shot: once, until changed. */
	int s[2];
	ev = (struct epoll_event){ .events = EPOLLIN | EPOLLONESHOT, .data.u64 = 10 };
	int once = socketpair(AF_UNIX, SOCK_STREAM, 0, s) == 0 && epoll_ctl(ep, EPOLL_CTL_ADD, s[0], &ev) == 0 &&
		   write(s[1], "x", 1) == 1 && one_event(ep, &events) == 10 && epoll_wait(ep, out, 4, 0) == 0 &&
		   epoll_ctl(ep, EPOLL_CTL_MOD, s[0], &ev) == 0 && one_event(ep, &events) == 10;
	CHECK("one-shot", once, "events %#x", events);
	close(s[0]);
	close(s[1]);

	/* A TCP connection: the listener readable with it, the client
	   writable once it is made, then readable with what comes. */
	int server = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
	struct sockaddr_in addr = ipv4(htonl(INADDR_LOOPBACK), 0);
	socklen_t len = sizeof addr;
	int listening = server >= 0 && bind(server, (struct sockaddr *)&addr, sizeof addr) == 0 &&
			listen(server, 4) == 0 && getsockname(server, (struct sockaddr *)&addr, &len) == 0;
	ev = (struct epoll_event){ .events = EPOLLIN | EPOLLET, .data.u64 = 11 };
	CHECK("a listener", listening && epoll_ctl(ep, EPOLL_CTL_ADD, server, &ev) == 0, "failed");
	int client = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
	int r = connect(client, (struct sockaddr *)&addr, sizeof addr);
	ev = (struct epoll_event){ .events = EPOLLIN | EPOLLOUT | EPOLLRDHUP | EPOLLET, .data.u64 = 12 };
	CHECK("connecting", (r == 0 || errno == EINPROGRESS) && epoll_ctl(ep, EPOLL_CTL_ADD, client, &ev) == 0,
	      "failed");
	/* Both events come, in either order. */
	int listener_ready = 0, client_writable = 0;
	for (int i = 0; i < 4 && !(listener_ready && client_writable); i++) {
		n = epoll_wait(ep, out, 4, 1000);
		for (int j = 0; j < n; j++) {
			listener_ready |= out[j].data.u64 == 11 && (out[j].events & EPOLLIN);
			client_writable |= out[j].data.u64 == 12 && (out[j].events & EPOLLOUT);
		}
	}
	CHECK("a connection's events", listener_ready && client_writable, "listener %d, client %d", listener_ready,
	      client_writable);
	int conn = accept4(server, 0, 0, SOCK_NONBLOCK);
	CHECK("accept4", conn >= 0 && accept4(server, 0, 0, SOCK_NONBLOCK) < 0 && errno == EAGAIN, "failed");
	CHECK("send", send(conn, "hello", 5, MSG_NOSIGNAL) == 5, "failed");
	int readable = one_event(ep, &events) == 12 && (events & EPOLLIN) && recv(client, buf, sizeof buf, 0) == 5;
	CHECK("readable with what came", readable, "events %#x", events);
	close(conn);
	/* Edge-triggered events may come that say nothing new (as on Linux):
	   the peer's end is among the next few. */
	int ended = 0;
	for (int i = 0; i < 4 && !ended; i++)
		ended = one_event(ep, &events) == 12 && (events & EPOLLRDHUP);
	CHECK("the peer's end", ended && recv(client, buf, sizeof buf, 0) == 0, "events %#x", events);
	close(client);
	close(server);

	/* What is always ready cannot be added, as on Linux. */
	int file = open("notes.txt", O_RDONLY);
	ev = (struct epoll_event){ .events = EPOLLIN };
	CHECK("files cannot be added", file >= 0 && epoll_ctl(ep, EPOLL_CTL_ADD, file, &ev) < 0 && errno == EPERM,
	      "added");
	close(file);
	close(e);
	close(ep);
}

/* A Unix domain socket's address for `path`, and its length. */
static socklen_t unix_addr(struct sockaddr_un *a, const char *path)
{
	memset(a, 0, sizeof *a);
	a->sun_family = AF_UNIX;
	snprintf(a->sun_path, sizeof a->sun_path, "%s", path);
	return offsetof(struct sockaddr_un, sun_path) + strlen(path) + 1;
}

/* Sends the byte `what` over `s` with the `n` descriptors at `fds`. */
static int send_fds(int s, char what, const int *fds, int n)
{
	struct iovec iov = { &what, 1 };
	union {
		struct cmsghdr h;
		char buf[CMSG_SPACE(sizeof(int) * 4)];
	} u;
	struct msghdr m = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = u.buf,
			    .msg_controllen = CMSG_SPACE(sizeof(int) * n) };
	struct cmsghdr *c = CMSG_FIRSTHDR(&m);
	c->cmsg_level = SOL_SOCKET;
	c->cmsg_type = SCM_RIGHTS;
	c->cmsg_len = CMSG_LEN(sizeof(int) * n);
	memcpy(CMSG_DATA(c), fds, sizeof(int) * n);
	return sendmsg(s, &m, 0) == 1 ? 0 : -1;
}

/* Receives up to `len` bytes from `s` into `buf`, and the descriptors that
   come with them into `fds`, with room for `room` of them: how many bytes
   came (-1 on failure); `*got` how many descriptors, `*flags` the
   message's flags. */
static ssize_t recv_fds(int s, char *buf, size_t len, int *fds, int room, int *got, int *flags)
{
	struct iovec iov = { buf, len };
	union {
		struct cmsghdr h;
		char buf[CMSG_SPACE(sizeof(int) * 8)];
	} u;
	struct msghdr m = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = u.buf,
			    .msg_controllen = room ? CMSG_SPACE(sizeof(int) * room) : 0 };
	ssize_t n = recvmsg(s, &m, MSG_CMSG_CLOEXEC);
	*got = 0;
	*flags = m.msg_flags;
	if (n < 0)
		return n;
	for (struct cmsghdr *c = CMSG_FIRSTHDR(&m); c; c = CMSG_NXTHDR(&m, c)) {
		if (c->cmsg_level != SOL_SOCKET || c->cmsg_type != SCM_RIGHTS)
			continue;
		int k = (c->cmsg_len - CMSG_LEN(0)) / sizeof(int);
		memcpy(fds + *got, CMSG_DATA(c), sizeof(int) * k);
		*got += k;
	}
	return n;
}

/* Unix domain sockets: a listener named by a path, which stat shows as a
 * socket; connections to it, peeking, credentials, the end of a stream;
 * datagram sockets with names and without; seqpacket and datagram pairs;
 * descriptors sent over them (SCM_RIGHTS), in this process and to a child,
 * and who sent a message (SCM_CREDENTIALS). */
static void check_unix_sockets(void)
{
	char path[64], dpath[64], buf[256];
	snprintf(path, sizeof path, "/tmp/posix-%d.sock", (int)getpid());
	snprintf(dpath, sizeof dpath, "/tmp/posix-%d.dgram", (int)getpid());
	unlink(path);
	unlink(dpath);
	struct sockaddr_un a, b;
	socklen_t alen = unix_addr(&a, path), len;
	int listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	struct stat st;
	int bound = listener >= 0 && bind(listener, (struct sockaddr *)&a, alen) == 0 && stat(path, &st) == 0;
	CHECK("a socket bound to a path", bound && S_ISSOCK(st.st_mode), "mode %o", bound ? st.st_mode : 0);
	int other = socket(AF_UNIX, SOCK_STREAM, 0);
	CHECK("binding a name taken", bind(other, (struct sockaddr *)&a, alen) < 0 && errno == EADDRINUSE, "bound");
	CHECK("opening a socket", open(path, O_RDONLY) < 0 && errno == ENXIO, "opened");
	DIR *dir = opendir("/tmp");
	int typed = 0;
	for (struct dirent *e; dir && (e = readdir(dir));)
		typed |= !strcmp(e->d_name, path + strlen("/tmp/")) && e->d_type == DT_SOCK;
	if (dir)
		closedir(dir);
	CHECK("a socket in a listing", typed, "no DT_SOCK entry");
	len = sizeof b;
	int named = getsockname(listener, (struct sockaddr *)&b, &len) == 0 && len == alen && !strcmp(b.sun_path, path);
	CHECK("getsockname", named, "length %u, '%s'", len, b.sun_path);
	CHECK("connecting to a socket not listening",
	      connect(other, (struct sockaddr *)&a, alen) < 0 && errno == ECONNREFUSED, "connected");
	close(other);
	struct pollfd pfd = { .fd = listener, .events = POLLIN };
	CHECK("listen", listen(listener, 8) == 0 && poll(&pfd, 1, 0) == 0, "a connection is waiting");
	int ep = epoll_create1(EPOLL_CLOEXEC);
	struct epoll_event ev = { .events = EPOLLIN | EPOLLET, .data.u64 = 21 }, out[2];
	CHECK("a listener in epoll", epoll_ctl(ep, EPOLL_CTL_ADD, listener, &ev) == 0 && epoll_wait(ep, out, 2, 0) == 0,
	      "failed");

	int client = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0);
	CHECK("connect", client >= 0 && connect(client, (struct sockaddr *)&a, alen) == 0, "failed");
	CHECK("a listener with a connection", poll(&pfd, 1, 1000) == 1 && pfd.revents == POLLIN, "revents %#x",
	      pfd.revents);
	int n_ev = epoll_wait(ep, out, 2, 1000);
	CHECK("its edge", n_ev == 1 && out[0].data.u64 == 21 && (out[0].events & EPOLLIN), "%d events", n_ev);
	close(ep);
	len = sizeof b;
	int conn = accept4(listener, (struct sockaddr *)&b, &len, SOCK_CLOEXEC);
	CHECK("accept4", conn >= 0 && len == sizeof(sa_family_t), "length %u", len);
	len = sizeof b;
	CHECK("getpeername", getpeername(client, (struct sockaddr *)&b, &len) == 0 && !strcmp(b.sun_path, path),
	      "'%s'", b.sun_path);
	struct ucred cred;
	socklen_t clen = sizeof cred;
	int credited = getsockopt(conn, SOL_SOCKET, SO_PEERCRED, &cred, &clen) == 0 && clen == sizeof cred &&
		       cred.pid == getpid() && cred.uid == getuid() && cred.gid == getgid();
	CHECK("SO_PEERCRED", credited, "pid %d, uid %u", (int)cred.pid, cred.uid);
	int avail = 0;
	int peeked = write(client, "hello", 5) == 5 && recv(conn, buf, sizeof buf, MSG_PEEK) == 5 &&
		     ioctl(conn, FIONREAD, &avail) == 0 && avail == 5;
	CHECK("MSG_PEEK", peeked && read(conn, buf, sizeof buf) == 5 && !memcmp(buf, "hello", 5), "avail %d", avail);
	pfd = (struct pollfd){ .fd = conn, .events = POLLIN | POLLRDHUP };
	int ended = shutdown(client, SHUT_WR) == 0 && poll(&pfd, 1, 1000) == 1 && (pfd.revents & POLLRDHUP) &&
		    read(conn, buf, sizeof buf) == 0;
	CHECK("the end of a stream", ended, "revents %#x", pfd.revents);
	close(conn);
	CHECK("writing to a closed peer",
	      send(client, "x", 1, MSG_NOSIGNAL) < 0 && errno == EPIPE, "wrote");
	close(client);
	close(listener);
	client = socket(AF_UNIX, SOCK_STREAM, 0);
	CHECK("a closed listener refuses", connect(client, (struct sockaddr *)&a, alen) < 0 && errno == ECONNREFUSED,
	      "connected");
	CHECK("a name removed", unlink(path) == 0 && connect(client, (struct sockaddr *)&a, alen) < 0 &&
	      errno == ENOENT, "connected");
	close(client);

	/* Datagrams, from a socket with a name and one without. */
	int dg = socket(AF_UNIX, SOCK_DGRAM, 0), sender = socket(AF_UNIX, SOCK_DGRAM, 0);
	socklen_t dlen = unix_addr(&b, dpath);
	CHECK("a datagram socket", dg >= 0 && bind(dg, (struct sockaddr *)&b, dlen) == 0, "bind failed");
	CHECK("sendto", sendto(sender, "unnamed", 7, 0, (struct sockaddr *)&b, dlen) == 7, "failed");
	struct sockaddr_un from;
	len = sizeof from;
	ssize_t n = recvfrom(dg, buf, sizeof buf, 0, (struct sockaddr *)&from, &len);
	CHECK("a datagram from nobody", n == 7 && len == 0, "%zd bytes, address of %u", n, len);
	listener = socket(AF_UNIX, SOCK_STREAM, 0);
	bind(listener, (struct sockaddr *)&a, alen);
	listen(listener, 1);
	CHECK("a datagram to a stream socket",
	      sendto(sender, "x", 1, 0, (struct sockaddr *)&a, alen) < 0 && errno == EPROTOTYPE, "sent");
	close(listener);
	unlink(path);
	int named_sender = socket(AF_UNIX, SOCK_DGRAM, 0);
	bind(named_sender, (struct sockaddr *)&a, alen);
	sendto(named_sender, "a long datagram", 15, 0, (struct sockaddr *)&b, dlen);
	avail = 0;
	len = sizeof from;
	int sized = ioctl(dg, FIONREAD, &avail) == 0 && avail == 15;
	n = recvfrom(dg, buf, 4, MSG_TRUNC, (struct sockaddr *)&from, &len);
	CHECK("a datagram cut short", sized && n == 15 && !strcmp(from.sun_path, path), "%zd bytes from '%s', avail %d",
	      n, from.sun_path, avail);
	CHECK("connected datagrams", connect(named_sender, (struct sockaddr *)&b, dlen) == 0 &&
	      send(named_sender, "again", 5, 0) == 5 && recv(dg, buf, sizeof buf, 0) == 5, "failed");
	close(named_sender);
	close(sender);
	close(dg);
	unlink(path);
	unlink(dpath);

	/* Pairs of the other types: messages keep their bounds. */
	int sp[2];
	struct iovec iov = { buf, 3 };
	struct msghdr m = { .msg_iov = &iov, .msg_iovlen = 1 };
	int packets = socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sp) == 0 && write(sp[0], "first", 5) == 5 &&
		      write(sp[0], "second", 6) == 6 && recvmsg(sp[1], &m, 0) == 3 && (m.msg_flags & MSG_TRUNC) &&
		      read(sp[1], buf, sizeof buf) == 6;
	CHECK("seqpacket pairs", packets, "flags %#x", m.msg_flags);
	CHECK("a seqpacket connection's end", shutdown(sp[0], SHUT_WR) == 0 && read(sp[1], buf, sizeof buf) == 0,
	      "no end");
	close(sp[0]);
	close(sp[1]);
	int dgrams = socketpair(AF_UNIX, SOCK_DGRAM | SOCK_NONBLOCK, 0, sp) == 0 && write(sp[0], "a", 1) == 1 &&
		     write(sp[0], "bc", 2) == 2 && read(sp[1], buf, sizeof buf) == 1 && read(sp[1], buf, sizeof buf) == 2 &&
		     read(sp[1], buf, sizeof buf) < 0 && errno == EAGAIN;
	CHECK("datagram pairs", dgrams, "failed");
	close(sp[0]);
	close(sp[1]);

	/* Descriptors: a pipe's end sent over a stream, which the receiver
	   writes into; two sent where there is room for one; two messages'
	   descriptors not in one read. */
	int p[2], fds[8], got = 0, flags = 0;
	int sent = socketpair(AF_UNIX, SOCK_STREAM, 0, sp) == 0 && pipe(p) == 0 && send_fds(sp[0], 'P', &p[1], 1) == 0;
	n = recv_fds(sp[1], buf, sizeof buf, fds, 4, &got, &flags);
	int passed = sent && n == 1 && buf[0] == 'P' && got == 1 && fds[0] != p[1] && fcntl(fds[0], F_GETFD) == FD_CLOEXEC &&
		     write(fds[0], "through", 7) == 7 && read(p[0], buf, sizeof buf) == 7 && !memcmp(buf, "through", 7);
	CHECK("SCM_RIGHTS", passed, "%zd bytes, %d descriptors", n, got);
	if (got > 0)
		close(fds[0]);
	/* Room for one descriptor (CMSG_SPACE rounds it to a 64-bit word)
	   holds two, as on Linux: the third is closed. */
	int three[3] = { p[0], p[1], p[0] };
	n = send_fds(sp[0], 'T', three, 3) == 0 ? recv_fds(sp[1], buf, sizeof buf, fds, 1, &got, &flags) : -1;
	CHECK("descriptors that do not fit", n == 1 && got == 2 && (flags & MSG_CTRUNC), "%d descriptors, flags %#x",
	      got, flags);
	for (int i = 0; i < got; i++)
		close(fds[i]);
	int two = send_fds(sp[0], '1', &p[0], 1) == 0 && send_fds(sp[0], '2', &p[1], 1) == 0;
	n = recv_fds(sp[1], buf, sizeof buf, fds, 4, &got, &flags);
	int first = n == 1 && buf[0] == '1' && got == 1;
	if (got > 0)
		close(fds[0]);
	n = recv_fds(sp[1], buf, sizeof buf, fds, 4, &got, &flags);
	CHECK("a read takes one message's descriptors", two && first && n == 1 && buf[0] == '2' && got == 1,
	      "second read: %zd bytes, %d descriptors", n, got);
	if (got > 0)
		close(fds[0]);
	int one = 1;
	setsockopt(sp[1], SOL_SOCKET, SO_PASSCRED, &one, sizeof one);
	write(sp[0], "c", 1);
	union {
		struct cmsghdr h;
		char buf[CMSG_SPACE(sizeof(struct ucred))];
	} u;
	iov = (struct iovec){ buf, sizeof buf };
	m = (struct msghdr){ .msg_iov = &iov, .msg_iovlen = 1, .msg_control = u.buf, .msg_controllen = sizeof u.buf };
	struct cmsghdr *c = recvmsg(sp[1], &m, 0) == 1 ? CMSG_FIRSTHDR(&m) : 0;
	struct ucred *uc = c && c->cmsg_type == SCM_CREDENTIALS ? (struct ucred *)CMSG_DATA(c) : 0;
	CHECK("SCM_CREDENTIALS", uc && uc->pid == getpid() && uc->uid == getuid(), "%s", c ? "other data" : "none");
	close(sp[0]);
	close(sp[1]);

	/* Over datagrams, and to a child, which writes into the pipe end it
	   receives. */
	int dgram_fds = socketpair(AF_UNIX, SOCK_DGRAM, 0, sp) == 0 && send_fds(sp[0], 'D', &p[1], 1) == 0 &&
			recv_fds(sp[1], buf, sizeof buf, fds, 4, &got, &flags) == 1 && got == 1;
	CHECK("SCM_RIGHTS over datagrams", dgram_fds, "%d descriptors", got);
	if (got > 0)
		close(fds[0]);
	close(sp[0]);
	close(sp[1]);
	socketpair(AF_UNIX, SOCK_STREAM, 0, sp);
	posix_spawn_file_actions_t fa;
	posix_spawn_file_actions_init(&fa);
	posix_spawn_file_actions_adddup2(&fa, sp[1], 3);
	char *argv[] = { "posix", "child", "fds", 0 };
	pid_t pid;
	int r = posix_spawn(&pid, program, &fa, 0, argv, environ);
	posix_spawn_file_actions_destroy(&fa);
	close(sp[1]);
	int status = 0;
	int to_child = r == 0 && send_fds(sp[0], 'C', &p[1], 1) == 0 && read(sp[0], buf, 1) == 1 &&
		       waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0;
	close(p[1]);
	n = read(p[0], buf, sizeof buf);
	CHECK("SCM_RIGHTS to a child", to_child && n == 14 && !memcmp(buf, "from the child", 14), "status %#x, %zd bytes",
	      status, n);
	close(p[0]);
	close(sp[0]);
}

/* An abstract name's address (Linux's: a NUL, then the name's `n` bytes,
   as many as the length says), and its length. */
static socklen_t abstract_addr(struct sockaddr_un *a, const char *name, size_t n)
{
	memset(a, 0, sizeof *a);
	a->sun_family = AF_UNIX;
	memcpy(a->sun_path + 1, name, n);
	return offsetof(struct sockaddr_un, sun_path) + 1 + n;
}

/* Linux's abstract names: a listener and connections by name, from this
 * process and from a child; the name's length, NULs in names, a name taken
 * while its socket is open and free again once it is closed, apart for
 * each type; names chosen for a socket bound to the family alone, and for
 * one that says who sends (SO_PASSCRED). */
static void check_unix_abstract(void)
{
	char name[64], buf[64];
	int n = snprintf(name, sizeof name, "posix-%d", (int)getpid());
	struct sockaddr_un a, b, from;
	socklen_t alen = abstract_addr(&a, name, n), len = sizeof b;
	int listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	int bound = listener >= 0 && bind(listener, (struct sockaddr *)&a, alen) == 0 && listen(listener, 4) == 0 &&
		    getsockname(listener, (struct sockaddr *)&b, &len) == 0;
	CHECK("an abstract name", bound && len == alen && b.sun_path[0] == 0 && !memcmp(b.sun_path + 1, name, n),
	      "length %u", len);
	int other = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	CHECK("an abstract name taken", bind(other, (struct sockaddr *)&a, alen) < 0 && errno == EADDRINUSE, "bound");
	int dg = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
	CHECK("names apart for each type", bind(dg, (struct sockaddr *)&a, alen) == 0, "failed");
	close(dg);
	/* One NUL more is another name. */
	struct sockaddr_un longer;
	char with_nul[64];
	memcpy(with_nul, name, n);
	with_nul[n] = 0;
	socklen_t llen = abstract_addr(&longer, with_nul, n + 1);
	CHECK("a NUL in a name", bind(other, (struct sockaddr *)&longer, llen) == 0, "taken");
	CHECK("no file", access(name, F_OK) < 0, "a file is there");

	int client = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	len = sizeof b;
	int connected = connect(client, (struct sockaddr *)&a, alen) == 0 &&
			getpeername(client, (struct sockaddr *)&b, &len) == 0 && len == alen && b.sun_path[0] == 0;
	int conn = accept4(listener, 0, 0, SOCK_CLOEXEC);
	int talked = conn >= 0 && write(client, "hi", 2) == 2 && read(conn, buf, sizeof buf) == 2;
	CHECK("connecting to an abstract name", connected && talked, "length %u", len);
	close(conn);
	close(client);
	char *argv[] = { "posix", "child", "abstract", name, 0 };
	pid_t pid;
	int status = 0;
	int spawned = posix_spawn(&pid, program, 0, 0, argv, environ) == 0;
	conn = spawned ? accept4(listener, 0, 0, SOCK_CLOEXEC) : -1;
	ssize_t got = conn >= 0 ? read(conn, buf, sizeof buf) : -1;
	int child_ok = spawned && waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0;
	CHECK("an abstract name from a child", child_ok && got == 14 && !memcmp(buf, "from the child", 14),
	      "status %#x, %zd bytes", status, got);
	close(conn);
	/* Free again once its socket is closed. */
	close(listener);
	client = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	CHECK("a closed socket's name refuses",
	      connect(client, (struct sockaddr *)&a, alen) < 0 && errno == ECONNREFUSED, "connected");
	int again = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	CHECK("and is free again", bind(again, (struct sockaddr *)&a, alen) == 0, "taken");
	close(again);
	close(client);
	close(other);

	/* Names chosen: a NUL and five hex digits, which datagrams say. */
	struct sockaddr_un family = { .sun_family = AF_UNIX };
	int x = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0), y = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
	struct sockaddr_un xa;
	socklen_t xlen = sizeof xa;
	int chosen = bind(x, (struct sockaddr *)&family, sizeof(sa_family_t)) == 0 &&
		     getsockname(x, (struct sockaddr *)&xa, &xlen) == 0 &&
		     xlen == offsetof(struct sockaddr_un, sun_path) + 6 && xa.sun_path[0] == 0;
	for (int i = 1; chosen && i < 6; i++)
		chosen = xa.sun_path[i] && strchr("0123456789abcdef", xa.sun_path[i]);
	CHECK("a name chosen", chosen, "length %u", xlen);
	int y_named = bind(y, (struct sockaddr *)&family, sizeof(sa_family_t)) == 0;
	struct sockaddr_un ya;
	socklen_t ylen = sizeof ya;
	getsockname(y, (struct sockaddr *)&ya, &ylen);
	len = sizeof from;
	int replied = y_named && sendto(x, "ping", 4, 0, (struct sockaddr *)&ya, ylen) == 4 &&
		      recvfrom(y, buf, sizeof buf, 0, (struct sockaddr *)&from, &len) == 4 && len == xlen &&
		      !memcmp(&from, &xa, xlen) && sendto(y, "pong", 4, 0, (struct sockaddr *)&from, len) == 4 &&
		      recv(x, buf, sizeof buf, 0) == 4 && !memcmp(buf, "pong", 4);
	CHECK("a reply to a name chosen", replied, "from length %u", len);
	int one = 1, passer = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
	len = sizeof from;
	int passing = setsockopt(passer, SOL_SOCKET, SO_PASSCRED, &one, sizeof one) == 0 &&
		      sendto(passer, "who", 3, 0, (struct sockaddr *)&xa, xlen) == 3 &&
		      recvfrom(x, buf, sizeof buf, 0, (struct sockaddr *)&from, &len) == 3;
	CHECK("SO_PASSCRED names its socket", passing && len == xlen && from.sun_path[0] == 0, "from length %u", len);
	close(passer);
	close(x);
	close(y);
}

/* Starts this program as a child in `mode`, with `fd` as its descriptor 3;
   its process id, or -1. */
static pid_t spawn_with(int fd, const char *mode)
{
	posix_spawn_file_actions_t fa;
	posix_spawn_file_actions_init(&fa);
	posix_spawn_file_actions_adddup2(&fa, fd, 3);
	char *argv[] = { "posix", "child", (char *)mode, 0 };
	pid_t pid;
	int r = posix_spawn(&pid, program, &fa, 0, argv, environ);
	posix_spawn_file_actions_destroy(&fa);
	return r == 0 ? pid : -1;
}

/* Starts this program as a child in `mode`, with `fd` as its standard
   input and output (as inetd gives a connection); its process id, or -1. */
static pid_t spawn_stdio(int fd, const char *mode)
{
	posix_spawn_file_actions_t fa;
	posix_spawn_file_actions_init(&fa);
	posix_spawn_file_actions_adddup2(&fa, fd, 0);
	posix_spawn_file_actions_adddup2(&fa, fd, 1);
	char *argv[] = { "posix", "child", (char *)mode, 0 };
	pid_t pid;
	int r = posix_spawn(&pid, program, &fa, 0, argv, environ);
	posix_spawn_file_actions_destroy(&fa);
	return r == 0 ? pid : -1;
}

/* Reads `s` to its end into `buf` (room for `cap - 1` bytes, then a NUL):
   how many bytes. */
static ssize_t read_all(int s, char *buf, size_t cap)
{
	size_t got = 0;
	ssize_t n;
	while (got < cap - 1 && (n = read(s, buf + got, cap - 1 - got)) > 0)
		got += n;
	buf[got] = 0;
	return got;
}

/* Whether child `pid` exited with status 0. */
static int exited_well(pid_t pid)
{
	int status = 0;
	return pid > 0 && waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

/* Greets connection `c` with this process's id, and closes it. */
static int greet(int c)
{
	char buf[32];
	int n = snprintf(buf, sizeof buf, "accepted by %d", (int)getpid());
	int ok = c >= 0 && write(c, buf, n) == n;
	close(c);
	return ok;
}

/* Connects `s` to `to` and reads its greeting: the process id that
   accepted it, or -1. */
static long greeted(int s, const struct sockaddr *to, socklen_t len)
{
	char buf[32];
	ssize_t n = s >= 0 && connect(s, to, len) == 0 ? read(s, buf, sizeof buf - 1) : -1;
	close(s);
	if (n <= 12)
		return -1;
	buf[n] = 0;
	return strncmp(buf, "accepted by ", 12) ? -1 : atol(buf + 12);
}

/* Whether `by`, the processes that accepted two connections, are the two
   children `kids`, one each. */
static int one_each(const long *by, const pid_t *kids)
{
	return by[0] > 0 && by[1] > 0 && by[0] != by[1] && (by[0] == kids[0] || by[0] == kids[1]) &&
	       (by[1] == kids[0] || by[1] == kids[1]);
}

/* A TCP connection on the loopback interface: `*client` connected to
   `*server`'s end, which `listener` accepted; whether it was made. */
static int tcp_pair(int listener, const struct sockaddr_in *at, int *client, int *server)
{
	*client = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
	*server = *client >= 0 && connect(*client, (const struct sockaddr *)at, sizeof *at) == 0 ?
			  accept4(listener, 0, 0, SOCK_CLOEXEC) :
			  -1;
	return *server >= 0;
}

/* Connections and datagram sockets shared with other processes: a
 * connection given to a child as its standard input and output (as inetd
 * gives it), this process's copy closed at once; one that a child and this
 * process both write to, in turn; one sent to a child over a socket; and a
 * UDP socket that two children receive from, each datagram going to one of
 * them. */
static void check_shared_connections(void)
{
	struct sockaddr_in at = ipv4(htonl(INADDR_LOOPBACK), 0);
	socklen_t len = sizeof at;
	int listener = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
	int listening = listener >= 0 && bind(listener, (struct sockaddr *)&at, sizeof at) == 0 &&
			listen(listener, 8) == 0 && getsockname(listener, (struct sockaddr *)&at, &len) == 0;
	char buf[128];
	int client = -1, server = -1;

	int made = listening && tcp_pair(listener, &at, &client, &server);
	pid_t kid = made ? spawn_stdio(server, "upper") : -1;
	close(server);
	ssize_t n = kid > 0 && write(client, "hello, child\n", 13) == 13 && shutdown(client, SHUT_WR) == 0 ?
			    read_all(client, buf, sizeof buf) :
			    -1;
	CHECK("a connection as a child's standard input and output",
	      exited_well(kid) && n == 13 && !strcmp(buf, "HELLO, CHILD\n"), "%zd bytes: '%s'", n, n > 0 ? buf : "");
	close(client);

	made = listening && tcp_pair(listener, &at, &client, &server);
	kid = made ? spawn_with(server, "say") : -1;
	int said = exited_well(kid) && write(server, "and the parent\n", 15) == 15;
	close(server);
	n = said ? read_all(client, buf, sizeof buf) : -1;
	CHECK("a connection a child and this process write to", n == 30 && !strcmp(buf, "from the child\nand the parent\n"),
	      "%zd bytes: '%s'", n, n > 0 ? buf : "");
	close(client);

	int sp[2];
	made = listening && tcp_pair(listener, &at, &client, &server) &&
	       socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sp) == 0;
	kid = made ? spawn_with(sp[1], "upper-sent") : -1;
	int sent = kid > 0 && send_fds(sp[0], 'C', &server, 1) == 0;
	close(server);
	n = sent && write(client, "sent over\n", 10) == 10 && shutdown(client, SHUT_WR) == 0 ?
		    read_all(client, buf, sizeof buf) :
		    -1;
	CHECK("a connection sent to a child", exited_well(kid) && n == 10 && !strcmp(buf, "SENT OVER\n"),
	      "%zd bytes: '%s'", n, n > 0 ? buf : "");
	close(client);
	if (made) {
		close(sp[0]);
		close(sp[1]);
	}
	close(listener);

	/* Two children receive from one UDP socket: a datagram each, which
	   they answer with their process ids. */
	struct sockaddr_in u = ipv4(htonl(INADDR_LOOPBACK), 0), me = u;
	len = sizeof u;
	int shared = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0), asker = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
	int bound = shared >= 0 && bind(shared, (struct sockaddr *)&u, sizeof u) == 0 &&
		    getsockname(shared, (struct sockaddr *)&u, &len) == 0 &&
		    bind(asker, (struct sockaddr *)&me, sizeof me) == 0;
	/* A datagram lost fails the check rather than holding it up for
	   ever; the children find the time-out set as it was. */
	struct timeval wait = { 10, 0 };
	setsockopt(asker, SOL_SOCKET, SO_RCVTIMEO, &wait, sizeof wait);
	setsockopt(shared, SOL_SOCKET, SO_RCVTIMEO, &wait, sizeof wait);
	pid_t kids[2] = { bound ? spawn_with(shared, "udp-reply") : -1, bound ? spawn_with(shared, "udp-reply") : -1 };
	close(shared);
	long by[2] = { -1, -1 };
	for (int i = 0; i < 2 && kids[0] > 0 && kids[1] > 0; i++) {
		n = sendto(asker, "who?", 4, 0, (struct sockaddr *)&u, sizeof u) == 4 ? recv(asker, buf, sizeof buf - 1, 0) : -1;
		buf[n > 0 ? n : 0] = 0;
		by[i] = n > 12 && !strncmp(buf, "answered by ", 12) ? atol(buf + 12) : -1;
	}
	int exited = exited_well(kids[0]) & exited_well(kids[1]);
	CHECK("a UDP socket two children receive from", bound && exited && one_each(by, kids),
	      "answered by %ld and %ld; children %d and %d", by[0], by[1], (int)kids[0], (int)kids[1]);
	close(asker);
}

/* Sockets shared with other processes, as pre-forking servers share them:
 * a Unix domain listener and a TCP one (which the children find
 * non-blocking, as it was given), from which the children this process
 * starts accept, each connection going to one of them, and which stay open
 * until the last of them closes; a listener sent over a socket; and the
 * other kinds of Unix domain sockets, which go to a child as well. */
static void check_shared_sockets(void)
{
	char path[64];
	snprintf(path, sizeof path, "/tmp/posix-shared-%d", (int)getpid());
	unlink(path);
	struct sockaddr_un a;
	socklen_t alen = unix_addr(&a, path);
	int listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	int listening = listener >= 0 && bind(listener, (struct sockaddr *)&a, alen) == 0 && listen(listener, 8) == 0;
	pid_t kids[2] = { spawn_with(listener, "accept"), spawn_with(listener, "accept") };
	long by[2];
	for (int i = 0; i < 2; i++)
		by[i] = greeted(socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0), (struct sockaddr *)&a, alen);
	int exited = exited_well(kids[0]) & exited_well(kids[1]);
	CHECK("a Unix domain listener shared with children", listening && exited && one_each(by, kids),
	      "accepted by %ld and %ld; children %d and %d", by[0], by[1], (int)kids[0], (int)kids[1]);
	/* Still this process's once they are gone; closed once it closes it. */
	int c = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	int conn = c >= 0 && connect(c, (struct sockaddr *)&a, alen) == 0 ? accept4(listener, 0, 0, SOCK_CLOEXEC) : -1;
	CHECK("the listener's own process accepts too", conn >= 0, "errno %d", errno);
	close(conn);
	close(c);
	close(listener);
	c = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	CHECK("closed once every process closed it", connect(c, (struct sockaddr *)&a, alen) < 0 && errno == ECONNREFUSED,
	      "connected");
	close(c);
	unlink(path);

	/* A TCP listener, non-blocking: the children poll it, and one of them
	   finds each connection taken by the other first. */
	struct sockaddr_in t = ipv4(htonl(INADDR_LOOPBACK), 0);
	socklen_t tlen = sizeof t;
	int tcp = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
	listening = tcp >= 0 && bind(tcp, (struct sockaddr *)&t, sizeof t) == 0 && listen(tcp, 8) == 0 &&
		    getsockname(tcp, (struct sockaddr *)&t, &tlen) == 0;
	kids[0] = spawn_with(tcp, "accept-polling");
	kids[1] = spawn_with(tcp, "accept-polling");
	for (int i = 0; i < 2; i++)
		by[i] = greeted(socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0), (struct sockaddr *)&t, sizeof t);
	exited = exited_well(kids[0]) & exited_well(kids[1]);
	CHECK("a TCP listener shared with children", listening && exited && one_each(by, kids),
	      "accepted by %ld and %ld; children %d and %d", by[0], by[1], (int)kids[0], (int)kids[1]);

	/* Sent over a socket to a child, which accepts from it. */
	int sp[2];
	int paired = socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sp) == 0;
	pid_t kid = paired ? spawn_with(sp[1], "accept-sent") : -1;
	long got = kid > 0 && send_fds(sp[0], 'L', &tcp, 1) == 0 ?
			   greeted(socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0), (struct sockaddr *)&t, sizeof t) :
			   -1;
	CHECK("a listener sent to a child", exited_well(kid) && got == kid, "accepted by %ld, child %d", got, (int)kid);
	close(sp[0]);
	close(sp[1]);
	close(tcp);

	/* A seqpacket socket pair's end: the child sends back what comes. */
	char buf[64];
	paired = socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sp) == 0;
	kid = paired ? spawn_with(sp[1], "echo-packet") : -1;
	ssize_t n = kid > 0 && send(sp[0], "ping", 4, 0) == 4 ? recv(sp[0], buf, sizeof buf, 0) : -1;
	CHECK("a seqpacket socket to a child", exited_well(kid) && n == 4 && !memcmp(buf, "ping", 4), "%zd bytes", n);
	close(sp[0]);
	close(sp[1]);

	/* A datagram socket bound to a path: the child receives, and replies
	   from it. */
	snprintf(path, sizeof path, "/tmp/posix-dgram-%d", (int)getpid());
	unlink(path);
	alen = unix_addr(&a, path);
	int d = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0), me = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
	struct sockaddr_un mine = { .sun_family = AF_UNIX }, from;
	socklen_t len = sizeof from;
	int bound = d >= 0 && bind(d, (struct sockaddr *)&a, alen) == 0 &&
		    bind(me, (struct sockaddr *)&mine, sizeof(sa_family_t)) == 0;
	kid = bound ? spawn_with(d, "reply-datagram") : -1;
	close(d);
	n = kid > 0 && sendto(me, "ping", 4, 0, (struct sockaddr *)&a, alen) == 4 ?
		    recvfrom(me, buf, sizeof buf, 0, (struct sockaddr *)&from, &len) :
		    -1;
	CHECK("a bound datagram socket to a child",
	      exited_well(kid) && n == 4 && !memcmp(buf, "ping", 4) && len == alen && !memcmp(&from, &a, alen),
	      "%zd bytes from length %u", n, len);
	close(me);
	unlink(path);
}

/* The order of two names of NAME_MAX + 1 bytes, for qsort. */
static int compare_names(const void *a, const void *b)
{
	return strcmp(a, b);
}

/* The names a listing of directory DIR shows, in order, joined by spaces,
   into OUT; how many, or -1 if it cannot be listed. */
static int listing(const char *dir, char *out, size_t cap)
{
	DIR *d = opendir(dir);
	if (!d)
		return -1;
	char names[64][256];
	int n = 0;
	struct dirent *e;
	while ((e = readdir(d)) && n < 64) {
		if (strcmp(e->d_name, ".") && strcmp(e->d_name, ".."))
			snprintf(names[n++], sizeof names[0], "%s", e->d_name);
	}
	closedir(d);
	qsort(names, n, sizeof names[0], compare_names);
	out[0] = 0;
	for (int i = 0; i < n; i++)
		snprintf(out + strlen(out), cap - strlen(out), "%s%s", i ? " " : "", names[i]);
	return n;
}

/* Calls the code at CODE, a function that returns an int. */
static int run_code(void *code)
{
	int (*f)(void);
	memcpy(&f, &code, sizeof f);
	return f();
}

/* `posix child sandboxed-child`, started by a sandboxed program: what it
   inherits of the sandbox (its registry channel is cloned from the
   confined one). */
static int sandboxed_child(void)
{
	veda_handle_t ch;
	int r = veda_service_connect("display", &ch);
	CHECK("sandbox: its child reaches the service its parent kept", r == 0, "connect returned %d", r);
	if (r == 0)
		veda_close(ch);
	r = veda_service_connect("net", &ch);
	CHECK("sandbox: and no other", r < 0, "connected to the network service");
	errno = 0;
	CHECK("sandbox: nor the file system", access("/system", F_OK) != 0, "it reaches /system");
	return failures;
}

/* `posix child sandbox`: gives up, as the browser's page processes do, all
   of the file system but the system image (to read), the devices (to read;
   /dev/null to write) and a directory of its own (`kept`, in the parent's
   scratch directory, to write); every service but one (the window
   system's); and processes, the kernel log and new executable memory, but
   for a page made executable before, as a JIT's pool is. Then it tries
   each thing it gave up, and what it kept. */
static int sandboxed(void)
{
	char text[1024], dir[1024];
	if (!getcwd(dir, sizeof dir))
		return 99;
	CHECK("sandbox: the user's files are there before", access("/home/user", F_OK) == 0, "no /home/user");
	mkdir("kept", 0755);
	int fd = open("kept/before", O_WRONLY | O_CREAT | O_TRUNC, 0644);
	CHECK("sandbox: a file of its own", fd >= 0 && write(fd, "kept", 4) == 4, "cannot write kept/before");
	close(fd);
	fd = open("outside", O_WRONLY | O_CREAT | O_TRUNC, 0644);
	close(fd);
	unsigned char forty_two[] = VEDA_CODE_RETURNING(42), forty_three[] = VEDA_CODE_RETURNING(43);
	unsigned char *pool = mmap(0, VEDA_PAGE_SIZE, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	CHECK("sandbox: an executable page, before", pool != MAP_FAILED, "mmap failed");
	if (pool == MAP_FAILED)
		return failures;
	memcpy(pool, forty_two, sizeof forty_two);
	CHECK("sandbox: code in it runs", run_code(pool) == 42, "it returned something else");

	/* The files. */
	struct veda_place places[] = { { "/system", 0 }, { "/dev", 0 }, { "/dev/null", 1 }, { "kept", 1 } };
	int r = veda_confine_files(places, 4);
	CHECK("sandbox: confining its files", r == 0, "returned %d", r);
	errno = 0;
	CHECK("sandbox: the user's files are not there", access("/home/user", F_OK) != 0 && errno == ENOENT,
	      "/home/user is there");
	errno = 0;
	CHECK("sandbox: nor anything outside its places", open("outside", O_RDONLY) < 0 && errno == ENOENT,
	      "it opened ./outside");
	errno = 0;
	CHECK("sandbox: nor can it be made", mkdir("/tmp/escape", 0755) != 0 && errno == ENOENT, "made /tmp/escape");
	CHECK("sandbox: the root lists the way to its places", listing("/", text, sizeof text) == 3 &&
	      !strcmp(text, "dev system tmp"), "it lists '%s'", text);
	listing(dir, text, sizeof text);
	CHECK("sandbox: its directory lists its place alone", !strcmp(text, "kept"), "it lists '%s'", text);
	struct stat st;
	CHECK("sandbox: a directory on the way counts what it shows", stat(dir, &st) == 0 && st.st_size == 1,
	      "size %ld", (long)st.st_size);
	fd = open("/system/tests/c/posix", O_RDONLY);
	char magic[4] = { 0 };
	CHECK("sandbox: the system image is read", fd >= 0 && read(fd, magic, 4) == 4 && !memcmp(magic, "\177ELF", 4),
	      "cannot read its program");
	close(fd);
	CHECK("sandbox: what it only reads says so", stat("/dev/zero", &st) == 0 && !(st.st_mode & 0222),
	      "mode %o", st.st_mode);
	errno = 0;
	CHECK("sandbox: nor is it changed", open("/dev/zero", O_WRONLY) < 0 && errno == EACCES, "opened /dev/zero to write");
	fd = open("/dev/null", O_WRONLY);
	CHECK("sandbox: what it may change is changed", fd >= 0 && write(fd, "x", 1) == 1, "cannot write /dev/null");
	close(fd);
	fd = open("kept/before", O_RDONLY);
	CHECK("sandbox: its own files, made before", fd >= 0 && read(fd, magic, 4) == 4 && !memcmp(magic, "kept", 4),
	      "cannot read kept/before");
	close(fd);
	fd = open("kept/after", O_WRONLY | O_CREAT, 0644);
	CHECK("sandbox: and made after", fd >= 0 && write(fd, "x", 1) == 1, "cannot write kept/after");
	/* As a database commits: what it changed is saved as anything is. */
	CHECK("sandbox: and synced", fsync(fd) == 0, "fsync failed: %s", strerror(errno));
	close(fd);
	errno = 0;
	CHECK("sandbox: but not moved out", rename("kept/after", "moved") != 0 && errno == ENOENT, "moved it out");
	int u = socket(AF_UNIX, SOCK_STREAM, 0);
	struct sockaddr_un a;
	socklen_t len = abstract_addr(&a, "sandboxed", 9);
	errno = 0;
	CHECK("sandbox: no abstract socket names", bind(u, (struct sockaddr *)&a, len) != 0 && errno == EACCES,
	      "bound one");
	close(u);
	struct veda_place wider[] = { { "/", 0 } }, narrower[] = { { "/system", 0 }, { "/dev/null", 1 }, { "kept", 1 } };
	CHECK("sandbox: no wider again", veda_confine_files(wider, 1) == EACCES, "it widened them");
	CHECK("sandbox: narrower", veda_confine_files(narrower, 3) == 0, "it cannot narrow them");
	errno = 0;
	CHECK("sandbox: and narrower it is", access("/dev/zero", F_OK) != 0 && errno == ENOENT, "/dev/zero is there");

	/* The services. */
	const char *display[] = { "display" }, *more[] = { "display", "net" };
	r = veda_confine_services(display, 1);
	CHECK("sandbox: confining its services", r == 0, "returned %d", r);
	veda_handle_t ch;
	r = veda_service_connect("display", &ch);
	CHECK("sandbox: the service it kept", r == 0, "connect returned %d", r);
	if (r == 0)
		veda_close(ch);
	CHECK("sandbox: not the network's", veda_service_connect("net", &ch) < 0, "connected");
	CHECK("sandbox: nor the file system's, past its places", veda_service_connect("vfs", &ch) < 0, "connected");
	CHECK("sandbox: nor the launcher", veda_service_connect("launcher", &ch) < 0, "connected");
	r = veda_confine_services(more, 2);
	CHECK("sandbox: no more services again", r == 0 && veda_service_connect("net", &ch) < 0, "reaches the network");
	int s = socket(AF_INET, SOCK_STREAM, 0);
	struct sockaddr_in to = ipv4(htonl(INADDR_LOOPBACK), 80);
	errno = 0;
	CHECK("sandbox: no TCP connection", connect(s, (struct sockaddr *)&to, sizeof to) != 0 && errno == EACCES,
	      "connected");
	close(s);
	s = socket(AF_INET, SOCK_DGRAM, 0);
	errno = 0;
	CHECK("sandbox: no UDP", connect(s, (struct sockaddr *)&to, sizeof to) != 0 && errno == EACCES, "connected");
	close(s);
	char out[2048];
	int status = 0;
	r = spawn_child("sandboxed-child", out, sizeof out, &status);
	fputs(out, stdout);
	CHECK("sandbox: its child is confined alike", r == 0 && WIFEXITED(status) && WEXITSTATUS(status) == 0,
	      "status %#x", status);

	/* The kernel. */
	CHECK("sandbox: what the kernel does not know", veda_process_restrict(1u << 20) == EINVAL, "accepted");
	r = veda_process_restrict(VEDA_RESTRICT_LOG | VEDA_RESTRICT_PROCESSES | VEDA_RESTRICT_EXECUTE);
	CHECK("sandbox: giving up processes, the log and new executable memory", r == 0, "returned %d", r);
	pid_t pid;
	char *argv[] = { "posix", "child", "echo", 0 };
	r = posix_spawn(&pid, program, 0, 0, argv, environ);
	CHECK("sandbox: no process", r == EACCES, "posix_spawn returned %d", r);
	errno = 0;
	void *more_code = mmap(0, VEDA_PAGE_SIZE, PROT_READ | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	CHECK("sandbox: no new executable memory", more_code == MAP_FAILED && errno == EACCES, "mapped it");
	void *data = mmap(0, VEDA_PAGE_SIZE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	errno = 0;
	CHECK("sandbox: nor memory made executable", data != MAP_FAILED &&
	      mprotect(data, VEDA_PAGE_SIZE, PROT_READ | PROT_EXEC) != 0 && errno == EACCES, "made it executable");
	memcpy(pool, forty_three, sizeof forty_three);
	CHECK("sandbox: the page made before takes new code, which runs", run_code(pool) == 43, "it does not");
	return failures;
}

/* A sandbox (<veda/sandbox.h>), as the browser's page processes have
   theirs, in a child that gives up what it does not need and tries it. */
static void check_sandbox(void)
{
	char out[16384];
	int status = 0;
	int r = spawn_child("sandbox", out, sizeof out, &status);
	fputs(out, stdout);
	CHECK("a sandboxed child is refused what it gave up, and keeps the rest",
	      r == 0 && WIFEXITED(status) && WEXITSTATUS(status) == 0, "status %#x", status);
}

/* Writes TEXT to the file PATH: whether it did. */
static int put(const char *path, const char *text)
{
	int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
	int ok = fd >= 0 && write(fd, text, strlen(text)) == (ssize_t)strlen(text);
	if (fd >= 0)
		close(fd);
	return ok;
}

/* `posix child borrower LOAN`: confined to the system image and the
   devices, it takes LOAN (hexadecimal), of `lent` in the parent's scratch
   directory, as the browser's network process takes the folder of a page
   the user opens; it tries what the loan lends, and what it does not. */
static int borrower(const char *hex)
{
	unsigned char loan[VEDA_LOAN_SIZE], wrong[VEDA_LOAN_SIZE], mine[VEDA_LOAN_SIZE];
	char dir[1024], text[1024];
	if (!hex || strlen(hex) != 2 * VEDA_LOAN_SIZE || !getcwd(dir, sizeof dir))
		return 99;
	for (int i = 0; i < VEDA_LOAN_SIZE; i++)
		sscanf(hex + 2 * i, "%2hhx", &loan[i]);
	struct veda_place places[] = { { "/system", 0 }, { "/dev", 0 } };
	int r = veda_confine_files(places, 2);
	CHECK("loan: confining its files", r == 0, "returned %d", r);
	errno = 0;
	CHECK("loan: what is lent is not there before", access("lent/page.html", F_OK) != 0 && errno == ENOENT,
	      "it is there");
	memcpy(wrong, loan, sizeof wrong);
	wrong[VEDA_LOAN_SIZE - 1] ^= 1;
	CHECK("loan: another secret takes nothing", veda_borrow_files("lent", wrong) == EACCES, "it took something");
	CHECK("loan: nor does the loan, for another place",
	      veda_borrow_files("beside", loan) == EACCES && veda_borrow_files("lent/inside", loan) == EACCES &&
		      veda_borrow_files(dir, loan) == EACCES, "it took another place");
	CHECK("loan: a sandboxed program lends nothing", veda_lend_files("/system", mine) == EACCES, "it lent");
	r = veda_borrow_files("lent", loan);
	CHECK("loan: taken", r == 0, "returned %d", r);
	int fd = open("lent/page.html", O_RDONLY);
	memset(text, 0, sizeof text);
	CHECK("loan: what was lent is read", fd >= 0 && read(fd, text, sizeof text - 1) == 4 && !strcmp(text, "page"),
	      "read '%s'", text);
	close(fd);
	fd = open("lent/inside/picture", O_RDONLY);
	CHECK("loan: and what is inside it", fd >= 0, "cannot open lent/inside/picture");
	close(fd);
	errno = 0;
	CHECK("loan: but not changed", open("lent/page.html", O_WRONLY) < 0 && errno == EACCES, "opened it to write");
	errno = 0;
	CHECK("loan: nor added to", open("lent/new", O_WRONLY | O_CREAT, 0644) < 0 && errno == EACCES, "made lent/new");
	errno = 0;
	CHECK("loan: what is beside it is not there", access("beside", F_OK) != 0 && errno == ENOENT, "it is there");
	CHECK("loan: its directory lists it alone", listing(dir, text, sizeof text) == 1 && !strcmp(text, "lent"),
	      "it lists '%s'", text);
	CHECK("loan: taken once", veda_borrow_files("lent", loan) == EACCES, "taken again");
	return failures;
}

/* `posix child borrower-again LOANS`: confined as `borrower` is, it takes
   the first of LOANS (two, hexadecimal), of `lent`, and opens what is
   there; it says so on descriptor 3 and waits on its standard input while
   the parent takes the loan back, as the browser takes back the folder of
   a page no longer open. Then the place is gone, but for the file it has
   open; nor does the second loan, which the parent took back before it was
   taken, take anything. */
static int borrower_again(const char *hex)
{
	unsigned char loan[VEDA_LOAN_SIZE], early[VEDA_LOAN_SIZE];
	char dir[1024], text[1024];
	if (!hex || strlen(hex) != 4 * VEDA_LOAN_SIZE || !getcwd(dir, sizeof dir))
		return 99;
	for (int i = 0; i < VEDA_LOAN_SIZE; i++) {
		sscanf(hex + 2 * i, "%2hhx", &loan[i]);
		sscanf(hex + 2 * (VEDA_LOAN_SIZE + i), "%2hhx", &early[i]);
	}
	struct veda_place places[] = { { "/system", 0 }, { "/dev", 0 } };
	int r = veda_confine_files(places, 2);
	CHECK("taking back: confining its files", r == 0, "returned %d", r);
	CHECK("taking back: a loan taken back before it is taken takes nothing",
	      veda_borrow_files("lent", early) == EACCES, "it took it");
	r = veda_borrow_files("lent", loan);
	CHECK("taking back: the loan taken", r == 0, "returned %d", r);
	int fd = open("lent/page.html", O_RDONLY);
	CHECK("taking back: what was lent is opened", fd >= 0, "cannot open lent/page.html");
	char c = 't';
	if (write(3, &c, 1) != 1 || read(0, &c, 1) != 1)
		return 98;
	errno = 0;
	CHECK("taking back: what was lent is not there", access("lent/page.html", F_OK) != 0 && errno == ENOENT,
	      "it is there");
	errno = 0;
	CHECK("taking back: nor what is inside it", open("lent/inside/picture", O_RDONLY) < 0 && errno == ENOENT,
	      "opened it");
	errno = 0;
	CHECK("taking back: nor the way to it", access(dir, F_OK) != 0 && errno == ENOENT, "it is there");
	memset(text, 0, sizeof text);
	CHECK("taking back: what it has open stays open",
	      fd >= 0 && read(fd, text, sizeof text - 1) == 4 && !strcmp(text, "page"), "read '%s'", text);
	close(fd);
	CHECK("taking back: nor is the loan taken again", veda_borrow_files("lent", loan) == EACCES, "taken again");
	CHECK("taking back: the system image stays", access("/system", F_OK) == 0, "it is gone");
	return failures;
}

/* Loans (<veda/sandbox.h>): a place lent to a sandboxed child, and nothing
   more, as the browser lends its network process the folder of a page the
   user opens. */
static void check_loans(void)
{
	mkdir("lent", 0755);
	mkdir("lent/inside", 0755);
	CHECK("loan: files to lend", put("lent/page.html", "page") && put("lent/inside/picture", "picture") &&
	      put("beside", "beside"), "cannot write them");
	unsigned char loan[VEDA_LOAN_SIZE], other[VEDA_LOAN_SIZE];
	CHECK("loan: of nothing", veda_lend_files("not-there", other) == ENOENT, "it lent it");
	CHECK("loan: never of the home folder, nor what holds it",
	      veda_lend_files("/home/user", other) == EACCES && veda_lend_files("/home", other) == EACCES &&
		      veda_lend_files("/", other) == EACCES, "it lent one");
	int r = veda_lend_files("lent", loan);
	CHECK("loan: made", r == 0, "returned %d", r);
	r = veda_lend_files("lent", other);
	CHECK("loan: each a secret of its own", r == 0 && memcmp(loan, other, sizeof loan), "the same twice");
	char hex[2 * VEDA_LOAN_SIZE + 1], out[8192];
	for (int i = 0; i < VEDA_LOAN_SIZE; i++)
		snprintf(hex + 2 * i, 3, "%02x", loan[i]);
	int status = 0;
	r = spawn_child_with("borrower", hex, out, sizeof out, &status);
	fputs(out, stdout);
	CHECK("a sandboxed child reads the place it was lent, and nothing more",
	      r == 0 && WIFEXITED(status) && WEXITSTATUS(status) == 0, "status %#x", status);
}

/* Taking a loan back (<veda/sandbox.h>): from a sandboxed child that took
   it and has a file open there (it says so on descriptor 3, and waits on
   its standard input), as the browser takes back the folder of a page no
   longer open; and one not taken yet. */
static void check_taking_back(void)
{
	unsigned char loan[VEDA_LOAN_SIZE], early[VEDA_LOAN_SIZE], never[VEDA_LOAN_SIZE];
	memset(never, 0x5a, sizeof never);
	int r = veda_lend_files("lent", loan);
	CHECK("taking back: a loan made", r == 0, "returned %d", r);
	r = veda_lend_files("lent", early);
	CHECK("taking back: one not taken yet", r == 0 && veda_take_back_files(early) == 0, "returned %d", r);
	CHECK("taking back: nothing, of a loan never made", veda_take_back_files(never) == 0, "it failed");
	char hex[4 * VEDA_LOAN_SIZE + 1], out[8192];
	for (int i = 0; i < VEDA_LOAN_SIZE; i++)
		snprintf(hex + 2 * i, 3, "%02x", loan[i]);
	for (int i = 0; i < VEDA_LOAN_SIZE; i++)
		snprintf(hex + 2 * (VEDA_LOAN_SIZE + i), 3, "%02x", early[i]);
	int said[2], go[2], text[2];
	if (pipe(said) || pipe(go) || pipe(text)) {
		CHECK("taking back: pipes to the child", 0, "%s", strerror(errno));
		return;
	}
	posix_spawn_file_actions_t fa;
	posix_spawn_file_actions_init(&fa);
	posix_spawn_file_actions_addclose(&fa, said[0]);
	posix_spawn_file_actions_addclose(&fa, go[1]);
	posix_spawn_file_actions_addclose(&fa, text[0]);
	posix_spawn_file_actions_adddup2(&fa, text[1], 1);
	posix_spawn_file_actions_adddup2(&fa, said[1], 3);
	posix_spawn_file_actions_adddup2(&fa, go[0], 0);
	char *argv[] = { "posix", "child", "borrower-again", hex, 0 };
	pid_t pid = 0;
	int spawned = posix_spawn(&pid, program, &fa, 0, argv, environ);
	posix_spawn_file_actions_destroy(&fa);
	close(said[1]);
	close(go[0]);
	close(text[1]);
	char c = 0;
	int took = !spawned && read(said[0], &c, 1) == 1 && c == 't';
	CHECK("taking back: the child took the loan", took, "posix_spawn returned %d", spawned);
	r = veda_take_back_files(loan);
	CHECK("taking back: taken back", r == 0, "returned %d", r);
	CHECK("taking back: again, nothing", veda_take_back_files(loan) == 0, "it failed");
	if (took && write(go[1], "g", 1) != 1)
		CHECK("taking back: the child goes on", 0, "%s", strerror(errno));
	close(go[1]);
	close(said[0]);
	if (!spawned) {
		read_all(text[0], out, sizeof out);
		fputs(out, stdout);
	}
	close(text[0]);
	CHECK("a sandboxed child reaches a place taken back no more", !spawned && exited_well(pid),
	      "it did not exit well");
}

static int child(const char *mode, const char *arg)
{
	if (!strcmp(mode, "sandbox"))
		return sandboxed();
	if (!strcmp(mode, "borrower"))
		return borrower(arg);
	if (!strcmp(mode, "borrower-again"))
		return borrower_again(arg);
	if (!strcmp(mode, "sandboxed-child"))
		return sandboxed_child();
	if (!strcmp(mode, "accept")) {
		/* Descriptor 3 listens: one connection, greeted. */
		return greet(accept4(3, 0, 0, SOCK_CLOEXEC)) ? 0 : 1;
	}
	if (!strcmp(mode, "accept-polling")) {
		/* Descriptor 3 listens without waiting, as it was given: a
		   connection another process took first is waited for again. */
		if (!(fcntl(3, F_GETFL) & O_NONBLOCK))
			return 1;
		for (;;) {
			struct pollfd p = { .fd = 3, .events = POLLIN };
			if (poll(&p, 1, 20000) != 1)
				return 2;
			int c = accept4(3, 0, 0, SOCK_CLOEXEC);
			if (c >= 0)
				return greet(c) ? 0 : 3;
			if (errno != EAGAIN)
				return 4;
		}
	}
	if (!strcmp(mode, "accept-sent")) {
		/* A listener comes over the stream socket of descriptor 3. */
		char byte;
		int fd = -1, got = 0, flags = 0;
		if (recv_fds(3, &byte, 1, &fd, 1, &got, &flags) != 1 || got != 1)
			return 1;
		/* It came non-blocking: wait for the connection. */
		struct pollfd p = { .fd = fd, .events = POLLIN };
		return poll(&p, 1, 20000) == 1 && greet(accept4(fd, 0, 0, SOCK_CLOEXEC)) ? 0 : 2;
	}
	if (!strcmp(mode, "upper")) {
		/* Its standard input and output are a connection: what comes,
		   in capitals, until it ends. */
		char buf[64];
		ssize_t n;
		while ((n = read(0, buf, sizeof buf)) > 0) {
			for (ssize_t i = 0; i < n; i++)
				buf[i] = buf[i] >= 'a' && buf[i] <= 'z' ? buf[i] - 'a' + 'A' : buf[i];
			if (write(1, buf, n) != n)
				return 1;
		}
		return n == 0 ? 0 : 2;
	}
	if (!strcmp(mode, "say")) {
		/* Descriptor 3 is a connection: the child's line on it. */
		return write(3, "from the child\n", 15) == 15 ? 0 : 1;
	}
	if (!strcmp(mode, "upper-sent")) {
		/* A connection comes over the stream socket of descriptor 3:
		   what comes on it, in capitals, until it ends. */
		char byte, buf[64];
		int fd = -1, got = 0, flags = 0;
		if (recv_fds(3, &byte, 1, &fd, 1, &got, &flags) != 1 || got != 1)
			return 1;
		ssize_t n;
		while ((n = read(fd, buf, sizeof buf)) > 0) {
			for (ssize_t i = 0; i < n; i++)
				buf[i] = buf[i] >= 'a' && buf[i] <= 'z' ? buf[i] - 'a' + 'A' : buf[i];
			if (write(fd, buf, n) != n)
				return 2;
		}
		return n == 0 ? 0 : 3;
	}
	if (!strcmp(mode, "udp-reply")) {
		/* Descriptor 3 is a UDP socket: one datagram, answered with
		   this process's id. */
		char buf[64];
		struct sockaddr_in from;
		socklen_t len = sizeof from;
		if (recvfrom(3, buf, sizeof buf, 0, (struct sockaddr *)&from, &len) < 0)
			return 1;
		int n = snprintf(buf, sizeof buf, "answered by %d", (int)getpid());
		return sendto(3, buf, n, 0, (struct sockaddr *)&from, len) == n ? 0 : 2;
	}
	if (!strcmp(mode, "echo-packet")) {
		char buf[64];
		ssize_t n = recv(3, buf, sizeof buf, 0);
		return n > 0 && send(3, buf, n, 0) == n ? 0 : 1;
	}
	if (!strcmp(mode, "reply-datagram")) {
		char buf[64];
		struct sockaddr_un from;
		socklen_t len = sizeof from;
		ssize_t n = recvfrom(3, buf, sizeof buf, 0, (struct sockaddr *)&from, &len);
		return n > 0 && sendto(3, buf, n, 0, (struct sockaddr *)&from, len) == n ? 0 : 1;
	}
	if (!strcmp(mode, "abstract") && arg) {
		/* The parent listens on the abstract name `arg`. */
		struct sockaddr_un a;
		socklen_t len = abstract_addr(&a, arg, strlen(arg));
		int s = socket(AF_UNIX, SOCK_STREAM, 0);
		if (s < 0 || connect(s, (struct sockaddr *)&a, len) != 0)
			return 1;
		return write(s, "from the child", 14) == 14 ? 0 : 2;
	}
	if (!strcmp(mode, "fds")) {
		/* Descriptor 3 is a stream socket: a pipe's end comes over it,
		   and the child writes into it, then says so. */
		char byte;
		int fd = -1, got = 0, flags = 0;
		if (recv_fds(3, &byte, 1, &fd, 1, &got, &flags) != 1 || got != 1)
			return 1;
		if (write(fd, "from the child", 14) != 14)
			return 2;
		close(fd);
		return write(3, "!", 1) == 1 ? 0 : 3;
	}
	if (!strcmp(mode, "echo")) {
		printf("child says hello %s\n", getenv("VEDA_TEST") ? getenv("VEDA_TEST") : "(no env)");
		return 7;
	}
	if (!strcmp(mode, "abort"))
		abort();
	if (!strcmp(mode, "cwd")) {
		/* The parent's scratch directory, where notes.txt is. */
		return access("notes.txt", R_OK) == 0 ? 0 : 1;
	}
	return 99;
}

int main(int argc, char **argv)
{
	/* Where this program is, for the checks that start it again (a
	   sandboxed child's too). */
	const char *self = (const char *)getauxval(AT_EXECFN);
	snprintf(program, sizeof program, "%s", self && *self == '/' ? self : "/system/tests/c/posix");
	if (argc >= 3 && !strcmp(argv[1], "child"))
		return child(argv[2], argc >= 4 ? argv[3] : 0);
	setvbuf(stdout, 0, _IOLBF, 0);
	printf("posix: %s, tty %d\n", program, isatty(1));
	check_startup();
	check_stdio_and_strings();
	check_heap();
	check_mmap();
	check_system_files();
	check_memfd();
	check_files();
	check_directories();
	check_pipes();
	check_threads();
	check_thread_suspension();
	check_time();
	check_signals();
	check_processes();
	check_veda_interfaces();
	check_epoll();
	check_timerfd();
	check_unix_sockets();
	check_unix_abstract();
	check_shared_sockets();
	check_shared_connections();
	check_sandbox();
	check_loans();
	check_taking_back();
	if (argc >= 5 && !strcmp(argv[1], "host"))
		check_network(argv[2], atoi(argv[3]), atoi(argv[4]));
	else
		check_network(0, 0, 0);
	printf("%s: %d failed\n", failures ? "FAIL" : "PASS", failures);
	return failures;
}

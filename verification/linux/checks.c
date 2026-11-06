/* SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * checks — a Linux program, built for Linux by Linux's own toolchains (on
 * musl and on glibc, linked statically and dynamically), that checks what
 * Veda's Linux compatibility gives it: its start (arguments, auxiliary
 * vector, uname), its memory (the heap, mmap and friends, the program
 * break), files (sendfile and copy_file_range among them), /proc and
 * /sys, pipes, threads and their thread-local storage, signals (handlers
 * with their context, masks, the alternate stack, faults turned into
 * signals, registers kept across a handler, SA_RESTART), robust mutexes,
 * time, sockets, epoll, futexes, starting programs, and execve in place.
 *
 * Each check prints "ok - NAME" or "not ok - NAME: REASON", and the last
 * line is "PASS: N failed"; the program exits with the number of failed
 * checks. `systest` runs it inside Veda in a scratch directory.
 * `checks exec` replaces itself with `checks after-exec PID`, which checks
 * that it is the same process. */

#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <sched.h>
#include <setjmp.h>
#include <signal.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/sendfile.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <spawn.h>
#include <time.h>
#include <ucontext.h>
#include <unistd.h>
#include <linux/futex.h>

static int failures;

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
	printf(" (errno %d)\n", errno);
}

#define CHECK(name, cond, ...) report(name, (cond), __VA_ARGS__)

static void check_startup(int argc, char **argv)
{
	CHECK("argc", argc >= 1 && argv[argc] == 0, "argc %d", argc);
	CHECK("environment", getenv("HOME") != 0, "no HOME");
	CHECK("page size", getauxval(AT_PAGESZ) == 4096, "AT_PAGESZ %lu", getauxval(AT_PAGESZ));
	CHECK("random bytes", getauxval(AT_RANDOM) != 0, "no AT_RANDOM");
	const char *exe = (const char *)getauxval(AT_EXECFN);
	CHECK("program path", exe && exe[0], "AT_EXECFN %s", exe ? exe : "(none)");
	struct utsname u;
	int r = uname(&u);
	CHECK("uname", r == 0 && !strcmp(u.sysname, "Linux") && !strcmp(u.machine, "x86_64"), "%s %s", u.sysname,
	      u.machine);
	CHECK("ids", getuid() == 1000 && geteuid() == 1000 && getgid() == 1000, "uid %d", getuid());
	CHECK("main thread", getpid() == (pid_t)syscall(SYS_gettid), "pid %d tid %ld", getpid(), syscall(SYS_gettid));
}

static void check_memory(void)
{
	/* Small blocks (the program break, for glibc) and large ones (mmap),
	   grown in place or moved (mremap, for glibc). */
	char *small = malloc(100);
	char *large = malloc(8 << 20);
	int ok = small && large;
	if (ok) {
		memset(small, 1, 100);
		memset(large, 2, 8 << 20);
		large = realloc(large, 32 << 20);
		ok = large && large[(8 << 20) - 1] == 2;
		memset(large, 3, 32 << 20);
	}
	CHECK("heap", ok, "malloc and realloc");
	free(small);
	free(large);
	/* The program break, moved by the system call (musl's sbrk moves it
	   no more). */
	char *brk0 = (char *)syscall(SYS_brk, 0);
	char *grown = (char *)syscall(SYS_brk, brk0 + 8192);
	int grew = grown == brk0 + 8192;
	if (grew)
		brk0[8191] = 1;
	CHECK("program break", grew && (char *)syscall(SYS_brk, brk0) == brk0, "at %p, then %p", brk0, grown);

	char *m = mmap(0, 1 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	CHECK("mmap", m != MAP_FAILED, "anonymous");
	if (m == MAP_FAILED)
		return;
	m[0] = 1;
	m[(1 << 20) - 1] = 2;
	CHECK("mprotect", mprotect(m, 4096, PROT_READ) == 0, "read-only");
	/* A page mapped over one of a reservation, in place. */
	char *fixed = mmap(m + 8192, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
	CHECK("MAP_FIXED", fixed == m + 8192 && fixed[0] == 0, "fixed");
	errno = 0;
	void *taken = mmap(m + 8192, 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
	CHECK("MAP_FIXED_NOREPLACE", taken == MAP_FAILED && errno == EEXIST, "got %p", taken);
	munmap(m, 1 << 20);
	/* One mapping, grown where it is or moved, its bytes with it. */
	char *one = mmap(0, 1 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	one[(1 << 20) - 1] = 2;
	char *moved = mremap(one, 1 << 20, 4 << 20, MREMAP_MAYMOVE);
	CHECK("mremap", moved != MAP_FAILED && moved[(1 << 20) - 1] == 2, "grown");
	if (moved != MAP_FAILED)
		munmap(moved, 4 << 20);
	CHECK("munmap of nothing", munmap((void *)0x10000000000, 4096) == 0, "Linux allows it");
	errno = 0;
	CHECK("madvise of nothing", madvise((void *)0x10000000000, 4096, MADV_DONTNEED) < 0 && errno == ENOMEM,
	      "Linux refuses it");
	/* Shared memory that grows stays shared, or does not grow. */
	int fd = memfd_create("checks", 0);
	char *shared = MAP_FAILED;
	if (fd >= 0 && ftruncate(fd, 8192) == 0)
		shared = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	int kept = 0;
	if (shared != MAP_FAILED) {
		errno = 0;
		char *bigger = mremap(shared, 4096, 8192, MREMAP_MAYMOVE);
		if (bigger == MAP_FAILED) {
			kept = errno == ENOMEM;
			munmap(shared, 4096);
		} else {
			char c = 0;
			bigger[8191] = 9;
			kept = pread(fd, &c, 1, 8191) == 1 && c == 9;
			munmap(bigger, 8192);
		}
	}
	CHECK("mremap of shared memory", kept, "its pages are not the file's");
	if (fd >= 0)
		close(fd);
	char *here = mmap(0, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	errno = 0;
	void *onto = here == MAP_FAILED ? here : mremap(here, 8192, 8192, MREMAP_MAYMOVE | MREMAP_FIXED, here + 4096);
	CHECK("mremap onto itself", here != MAP_FAILED && onto == MAP_FAILED && errno == EINVAL, "got %p", onto);
	if (here != MAP_FAILED)
		munmap(here, 8192);
}

/* sendfile and copy_file_range, which cp and servers use: more than one
   chunk's worth, from an offset given and the file's own. */
static void check_copies(void)
{
	static char big[200000], back[200000];
	for (size_t i = 0; i < sizeof big; i++)
		big[i] = (char)(i * 7 + i / 251);
	int from = open("from.bin", O_CREAT | O_RDWR | O_TRUNC, 0644);
	int to = open("to.bin", O_CREAT | O_RDWR | O_TRUNC, 0644);
	int copy = open("copy.bin", O_CREAT | O_RDWR | O_TRUNC, 0644);
	if (from < 0 || to < 0 || copy < 0 || write(from, big, sizeof big) != sizeof big) {
		CHECK("copies", 0, "could not make the files");
		return;
	}
	off_t off = 1000;
	ssize_t sent = sendfile(to, from, &off, sizeof big);
	CHECK("sendfile", sent == sizeof big - 1000 && off == sizeof big && lseek(from, 0, SEEK_CUR) == sizeof big &&
	                      lseek(to, 0, SEEK_CUR) == sent && pread(to, back, sizeof back, 0) == sent &&
	                      !memcmp(back, big + 1000, sizeof big - 1000),
	      "sent %zd, offset %lld", sent, (long long)off);
	off_t in = 0, out = 0;
	ssize_t copied = 0, n;
	while (copied < (ssize_t)sizeof big && (n = copy_file_range(from, &in, copy, &out, sizeof big - copied, 0)) > 0)
		copied += n;
	CHECK("copy_file_range", copied == sizeof big && in == sizeof big && out == sizeof big &&
	                             lseek(copy, 0, SEEK_CUR) == 0 && pread(copy, back, sizeof back, 0) == sizeof big &&
	                             !memcmp(back, big, sizeof big),
	      "copied %zd", copied);
	in = 0, out = 100;
	CHECK("copy_file_range overlapping", copy_file_range(from, &in, from, &out, 1000, 0) < 0 && errno == EINVAL,
	      "not refused");
	close(from);
	close(to);
	close(copy);
	unlink("from.bin");
	unlink("to.bin");
	unlink("copy.bin");
}

static void check_files(void)
{
	int fd = open("file.txt", O_CREAT | O_RDWR | O_TRUNC, 0644);
	CHECK("open", fd >= 0, "create");
	if (fd < 0)
		return;
	CHECK("write", write(fd, "hello, linux\n", 13) == 13, "13 bytes");
	char buf[32] = {0};
	CHECK("pread", pread(fd, buf, sizeof buf, 0) == 13 && !strcmp(buf, "hello, linux\n"), "%s", buf);
	struct stat st;
	CHECK("fstat", fstat(fd, &st) == 0 && st.st_size == 13 && S_ISREG(st.st_mode), "size %ld", (long)st.st_size);
	close(fd);
	CHECK("stat", stat("file.txt", &st) == 0 && st.st_size == 13, "by name");
	CHECK("rename", rename("file.txt", "moved.txt") == 0 && access("moved.txt", R_OK) == 0, "moved");
	CHECK("mkdir", mkdir("dir", 0755) == 0, "dir");
	DIR *d = opendir(".");
	int seen = 0;
	if (d) {
		struct dirent *e;
		while ((e = readdir(d)))
			seen += !strcmp(e->d_name, "moved.txt") || !strcmp(e->d_name, "dir");
		closedir(d);
	}
	CHECK("readdir", seen == 2, "saw %d", seen);
	CHECK("unlink", unlink("moved.txt") == 0 && rmdir("dir") == 0, "removed");
	char cwd[4096];
	CHECK("getcwd", getcwd(cwd, sizeof cwd) && cwd[0] == '/', "%s", cwd);
	CHECK("/dev/null", (fd = open("/dev/null", O_WRONLY)) >= 0 && write(fd, "x", 1) == 1, "write");
	close(fd);
	check_copies();
}

/* /proc/self/fd/N: a link to what the descriptor is open on (a file's
   path, a pipe), which opens and is looked at as the descriptor's file,
   as ttyname, fexecve and programs that reopen their descriptors use it. */
static void check_fd_links(void)
{
	int file = open("fd-link.txt", O_CREAT | O_RDWR | O_TRUNC, 0644);
	int p[2];
	if (file < 0 || write(file, "fd", 2) != 2 || pipe(p)) {
		CHECK("/proc/self/fd", 0, "could not make the descriptors");
		return;
	}
	char link[64], target[4096] = {0}, cwd[4096] = {0}, want[8200];
	snprintf(link, sizeof link, "/proc/self/fd/%d", file);
	ssize_t n = readlink(link, target, sizeof target - 1);
	snprintf(want, sizeof want, "%s/fd-link.txt", getcwd(cwd, sizeof cwd) ? cwd : "?");
	CHECK("/proc/self/fd/N of a file", n > 0 && !strcmp(target, want), "%s, not %s", target, want);
	int again = open(link, O_RDONLY);
	char two[3] = {0};
	CHECK("/proc/self/fd/N opened", again >= 0 && pread(again, two, 2, 0) == 2 && !strcmp(two, "fd"), "read %s", two);
	struct stat followed, own, lnk;
	CHECK("/proc/self/fd/N looked at", stat(link, &followed) == 0 && fstat(file, &own) == 0 &&
	                                       followed.st_dev == own.st_dev && followed.st_ino == own.st_ino &&
	                                       lstat(link, &lnk) == 0 && S_ISLNK(lnk.st_mode),
	      "not the same file");
	snprintf(link, sizeof link, "/proc/self/fd/%d", p[0]);
	memset(target, 0, sizeof target);
	CHECK("/proc/self/fd/N of a pipe", readlink(link, target, sizeof target - 1) > 6 && !strncmp(target, "pipe:[", 6),
	      "%s", target);
	CHECK("/proc/self/fd/N of none", readlink("/proc/self/fd/999", target, sizeof target) < 0 && errno == ENOENT,
	      "found");
	if (again >= 0)
		close(again);
	close(file);
	close(p[0]);
	close(p[1]);
	unlink("fd-link.txt");
}

static void check_proc(void)
{
	/* The program's path, whole, which ends as the name it was started by
	   does. */
	char exe[4096] = {0};
	const char *execfn = (const char *)getauxval(AT_EXECFN);
	ssize_t n = readlink("/proc/self/exe", exe, sizeof exe - 1);
	const char *base = execfn ? strrchr(execfn, '/') : 0;
	base = base ? base + 1 : execfn;
	const char *last = strrchr(exe, '/');
	int same = n > 0 && exe[0] == '/' && base && last && !strcmp(last + 1, base);
	CHECK("/proc/self/exe", same, "%s against %s", exe, execfn);
	FILE *f = fopen("/proc/self/maps", "r");
	char line[512];
	int stack = 0;
	while (f && fgets(line, sizeof line, f))
		stack |= strstr(line, "[stack]") != 0;
	if (f)
		fclose(f);
	CHECK("/proc/self/maps", stack, "no [stack]");
	f = fopen("/proc/cpuinfo", "r");
	int processors = 0;
	while (f && fgets(line, sizeof line, f))
		processors += !strncmp(line, "processor", 9);
	if (f)
		fclose(f);
	long online = sysconf(_SC_NPROCESSORS_ONLN);
	CHECK("processors", processors >= 1 && online == processors, "%d in cpuinfo, %ld online", processors, online);
	f = fopen("/proc/self/stat", "r");
	int pid = 0;
	if (f) {
		if (fscanf(f, "%d", &pid) != 1)
			pid = 0;
		fclose(f);
	}
	CHECK("/proc/self/stat", pid == getpid(), "pid %d", pid);
	struct stat st;
	CHECK("/proc is a directory", stat("/proc/self", &st) == 0 && S_ISDIR(st.st_mode), "mode %o", st.st_mode);
	check_fd_links();
}

static void check_pipes(void)
{
	int p[2];
	CHECK("pipe", pipe(p) == 0, "pipe");
	struct pollfd pfd = {p[0], POLLIN, 0};
	CHECK("poll empty", poll(&pfd, 1, 0) == 0, "ready");
	CHECK("pipe write", write(p[1], "abc", 3) == 3, "3 bytes");
	CHECK("poll ready", poll(&pfd, 1, 1000) == 1 && pfd.revents & POLLIN, "revents %x", pfd.revents);
	char buf[4] = {0};
	CHECK("pipe read", read(p[0], buf, 3) == 3 && !strcmp(buf, "abc"), "%s", buf);
	close(p[0]);
	close(p[1]);
}

static __thread int tls_value;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cond = PTHREAD_COND_INITIALIZER;
static int counter, ready;

static void *worker(void *arg)
{
	tls_value = (int)(intptr_t)arg;
	for (int i = 0; i < 1000; i++) {
		pthread_mutex_lock(&lock);
		counter++;
		pthread_mutex_unlock(&lock);
	}
	pthread_mutex_lock(&lock);
	ready++;
	pthread_cond_signal(&cond);
	pthread_mutex_unlock(&lock);
	return (void *)(intptr_t)(tls_value * 2 + (syscall(SYS_gettid) != getpid() ? 0 : 1000));
}

static void check_threads(void)
{
	enum { N = 8 };
	pthread_t t[N];
	tls_value = 99;
	int made = 0;
	for (int i = 0; i < N; i++)
		made += pthread_create(&t[i], 0, worker, (void *)(intptr_t)(i + 1)) == 0;
	CHECK("pthread_create", made == N, "%d made", made);
	pthread_mutex_lock(&lock);
	while (ready < made)
		pthread_cond_wait(&cond, &lock);
	pthread_mutex_unlock(&lock);
	int joined = 0;
	for (int i = 0; i < made; i++) {
		void *r;
		joined += pthread_join(t[i], &r) == 0 && (intptr_t)r == 2 * (i + 1);
	}
	CHECK("pthread_join", joined == made, "%d joined right", joined);
	CHECK("mutex", counter == made * 1000, "counter %d", counter);
	CHECK("thread-local storage", tls_value == 99, "%d", tls_value);
}

static volatile sig_atomic_t got_signal, got_code;
static void *handler_stack;

static void on_usr1(int s, siginfo_t *info, void *context)
{
	(void)context;
	got_signal = s;
	got_code = info->si_code;
	handler_stack = __builtin_frame_address(0);
}

static sigjmp_buf fault_jump;
static void *fault_addr;
static volatile sig_atomic_t fault_signal;

static void on_fault(int s, siginfo_t *info, void *context)
{
	(void)context;
	fault_signal = s;
	fault_addr = info->si_addr;
	siglongjmp(fault_jump, 1);
}

static void on_alarm(int s)
{
	(void)s;
	/* Clobbers the vector registers, which the interrupted code keeps. */
	volatile double x = 1.5;
	for (int i = 0; i < 100; i++)
		x = x * 1.0001 + 0.5;
}

static void check_signals(void)
{
	struct sigaction sa = {0};
	sa.sa_sigaction = on_usr1;
	sa.sa_flags = SA_SIGINFO;
	sigemptyset(&sa.sa_mask);
	CHECK("sigaction", sigaction(SIGUSR1, &sa, 0) == 0, "SIGUSR1");
	raise(SIGUSR1);
	CHECK("handler", got_signal == SIGUSR1 && got_code == SI_TKILL, "signal %d code %d", got_signal, got_code);

	/* Blocked, pending, then run as it is unblocked. */
	sigset_t set, old;
	sigemptyset(&set);
	sigaddset(&set, SIGUSR1);
	sigprocmask(SIG_BLOCK, &set, &old);
	got_signal = 0;
	kill(getpid(), SIGUSR1);
	sigset_t pending;
	sigpending(&pending);
	CHECK("pending while blocked", got_signal == 0 && sigismember(&pending, SIGUSR1), "ran blocked");
	sigprocmask(SIG_SETMASK, &old, 0);
	CHECK("run once unblocked", got_signal == SIGUSR1, "did not run");

	/* On the alternate stack. */
	static char altstack[64 << 10];
	stack_t ss = {.ss_sp = altstack, .ss_size = sizeof altstack, .ss_flags = 0};
	CHECK("sigaltstack", sigaltstack(&ss, 0) == 0, "set");
	sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
	sigaction(SIGUSR1, &sa, 0);
	raise(SIGUSR1);
	int on = (char *)handler_stack > altstack && (char *)handler_stack < altstack + sizeof altstack;
	CHECK("handler on the alternate stack", on, "at %p", handler_stack);

	/* A bad access, as SIGSEGV with its address, which a handler leaves
	   by siglongjmp. */
	struct sigaction fa = {0};
	fa.sa_sigaction = on_fault;
	fa.sa_flags = SA_SIGINFO | SA_NODEFER;
	sigaction(SIGSEGV, &fa, 0);
	sigaction(SIGILL, &fa, 0);
	volatile int *bad = (int *)0x7000;
	if (!sigsetjmp(fault_jump, 1))
		*bad = 1;
	CHECK("SIGSEGV", fault_signal == SIGSEGV && fault_addr == (void *)bad, "signal %d at %p", fault_signal,
	      fault_addr);
	fault_signal = 0;
	if (!sigsetjmp(fault_jump, 1))
		__asm__ volatile("ud2");
	CHECK("SIGILL", fault_signal == SIGILL, "signal %d", fault_signal);

	/* A timer's signal interrupts a wait (no SA_RESTART: EINTR), and the
	   vector registers the code had are its own again. */
	struct sigaction aa = {0};
	aa.sa_handler = on_alarm;
	sigaction(SIGALRM, &aa, 0);
	volatile double before = 3.25;
	struct itimerval it = {{0, 0}, {0, 50000}};
	setitimer(ITIMER_REAL, &it, 0);
	int p[2];
	if (pipe(p))
		p[0] = p[1] = -1;
	char c;
	errno = 0;
	ssize_t r = read(p[0], &c, 1);
	double after = before * 2;
	CHECK("EINTR", r == -1 && errno == EINTR, "read returned %zd", r);
	CHECK("vector registers kept", after == 6.5, "%f", after);
	close(p[0]);
	close(p[1]);
}

static int restart_pipe[2];

static void *write_later(void *arg)
{
	(void)arg;
	usleep(150000);
	if (write(restart_pipe[1], "r", 1) != 1)
		return (void *)1;
	return 0;
}

static volatile sig_atomic_t restart_handled;

static void on_restart_alarm(int s)
{
	(void)s;
	restart_handled = 1;
}

/* A blocking call a handler with SA_RESTART interrupts starts again after
   it, and returns what came. */
static void check_restart(void)
{
	if (pipe(restart_pipe)) {
		CHECK("SA_RESTART", 0, "pipe");
		return;
	}
	struct sigaction sa = {0};
	sa.sa_handler = on_restart_alarm;
	sa.sa_flags = SA_RESTART;
	sigaction(SIGALRM, &sa, 0);
	pthread_t writer;
	pthread_create(&writer, 0, write_later, 0);
	struct itimerval it = {{0, 0}, {0, 50000}};
	setitimer(ITIMER_REAL, &it, 0);
	char c = 0;
	ssize_t r = read(restart_pipe[0], &c, 1);
	pthread_join(writer, 0);
	CHECK("SA_RESTART", r == 1 && c == 'r' && restart_handled, "read returned %zd, handled %d", r,
	      (int)restart_handled);
	signal(SIGALRM, SIG_DFL);
	close(restart_pipe[0]);
	close(restart_pipe[1]);
}

static pthread_mutex_t robust;

static void *die_holding(void *arg)
{
	(void)arg;
	pthread_mutex_lock(&robust);
	return 0;
}

/* A robust mutex whose owner ended holding it is the next locker's, who
   hears so (EOWNERDEAD): the robust list, which the thread's end walks. */
static void check_robust_mutex(void)
{
	pthread_mutexattr_t attr;
	pthread_mutexattr_init(&attr);
	pthread_mutexattr_setrobust(&attr, PTHREAD_MUTEX_ROBUST);
	pthread_mutex_init(&robust, &attr);
	pthread_t t;
	pthread_create(&t, 0, die_holding, 0);
	pthread_join(t, 0);
	int r = pthread_mutex_lock(&robust);
	CHECK("robust mutex", r == EOWNERDEAD, "lock returned %d", r);
	if (r == EOWNERDEAD) {
		pthread_mutex_consistent(&robust);
		pthread_mutex_unlock(&robust);
	}
}

static void check_time(void)
{
	struct timespec a, b;
	clock_gettime(CLOCK_MONOTONIC, &a);
	struct timespec ten = {0, 10000000};
	nanosleep(&ten, 0);
	clock_gettime(CLOCK_MONOTONIC, &b);
	long ns = (b.tv_sec - a.tv_sec) * 1000000000L + (b.tv_nsec - a.tv_nsec);
	CHECK("nanosleep", ns >= 10000000 && ns < 2000000000L, "%ld ns", ns);
	struct timeval tv;
	CHECK("gettimeofday", gettimeofday(&tv, 0) == 0 && tv.tv_sec > 1700000000, "%ld", (long)tv.tv_sec);
	CHECK("time", time(0) > 1700000000, "time");
}

static void check_sockets(void)
{
	int sv[2];
	CHECK("socketpair", socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0, "unix stream");
	char buf[8] = {0};
	CHECK("send and recv", send(sv[0], "ping", 4, 0) == 4 && recv(sv[1], buf, sizeof buf, 0) == 4, "%s", buf);
	int ep = epoll_create1(EPOLL_CLOEXEC);
	struct epoll_event ev = {.events = EPOLLIN, .data.fd = sv[1]}, out;
	CHECK("epoll", ep >= 0 && epoll_ctl(ep, EPOLL_CTL_ADD, sv[1], &ev) == 0, "create and add");
	CHECK("epoll empty", epoll_wait(ep, &out, 1, 0) == 0, "ready");
	send(sv[0], "x", 1, 0);
	CHECK("epoll ready", epoll_wait(ep, &out, 1, 1000) == 1 && out.data.fd == sv[1], "not ready");
	int e = eventfd(0, EFD_CLOEXEC);
	uint64_t v = 5;
	CHECK("eventfd", e >= 0 && write(e, &v, 8) == 8 && read(e, &v, 8) == 8 && v == 5, "%lu", (unsigned long)v);
	close(e);
	close(ep);
	close(sv[0]);
	close(sv[1]);
}

static void check_futex(void)
{
	atomic_int word = 0;
	struct timespec t = {0, 1000000};
	long r = syscall(SYS_futex, &word, FUTEX_WAIT_PRIVATE, 0, &t, 0, 0);
	CHECK("futex timeout", r == -1 && errno == ETIMEDOUT, "returned %ld", r);
	r = syscall(SYS_futex, &word, FUTEX_WAIT_PRIVATE, 1, &t, 0, 0);
	CHECK("futex value", r == -1 && errno == EAGAIN, "returned %ld", r);
	CHECK("futex wake", syscall(SYS_futex, &word, FUTEX_WAKE_PRIVATE, 1, 0, 0, 0) == 0, "woke");
}

extern char **environ;

static volatile sig_atomic_t chld_pid, chld_status, chld_code;

static void on_chld(int s, siginfo_t *info, void *context)
{
	(void)s;
	(void)context;
	chld_pid = info->si_pid;
	chld_status = info->si_status;
	chld_code = info->si_code;
}

/* Programs started: by posix_spawn (the C library's vfork and execve), with
   a file action, the child's output on a pipe; one that is not there; and
   by vfork itself, the child ending with _exit. */
static void check_processes(const char *self)
{
	int p[2];
	if (pipe(p)) {
		CHECK("pipe for a child", 0, "pipe");
		return;
	}
	/* The child's end sends SIGCHLD, saying who and how. */
	struct sigaction ca = {0};
	ca.sa_sigaction = on_chld;
	ca.sa_flags = SA_SIGINFO | SA_RESTART;
	sigaction(SIGCHLD, &ca, 0);
	posix_spawn_file_actions_t fa;
	posix_spawn_file_actions_init(&fa);
	posix_spawn_file_actions_adddup2(&fa, p[1], 1);
	posix_spawn_file_actions_addclose(&fa, p[0]);
	char *args[] = {(char *)self, "child", "7", 0};
	pid_t pid = 0;
	int r = posix_spawn(&pid, self, &fa, 0, args, environ);
	posix_spawn_file_actions_destroy(&fa);
	close(p[1]);
	char out[128] = {0};
	size_t got = 0;
	ssize_t n;
	while (got < sizeof out - 1 && (n = read(p[0], out + got, sizeof out - 1 - got)) > 0)
		got += n;
	close(p[0]);
	int status = -1;
	pid_t waited = r == 0 ? waitpid(pid, &status, 0) : -1;
	char expected[64];
	snprintf(expected, sizeof expected, "child of %d\n", getpid());
	CHECK("posix_spawn", r == 0 && waited == pid && pid != getpid(), "returned %d, waited %d for %d", r, waited, pid);
	CHECK("child's exit status", WIFEXITED(status) && WEXITSTATUS(status) == 7, "status %#x", status);
	CHECK("child's output", !strcmp(out, expected), "%s", out);
	for (int i = 0; i < 200 && chld_pid != pid; i++)
		usleep(5000);
	CHECK("SIGCHLD", chld_pid == pid && chld_code == CLD_EXITED && chld_status == 7, "pid %d code %d status %d",
	      (int)chld_pid, (int)chld_code, (int)chld_status);
	signal(SIGCHLD, SIG_DFL);
	args[0] = "/nonexistent/program";
	r = posix_spawn(&pid, args[0], 0, 0, args, environ);
	if (r == 0) {
		/* Some C libraries report it only as the child's exit status. */
		waitpid(pid, &status, 0);
		r = WIFEXITED(status) && WEXITSTATUS(status) == 127 ? ENOENT : -1;
	}
	CHECK("posix_spawn of nothing", r == ENOENT, "returned %d", r);
	pid = vfork();
	if (pid == 0)
		_exit(getppid() > 0 ? 3 : 4);
	status = -1;
	waited = pid > 0 ? waitpid(pid, &status, 0) : -1;
	CHECK("vfork", pid > 0 && waited == pid && WIFEXITED(status) && WEXITSTATUS(status) == 3, "pid %d status %#x",
	      pid, status);
	/* A fault whose handler has no stack to run on ends the process by
	   SIGSEGV, as Linux has it (rather than trying for ever). */
	char *bad[] = {(char *)self, "bad-stack", 0};
	r = posix_spawn(&pid, self, 0, 0, bad, environ);
	status = -1;
	waited = r == 0 ? waitpid(pid, &status, 0) : -1;
	CHECK("a handler with no stack", waited == pid && WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV,
	      "status %#x", status);
}

/* What `checks bad-stack` has for SIGSEGV: a handler that cannot run, its
   stack pointer being nowhere. */
static void on_segv_with_no_stack(int s)
{
	(void)s;
}

int main(int argc, char **argv)
{
	setvbuf(stdout, 0, _IOLBF, 0);
	if (argc >= 3 && !strcmp(argv[1], "child")) {
		printf("child of %d\n", getppid());
		return atoi(argv[2]);
	}
	if (argc >= 3 && !strcmp(argv[1], "after-exec")) {
		CHECK("execve keeps the process", getpid() == atoi(argv[2]), "pid %d, was %s", getpid(), argv[2]);
		printf("PASS: %d failed\n", failures);
		return failures;
	}
	if (argc >= 2 && !strcmp(argv[1], "bad-stack")) {
		signal(SIGSEGV, on_segv_with_no_stack);
		__asm__ volatile("mov $16, %%rsp\n\tmovq 0, %%rax" ::: "memory");
		__builtin_unreachable();
	}
	if (argc >= 2 && !strcmp(argv[1], "exec")) {
		char pid[16];
		snprintf(pid, sizeof pid, "%d", getpid());
		char *args[] = {argv[0], "after-exec", pid, 0};
		execv((const char *)getauxval(AT_EXECFN), args);
		printf("not ok - execve: errno %d\n", errno);
		return 1;
	}
	check_startup(argc, argv);
	check_memory();
	check_files();
	check_proc();
	check_pipes();
	check_threads();
	check_signals();
	check_restart();
	check_robust_mutex();
	check_time();
	check_sockets();
	check_futex();
	check_processes((const char *)getauxval(AT_EXECFN));
	printf("PASS: %d failed\n", failures);
	return failures;
}

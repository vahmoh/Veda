# SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
# SPDX-License-Identifier: GPL-3.0-only

"""stdlib.py: checks of Python's standard library on Veda, where CPython
stands on the C library and its modules are built into the interpreter (see
documentation/PYTHON.md).

Each check prints `ok - NAME` or `not ok - NAME: REASON`; the program exits
with the number of failed checks (0: all passed). `systest` runs it inside
Veda in a scratch directory, with its standard output and error on a socket
and the empty input as its standard input: `python3 stdlib.py`, or
`python3 stdlib.py host ADDRESS TCP-PORT UDP-PORT` to reach the host
machine's servers too (see `check_host`).

What Veda does not have (fork, file locks, dynamic loading...) is checked
too: it must fail as documentation/PYTHON.md says.
"""

import errno
import io
import os
import sys
import tempfile
import threading
import time
import traceback

HOST = None

# A self-signed certificate for localhost and 127.0.0.1, valid from 2026 to
# 2126, and its P-256 key: the TLS checks' server, which the client trusts.
TEST_KEY = """\
-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgzSJFjlcYjUFJFwv5
bgHQUP1s7EGyG+W2ualQwUw4pMKhRANCAAS/Rm4Lc19i1j4wtugcwr4W7k1VZt6b
bSM0RK+jA1zITsikdwffkpmyQujjgEpmJGE4ICoNQPvQYLmUOnDTAFqc
-----END PRIVATE KEY-----
"""
TEST_CERT = """\
-----BEGIN CERTIFICATE-----
MIIBmDCCAT6gAwIBAgIUMSOo8wAH/o+ZllHG9NGwvDpMYggwCgYIKoZIzj0EAwIw
FDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MDEwMTAwMDAwMFoYDzIxMjYwMTAx
MDAwMDAwWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIBBggqhkjO
PQMBBwNCAAS/Rm4Lc19i1j4wtugcwr4W7k1VZt6bbSM0RK+jA1zITsikdwffkpmy
QujjgEpmJGE4ICoNQPvQYLmUOnDTAFqco2wwajAdBgNVHQ4EFgQUWMr3rgy3BP9Q
HHe63Zt6M3VVKXYwHwYDVR0jBBgwFoAUWMr3rgy3BP9QHHe63Zt6M3VVKXYwGgYD
VR0RBBMwEYIJbG9jYWxob3N0hwR/AAABMAwGA1UdEwEB/wQCMAAwCgYIKoZIzj0E
AwIDSAAwRQIgZC549GsaFrIEvrVQAlGC6YKOZNrCYPD9OzNDhsDn3QQCIQCc4EfO
f7GSzUCLiOffpH7Wxzwn/BAM6qyG3DSzMRa3CQ==
-----END CERTIFICATE-----
"""


class Failed(Exception):
    """A check's reason for failing."""


def ensure(ok, what):
    """Fails with `what` (a string, or a function that makes one) unless `ok`."""
    if not ok:
        raise Failed(what() if callable(what) else what)


def raises(exception, call, *args, **kwargs):
    """The exception `call(*args, **kwargs)` raises, which must be an
    `exception`."""
    try:
        call(*args, **kwargs)
    except exception as e:
        return e
    raise Failed(f"{getattr(call, '__name__', call)} did not raise {exception.__name__}")


def python(*args, **kwargs):
    """Runs this Python with `args`, its output captured as text."""
    import subprocess

    kwargs.setdefault("capture_output", True)
    kwargs.setdefault("text", True)
    kwargs.setdefault("timeout", 120)
    return subprocess.run([sys.executable, *args], **kwargs)


def check_startup():
    import getpass
    import platform
    import pwd
    import sysconfig

    ensure(sys.platform == "veda" and os.name == "posix", f"the platform is {sys.platform} ({os.name})")
    ensure(platform.system() == "Veda", f"platform.system() is {platform.system()!r}")
    ensure(os.uname().sysname == "Veda", f"uname says {os.uname()}")
    ensure(sys.executable == "/system/bin/python3", f"sys.executable is {sys.executable!r}")
    ensure(sys.prefix == sys.exec_prefix == "/system", f"the prefixes are {sys.prefix!r}, {sys.exec_prefix!r}")
    ensure(sys.implementation._multiarch == "x86_64-veda", f"the triplet is {sys.implementation._multiarch!r}")
    ensure(sysconfig.get_config_var("SOABI") == "cpython-315-x86_64-veda", "SOABI")
    ensure(sysconfig.get_platform() == "veda-x86_64", f"sysconfig's platform is {sysconfig.get_platform()!r}")
    # Text is UTF-8 throughout, whatever the locale's variables say.
    ensure(sys.flags.utf8_mode == 1, "UTF-8 mode is off")
    ensure(sys.getfilesystemencoding() == "utf-8", f"file names are {sys.getfilesystemencoding()}")
    for stream in (sys.stdin, sys.stdout, sys.stderr):
        ensure(stream.encoding == "utf-8", f"{stream.name} is {stream.encoding}")
    ensure(os.environ.get("HOME", "").startswith("/"), "HOME is not set")
    # The C library is musl, and every program runs as Veda's one user.
    lib, version = platform.libc_ver()
    ensure(lib == "musl" and version.count(".") == 2, f"the C library is {lib} {version}")
    user = pwd.getpwuid(os.getuid())
    ensure(user.pw_name == "user" and user.pw_dir == "/home/user", f"the user is {user}")
    ensure(getpass.getuser() == "user", f"getpass says {getpass.getuser()!r}")
    ensure(tempfile.gettempdir() == "/tmp", f"the temporary directory is {tempfile.gettempdir()}")
    # No compiler for C extensions, and none to load: the modules are built
    # in.
    import importlib.machinery

    ensure(importlib.machinery.EXTENSION_SUFFIXES == [], "extension modules could be loaded")


# The modules of the standard library built into the interpreter, beyond
# those every build of CPython has.
BUILT_IN = (
    "_asyncio _bisect _blake2 _bz2 _contextvars _csv _datetime _decimal _elementtree _hashlib _heapq _hmac _json "
    "_lsprof _lzma _md5 _multibytecodec _multiprocessing _opcode _pickle _posixshmem _posixsubprocess _queue "
    "_random _sha1 _sha2 _sha3 _socket _sqlite3 _ssl _statistics _struct _zoneinfo _zstd array binascii cmath "
    "fcntl grp math mmap pyexpat resource select termios unicodedata zlib"
).split()

# What Veda has not, and why a traceback says it is missing.
MISSING = {
    "ctypes": "dynamic loading",
    "curses": "cannot move the cursor",
    "readline": "edits each line itself",
    "tkinter": "has no Tk",
    "dbm.gnu": "dbm.sqlite3",
    "pty": "no pseudo-terminals",
    "syslog": "standard error",
}


def check_modules():
    for name in BUILT_IN:
        ensure(name in sys.builtin_module_names, f"{name} is not built in")
        __import__(name)
    for name, why in MISSING.items():
        e = raises(ModuleNotFoundError, __import__, name)
        said = "".join(traceback.format_exception_only(e))
        ensure(why in said, f"import {name} says: {said.strip()}")


def check_bytecode():
    import importlib.util
    import json

    # The standard library's bytecode is in the image, checked against no
    # source (the image does not change): nothing is compiled at import.
    cached = json.__spec__.cached
    ensure(cached and os.path.isfile(cached), f"json has no bytecode ({cached})")
    with open(cached, "rb") as f:
        header = f.read(16)
    ensure(header[:4] == importlib.util.MAGIC_NUMBER, "the bytecode is of another Python")
    ensure(int.from_bytes(header[4:8], "little") == 0b01, "the bytecode is checked against its source")
    ensure(json.__file__ == "/system/lib/python3.15/json/__init__.py", f"json is {json.__file__}")


def check_files():
    import pathlib
    import shutil

    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        (root / "a.txt").write_text("Hello from Python on Veda\n", encoding="utf-8")
        ensure((root / "a.txt").read_text(encoding="utf-8") == "Hello from Python on Veda\n", "read back")
        with open(root / "a.txt", "r+b") as f:
            f.seek(6)
            f.write(b"FROM")
            f.truncate(17)
        ensure((root / "a.txt").read_bytes() == b"Hello FROM Python", "seek, write and truncate")
        (root / "d" / "e").mkdir(parents=True)
        (root / "d" / "e" / "f.bin").write_bytes(bytes(range(256)) * 64)
        os.rename(root / "a.txt", root / "d" / "b.txt")
        names = sorted(str(p.relative_to(root)) for p in root.rglob("*"))
        ensure(names == ["d", "d/b.txt", "d/e", "d/e/f.bin"], f"the tree is {names}")
        st = os.stat(root / "d" / "e" / "f.bin")
        ensure(st.st_size == 16384 and st.st_ino != 0, f"stat gave {st}")
        os.utime(root / "d" / "b.txt", (1_000_000_000, 1_000_000_000))
        ensure(os.stat(root / "d" / "b.txt").st_mtime == 1_000_000_000, "utime")
        shutil.copytree(root / "d", root / "copy")
        ensure((root / "copy" / "e" / "f.bin").read_bytes() == (root / "d" / "e" / "f.bin").read_bytes(), "copytree")
        shutil.rmtree(root / "copy")
        ensure(not (root / "copy").exists(), "rmtree")
        with os.scandir(root / "d") as entries:
            kinds = sorted((e.name, e.is_dir()) for e in entries)
        ensure(kinds == [("b.txt", False), ("e", True)], f"scandir gave {kinds}")
        # Veda's files have no links, hard or symbolic, and there are no
        # named pipes or device files to make: os has no way to.
        for name in ("link", "symlink", "mkfifo", "mknod"):
            ensure(not hasattr(os, name), f"os.{name} is there")
        raises(pathlib.UnsupportedOperation, (root / "link").symlink_to, root / "d" / "b.txt")
        raises(pathlib.UnsupportedOperation, (root / "hard").hardlink_to, root / "d" / "b.txt")
        # A file is the user's, who cannot give it away.
        os.chown(root / "d" / "b.txt", os.getuid(), os.getgid())
        raises(PermissionError, os.chown, root / "d" / "b.txt", 0, 0)


def check_threads():
    import concurrent.futures
    import queue

    results = queue.Queue()
    lock = threading.Lock()
    count = [0]

    def work(n):
        for _ in range(1000):
            with lock:
                count[0] += 1
        results.put(n * n)

    threads = [threading.Thread(target=work, args=(n,), name=f"worker-{n}") for n in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(30)
    ensure(not any(t.is_alive() for t in threads), "a thread did not end")
    ensure(count[0] == 8000, f"the counter is {count[0]}")
    ensure(sorted(results.get() for _ in threads) == [n * n for n in range(8)], "the results")
    with concurrent.futures.ThreadPoolExecutor(4) as pool:
        ensure(list(pool.map(pow, range(10), [2] * 10)) == [n * n for n in range(10)], "a pool's results")
    condition = threading.Condition()
    ready = []

    def signal():
        with condition:
            ready.append(True)
            condition.notify()

    with condition:
        threading.Timer(0.05, signal).start()
        ensure(condition.wait_for(lambda: ready, timeout=10), "a condition was not notified")
    # A thread's stack (8 MiB) holds the recursion limit's calls, and a
    # recursion beyond it is an error, not a crash.
    depth = []

    def recurse(n):
        return n if n == 0 else recurse(n - 1) + 1

    def deep():
        depth.append(recurse(900))
        try:
            recurse(10**6)
        except RecursionError:
            depth.append("limit")

    t = threading.Thread(target=deep)
    t.start()
    t.join(60)
    ensure(depth == [900, "limit"], f"the thread's recursion gave {depth}")
    ensure(threading.get_native_id() > 0, "no thread id")


def check_subprocess():
    import subprocess

    r = python("-c", "import sys; print(sys.argv[1:]); sys.exit(3)", "one", "two")
    ensure(r.returncode == 3 and r.stdout == "['one', 'two']\n", f"the child gave {r}")
    r = python("-c", "import sys; data = sys.stdin.read(); print(data.upper(), end='')", input="through a pipe")
    ensure(r.stdout == "THROUGH A PIPE", f"the child read {r.stdout!r}")
    r = python("-c", "import sys; sys.stderr.write('to stderr')", stderr=subprocess.STDOUT, capture_output=False,
               stdout=subprocess.PIPE)
    ensure(r.stdout == "to stderr", f"stderr went to {r.stdout!r}")
    with tempfile.TemporaryDirectory() as tmp:
        r = python("-c", "import os; print(os.getcwd(), os.environ['VEDA_CHECK'])", cwd=tmp,
                   env={**os.environ, "VEDA_CHECK": "passed"})
        ensure(r.stdout == f"{tmp} passed\n", f"cwd and env gave {r.stdout!r}")
    # A descriptor passed on, and one that is not.
    read_end, write_end = os.pipe()
    other_read, other_write = os.pipe()
    os.set_inheritable(other_write, True)
    try:
        r = python("-c", f"import os; os.write({write_end}, b'passed'); os.write({other_write}, b'x')",
                   pass_fds=[write_end])
        ensure(r.returncode != 0 and "Bad file descriptor" in r.stderr, "close_fds left a descriptor open")
        os.close(write_end)
        write_end = -1
        ensure(os.read(read_end, 100) == b"passed", "pass_fds")
    finally:
        for fd in (read_end, write_end, other_read, other_write):
            if fd != -1:
                os.close(fd)
    # Errors are the parent's, as a forked child's would be reported.
    e = raises(FileNotFoundError, subprocess.run, ["/system/bin/no-such-program"])
    ensure(e.filename == "/system/bin/no-such-program", f"the error names {e.filename!r}")
    e = raises(FileNotFoundError, subprocess.run, [sys.executable, "-c", ""], cwd="/no/such/directory")
    ensure(e.filename == "/no/such/directory", f"the error names {e.filename!r}")
    e = raises(OSError, subprocess.run, [sys.executable, "-c", ""], preexec_fn=lambda: None)
    ensure(e.errno == errno.ENOTSUP, f"preexec_fn failed with {e}")
    raises(FileNotFoundError, subprocess.run, "echo", shell=True)
    # Time-outs, and ending a child.
    raises(subprocess.TimeoutExpired, python, "-c", "import time; time.sleep(60)", timeout=0.5)
    p = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
    p.terminate()
    ensure(p.wait(30) == -15, f"a child ended by SIGTERM exited with {p.returncode}")
    # os.posix_spawn directly.
    pid = os.posix_spawn(sys.executable, [sys.executable, "-c", "raise SystemExit(7)"], os.environ)
    ensure(os.waitstatus_to_exitcode(os.waitpid(pid, 0)[1]) == 7, "posix_spawn's child")
    ensure(not hasattr(os, "fork") and not hasattr(os, "execv"), "fork or exec is there")
    for name in ("execl", "execle", "execlp", "execlpe", "execvp", "execvpe"):
        ensure(not hasattr(os, name), f"os.{name} is there")
    # os.spawn*() start programs with posix_spawn.
    code = os.spawnv(os.P_WAIT, sys.executable, [sys.executable, "-c", "raise SystemExit(5)"])
    ensure(code == 5, f"spawnv's child exited with {code}")
    pid = os.spawnl(os.P_NOWAIT, sys.executable, sys.executable, "-c", "pass")
    ensure(os.waitstatus_to_exitcode(os.waitpid(pid, 0)[1]) == 0, "spawnl's child")
    # A child's nice value is its parent's.
    os.setpriority(os.PRIO_PROCESS, 0, 3)
    try:
        r = python("-c", "import os; print(os.getpriority(os.PRIO_PROCESS, 0))")
    finally:
        os.setpriority(os.PRIO_PROCESS, 0, 0)
    ensure(r.stdout == "3\n", f"the child's nice value is {r.stdout!r}")


def child_of_multiprocessing(connection, value):
    connection.send(value * 2)
    connection.close()


def child_with_a_lock(lock, event, results):
    # The parent holds the lock until the child says it waits.
    results.put(lock.acquire(timeout=0.1))
    event.set()
    results.put(lock.acquire(timeout=60))
    lock.release()
    results.put("done")


def square(n):
    return n * n


def check_multiprocessing():
    import multiprocessing

    ensure(multiprocessing.get_all_start_methods() == ["spawn"], multiprocessing.get_all_start_methods)
    ensure(multiprocessing.get_start_method() == "spawn", "the default start method")
    ours, theirs = multiprocessing.Pipe()
    p = multiprocessing.Process(target=child_of_multiprocessing, args=(theirs, 21))
    p.start()
    ensure(ours.poll(60) and ours.recv() == 42, "the child's answer")
    p.join(60)
    ensure(p.exitcode == 0, f"the child exited with {p.exitcode}")
    # A lock, an event and a queue between processes: named semaphores.
    lock, event, results = multiprocessing.Lock(), multiprocessing.Event(), multiprocessing.Queue()
    lock.acquire()
    p = multiprocessing.Process(target=child_with_a_lock, args=(lock, event, results))
    p.start()
    ensure(results.get(timeout=60) is False, "the child took the lock this process holds")
    ensure(event.wait(60), "the child's event")
    lock.release()
    ensure(results.get(timeout=60) is True, "the child did not get the lock once it was free")
    ensure(results.get(timeout=60) == "done", "the child's last word")
    p.join(60)
    ensure(p.exitcode == 0, f"the child with the lock exited with {p.exitcode}")


def check_process_pools():
    import concurrent.futures
    import multiprocessing

    expected = [n * n for n in range(10)]
    with multiprocessing.Pool(2) as pool:
        squares = pool.map(square, range(10))
        ensure(pool.apply_async(square, (11,)).get(60) == 121, "an asynchronous result")
    ensure(squares == expected, f"Pool.map gave {squares}")
    with concurrent.futures.ProcessPoolExecutor(2) as executor:
        squares = list(executor.map(square, range(10)))
        ensure(executor.submit(square, 12).result(timeout=60) == 144, "a future of the executor")
    ensure(squares == expected, f"ProcessPoolExecutor.map gave {squares}")


def child_of_shared_memory(name):
    from multiprocessing import shared_memory

    memory = shared_memory.SharedMemory(name)
    memory.buf[:5] = bytes(memory.buf[:5]).upper()
    memory.close()


def check_shared_memory():
    import multiprocessing
    from multiprocessing import shared_memory

    memory = shared_memory.SharedMemory(create=True, size=1000)
    path = f"/dev/shm/{memory.name}"
    try:
        ensure(memory.size == 1000 and os.path.exists(path), f"{path} is not there")
        memory.buf[:5] = b"hello"
        p = multiprocessing.Process(target=child_of_shared_memory, args=(memory.name,))
        p.start()
        p.join(60)
        ensure(p.exitcode == 0, f"the child exited with {p.exitcode}")
        ensure(bytes(memory.buf[:5]) == b"HELLO", f"the memory holds {bytes(memory.buf[:5])!r}")
        # A list in shared memory, found by its name.
        items = shared_memory.ShareableList([1, "two", 3.0])
        try:
            again = shared_memory.ShareableList(name=items.shm.name)
            again[0] = 10
            ensure(items[0] == 10 and list(again) == [10, "two", 3.0], f"the lists hold {list(again)}")
            again.shm.close()
        finally:
            items.shm.close()
            items.shm.unlink()
    finally:
        memory.close()
        memory.unlink()
    ensure(not os.path.exists(path), f"{path} is still there")


def check_signals():
    import signal

    got = []
    old = signal.signal(signal.SIGUSR1, lambda signum, frame: got.append(signum))
    try:
        os.kill(os.getpid(), signal.SIGUSR1)
        signal.raise_signal(signal.SIGUSR1)
        # From another thread, as its next system call returns.
        main = threading.get_ident()
        threading.Thread(target=signal.pthread_kill, args=(main, signal.SIGUSR1)).start()
        deadline = time.monotonic() + 10
        while len(got) < 3 and time.monotonic() < deadline:
            time.sleep(0.01)
        ensure(got == [signal.SIGUSR1] * 3, f"the handler ran for {got}")
    finally:
        signal.signal(signal.SIGUSR1, old)
    raises(KeyboardInterrupt, signal.raise_signal, signal.SIGINT)
    # Signals are synchronous: no alarms or interval timers.
    ensure(not hasattr(signal, "alarm") and not hasattr(signal, "setitimer"), "alarm or setitimer is there")


def check_time():
    import datetime
    import zoneinfo

    ensure(time.time() > 1_767_225_600, f"the time is {time.time()} (before 2026)")
    a = time.monotonic()
    time.sleep(0.05)
    ensure(0.05 <= time.monotonic() - a < 5, "sleeping 50 ms")
    ensure(time.gmtime(0)[:6] == (1970, 1, 1, 0, 0, 0), "the epoch")
    now = datetime.datetime.now(datetime.timezone.utc)
    ensure(abs(now.timestamp() - time.time()) < 5, "datetime and time disagree")
    # Local time follows TZ, a POSIX time zone.
    old = os.environ.get("TZ")
    try:
        os.environ["TZ"] = "<+0530>-5:30"
        time.tzset()
        ensure(time.localtime(0)[3:5] == (5, 30) and time.timezone == -19800, "TZ's offset")
    finally:
        if old is None:
            del os.environ["TZ"]
        else:
            os.environ["TZ"] = old
        time.tzset()
    # There is no tz database to name zones by.
    raises(zoneinfo.ZoneInfoNotFoundError, zoneinfo.ZoneInfo, "Europe/London")
    # A thread's CPU time is its own: one waiting for another that spins
    # has not spun.
    spun = []

    def spin():
        start = time.thread_time()
        while time.thread_time() - start < 0.05:
            pass
        spun.append(time.thread_time() - start)

    mine = time.thread_time()
    t = threading.Thread(target=spin)
    t.start()
    t.join(60)
    ensure(spun and spun[0] >= 0.05, f"the thread spun {spun}")
    ensure(time.thread_time() - mine < 0.025, "the waiting thread's CPU time counts the other's")


def check_sockets():
    import select
    import socket

    for family, address in ((socket.AF_INET, "127.0.0.1"), (socket.AF_INET6, "::1")):
        with socket.create_server((address, 0), family=family) as server:
            port = server.getsockname()[1]
            with socket.create_connection((address, port), timeout=10) as client:
                conn, _ = server.accept()
                with conn:
                    client.sendall(b"ping" * 10000)
                    data = b""
                    while len(data) < 40000:
                        data += conn.recv(65536)
                    ensure(data == b"ping" * 10000, f"TCP over {address} gave {len(data)} bytes")
                    conn.sendall(b"pong")
                    ensure(client.recv(4) == b"pong", "the answer")
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as a, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as b:
        a.bind(("127.0.0.1", 0))
        b.sendto(b"datagram", a.getsockname())
        a.settimeout(10)
        data, sender = a.recvfrom(100)
        ensure(data == b"datagram" and sender[0] == "127.0.0.1", f"UDP gave {data!r} from {sender}")
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "socket")
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
            server.bind(path)
            server.listen()
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
                client.connect(path)
                conn, _ = server.accept()
                with conn:
                    client.sendall(b"unix")
                    ensure(conn.recv(4) == b"unix", "a Unix domain socket")
    a, b = socket.socketpair()
    with a, b:
        ensure(select.select([a], [], [], 0)[0] == [], "select saw data that is not there")
        b.send(b"x")
        ensure(select.select([a], [], [], 10)[0] == [a], "select")
        poller = select.poll()
        poller.register(a, select.POLLIN)
        ensure(poller.poll(10000) == [(a.fileno(), select.POLLIN)], "poll")
        epoll = select.epoll()
        epoll.register(a.fileno(), select.EPOLLIN)
        ensure(epoll.poll(10) == [(a.fileno(), select.EPOLLIN)], "epoll")
        epoll.close()
    infos = socket.getaddrinfo("localhost", 80, type=socket.SOCK_STREAM)
    ensure(any(info[4][0] == "127.0.0.1" for info in infos), f"localhost is {infos}")
    # Services by name (the C library's database).
    ensure(socket.getservbyname("http", "tcp") == 80, "the http service")
    ensure(socket.getservbyport(443, "tcp") == "https", "port 443")
    infos = socket.getaddrinfo("127.0.0.1", "https", type=socket.SOCK_STREAM)
    ensure(infos[0][4][1] == 443, f"getaddrinfo of https gave {infos}")
    # A TCP socket has its port from bind() on: no other binds it until it
    # is closed.
    with socket.socket() as bound:
        bound.bind(("127.0.0.1", 0))
        port = bound.getsockname()[1]
        ensure(port != 0, "bind() gave no port")
        with socket.socket() as other:
            e = raises(OSError, other.bind, ("127.0.0.1", port))
            ensure(e.errno == errno.EADDRINUSE, f"binding a port bound failed with {e}")
        bound.listen()
        ensure(bound.getsockname()[1] == port, "listening moved the port")
    with socket.socket() as again:
        again.bind(("127.0.0.1", port))
    # Sockets on addresses apart share a port, as a server on every address
    # makes them: an IPv4 socket and an IPv6-only one. A socket of both
    # versions takes IPv4 too, through mapped addresses.
    with socket.create_server(("0.0.0.0", 0)) as v4:
        port = v4.getsockname()[1]
        with socket.create_server(("::", port), family=socket.AF_INET6) as v6:
            for address, server in (("127.0.0.1", v4), ("::1", v6)):
                with socket.create_connection((address, port), timeout=10):
                    conn, peer = server.accept()
                    conn.close()
                    ensure(peer[0] == address, f"a connection to {address} came from {peer}")
        e = raises(OSError, socket.create_server, ("::", port), family=socket.AF_INET6, dualstack_ipv6=True)
        ensure(e.errno == errno.EADDRINUSE, f"both versions on an IPv4 socket's port gave {e}")
    with socket.create_server(("::", 0), family=socket.AF_INET6, dualstack_ipv6=True) as both:
        with socket.create_connection(("127.0.0.1", both.getsockname()[1]), timeout=10):
            conn, peer = both.accept()
            conn.close()
            ensure(peer[0] == "::ffff:127.0.0.1", f"IPv4 to a socket of both versions came from {peer}")


def check_asyncio():
    import asyncio

    async def main():
        async def echo(reader, writer):
            writer.write(await reader.readline())
            await writer.drain()
            writer.close()

        server = await asyncio.start_server(echo, "127.0.0.1", 0)
        port = server.sockets[0].getsockname()[1]
        reader, writer = await asyncio.open_connection("127.0.0.1", port)
        writer.write(b"through asyncio\n")
        line = await asyncio.wait_for(reader.readline(), 10)
        writer.close()
        server.close()
        await server.wait_closed()
        child = await asyncio.create_subprocess_exec(
            sys.executable, "-c", "print('from a child')", stdout=asyncio.subprocess.PIPE
        )
        out, _ = await asyncio.wait_for(child.communicate(), 60)
        return line, out, child.returncode

    line, out, code = asyncio.run(main())
    ensure(line == b"through asyncio\n", f"the echo was {line!r}")
    ensure(out == b"from a child\n" and code == 0, f"the child gave {out!r} ({code})")


def check_tls():
    import socket
    import ssl

    ensure(ssl.OPENSSL_VERSION.startswith("OpenSSL 3.5."), ssl.OPENSSL_VERSION)
    paths = ssl.get_default_verify_paths()
    ensure(paths.openssl_cafile == "/system/etc/ssl/cert.pem", f"the default CA file is {paths.openssl_cafile}")
    ensure(os.path.isfile(paths.openssl_cafile), "the system has no certificate authorities")
    ensure(len(ssl.create_default_context().get_ca_certs()) > 50, "the default context trusts few authorities")
    with tempfile.TemporaryDirectory() as tmp:
        certfile = os.path.join(tmp, "cert.pem")
        with open(certfile, "w") as f:
            f.write(TEST_KEY + TEST_CERT)
        server_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        server_context.load_cert_chain(certfile)
    client_context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    client_context.load_verify_locations(cadata=TEST_CERT)
    with socket.create_server(("127.0.0.1", 0)) as listener:
        port = listener.getsockname()[1]
        served = []

        def serve():
            conn, _ = listener.accept()
            with server_context.wrap_socket(conn, server_side=True) as tls:
                served.append(tls.recv(100))
                tls.sendall(b"secret answer")

        t = threading.Thread(target=serve)
        t.start()
        with socket.create_connection(("127.0.0.1", port), timeout=30) as raw:
            with client_context.wrap_socket(raw, server_hostname="localhost") as tls:
                version = tls.version()
                tls.sendall(b"secret question")
                answer = tls.recv(100)
        t.join(30)
    ensure(version == "TLSv1.3", f"the connection was {version}")
    ensure(served == [b"secret question"] and answer == b"secret answer", f"TLS carried {served}, {answer!r}")


def check_hashes():
    import hashlib
    import hmac

    vectors = {
        "md5": "900150983cd24fb0d6963f7d28e17f72",
        "sha1": "a9993e364706816aba3e25717850c26c9cd0d89d",
        "sha256": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "sha3_256": "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532",
        "blake2b": "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d1"
        "7d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923",
    }
    for name, digest in vectors.items():
        ensure(hashlib.new(name, b"abc").hexdigest() == digest, f"{name} of 'abc'")
    ensure("sha512_256" in hashlib.algorithms_available, "OpenSSL's digests are not there")
    mac = hmac.new(b"key", b"The quick brown fox jumps over the lazy dog", "sha256").hexdigest()
    ensure(mac == "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8", "HMAC-SHA256")
    ensure(len(hashlib.scrypt(b"password", salt=b"salt", n=16, r=8, p=1)) == 64, "scrypt")
    ensure(len(os.urandom(32)) == 32 and os.urandom(16) != os.urandom(16), "os.urandom")


def check_compression():
    import bz2
    import gzip
    import lzma
    import tarfile
    import zipfile
    import zlib
    from compression import zstd

    data = b"Veda " * 10000 + bytes(range(256))
    for name, compress, decompress in (
        ("zlib", zlib.compress, zlib.decompress),
        ("gzip", gzip.compress, gzip.decompress),
        ("bz2", bz2.compress, bz2.decompress),
        ("lzma", lzma.compress, lzma.decompress),
        ("zstd", zstd.compress, zstd.decompress),
    ):
        packed = compress(data)
        ensure(len(packed) < len(data) // 10 and decompress(packed) == data, f"{name}'s round trip")
    ensure(zstd.compress(data, options={zstd.CompressionParameter.nb_workers: 2}) != b"", "zstd's worker threads")
    with tempfile.TemporaryDirectory() as tmp:
        archive = os.path.join(tmp, "a.zip")
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as z:
            z.writestr("veda.txt", data)
        with zipfile.ZipFile(archive) as z:
            ensure(z.read("veda.txt") == data and z.testzip() is None, "a zip archive")
        for mode in ("gz", "bz2", "xz", "zst"):
            archive = os.path.join(tmp, f"a.tar.{mode}")
            with tarfile.open(archive, f"w:{mode}") as t:
                info = tarfile.TarInfo("veda.txt")
                info.size = len(data)
                t.addfile(info, io.BytesIO(data))
            with tarfile.open(archive) as t:
                ensure(t.extractfile("veda.txt").read() == data, f"a tar.{mode} archive")


def check_sqlite():
    import sqlite3

    ensure(sqlite3.sqlite_version == "3.54.1", f"SQLite is {sqlite3.sqlite_version}")
    with tempfile.TemporaryDirectory() as tmp:
        connection = sqlite3.connect(os.path.join(tmp, "veda.db"))
        with connection:
            connection.execute("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)")
            connection.executemany("INSERT INTO notes (body) VALUES (?)", [("one",), ("two",), ("three",)])
        rows = connection.execute("SELECT body FROM notes ORDER BY id").fetchall()
        ensure(rows == [("one",), ("two",), ("three",)], f"the table holds {rows}")
        connection.execute("CREATE VIRTUAL TABLE pages USING fts5(text)")
        connection.execute("INSERT INTO pages VALUES ('an operating system written in Rust')")
        found = connection.execute("SELECT text FROM pages WHERE pages MATCH 'rust'").fetchall()
        ensure(found == [("an operating system written in Rust",)], "full-text search")
        ensure(connection.execute("SELECT json_extract('{\"a\": [1, 2]}', '$.a[1]'), sqrt(16)").fetchone() == (2, 4.0),
               "JSON and the mathematical functions")
        connection.execute("CREATE VIRTUAL TABLE boxes USING rtree(id, x0, x1)")
        connection.close()
    import dbm.sqlite3

    with tempfile.TemporaryDirectory() as tmp:
        with dbm.open(os.path.join(tmp, "db"), "c") as db:
            db[b"key"] = b"value"
        with dbm.open(os.path.join(tmp, "db"), "r") as db:
            ensure(db[b"key"] == b"value", "dbm.sqlite3")


def check_data_formats():
    import array
    import csv
    import decimal
    import json
    import pickle
    import struct
    import xml.etree.ElementTree as ET

    import pyexpat

    ensure(decimal.__libmpdec_version__ == "4.0.1", f"libmpdec is {decimal.__libmpdec_version__}")
    decimal.getcontext().prec = 50
    third = decimal.Decimal(1) / decimal.Decimal(3)
    ensure(str(third) == "0." + "3" * 50, f"1/3 is {third}")
    ensure(pyexpat.EXPAT_VERSION == "expat_2.9.0", pyexpat.EXPAT_VERSION)
    tree = ET.fromstring("<veda><app name='Terminal'/><app name='Python'/></veda>")
    ensure([a.get("name") for a in tree.iter("app")] == ["Terminal", "Python"], "ElementTree")
    value = {"name": "Veda", "list": [1, 2.5, None, True], "nested": {"é": "ü"}}
    ensure(json.loads(json.dumps(value)) == value, "json")
    ensure(pickle.loads(pickle.dumps(value, protocol=5)) == value, "pickle")
    ensure(struct.unpack("<IdH", struct.pack("<IdH", 7, 2.5, 9)) == (7, 2.5, 9), "struct")
    ensure(array.array("q", range(5)).tobytes() == struct.pack("<5q", *range(5)), "array")
    rows = list(csv.reader(io.StringIO('a,"b,c",d\n1,2,3\n')))
    ensure(rows == [["a", "b,c", "d"], ["1", "2", "3"]], f"csv gave {rows}")


def check_memory():
    import mmap

    with mmap.mmap(-1, 1 << 20) as m:
        m[:5] = b"hello"
        m[-5:] = b"world"
        ensure(m[:5] + m[-5:] == b"helloworld", "anonymous memory")
    with tempfile.TemporaryFile() as f:
        f.write(b"x" * 8192)
        f.flush()
        with mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as m:
            ensure(m[:3] == b"xxx" and len(m) == 8192, "a file mapped to read")
        with mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_COPY) as m:
            m[:3] = b"yyy"
            ensure(m[:3] == b"yyy", "a file's private copy")
        # Writes to a shared mapping of a file would never reach it.
        e = raises(OSError, mmap.mmap, f.fileno(), 0, access=mmap.ACCESS_WRITE)
        ensure(e.errno == errno.ENODEV, f"a shared writable mapping failed with {e}")
    # A memory file is shared memory.
    fd = os.memfd_create("veda-check")
    try:
        os.ftruncate(fd, 4096)
        with mmap.mmap(fd, 4096) as a, mmap.mmap(fd, 4096) as b:
            a[:4] = b"same"
            ensure(b[:4] == b"same", "a memory file's mappings")
    finally:
        os.close(fd)
    fd = os.eventfd(0)
    try:
        os.eventfd_write(fd, 3)
        ensure(os.eventfd_read(fd) == 3, "an eventfd")
    finally:
        os.close(fd)


def check_file_locks():
    import fcntl

    # Veda's file system cannot arbitrate locks between processes yet.
    with tempfile.TemporaryFile() as f:
        e = raises(OSError, fcntl.flock, f.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        ensure(e.errno == errno.ENOLCK, f"flock failed with {e}")
        fcntl.flock(f.fileno(), fcntl.LOCK_UN)


def check_environments():
    import venv  # noqa: F401: the module is there

    with tempfile.TemporaryDirectory() as tmp:
        env = os.path.join(tmp, "env")
        r = python("-m", "venv", env, timeout=300)
        ensure(r.returncode == 0, f"venv failed: {r.stderr}")
        # Copies, not links: there are none; and no warning of it.
        interpreter = os.path.join(env, "bin", "python3")
        ensure(os.path.isfile(interpreter) and not os.path.islink(interpreter), "the environment's python3")
        ensure("symlink" not in r.stderr, f"venv warned: {r.stderr}")
        code = "import sys; print(sys.prefix, sys.base_prefix, sys.executable)"
        r = __import__("subprocess").run([interpreter, "-c", code], capture_output=True, text=True, timeout=60)
        ensure(r.stdout.split() == [env, "/system", interpreter], f"the environment's Python says {r.stdout!r}")
        # pip, from the wheel ensurepip has, without the network.
        r = __import__("subprocess").run(
            [interpreter, "-m", "pip", "--version"], capture_output=True, text=True, timeout=300
        )
        ensure(r.returncode == 0 and r.stdout.startswith("pip "), f"pip says {r.stdout!r} {r.stderr!r}")


def check_host():
    import socket
    import urllib.request

    if HOST is None:
        print("# the host: no servers were named")
        return
    address, tcp, udp = HOST
    # The network comes up after the system starts (its card is the driver
    # VM's): until it does, a connection fails at once.
    deadline = time.monotonic() + 60
    while True:
        try:
            with urllib.request.urlopen(f"http://{address}:{tcp}/hello", timeout=10) as response:
                body = response.read()
            break
        except OSError as e:
            reason = getattr(e, "reason", e)
            unreachable = (errno.ENETDOWN, errno.ENETUNREACH, errno.EHOSTUNREACH)
            if getattr(reason, "errno", None) not in unreachable or time.monotonic() > deadline:
                raise
            time.sleep(0.25)
    ensure(body == b"hello from the host\n", f"the host answered {body!r}")
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as u:
        u.settimeout(2)
        # A datagram may be lost: a few tries.
        for _ in range(5):
            u.sendto(b"echo from python", (address, udp))
            try:
                data, sender = u.recvfrom(64)
            except TimeoutError:
                continue
            ensure(data == b"echo from python" and sender == (address, udp), f"got {data!r} from {sender}")
            return
    raise Failed(f"{address}:{udp} did not echo")


CHECKS = [
    ("startup", check_startup),
    ("modules", check_modules),
    ("bytecode", check_bytecode),
    ("files", check_files),
    ("threads", check_threads),
    ("subprocess", check_subprocess),
    ("multiprocessing", check_multiprocessing),
    ("process pools", check_process_pools),
    ("shared memory", check_shared_memory),
    ("signals", check_signals),
    ("time", check_time),
    ("sockets", check_sockets),
    ("asyncio", check_asyncio),
    ("tls", check_tls),
    ("hashes", check_hashes),
    ("compression", check_compression),
    ("sqlite", check_sqlite),
    ("data formats", check_data_formats),
    ("memory", check_memory),
    ("file locks", check_file_locks),
    ("environments", check_environments),
    ("the host", check_host),
]


def main():
    global HOST
    if len(sys.argv) == 5 and sys.argv[1] == "host":
        HOST = (sys.argv[2], int(sys.argv[3]), int(sys.argv[4]))
    print(f"stdlib.py: Python {sys.version.split()[0]} on {sys.platform}, terminal {sys.stdout.isatty()}")
    failures = 0
    for name, check in CHECKS:
        try:
            check()
        except Failed as e:
            failures += 1
            print(f"not ok - {name}: {e}")
        except Exception as e:
            failures += 1
            where = traceback.extract_tb(e.__traceback__)[-1]
            print(f"not ok - {name}: {type(e).__name__}: {e} (line {where.lineno})")
        else:
            print(f"ok - {name}")
        sys.stdout.flush()
    print(f"{'PASS' if failures == 0 else 'FAIL'}: {failures} failed")
    return failures


if __name__ == "__main__":
    sys.exit(main())

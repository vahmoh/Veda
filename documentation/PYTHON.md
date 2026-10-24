<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Python on Veda

Veda's system image can have Python: CPython 3.15, its interpreter and
standard library, built for Veda from the release's source, as the C
toolchain builds C programs ([C on Veda](C.md)). This document covers
using Python on Veda, building it, how it works, its limits, the port of
CPython, and its tests.

**Contents:** [Using Python](#using-python) · [Building it](#building-it) · [How it works](#how-it-works) · [Limits](#limits) · [The port](#the-port) · [Tests](#tests)

## Using Python

In the Terminal, `python3` (also `python3.15` and `python`) runs a script,
a module (`python3 -m json.tool`) or the interactive interpreter, which
ends at Ctrl+D. A script whose first line names the interpreter
(`#!/system/bin/python3`) is a program of its own. Ctrl+C ends a program,
as it ends any (`$?` is 130).

```bash
echo 'import sys; print(sys.platform, sys.version)' > hello.py
python3 hello.py
```

**The standard library** is all there, but for what Veda cannot carry
out (see [Limits](#limits)): its extension modules are built into the
interpreter, with the libraries they stand on.

| Modules | Built with |
|---------|------------|
| `zlib`, `gzip`, `zipfile` | zlib 1.3.2 |
| `bz2` | bzip2 1.0.8 |
| `lzma` | XZ Utils 5.8.4 (liblzma) |
| `compression.zstd` | Zstandard 1.5.7 |
| `decimal` | mpdecimal 4.0.1 |
| `xml.etree`, `xml.parsers.expat` | Expat 2.9.0 |
| `sqlite3`, `dbm.sqlite3` | SQLite 3.54.1, with full-text search (FTS3, FTS4, FTS5), R*Tree indexes and the mathematical functions |
| `ssl`, `hashlib`, `hmac` | OpenSSL 3.5.9; `ssl` trusts the certificate authorities of `/system/etc/ssl/cert.pem` (Mozilla's list) |

A module that cannot be had says why, in the traceback of its import:

| Module | Why it is not there |
|--------|---------------------|
| `ctypes` | It needs dynamic loading (`dlopen`), which Veda has not yet |
| `curses`, `readline` | The Terminal edits each line itself, and cannot move the cursor yet |
| `dbm.gnu`, `dbm.ndbm` | `dbm.sqlite3` and `dbm.dumb` are there |
| `multiprocessing.shared_memory` | There is no POSIX shared memory |
| `pty` | There are no pseudo-terminals |
| `syslog` | What a program writes to its standard error goes to the system log |
| `tkinter`, `turtledemo`, IDLE (`idlelib`) | There is no Tk |
| `_remote_debugging` (`python3 -m asyncio ps` and `pstree`) | It reads the memory of another process |

**Virtual environments** are copies (Veda's files have no links):
`python3 -m venv ENV` makes one, with pip from the wheel the standard
library carries, and `ENV/bin/pip install` installs packages of pure
Python from PyPI. A package with C extensions installs, but cannot load
them (see [Limits](#limits)). The image's `site-packages` is empty; a
user's packages go in `~/.local/lib/python3.15/site-packages`.

**The interactive interpreter** is the basic one: the Terminal sends a
line once it is edited, and the interpreter reads it.

## Building it

> [!IMPORTANT]
> Python needs the toolchain (`cargo automation toolchain`), whose cross
> compiler builds it.

`cargo automation python` builds Python in `target/python` (`$VEDA_PYTHON`
overrides it) from the ports in `implementation/ports/`, in a few minutes
on a recent PC (`--jobs N`; by default, a job per processor). Every image
built afterwards has it: `cargo automation build`, `run`, `script`...
`cargo automation doctor` says whether it is built, and from the ports as
they are now; an image build says so too, and keeps the Python it has
until `cargo automation python` builds it again.
`implementation/ports/python/build.sh` builds it in stages, each done
again only when what it is made from changed:

1. **The tools for the development machine** its build runs: pkgconf,
   and the same release of Python (CPython's build runs Python to
   generate code and to compile the standard library).
2. **The libraries** its modules stand on, for Veda, static and
   position-independent: zlib, bzip2, XZ Utils, Zstandard, mpdecimal,
   Expat, SQLite (one file, the amalgamation) and OpenSSL.
3. **CPython**, configured for Veda (`--host=x86_64-veda`) with its
   modules built into the interpreter, and the answers of the run-time
   checks a cross build cannot make (`config.site`, beside `build.sh`).
   The build fails if a module Veda must have cannot be built
   (`PYTHON_MODULES`).
4. **The interpreter linked** with the C library as it is now: the POSIX
   layer in it changes more often than Python, and when it does, an image
   build links the interpreter again, without building the rest.
5. **What the image has**: the interpreter, stripped; the standard library
   but for what Veda cannot use, compiled to bytecode once; and the licence
   texts of Python and of the libraries built into it.

The build needs, beside a C compiler, make, patch, tar, xz, gzip, curl,
CMake, Ninja (for XZ Utils and Expat), Perl (for OpenSSL's configuration)
and sed; `cargo automation doctor` lists what is missing. It downloads the
ports' releases into `target/python/downloads`, each checked against the
SHA-256 its `port.toml` pins.

**CPython's own tests** (its `test` package, some 90 MB) are in an image
built with `--python-tests`: `python3 -m test` runs them in the Terminal
([Tests](#tests)).

## How it works

**The interpreter** is a C program on Veda's C library (musl and the
POSIX layer), linked statically and position-independent (`-static-pie`),
with its modules built in: `sys.builtin_module_names` names them, and
`lib-dynload` is empty. Python knows itself as `sys.platform == "veda"`
(`os.name == "posix"`), its platform as `veda-x86_64` (`sysconfig`), and
its C library as musl (`platform.libc_ver()`). It finds its standard
library from its own path, which Veda's loader gives it (`AT_EXECFN`).

**The standard library** is `/system/lib/python3.15`, compiled once at
the default optimisation level: its bytecode is checked against no source
(`unchecked-hash`), as the system image does not change, so importing
compiles nothing. Its build details (PEP 739, `build-details.json`) say
what the image has: no C API and no library to build extensions with.

| Python's | On Veda |
|----------|---------|
| `subprocess`, `os.posix_spawn`, `os.spawn*` | `posix_spawn`, which starts the program in one step, with the descriptors, working directory and environment asked for (`pass_fds`, `close_fds`, `cwd`, `env`); a program that cannot be started fails in the parent (`OSError`), as a forked child's failure is reported there. Veda's processes have no groups or sessions: `start_new_session` and `process_group` change nothing |
| `multiprocessing` | Processes spawned (`spawn`, the one start method), which pipes connect |
| `threading` | The C library's threads, named as Linux names them (15 characters) |
| `os.nice`, `os.setpriority` | The process's nice value, which its threads run at and the programs it starts inherit (below 0 for services alone) |
| `socket`, `ssl`, `asyncio`, `selectors` | The network service's sockets, over IPv4 and IPv6 (a server on every address has an IPv4 socket and an IPv6-only one on one port), Unix domain sockets (with Linux's abstract names), `epoll` |
| `os`, `pathlib`, `shutil`, `tempfile` | The VFS. File names are UTF-8 (a name that is not is refused, `EILSEQ`): Python's file system encoding is UTF-8 whatever the locale |
| `time`, `datetime` | The kernel's clocks; local time is the system's offset from UTC unless `TZ` says otherwise |
| `locale`, standard streams | UTF-8, in the C locale too |
| `signal` | Handlers run for the signals a program raises itself and its threads send each other |

## Limits

Python shares the limits of C programs ([C on Veda](C.md#limits)). In
Python's terms:

- **No extension modules but those built in.** Veda links its programs
  statically and has no `dlopen`: `ctypes` is not there, and a package's
  C extension cannot be loaded (`ImportError`).
- **No `fork` or `exec`.** `os.fork`, `os.exec*` and `os.forkpty` are not
  there; `subprocess` refuses a `preexec_fn` (`OSError`, `EOPNOTSUPP`), as
  no code of the parent's runs in the child. `multiprocessing` spawns, and
  has no locks, queues or pools between processes, which stand on named
  semaphores (`sem_open`), which Veda has not (`ImportError`), nor then
  has `concurrent.futures` a `ProcessPoolExecutor` (`NotImplementedError`);
  its pipes and connections work, as do the executors of threads and of
  interpreters.
- **No shell.** `/system/bin/sh` is not there yet: `shell=True`,
  `os.system` and `os.popen` fail (`FileNotFoundError` for `/bin/sh`).
  The Terminal's commands are its own.
- **Signals are synchronous.** There are no alarms or interval timers
  (`signal.alarm` and `signal.setitimer` are not there). A signal from
  another process runs no handler: `Popen.send_signal(SIGINT)` ends the
  child, without a `KeyboardInterrupt`, as Ctrl+C in the Terminal does. A
  signal waits for a blocking call other than a sleep or a wait (a
  `send` that has no room) to end. A fault ends the program:
  `faulthandler` reports the signals a program raises, not a stack
  overflow.
- **Files have no links, permissions or locks.** `os.link`, `os.symlink`
  and `os.mkfifo` are not there (pathlib's raise
  `UnsupportedOperation`); `os.chmod` changes nothing, a file is the one
  user's (`os.chown` to another fails, `PermissionError`), and
  `fcntl.flock` and `fcntl.lockf` fail (`ENOLCK`). SQLite therefore locks
  no database: a database is one connection's to write at a time. A file
  keeps one time, its modification's, which `st_atime` and `st_ctime`
  give too. Its blocks are all there: a file seeked past its end takes the
  space it skips, and a file holds 2 GiB at most (`OSError`, `ENOSPC`).
- **Files map as copies.** `mmap` maps a file to read (`ACCESS_READ`) or
  as a private copy (`ACCESS_COPY`); a mapping that writes the file
  (`ACCESS_WRITE`, and `ACCESS_DEFAULT`, which is that) fails (`ENODEV`).
  Memory files (`os.memfd_create`) are shared memory.
- **The network** has no out-of-band data (`MSG_OOB`, which
  `ftplib.FTP.abort` sends), no ancillary data on Internet sockets
  (`IPV6_RECVHOPLIMIT` and the like) and no list of interfaces
  (`socket.if_nameindex` is not there).
- **No tz database.** `zoneinfo` knows no zone by name: local time is the
  system's offset from UTC, or what `TZ` says in POSIX's form
  (`<+0530>-5:30`).
- **Resource limits are kept, not enforced.** `resource.setrlimit` keeps
  what it is given for `getrlimit` to read back.
- **Pipes hold 256 KiB**, which `F_SETPIPE_SZ` (and `subprocess`'s
  `pipesize`) cannot change.

## The port

`implementation/ports/python` is the port of CPython, as
[the ports' README](../implementation/ports/README.md) describes ports:
`port.toml` pins the release (`Python-3.15.1.tar.xz`, by its SHA-256),
`veda.patch` changes it, `config.site` answers configure's run-time checks
and `build.sh` builds it. The patch:

- **Makes Veda a platform of CPython's build** (`configure.ac`,
  `configure` generated from it, `config.sub`, `Misc/platform_triplet.c`):
  `x86_64-veda`, whose modules are built in, whose C library's release
  `platform.libc_ver()` gives, and which has none of the functions musl
  has but Veda does not carry out, so that Python leaves out what would
  fail (`fork`, `exec*`, `dlopen`, links, alarms, pseudo-terminals, named
  semaphores, `if_nameindex`...). The modules that cannot be built are
  named, with the reasons their imports give (`MISSING_MODULES` in
  `build.sh`).
- **Starts programs with `posix_spawn`** (`Modules/_posixsubprocess.c`):
  the descriptors, working directory and the rest that a forked child
  would set up are file actions and attributes; what only a forked child
  could run (`preexec_fn`) is refused. `Lib/os.py` has `os.spawn*` start
  programs so too, and no `os.exec*`; `multiprocessing` spawns.
- **Fits the interpreter to Veda**: its own path from the loader
  (`Modules/getpath.c`), UTF-8 as the locale's encoding
  (`Include/pyport.h`), Linux's abstract socket names
  (`Modules/socketmodule.c`), thread names of 15 characters, the basic
  interactive interpreter in Veda's Terminal (`Lib/_pyrepl/main.py`),
  virtual environments as copies (`Lib/venv`), the asyncio shell without
  the tools that read other processes (`Lib/asyncio/__main__.py`).
- **Has CPython's tests skip what Veda lacks** (`Lib/test`), by what they
  need where it can be told: a shell (`support.requires_shell`), a
  `preexec_fn` (`support.requires_preexec_fn`), signals from other
  processes, file locks (`os_helper.skip_unless_file_locks`), permission
  bits (`os_helper.skip_unless_working_chmod`), shared writable mappings,
  symbolic links, ancillary data; and expect what Veda does as it does it
  (UTF-8 in the C locale, names that are not UTF-8 refused, musl's
  behaviour).

## Tests

- **`verification/python/stdlib.py`** checks the standard library inside
  Veda: the interpreter's paths and modules, its bytecode, files,
  threads, processes (`subprocess`, `os.spawn*`, `multiprocessing`),
  signals, time, sockets (TCP and UDP over IPv4 and IPv6, Unix domain,
  ports shared by an IPv4 and an IPv6-only socket), asyncio, TLS, hashes,
  compression, SQLite, data formats, memory, and virtual environments
  with pip; and that what Veda has not fails as this document says.
  `systest` runs it (`/system/tests/python/stdlib.py`), with the host's
  servers.
- **`verification/ui/python.vts`** runs Python in the Terminal: a script
  and its exit status, a script that is a program of its own, the
  interactive interpreter, Ctrl+C, and a virtual environment's pip. It
  says `requires python`, and is skipped where Python is not built.
- **CPython's regression tests** run inside Veda in an image built with
  `--python-tests`: `python3 -m test -j4` in the Terminal, or from the
  kernel's command line, as a script does
  (`boot-cmdline "run=python3:/system/tests/python/regrtest.py,-j4"`, then
  `wait-serial "Tests result:"`), in some four minutes on four processors.
  They all pass: of their 560 test files, 431 run, 120 skip whole (they
  test other systems, Windows' above all, or modules Veda has not: Tk,
  curses, `ctypes`...) and 9 need resources a run is not given by default
  (`-u`: the Internet, a lot of time or disk). Of the 47,842 tests in
  the files that run, 3,761 skip, each saying what it needs: another
  system, or what Veda has not. With every resource but the Internet and
  what Veda has not (`-uall` less `network`, `urlfetch`, `largefile`,
  `extralargefile`, `gui`, `audio`, `tzdata` and `curses`, each taken
  away as `-u-network` takes the first), 432 files run and pass, in some
  six minutes; `test_socketserver`, which asks for the network but stays
  on the machine, passes with `-unetwork`.

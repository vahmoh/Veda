<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Linking and loading

Veda links its programs statically by default, and a program in C, C++
or Rust on the standard library can be linked dynamically instead: it is
then linked, as it starts, with the C library's shared object, which is
the system's dynamic linker too, and it can load shared libraries at run
time. Python's interpreter is such a program: it loads extension modules,
and the libraries `ctypes` calls. This document covers the two kinds of
program, building shared libraries, loading them at run time from each
language, how it works, what keeps it safe, its limits, and its tests.

**Contents:** [Two kinds of program](#two-kinds-of-program) · [Shared libraries](#shared-libraries) · [Loading libraries at run time](#loading-libraries-at-run-time) · [In Rust](#in-rust) · [In Python](#in-python) · [How it works](#how-it-works) · [Security](#security) · [Limits](#limits) · [Tests](#tests)

## Two kinds of program

| | Linked statically | Linked dynamically |
|---|---|---|
| C and C++ (GCC) | By default, at a fixed address; `-static-pie` with `-fPIE`, position-independent | `-pie` with `-fPIE` |
| Rust on the standard library | By default, position-independent (`-static-pie`); `-C relocation-model=static`, at a fixed address | `-C target-feature=-crt-static` |
| Veda's own programs | All of them, but those below | Those that load libraries (`Program::dynamic` in `automation/source/components.rs`) |
| Holds | Its code, the C library's and GCC's runtime's | Its code alone: the C library and GCC's runtime are shared objects in `/system/lib` |
| `dlopen` | Fails, and `dlerror` says why | Loads the library |

A process has one C library, whose state (the heap, the descriptors,
`errno`, the threads, the signals) every library it loads shares: a
library that brought a C library of its own would have another. So the
libraries a program loads stand on the C library's shared object, and only
a program that does too can load them. GCC's runtime is shared alike, as
on Linux distributions: `libgcc_s.so.1`, the unwinder, which every
object's exceptions and panics go through; `libstdc++.so.6`, the C++
library; and `libatomic.so.1`, the atomic operations a processor cannot
do in one instruction, under locks that one process must share.

**The dynamic linker** is musl's, as on Linux distributions built on musl:
the C library's shared object, `/system/lib/libc.so`, with Veda's POSIX
layer in it, is the program interpreter that a dynamically linked program
names (its `PT_INTERP`). It links the program with the libraries it needs
(its `DT_NEEDED`: `libc.so`, and any other), then starts it. It is the
only one Veda starts (`elf::INTERPRETER`).

**Which to choose.** A program that loads nothing is best static: it
starts at once and needs nothing else, as Veda's own do. A dynamically
linked one is much smaller (a `hello` stripped is 14 KB rather than
629 KB in C, or 1.6 MB in C++), and the pages of the libraries it stands
on are the system image's, shared by every dynamically linked program that
runs.

## Shared libraries

A shared library is compiled `-fPIC` and linked `-shared`; a program that
needs it at start is linked with it (`-l`), and one that loads it at run
time is not:

```text
user@veda:~$ cat greet.c
#include <stdio.h>
void greet(const char *who) { printf("Hello, %s!\n", who); }
user@veda:~$ gcc -O2 -fPIC -shared -o libgreet.so greet.c
user@veda:~$ cat hello.c
void greet(const char *who);
int main(void) { greet("Veda"); return 0; }
user@veda:~$ gcc -O2 -fPIE -pie -o hello hello.c -L. -lgreet -Wl,-rpath,'$ORIGIN'
user@veda:~$ ./hello
Hello, Veda!
```

- **What GCC adds.** A dynamically linked program or a shared library is
  linked as Linux's distributions link theirs: no relocations of code
  (`-z text`, which needs `-fPIC` or `-fPIE`), what is relocated
  read-only before any code runs (`-z relro -z now`), the GNU hash table
  of symbols (`--hash-style=gnu`), relative relocations packed
  (`DT_RELR`), and a search path given with `-rpath` as `RUNPATH`. It
  links GCC's runtime shared: `libgcc_s.so.1` if the object calls the
  unwinder (as C++ does, and Rust), `libstdc++.so.6` for C++ (with
  `g++`). `-rdynamic` gives the libraries a program loads what it
  defines, as a plug-in uses its host's functions (Python's extension
  modules do).
- **C++.** The program and every library it loads share the C++ library
  and the unwinder, so C++ works between them as within one: an
  exception goes from a library to the program, or from the program
  through a library's frames, and is counted once
  (`std::uncaught_exceptions`); `std::cout`, the global locale and the
  handlers (`std::set_terminate`) are the process's. A library linked
  with `-static-libstdc++ -static-libgcc` has copies of its own instead,
  whose state is its own: its exceptions must not leave it, as on Linux.
- **Names.** Veda's files have no symbolic links, so a library's file is
  named as programs ask for it: by its soname if it has one, as GCC's
  runtime's are (`/system/lib/libstdc++.so.6`), otherwise by the name it
  was linked by (`libgreet.so`). A library with a versioned soname
  (`-Wl,-soname,libgreet.so.1`) is linked by a script of its unversioned
  name, as `libgcc_s.so` is (`INPUT(libgreet.so.1)` in `libgreet.so`).
- **Where a library is found**, for a name without a `/`, in this order:
  the directories of `LD_LIBRARY_PATH`; those of the `RUNPATH` (or
  `RPATH`) of the object that needs it, then of the object that needed
  that one, up to the program, where `$ORIGIN` is the object's own
  directory (the program's is the path it was started from,
  `AT_EXECFN`); then the system's, `/system/lib`, and the installed
  packages', `/home/packages/lib` ([Packages](PACKAGES.md)). Not the
  working directory. A name with a `/` is a path.
- **What a file needs:** `readelf -d` lists its `NEEDED`, `RUNPATH` and
  soname, and `objdump -T` its dynamic symbols.

## Loading libraries at run time

In C, `<dlfcn.h>` has musl's functions, as on Linux:

```c
#include <dlfcn.h>
#include <stdio.h>

int main(void)
{
	void *h = dlopen("libgreet.so", RTLD_NOW);
	if (!h) {
		printf("%s\n", dlerror());
		return 1;
	}
	void (*greet)(const char *) = (void (*)(const char *))dlsym(h, "greet");
	if (greet)
		greet("plug-in");
	dlclose(h);
	return 0;
}
```

| Call | On Veda |
|------|---------|
| `dlopen(file, mode)` | Loads the library and those it needs, runs their constructors, dependencies first, and gives a handle; the same handle for a library loaded already. There is no lazy binding: `RTLD_NOW` and `RTLD_LAZY` alike bind every symbol as the library loads, and one missing fails the load (for a library linked without `-z now`, which Veda's GCC links with it, `RTLD_LAZY` waits for a library loaded later to bring it). `RTLD_LOCAL`, the default, keeps its symbols to those who ask its handle, `RTLD_GLOBAL` gives them to the libraries loaded later; `RTLD_NOLOAD` gives the handle of a library loaded already, and loads none. `dlopen(NULL, ...)` is the program's handle |
| `dlsym(handle, name)` | The address of a function or of data, or of the calling thread's copy of a thread-local variable; `RTLD_DEFAULT` searches the program and what is global, `RTLD_NEXT` what comes after the caller's object |
| `dlclose(handle)` | Lets the handle go. The library stays loaded until the program ends, as musl has it: nothing that could still point into it is unmapped |
| `dlerror()` | The calling thread's last error, once, in words: `Error loading shared library libgreet.so: No such file or directory`, `Symbol not found: greet` |
| `dladdr(address, info)` | The object an address is in, its path and where it is loaded, and the nearest symbol it exports |
| `dl_iterate_phdr(callback, data)` | Every object loaded: the program, the C library, the libraries, with their program headers and thread-local storage. Unwinders find every object's unwind tables through it |
| `dlinfo(handle, RTLD_DI_LINKMAP, map)` | The object's `struct link_map` |

- **Constructors and destructors.** A library's constructors run as it
  loads, after those of the libraries it needs. As the program ends, the
  handlers given `atexit` run (C++'s destructors of static objects among
  them), then the destructors, of every object in the reverse of the order
  they were constructed in: a library loaded at run time before the
  program.
- **Thread-local storage.** Each thread has a library's, the threads that
  ran before it was loaded too, at its initial values: the dynamic linker
  gives every thread its block as it loads the library (a barrier on the
  processors running them, `membarrier`, orders what they see), and those
  started later have it from their start. A library compiled for the
  initial-exec model (`-ftls-model=initial-exec`) cannot be loaded at run
  time, as on Linux.
- **Threads.** The calls may be made from any thread; one library is
  loaded at a time, and constructors run once.

## In Rust

**A program** that loads libraries is linked dynamically, with the target
feature `crt-static` off, for its own crate and every one it builds (the
standard library's code is the same; how it is linked changes). In a
crate's `.cargo/config.toml`:

```toml
[target.x86_64-unknown-veda]
rustflags = ["-C", "target-feature=-crt-static"]
```

**A library** in Rust is a `cdylib` (`crate-type = ["cdylib"]`), with
functions of C's calling convention (`#[unsafe(no_mangle)] pub extern "C"
fn`), which programs in any language load. It always stands on the
shared objects of the C library and GCC's runtime, whatever `crt-static`
is (Veda's patch of `rustc_session`), and has a standard library of its
own, with its heap (the C library's `malloc`, under `System`): it unwinds
with the process's unwinder, but its panics are its own, and one must not
leave it through an `extern "C"` function, as anywhere.

**Loading** is libloading's, the crate Rust's ecosystem loads plug-ins and
drivers with, which Veda ports (`implementation/ports/libloading`):

```rust
use libloading::{Library, Symbol};

fn main() -> Result<(), libloading::Error> {
    // SAFETY: the library's constructors are trusted to be sound.
    let library = unsafe { Library::new("/home/user/plugins/libgreet.so")? };
    // SAFETY: `greet` has this type.
    let greet: Symbol<unsafe extern "C" fn(*const std::ffi::c_char)> = unsafe { library.get(c"greet")? };
    unsafe { greet(c"Rust".as_ptr()) };
    Ok(())
}
```

The `libc` crate has the C functions themselves (`libc::dlopen`, ...).

**Veda's own programs** are built by `cargo automation build`: one that
loads libraries is a `Program::dynamic`, built in a target directory of
its own (`target/std/dynamic`), as Cargo builds every crate again for
other flags. `verification/plugins` is one.

## In Python

Python's interpreter is a dynamically linked program
([Python on Veda](PYTHON.md)), which gives its C API to what it loads
(`-export-dynamic`).

**`ctypes`** loads libraries and calls their functions, as on Linux, and
`ctypes.util.find_library` finds a library where the dynamic linker would,
by its file's name or its soname: `find_library("m")` is the C library's
shared object, `/system/lib/libc.so`, as musl's libraries are one, and
`find_library("stdc++")` is `/system/lib/libstdc++.so.6`. Its callbacks
into Python are libffi's closures, code made at run time: in a memory file
mapped twice, where it is written and where it runs, never memory both
writable and executable. multiprocessing's shared values and arrays are
`ctypes`' too.

```python
import ctypes, ctypes.util

libc = ctypes.CDLL(ctypes.util.find_library("c"))
print(libc.strlen(b"Veda"))
```

**Extension modules** are shared libraries named
`<module>.cpython-315-x86_64-veda.so` (or `.abi3.so`, for the stable
ABI), compiled with the C API in `/system/include/python3.15`, as GCC
compiles one in Veda's Terminal:

```text
user@veda:~$ gcc -O2 -fPIC -shared -I/system/include/python3.15 -o spam.cpython-315-x86_64-veda.so spam.c
user@veda:~$ python3 -c 'import spam'
```

The standard library's own are built into the interpreter. A module built
for Linux (a wheel for manylinux or musllinux) does not load: it is for
another C library. pip builds a package's C extensions from their source
instead, with Veda's GCC, which Python's configuration (`sysconfig`)
names.

## How it works

**The toolchain** (`cargo automation toolchain`; [the ports](../implementation/ports/README.md#toolchain)):

| Part | What it does |
|------|--------------|
| musl's `libc.so` | The C library and its dynamic linker (`ldso/dynlink.c`), linked by musl's `Makefile` with the POSIX layer's object, as `libc.a` holds it: one object of the layer's, which exports its interface and nothing else, so the layer's state is one per process. The image has it stripped of all but its dynamic symbols |
| GCC's `gcc/config/veda.h` | How each kind of program and library is linked: the start files (`Scrt1.o` for a dynamically linked program, which leaves the bootstrap channel to the dynamic linker), `-dynamic-linker /system/lib/libc.so`, the hardening above, and GCC's runtime, static or shared (`REAL_LIBGCC_SPEC`) |
| GCC's runtime | libgcc, libstdc++ and libatomic, static (compiled `-fPIC`, for position-independent programs) and shared: `libgcc_s.so.1` (libgcc's `t-slibgcc`, versioned as on Linux), `libstdc++.so.6` and `libatomic.so.1` (libtool, which Veda's patch of `libtool.m4` and the scripts generated from it teaches Veda's shared libraries). The unwinder finds every object's tables through `dl_iterate_phdr` (`unwind-dw2-fde-dip.c`, with `TARGET_DL_ITERATE_PHDR`), and the image has the three stripped, by their sonames |
| Rust's target | `dynamic_linking` and `has_rpath` (`spec/base/veda.rs`); `cdylib` and `dylib` crates never take the static C library; the unwinder static (`libgcc_eh`) or shared (`libgcc_s.so.1`), as on Linux (`library/unwind`) |
| CPython | The interpreter linked dynamically, its C API exported; `_ctypes` on libffi, whose closures are memory files mapped twice (its patch); `ctypes.util.find_library` for Veda's names; its configuration (`sysconfig`) naming Veda's own compilers; the tests' extension modules, which must be shared libraries, in images with Python's tests |

**Starting a dynamically linked program.** Programs start in user space
(`runtime::process::Spawn`, [Architecture](ARCHITECTURE.md#user-space)):
the loader maps the program, at a random address, then the dynamic linker,
which whoever starts the program gives it ([`Spawn::dynamic_linker`]: the
POSIX layer's `posix_spawn`, the Terminal, `init`'s launcher and `systest`
read `/system/lib/libc.so`, the system image's pages), at a random address
of its own, clear of the program's. The program's pages and the dynamic
linker's that nothing writes are the files' own, shared by every process
that runs them. The main thread starts at the dynamic linker's entry, the
auxiliary vector saying where the program is (`AT_PHDR`, `AT_ENTRY`) and
where the dynamic linker is (`AT_BASE`). The dynamic linker:

1. relocates itself (`_dlstart`), and saves the bootstrap channel;
2. has the POSIX layer read the startup message, its descriptors, working
   directory and connections to the system's services
   (`__veda_startup`), so that it can open the libraries;
3. loads the libraries the program needs, and theirs, relocates them and
   the program, and lays out the main thread's thread-local storage;
4. starts the program (`_start` in `Scrt1.o`), whose
   `__libc_start_main` has the POSIX layer set up the signals, on the
   thread pointer the main thread keeps (`__veda_init`), then runs every
   object's constructors and `main`.

**Loading a library** (`dlopen`): the dynamic linker opens its file
through the VFS, reserves its span of addresses (inaccessible, at a place
of the process's random region), and maps each segment over it with its
own pages of the file: a library of the system image (`/system/lib`) runs
from the image's pages, shared by every process that loads it; its
writable pages are copies of the process's own, made read-only where they
are relocated (RELRO); its zeros are new memory. It then binds its
symbols, gives every thread its thread-local storage, through a barrier
on the processors running the process's threads (the kernel's
`vm_barrier`, which the POSIX layer's `membarrier` makes), and runs its
constructors.

## Security

- **The system's dynamic linker alone.** The loader starts no program
  that names another (`SpawnError::OtherDynamicLinker`), so a program's
  file cannot have code of its choosing run in its place.
- **No memory both writable and executable.** Libraries with relocations
  of their code (text relocations) are refused (`ENOEXEC`), and GCC makes
  none (`-z text`). Every symbol is bound as a library loads, and what is
  relocated is read-only before its code runs (full RELRO): there is no
  lazy binding, whose tables must stay writable. Code made at run time,
  such as `ctypes`' callbacks, is a memory file's, mapped where it is
  written and elsewhere where it runs.
- **Where code comes from.** By name, a library is found among the
  system's (`/system/lib`, the read-only system image) and then the
  installed packages' (`/home/packages/lib`, which only the packages
  service writes), so a package cannot stand in for a system library;
  never in the working directory.
- **Addresses.** The program, the dynamic linker and each library are at
  random addresses: the program and the dynamic linker each at one of
  2^28 pages, the libraries in the region the kernel draws for each
  process.
- **Sandboxes.** A process that has given up new executable memory
  (`veda_process_restrict`, [C on Veda](C.md#how-it-works)) loads no
  library: `dlopen` fails, `Permission denied`.
- **`LD_LIBRARY_PATH` and `LD_PRELOAD`** are honoured, as on Linux for a
  program that gains nothing by starting (Veda has no setuid): a program
  that a process starts speaks for that process, with what it may do, and
  no more ([Architecture](ARCHITECTURE.md#user-space)).

## Limits

| Area | On Veda |
|------|---------|
| Statically linked programs | Load nothing: `dlopen` fails, `Dynamic loading needs a dynamically linked program: this one is linked statically` |
| Veda's programs on `runtime` alone | Static, as they stand on no C library (the services, `init`, the drivers) |
| Unloading | `dlclose` unloads nothing: a library stays until the program ends |
| `-static-libstdc++`, `-static-libgcc` | A library linked so has its own C++ library and unwinder: its exceptions must not leave it, and its C++ state (`std::cout`, `std::set_terminate`, `std::uncaught_exceptions`) is its own, as on Linux |
| Executable stacks | Never: a library that asks for one (`PT_GNU_STACK`) loads, and the code that would run on its stack faults |
| Symbol versions | Not checked, as in musl: a symbol binds to its default version, whichever version the program was linked with (GCC's runtime libraries have versions, as on Linux). A program built with a later toolchain than the system's may need a symbol its libraries do not have yet: it does not start (`Error relocating ...: symbol not found`) |
| Libraries built for Linux | Do not load: they are for glibc or musl on Linux, not Veda's C library. Python's wheels for Linux are such libraries |
| `ldd`, `ldconfig` | None: `readelf -d` says what a file needs |

## Tests

- **`verification/c/libraries.c`**, built statically (at a fixed address
  and position-independent) and dynamically, as the other C test programs
  are: statically, that loading fails as it says; dynamically, the dynamic
  linker and the C library where the program says, loading by path, a
  dependency found through `$ORIGIN`, constructors in order, functions and
  data, `RTLD_LOCAL`, `RTLD_GLOBAL` and `RTLD_NOLOAD`, `dladdr` and
  `dl_iterate_phdr`, a library's thread-local storage in threads started
  before and after it was loaded, a library that uses the program's
  symbols, the errors, destructors at the program's end, `LD_LIBRARY_PATH`,
  a sandboxed program that loads nothing, and a Rust library from C. Its
  libraries are `verification/c/shared` (`/system/tests/c/lib`).
- **`verification/c/posix.c`** and **`cxx.cc`** run dynamically linked
  too; `cxx.cc` throws exceptions between itself and a C++ library
  (`verification/c/shared/thrower.cc`), each way and through each other's
  frames, in threads too, with the C++ library they share
  (`/system/lib/libstdc++.so.6`) counting each once. `libraries.c` loads
  that library from C, which brings the C++ library in, and has it throw
  and catch in a thread that began before.
- **`verification/rust/loading.rs`**, built in the three ways the Rust
  test programs are: a Rust library (`verification/rust/shared`, a
  `cdylib`) and a C one, the library's thread-locals, panics and
  backtraces beside them; **`stdlib.rs`** runs dynamically linked too.
- **`verification/plugins`**, a `Program::dynamic`: libloading, as Veda's
  port has it.
- **`verification/python/stdlib.py`**: `ctypes` (finding and loading
  libraries, callbacks into Python, multiprocessing's shared values) and an
  extension module that Veda's GCC compiles and Python imports.
- **`systest`** runs them all ([Testing](ARCHITECTURE.md#testing)), and
  checks the kernel's barrier with a thread busy on every processor.
- **Unit tests** of `elf` on the host: the dynamic linker's path read and
  checked (`PT_INTERP`), and malformed ones refused.

[`Spawn::dynamic_linker`]: ../implementation/libraries/runtime/source/process.rs

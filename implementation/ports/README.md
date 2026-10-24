<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Ports

Third-party software that Veda builds from source. Veda's own code is
Rust; C code, Rust itself and any crate Veda changes come as ports: the
upstream release, pinned by its SHA-256 and changed only by a small,
readable and explained patch. The fonts Veda ships are here too, as
released, and the data the web engine reads (more fonts, the certificate
authorities TLS trusts), downloaded as released.

**Contents:** [How ports are built](#how-ports-are-built) · [Toolchain](#toolchain) · [Driver VM](#driver-vm) · [The web engine](#the-web-engine) · [Python](#python) · [Crates](#crates) · [Fonts](#fonts) · [Updating a port](#updating-a-port) · [Licences](#licences)

## How ports are built

Each port is a directory:

| File | What it holds |
|------|---------------|
| `port.toml` | The name, version, download address, the archive's SHA-256 and the licence; `archive`, the name the download is kept under where its address's last part will not do (GitHub's archives of tags and commits); for a port of several files (`files`), the file that lists each one's SHA-256 and path under the address |
| `veda.patch` | Veda's changes, applied with `patch -p1` to the unpacked archive |
| `architectures/<architecture>/` | The architecture's `veda.patch`, applied after the port's, and its other files, such as the driver VM kernel's options |

The `cargo automation` command that builds a port downloads its archive
first, refusing one whose SHA-256 differs from the pin. For Rust it also
fetches its stage 0 (the compiler, standard library and Cargo of the
release that builds it), checked against the SHA-256s in the release's
`src/stage0`.

The scripts (`build.sh`, `linux/build.sh`, `wpewebkit/build.sh`,
`python/build.sh`, `crates.sh`; `common.sh` holds what they share) list
what they expect and
make in their opening comments. They build for the architecture
`cargo automation` names (`VEDA_ARCH`), with its settings from
`architectures/<architecture>/settings.sh` (target triples, the names
other projects give the architecture, the media libraries' assembler). A
stage runs again only when its inputs change: its commands, the contents
of the files and ports it uses (not where the checkout is), the stages
before it, and the variables it reads, its build directory among them.

## Toolchain

`cargo automation toolchain` builds these with `build.sh`, in
`target/toolchain`, as [C on Veda](../../documentation/C.md#the-toolchain)
and [Rust on Veda](../../documentation/RUST.md#the-toolchain) describe.
Its last stage gathers their licence texts ([Licences](#licences)), and
those of the crates Rust's standard library for Veda is built from, which
the release vendors.

| Port | Version | Licence | What Veda uses it for |
|------|---------|---------|-----------------------|
| `binutils` | 2.47 | GPL-3.0-or-later | Assembler, linker, object file tools |
| `gcc` | 16.2.0 | GPL-3.0-or-later WITH GCC-exception-3.1 | C and C++ (cross), C (native) |
| `gmp`, `mpfr`, `mpc` | 6.3.0, 4.2.2, 1.4.1 | GMP LGPL-3.0-or-later OR GPL-2.0-or-later; MPFR and MPC LGPL-3.0-or-later | The native GCC's libraries |
| `musl` | 1.2.6 | MIT | The C library |
| `rust` | 1.99.0 | MIT OR Apache-2.0; its LLVM Apache-2.0 WITH LLVM-exception | Compiler, standard library for Veda's target, Clippy |
| `dav1d` | 1.5.4 | BSD-2-Clause | AV1, in FFmpeg |
| `ffmpeg` | 9.0.2 | LGPL-2.1-or-later | `mediadec`, the sandboxed decoder of Videos |

And the data the C library reads, which the image installs with the
toolchain (`automation/source/databases.rs`, which writes the user
database, `passwd` and `group`, beside it):

| Port | Version | Licence | What it is |
|------|---------|---------|------------|
| `iana-etc` | 20260930 | CC0-1.0 | IANA's registry of service names and port numbers, as iana-etc converts it (`/system/etc/services`) |

What the patches change:

* **binutils**: `veda` in `config.sub`; on AMD64, `x86_64-veda` in BFD
  (`x86_64_elf64_vec`), gas and ld (`elf_x86_64`, with code on pages of its
  own, as on Linux).
* **gcc**: Veda's target in `config.sub`, `gcc/config.gcc`,
  `libgcc/config.host`, `gcc/config/veda.h` (`/system/lib`, static
  linking, `rcrt1.o` for `-static-pie`, musl's types) and `veda.opt`
  (`-pthread`, `-rdynamic`, `-posix`); no `fixincludes` for musl's
  headers; libstdc++ checks Veda's C library as Linux's when cross
  compiled; C++ sees the C library's GNU extensions (`_GNU_SOURCE`), as on
  GNU/Linux; libatomic is built, as for any POSIX system. On AMD64,
  `gcc/config/i386/veda.h`: the System V x86-64 ABI, the stack
  protector's canary where musl keeps it.
* **gmp**, **mpfr**, **mpc**: `config.sub` knows `veda`.
* **musl**: `__init_libc` sets the POSIX layer up, `posix_spawn` asks it to
  start the program, `dl_iterate_phdr` names the program by `AT_EXECFN`
  (there is no `/proc/self/exe`), the databases Linux keeps in `/etc`
  (`hosts`, `services`, `passwd`, `group`, `shadow`, `shells`) are in
  `/system/etc`, names not in its `hosts` go to the network service's
  resolver (`__lookup_name`), musl's DNS client to its DNS servers
  (`__get_resolv_conf`), `pthread_getattr_np` takes the main thread's
  stack from the layer (`__veda_main_stack`; there is no `mremap` to probe
  it with), and with no `TZ` the time zone is the system's offset from UTC
  (`__tz.c`, from the layer's `__veda_tz`). On AMD64, system calls,
  `_start` (the bootstrap channel) and the thread routines
  (`src/thread/x86_64/*.s`) go through the layer; `vfork` takes the
  generic version, which fails: there is no `fork`. See
  [C on Veda](../../documentation/C.md#how-it-works).
* **rust**: Veda's target in the compiler (`Os::Veda`,
  `spec/base/veda.rs`; on AMD64 `x86_64-unknown-veda`, linked by
  `x86_64-veda-gcc`, in `STAGE0_MISSING_TARGETS`) and in `library/std`,
  which takes the code of the systems whose C library has what Veda's has;
  Linux's definitions on musl in the vendored `libc` (`vendor/libc-*`;
  `build.sh` updates its `.cargo-checksum.json`); GCC's unwinder in
  `library/unwind`, shared with C++; `AT_EXECFN` for `library/backtrace`.
  See [Rust on Veda](../../documentation/RUST.md#how-it-works).
* **ffmpeg**: `veda` as a `--target-os` in `configure`. **dav1d**: none.

dav1d (8- and 16-bit) and FFmpeg are built static, with the architecture's
assembler (nasm on AMD64) and their `checkasm`, which `systest` runs on
Veda, into `target/toolchain/media`, as only `mediadec` links them. FFmpeg
has only the demuxers, decoders (AV1 through dav1d), parsers, bitstream
filter, scaler and resampler that `mediadec` uses. It stays LGPL:
`build.sh` checks that `config.h` has `CONFIG_GPL 0` and
`CONFIG_NONFREE 0`, and that the assembly, threads and dav1d were found.

## Driver VM

`cargo automation linux` builds these with `linux/build.sh`, in
`target/linux`, for the [driver VM](../../documentation/DRIVERVM.md), and
gathers the kernel's and Mesa's licence texts ([Licences](#licences)).

| Port | Version | Licence | What Veda uses it for |
|------|---------|---------|-----------------------|
| `linux` | 7.2.9 | GPL-2.0-only | The driver VM's kernel |
| `mesa` | 26.2.4 | MIT (the parts the renderer uses) | OpenGL ES in the renderer: Gallium, NIR, Intel's shader compiler |
| `zlib` | 1.3.2 | Zlib | Compression, for libelf |
| `elfutils` | 0.196 | LGPL-3.0-or-later OR GPL-2.0-or-later | libelf for objtool, the kernel's build tool, so the build machine needs none |

What the patches change:

* **linux**: the Veda platform (`veda.patch`, and
  `architectures/amd64/veda.patch`), whose files
  [The pieces](../../documentation/DRIVERVM.md#the-pieces) lists. The
  kernel is `allnoconfig` with the options of `veda.config` and
  `architectures/<architecture>/veda.config`, and the build fails if one
  does not take. `firmware.txt` and
  `architectures/<architecture>/firmware.txt` list the firmware it loads,
  with each file's source, SHA-256 and licence, and the sources' licences
  (`licence` lines, each text pinned by its SHA-256 as the files are).
* **zlib**, **elfutils**: none; built static for the build machine.
* **mesa**: Veda's renderer (`implementation/guests/linux/renderer`) is a
  target of Mesa's build (`src/gallium/targets/veda`): the guest's program
  around its Rust half (`veda-renderer-lib`), and `vgallium.so` for the
  OpenGL ES tests on the build machine (softpipe, virgl over
  virglrenderer's test server, iris). Gallium is built without an OpenGL
  API (`with_gfx_compute`), with the renderer's small libdrm; the static
  program does without Mesa's build id and links Gallium's stand-ins for
  video. iris is built without what needs Mesa's OpenCL compiler (LLVM):
  indirect draws generated on the GPU, and blorp's indirect copies (its
  bounds check is in NIR instead). `linux/build.sh` configures Mesa
  (Meson) with the guest's toolchain, built from the same binutils, GCC
  and musl releases without Veda's patches; `cargo automation` builds
  the renderer (Ninja) with the system.

## The web engine

`cargo automation web` builds these with `wpewebkit/build.sh`, in
`target/web`, for [the browser](../../documentation/BROWSER.md): the
libraries for Veda, static and position-independent, each compiled with
the same flags (`VEDA_CFLAGS`: stack protection, a section per function
for the link to drop what it does not use), and the tools their builds
run, for the build machine, built from their releases without Veda's
patches. Its last stage gathers the libraries' licence texts
(`LICENCE_FILES`), which the image installs in `/system/licences/web`:
a library added without them fails the build.

| Port | Version | Licence | What Veda uses it for |
|------|---------|---------|-----------------------|
| `wpewebkit` | 2.54.1 | LGPL-2.1-or-later AND BSD-2-Clause | The web engine: HTML, CSS, JavaScript, the network process |
| `glib` | 2.90.1 | LGPL-2.1-or-later | GLib, GObject and GIO: the event loop, objects and I/O the engine stands on |
| `glib-networking` | 2.90.0 | LGPL-2.1-or-later | GIO's TLS, on OpenSSL, and proxies from the environment |
| `libsoup` | 3.8.0 | LGPL-2.0-or-later | HTTP/1.1 and HTTP/2, cookies, caches, WebSockets |
| `openssl` | 3.5.9 | Apache-2.0 | TLS, under glib-networking ([why not rustls](../../documentation/DEPENDENCIES.md#c-and-c-libraries)) |
| `nghttp2`, `brotli`, `libpsl` | 1.70.0, 1.2.0, 0.23.3 | MIT | HTTP/2's framing; Brotli (pages, WOFF2 fonts); the Public Suffix List |
| `icu` | 78.3 | Unicode-3.0 | Unicode, text breaking, collation, locales and `Intl` |
| `harfbuzz` | 14.6.0 | MIT-Modern-Variant | Text shaping |
| `freetype` | 2.15.0 | FTL OR GPL-2.0-or-later | Font files and their glyphs |
| `fontconfig` | 2.18.3 | HPND-sell-variant | Which font serves a family and the characters a page asks for (`fonts.conf`, Veda's configuration) |
| `expat` | 2.9.0 | MIT | Skia's SVG glyphs |
| `libxml2`, `libxslt` | 2.15.4, 1.1.45 | MIT | XML documents and XSLT; fontconfig's configuration |
| `sqlite` | 3.54.1 | blessing | The engine's databases: IndexedDB, local storage, cookies, HSTS |
| `libpng`, `libjpeg-turbo`, `libwebp`, `lcms2` | 1.6.59, 3.2.0, 1.6.0, 2.19.1 | libpng-2.0; IJG AND BSD-3-Clause AND Zlib; BSD-3-Clause; MIT | Pictures and their colour profiles |
| `libgcrypt`, `libgpg-error`, `libtasn1` | 1.12.4, 1.61, 4.21.0 | LGPL-2.1-or-later | The Web Crypto API and digests; ASN.1 for its keys |
| `libepoxy`, `egl-registry` | 1.5.10, a commit | MIT; Apache-2.0 AND MIT | OpenGL ES and EGL's entry points, and EGL's headers |
| `libxkbcommon` | 1.13.2 | MIT | Key symbols, which WPE's platform names keys with |
| `libffi`, `pcre2`, `zlib` | 3.8.0, 10.49, 1.3.2 | MIT; BSD-3-Clause WITH PCRE2-exception; Zlib | GLib's closures and regular expressions; compression |
| `gstreamer`, `gst-plugins-base`, `gst-plugins-good`, `gst-plugins-bad`, `gst-libav` | 1.28.8 | LGPL-2.1-or-later | Media: `<video>`, `<audio>`, Media Source Extensions and Web Audio. GStreamer's core and libraries, and only the plugins the engine uses: playback, finding types, converting and resampling, GIO's streams; the MP4, Matroska (WebM), Ogg and WAV demuxers, the parsers, the automatic audio sink, splitting channels, tempo, WebVTT's encoder, the video sink of players of sound alone; the decoders on FFmpeg |
| `ffmpeg`, `dav1d` | 9.0.2, 1.5.4 | LGPL-2.1-or-later; BSD-2-Clause | The web's codecs, through gst-libav: AV1 (dav1d), VP9, VP8, H.264, Opus, Vorbis, AAC, MP3 and FLAC. The ports of Videos' decoder ([above](#toolchain)), built again for the web |
| `libogg`, `opus` | 1.3.6, 1.6.1 | BSD-3-Clause | The Ogg container; Opus's parser, for Media Source Extensions |
| `meson`, `pkgconf`, `gperf`, `ruby`, `libyaml` | 1.12.1, 3.0.9, 3.3, 3.4.11, 0.2.5 | Apache-2.0; ISC; GPL-3.0-or-later; Ruby OR BSD-2-Clause; MIT | For the build machine: the libraries' builds, finding them, WebKit's generated tables, JavaScriptCore's interpreter and code generators (Ruby, whose YAML is libyaml's) |

GStreamer is static (`gstreamer-full`, as GStreamer's own build makes it
for static programs): its core is built so that `gst_init` calls the
program's `gst_init_static_plugins`; `build.sh` names the plugins
(`GST_PLUGINS`) and makes `libgstreamer-full-1.0.a`, whose
`gst_full_register_plugins` registers them, and `gstreamer-full-1.0.pc`,
with which WebKit is built (`USE_GSTREAMER_FULL`) and the browser links
them and the libraries they need ([Media](../../documentation/BROWSER.md#media)).
No GPL code comes in: gst-plugins-bad is built without its GPL plugins,
and FFmpeg as Videos' is, LGPL (`build.sh` checks `CONFIG_GPL 0` and
`CONFIG_NONFREE 0`), with only the decoders and parsers of the web's
codecs, libavformat and libavfilter without their demuxers or filters
(gst-libav links them), its compiler's flags out of the configuration it
records. WebKit is built again for a change of GStreamer's headers or its
libraries' pkg-config files (`GST_HEADERS`), not of its plugins.

And the data the engine reads, which the image installs:

| Port | Version | Licence | What it is |
|------|---------|---------|------------|
| `ca-certificates` | 2026-09-25 | MPL-2.0 | Mozilla's certificate authorities, as curl extracts them (`/system/etc/ssl/cert.pem`) |
| `noto-fonts` | 2026.10.01 | OFL-1.1 | Noto's serif, and the sans-serif faces of the scripts Veda's own fonts lack: a set of files (`files.sha256`) from the project's monthly release |
| `noto-cjk` | 2.004 | OFL-1.1 | Noto Sans CJK, every weight in one variable collection |
| `noto-emoji` | 2.057 | OFL-1.1 | Noto Color Emoji (COLRv1), of Unicode 18.0 |

What the patches change:

* **wpewebkit**: Veda as a platform (`OS(VEDA)`), most of it taking
  Linux's way where Veda has Linux's interface: memory (bmalloc, libpas),
  threads (stacks, names, ids, suspension through `<veda/thread.h>`, the
  machine context), JavaScriptCore (the DFG's polled traps, WebAssembly's
  bounds checks), shared memory as memory files, memory figures from
  `<veda/memory.h>` and `sysinfo`, the program's path, `navigator.platform`;
  the engine's processes as the program that embeds it, started again
  (`Source/WebKit/Shared/veda`); the web process's sandbox, which Linux's
  ports have of bubblewrap and seccomp (off): as it starts, before any
  page, a web process gives up, for good, all of the file system but the
  system image, fontconfig's cache and the devices, every service but those
  the embedder's code in it reaches (`WEBKIT_VEDA_WEB_PROCESS_SERVICES`),
  and creating processes, reading the kernel log and making memory
  executable, with `<veda/sandbox.h>`, then checks that they are gone
  (`WebProcess::initializeSandbox`, `Source/WebKit/WebProcess/veda`); the
  network process's, which Linux's ports have none of: it gives up all of
  the file system but the system image, the devices and the places of the
  user's files the embedder names (`WEBKIT_VEDA_NETWORK_PROCESS_FILES`:
  the engine's data, cache, downloads and temporary files), every service
  but the network's, and the same of the kernel's, then checks that they
  are gone (`NetworkProcess::initializeSandbox`,
  `Source/WebKit/NetworkProcess/veda`); pages from the user's files, which
  the network process reads: the embedder's loads lend it the page's
  folder, to read, through the file system's loans (`veda_lend_files` in
  the UI process, a new message `NetworkProcess::BorrowFiles`,
  `veda_borrow_files`), where Cocoa's ports give a sandbox extension, and
  the UI process takes it back (`veda_take_back_files`) once no page open
  needs it (none shows, in any of its frames, loads or has in its history
  a page from it, as its tabs close and their histories move on), and has
  WebKit forget it for every web process (another message,
  `NetworkProcess::ForgetFiles`,
  and in what the UI process assumes each may read, so that a page's
  navigation there fails as one outside the folders lent for it does),
  lending it again as the embedder loads, reloads or goes back to such a
  page; no
  "universal file read" (a page's link or form to a file outside the
  folders lent for it, into its window or a new one, fails, saying so,
  rather than its process being ended for the address; a web process
  ended for an invalid message is logged), and a web process has the
  network process read only the places lent for its page, allowed it
  before it has shown anything, a page from the user's files going to a
  new process where its tab's was not allowed its folder
  (`Source/WebKit/UIProcess/veda/FileLoansVeda.cpp`,
  `NetworkFileLoansVeda.cpp`), and without WebCore's check that such a
  page shows only files of its volume, which a sandboxed web process
  cannot make (as under bubblewrap); the types of the web's files by their
  extensions, there being no shared MIME database for xdgmime to read
  (`MIMETypeRegistryXdg.cpp`); a static build, with xdgmime's
  names kept apart from GIO's copy; WPE's hardware acceleration policy, as
  GTK's (pages painted on the CPU, or composited, as the browser chooses);
  no injected bundle; damage that covers an antialiased layer's edges
  (the fringe a rotating element's corners left behind); a frame read
  back from the GPU only where it changed since its buffer was last drawn,
  which is all the compositor draws of it (`AcceleratedSurface.cpp`);
  each shared buffer's memory file kept on its WPE buffer in the UI process
  (`wpe-veda-memory-file`), which the embedder hands to Veda's window
  system, as the page's subsurface shows it from there
  (`AcceleratedBackingStore.cpp`); ANGLE, for WebGL, on Veda as on Linux
  (`platform.h`), with musl's mutex where it would use Linux's futexes
  directly, its OpenGL back end finding Veda's EGL and OpenGL ES through
  the program's `eglGetProcAddress` rather than in a library it opens
  (`FunctionsEGLDL.cpp`), and without its exported OpenGL ES names, which
  would clash with Veda's (WebKit calls ANGLE's by its own); the web
  process made the first to go when memory runs out, as it starts
  (`veda_process_set_memory_class`, in `AuxiliaryProcessMainVeda.cpp`), and
  one the kernel ended for memory reported as having used too much of it,
  not as crashed (`veda_child_ended_for_memory`, in
  `WebProcessProxy::didClose`); and WPE's reduced motion read as its
  setting says, where WebKit 2.54.1 tells every page the opposite, that
  the user prefers reduced motion (`SystemSettingsManagerProxyWPE.cpp`):
  WebKit's own fix ([bug 325212](https://bugs.webkit.org/show_bug.cgi?id=325212)),
  which the patch drops once a release has it. See
  [WebKit on Veda](../../documentation/BROWSER.md#webkit-on-veda).
* **icu**: Veda as a Linux-like platform in its build, and the system's
  time zone named by the tz database's zone of its offset
  (`uprv_tzname`).
* **glib**: `g_unix_fd_query_path` says it is not supported (Veda's
  descriptors do not know their paths); its libatomic found as the static
  library it is.
* **glib-networking**: its TLS and proxy modules built as static
  libraries, which the program registers (`g_io_openssl_load`,
  `g_io_environmentproxy_load`), there being no modules to load.
* **libepoxy**: with no `dlopen`, it finds every function through the
  program's own `eglGetProcAddress` (the `egl` crate's), to which it
  refers weakly.
* **openssl**: Veda's configuration (`Configurations/50-veda.conf`):
  threads, no shared libraries or modules.
* **fontconfig**: Veda's `statfs` is Linux's (`fcstat.c`); `fonts.conf`,
  beside the port, is Veda's configuration: the fonts in `/system/fonts`,
  the generic families and the languages' CJK faces, and the cache in
  `/tmp/fontconfig`, which the browser makes and its web processes, which
  cannot write there, only read.
* **libxml2**: no test module where there are no shared libraries.
* **gst-libav**: FFmpeg's dav1d decoder (`avdec_libdav1d`) is
  registered, which gst-libav leaves out as one of an external library's:
  Veda has no other AV1 decoder for GStreamer (gst-plugins-rs's `dav1ddec`
  is built apart, with Cargo), and Videos decodes AV1 so too. A frame that
  a picture comes out for is shown: gst-libav took one that FFmpeg asked
  no buffer for (`get_buffer2`) to be decoded only, and dav1d's decoder
  allocates its pictures itself.
* **libffi**: Veda's ELF in its assembly; **libgcrypt**, **libgpg-error**,
  **libtasn1**, **icu**: `config.sub` knows Veda; **libgpg-error**: the
  lock type of musl's threads (`lock-obj-pub.x86_64-pc-veda.h`).
* The others: none.

## Python

`cargo automation python` builds these with `python/build.sh`, in
`target/python`, for [Python on Veda](../../documentation/PYTHON.md): the
libraries its modules stand on, for Veda, static and position-independent,
compiled as the web engine's are (`VEDA_CFLAGS`), then CPython, its
modules built into the interpreter; and, for the build machine, pkgconf and
the same release of Python, which CPython's build runs. The ports it shares
with the web engine are built for each apart. Its last stage gathers the
licence texts of Python and of the libraries built into it
(`LICENCE_FILES`), and those of pip and the packages it vendors, from the
wheel `ensurepip` carries, which the image installs in
`/system/licences/python`.

| Port | Version | Licence | What Veda uses it for |
|------|---------|---------|-----------------------|
| `python` | 3.15.1 | PSF-2.0 | CPython: the interpreter and its standard library |
| `zlib`, `bzip2`, `xz`, `zstd` | 1.3.2, 1.0.8, 5.8.4, 1.5.7 | Zlib; bzip2-1.0.6; 0BSD; BSD-3-Clause OR GPL-2.0-only | `zlib`, `bz2`, `lzma` (liblzma) and `compression.zstd` |
| `mpdecimal` | 4.0.1 | BSD-2-Clause | `decimal` |
| `expat` | 2.9.0 | MIT | `xml.etree`, `xml.parsers.expat` |
| `sqlite` | 3.54.1 | blessing | `sqlite3`, `dbm.sqlite3` |
| `openssl` | 3.5.9 | Apache-2.0 | `ssl`, `hashlib`, `hmac` |
| `pkgconf` | 3.0.9 | ISC | For the build machine: finding the libraries |

What the patch changes:

* **python**: Veda as a platform of CPython's build (`configure.ac`, and
  `configure`, generated from it by Autoconf 2.72; `config.sub`,
  `Misc/platform_triplet.c`): `x86_64-veda`, its modules built into the
  interpreter, without the functions musl has that Veda does not carry
  out (`fork`, `exec*`, `dlopen`, links, alarms, pseudo-terminals,
  `if_nameindex`); programs started with `posix_spawn`
  (`Modules/_posixsubprocess.c`, `Lib/os.py`'s `spawn*`, `multiprocessing`
  spawning alone); the interpreter's own path from the loader
  (`Modules/getpath.c`), UTF-8 as the locale's encoding
  (`Include/pyport.h`), Linux's abstract socket names
  (`Modules/socketmodule.c`), the thread names' limit, the basic
  interactive interpreter in Veda's Terminal (`Lib/_pyrepl/main.py`),
  virtual environments as copies (`Lib/venv`), the asyncio shell without
  the tools that read other processes' memory, `multiprocessing`'s heap
  in `/dev/shm` (`Lib/multiprocessing/heap.py`); and CPython's tests,
  which skip what Veda has not by what they need
  ([Python on Veda](../../documentation/PYTHON.md#the-port)).
  `config.site` answers configure's run-time checks.
* **sqlite**: none; built from the amalgamation, with its unix-none VFS,
  which locks no files (Veda's file system cannot arbitrate locks between
  processes yet).
* The others: none.

## Crates

Rust crates that Veda changes. The releases of all but smoltcp and
zune-jpeg do not know Veda; their ports let the crates on top of them run
in Veda's programs on the standard library.

| Port | Version | Licence | What Veda uses it for |
|------|---------|---------|-----------------------|
| `smoltcp` | 0.14.0 | 0BSD | `netd`'s TCP/IP stack (`netstack`) |
| `zune-jpeg` | 0.5.15 | MIT OR Apache-2.0 OR Zlib | `image`'s JPEG decoder |
| `libc` | 0.2.190 | MIT OR Apache-2.0 | Bindings of the C library |
| `getrandom`, `getrandom-0.3` | 0.4.3, 0.3.4 | MIT OR Apache-2.0 | Random bytes; 0.3 for crates still on it (`rand` 0.9) |
| `rustix` | 1.1.5 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | POSIX and Linux system calls |
| `errno` | 0.3.14 | MIT OR Apache-2.0 | The C library's `errno` |
| `socket2` | 0.6.5 | MIT OR Apache-2.0 | Sockets, under `tokio` |
| `mio` | 1.2.4 | MIT | The event loop under `tokio` |
| `tokio` | 1.53.3 | MIT | The asynchronous runtime |

A crate's release is its `.crate` archive on crates.io, pinned by
crates.io's checksum. The workspace depends on its version, directly
(`version = "=0.14.0"` for smoltcp in `Cargo.toml`) or through another
crate, and Cargo builds the port instead:

1. Every `cargo automation` command first prepares the crates
   (`automation/source/crates.rs`): it downloads and checks a changed
   port's release, and `crates.sh` unpacks and patches it in
   `implementation/ports/.crates/src/<crate>`, which Git ignores.
2. It writes `implementation/ports/.crates/config.toml`, a
   `[patch.crates-io]` that `.cargo/config.toml` includes.
3. It moves back to its port's version any crate that Cargo has moved to
   a newer release, and checks that `Cargo.lock` has the prepared crates.

Without the patch, as in a new checkout until step 2 (Cargo first runs
for `cargo automation` itself), Cargo drops the ports' entries from
`Cargo.lock` and takes each crate's newest release that the requirements
on it allow. A crate the workspace depends on directly keeps the `=`
before its port's version in `Cargo.toml`, which holds it there; step 3
is for the others (errno, socket2, mio), which the workspace reaches only
through other crates.

When the workspace has two releases of a crate, the older one's port is
named for its release (`getrandom-0.3`), and its entry in the patch names
the crate as its `package`. The prepared crates are kept outside
`target/` and `cargo clean`, as Cargo reads them before it builds
anything; removing `implementation/ports/.crates` has them prepared anew.

> [!IMPORTANT]
> Until the crates are prepared, Cargo resolves their releases as
> published, and `Cargo.lock` shows them so. In a new checkout, run a
> `cargo automation` command (`cargo automation doctor`) before
> Cargo itself (`cargo test -p ...`, rust-analyzer).

A crate's own tests run outside the workspace, from the repository's
root:

```bash
cargo test --lib --manifest-path implementation/ports/.crates/src/smoltcp/Cargo.toml --target-dir target/ports
```

What the patches change:

* **smoltcp**: eight changes, each with tests of its own.
  * **Unpredictable protocol numbers** (`src/rand.rs`): given
    `Config::random_key`, initial sequence numbers, DHCP and DNS ids, IPv4
    identification and source ports come from ChaCha20, not the
    predictable sPCG32 (RFC 6528); `netd` keys it from the kernel's
    generator.
  * **Quiet connections** (`src/socket/tcp.rs`): the timeout runs only
    while a SYN, data or a FIN of ours awaits acknowledgement or
    keep-alive is on (`awaits_ack`), so idle connections stay up;
    `has_timed_out` tells a timeout from a reset.
  * **Windows that end short** (`src/socket/tcp.rs`): data up to the
    furthest edge advertised (`remote_max_edge`) is accepted, as RFC 7323
    (2.4) has it, even when a rounded-down window ends short of it.
  * **The lease** (`src/socket/dhcpv4.rs`): `lease_expires_at` gives the
    end of the lease the client accepted; `netd` had read leases from any
    acknowledgement on the network.
  * **Stable privacy addresses** (`src/iface/interface`, `src/rand.rs`):
    given `Config::slaac_secret`, autoconfigured addresses get RFC 7217
    identifiers, not EUI-64 ones, which carry the hardware address between
    networks; `Interface::ipv6_address_in` forms `netd`'s link-local
    addresses.
  * **Errors for ping** (`src/socket/icmp.rs`, `src/wire/icmpv4.rs`): ICMP
    sockets receive the Destination Unreachable and Time Exceeded messages
    quoting their echo requests, and ICMPv4 errors quoting only a packet's
    start (RFC 792) are parsed.
  * **Neighbours found at once** (`src/iface/neighbor.rs`): ARP requests
    and Neighbor Solicitations are rate-limited per address (RFC 1122,
    2.3.2.1), not for the whole cache.
  * **Addresses matched as on Linux** (`src/socket/tcp.rs`,
    `src/socket/udp.rs`): a socket bound to the unspecified address of one
    IP version (`0.0.0.0`, `::`) takes the packets to every address of
    that version and no other, as an IPv4 socket and an IPv6-only one
    (`IPV6_V6ONLY`) do, and a UDP one sends from the address of its route;
    a UDP socket bound to one address takes what comes to that address
    alone. Released, a socket bound to an unspecified address took nothing
    sent to the host's own addresses, and sent from the unspecified
    address; one bound to an address took every broadcast and multicast on
    its port, of either version.
* **zune-jpeg**: sequential JPEGs with a scan per component (T.81, A.2.2),
  as jpeg-encoder writes baseline files with optimised Huffman tables,
  decode instead of coming out as stripes (`single_component_scan_rows` in
  `src/mcu.rs`, tested by `image`'s `jpegs_with_a_scan_per_component`),
  and `src/lib.rs` loses a `#![macro_use]` that rustc warns will become an
  error. Left as released, as `image` does not use it: CMYK and YCCK
  pictures converted to RGBA or BGRA come out three bytes a pixel
  (`src/worker.rs`).
* **libc**: Veda takes Linux's definitions on musl, as in Rust's vendored
  copy: wherever the release names Linux beside other systems, it names
  Veda too; `build.rs` counts Veda among the targets with only a recent
  musl and in `check-cfg`.
* **getrandom**, **getrandom-0.3**: `src/backends.rs` calls the C library's
  `getrandom()`, which the POSIX layer always provides, without a fallback,
  reading errno as on Linux; libc's deprecated `INT_MAX` becomes
  `c_int::MAX`.
* **rustix**: on its `libc` backend, Veda is Linux's kernel behind musl, as
  with `rustix_use_libc`: `build.rs` gives it `linux_like` and
  `linux_kernel` (with `linux-raw-sys`'s constants), and every
  `target_os = "linux"` outside the `linux_raw` backend names Veda too. Its
  `[hints]` go, as Cargo warns about them for a crate built from a path.
* **errno**: read where Linux's C library keeps it (`__errno_location`).
* **socket2**, **mio**, **tokio**: every `target_os = "linux"` outside
  comments names Veda too, so they take Linux's code (`epoll`, `eventfd`,
  `accept4`, `SOCK_NONBLOCK`, `pidfd_open`, socket options), which the
  POSIX layer carries out; mio and tokio add Veda to `check-cfg`. Their
  Unix domain sockets (mio's `src/sys/unix/uds/mod.rs`, tokio's
  `src/net/unix/{listener,socketaddr,stream}.rs`) stay as released, named
  by paths, as Veda's standard library has no abstract names
  (`std::os::linux`); tokio's `peer_cred` is Linux's `SO_PEERCRED`.

## Fonts

[`fonts/`](fonts/README.md) holds Inter, Lato and JetBrains Mono,
unmodified, under the SIL Open Font License 1.1 (`OFL.txt`); its README
gives each one's copyright. Nothing is built or patched:
`cargo automation build` installs them in `/system/fonts`, licence
included, and `font`'s tests and `videogen` read them from here. Noto's
fonts for the web engine are ports of their own ([above](#the-web-engine)),
downloaded as released and installed in `/system/fonts/noto` with the
browser.

## Updating a port

1. Download the new release and check its signature against the
   project's keys: the GNU keyring, musl's key on musl.libc.org, Rust's
   release key on rust-lang.org (which also vouches for the archive's
   `src/stage0`). A crate on crates.io has no signature: its SHA-256 is
   the checksum crates.io's index gives, which a `Cargo.lock` with the
   release shows.
2. Put the new version, address and SHA-256 in `port.toml`, and a crate's
   version in the workspace's `Cargo.toml` too. Check that the files its
   build's `LICENCE_FILES` names still hold the release's licence.
3. Unpack the release, apply the old `veda.patch`, then each
   `architectures/<architecture>/veda.patch`, and resolve what no longer
   applies. Make the patches again with `diff -ruN a b`, paths starting at
   the release's top directory: `veda.patch` against the pristine release,
   an architecture's against the release with `veda.patch` applied (what
   only an architecture needs goes there).
4. Rebuild with the port's command, which rebuilds what changed (run a
   crate's own tests as [Crates](#crates) shows), then run
   `cargo automation test`.

## Licences

The ports keep their licences, given in the tables above. The image
carries the texts of each one it has, as its release has them, in
`/system/licences/<port>` (the web engine's in `/system/licences/web`),
with `SOURCE`, which says where its source is: the archive `port.toml`
names, and the patches here. About Veda lists them
([Licensing](../../documentation/LICENSING.md#the-texts-in-the-image)).

Each build gathers its ports' texts in a stage of its own, `licences`: the
files of each port's release that `LICENCE_FILES` names in `build.sh`,
`linux/build.sh`, `wpewebkit/build.sh` and `python/build.sh` (with, for
Rust, those of the crates its standard library is built from). A port
added to a build without its texts listed there fails that build (the
libraries whose licences ask for none are in its `LICENCE_FREE`), and an
image build fails where it has a port without its texts: build its ports
again (`cargo automation toolchain`, `linux`, `web` or `python`), which
gathers them.
The firmware lists name the licence each file comes under, and the image
carries those texts in `/system/licences/firmware`.

Distributing Veda's image means offering the source of the ports it
contains: the archives `port.toml` names and the patches here.
[Licensing](../../documentation/LICENSING.md) covers Veda as a whole.

> [!NOTE]
> Some formats FFmpeg decodes (H.264, HEVC and AAC among them) are covered
> by patents in some countries, which the software licences do not grant.
> Whoever distributes Veda there decides whether licences are needed, or
> leaves those decoders out of `build.sh` (Videos') and
> `wpewebkit/build.sh` (the browser's).

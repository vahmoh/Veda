<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# The web browser

Veda's browser (`implementation/applications/browser`) shows the web in
tabs. Its engine is [WPE WebKit](https://wpewebkit.org) 2.54, ported to Veda
with the libraries it stands on and linked into the browser: one program,
which is also each of WebKit's processes. This guide covers using it,
building it, how the engine runs on Veda, how pages are drawn, how media
play, what keeps browsing safe, the tests, and what is not there yet.

**Contents:** [Using it](#using-it) · [Building it](#building-it) · [How it fits together](#how-it-fits-together) · [WebKit on Veda](#webkit-on-veda) · [Drawing pages](#drawing-pages) · [Media](#media) · [Security and privacy](#security-and-privacy) · [Tests](#tests) · [Limits](#limits)

## Using it

The address bar takes an address or words to search for: an address with
its scheme goes as typed (spaces in its path are escaped), a host
(`example.com/a`, `localhost:8080`, `192.168.1.1`) goes over HTTPS, or over
HTTP for this machine, a network's own names and bare addresses, and
anything else is searched for with the chosen search engine (DuckDuckGo,
Startpage, Brave Search, Google, Bing, Ecosia or Wikipedia). A
`javascript:` address typed there is searched for, never run. As the page
loads the bar shows its address from the start, the host, with what its
connection is: encrypted, not encrypted, or with a certificate the user
let through.

| Keys | What they do |
|------|--------------|
| Ctrl+T, Ctrl+Shift+N | A new tab; a new private tab |
| Ctrl+W, Ctrl+Shift+T | Close the tab; open the last one closed again |
| Ctrl+Tab, Ctrl+Page Down, Ctrl+1 to 9 | The next tab (Shift: the one before); a tab by its place (9: the last) |
| Ctrl+L, Alt+D, F6 | The address bar, its text selected |
| Alt+Left, Alt+Right, Alt+Home | Back, forward, the home page |
| Ctrl+R, F5 | Reload (Shift: without the cache); Esc stops loading |
| Ctrl+F, F3, Ctrl+G | Find in the page; the next match (Shift: the one before) |
| Ctrl+=, Ctrl+-, Ctrl+0 | Zoom in, out, back to the setting's |
| Ctrl+D, Ctrl+H, Ctrl+Shift+O, Ctrl+J | Bookmark the page; history; bookmarks; downloads |
| F11 | Full screen |

**Settings** (the menu at the right of the toolbar) choose the search
engine, the home page (the new tab page, or a page), whether the tabs open
when the browser closed open again as it starts, whether cookies from
sites other than the page's are refused (they are, by default), pages'
zoom and hardware acceleration ([Drawing pages](#drawing-pages)), and
clear the history, cookies and site data, or the cache.

**Private tabs** keep nothing: their cookies, storage and cache are in
memory and go when the last private tab closes (the next starts with
none), and their pages are not in the history or among the tabs to open
again.

**Pages' requests** wait for the user's answer: a script's alert,
confirmation or prompt, leaving a page that asks, signing in to a site,
a file to upload, a site's wish to know the location, show notifications
or keep data, and a certificate that is not trusted (going on needs a
click that says so). Downloads go to `~/Downloads` under the name the site
gives, made unique, and so does what a tab cannot show; a PDF it shows, in
WebKit's PDF viewer (PDF.js).

**The voice agent** has the browser's actions: going to an address or
searching, back, forward, reload and stop, opening, showing and closing
tabs, reading the page's text (title, address and words), finding,
scrolling, zooming, bookmarking, and forgetting the history, which it asks
the user about first. They run the same code as the toolbar and keys, so
the window shows what the agent does ([The agent](AGENT.md)).

One browser runs at a time: starting it again (`browser ADDRESS...`) gives
the running one the addresses, in new tabs. It keeps its settings in
`~/.config/browser`, the history, bookmarks, the session and the engine's
cookies and storage in `~/.local/share/browser`, its cache in
`~/.cache/browser` (a sixteenth of the free room there as it starts, and
64 MB at most, as the user's files are kept in memory), and its temporary files (a copy of each file chosen
for a page to upload among them) in `/tmp/browser`, which go as it closes.

## Building it

```bash
cargo automation web     # once: the web engine, in target/web
cargo automation build   # the image, with the browser
```

`cargo automation web` downloads the engine's ports (each archive checked
against the SHA-256 its `port.toml` pins) and runs
`implementation/ports/wpewebkit/build.sh`, which builds, in `target/web`:

* **the tools their builds run** on the build machine: Meson, pkgconf,
  gperf, Ruby, and GLib's code generators;
* **the libraries** for Veda, static and position-independent, with the
  toolchain of `cargo automation toolchain`: GLib and glib-networking,
  libsoup, ICU, HarfBuzz, FreeType, fontconfig, libxml2, libxslt, SQLite,
  OpenSSL, libgcrypt, nghttp2, brotli, the image codecs, GStreamer and
  the plugins of its that pages' media use (with FFmpeg and dav1d, the
  ports of Videos' decoder, libogg and Opus) and the rest that
  [the ports' README](../implementation/ports/README.md#the-web-engine)
  lists;
* **WebKit**, whose static libraries (some 420 MB) the browser links;
* **the licences** of WebKit, of the code of others it builds in and of
  each library, as their releases have them, which the image installs in
  `/system/licences/web` (the build fails if a library has none listed);
* **its data**: the fonts and Mozilla's list of trusted certificate
  authorities, which the image installs in `/system/fonts` and
  `/system/etc/ssl`.

The first time takes about an hour on a machine with 20 threads and 32 GB
of memory, most of it WebKit's C++, and longer on a smaller one: WebKit's
compiler jobs take some 2.5 GB of memory each, so `build.sh` runs no more
of them than the memory holds. It needs about 8 GB of disk. Later runs
rebuild what changed. Until the engine is built, `cargo automation
build` leaves the browser out of the image and says so, and `cargo
automation doctor` says whether it is built.

## How it fits together

| Path | What it is |
|------|------------|
| `implementation/applications/browser/core` | `browser-core`, `no_std` and tested on the host: what the address bar's text means, search, history, bookmarks, the session, settings, downloads' names, tabs' order, and the keys pages get |
| `implementation/applications/browser/src` | The program: the window (tabs, toolbar, page, dialogs, the browser's own pages), its files, the agent's actions, the engine's interface (`engine.rs`), and pages' sound (`sound.rs`) |
| `implementation/applications/browser/engine` | The engine's C bridge: `engine.c` runs WebKit's API on a thread of its own; `platform.c` is WebKit's platform on Veda; `media.c` registers GStreamer's plugins and the sink that plays pages' sound |
| `implementation/libraries/egl` | `egl`: EGL and OpenGL ES's C functions over Veda's OpenGL ES, for WebKit's compositor |
| `implementation/ports/wpewebkit` | WebKit's release, Veda's patch, and `build.sh`, which builds the engine |
| `implementation/ports/*` | The libraries WebKit stands on, and the fonts and certificates it uses ([the ports' README](../implementation/ports/README.md#the-web-engine)) |

**One program, three roles.** WebKit splits a browser into processes: the
UI process, which is the browser; web processes, which run pages, their
scripts and layout (a tab's in one of its own, which a popup shares with
its opener, and which a tab may change for another as it goes to another
site); and the network process, which fetches for them all and keeps
cookies and the cache. On Veda each is the
browser's program started again (`posix_spawn` of its own path) with
`WEBKIT_PROCESS_TYPE` naming the role, which the program checks first
thing (`engine::auxiliary_process`). Veda links programs statically, so
this shares one copy of the engine: the program's code and read-only data
are the system image's own pages, which every process running it maps,
not copies ([zero-copy loading](ARCHITECTURE.md#user-space)). The
processes talk over Unix domain socket pairs, passing memory files
(`memfd_create`) for frames and large messages.

**The engine thread.** In the browser, WebKit's API and its GLib main loop
run on a thread of their own (8 MiB of stack, which JavaScriptCore wants),
so that a slow page never stops the window. The bridge (`engine.c`)
queues the browser's commands to that thread and its events (a load, a
title, a request, a new frame) back to the window's loop, which waits on
an event the bridge signals. A request (a dialog, a permission, a
download) carries an id; the browser answers it by that id, and a request
whose page went away is refused.

**Frames.** WebKit hands each frame of a page to the platform's view
(`platform.c`) as shared memory with the rectangles that changed. The
bridge copies those into the tab's frame and tells the window, which draws
the page area from it; WebKit hears that the frame was shown when the
window has drawn it (or after 100 ms, when the window cannot be seen), and
only then draws the next, so a page never runs ahead of the screen.

**Input.** The window gives the page the pointer, the wheel and the keys
it does not keep for itself (its shortcuts), each key as XKB's key code
and X11's key symbol, which `browser-core::keys` works out from Veda's
key events.

**Memory.** With two pages open (the host's page of the world's scripts,
and a Wikipedia article), the browser's processes hold about 170 MB: the
browser 91 MB, the two web processes 29 and 39 MB, the network process
9 MB (Task Manager's figures). Its program, some 190 MB of code and data
(WebKit, ICU's data, Skia, and some 9 MB of GStreamer and the codecs), is
the system image's, which is in memory
whether the browser runs or not; under memory pressure, WebKit's
processes give back caches as the UI process tells them to. A page
composited on the GPU takes some 85 MB more in its web process than one
painted on the CPU (the GPU's staging memory and buffers, the frames read
back): about 120 MB for a page at 1920x1080. QEMU's default 2 GiB is
enough for browsing at that size; with 1 GiB (about 530 MiB for
programs, the driver VM taking 256 of them) the browser paints on the
CPU ([Drawing pages](#drawing-pages)).

## WebKit on Veda

WebKit's port to Veda is a patch of its release
(`implementation/ports/wpewebkit/veda.patch`), most of it `OS(VEDA)`
taking Linux's way where Veda has Linux's interface, and a few changes of
its own. Veda's POSIX layer gained what WebKit needs, which any program
may use.

| WebKit needs | On Veda |
|--------------|---------|
| Memory: reserving address space, committing, decommitting | bmalloc and libpas as on Linux: Veda's private memory is committed as it is touched, and `madvise(MADV_DONTNEED)` drops pages, which read as zeros. libpas zeroes in place what it allocates zeroed below 8 MiB (Skia's tiles, pictures), where Linux gives the pages back from 1 MiB: on Veda that costs a TLB shootdown and a page fault for each page touched again. JavaScriptCore reserves its cages (about 112 GiB of address space with their alignment) in VMOs of up to 1 TiB, which cost only the pages used |
| The JIT | JavaScriptCore's baseline JIT, DFG and FTL, and WebAssembly's BBQ and OMG, on x86-64. The DFG polls its traps, as Veda's signals are taken as a system call returns and cannot stop code that makes none; WebAssembly checks its bounds rather than relying on guard pages' faults, which end a Veda program |
| A garbage collector that scans every thread's stack | `veda_thread_suspend`, `veda_thread_resume` and `veda_thread_registers` (`<veda/thread.h>`): the kernel stops a thread and saves its registers, as Mach's `thread_suspend` does; Linux's way, a signal, would reach a thread only as its next system call returns |
| Threads with large stacks and names | WTF's threads get 1 MiB stacks (musl gives 128 KiB); the main thread's stack is the startup message's (`pthread_getattr_np`) |
| Shared memory between processes | Memory files (`memfd_create`), mapped shared; a read-only handle is a descriptor opened read-only (`/dev/fd/N`), whose kernel handle has no right to write, so no process it goes to can write the memory |
| Timers | `timerfd` on the monotonic, real-time and boot clocks, which GLib's main loop and epoll wait on |
| The time zone | The system's offset from UTC (the `tz=` boot option, or the firmware's): musl reads it with no `TZ` set (`__veda_tz`), and ICU names it as the tz database's zone of that offset, `Etc/GMT-2`, or a region's on it all year (`Asia/Kolkata` for +05:30) |
| How much memory the system has, and the process | `sysinfo`, whose free memory is what programs may still take, and `veda_process_memory` (`<veda/memory.h>`), for WebKit's memory pressure handling: the UI process watches the system's memory, four times a second once 70% of it is used, and tells the others to give some back from 80% (90%: at once, all they can), earlier than on Linux, as Veda has no swap and ends a process it has no page for |
| Processes | One program, started again for each role (above); no injected bundle, as Veda loads no code at run time |
| Its resources: the pictures WebCore draws (a broken image's icon, a text area's resizer, the media controls' icons), Web Audio's spatial sound, and the PDF viewer (PDF.js, with the style and script WebKit adds to it), in GResource bundles its library registers as it loads | Linked in, each bundle registering itself as the program starts (its object's constructor); `engine.c` refers to them, as a static link leaves out an object nothing refers to. The viewer is most of them, 3.7 MB of the program; its fonts, character maps and decoders have their licence texts with WebKit's |
| Files | `statx`; GLib's file monitors poll, there being no inotify |
| Files' types (shared-mime-info's database, which xdgmime reads) | None on Veda: WebKit knows the types of the files the web is made of (HTML, CSS, scripts, pictures, fonts, sound and video) by their extensions, so that a page from the user's files is shown as one; other files go by their contents |
| A sandbox for the web processes (Linux's bubblewrap and seccomp), and one for the network process, which Linux's ports leave out | Veda's: the VFS connection and the registry channel confined, the kernel's `process_restrict` (`<veda/sandbox.h>`; [Security and privacy](#security-and-privacy)) |
| Sandbox extensions (Cocoa's), which let a sandboxed process read a file the user opened | The VFS's loans (`veda_lend_files`, `veda_borrow_files`, `veda_take_back_files`): the browser lends its network process the folder of a page the user opens, and takes it back once no page open needs it ([Pages from the user's files](#pages-from-the-users-files)) |

**Text.** HarfBuzz shapes, FreeType renders and fontconfig chooses from
`/system/fonts` (`implementation/ports/fontconfig/fonts.conf`): Inter for
sans-serif and the interface's, Noto Serif, JetBrains Mono, Noto's fonts
for the world's other scripts (Arabic, Hebrew, Indic, Southeast Asian,
Ethiopic, Georgian, Armenian, symbols and mathematics), Noto Sans CJK for
Chinese, Japanese and Korean, chosen by the page's language, and Noto
Color Emoji (COLRv1). A family the system lacks (Arial, Georgia...) gets
the closest generic one.

**The network.** libsoup speaks HTTP/1.1 and HTTP/2 (nghttp2), with
brotli and gzip, over GIO's sockets, which are the POSIX layer's and so
`netd`'s ([Networking](NETWORKING.md)). TLS is glib-networking's OpenSSL
backend, checking certificates against Mozilla's list of certificate
authorities (`/system/etc/ssl/cert.pem`); names are resolved by the
network service. The browser hears the network service's events, so a
page that failed for want of a network loads again once there is one.

## Drawing pages

Skia paints pages on the CPU. What becomes of the paint depends on
**Use hardware acceleration when available** (Settings), and on the
machine:

* **Painted on the CPU**, each page as one picture: WebKit paints what
  changed straight into the frame's shared memory. This is the mode on a
  machine without a GPU, or with the setting off.
* **Composited on the GPU**: WebKit paints each layer of the page into
  tiles, and its compositor (TextureMapper) draws the layers with OpenGL
  ES, so scrolling, CSS animations and transforms run on the compositor's
  thread without painting again, and keep going while a page's script is
  busy. The frame is read back into shared memory (as BGRA) for the
  window. This is the mode when the setting is on and a GPU can render.

The browser chooses as it starts: it composites if `egl::gpu_renderer()`
finds a GPU that can render (the `gpu` service is registered and has what
`gl` needs) and the system has 1 GiB of memory for programs, and says
which in the log (`browser: pages are composited on the GPU: RENDERER`,
or `painted on the CPU`, with why where memory decided). A browser
started before the GPU's service, as the system starts, paints on the CPU
until it starts again. In software,
compositing would take far more of the processor than painting does, so
it is never chosen there. Measured in QEMU (4 processors, the browser's
window 1088x596, a GPU through virgl on the host's Intel Xe):

| Page | Painted on the CPU | Composited on the GPU | Composited in software |
|------|-------------------|-----------------------|------------------------|
| A box spinning (CSS animation) | 62 frames a second; web process 21% of a processor | 67; 8% | 75; 125% |
| A long page scrolling itself | 50; 14% | 50; 10% | 50; 200% |
| Wikipedia, scrolled with the keyboard | 60, with a 2-second pause; 17 to 28% | 60; 12 to 21% | 60; 266% |

**EGL and OpenGL ES for C.** WebKit reaches OpenGL ES through libepoxy,
which on Veda finds every function by name through the program's
`eglGetProcAddress` rather than in a shared library. The `egl` crate
(`implementation/libraries/egl`) is that function and the rest: EGL 1.5's
functions and OpenGL ES 3.0's, over `gl`, Veda's OpenGL ES in Rust
([OpenGL ES and the GPU](ARCHITECTURE.md#opengl-es-and-the-gpu)).

* **One display**, the surfaceless platform's
  (`EGL_MESA_platform_surfaceless`), which is also the default display:
  contexts draw into framebuffer objects (`EGL_KHR_surfaceless_context`)
  or pbuffers. There are no window surfaces yet.
* **Contexts** are OpenGL ES 3.0, which EGL allows for 2.0. Contexts that
  share objects (a share group) are one `gl::Context` with a `gl::Local`
  (bindings, current program, vertex arrays, framebuffers, queries) for
  each, swapped in under the group's lock as a thread calls: their calls,
  from any threads, take turns. A binding to an object another context
  deleted is forgotten as the context comes back.
* **Each thread** has its current context (`eglMakeCurrent`, with EGL's
  rules: a context current in another thread cannot be made current);
  fences (`EGL_KHR_fence_sync`, `EGL_KHR_wait_sync`) and GL's syncs work
  across a group's contexts.
* **What is not there**: client-side vertex and index arrays (draws that
  would read them fail with `GL_INVALID_OPERATION`, as with a vertex array
  object bound), EGL images, and robustness. A context whose GPU stopped
  answering draws nothing, so a page composited on a GPU that went away
  stops changing until the browser starts again.

`gl` also gained what WebKit's compositor uses: surfaceless contexts and
switching a context's surface (`Config::surfaceless`,
`DefaultFramebuffer::set_surface`), BGRA pixels
(`GL_EXT_texture_format_BGRA8888`, `GL_EXT_read_format_bgra`), share
groups (`Local`), and the sizes of the pixel data C functions take
pointers to (`Context::unpack_size`, `Context::pack_size`).

## Media

`<video>`, `<audio>`, Media Source Extensions and Web Audio play with
[GStreamer](https://gstreamer.freedesktop.org) 1.28, in the page's web
process, as in WebKit's ports to Linux. A media element's player is
GStreamer's `playbin` (`playbin3` for Media Source Extensions), whose
source is WebKit's own: `webkitwebsrc`, which the network process fetches
for, with the page's cookies and cache, or for Media Source Extensions
`webkitmediasrc`, which plays what WebKit has demuxed and parsed of the
page's appends. GStreamer finds a stream's type, demuxes it (MP4, Matroska
and WebM, Ogg, WAV; MP3, AAC and FLAC streams), parses, decodes, converts
and plays it.

* **Codecs** are FFmpeg's, through gst-libav, as Videos' decoder's are
  (`mediadec`): AV1 (dav1d), VP9, VP8 and H.264 pictures; Opus, Vorbis,
  AAC, MP3 and FLAC sound, and PCM in WAV. `canPlayType` and
  `MediaSource.isTypeSupported` answer what these can, as WebKit asks
  GStreamer's registry. The processor decodes (there is no hardware
  decoder), with threads of the decoder's own.
* **Linked in**: Veda links programs statically and loads no code, so
  GStreamer has no plugins to find. Its core, built for static programs,
  calls the program's `gst_init_static_plugins` as it starts, the first
  time a page has media: `engine/media.c` registers there the plugins the
  web build linked in (`gst_full_register_plugins`, of
  `libgstreamer-full-1.0.a`) and the browser's sink. GStreamer keeps no
  registry on disk, and has no plugin scanner to start.
* **Sound** goes to Veda's audio service. The sink (`vedaaudiosink`, a
  `GstAudioSink`, whose rank makes it the one `autoaudiosink`, WebKit's,
  chooses) writes 16-bit samples, mono or stereo, into a stream of the
  service (`src/sound.rs`): one for each media element or Web Audio
  context that plays, opened as its first sound comes and closed as it
  stops (the service logs `stream N "Browser" opened` and `closed`), on
  one connection of the web process's, as the service keeps up to 16
  streams a connection. A pause pauses the stream and a seek empties it;
  the clock that keeps pictures in step with the sound is the samples the
  service says were heard. With no stream (no audio service, or none to
  spare) the sound goes unheard at its pace, and the pictures still play
  at theirs. GStreamer sets the volume, and converts and resamples to what
  the stream takes.
* **Pictures** are converted to BGRx for WebKit's video sink, without
  GStreamer's OpenGL: painted on the CPU, Skia draws the current one into
  the page as WebKit paints it; composited on the GPU, the compositor
  uploads each into a texture.
* **Web Audio** renders in WebKit, into a GStreamer pipeline from WebKit's
  source to the same sink; `decodeAudioData` decodes with GStreamer, from
  memory, and the analyser's FFT is GStreamer's.
* **Sound waits for the user**: as in other browsers, a page plays sound
  only once the user has done something on it (clicked, pressed a key),
  WebKit's policy of autoplay without sound: an `<audio>` or `<video>` that
  would play by itself waits, and an `AudioContext` stays suspended, until
  then. A video without sound plays at once.
* **Buffered in memory**: what a page plays is buffered in memory, as much
  as GStreamer's queues hold, never in a file of its own as WebKit would on
  Linux (`WPE_SHELL_DISABLE_MEDIA_DISK_CACHE`): a web process writes no
  files, and such a file would keep what the page played where other
  programs read.
* **Memory**: playing the test page's video (640x360, AV1 and FLAC) adds
  about 16 MB and 15 threads to its web process, for GStreamer's queues and
  the decoders' pictures and threads (Task Manager's figures: 35 MB on a
  page without media, 51 MB as it plays).

## Security and privacy

* **TLS** is checked, and a certificate that is not trusted stops the
  page: the browser says why (an unknown issuer, another site's, expired,
  revoked, too weak) and offers to go back; going on takes a click on a
  red button that names the site.
* **Cookies** from sites other than the page's are refused by default,
  and WebKit's Intelligent Tracking Prevention is on, in private tabs too.
* **Permissions** (location, notifications, storage, devices) are asked
  for and refused unless the user allows them; pages cannot open windows
  without a click.
* **Typed scripts**: an address bar `javascript:` address is searched for,
  so that a page cannot talk a user into running its script there.
* **No code at run time**: there are no injected bundles or extensions,
  and the developer tools are off.
* **Processes**: pages run in web processes apart from the browser, so a
  page that crashes takes only its tab down (the browser offers to load
  it again). WebKit ends a web process that sends the browser a message
  it should not (one it cannot read, or one that fails WebKit's checks,
  such as the address of a file the page may not read), as one a page has
  taken over; the log says so (`Web process N is ended for an invalid
  message: NAME`), WebKit's own log of it being left out of the build.

**The web processes are sandboxed**, with Veda's own means where Linux's
ports of WebKit use bubblewrap and seccomp. A process on Veda starts with
its parent's rights, so a web process would have all of the browser's:
the user's files, the network, the system's services, starting programs.
As it starts, before any page reaches it, it gives up for good what a
page's process does not need (`WebProcess::initializeSandbox`, WebKit's
hook, with `<veda/sandbox.h>`):

| | A web process keeps | It gives up |
|---|---|---|
| Files: its connection to the VFS, confined (`vfs::confine`) | The system image (fonts, fontconfig's configuration, the program), fontconfig's cache (`/tmp/fontconfig`) and the devices, to read; `/dev/null`, to write | Everything else: `/home`, `/tmp`, sockets bound to paths, abstract socket names |
| Services: its registry channel, and those cloned from it, confined (`registry::confine`) | The audio service, which pages' sound plays through (`engine/media.c`'s sink, in the web process), and the GPU's while pages are composited: the services the browser names (`WEBKIT_VEDA_WEB_PROCESS_SERVICES`) | The network service, so it has no sockets (`EACCES`; the network process fetches for it, and resolves names); the VFS's, as a connection of its own would reach past its view; the launcher; any other |
| The kernel's abilities (`process_restrict`) | Threads, memory, and JavaScriptCore's pool of executable memory, which the engine reserves (read, write and execute, as on Linux on x86-64) before the sandbox and writes all its JIT code into | Creating, listing and opening processes; reading the kernel log; making any other memory executable (a file's, a memory file's it is sent) |

Its other files are memory files (frames, large messages), which come to
it as descriptors. fontconfig's cache is the browser's to make, as its
engine starts (`FcInit`), so that a page's process cannot change what the
others read. The web process then checks that what it gave up is gone
(the user's files, a socket of its own, a process it starts), says so in
the log (`Web process N is sandboxed: ...`; the VFS logs the first paths
it refused it that are there, `/home` among them), and goes no further if
it is not.
`WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` in the browser's environment
leaves the web processes unsandboxed, as on Linux, to find out whether
the sandbox is what breaks a page.

**The network process is sandboxed too**, where Linux's ports of WebKit
leave it out. It fetches for every page and keeps every site's cookies,
storage and cache, so it reaches the network and the engine's files; as
it starts, before any request reaches it, it gives up the rest
(`NetworkProcess::initializeSandbox`, with `<veda/sandbox.h>`):

| | The network process keeps | It gives up |
|---|---|---|
| Files | The system image (the program, ICU's data, the certificate authorities in `/system/etc/ssl`) and the devices, to read; `/dev/null`, to write; and, to read and change, the places the browser names (`WEBKIT_VEDA_NETWORK_PROCESS_FILES`): the engine's data (`~/.local/share/browser/engine`: cookies, storage) and cache (`~/.cache/browser`), the downloads (`~/Downloads`), and the engine's temporary files (`/tmp/browser`) | The rest of the user's files (their documents, the browser's history and bookmarks, other programs' files), but for the folders of the pages the user opens, which the browser lends it as they open, to read, while a page open needs them ([below](#pages-from-the-users-files)); and the rest of `/tmp`, fontconfig's cache among it, which the web processes trust |
| Services | The network service: sockets, and names looked up | The VFS's, as a connection of its own would reach past its view; the launcher; any other |
| The kernel's abilities | Threads and memory | Creating, listing and opening processes; reading the kernel log; making memory executable (it runs no script: proxies come from the environment, never from a PAC script) |

What needs files works within those places:

* **Uploads**: a file the user chooses for a page is copied into the
  engine's temporary files (`/tmp/browser/uploads`), and the page gets the
  copy, under the file's name, which the network process reads as it
  uploads. The VFS copies in one step, the copy sharing the file's
  contents until either changes, so it takes neither time nor memory,
  however large the file.
* **Downloads** go to `~/Downloads`, which it writes.
* **Pages from files** (`file://`): the folder of a page the user opens
  is lent to it, to read, while a page open needs it
  ([below](#pages-from-the-users-files)).
* **Its own temporary files** (IndexedDB's blobs on their way in, the
  File System API's writes) go in `/tmp/browser`, its `TMPDIR`.
* **Its databases** (IndexedDB, local storage, ITP's) commit with `fsync`
  as on Linux: a confined connection's `sync` returns at once, and the
  VFS saves what changed within seconds, as it saves everything.

It first makes the directories on the way to its places (which the
sandbox lets it look at but not change), then checks that what it gave
up is gone (a file of the user's beside its places, the VFS's service, a
process it starts), says so in the log (`Network process N is
sandboxed: ...`; init logs that it may not reach the service `vfs`, and
the VFS the file it refused it), and goes no further if it is not.
`WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` leaves it unsandboxed too.

### Pages from the user's files

A page from a file (`file://`) is read by the network process, which
reaches none of the user's files but the engine's own. As the page opens,
the browser lends it the folder the page is in, to read, through the
VFS's loans ([the VFS](ARCHITECTURE.md#storage-and-files)), as Apple's
ports of WebKit give the network process a sandbox extension:

* **What lends** is the browser's own loading of a page: an address the
  user typed, a bookmark or a page of the history, a file opened with the
  browser (WebKit's `webkit_web_view_load_uri` and
  `webkit_web_view_load_request` on Veda). A page's own links, scripts and
  redirects, the windows it opens and its history lend nothing, so no page
  widens what the network process reaches.
* **What is lent** is the page's folder and what is inside it, to read,
  never to change: a page shows what is beside it (its pictures, styles,
  scripts and frames, and the pages it links to, as a page saved with its
  files has them), which the file alone would leave out. A folder's own
  address lends that folder. The home folder is never lent whole, nor
  what holds it (the VFS refuses such a loan too): a page in it lends
  only itself.
* **How**: the browser, which is not sandboxed, has the VFS make a loan of
  the folder, a secret of 16 random bytes, and sends it to the network
  process over WebKit's own channel between them
  (`NetworkProcess::BorrowFiles`, never from a web process); the network
  process takes it (`veda_borrow_files`), once, and the VFS logs it
  (`browser (process N), confined, was lent /home/user/Documents to
  read`). A network process started after another ended is lent what the
  pages open need.
* **Taken back**: a folder is lent while a page open needs it, one that
  shows a page from there (in its tab, or in a frame, an iframe), is
  loading one, or has one in its history (back and forward, its frames'
  too), or a window such a page opened that does. Once none does (its
  last tab closed, or its last page left the history of its tab), the
  browser takes the loan back (`veda_take_back_files`, with its secret):
  the VFS takes the folder out of what the network process reaches,
  which nothing the network process does can keep, and logs it
  (`browser (process N), confined, had /home/user/Documents taken
  back`). So a compromised network process reads only the folders of
  the pages still open, not every one opened since the browser started.
  Loading such a page again lends its folder again, as does reloading
  it, or going back or forward to it, if its folder was lent for its tab
  before; a page's own history lends nothing. WebKit forgets the folder
  too, for every web process (`NetworkProcess::ForgetFiles`, sent as the
  loan is taken back, and in what its UI process assumes each may read):
  the network process reads it for none of them, and each counts as never
  allowed it, so that a page there later goes to a new process. So a
  process that was allowed the folder (a window that outlasts its opener,
  in its process) does not have it read when it is lent again, for
  another page: the network process refuses it, as it refuses a web
  process any file lent for no page of its. Nor does such a window go to
  a page of the folder (a link, `location`), or open one in a new window:
  that fails, saying to open the file from the address bar, as a page's
  link to a file outside the folders lent for it does. (Else it would go
  to a new process, allowed nothing there, which WebKit would end as it
  told of the page's address, a file's it may not read: the window would
  stay as it was, without a word.) A frame that shows a page of the
  folder keeps it lent as a tab does: WebKit takes a page's address (as
  it goes to a fragment, pushes a state, or its load ends) only from a
  process that may read its folder, and would otherwise end the frame's
  process, as one that names a file it may not read. So a window a page
  opened, with a frame of a page of the folder in it (the page put it
  there), keeps the folder lent, as its own, once that page's tab closes,
  and the frame's page goes on; and a page let go to a file there is
  loading it from then on, before its process starts the load (for a
  while: the process starts it at once), so that the folder is not taken
  back between the two.
* **Within the network process**, a web process has it read only what was
  lent for its page (WebKit's allowance, `AllowFileAccessFromWebProcess`,
  which the network process checks on Veda for every file it reads for a
  web process), and is allowed a folder only as it starts, before it has
  shown anything: a process that has shown a page, from the web or from
  the user's files, gains no folder after, and a page from the user's
  files whose folder its tab's process was not allowed goes to a new
  process. So no page reaches a folder lent for another, in another tab
  or before it in the same one, nor one lent after it was shown, whatever
  the user then opens in its tab (from its error page's Try again too).
  Veda has no "universal file read", which WebKit's other ports give a web
  process that loaded a file: a page's link to a file outside the folders
  lent for it fails, saying to open the file from the address bar, in its
  window or a new one (a link or form with a target, such as `_blank`:
  the page's tab says so and no window opens, where WebKit would end the
  page's process, as one that named a file it may not read, and the tab
  would only say that the page stopped working), and the network process
  refuses a web process any other file it asks for
  (`Network process N: web process M was refused PATH: it was lent for no
  page of its`). That check stands in for WebCore's own, that a page from
  files shows only files of its volume, which a sandboxed web process
  cannot make (it cannot look at the user's files) and which WebKit
  leaves out under Linux's bubblewrap too.

## Tests

| Test | What it covers |
|------|----------------|
| `browser-core`'s unit tests (`cargo test -p browser-core`) | Addresses and search, history, bookmarks, the session, settings, downloads' names, tabs, keys |
| `egl`'s and `gl`'s unit tests | The display and configurations WebKit looks for, drawing surfaceless and reading BGRA back, contexts shared across threads (and EGL's access rules), fences, pbuffers, every function by name, buffers; in `gl`, sharing, deletion across contexts, surfaceless contexts, BGRA uploads and reads, and WebKit's compositing at a page's size (Skia's tiles uploaded whole and in parts, an image turned round by TextureMapper's shader, clears and solid fills, the frame read back whole), which `GL_TEST_BACKEND` runs on a GPU too |
| `verification/ui/browser.vts` | The browser as a person uses it, on the host's test pages (`automation/src/hostweb.rs`): clicking, Tab and Enter follow a link, Alt+Left goes back, a new tab, typing an address, finding text, closing a tab, the world's scripts and emoji; and the test page's web process sandboxed: it found the user's files, a socket and a process of its own refused, and the VFS refused it `/home`; and the network process sandboxed: the VFS refused it a file of the user's beside its places, and init the VFS's service |
| `verification/ui/browser-memory.vts` | The browser on a machine of 1 GiB with a GPU: it paints pages on the CPU, as compositing them would take more memory than the machine can spare, and shows a page with no process running out of memory |
| `verification/agent/app-browser.vts` | The agent's actions, with the stand-in for the Voice Agent API: opening and reading pages (their scripts ran), the keyboard on a page the agent opened, back, tabs, finding, zooming, bookmarking, the time zone pages see, a PDF shown in WebKit's viewer (its text found there), and private tabs forgetting cookies; the pages' web processes and the network process sandboxed, as above, and GLib never complaining |
| `verification/agent/app-browser-files.vts` | The network process and the user's files: a download saved in Downloads and opened from there (`file://`); a picture chosen for a page to upload read whole, through its copy, never the picture itself; a page in Documents opened (`file://`) in a tab of its own, its folder lent to the network process, the picture beside it shown and one in Pictures, which the page reaches for outside its folder, refused, and a page's own navigation to Pictures failing, and its link to Pictures in a new window (`target=_blank`) failing alike, in its tab, no window opened and its process going on, Pictures never lent; the folder taken back as the tab closes, which a window one of its pages opened outlasts, and forgotten for every web process: the network process refuses the window the picture, never reading it; a window that another page there opened with a frame of a page of the folder in it keeping the folder lent as that page's tab closes (it shows the picture), the frame's page going to a fragment and pushing a state, its process going on, and the folder taken back as the window closes; the page opened again, its folder lent again, for it alone: the first window, whose process was allowed the folder before, refused the picture again, and its own navigation to a page of the folder failing, saying so, the page unread; a site's IndexedDB kept as the browser starts again, and the temporary files gone as it closed |
| `verification/c/posix.c` | The POSIX layer's `timerfd`, memory files, suspending threads, the system image's files mapped in place and time zones, among the rest; and `<veda/sandbox.h>`: a child confines itself as a web process does (places to read and to change, one service, no processes or new executable memory), is refused all it gave up, keeps the rest (code written into memory made executable before runs), and starts a child that is confined alike; and loans: a place lent to a confined child, which reads it and what is inside it, changes none of it, finds nothing beside it, and takes the loan once, with its own secret and for its own place, as a sandboxed program lends nothing and nothing holding the home folder is lent; and taken back: the place gone from a confined child that took it, but for the file it has open there, a loan taken back before it is taken taking nothing |
| `vfs-core`'s unit tests (`cargo test -p vfs-core`) | What a confined connection's view reaches: its places, the way to them, what listings show, confining again only narrower, a place borrowed reaching nothing less and taken back reaching nothing more, each loan's place its own, confining again keeping what was borrowed; and loans: taken once, for their place, lapsing, a few for each lender, taken back by their lender alone, and ending with their lender's connection |
| `verification/ui/browser-media.vts` | Media on the host's video page (`/video`): a click plays the countdown (videogen's, AV1 and FLAC in WebM) with its sound, which the audio service plays through a stream of the page's to the end, never short; the machine's sound output has its beeps |
| `verification/agent/app-browser-media.vts` | The media pages' own checks, which the agent reads: Web Audio (`/webaudio`: an oscillator its analyser hears, a sound file decoded and played), a sound file in an `<audio>` element (`/audio`), the countdown played to its end with every picture shown and its first colours right, drawn into a canvas (`/video`), and the same through Media Source Extensions (`/mse`) |
| `automation`'s unit tests (`cargo test -p automation hostweb`) | The host's test pages, their media sent whole or in the byte ranges asked for, the file to download, and the PDF, whose table says where each of its objects is |

The scripts need the engine built (`requires web`) and are skipped
without it.

## Limits

* **Media**: no WebRTC, WebCodecs, capture or recording, or encrypted
  media (EME). HLS and DASH are left to pages' own players, through Media
  Source Extensions, as WebKit leaves them by default. The processor
  decodes, which large videos may outrun (it drops pictures to keep up),
  and HEVC is not decoded.
* **WebGL** and the GPU process are off, and canvases are painted on the
  CPU.
* **Pages from files** (`file://`) reach their own folder, and what is
  inside it: a picture, style or page they name outside it (`../`) is not
  there for them unless the user opened a page from there in the same tab,
  and a link to one fails. The network process takes at most 64 places
  in all, its own among them. A folder taken back is out of its reach at
  once, but for a file it was reading there, which stays open until it
  has read it.
* **Spelling** is not checked, and there are no developer tools,
  extensions or printing.
* **PDFs**: the agent reads none of a PDF's text, which is in its
  viewer's frame, not the page's, though finding finds it there.
* **The GPU** is chosen as the browser starts (above), and losing it
  stops composited pages until the browser starts again.
* **Files** changed behind the browser's back are noticed by polling, as
  Veda has no inotify.

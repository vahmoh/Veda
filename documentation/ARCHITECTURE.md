<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Veda architecture

Veda is an x86-64 operating system built around a capability-based
microkernel. The kernel implements only isolation, scheduling, memory
management and inter-process communication; drivers, file systems, the
window system and every application are user-space processes that talk
over kernel channels. This document describes the parts, how they talk and
who may call what, and links to each subsystem's own document.

**Contents:**

- **The core:** [Boot](#boot), [The kernel](#the-kernel),
  [User space](#user-space)
- **Devices and data:** [Devices](#devices),
  [Storage and files](#storage-and-files), [Networking](#networking)
- **Graphics:** [The window system](#the-window-system),
  [Displays and GPUs](#displays-and-gpus),
  [The desktop shell](#the-desktop-shell),
  [Graphics and the toolkit](#graphics-and-the-toolkit),
  [OpenGL ES and the GPU](#opengl-es-and-the-gpu)
- **Media and applications:** [Audio](#audio), [Video](#video),
  [Applications](#applications), [The voice agent](#the-voice-agent)
- **The code:** [Testing](#testing), [Architectures](#architectures),
  [Repository layout](#repository-layout)

## Boot

1. The firmware (OVMF under QEMU) loads the loader, `vboot`
   (`implementation/boot/`), from UEFI's boot path for the architecture
   (`\EFI\BOOT\BOOTX64.EFI` on AMD64) on the EFI system partition.
2. `vboot` reads `\VEDA\BOOT.CFG`, `VKERNEL.ELF` and `INITRD.IMG`, picks a
   GOP mode near `BOOT.CFG`'s size in the display's own shape, and clears
   the screen through a write-combining mapping. It loads the kernel,
   builds its page tables (identity map, direct map at
   `0xFFFF800000000000`, kernel at `0xFFFFFFFF80000000`, each page with its
   segment's permissions), reads the firmware variables an operating system
   may read (`bootinfo::variables`), exits boot services and jumps to the
   kernel with a `bootinfo::BootInfo`.
3. The kernel initialises memory, ACPI, the interrupt controllers, timers
   and the other CPUs, then starts `bin/init` from the initrd, the only
   program it loads itself.
4. `init` starts the system services in this order, each with only the
   capabilities it needs, and restarts those that may be restarted when
   they exit, at most three times a minute. Drivers reconnect to a
   restarted service through the registry, which queues connections until
   it registers again.

| Service | Role | Restarted if it exits |
|---------|------|-----------------------|
| `vfs` | the file systems | no: it holds the user's files |
| `netd`, `wlan` | the network and Wi-Fi services | yes |
| `devmgr` | devices, drivers and the driver VM | no: it owns the running drivers |
| `compositor` | the window system | yes |
| `audio` | the sound mixer | yes |
| `agent` | the voice agent | yes |
| `shell` | the desktop shell | yes |

**The startup sequence** (`implementation/services/compositor/src/startup.rs`).
The screen stays black until the screen the desktop will be on is ready:
the driver VM's display driver has shown its first picture, or `devmgr`
says no driver is coming (`displaydev::expect`). The splash
(`vsplash`) then fades in, and the desktop dissolves in with the startup
sound once the desktop and the taskbar have each presented a frame, the
splash has been up for 2.1 s and the shell has the sound ready (3 s at
most). `init` asks for the sequence and the sound only at system start: a
window system or shell restarted after a crash shows the desktop at once,
silently.

**The live system** (`cargo orchestration iso`) is a hybrid ISO 9660 image
that starts the same way from a disc, or from a USB stick it is written to
or copied onto (as Rufus does). `live` on the kernel command line makes
`devmgr` start no disk driver and `vfs` keep `/home` in memory, so the
computer's disks are never read or written; the stick keeps the system's
logs (see [the driver VM](DRIVERVM.md)).

## The kernel

`vkernel` (`implementation/kernel/`):

| Module | Responsibility |
|--------|----------------|
| `arch` | the processor architecture and its platform, behind one interface (`src/arch.rs`); on AMD64 the GDT, TSS, IDT and entry paths, APICs, timers, SMP bring-up, TLB shootdowns, page tables, the context switch, VMX and EPT, and the IOMMU (VT-d) |
| `mm` | frame allocator, page tables, kernel heap, kernel stacks and MMIO windows, VMOs, address spaces, user copies |
| `sched` | threads, priority round-robin scheduling, blocking with timeouts, idle loop |
| `object` | handles, signals, processes, channels, sockets, events, ports, interrupts, I/O ports, resources |
| `syscall` | the system call layer (numbers and types in `implementation/libraries/abi`) |
| `hv` | the hypervisor: guests and their virtual processors, for [the driver VM](DRIVERVM.md) |
| `sync`, `random` | the big kernel lock and spinlocks; the ChaCha20 random number generator |
| `acpi`, `time`, `percpu`, `futex`, `loader`, `log`, `panic` | supporting subsystems |

- **Concurrency.** Kernel code runs with interrupts disabled under a big
  kernel lock (BKL): the kernel is effectively single-threaded while user
  code runs on all CPUs, and its operations are short, which keeps
  contention low. Per-object spinlocks catch accidental recursion; TLB
  shootdowns are acknowledged without the BKL, so they cannot deadlock.
- **Capabilities.** Everything user space can touch is a kernel object
  reached through a per-process handle with rights (`vabi::Rights`); there
  is no ambient authority. Hardware access needs *resource* handles:
  `init` holds the root resource and derives narrow ones (an I/O port
  range, one IRQ, one MMIO range) for each driver. A process handle with
  `MANAGE` ends the process alone or with its job (every process it
  started, recursively). A process can give up, for good, reading the
  kernel log, creating, listing and opening processes, and making memory
  executable (`process_restrict`).
- **Memory.** VMOs are the unit of memory. Anonymous ones are committed on
  page faults; a write-combining one (for a device that does not snoop the
  caches) is kept out of the caches whenever the kernel touches it; a
  physical one never covers RAM. `vm_protect` changes a mapping's
  permissions within the rights of the handle it was mapped with. *Private
  memory* (`vm_allocate`) is a VMO no handle reaches, for one mapping, whose
  pages are freed when unmapped or decommitted (`vm_decommit`). Frames are
  freed only after the TLB shootdown that makes them unreachable.
- **IPC.** Channels carry messages of up to 64 KiB plus 64 handles. An
  endpoint has one handle at a time unless its channel is shared
  (`channel_flags::SHARED`): then several processes can hold it, and each
  message goes to one of them. Bulk data travels in shared VMOs. *Sockets*
  are byte streams (pipes, terminals, the POSIX layer's stream sockets):
  256 KiB each way, all-or-nothing writes of up to 4096 bytes,
  half-closing, and handles that travel with the bytes written with them.
  No channel or socket may carry a handle that would keep it alive through
  what is queued (`object::would_cycle`).
- **Waiting.** `object_wait_many` waits on up to 1024 objects. A *port*
  keeps registrations (`object_wait_async`) and queues a packet for each
  that fires (`port_wait`), so a wait costs what is ready, not what is
  watched; it holds at most 65536 registrations, and its handle cannot
  leave its process. `vipc`'s
  `WaitSet`, the core of every service loop, uses one when it is kept from
  one wait to the next.
- **Time.** On AMD64 the monotonic clock counts the TSC, calibrated against
  the HPET (or the PIT); on an Intel processor not under a hypervisor, the
  frequency the processor reports (CPUID leaves 0x15 and 0x16) wins over a
  measurement more than 5% off it (`dmesg time`). The kernel sets each
  processor's `IA32_TSC_ADJUST` to 0 as it starts and corrects any skew
  left (`dmesg smp`). The wall clock starts from the RTC, in UTC; the `tz=`
  boot option gives local time.
- **Processors and scheduling.** Each processor turns its caches on as it
  starts and, with Intel's hardware P-states, chooses its own speed, up to
  turbo. The scheduler has 32 priorities, round-robin within each, 10 ms
  slices, preemption when a higher-priority thread wakes, tickless one-shot
  timers and eager switching of FPU and vector state (XSAVE). A thread can
  name an *exit futex*, which the kernel clears and wakes once the thread
  has left its stack, so that thread libraries can join it.

## User space

Every program, and the kernel, is a static ELF64 executable for the System
V ABI; only the loader is PE/COFF, the format UEFI starts. Programs are of
two kinds (see [Rust on Veda](RUST.md)); `orchestration/src/components.rs`
says which each is:

| Kind | Programs | Built |
|------|----------|-------|
| On Rust's standard library (`Program::std`) | the applications, and the services that gain from it (the shell, the agent) | for Veda's own Rust target, linked by GCC with the C library: POSIX programs, as C programs are |
| On `vrt` alone (`Program::native`, `#![no_std]`) | what the standard library stands on or runs before (`init`, the file system, the network stack), what drives hardware and what must answer in time (the window system, the sound mixer) | for Linux's musl target (hard-float SSE; the kernel is soft-float), linked by `rust-lld` with nothing of Linux's or musl's: position-independent, `DT_RELR`-packed, marked by a `velf::veda` note |

- **`vrt`** (`implementation/libraries/rt`) is Veda's runtime: system
  calls and kernel objects, the heap (TLSF over private memory, returning
  pages that stay free to the system), threads with guard pages, futex
  locks that make no system call uncontended, time, logging and process
  creation. In
  a program on the standard library, the standard library is the runtime
  (entry point, panics, threads) and the POSIX layer holds the startup
  handles, but `vrt`'s heap stays the global allocator.
- **Process creation** happens in user space (`vrt::process::Spawn`):
  create a process, map the program's image, and start a thread with a
  bootstrap channel carrying the startup message (arguments, environment,
  working directory, role-tagged handles). A position-independent program
  is loaded at a random address (one of 2^28 pages). A crash's log line
  gives the failing address within the program's image (`target/` keeps
  Veda's programs with their symbols).
- **POSIX.** C programs and programs on the standard library run on musl,
  whose system calls go to `vposix` (`implementation/libraries/posix`), a
  POSIX layer in Rust: files are the VFS's, pipes and terminals kernel
  sockets, network sockets `netd`'s, Unix domain sockets named by the VFS,
  `epoll` a kernel port and threads kernel threads; `posix_spawn` starts
  processes (see [C on Veda](C.md)).
- **Services** register by name with the registry in `init`, and clients
  connect by name; a connection to a service not registered yet waits for
  it (up to 64 per process). Protocols are declared with `vipc`'s macros,
  which generate typed client stubs and server dispatch code; every
  request is answered, and events travel on a channel of their own.
- **Identity.** With each connection the registry gives the service the
  client's identity (`vproto::init::ClientIdentity`: process, program name,
  and whether it is a system service, an application or a program the
  kernel command line asked for), which no process can forge; services
  decide who may do what by it. A program that a process starts itself
  speaks for that process (the Terminal's programs speak for the
  Terminal). System services may register any name and keep it; other
  programs register only names of their own (`music`, `music/...`), so
  that none can stand in for a service not registered yet.
- **The launcher** in `init` starts programs and installed applications
  (their `.app` manifests), lists the running ones and ends them with their
  jobs. Applications may end applications; only system services, Task
  Manager, the Terminal's `kill -f` and programs the kernel command line
  asked for may end the system's processes. Only system services shut the
  machine down or restart it, and hear of crashes (`launcher::watch`).

## Devices

`devmgr` (`implementation/services/devmgr`) owns the PCI bus and the
firmware's ACPI tables. It starts Veda's drivers for the disks and gives
every other device to Linux in [the driver VM](DRIVERVM.md).

- **PCI.** A driver gets a `pcidev` channel for exactly its device (matched
  by vendor and device, or by class): its configuration space, its memory
  BARs, its MSIs or INTx line, and DMA memory. A BAR reaches its driver as
  the pages it is on, so one that shares a page with another function's BAR
  reaches none. `devmgr` places the BARs the firmware left unplaced, and
  puts back those a function loses (waking from D3hot, or reset alone).
- **Devices are not trusted.** The drivers' helpers (`vproto::pci`,
  `vvirtio`) check every offset a device reports against the memory it is
  in, and refuse a device that describes itself wrongly.
- **ACPI.** Some devices exist only in the ACPI tables (a laptop's touchpad
  and speaker amplifiers, the GPIO pins wired to them). `vacpi`
  (`implementation/libraries/acpi`) evaluates them with a small, bounded
  AML interpreter that initialises devices as ACPICA does but changes
  nothing and reads no I/O port or embedded controller. A driver can ask
  for the devices described below its PCI function, with their resources;
  `cargo orchestration acpi` shows, on the host, what `devmgr` makes of a
  machine's tables.
- **GPIO.** `devmgr` drives the GPIO pins of a driver's devices on its
  behalf, and no others (`vgpio`: Intel's Tiger Lake-LP pads, which Alder
  Lake-P kept), refusing pads the firmware owns or locked. It raises each
  pin's interrupt from its controller's line with `irq_raise`: an
  interrupt a program raises, which reaches a driver or a guest's processor
  as a line's does.
- **The IOMMU.** Where there is one (VT-d), the kernel remaps every
  interrupt, so a device raises only its own MSIs (`msi_create` takes a
  resource naming the PCI function, which `devmgr` holds). Devices reach
  memory untranslated until one is given to a guest; one whose reserved
  memory (an RMRR) is not whole pages stays the host's.
- **The driver VM.** `drivervm` gives its Linux guest every device but the
  disks and the platform's own functions, whole, their DMA confined by the
  IOMMU. It starts when the processors run virtual machines, an IOMMU
  confines devices and the system image has its Linux. When it ends without
  Linux powering it off, `devmgr` resets its devices and starts it again,
  later after each failure in a row, and not after a few.
- **USB** is Linux's: input devices reach the window system through `input`
  (`implementation/guest/input`), network adapters reach `netd` through
  `net`. Veda has no driver for USB disks yet.

## Storage and files

- **Disks.** `virtio-blk` (QEMU), `ahci` (SATA) and `nvme` (NVM Express)
  serve each disk through the `block` protocol as `block/<serial>`, so the
  VFS finds a disk by its serial number whatever the controller. One
  service loop (`vproto::block::serve`) checks every request (whole
  sectors, at most 32 KiB, on the disk). Only the VFS may use a disk, since
  raw sectors would bypass its access rules. A request a device does not
  finish in time fails and the device is reset, so that no command left to
  it writes into memory a later one uses.
- **The VFS** (`vfs`) serves `/system` read-only from the initrd, keeps
  `/home` and `/tmp` in memory, and has `null`, `zero`, `full`, `random`
  and `urandom` in `/dev`. With a disk whose serial is `veda-home`, `/home`
  is restored at boot and saved half a second after changes stop, into
  whichever of two checksummed slots does not hold the newest snapshot,
  header last, so an interrupted save never damages the previous one; boot
  restores the newest snapshot that reads back whole. Unchanged sample
  files are stored as references to the system image. The file systems,
  snapshots and access rules are `vfs-core`
  (`implementation/services/vfs/core`), tested on the host.
- **Private directories.** `/home/.private/<service>` belongs to the system
  service of that name: only it sees and changes what is inside. A
  connection can give up that access for good (`renounce_private`), as the
  agent's worker does. The root, `/home`, `/home/user`, `/tmp` and
  `/home/.private` cannot be removed, moved or replaced.
- **Open files** are connections of the `file` protocol, each an open file
  description with its own offset, which duplicates share, as POSIX
  descriptors do; a file removed while open stays usable until its last
  connection closes. Copies (`copy_file`) share contents until one changes.
- **Unix domain sockets** bound to paths are nodes of the file system
  (`bind_socket`) that the home disk does not keep; abstract names live in
  the VFS while the socket is open. The VFS passes a socket its connections
  and datagrams through an inbox channel; a connection's bytes then go
  between the two programs, past the VFS.
- **Limits.** A path is at most 4096 bytes (`MAX_PATH`), and a move that
  would take one beyond it is refused. Files grow only while memory is
  left for the rest of the system, and in `/home` while they fit on its
  disk. The VFS waits on all its connections at once, so they stay within
  the 1024 handles of one kernel wait, and a program holds at most a
  quarter of them (`TooMany`).
- **`vfiles`** is how applications use files: paths, formatting,
  thumbnails, tree walks of any depth, and a VFS client whose calls have
  deadlines, so that a VFS that stops answering fails calls rather than
  freezes programs. Its table of file types is the system's only list of
  which application opens which file.
- **The Trash** (`vfiles::trash`) is freedesktop.org's home trash. An
  item's `.trashinfo` is created first, as a file that must not exist yet,
  to reserve its name; then the item is renamed in. Items are restored only
  into the home folder or `/tmp`, whatever their info says, since any
  program can write an info file and the agent restores with a system
  service's access.

## Networking

Networking is three layers of processes; [Networking](NETWORKING.md) has
the detail.

| Layer | Processes | Does |
|-------|-----------|------|
| Drivers | `net`, `wifi` (`implementation/guest/`), in the driver VM | move Ethernet frames through shared-memory rings (`vproto::netring`) that neither side trusts; `wifi` offers Linux's radios as *managed* radios, which scan, join and encrypt on `wlan`'s commands |
| Wi-Fi | `wlan`, on `vwlan` | chooses networks, authenticates (open system or SAE), runs the key handshakes and gives the radio only the session keys, keeps saved networks, reconnects and roams; each radio is `wlan0` to `netd` |
| Network | `netd`, on `vnetstack` and smoltcp | interfaces, DHCP, IPv6 autoconfiguration, routes, a caching DNS resolver, sockets, the firewall |

- **Sockets.** Each `vnet` socket is a channel with credit-based flow
  control both ways, so a slow client holds back only its own sockets; the
  POSIX layer's sockets, which processes share, carry TCP bytes on kernel
  sockets instead.
- **The firewall** in `netd` judges every packet from another machine
  before smoltcp does: replies come in, and new connections and datagrams
  only where a port rule, or the rule of the application on the port, lets
  them; in stealth mode closed ports give no answer. When an application no
  rule covers starts listening, `netd` asks the user through the desktop
  shell, which alone answers (see [the firewall](NETWORKING.md#the-firewall)).
- **Who may do what**, by identity: network devices attach only from
  `devmgr` (and `wlan`), radios only from `devmgr`; only Settings changes
  the network's configuration; Settings, the Terminal, the shell and the
  agent change Wi-Fi; what tells where the machine is or has been, and the
  firewall's settings and log, go only to Settings and the Terminal. Every
  process has limits on its connections, sockets, lookups and watchers,
  with a share of each kept for system services.
- **TLS** is not a service: `vtls` (rustls with a pure-Rust cryptography
  provider) runs inside each application, so keys and plaintext never
  leave its process.
- **Failures.** Calls from a service to a driver have timeouts, and drivers
  report their state with one-way events, so a hung driver cannot block a
  service.

## The window system

The compositor (`implementation/services/compositor`) owns the screen and
serves `display` to applications, `input` to input drivers and
`displaydev` to a display driver. What it decides (windows, stacking,
focus, input, who may do what, limits) is `compositor-core` (`core/`),
tested on the host.

- **Surfaces.** A client draws into the back one of two buffers in a
  shared VMO and `present`s it. `FrameDone` comes once the other buffer is
  free, pacing every client to the display without copying pixels, and
  says when the frame is seen. A minimised window's frames are neither
  composed nor acknowledged until it is shown again. Clients' memory is
  checked when attached and never borrowed as a Rust slice, since the
  client may be writing it.
- **Composition** is damage driven, at most once per display frame. Where
  a display flips and the GPU can draw into its pictures, the GPU composes
  every frame with OpenGL ES straight into the next picture (`gpu.rs`):
  each window is a texture, and the effects are shaders from the
  processor's formulas. If the GPU fails a read-back check, or later takes
  two seconds over a frame, the processor composes again (the GPU is set
  up again once its service is back, three times at most). Elsewhere the
  processor composes into a back buffer and copies it to the screen.
- **The screen** (`screen.rs`) is the firmware's framebuffer, written in
  place at 60 Hz, until a display driver that can flip attaches (see
  [Displays and GPUs](#displays-and-gpus)). Then each frame goes into a
  picture that is not on the screen, shown from the next vertical blank,
  and the next is composed once it is shown: no frame is seen half drawn,
  and frames come at the display's pace. Only drivers that `devmgr` started
  may attach; if one goes away or leaves a flip undone for a second, frames
  go into all its pictures and the firmware's framebuffer. Whenever it
  starts, the compositor leaves that framebuffer alone until it knows the
  memory is still the screen's (no driver is coming, a driver for which it
  stays plain pixels attaches, or 20 s pass), since a driver may use it for
  its own (an Intel GPU's aperture).
- **Windows** form layers: the desktop, normal and borderless windows,
  panels (which reserve screen space), popups and notifications. A popup
  opens only for its application's focused window, at most half the
  screen, and goes with the focus. The compositor draws decorations, so a
  hung client's window can still be moved; a client that leaves a request
  to close unanswered for 5 s is shown as not responding, and asking again
  closes its connection.
- **Who may do what**, by identity: the desktop, panels, notifications,
  managing other windows and the start menu's and agent's keys are the
  shell's; listing the windows is the shell's and the agent's; the
  clipboard is the focused window's process's (the shell may set it too).
  Only drivers (`devmgr`'s, and the driver VM's programs, which speak as
  `devmgr`) report input or attach a display. A process has at most 64
  windows and 16 screens' worth of buffers.
- **Events and input.** Events to a client that falls behind wait in the
  compositor, merged so that nothing that carries state is lost (a press is
  dropped only with its release). One loop serves the input drivers first,
  then the clients, a few requests at a time. Keys go through a keymap (US
  layout) to the focused window, pointer events to the window under the
  pointer or to the one a held button was pressed in; the window keys
  (Super+arrows, Super+D, Alt+Tab, Alt+F4, tapping Super for the start
  menu) are the compositor's. Keys and buttons that an input driver held
  when it went away are released.

## Displays and GPUs

Displays and GPUs are Linux's, in [the driver VM](DRIVERVM.md), as are
their drivers' quirks and firmware.

- **Displays.** `kms` (`implementation/guest/kms`) drives the display whose
  memory holds the firmware's framebuffer, through Linux's KMS, and
  attaches it to the compositor through `displaydev`. The compositor draws
  into write-combining VMOs that Linux shows as they are (as dma-bufs),
  flipped at the vertical blank, the flips' times on Veda's clock, and
  scaled on a display that has no mode of their size.
- **GPUs.** The renderer (`implementation/guest/renderer`) carries out
  applications' OpenGL ES on Mesa's Gallium driver of the GPU (see
  [OpenGL ES and the GPU](#opengl-es-and-the-gpu)). Under QEMU the driver
  VM gets QEMU's VGA (Linux's bochs driver) and a virtio-gpu (Mesa's
  virgl), whose virglrenderer renders on the host's GPU.

## The desktop shell

The shell (`implementation/applications/shell`) is an ordinary process that
the compositor trusts as the system service `shell`. Its surfaces (the
desktop, the taskbar, the start menu and calendar, notifications) are
`vui::Host`s that draw from one shared model and change it through actions
the main loop performs. Windows come from the compositor (`list_windows`,
refreshed on `WindowsChanged`), installed applications from the launcher.

- **The `shell` protocol** lets applications change the wallpaper and post
  notifications, which show the application's own tile and name, at most
  five in ten seconds a process.
- **Notifications** also report crashes (`launcher::watch`) and carry the
  firewall's questions, whose buttons take clicks only a second after they
  appear.
- **Restart and Shut down** ask every application window to close, as its
  close button would, and power off once none is left.
- **Timeouts.** The shell waits at most ten seconds for a service it calls
  (two minutes for the disk to be written before power-off), so that a
  hung service cannot freeze the desktop.

## Graphics and the toolkit

| Library | Role |
|---------|------|
| `vraster` | paths (lines, quadratic and cubic curves, arcs), transforms, strokes, anti-aliased scanline rasterisation with coverage spans |
| `vfont` | fonts on `ttf-parser` (TrueType and CFF outlines, character maps, names): metrics, kerning, rasterised glyphs, a glyph bitmap cache, text layout |
| `vimage` | images on proven libraries: JPEG, PNG and BMP decoding by zune's decoders and QOI by the `qoi` crate, into Veda's pixel layout with limits, EXIF orientation and errors to show the user; scaling by `fast_image_resize`; encoding, for Paint and the build machine's tools: PNG and BMP by Veda's writers (PNG's deflate by `miniz_oxide`), JPEG by `jpeg-encoder`, QOI by `qoi` |
| `vgfx` | `Canvas` drawing on pixel buffers: fills, gradients, rounded rectangles, paths, bitmaps and pictures with rounded corners, shadows, text layout and rendering; downscaling and blurring; the damage between two frames |
| `vui` | the immediate-mode toolkit: windows, frame pacing, input, widgets (buttons, toggles, sliders, text boxes, lists, scroll areas, tabs), menus, modal dialogs, the file dialog, vector icons and the tray's status glyphs, the dark theme |
| `vtext` | the text editing model behind the Text Editor: line buffer, selections, grouped undo, search, soft-wrap layout, syntax highlighting |

In `vui`, the application's `update` lays out and draws the widgets each
frame, and they report their interactions in the same call; state that
outlives a frame lives in a per-window `UiState` keyed by widget ids.
Frames are drawn only when input arrives or an animation asks for one.

- **Only what changed is presented.** A window compares each frame with
  the one on screen and presents only the bands of rows that differ.
- **One text context per process** (`vui::SharedText`): the fonts are
  parsed once, and their glyphs rasterised into one cache.
- **Drawing costs what shows.** Paths are rasterised against the clip
  rectangle alone, and what drawing keeps lasts from frame to frame
  (`vgfx::DrawCache`). The loops over pixels stay scalar, since QEMU's TCG
  emulates SSE's integer multiplications and shuffles with slow helpers
  (`vgfx::color` says how to check the generated code).
- **Icons.** Line icons (`vui::Icon`) are paths on a 24-unit grid; the
  tray's status glyphs (`vui::status`) sit on a 20-pixel grid, their edges
  on whole pixels.

## OpenGL ES and the GPU

Applications get OpenGL ES 3.0, with GLSL ES 1.00 and 3.00, from `vgl`
(`implementation/libraries/gl`), in pure Rust. `vgl::veda::context`
renders on the GPU when the `gpu` service exists and its renderer has at
least the draw buffers, vertex attributes and uniform blocks `vgl` offers,
and in software on every CPU otherwise.

- **Shaders.** `vglsl` (`implementation/libraries/glsl`) parses, checks and
  links shaders and lowers each stage to an SSA form, which it optimises
  and verifies; `vglsl::ops` defines every operation once, for constant
  folding and the interpreter alike. Its back ends emit bytecode for the
  software renderer and TGSI for virglrenderer. Shader source is hostile
  input: everything it can grow is bounded and every index clamped, so no
  shader can make the compiler panic or work without bound, or a back end
  read outside its storage.
- **The API.** `vgl::Context` has every OpenGL ES 3.0 entry point, as a
  method named after the C function, checked as the specification
  requires. Vertex and index data always come from buffers (as in
  WebGL 2), and a draw that would read past one draws nothing. Below it, a
  `Backend` with Gallium's shape renders, and a new resource always reads
  as zero.
- **The software renderer** (`vgl::soft`) bins triangles into 64-pixel
  tiles, rasterises the tiles in parallel and shades 16 fragments at a
  time, with 4x multisampling, every ES 3.0 format, ETC2 and exact sRGB.
- **The GPU renderer** (`vgl::virgl`) speaks virglrenderer's protocol: it
  caches state objects, sends only what changed, moves data through a
  shared staging buffer and reuses released buffers once the GPU has
  passed their fence. It works around what virglrenderer's hosts get wrong
  (QEMU renders on the host's desktop OpenGL, or on ANGLE's OpenGL ES when
  headless):

| Host behaviour | What `vgl::virgl` does |
|----------------|------------------------|
| ANGLE gives up the context on a draw that reads past a buffer | drops such draws, checking indices against a copy of the index buffer in guest memory |
| The host's compiler flushes integer constants as denormal floats | builds integer constants at run time |
| Capturing into several transform feedback buffers needs `gl_NextBuffer`, which OpenGL ES lacks | gives each buffer a vertex shader variant and a pass of its own |
| On OpenGL ES, binding a 3D texture's slice to a framebuffer aborts virglrenderer | only samples and writes 3D textures there, handling their slices through 2D images |
| Hosts without `glCopyImageSubData` copy images through framebuffers, not in formats they cannot render to | copies images by blits, or through guest memory |
| Intel's driver on Windows records only zeros as transform feedback from a shader that writes `gl_ClipDistance` (as virglrenderer's all do), and does not order `glCopyImageSubData` after draws into its source | checks the host with a known point at the first capture, shading captured vertices in guest memory where it fails; gives blits a scissor, so that virglrenderer does not turn them into image copies |
| On desktop OpenGL, virglrenderer reads a 3D texture's later slices as they were before earlier writes in a submission | reads images back one per transfer |
| virglrenderer's new resources may hold another context's frames | zeroes them itself (`Transport::zeroes_new_resources` says whether the host does), and buffers reused from its pool |
| Stencil-only attachments are incomplete | keeps stencil in a depth-stencil format |

**The renderer** (`implementation/guest/renderer`) serves the `gpu`
protocol from the driver VM on Mesa's Gallium drivers: iris on i915 or xe
(Intel's GPUs), virgl on virtio-gpu (QEMU's), or softpipe for tests
(`drivervm.renderer=softpipe`). It is Mesa's build of a decoder
(`decoder/`, C) around a Rust half that serves the protocol, built by
`cargo orchestration linux`.

- **Sessions.** Each connection gets a context and a block of shared memory
  (commands, staging, query results, the fence word), laid out as
  `vproto::gpu::Session` states, which `vgl` checks before mapping it: the
  renderer is not trusted with a client's memory.
- **Checking.** Commands are copied out of the shared memory and checked
  word by word before Gallium's calls carry them out: a client can fail its
  own context, never the renderer. Gallium's state cache (`cso_context`)
  sees that an object a client deletes leaves nothing behind that the
  driver still uses, and every resource reads as zero before a client has
  it, since the drivers recycle memory between clients.
- **Formats** are virgl's (depth in the upper bits of
  `S8_UINT_Z24_UNORM`); on iris, which keeps depth in the lower bits, the
  renderer turns texels round as they are copied (`VR_DEPTH_LOW=1` does so
  on softpipe too, for tests). It also draws into memory it is given
  (`gpu::import`), such as the compositor's display pictures, which a GPU
  reaches as dma-bufs; virgl cannot, as its host draws into memory of its
  own.

## Audio

- **Devices** are Linux's, in [the driver VM](DRIVERVM.md). `alsa`
  attaches a card to the audio service's private `audiodev` protocol, as a
  native driver would; without one, a null output consumes audio in real
  time.
- **Who may attach.** A device attached hears everything the computer plays
  or becomes every program's microphone, so only drivers that `devmgr`
  starts may attach one, and the test microphone (`testmic`, which the
  kernel command line starts) an input device. Only the voice agent may
  duck the other sound.
- **Mixing and recording.** The `audio` service mixes every stream with
  exact rational resampling (polyphase windowed sinc in integer
  arithmetic), ramped gains, click-free pause and seek, master volume and
  mute. It converts the input for each capture stream, cancelling the echo
  for a stream that asks (the agent's microphone) with the mixed output as
  reference. The device records only while a capture stream is open.
- **Rings.** PCM never travels in messages: each stream, and the device
  link, is a single-producer, single-consumer ring of 16-bit frames in a
  shared VMO, with an event for back-pressure and a play position stamped
  with the device's time under a seqlock, so that players extrapolate
  progress smoothly. Each side clamps the other's counters, so a
  misbehaving client can only garble its own audio. Mixing and recording
  allocate nothing once they run.
- **`vaudio`** holds the formats (WAV, QOA), resampling, mixing, an FFT,
  the synthesiser and sequencer with which `generation/music` renders the
  album and the startup sound, and voice processing:

| Part | What it does |
|------|--------------|
| `aec::EchoCanceller` | removes the loudspeakers' sound from the microphone: a partitioned-block frequency-domain adaptive filter (tails up to 1 s) that holds still during double talk and re-converges when the echo path changes, a bulk delay estimator (0 to 500 ms) and a residual echo suppressor; 16 ms of latency at 16 kHz |
| `vad::Vad` | flags speech every 10 ms: minimum-statistics noise floors, a likelihood ratio test, click rejection, hysteresis per utterance |
| `level` | RMS and peak levels in dBFS, which the agent's animation uses |

## Video

Videos (`implementation/applications/videos`) plays videos with FFmpeg and
dav1d, decoded in a sandboxed process, `mediadec`
(`implementation/services/mediadec`), over `vproto::media`. In the app, the
window shows pictures, an engine thread runs the decoder, an audio thread
feeds the audio service and keeps the clock, and a second decoder makes
thumbnails. In `mediadec`, a Rust half serves the protocol and a C shim is
the only code that calls FFmpeg.

- **The sandbox.** `mediadec` starts with one handle, its control channel,
  and no registry: it reaches nothing but a read-only channel to the one
  file it is sent. Before it reads anything, it gives up the kernel log,
  processes and new executable memory, for good.
- **A decoder that fails** is killed and started again at the picture on
  the screen: one that crashes, does not answer a call in time, stops
  counting its *heartbeat* (a shared page) for 5 s, or takes more than half
  the machine's memory (256 MiB to 1.5 GiB). Twice at the same place,
  playback goes on 10 s further; three times in a minute, the video is
  given up. A watchdog thread ends a decoder whose app has gone, even while
  FFmpeg is stuck.
- **Pictures** are converted by swscale straight into slots of a *frame
  pool* shared with the app, at the window's size (each slot FREE, WRITING,
  READY or HELD, changed by compare-and-swap); **sound** by swresample into
  a ring of records that say when each is heard. What the output has played
  is the clock pictures follow (the system clock without sound, or while
  the audio service is away).
- **Timing.** From `FrameDone` the app knows when each frame will be seen,
  and shows the newest picture due by then, presenting only when the
  picture or the controls change. A decoder that falls behind is asked to
  leave out the loop filter, then pictures nothing refers to, then all but
  key frames. Every 10 s the app logs how playback went
  (`videos: playback:`).
- **Seeking** starts a new *generation* of the pool and the ring, so that
  what the decoder made for an older one is passed over.
- **Playback** is a state machine in `videos-core`, tested on the host.
  `verification/fakedec` stands in for `mediadec`, crashing, hanging or
  falling behind when asked; `systest` checks the real decoder (`checkasm`,
  and a sample video against its encoder's MD5s).

## Applications

Applications keep their logic in `no_std` libraries tested on the host
(`paint-core`, `calculator-core`, `videos-core`, `interceptor-game`...);
the program around each has the window, the files and the agent's actions,
which run the same code as the keyboard and mouse.

- **The Calculator** works in fractions of 128-bit integers
  (`num-rational`), fractions times π, or 64-bit floats where nothing exact
  will do (most roots, logarithms and sines), so 0.1 + 0.2 is 0.3; a result
  that would not fit gives the float, never a wrong fraction.
- **Paint**'s undo keeps only the 64x64 tiles each step changed, within an
  eighth of the machine's memory (32 MB to 512 MB), and draws an edit under
  way afresh from them.
- **Interceptor** (`implementation/applications/interceptor`), police
  pursuits through a bay city, is the largest OpenGL ES program. Its game
  (`game/`) is everything but the window, the keyboard and the speakers,
  so that its world, physics, drivers, pictures and sounds are tested on
  the host. The bay (a 6.2 km loop of road, 400 buildings, about 1900
  trees) is made at start-up from control points and a seed. It is lit
  physically in half floats with 4x multisampling and drawn instanced
  (about 130 draws a frame); in software it gets less of everything, at a
  resolution that adapts to the frame rate. The simulation steps at 120 Hz
  whatever the frame rate, on libm's floating point everywhere, so that the
  host's tests play the same chases Veda plays.

## The voice agent

The agent service (`implementation/services/agent`) holds the conversation
with Deepgram's Voice Agent API (a WebSocket over TLS through `vweb` and
`vtls`), the microphone and the voice, and listens for its name while
asleep (a local voice detector, then Deepgram's streaming recognition). A
worker thread runs what the language model calls: system functions and
the actions every `vui` application offers over the `agentapp` protocol
(describe, state, invoke). [The agent](AGENT.md) describes it in full.

- **Consent, by identity.** Actions that need consent wait for an approval
  that only the shell may give; only Settings may change the agent's
  settings.
- **Private store.** The settings, the Deepgram key, memory and permissions
  live in `/home/.private/agent`. The worker reaches the user's files
  through a connection of its own, which has given up private access, and
  checks every path.
- **`vagent`** (`implementation/libraries/agent`) holds the decisions that
  need no I/O (the protocol, the tools and their risks, the paths file
  actions may reach, the prompt, memory, the approval policy, the wake
  word), tested on the host.

## Testing

[Developing](DEVELOPING.md) has the commands in full.

| Command | Runs |
|---------|------|
| `cargo orchestration test` | the host's unit tests, the driver VM programs' tests on Linux, the `vgl` tests on the host's GPU through virglrenderer and through the renderer's decoder where the machine has them, then `systest` inside Veda |
| `cargo orchestration test --ui` | also every GUI and agent script |
| `cargo orchestration script verification/real/...` | by hand: the scripts that need the Internet and a Deepgram key |

- **Host unit tests** cover every library with platform-independent logic,
  in one Cargo run (`vrt`'s in one of its own), and a test of the build
  tool's fails if a crate with tests is left out. The network's check
  802.11 and its cryptography against published vectors, a station against
  an access point, two TCP/IP stacks over a simulated cable, and TLS
  against a rustls server and real certificate chains.
- **`systest`** runs inside Veda: kernel objects, threads, the file system,
  the launcher, crash reports, the window system's restart, and the C, C++
  and Rust test programs (`verification/c`, `verification/rust`,
  `verification/stdcrates`). `nettest` checks DNS, UDP, TCP, HTTP, HTTPS
  and ping, against a server on the host in the network cards' scripts
  (`nettest local`).
- **GUI scripts** (`verification/ui/*.vts`, in the language of
  `orchestration/src/script.rs`) drive QEMU through QMP (mouse, keyboard,
  waits on log lines, screenshots) and fail on panics. All are checked
  before the system is built, once, and each boots an overlay of the disk
  image within a deadline. QEMU's machine has an IOMMU, so the driver VM
  runs in every script. The agent's scripts (`verification/agent/*.vts`)
  talk to a stand-in for Deepgram, with a test microphone fed from the
  host.
- **Simulated hardware.** `vgpio::sim` is a GPIO controller as its
  registers behave, which `devmgr` drives when a test's ACPI table
  describes one (`drivervm-gpio.vts`).

## Architectures

Veda runs on AMD64 (x86-64). What depends on the architecture or its
platform is in each component's `arch/<architecture>/` directory (a
crate's in its `src/`), reached through its `arch` module
([Coding conventions](CODING.md) has the rules). On AMD64:

| Component | What is AMD64's (`arch/amd64/`) |
|-----------|---------------------------------|
| `implementation/boot/` | the kernel's page tables, the framebuffer's memory type, COM1, the TSC, the jump into the kernel |
| `implementation/kernel/` | see `arch` in [The kernel](#the-kernel) |
| `implementation/libraries/bootinfo` | the address space the loader hands over, and how the kernel is entered |
| `implementation/libraries/abi` | the calling convention of system calls, the user address space, a virtual processor's registers |
| `implementation/libraries/rt` | the entry points, the system call instruction, the C runtime's helpers; the driver VM's programs' Linux system calls |
| `implementation/libraries/posix` | Linux's system call numbers and the calls only x86-64 has, the thread pointer |
| `implementation/libraries/elf` | the executables' machine type and relative relocation (on every host, for the tools) |
| `implementation/libraries/build` | the user programs' target |
| `implementation/libraries/{hv,iommu}` | the guests' local APIC, CPUID, memory map, platform discovery and Linux's boot protocol; Intel VT-d's tables (on every host, for the tests) |
| `implementation/services/{devmgr,drivervm,init}` | the PC's own devices (keyboard controller, DMAR), the guest's memory map and boot, the I/O ports and IRQs `init` hands out |
| `verification/{systest,c,ui}` | what the tests expect of the architecture, and the GUI scripts that hold only on QEMU's PC |
| `orchestration/` | the Rust and C targets, the loader's boot path, QEMU's PC (q35, OVMF), its ACPI test tables |
| `.cargo/`, `implementation/ports/` | Cargo's settings of the targets; the ports' settings, patches (binutils, GCC, musl, Linux) and the driver VM kernel's options |

## Repository layout

Libraries are crates named after their directory with a `v` before it
(`implementation/libraries/rt` is `vrt`), but for `bootinfo` and `initrd`.
[Libraries](DEPENDENCIES.md) lists the third-party ones Veda builds on.

| Path | Contents |
|------|----------|
| `implementation/boot/` | UEFI loader (`vboot`) |
| `implementation/kernel/` | microkernel (`vkernel`) |
| `implementation/libraries/{abi,bootinfo,initrd,elf}` | shared formats and the kernel ABI |
| `implementation/libraries/{rt,heap,build}` | user runtime, allocator, build helper |
| `implementation/libraries/frames` | the searches of the kernel's bitmap of physical memory frames, a word at a time |
| `implementation/libraries/entropy` | the ChaCha20 random number generator and BLAKE2s entropy pool |
| `implementation/libraries/posix` | the POSIX layer under the C library |
| `implementation/libraries/{ipc,proto}` | message encoding and the service protocols |
| `implementation/libraries/{raster,font,image}` | vector rasterisation, fonts, images |
| `implementation/libraries/{gfx,ui,text}` | 2D drawing, the GUI toolkit, the text editing model |
| `implementation/libraries/splash` | the startup splash's picture, which the window system's processor and GPU paths draw alike |
| `implementation/libraries/files` | files for applications: paths, file types and the apps that open them, formatting, VFS access, thumbnails |
| `implementation/libraries/{glsl,gl}` | the GLSL ES compiler (with its TGSI back end), and OpenGL ES 3.0 with its software and GPU (virgl) renderers |
| `implementation/libraries/audio` | audio formats, resampling, mixing, FFT, the synthesiser, echo cancellation, voice activity detection and level metering |
| `implementation/libraries/virtio` | virtio device access shared by the drivers |
| `implementation/libraries/{acpi,gpio}` | the ACPI tables, the AML interpreter and resource templates, and Intel's GPIO pads (for `devmgr`) |
| `implementation/libraries/{hv,iommu}` | what the hypervisor and the IOMMU driver know that touches no hardware: the guests' platform, its ACPI tables and the bridge's ABI, and each architecture's virtual devices, boot protocol and IOMMU tables |
| `implementation/libraries/{netstack,net}` | the TCP/IP stack around smoltcp, with its firewall, and the networking API for applications |
| `implementation/libraries/tls` | the TLS client for applications: rustls and its pure-Rust cryptography provider |
| `implementation/libraries/web` | URLs, and HTTP/1.1 and WebSocket clients over TCP or TLS |
| `implementation/libraries/wlan` | IEEE 802.11 (frames, RSN, handshakes, SAE, station and access point) |
| `implementation/libraries/agent` | the voice agent's logic: the Deepgram protocol, tools, prompt, memory, approval policy, wake word |
| `implementation/services/`, `implementation/drivers/`, `implementation/applications/` | system services, drivers and applications (the applications with their manifests, `<name>.app`, and what they share in `common`) |
| `implementation/guest/` | the driver VM's Linux programs: its `init`, Veda's drivers for Linux (`input`, `alsa`, `net`, `wifi`, `kms`), the renderer, `logkeeper` (the system's log on the live system's stick), and its tests (`bridgetest`, `pcitest`, `gpiotest`, `efivartest`, `kmspause`) |
| `implementation/ports/` | third-party software built from source, most of it with a small patch of Veda's (see its `README.md`): the C toolchain (GCC, binutils, GMP, MPFR, MPC, musl), Rust for Veda's target, FFmpeg and dav1d, the driver VM's Linux (with zlib and elfutils for its build), its own toolchain (the same GCC, binutils and musl, unpatched) and Mesa, the crates Veda changes (smoltcp, zune-jpeg, libc, getrandom, rustix, errno, socket2, mio, tokio), and the fonts, unmodified |
| `verification/` | in-system tests, C and C++ test programs (`verification/c`), Rust's on its standard library (`verification/rust`, `verification/stdcrates`), GUI and agent scripts |
| `generation/` | host programs that make the system image's media at build time: wallpapers and sample pictures, music and the startup sound, sample videos |
| `orchestration/` | build orchestration, disk image creation, QEMU automation |
| `documentation/` | documentation |

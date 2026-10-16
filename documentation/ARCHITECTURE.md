<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Veda architecture

Veda is an x86-64 operating system built around a capability-based
microkernel. The kernel implements only isolation, scheduling, memory
management and inter-process communication; device drivers, file systems,
the window system and every application are ordinary user-space processes
that communicate over kernel channels.

## Boot

1. The firmware (OVMF under QEMU) loads the loader (`implementation/boot/`,
   `vboot`) from UEFI's boot path for the architecture
   (`\EFI\BOOT\BOOTX64.EFI` on AMD64) on the FAT32 EFI system partition
   built by `cargo orchestration build`.
2. `vboot` reads `\VEDA\BOOT.CFG`, `VKERNEL.ELF` and `INITRD.IMG`, picks a
   GOP graphics mode (about `BOOT.CFG`'s size, and of the display's own
   shape where the firmware gives the display's EDID: 1920x1200 rather
   than 1920x1080 on a 16:10 panel, which the firmware and later the
   display's driver then scale evenly, without bars; the kernel logs the
   modes there were, `boot: the firmware's screen modes: ...`), clears the
   screen to black, whatever the firmware left there, loads the
   kernel's segments (an ELF64 executable's, linked at its address),
   builds page tables (identity map, direct map at `0xFFFF800000000000`,
   kernel at `0xFFFFFFFF80000000`, each page with its segment's
   permissions, which it tells the kernel), reads the
   firmware's variables that an operating system may read (for the driver
   VM's Linux: `bootinfo::variables`), exits boot services
   and jumps to the kernel with a `bootinfo::BootInfo`. Firmware usually
   leaves its framebuffer uncached, where every write is a bus transaction
   of its own and a PC's screen shows it being painted from the top down;
   so the loader paints through page tables of its own that map the
   framebuffer write-combining by its page attributes (which the
   firmware's MTRRs cannot overrule), and the screen is black at once.
   The kernel logs how it went (`boot: the loader cleared the screen in
   ...`).
3. The kernel initialises memory, ACPI, the interrupt controllers, timers
   and the other CPUs, then starts `bin/init` from the initrd (the only
   program it loads itself).
4. `init` starts the system services and the desktop shell, then supervises
   them: the window system, the shell, audio, the network and Wi-Fi services
   and the agent are restarted if they crash (at most three times a
   minute). Drivers reconnect to a
   restarted compositor through the registry, which queues connections
   until a service registers again.

**The startup sequence.** The screen stays black while the kernel, the
services and the drivers start: the window system draws nothing until
the screen its frames go to from then on is up. Where the display is
the driver VM's (devmgr gave the display controller whose memory holds
the firmware's framebuffer to Linux, and tells the window system so:
`displaydev::expect`), that is once Linux's driver shows its first
picture, black too (on a PC's panel Linux keeps the link the firmware
set, so the panel stays on: Veda's patch to i915 keeps the firmware's
bits a colour at the first commit); without a driver coming, at once,
on the firmware's framebuffer. Then the window system fades the splash
in from black (the gradient and the ring: `vsplash`,
`implementation/libraries/splash`, drawn by the processor or by the GPU's
shaders from the same arithmetic;
`implementation/services/compositor/src/startup.rs`), and brings it to
life: a soft light gathers around the ring and breathes, with a glint
going round while the system works, and the name and the tagline ("The
agentic-native operating system") rise into view. Once the shell's
desktop and taskbar have each presented a frame, the splash has been up
for at least 2.1 s, and the
shell has the startup sound ready (`display::desktop_ready`: the sound
card is attached, or none is coming, as devmgr tells the audio service;
the window system waits for that 3 s at most), the ring swells and fades,
the words lift away and the desktop dissolves in, in under a second, and
the startup sound, `/system/sounds/startup.wav` (composed by
`generation/music`), starts with it: the shell holds it in a paused stream
and starts it when told the desktop appears (`WindowEvent::Appearing`).
So the desktop appears on its final screen, with its sound and its
devices there. The splash's gradient is dark and steps only a few dozen
times from top to bottom in each colour; it is dithered (an 8x8 ordered
pattern) so that it shows no bands. `init` asks for the sequence
(`splash`) and the sound (`startup`) only at system start: a window
system or shell restarted after a crash shows the desktop at once (as
soon as the screen may be drawn into: see the window system's screen),
and quietly.

The live system (`cargo orchestration iso`) starts the same way from a USB
stick or a disc. Its image is a hybrid ISO 9660 image: the boot files in the
ISO 9660 tree (what Rufus copies onto a stick), and a FAT file system with
the same files, which is both the El Torito UEFI boot image and, through an
MBR partition table in the system area, the EFI system partition of a stick
the image is written to as it is. Its `BOOT.CFG` puts `live` on the kernel
command line; `init` passes it on to `devmgr`, which then starts no disk
driver, and to `vfs`, which keeps `/home` in memory, so the computer's disks
are never read or written. The stick keeps the system's log instead
(`VEDA/LOGS`, which the image has, with room for it): the driver VM's
Linux, which has the stick on its USB controller, writes each start's
logs there (`logkeeper`, see [DRIVERVM.md](DRIVERVM.md)).

## The kernel (`implementation/kernel/`)

| Module | Responsibility |
|--------|----------------|
| `arch` | the processor architecture and its platform, behind one interface (`implementation/kernel/src/arch.rs`, which lists it): implemented in `implementation/kernel/src/arch/<architecture>/`, on AMD64 the GDT/TSS, IDT and entry paths, x2APIC/xAPIC and I/O APIC, the TSC, HPET and PIT, SMP bring-up, TLB shootdowns, page tables, the context switch, VMX and EPT, and the IOMMU (Intel VT-d: every device's interrupts remapped, devices' DMA passed through or confined to a guest's memory) |
| `mm` | frame allocator, page tables, kernel heap, kernel stacks/MMIO windows, VMOs, address spaces, user copies |
| `sched` | threads, priority round-robin scheduling, blocking with timeouts, idle loop |
| `object` | handles, signals, processes, channels, sockets, events, interrupts, I/O ports, resources |
| `syscall` | the system call layer (see `implementation/libraries/abi`) |
| `hv` | the hypervisor: guests and their virtual processors (see [the driver VM](DRIVERVM.md)), on the architecture's virtualization (`arch::hv`) |
| `acpi`, `time`, `percpu`, `futex`, `loader`, `log`, `panic` | supporting subsystems |

**Concurrency.** The kernel runs with interrupts disabled and holds a big
kernel lock (BKL) whenever it touches shared state, so kernel code is
effectively single-threaded while user code runs on all CPUs in parallel.
Kernel operations are short, which keeps contention low. Per-object spinlocks
provide interior mutability (and catch accidental recursion); TLB shootdowns
are acknowledged without the BKL so they cannot deadlock.

**Objects and capabilities.** Everything user space can touch is a kernel
object reached through a per-process handle with rights (`vabi::Rights`).
There is no ambient authority: a process can only use the objects it was
given. Hardware access requires *resource* handles: `init` receives the root
resource and derives narrow ones (an I/O port range, one IRQ, one MMIO range)
for each driver. A process's handle (with `MANAGE`) can end the process
alone or with every process it started, and those they started: its job,
as the Terminal's Ctrl+C ends a program. A process can also give up, for
good, what it would otherwise be allowed (`process_restrict`): reading the
kernel log, creating, listing and opening processes, and new executable
memory. The media decoder does so before it parses a file.

**Memory.** VMOs are the unit of memory. Anonymous VMOs are committed lazily
on page faults; they may be write-combining, for a device that reads them
without snooping the processor's caches (a display engine's pictures), and
then the kernel leaves no line of them in a cache as it zeroes, reads or
writes them. Address spaces map VMOs with per-mapping permissions, which
`vm_protect` can change for any part of a mapping but never beyond what the
handle the VMO was mapped with allows; parts of mappings can be unmapped.
*Private memory* (`vm_allocate`) is a VMO that no handle reaches, made for
one mapping: its pages are freed as soon as they are unmapped, and
`vm_decommit` frees them while the mapping stays (they read as zeros
afterwards), which is what C's `munmap` and `madvise` need. Frames are freed
only after the TLB shootdown that makes them unreachable. They come from a
bitmap: a single frame from where the last one was found, a run of them (a
DMA buffer, or the kernel heap's allocation of more than 2 KiB, such as the
buffer of a system call that carries data) as low as one fits, looked for a
word of the bitmap at a time (`vframes`), as the frames in use below the
first run can be most of memory. The kernel half of every address space is
shared.

**IPC.** Channels carry messages of up to 64 KiB plus up to 64 handles.
An endpoint has one handle at a time, unless its channel was made shared
(`channel_flags::SHARED`): then, as a socket's, it can be duplicated, so
several processes hold it, each message read by one of them (the POSIX
layer's sockets, which processes share). Bulk data (window buffers, audio
rings, file contents) travels in shared VMOs. *Sockets* are byte streams
for pipes, terminals, the POSIX layer's Unix domain stream sockets and
its TCP connections' bytes: a 256 KiB buffer each way, writes of up to
4096 bytes that are all-or-nothing (as POSIX promises for pipes),
half-closing to end a stream, endpoints that can be duplicated, so several
processes can share one, and handles that travel with the bytes they were
written with (a read takes one write's at most, and ends before the
next's), as descriptors do over Linux's Unix domain sockets. No channel or
socket may carry a handle that would keep it alive through what is queued
(`object::would_cycle`). Threads wait on signals
of several objects at once: `object_wait_many` registers a thread with
each of up to 1024 objects for one wait; a *port* keeps registrations
(`object_wait_async`: a packet with a key once the object's signals are
active, at once if they are, one packet per registration) and gives a
waiting thread the packets queued (`port_wait`), so that a wait costs
what is ready, not how many objects are watched. A port holds at most
65536 registrations and packets, and its handle cannot travel to another
process. `vipc`'s `WaitSet`, every service loop's, waits one call at a
time when it waits once, and on a port when it is kept from one wait to
the next (the file systems, the network, init and the window system keep
theirs: a wait then registers only what changed and what fired since the
last), or holds more handles than one call takes.

**Time.** The monotonic clock counts the processors' clock counter
(`arch::clock`). On AMD64 that is the TSC, calibrated against the HPET
(or the PIT without one). On an Intel processor not under a
hypervisor, the TSC frequency the processor reports itself (CPUID leaves
0x15 and 0x16, as Linux reads them) wins over a timer that measures more
than 5% off it; `dmesg time` says which was used. The clock and every
timer read the TSC of the processor they run on, so all processors' TSCs
must agree. They count the same clock, each offset by its
`IA32_TSC_ADJUST`, which firmware may leave different from one processor
to another; as Linux does, the kernel sets the boot processor's to 0
before the clock starts and each other processor's alike as it starts,
then checks with an exchange of readings that they agree and corrects
what is left (`dmesg smp` says by how much). The wall clock starts from
the firmware's RTC, which keeps UTC; the `tz=` boot option
(`cargo orchestration` passes the host's offset) gives local time, which
people see, while protocols and certificates use the UTC clock.

**Processors.** Each processor turns its caches on as it starts (a
processor started with INIT keeps the setting the firmware left it, which
may be off), and on Intel processors with hardware P-states lets the
processor choose its own speed, up to turbo, leaning towards performance;
without them it would stay at whatever speed the firmware left it at.
`dmesg cpu` says how fast the boot processor runs while busy.

**Scheduling.** 32 priorities, round-robin within a priority, 10 ms slices,
preemption on wake-up of a higher-priority thread, tickless one-shot timers,
eager switching of the FPU's and vector registers' state (with XSAVE on
AMD64). A thread can name an *exit
futex*: a word the kernel clears, and wakes a waiter on, once the thread
has ended and left its stack, which is how thread libraries join threads.

## User space

* **Executables.** Every program, and the kernel, is a static ELF64
  executable for the System V ABI (x86-64's on AMD64); only the loader is
  a PE/COFF image, the one format UEFI's firmware starts. Veda's programs
  are of two kinds (see [Rust on Veda](RUST.md)):
  * **on Rust's standard library**: the applications, and the services
    that gain from it (the shell, the agent). Rust compiles them for its
    own target for Veda, and GCC links them with the C library, which the
    standard library stands on: they are POSIX programs, as C programs
    are (static ELF64 executables built with GCC and musl; see
    [C on Veda](C.md));
  * **on `vrt` alone**: what the standard library stands on or runs
    before (`init`, the file system, the network stack), what drives
    hardware (the device manager, drivers, the driver VM) and what must
    answer in time (the window system, the sound mixer). They are built
    with `#![no_std]` for Linux's musl target, which gives stable Rust
    hard-float SSE code (the kernel is soft-float), and linked by
    `rust-lld` with nothing of Linux's or musl's: position-independent,
    their relative relocations packed (`DT_RELR`), and marked by a note
    (`velf::veda`).
* **`vrt`** is Veda's runtime: syscall wrappers and kernel objects, heap
  (TLSF over private memory: free pages go back to the system once they
  have stayed free for a while, and allocations of 4 MiB and more are
  mappings of their own), threads (each stack with guard pages below it,
  and freed when the thread ends, whether it was joined or let go),
  futex-based locks that make no system call uncontended, time, logging
  and process creation; for a program on `vrt` alone, also its entry point
  and startup message. In a program on the standard library, which links
  it too, the standard library is the runtime (entry point, panics,
  threads), the C library's POSIX layer has the startup handles and gives
  it a registry channel of the program's own (`veda_registry`), and its
  heap stays the global allocator.
* **Process creation** happens in user space (`vrt::process::Spawn`):
  create a process, map the program's image from VMOs, create a thread, and
  start it with a bootstrap channel carrying the startup message
  (arguments, environment, working directory, role-tagged handles). A
  program's segments are checked and mapped by
  `implementation/libraries/elf`, at a random address if it is
  position-independent (one of 2^28 pages); Veda's programs are relocated
  there first, then their RELRO is read-only. A POSIX program relocates
  itself, and its stack starts the way Linux starts one: arguments,
  environment and the auxiliary vector. A crash's line in the kernel's log
  gives where the failing code is in its program's image, which for
  Veda's programs is its address in the executable that `target/` keeps
  with its symbols (the system image's copies have none).
* **C programs** and programs on Rust's standard library run on musl,
  whose system calls go to `vposix` (`implementation/libraries/posix`), a
  POSIX layer in Rust linked into the C library: files are the VFS's open
  files, pipes and terminals are sockets, network sockets are `netd`'s
  and names its resolver's, Unix domain sockets are named by the VFS and
  are kernel sockets and channels between programs (descriptors travel
  on them), sockets of both kinds go to other processes, which share
  them, `epoll` waits with a kernel port (so tokio runs), threads are
  kernel threads, `posix_spawn` starts processes, and the standard output
  and error of a program started without them (by `init`) are the system
  log, a line at a time. The Terminal runs them with a terminal of their
  own (canonical line editing, `termios`, Ctrl+C). See [C on Veda](C.md).
* **Services** register with the registry in `init`; clients connect by name.
  Protocols are declared with the `vipc` macros, which generate typed client
  stubs and server dispatch code. With every connection the registry hands
  the service the client's identity (process, program name, whether it is
  a system service, an application, or a program the kernel command line
  asked for, as tests are), so a service can decide who may do what. (A process another started speaks for the process that started it,
  whose registry channel it was handed a copy of.) System services may
  register any name, and the names they register stay theirs; any other
  program registers only names of its own (Music `music`, or names under
  it: `music/...`), so that none can stand in for a service that has not
  registered yet. A connection to a service not registered yet waits for
  it, up to 64 per process.
* **The launcher** in `init` starts programs and installed applications,
  lists the running ones and ends them, each with what it started (its
  job): an application may end
  applications and what they started, while the system's processes (its
  services, and what those started, such as drivers) only the system
  services, the user's tools for managing the system (Task Manager and the
  Terminal's `kill -f`) and the programs the kernel command line asked for
  (`systest`). Only system services shut the machine down or restart it,
  and hear of crashes (`launcher::watch`).

## Devices (`implementation/services/devmgr`)

* `devmgr` enumerates the PCI bus and starts a driver for each device it
  has one for, matched by vendor and device or by PCI class. A driver gets
  a channel speaking the `pcidev` protocol for exactly its device: that
  device's configuration space (its BAR registers stay as the firmware set
  them), its memory BARs mapped, MSI interrupts (as many as the function
  can raise, no more) or the line its INTx is wired to (where the
  firmware's `_PRT` says), and DMA memory. Drivers never see other
  devices: a BAR reaches its driver as the pages it is on, so one on a
  page another function's BAR is also on reaches none. A memory BAR the
  firmware leaves unplaced (a laptop's does so for its serial bus
  controllers) devmgr places first, in a window of the PCI root bridge's
  (`_CRS`) where nothing else is, from the top down, on pages of its own;
  one it cannot place reaches no driver, and no physical VMO reaches the
  kernel's RAM. A function woken from D3hot that lost its BARs (it resets,
  unless it says it does not) gets them back, as does one its driver (or
  the driver VM's guest) resets alone (a function-level reset).
* What a device says of itself steers no driver's accesses elsewhere: the
  drivers' helpers (`vproto::pci`: the capability list, MSI-X, a BAR's
  registers, DMA memory; `vvirtio`'s structures, queues and MSI-X table)
  check every offset a device reports against the BAR or the memory it
  is in, and refuse a device that describes itself wrongly rather than
  crash or write past it.
* **ACPI.** `devmgr` also reads the firmware's ACPI tables (the kernel
  hands `init` the RSDP and the firmware's ACPI memory ranges, `init`
  hands them to `devmgr`), because some devices exist only there: a
  laptop's touchpad on an I2C bus, its speaker amplifiers on an SPI bus,
  the GPIO pins wired to them.
  `vacpi` (`implementation/libraries/acpi`) loads the DSDT and SSDTs into a
  namespace and evaluates its objects with a small AML interpreter:
  integer, string, buffer and package operations, buffer fields, control
  flow, method calls, and the fields of firmware memory regions (the
  firmware's settings, which decide what exists and where) and of PCI
  configuration space (the registers of the function at the `_ADR` of the
  device a region is in, on its root bridge's bus or below its bridges, as
  ACPICA finds it: an Intel PC's root bridge sizes its windows by its host
  bridge's), read with the accesses their fields' access types say (a
  device's register may answer only to its own width). Once the tables are
  loaded, it initialises the devices as an OS does: `\_INI`, `\_SB._INI` and
  the `_INI` of each present device, in ACPICA's order, in one evaluation
  whose stores every later one starts from (Intel's reference code for a
  laptop's touchpad gives it its id, I2C address and GPIO pin so, through
  buffer fields over its resource templates). It changes nothing: what a
  method stores lasts only for that evaluation (what the `_INI` methods
  store, for every one after), and it reads no I/O ports or embedded
  controller. Definitions inside an `If` outside any method exist only when
  their condition holds (that is how a board leaves out what its settings
  turn off); a region whose address, or a buffer field whose offset,
  depends on a later table is placed when first used, and a name whose
  data refers to objects defined after it (a `_PRT` package naming
  interrupt link devices) is evaluated again once they are, as ACPICA does.
  Every evaluation is bounded: in steps, in the bytes of the values it
  makes and copies (none larger than 1 MiB), in how deep its code and its
  values nest (which bounds the stack it takes) and in nested calls; so is
  a table's load as a whole, the initialisation, and the evaluations that
  answer one question (a device's identity, the root bridges); an owner
  may bound a phase of its work together (`Namespace::limit`). The tables
  and the firmware's ACPI memory are mapped cached, anything else AML
  reads uncached. On the laptop this was written for, all of a 480 KiB
  DSDT and sixteen SSDTs load, every conditional definition with them;
  `cargo orchestration acpi` shows what devmgr makes of a machine's
  tables, on the host.
* **The IOMMU.** Where the firmware describes one (VT-d), the kernel
  remaps every interrupt: a device's MSI raises an entry that names the
  device, so no other can raise it, and the compatibility format is
  blocked. Devices reach memory untranslated, as without an IOMMU, until
  one is given to a guest. MSIs are therefore made for a PCI function
  (`msi_create` takes a resource naming it, `resource_kind::PCI`), which
  `devmgr` holds for all of them; a function has at most as many at once
  as it can raise, which `devmgr` reads of it and the kernel counts. A device whose reserved memory (an
  RMRR: memory the firmware goes on using with it) the DMAR table gives
  as other than whole pages stays the host's (`viommu`'s
  `Reserved::range`): it could not be confined without knowing that
  memory. The table's other devices are unaffected, as on Linux.
* **The driver VM.** Every other device goes to Linux in a virtual machine
  ([the driver VM](DRIVERVM.md)): all but the disks and the platform's own
  functions. `devmgr`
  hands their `pcidev` channels to `drivervm`, which gives them to its
  guest whole, their DMA confined to the guest's memory by the IOMMU, and
  describes them in ACPI tables of its own, as a PC's firmware would. It
  starts when the processors run virtual machines and an IOMMU confines
  devices (the kernel says both in the boot information, `vabi::platform`),
  and the system image has its Linux. When it ends without Linux having
  powered it off, `devmgr` resets its devices and starts it again (as it
  tries again when it does not start), later after each failure in a row,
  and not after a few.
* A driver asks for the devices the firmware describes below its PCI
  function (its ACPI companion, found by `_ADR` under the PCI root bridge):
  their ids (`_HID`, `_CID`, `_UID`, `_SUB`), status and resources (`_CRS`:
  memory, ports, interrupts, GPIO and SPI or I2C connections, their
  controllers resolved), and what the driver VM's monitor passes on: their
  interrupt lines, what their `_DSM` answers, constant data (`_DSD`, an
  I2C controller's timing).
* **GPIO.** `devmgr` drives the GPIO pins those devices are wired to on
  the driver's behalf, and no others: a driver names a pin of one of its
  devices (counting the pins of its GPIO connections), `devmgr` finds the
  controller (its registers from its own `_CRS`) and the pad (`vgpio`,
  `implementation/libraries/gpio`). The controllers are Intel's (Tiger
  Lake-LP's pads, which Alder Lake-P kept; the layout as Linux's
  `pinctrl-tigerlake` has it): a pad owned by the firmware, or whose
  settings are locked, is refused; one in another function becomes a GPIO
  only when needed; a level is set before the output is enabled. A pin's
  interrupt (a `GpioInt` connection) is an interrupt `devmgr` raises: a
  controller has one line for all its pins, which `devmgr` waits on, and
  when it fires each pin with an interrupt pending raises its own: an edge,
  or level-triggered as its connection says, the pin masked until the
  interrupt is ended (the ending signals an event, and `devmgr` unmasks the
  pin, which fires again if its level lasts). Only a pad the firmware
  leaves in GPIO driver mode interrupts (one in ACPI mode raises the
  firmware's events; Linux refuses those too). The kernel's part is
  mechanism: an interrupt a program raises (`irq_raise`, with a handle's
  `SIGNAL` right; whoever it is for gets a handle without it), which
  reaches its driver, or a guest's processor, as a line's does, and signals
  the event its maker gave when a level-triggered one ends (as KVM's
  resample event does for VFIO). A test's ACPI table may describe a
  simulated controller (`VTST0002`): `vgpio::sim`'s registers, as the
  hardware keeps them, its even pins wired to the odd ones after them.

## Storage

* `virtio-blk` (QEMU), `ahci` (SATA disks: QEMU's q35 controller, many
  PCs) and `nvme` (NVM Express: the SSDs of most PCs since about 2016,
  QEMU's `nvme`) serve each disk (each namespace of an NVMe controller)
  through the `block` protocol under the name `block/<serial>` (where it
  is, `block/pci-00:04.0`, if it has no serial number), so the VFS finds
  a disk by its serial number whatever the controller. They share one
  service loop (`vproto::block::serve`), which checks each request
  against the disk before its driver sees it (whole sectors, at most
  32 KiB, all on the disk). Only the VFS may use a disk: anyone else's
  connection is closed, since reading or writing its sectors would bypass
  what the VFS decides of who reads and writes which files (`/system`,
  `/home/.private`). A request a device does not finish in time fails:
  an AHCI port is restarted, an NVMe controller reset and started again,
  and a virtio disk reset and given up, so that no command left to a
  device writes into memory a later one uses. `devmgr` matches drivers by
  vendor and device, and AHCI and NVMe controllers by their PCI class.
  The live system starts none of them (see Boot).
* `nvme` resets the controller and gives it an admin queue pair and one
  I/O queue pair, and runs a request at a time: READ, WRITE and FLUSH, the
  data in one buffer that PRP entries describe (a list past two pages), a
  request the controller cannot take whole split into several; IDENTIFY
  gives the serial number, the namespaces and their block sizes (512 or
  4096 bytes). It waits on an MSI-X or MSI interrupt, or polls.
* `vfs` serves `/system` straight from the initrd and keeps `/home` and
  `/tmp` in memory. If a disk with serial `veda-home` is attached, `/home`
  is restored from it at boot and written back half a second after changes
  stop (and before a shutdown from the start menu). The disk holds two
  snapshot slots with checksums; a save writes the slot not holding the
  newest snapshot, data first and header last, so an interrupted save never
  damages the previous one, and boot restores the newest snapshot that
  reads back whole, else the other, which is not overwritten until a newer
  one has replaced the bad one. Snapshots stream to and from the disk a
  transfer at a time (a second copy of the home directory in memory could
  be more than memory holds), every disk request has a deadline, and a save
  that fails is tried again, the changes counting as unsaved meanwhile.
  Unchanged sample files are stored as references to the system image. The
  file systems, their snapshots and the access rules are `vfs-core`
  (`implementation/services/vfs/core`), which the host tests; the service
  around them is `vfs`.
* Besides whole-file requests, the VFS opens files as connections of the
  `file` protocol, each an open file description with its own offset;
  duplicates share it, as POSIX descriptors do. A file removed while open
  stays readable and writable through its connections until the last one
  closes; one moved while open is wherever it is now (into `/home`, it
  counts against the home disk). Copies of a file (`copy_file`) share its
  contents until one of them changes, so copying takes neither time nor
  memory. Directories are listed a page at a time, each page in one message
  (`read_dir_page`; the client's `read_dir` asks for every page), and a
  directory's modification time changes whenever an entry is added,
  removed or renamed, so a program can tell from a stat whether a listing
  is current; its size is its number of entries. Stat results carry inode
  numbers and whether a file is a program (by its first bytes: ELF or `#!`;
  directory listings say so too), and `/dev` holds `null`, `zero`, `full`,
  `random` and `urandom`.
* A POSIX program's Unix domain socket bound to a path is a node of the
  writable file system (`bind_socket`), which `stat` and listings show
  and `remove` takes away, and which the home disk does not keep; one bound
  to an abstract name (Linux's) has the name, apart for each type of
  socket, in the VFS's abstract namespace while it is open. The VFS
  keeps the socket's inbox, a shared channel whose other end the program
  holds (and every process the socket went to, which take what comes on
  it in turn), and writes on it the connections to the socket once it
  listens (with the end the connecting program made: the bytes then go
  between the two programs, past the VFS) and the datagrams sent to it; a
  full inbox makes connecting and sending wait. Who connects or sends is
  told to the other end: the process, as it says, and the name of its
  socket, if the same program bound it.
* Every path is at most 4096 bytes long (`MAX_PATH`, as Linux's
  `PATH_MAX`), so every file can be named, and moves that would take a
  folder's contents deeper are refused. The root, `/home`, `/home/user`,
  `/tmp` and `/home/.private` cannot be removed, moved or replaced: moving
  `/home` would take the services' private directories out of the guard
  that keeps them private. A connection may renounce its access to those
  directories for good (`renounce_private`); the agent's worker, which acts
  on the user's files for others, should. Files grow only while memory is
  left for the rest of the system, and in `/home` while they fit on the
  home disk. A program holds at most a quarter of the VFS's connections
  (clients and open files), and all of them together stay within the 1024
  handles the kernel waits on at once (`vabi::WAIT_MANY_MAX`), as the VFS
  waits on all of them with one wait set; more are refused (`TooMany`).
* Applications work with files through `vfiles`: paths (resolving against a
  working directory and `~`, the read-only `/system`, wildcards, natural
  name order, free names for new items, and the user's well-known places,
  `vfiles::path::PLACES`), size and time formatting, a VFS client and the
  Trash, and, with the `thumbnails` feature, image thumbnails made on a
  background thread. The client's calls all have deadlines (a minute, more
  for whole-file transfers by their size), so a VFS that stops answering
  fails calls rather than freezes programs. It reads whole files copied,
  bounded (`read_limited`) or mapped (`map`), writes them, replaces one
  with another in one step (`replace`), lists directories of any size, and
  copies, moves, removes and measures whole trees with a stack of its own,
  however deep they are, files being copied by the VFS; a hook told of
  every item shows the progress and can stop the walk. Its table of file
  types is the system's only list of which application opens which file:
  Files, the Terminal's `open`, the desktop icons and the pickers of
  Photos, Music and Settings all use it.
* The Trash (`vfiles::trash`) is the home trash of freedesktop.org's Trash
  specification: deleted items go to `~/.local/share/Trash/files`, and for
  each a `NAME.trashinfo` in `info` records where it was and when it was
  deleted. The info is created first, as a file that must not exist yet,
  which reserves the name against other programs trashing at the same time;
  then the item is renamed into the Trash in one step (`/home` and `/tmp`
  are one file system). An item is restored only into the home folder or
  `/tmp`, never into the Trash or a `.private` folder, whatever its info
  says: info is a file any program can write, and the agent restores with
  a system service's access. How many items the Trash holds is the size of
  its folder of items, one stat. Files deletes to the Trash (Shift+Delete
  deletes for good), lists it with where each item was deleted from and
  when, and restores and empties it; the desktop's first icon is the
  Trash, which shows whether anything is in it and takes items of the
  desktop dragged onto it. The agent's `files` function and Files' agent
  actions delete to the Trash with the user's OK, restore without asking,
  and ask every time before emptying it. The Terminal's `rm` deletes for
  good, as Unix's does. Files copies, moves and deletes on a thread of its
  own, one operation after another, showing how far it is and stopping
  when asked.

## USB

USB is Linux's: the controllers go to [the driver VM](DRIVERVM.md) whole,
and Linux drives them, their hubs and the devices on them. Keyboards,
mice, tablets and touchscreens reach the window system's `input` service
through `input` (`implementation/guest/input`), which reads Linux's event
devices; network adapters reach the network service through `net`;
Bluetooth adapters get Linux's Bluetooth stack, which no service of Veda's
uses yet. Disks on USB are left alone: disks are Veda's, which has no
driver for them yet.

## Networking

Networking is three layers of processes (details in
[NETWORKING.md](NETWORKING.md)):

* **Drivers** only move frames, and they are Linux's, in [the driver
  VM](DRIVERVM.md): `net` (`implementation/guest/net`) offers its network
  cards' Ethernet frames to the network service; `wifi`
  (`implementation/guest/wifi`) offers its Wi-Fi radios to the Wi-Fi
  service as *managed* radios, which scan, join access points, encrypt and
  move Ethernet frames on the service's commands. Frames travel through
  shared-memory rings (`vproto::netring`) with wake-up events; neither side
  trusts the other's indices or lengths.
* **`wlan`** (the Wi-Fi service, built on `vwlan`) decides what to join,
  authenticates (open system; SAE, which it computes), runs the key
  handshakes and gives the radio only the session keys, keeps the saved
  networks and decides when to reconnect or roam. It presents each radio
  to `netd` as `wlan0`.
* **`netd`** (on `vnetstack` and smoltcp) runs interfaces, DHCP, IPv6
  autoconfiguration, routes, a caching DNS resolver and the sockets that
  applications use through `vnet` (and C programs and programs on Rust's
  standard library through the POSIX layer's sockets, `std::net`'s), one
  channel per socket with credit-based flow control in both directions:
  what a client has not taken stays in the stack (a TCP connection's
  window closes), so a slow client holds back only its own sockets. The
  POSIX layer's sockets go to other processes, which share them, so it
  opens them otherwise: their channels are shared, a TCP connection's
  bytes go on a kernel socket between the program and the service, whose
  buffers are the flow control, and a UDP socket's datagrams go without
  credit (the service reads no more of a socket's datagrams to send while
  256 KiB of them wait). A listener's connections, a connection's bytes
  and a socket's datagrams go to whichever process takes them first.

Both services check who is asking, by the identity the registry gives
each connection: network devices attach only from the device manager (and
`wlan`), radios only from the device manager; changing the network's
configuration is for Settings and the Terminal, and changing Wi-Fi for
them, the desktop shell and the agent; what tells where the machine is or
has been (access points' and adapters' addresses, saved networks,
diagnostics) goes only to Settings and the Terminal. Every process has
limits on its connections, sockets, lookups and watchers, and a share of
each is kept for system services.

TLS is not a service: `vtls` (rustls with a pure-Rust cryptography
provider) runs inside each application, over its `vnet` sockets, so keys
and plaintext never leave the process that uses them.

`netd` and `wlan` are restarted by `init` if they exit; drivers attach to
the new instance. Calls from a service to a driver have timeouts and the
driver reports its state with one-way events, so a hung or crashed driver
cannot block a service. Random numbers (TCP sequence numbers, DHCP and DNS
ids, Wi-Fi nonces and keys, TLS secrets) come from the kernel's ChaCha20
generator.

## The window system (`implementation/services/compositor`)

The compositor owns the screen and serves three protocols: `display` for
applications, `input` for input drivers and `displaydev` for a display
driver. What it decides (windows, stacking and focus, input, who may do
what, the limits clients are held to, what each client hears) is
`compositor-core` (`implementation/services/compositor/core`), tested on
the host; the program serves the protocols, maps clients' memory and
draws.

* **Surfaces.** A client creates a window and attaches two pixel buffers in
  a shared VMO. It draws into the back buffer and `present`s it; the
  compositor answers `FrameDone` once the previous buffer is no longer
  needed, which paces every client to the display without copying pixels.
  `FrameDone` also says when the frame is seen (the vertical blank it is
  flipped at) and how often frames follow, so that a client can time what
  it shows to the screen (Videos does). Buffers attached are shown from
  their first `present` on (a window being resized shows its last frame
  meanwhile), and clients hear their sizes (`Configure`) once a frame at
  most. A minimised window hears it is (`Configure` with `Minimized`); its
  frames are not composed, and are acknowledged only once it is shown
  again, so that a minimised video stops where it was.
* **Clients' memory** is checked as it is attached: sizes in `usize`
  against the VMO, rows no longer than the buffer is wide plus a little,
  buffers no larger than the screen. The compositor never borrows it as a
  Rust slice (the client may be writing it): it copies pixels out through
  raw pointers.
* **Composition** is damage driven: changed rectangles are recomposed from
  the bottom up (premultiplied alpha, shadows, rounded corners,
  open/close/minimise animations), at most once per display frame. At
  system start the screen shows the startup sequence instead (see
  [Boot](#boot)) until the desktop has drawn itself and dissolved in.
* **On the GPU** (`gpu.rs`). Where a display flips and the GPU can draw
  into its pictures (Linux's drivers of both in the driver VM: `kms`, and
  the renderer on Mesa's driver of the GPU), the GPU composes every frame
  with OpenGL ES (`vgl`),
  straight into the picture the display shows next, as modern window
  systems do. Each window is a texture, brought up to date where its
  client drew; shadows, borders, rounded corners and the startup
  sequence's gradient, light and ring are shaders, from the formulas the
  processor draws with; title bars, cursors, the switcher and the
  sequence's words are drawn once by the processor into textures. What a
  picture lacks of the screen is copied from the one that shows it, and a
  picture is asked for once the GPU's fence after its frame has
  signaled. The GPU is set up in a thread of its own once a driver
  attaches (the screen keeps moving meanwhile), then checked: the
  processor reads back what the GPU drew into a picture. If that fails,
  or the GPU later takes two seconds over a frame, frames are composed by
  the processor again (`compositor: frames are drawn by ...` says which).
* **On the processor** frames are composed into a back buffer and copied
  to the screen: in a virtual machine, or wherever the GPU cannot draw
  into the display's pictures.
* **The screen** (`screen.rs`). Frames go into the framebuffer the
  firmware left, written in place and paced by a 60 Hz timer, until a
  display driver that can flip attaches (`displaydev`; see
  [Displays and GPUs](#displays-and-gpus)). From then on each frame goes into
  one of the driver's pictures that is not on the screen; the compositor
  asks for it, the driver shows it from the next vertical blank, and the
  next frame is composed once it is shown. So the screen never shows a
  frame half drawn, frames come at the screen's own pace, and animations
  are timed for the vertical blank at which their frames will be seen.
  Each picture keeps what changed while the other was drawn into, and is
  brought up to date before its next frame. Only drivers (started by
  `devmgr`) may attach, and only to the screen the compositor draws on:
  the firmware's framebuffer address, the size and the pixel format must
  match. If the driver goes away, or leaves a flip undone for a second,
  frames go into all its pictures and the firmware's framebuffer, in
  place, whichever the screen shows. Whenever the compositor starts, at
  the system's start or again after a crash, it draws nothing into the
  firmware's framebuffer until devmgr says no driver is coming, a driver
  that keeps that memory only pixels attaches, or 20 s pass: a driver
  whose memory it is (an Intel GPU's aperture, which its tables point at
  memory of their own) may have it already. devmgr watches its connections
  to the compositor and the audio service: when one closes, it connects
  again and says once more what is coming, so a service started again
  after a crash hears it within a moment, not after 20 s.
* **Window kinds** form layers: the desktop, normal and borderless windows,
  panels (which reserve screen space), popups and notifications. A focused
  full-screen window rises above the panels. An application opens a popup
  only while one of its windows has the focus, for that window, and no
  larger than half the screen; the compositor hides it as soon as the
  focus goes elsewhere (and asks its client to close it). Decorations are
  drawn by the compositor, so a hung client's window can still be moved.
  A client that leaves a request to close one of its windows unanswered
  (asking the compositor for nothing for 5 s) is shown as not responding
  in the window's title, and asking again closes its connection, which
  takes its windows away; ending the process is the task manager's.
* **Who may do what** follows who opened the connection, as `init` vouches
  for it: the desktop, panels and notifications, managing other windows
  (activating, minimising, arranging, closing) and the start menu's and
  agent's keys are the shell's (the system service `shell`); listing the
  windows is the shell's and the agent's; the clipboard is the focused
  window's process's (the shell may set it too). Only drivers (devmgr's,
  and the driver VM's programs, which speak as devmgr) may report input or
  attach a display. A process has 64 windows and 16 screens' worth of
  buffers at most; titles are cut at 256 bytes.
* **Events** to a client that falls behind wait in the compositor, and lose
  nothing that carries state: pointer motion is merged, the latest size
  and focus replace older ones, and once a client is far behind a key or
  button pressed is dropped with its release, never a release without its
  press. One loop serves everything: each wake-up serves the input drivers
  first, then each client with requests waiting, a few requests at a time
  in turn, composing a frame between them when one is due.
* **Input**: keyboard events go through a keymap (US layout, modifiers, key
  repeat) to the focused window; pointer events go to the window under the
  pointer, or, while a button is held, to the one it was pressed in, which
  hears of every other button until all are released. Dragging a window to
  the top or a side edge maximises it or snaps it to that half of the
  screen; Super+arrows do the same from the keyboard and Super+D shows the
  desktop (and brings the windows back as they were: maximised, full
  screen). Alt+Tab shows the window switcher (live thumbnails, as many
  windows as fit on the screen), Alt+F4 closes, and tapping Super sends
  `StartMenuKey` to the shell. When an input driver goes away (the driver
  VM restarting), the keys and buttons it held are released for it.

## Displays and GPUs

Drawing into the picture the screen is showing tears, and frames paced by
a timer drift against the screen's own rhythm: what looks smooth in a
virtual machine's window, whose host shows whole frames, judders on a
laptop's panel. Displays and GPUs are Linux's, in
[the driver VM](DRIVERVM.md), as are their drivers' quirks and firmware:

* **Displays.** `kms` (`implementation/guest/kms`) drives, through Linux's
  KMS, the display whose memory holds the firmware's framebuffer (a laptop
  with two GPUs has outputs on both), and attaches it to the window system
  through `displaydev`: the compositor draws into pictures of Veda's
  memory, which Linux's driver shows as they are (dma-bufs of the VMOs,
  write-combining memory) and flips at the vertical blank, the flips' times
  on Veda's clock; scaled by the display engine to a display that has no
  mode of their size (a laptop's panel). Until it attaches, and whenever
  the driver VM is gone, the compositor draws into the firmware's
  framebuffer, as long as that memory is the screen's: not before it knows
  so (see the window system's screen), nor once a driver that uses it for
  its own (i915) has had the display.
* **GPUs.** The renderer (`implementation/guest/renderer`) carries out
  applications' OpenGL ES command streams on Mesa's Gallium driver of the
  GPU, over the GPU's Linux driver (see below).

Under QEMU, the driver VM gets QEMU's VGA (Linux's bochs driver) and, for
3D, a virtio-gpu (Linux's virtio_gpu, Mesa's virgl), whose virglrenderer
renders on the host's GPU; `verification/ui/drivervm-*.vts` cover both.

## The desktop shell (`implementation/applications/shell`)

The shell is an ordinary process that the compositor trusts, as the
system service named `shell`, with shell surfaces: the desktop (wallpaper
and icons), the taskbar (a panel), the start menu and calendar (popups)
and notifications. Each surface is a `vui::Host`; they draw from one
shared model and request changes as actions (launch an app, activate or
minimise a window, toggle a popup) that the main loop performs. Running
windows come from the compositor (`list_windows`, refreshed on
`WindowsChanged`); installed applications come from the launcher in
`init`, which reads the `.app` manifests. The
shell serves the `shell` protocol so applications can change the wallpaper
and post notifications; `vproto::shell::ShellLink` makes these calls from a
background thread, because setting a wallpaper decodes and scales a large
picture first, which the shell does on a thread of its own, answering once
the picture is set (one at a time for each client). A notification an
application posts shows that application's tile and name, whatever it says
of itself, and a process posts at most five in ten seconds. When an
application crashes (a CPU fault or a panic), `init` reports it on the
channels handed out by `launcher::watch`, and the shell tells the user with
a notification. Restart and Shut down first ask every application window to
close, as its close button would (an application with unsaved work asks
whether to save it), and power off once none is left; what is still open
after a few seconds is listed, to cancel or to go on without. The shell
waits at most ten seconds for a service it calls (two minutes for the
disk to be written before the machine powers off), so that a service that
hangs cannot freeze the desktop.

## Graphics and the toolkit

| Library | Role |
|---------|------|
| `vraster` | paths (lines, quadratic and cubic curves, arcs), transforms, strokes, anti-aliased scanline rasterisation with coverage spans |
| `vfont` | fonts on `ttf-parser` (TrueType and CFF outlines, character maps, names): metrics, kerning, rasterized glyphs, a glyph bitmap cache, text layout |
| `vimage` | images on proven libraries: JPEG, PNG and BMP decoding by zune's decoders and QOI by the `qoi` crate, into Veda's pixel layout with limits, EXIF orientation and errors to show the user; scaling by `fast_image_resize`; encoding, for Paint and the build machine's tools: PNG and BMP by Veda's writers (PNG's deflate by `miniz_oxide`), JPEG by `jpeg-encoder`, QOI by `qoi` |
| `vgfx` | `Canvas` drawing on pixel buffers: fills, gradients, rounded rectangles, paths, bitmaps and pictures with rounded corners, shadows, text layout and rendering; downscaling and blurring; the damage between two frames |
| `vui` | the immediate-mode toolkit: windows, frame pacing, input, widgets (buttons, toggles, sliders, text boxes, lists, scroll areas, tabs), menus, modal dialogs, the file dialog, vector icons and the tray's status glyphs, the dark theme |
| `vtext` | the text editing model behind the Text Editor: line buffer, selections, grouped undo, search, soft-wrap layout, syntax highlighting |

`vui` is immediate mode: each frame the application's `update` lays out and
draws widgets, and the widgets report their interactions in the same call.
State that must survive between frames (focus, scroll offsets, text cursors,
open menus, animations) lives in a per-window `UiState` keyed by widget ids.
Frames are drawn only when input arrives or an animation asks for one.

* **What a frame changed.** A frame is drawn whole, but the window compares
  it with the frame on screen, row by row, and presents only the bands of
  rows that differ (or nothing, if none does): a caret's blink, a fading
  hover or a clock's tick costs the compositor that much of a redraw, not
  the whole window's.
* **One text context per process.** The windows of a process draw with one
  text context (`vui::SharedText`): the fonts parsed once, their glyphs
  rasterized into one cache, behind a lock for threads. The shell has a
  window for each popup, tooltip and notification, each of which would
  otherwise parse the fonts and rasterize the same glyphs again.
* **The keyboard** is an open menu's first, then a modal dialog's: while
  one is up (from the frame after it appears), the widgets outside it see
  no keys or typed text and none has the focus, and its content gets them;
  keys that come as it goes are the window's, in the frame after.
* **Drawing costs what shows.** Paths are rasterized against the clip
  rectangle alone, and a shadow draws only its part in it, so a redraw of a
  small part of a window pays for that part; what drawing keeps (the
  rasterizer's buffers, the templates of shadows) lasts from frame to frame
  (`vgfx::DrawCache`). The loops over pixels stay scalar, since QEMU's TCG
  emulates SSE's integer multiplications and shuffles with slow helpers
  (`vgfx::color` says how to check the generated code).

The Text Editor keeps its layout of a document up to date line by line:
the document records which lines its edits changed (`vtext::LineChange`),
and only those are laid out again.

Icons come in two kinds. The line icons (`vui::Icon`) are paths on a
24-unit grid, drawn at any size. The status glyphs (`vui::status`: the
network, with its signal, and the sound, with its level) are drawn for the
size of the taskbar's tray: solid shapes on a 20-pixel grid whose straight
edges fall on whole pixels, so that they stay crisp on a 1x screen. Each is
drawn into a small layer first, where a slash or a badge (the agent's ring
uses them too) cuts a gap into what it crosses.

## Paint and the Calculator (`implementation/applications/{paint,calculator}`)

Each keeps what it works out in a `no_std` library of its own, tested on
the host (`paint-core`, `calculator-core`); the program around it has the
window, the files and the agent's actions, which run the same code as the
keyboard and mouse.

* **Exact numbers.** The Calculator's numbers are fractions of 128-bit
  integers (`num-rational`), fractions times π, or 64-bit floats where
  nothing exact will do (most roots, logarithms and sines). Sums,
  products, quotients and whole powers of fractions stay fractions, so
  0.1 + 0.2 is 0.3, 1 ÷ 3 × 3 is 1 and the sine of π is 0; an operation
  whose exact result would not fit gives the float, never a wrong
  fraction. Fractions are written out by long division, every digit
  right, and floats to 15 significant digits.
* **Expressions** are parsed by precedence climbing, written with the
  keys' signs (×, √, π) or the keyboard's (`*`, `sqrt`, `pi`), with
  products side by side (2π) and parentheses closed at the end. They are
  worked out as numbers or, in Programmer mode, as two's-complement words
  of 8 to 64 bits in any of four bases, with bitwise operators, shifts and
  rotations. The converter's units are fractions of their kind's base
  unit, exact where their definitions are (an inch is 0.0254 m),
  temperatures with their offsets.
* **Undo in tiles.** Paint's history keeps, for each step, only the 64x64
  tiles it changed, within a budget of an eighth of the machine's memory
  (32 MB to 512 MB); undo and redo swap them with the picture's. An edit
  under way (a stroke, a shape still being shaped, text being typed, a
  piece of the picture being moved, an effect's preview) is drawn afresh
  from those tiles as it changes. A stroke's coverage is kept in a mask,
  so a semi-transparent stroke that crosses itself does not darken there.
* **The picture on screen.** Each pixel of the window shows the picture's
  pixel under it: whole pixels zoomed in (with a pixel grid from 800%),
  and zoomed out, pixels of halved copies of the picture, made when first
  needed and kept, over a checkerboard where the picture is transparent.
  Pictures are read and written on a thread of their own (PNG, JPEG, BMP
  and QOI, by `vimage`), so that the window answers while a large one is
  encoded.

## OpenGL ES and the GPU (`implementation/libraries/{glsl,gl}`, `implementation/guest/renderer`)

Applications get OpenGL ES 3.0, with GLSL ES 1.00 and 3.00, from `vgl`,
in pure Rust: *Interceptor* (below) is its largest program.

* **The shading language.** `vglsl` preprocesses, parses and checks
  shaders, links programs (attribute, uniform and varying locations,
  counted as GLSL ES's packing rules count them, std140 blocks, transform
  feedback), and lowers each stage to an SSA form over structured control
  flow, which it optimises and verifies. Linking places every variable
  once (its slots, std140 offsets and samplers), and the SSA form is built
  from that placement. `vglsl::ops` is the one definition of what every
  operation computes: constant folding and the interpreter call the same
  function for each. Two back ends take the SSA form: bytecode for the
  software renderer's SIMD interpreter, and TGSI text for virglrenderer
  (`vglsl::tgsi`). Shader source is hostile input, so everything it can
  grow is bounded (nesting, macro expansion, types' sizes, constant
  folding, inlining, the code a program comes to, and the loop iterations
  the interpreter runs for a draw) and every index is clamped to its
  array: no shader makes the compiler panic, overflow its stack or work
  without bound, or makes either back end read outside the storage it is
  given.
* **The API.** `vgl::Context` has every OpenGL ES 3.0 entry point, as a
  method named after the C function, and checks every call as the
  specification requires; it keeps the GL's objects and state and converts
  pixel data. Vertex and index data always come from buffers (as in
  WebGL 2), and a draw that would read past the end of one draws nothing:
  what it would read is undefined, and hosts that check refuse it and give
  up the context. The context checks what a draw's arguments tell (its
  indices, and the vertices of arrays and instances); the renderers check
  the vertices indices name, as they read them. Below it, a `Backend` with
  Gallium's shape renders: resources, surfaces, and draws described by
  complete state. A new resource reads as zero on every renderer, never as
  what another context left in the memory it was given.
* **The software renderer** (`vgl::soft`) records draws per framebuffer,
  shades vertices, bins triangles into 64-pixel tiles and rasterises the
  tiles in parallel, running fragment shaders on 16 lanes at a time (four
  2x2 quads): 4x multisampling, every ES 3.0 format, ETC2, exact sRGB.
* **The GPU renderer** (`vgl::virgl`) speaks virglrenderer's protocol,
  which Veda's renderer carries out on the GPU (below), and which
  virglrenderer replays with a host's OpenGL in vgl's host tests. State
  objects are made once per distinct state and cached (a bounded number
  of each kind, the least recently used going first), draws set only what
  changed, data moves through a staging buffer shared with the device, in
  command order, and presenting blits the frame (resolved, flipped and
  scaled) into an image the window's size that is read back ready to
  copy. Making a resource and destroying one each take a call to the
  renderer, which streamed vertices (a buffer re-specified every frame)
  would make many times a frame: buffers the application lets go of are
  kept, and new ones are made of them once the GPU has passed the fence
  after the last commands that could read them; a resource is destroyed
  on the host only once the commands naming it have been submitted. What
  hosts refuse is handled first: draws that would read past a buffer are
  dropped (ANGLE would give up the context), which for indexed draws the
  renderer tells from a copy of the index buffer kept in guest memory;
  integer constants are built at run time (the host's compiler flushes
  them as denormal floats), and separate transform feedback buffers take
  a pass each on OpenGL ES hosts. So is what would
  end the host: on OpenGL ES, virglrenderer binds a 3D texture's slice to
  a framebuffer with a function ANGLE lacks, and the process aborts. 3D
  textures there are only sampled and written; their slices are read by
  drawing their texels into a 2D image, drawn into through a 2D copy
  that is written back, and their mipmaps are made in guest memory, as
  the software renderer makes them. Copies between images, which hosts
  without `glCopyImageSubData` make through framebuffers (a layer at a
  time, and not for formats they cannot render to), are made by blits, a
  layer at a time, or through guest memory. Some drivers err in ways only
  the host's desktop OpenGL shows (QEMU's window renders on it; headless
  QEMU on ANGLE): Intel's on Windows records only zeros as transform
  feedback from any vertex shader that writes `gl_ClipDistance`, which
  virglrenderer's all do there, and does not order
  `glCopyImageSubData` after draws into its source. The first draw that
  captures checks the host with a known point, and where it fails the
  captured vertices are shaded in guest memory by the software
  renderer's vertex stage (the host still draws what is seen); and blits
  carry a scissor that keeps virglrenderer from turning them into image
  copies. On desktop OpenGL, too, virglrenderer reads a 3D texture's
  slices after the first as they were before writes earlier in the same
  submission, so images are read back one per transfer. New resources
  read as zero, as the software renderer's do: Veda's renderer makes them
  so itself (below); on virglrenderer, whose memory may hold another
  context's frames, and for a buffer reused from the pool, `vgl` does
  (the transport says which, `Transport::zeroes_new_resources`):
  renderbuffers, and 2D and cube images the host can draw into, are
  cleared, other images written with zeros through staging, and buffers
  copied from a buffer of zeros. Stencil-only
  render targets are kept in a depth-stencil format (hosts find
  stencil-only attachments incomplete), whose depth draws ignore: a test
  without its attachment is turned off, as OpenGL ES has it pass.
* **The renderer** (`implementation/guest/renderer`) serves the `gpu`
  protocol from the driver VM: each connection gets a context and a block
  of Veda's memory shared with it (commands, staging, query results, the
  fence word), laid out by rules the protocol states
  (`vproto::gpu::Session`), which `vgl` checks before it maps the block:
  the renderer is not trusted with a client's memory. Its commands are
  carried out on one of Mesa's Gallium drivers, over the GPU's Linux
  driver: iris on i915 or xe (Intel's GPUs), virgl on virtio-gpu (QEMU's,
  which renders on the host's GPU), or softpipe
  (`drivervm.renderer=softpipe`, for tests). The program is Mesa's
  build of the decoder (`decoder/`, C) around its Rust half, which serves
  the protocol over the bridge; `cargo orchestration linux` builds Mesa for the
  guest with a toolchain of its own (`implementation/ports/linux/build.sh`).
  The decoder takes virgl's commands straight to Gallium's calls, checking
  every word first (a client can fail its own context, never the renderer),
  with Gallium's state cache (`cso_context`) between it and the driver, so
  that a client deleting an object leaves nothing behind that the driver
  still uses. Every resource it creates reads as zero before the client
  has it: the drivers recycle memory freed in the process, which serves
  every client, and a client must never see another's frames (buffers
  and images are cleared on the device where the driver can, compressed
  images on softpipe written with zeros through a mapping,
  multisampled ones cleared as render targets or depth buffers). Commands
  are copied out of the shared memory before they are decoded. Formats are virgl's, which OpenGL hosts take: 24-bit depth with
  stencil is `S8_UINT_Z24_UNORM`, depth in the upper bits as
  `GL_UNSIGNED_INT_24_8` packs it. iris has depth only in the lower bits
  (`Z24_UNORM_S8_UINT`), so on iris the renderer keeps those formats that
  way round and turns each texel round when it is copied in or out
  (`VR_DEPTH_LOW=1` makes it do so on softpipe too, for the host's tests).
  A buffer may be bound anywhere later, whatever it was made for, and
  drivers are told so, but virgl, whose host takes a buffer for one use
  and lets it serve them all; a cube map's faces are moved one at a time,
  as Mesa's OpenGL moves them.
  It also draws into memory it is given (`gpu::import`): a display's
  picture, which the compositor makes a render target of
  (`renderbuffer_storage_external`, as `glEGLImageTargetRenderbufferStorageOES`
  makes one of a display's buffer). For a GPU the memory becomes a dma-buf
  of the VMO (the bridge's), which its driver imports as Linux's would,
  linear, and reaches through the IOMMU; softpipe draws into it as the
  renderer maps it, through a window system of the renderer's own
  (`device.c`). virgl cannot (its host draws into memory of its own).

`vgl::veda::context` renders on the GPU when the `gpu` service exists,
and its renderer has at least the draw buffers, vertex attributes and
uniform blocks `vgl` offers applications, and in software on every CPU
otherwise. The difference is large: on the Zenbook's processors
Interceptor draws 5 frames a second at 640x360, and through the driver VM
on QEMU's virtual GPU 40 at 1920x1080.

The virgl renderer is tested on the host: `vgl::virgl::host` calls
virglrenderer as QEMU does, on OpenGL contexts made as QEMU makes them,
and runs the whole `vgl` test suite on the host's GPU
(`VGL_TEST_BACKEND=virgl`): it loads the system's virglrenderer and
renders through EGL on a GPU's render node (GBM), on desktop OpenGL as
QEMU does and on OpenGL ES (`VGL_TEST_HOST=gles`). The suite also runs
through the renderer's decoder (`VGL_TEST_BACKEND=gallium`,
`vgl::virgl::gallium`, which loads the decoder built with Mesa for the
host as `vgallium.so`): on softpipe, where it falls short of a GPU (no
multisampling, points an eighth of a pixel off, depth filtered before it
is compared) the tests say so; and on Mesa's virgl over virglrenderer's
test server (`VGL_GALLIUM_DEVICE=virgl`), as the driver VM's renderer
renders under QEMU.

## Interceptor (`implementation/applications/interceptor`)

*Interceptor*, police pursuits through a bay city, is OpenGL ES 3.0's
largest program. Its game (`implementation/applications/interceptor/game`,
`no_std`) is everything but the window, the keyboard and the speakers, so
that its world, physics, drivers, missions, pictures and sounds are tested
on the host; the program around it
(`implementation/applications/interceptor/src`) has the window, the menus
and the dashboard (`vgfx`), the speakers' thread, the save file and the
voice agent's link. Its vectors, matrices and quaternions are `glam`'s, its
random numbers `rand`'s (PCG32, seeded, so that the bay and its traffic are
the same every time), and the land's noise FastNoise Lite's.

* **Veda Bay** is made at start-up from a few dozen control points and a
  seed, in a fifth of a second: a 6.2 km loop of road (a Catmull-Rom
  spline sampled every 2 m, with its sections' cross-sections: downtown's
  pavements, the hills' guardrails, a tunnel, the coast, a suspension
  bridge, the port); the land around it, a height field whose fields
  (shores, roughness, the road's height) follow the nearest stretch of road
  (a distance transform finds it for every vertex) and are blurred where
  stretches meet; downtown's 400 buildings, the street lights, about 1900
  trees, the bridge's towers and cables, billboards and neon signs.
* **The pictures** are lit physically (GGX and Lambert, the sky's light
  and reflections from a cube map painted for the time of day), in half
  floats with 4x multisampling, then bloomed, tone mapped and graded per
  time of day. The street lights' pools are painted once into a map of the
  world that every lit shader reads; the sun's shadows come from a shadow
  map snapped to its texels; headlights and light bars are up to sixteen
  dynamic lights. The city, the street lights, the trees and the cars are
  drawn instanced (about 130 draws a frame), the buffers of what changes
  each frame rewritten in place rather than made again. Smoke, spray and
  sparks are particles; skid marks a ring of decals copied to the GPU as
  they are made; rain falls through a box that follows the camera,
  entirely on the GPU. The game starts full screen, and on a GPU renders
  at the window's full resolution (Full HD on a 1080p screen) whatever
  that costs. A software renderer gets fewer and smaller of everything,
  tone mapped in each shader, at a resolution that adapts to the frame
  rate.
* **The simulation** steps at 120 Hz whatever the frame rate (frames are
  drawn between the last two steps, and the cameras film the cars as
  drawn: the chase camera's springs are solved rather than stepped, so
  frames of uneven length never move it about behind the car, nor does a
  stall the game does not catch up on): four tyres with a friction circle,
  weight transfer, a gearbox, stability control and anti-lock brakes;
  collisions between cars' boxes and with the walls, a car's wheels and
  box those of its body as drawn. The computer drives the traffic
  (keeping its distance, yielding to sirens), the suspects (choosing the
  line with the most room, swerving at the police) and the police (taking
  its line only while it is clear of all but the suspect, never faster
  than it can stop behind a car on it, closing with a PIT). Each car has a
  number of its own, by which what refers to it (a police car's target, a
  chase's suspect, a crash) finds it, or finds it gone. A session is a
  mission, a quick chase, a patrol or the menus' showcase, which the
  computer drives and a cinematic camera films; its chase and its own
  timers (a patrol's calls, the showcase's next suspect) follow the game
  step by step, so that it goes the same however the frames fall. Its
  floating point is libm's on every target, never the host's C
  library's, so that the host's tests play the very chases Veda plays.
* **The sounds** are synthesised as the game plays (`game::audio`): each
  engine as its cylinders' firing pulses through its banks' exhaust pipes
  and silencer (a cross-plane V8's burble, a flat six's rasp), sirens
  with the Doppler shift of a car going by, tyres, wind, crashes and the
  cues of the menus; placed by distance, direction and motion, echoed in
  the tunnel, and limited, in a thread of their own. The music is loops of
  stems that `generation/music` renders at build time; the chase's second
  stem joins as the suspect is caught up with.
* **How it runs** shows in a corner: the frame rate and what renders (the
  GPU as Mesa names it, the host's under QEMU, or the processor); F3 adds
  the renderer's statistics and the last frames' lengths as a graph, or
  hides it all. Every 5 s the log has the frame rate and the longest frame
  with what its time went to (moving the game on, rendering, waiting for
  the window, reading the picture back, drawing over it).

## Audio

* Sound devices are Linux's, in [the driver VM](DRIVERVM.md): HD Audio
  controllers (most PCs' sound, QEMU's `intel-hda`) with their codecs,
  virtio's sound devices, and a laptop's speaker amplifiers on its SPI
  controller (Cirrus Logic's CS35L41, which the codec's driver plays
  through; the guest gets the GPIO pins they are wired to, and the
  firmware's variables their calibration is in), and the digital
  microphones of an Intel audio DSP (Linux's Sound Open Firmware on it,
  from Tiger Lake to Raptor Lake: the guest gets the firmware's NHLT,
  which describes them, with the controller). Linux's
  drivers drive them, and `alsa`, Veda's driver for Linux, attaches the
  card to the audio service's private `audiodev` protocol, as a native
  driver would; the service also runs without sound hardware (a null
  output then consumes audio in real time). A device attached hears
  everything the computer plays or becomes every program's microphone, so
  only the drivers devmgr starts may attach one (and say a card is
  coming), by the registry's identity; the test microphone (`testmic`,
  run by the kernel command line in automated tests) may attach an input
  device, which a sound card's microphone then replaces. Only the voice
  agent may duck the other sound.
* **Recording.** An input device produces into a ring with a capture
  clock; the service converts it for each capture stream (a ring's worth
  at most each pass of its loop), and runs the echo canceller for streams
  that ask for it (the agent's microphone) with the mixed output as
  reference, aligned by the two clocks; such a stream may carry the
  reference and the uncancelled microphone beside the cleaned one. The
  device records only while a capture stream is open.
* The `audio` service mixes every client stream: exact rational resampling
  (polyphase windowed sinc in integer arithmetic), ramped gains, click-free
  pause and seek, master volume and mute.
* PCM never travels in messages. Each stream, and the device link, is a
  single-producer/single-consumer ring of 16-bit frames in a shared VMO,
  with an event for back-pressure and a time-stamped play position, so
  players get accurate, smoothly extrapolated progress: a stream's
  position is stamped with when the device got there (the device's own
  time, the null output's clock), and position and time are published
  under a sequence count, so that no reader takes the one of one moment
  with the other of another (a seqlock, as the capture clock is). Each
  side keeps its own counters and clamps the peer's: a misbehaving client
  can only garble its own audio. Mixing and recording allocate nothing
  once they run.
* `vaudio` holds the formats (WAV, QOA; surround files of either are
  downmixed alike), the resampler, mixing, an FFT for visualisers, and a
  synthesiser and sequencer that `generation/music` uses to render the
  bundled album and the startup sound at build time.
* `vaudio` also holds the voice processing for an always-listening
  assistant. `aec::EchoCanceller` removes the loudspeakers' sound from the
  microphone, given the mixed speaker output as reference: a
  partitioned-block frequency-domain adaptive filter (tails up to 1 s)
  whose step follows the estimated residual echo, so it holds still during
  double talk and re-converges after the echo path changes, a bulk delay
  estimator (0 to 500 ms) and a residual echo suppressor; it adds 16 ms of
  latency at 16 kHz. `vad::Vad` flags speech every 10 ms (minimum-statistics
  noise floors, a likelihood ratio test, click rejection, hysteresis per
  utterance), and `level` meters RMS and peak levels in dBFS (the agent's
  animation of its voice and the microphone uses it).

## Video

Videos (`implementation/applications/videos`) plays videos with FFmpeg and
dav1d, decoded in `mediadec`, a process of their own; it speaks
`vproto::media` with it.

```
Videos (Rust, vui)                            mediadec (C program, sandboxed)
  window: library, player, picture choice,      Rust half: protocol, loop,
          copy to the window, controls            limits, rotation
  engine thread: the decoder, crash restart     C shim (the only code that
  audio thread: sound -> audio service,           calls FFmpeg): demuxing
          the clock                               through the file channel,
  thumbnail thread: a second decoder              decoders, swscale, swresample
        |  the control channel, a read-only file channel,
        |  and memory the app made: a heartbeat, a frame pool, a sound ring
        +------------------------------------------>
```

* **The sandbox.** The app starts `mediadec` with one handle, its control
  channel, and no registry: it reaches no file, service or network but
  what it is sent, a read-only channel to one file. FFmpeg reads through
  callbacks on that channel. Before it reads anything, the decoder gives
  up reading the kernel log, creating or seeing processes, and new
  executable memory, for good (`process_restrict`); like every
  position-independent program, it is loaded at a random address.
* **A decoder that fails** is killed (with what it started) and started
  again at the picture on the screen, which stays up meanwhile: one that
  crashes, does not answer a call in time (a call that fails in any way
  ends it), stops counting its *heartbeat* for 5 s (a page the app gives
  it when it starts it, in which it counts every round of its loop, and
  at least one a second while it waits: it is watched whatever it does,
  opening a file or playing sound only), or takes more memory than it
  may (half the machine's, from 256 MiB to 1.5 GiB: more is something
  wrong with the file). Twice at the same place, playback goes on 10 s
  further; three times in a minute, the video is given up. The restarted
  decoder is asked again what the old one was: how much to leave out, or
  that the window is hidden. Decoders end with Videos: it kills those
  still running when it ends, and a decoder's watchdog thread ends it
  when the app's end of the control channel closes (the app crashed or
  was ended), even while FFmpeg is stuck.
* **One loop.** The decoder's one thread answers the app between single
  calls into FFmpeg and decodes ahead while there is room: pictures and
  sound wait in queues of their own, and sound comes first when the app
  is about to run out of it. Frame threads of FFmpeg and dav1d decode.
  Its Rust half is built for `x86_64-unknown-none` and made one object
  that exports a single symbol, so that its runtime and musl's keep to
  themselves (see [C on Veda](C.md)).
* **Pictures** are converted by swscale (the stream's matrix and range,
  HDR tone-mapped to BT.709) straight into a slot of the *frame pool*, at
  the size the window shows them, turned upright. The pool is memory the
  app made: a page of state (each slot FREE, WRITING, READY or HELD,
  changed by compare-and-swap; times, generations, the end, and the
  window's clock, which the decoder reads as one with a sequence number
  so that it skips converting pictures already late) and the pixels. The
  app copies the picture it shows to its window, one row at a time; a
  resize or full screen gives the decoder a new pool, and the old one's
  pictures are shown until they run out.
* **Sound** is converted by swresample to 16-bit stereo at the output's
  rate, into a ring of records that say when each is heard. The app's
  audio thread moves it into an output stream and notes where each piece
  came from: what the output has played then says which moment of the
  video is heard. That is the clock pictures follow, smoothed, and held
  until the sound is heard after starting (the window is woken then), and
  until it stops after pausing. A video without sound (or whose sound
  ended) follows the system clock, as does one whose output went with the
  audio service: the stream is opened again when the service is back
  (Music's too), and meanwhile the sound is taken from the ring as fast
  as it would have played, and dropped.
* **Timing.** For each frame the app knows when it will be seen (from
  `FrameDone`) and so what the clock will say then; it shows the newest
  picture due by then, gives back those passed over, and wakes when the
  next is due. It presents only when the picture or the controls change:
  a 24 fps video is presented 24 times a second. A decoder that falls
  behind is asked to leave out the loop filter, then pictures nothing
  refers to, then all but key frames. The app logs how playback went
  every 10 s (`videos: playback: ...`: pictures shown and dropped, the
  offset from the sound, presents a second, drawing time).
* **Seeking** gives the pool and the ring a new *generation*: what the
  decoder made for an older one is passed over, and what it made for a
  newer one waits for the app to hear of it. The engine numbers the
  generations (the window's seeks, opening and restarts alike) and says
  which is decoded now and which of the window's seeks it carried out, or
  could not: the player shows nothing new while its seek is under way,
  goes on from where it was when the decoder cannot go there, and does
  not wait for a picture that will not come. Playing starts once a little
  sound of the new position is queued. Key frame seeks follow a dragged
  seek bar; an exact seek decodes from the key frame before the target
  and shows the picture at it, with the sound from there. A source that
  cannot seek shows how far it is without a seek bar.
* **Playback** (opening, seeking, starting with the sound, pausing, the
  end, restarts) is a state machine in `videos-core` that draws nothing
  and calls no one, tested on the host; the player feeds it what the
  engine, the audio thread and the pool say, and carries out what it
  asks.
* **Tests.** `verification/fakedec` stands in for `mediadec`: it draws its
  pictures and a tone, and crashes, hangs or falls behind when its clip
  asks. `systest` runs FFmpeg's and dav1d's `checkasm` on Veda and checks
  every picture of a sample video against its encoder's MD5s.

## The voice agent (`implementation/services/agent`)

The agent service holds the conversation with Deepgram's Voice Agent API
(a WebSocket over TLS through `vweb` and `vtls`, whose writes wait two
seconds at most, as one loop serves everything), the microphone and the
voice (two audio streams), and listens for its name while asleep (a local
voice detector, then Deepgram's streaming recognition, within an hourly
budget). A worker thread runs the functions the language model calls:
system functions (windows, files, volume, Wi-Fi, timers, memory, ...,
a module each) and applications' actions, which every `vui` application
offers over the `agentapp` protocol (describe, state, invoke) through
`vui::App`; a thread of its own asks registered applications what they
offer, off the path of calls. Actions that need the user's consent wait
for an approval from the shell (the agent's window or a notification);
only the shell may approve and only Settings may change the agent's
settings, by identity (Settings is served on a thread of its own). Their
outcomes, and due reminders, reach the language model as notices: a
sentence of the computer's, then what came from elsewhere as data. The
agent's settings, Deepgram key, memory and permissions live in
`/home/.private/agent`, which the file system service opens to the agent
alone; the worker reaches the user's files through a connection of its
own and checks every path it acts on, so that nothing a document or a
record in the Trash says can reach that directory. The decisions that
need no I/O (the protocol, tools and their risks, the paths file actions
may reach, the prompt, notices, memory, the approval policy, the wake
word) are in `vagent`, tested on the host. See [The agent](AGENT.md).

## Testing

* Host unit tests for the libraries with platform-independent logic (ABI,
  boot protocol, heap, IPC codec, service protocols, ELF loading, the POSIX
  layer's conversions, math, rasteriser, fonts, image codecs, 2D graphics,
  text editing, Paint's editing and the Calculator's arithmetic, paths and
  file types, audio, the hypervisor's guests and the IOMMU's tables, media
  generators, build tool), all in one Cargo run;
  the driver VM's programs' on Linux. A test of the build tool's fails if
  a crate with tests is left out.
* `systest`, a program that runs inside Veda and exercises kernel objects
  (sockets, memory protection, private memory, exit futexes, ending a job),
  threads, the file system, the launcher, crash reports and the restart of
  the window system, and runs C, C++ and Rust programs:
  `verification/c/posix.c` checks the C library and the POSIX layer,
  `verification/c/cxx.cc` the C++ library, `verification/rust/stdlib.rs`
  Rust's standard library on them, and `verification/stdcrates` crates
  that need it, as Veda's ports of their foundations have them.
  `nettest` checks DNS, UDP, TCP, HTTP, HTTPS and ping over Ethernet or
  Wi-Fi; the network cards' scripts have it check ping, TCP and HTTP
  against a server on the host instead (`nettest local`), so that they
  need no Internet.
* Network host tests: the 802.11 protocol and its cryptography against
  published vectors, a station against an access point, two TCP/IP stacks
  over a simulated cable, and the TLS client against a rustls server and
  real certificate chains.
* Hardware no emulator has is simulated where Veda's own code drives it:
  `vgpio::sim` is a GPIO controller as its registers behave (Intel's
  pads, its pins wired in pairs), which devmgr drives in place of a PC's
  when a test's ACPI table describes one (`drivervm-gpio.vts`, through to
  Linux's GPIO character device in the guest). `cargo orchestration acpi`
  shows what devmgr makes of a real machine's ACPI tables. The real machine
  stays the final check of what Linux drives there.
* GUI automation scripts (`verification/ui/*.vts`) that drive QEMU through
  QMP (mouse, keyboard, waits on log lines, screenshots) and fail on
  panics. They are checked whole before the system is built (their
  language: `orchestration/src/script.rs`); a run builds the system once,
  and each script's machine boots an overlay of its one disk image, within
  a deadline. QEMU's machine has an IOMMU, as PCs do, so the driver VM
  runs in every script, with the pointer, USB, the networks and the
  display.
  The sound cards' scripts also check QEMU's recording of the output for
  dropouts.
  `drivervm-display.vts` gives the window system a display that flips
  (QEMU's VGA, which Linux drives in the driver VM) and compares
  screenshots of the desktop before and after a window came and went, in
  both of its pictures; `drivervm-display-crash.vts` has Linux crash
  under it, and `drivervm-display-restart.vts` the window system restart
  while it is attached. `drivervm-compose.vts` has the GPU compose: the
  renderer on softpipe draws every frame into its pictures, slowly, under
  emulation; `drivervm-gpu.vts` renders Interceptor on the host's GPU
  through QEMU's virtio-gpu, given to the driver VM.
* The agent's scripts (`verification/agent/*.vts`) with a stand-in for
  Deepgram on the host and a test microphone fed from the host;
  `verification/real/` talks to the real Deepgram, and checks the network
  on the real Internet.

`cargo orchestration test --ui` runs the first two; `verification/real/`'s
scripts, which need the Internet and a Deepgram key, run by hand.

## Architectures

Veda runs on AMD64 (x86-64). What depends on the architecture, or on the
platform that comes with it, is in each component's `arch/<architecture>/`
directory (a crate's in its `src/`), which the rest of the component
reaches through its `arch` module ([CODING.md](CODING.md) has the rules).
On AMD64:

| Component | What is AMD64's (`arch/amd64/`) |
|-----------|---------------------------------|
| `implementation/boot/` | the kernel's page tables, the framebuffer's memory type, COM1, the TSC, the jump into the kernel |
| `implementation/kernel/` | see `arch` above |
| `implementation/libraries/bootinfo` | the address space the loader hands over, and how the kernel is entered |
| `implementation/libraries/abi` | the calling convention of system calls, the user address space, a virtual processor's registers |
| `implementation/libraries/rt` | the entry points, the system call instruction, the C runtime's helpers; the driver VM's programs' Linux system calls |
| `implementation/libraries/posix` | Linux's system call numbers and the calls only x86-64 has, the thread pointer |
| `implementation/libraries/elf` | the executables' machine type and relative relocation (on every host, for the tools) |
| `implementation/libraries/build` | the user programs' target |
| `implementation/libraries/hv`, `implementation/libraries/iommu` | the guests' local APIC, CPUID, memory map, platform discovery and Linux's boot protocol; Intel VT-d's tables (on every host, for the tests) |
| `implementation/services/devmgr`, `implementation/services/drivervm`, `implementation/services/init` | the PC's own devices (keyboard controller, DMAR), the guest's memory map and boot, the I/O ports and IRQs `init` hands out |
| `verification/systest`, `verification/c`, `verification/ui` | what the tests expect of the architecture, and the GUI scripts that hold only on QEMU's PC |
| `orchestration/` | the Rust and C targets, the loader's boot path, QEMU's PC (q35, OVMF), its ACPI test tables |
| `.cargo/`, `implementation/ports/` | Cargo's settings of the targets; the ports' settings, patches (binutils, GCC, musl, Linux) and the driver VM kernel's options |

## Repository layout

Crates with code particular to an architecture keep it in
`src/arch/<architecture>/`, other directories in an `arch/<architecture>/`
of their own (see Architectures above).

| Path | Contents |
|------|----------|
| `implementation/boot/` | UEFI loader |
| `implementation/kernel/` | microkernel |
| `implementation/libraries/abi`, `implementation/libraries/bootinfo`, `implementation/libraries/initrd`, `implementation/libraries/elf` | shared formats and the kernel ABI |
| `implementation/libraries/rt`, `implementation/libraries/heap`, `implementation/libraries/build` | user runtime, allocator, build helper |
| `implementation/libraries/posix` | the POSIX layer under the C library |
| `implementation/libraries/raster`, `implementation/libraries/font`, `implementation/libraries/image` | vector rasterisation, fonts, images |
| `implementation/libraries/ipc`, `implementation/libraries/proto` | message encoding and the service protocols |
| `implementation/libraries/gfx`, `implementation/libraries/ui`, `implementation/libraries/text` | 2D drawing, the GUI toolkit, the text editing model |
| `implementation/libraries/files` | files for applications: paths, file types and the apps that open them, formatting, VFS access, thumbnails |
| `implementation/libraries/glsl`, `implementation/libraries/gl` | the GLSL ES compiler (with its TGSI back end), and OpenGL ES 3.0 with its software and GPU (virgl) renderers |
| `implementation/libraries/audio` | audio formats, resampling, mixing, FFT, the synthesiser, echo cancellation, voice activity detection and level metering |
| `implementation/libraries/virtio` | virtio device access shared by the drivers |
| `implementation/libraries/acpi`, `implementation/libraries/gpio` | the ACPI tables, the AML interpreter and resource templates, and Intel's GPIO pads (for `devmgr`) |
| `implementation/libraries/hv`, `implementation/libraries/iommu` | what the hypervisor and the IOMMU driver know that touches no hardware: the guests' platform, its ACPI tables and the bridge's ABI; the architecture's virtual devices and boot protocol (on AMD64 the local APIC, `cpuid`, Linux's boot protocol) and its IOMMU's tables and structures (VT-d's) |
| `implementation/guest/` | the driver VM's Linux programs: its `init`, Veda's drivers for Linux (`input`, `alsa`, `net`, `wifi`, `kms`), the renderer (OpenGL ES on Mesa's drivers), `logkeeper` (the system's log on the live system's stick), and its tests (`bridgetest`, `pcitest`, `gpiotest`, `efivartest`, `kmspause`) |
| `implementation/libraries/splash` | the startup splash's picture, which the window system's processor and GPU paths draw alike |
| `implementation/libraries/entropy` | the ChaCha20 random number generator and BLAKE2s entropy pool |
| `implementation/libraries/frames` | the searches of the kernel's bitmap of physical memory frames, a word at a time |
| `implementation/libraries/netstack`, `implementation/libraries/net` | the TCP/IP stack around smoltcp, and the networking API for applications |
| `implementation/libraries/tls` | the TLS client for applications: rustls and its pure-Rust cryptography provider |
| `implementation/libraries/web` | URLs, and HTTP/1.1 and WebSocket clients over TCP or TLS |
| `implementation/libraries/agent` | the voice agent's logic: the Deepgram protocol, tools, prompt, memory, approval policy, wake word |
| `implementation/libraries/wlan` | IEEE 802.11 (frames, RSN, handshakes, SAE, station and access point) |
| `implementation/ports/` | third-party software built from source with Veda's patches: GCC, binutils, GMP, MPFR, MPC and musl; Rust, its compiler and standard library for Veda's target; for the driver VM, Linux (with zlib and elfutils for its build), a toolchain of its own (the same GCC, binutils and musl, unpatched) and Mesa; the crates smoltcp and zune-jpeg, which Cargo builds in place of their releases; and the fonts, unmodified |
| `implementation/services/`, `implementation/drivers/`, `implementation/applications/` | system services, drivers and applications (the applications with their manifests, `<name>.app`) |
| `verification/` | in-system tests, C and C++ test programs (`verification/c`), Rust's on its standard library (`verification/rust`, `verification/stdcrates`) and GUI automation scripts |
| `generation/` | host programs: media generators |
| `orchestration/` | build orchestration, disk image creation, QEMU automation |
| `documentation/` | documentation |

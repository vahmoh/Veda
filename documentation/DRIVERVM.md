<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# The driver VM

Veda drives only the disks it starts from. Every other device goes to
Linux, which runs in a virtual machine on Veda's own hypervisor (the
*driver VM*) and whose drivers serve Veda's services through the
protocols Veda's own drivers speak. A driver that crashes, hangs or is
compromised takes down a virtual machine that Veda restarts, never Veda.

**Contents:** [Why](#why) · [Decisions](#decisions) · [The pieces](#the-pieces) · [The platform](#the-platform) · [The hypervisor](#the-hypervisor) · [Devices](#devices) · [Veda's drivers for Linux](#vedas-drivers-for-linux) · [The bridge](#the-bridge) · [What others learned](#what-others-learned) · [Status](#status) · [Building](#building) · [Testing](#testing)

## Why

Every generation of GPUs, Wi-Fi chips and audio DSPs takes years of
driver work with the vendors' documentation and firmware, and Linux has
done it for nearly everything. Porting its drivers one by one means
emulating a growing part of Linux's kernel for each driver and following
it as it changes. Running
Linux itself, unmodified but for a few platform files, keeps every driver
as its authors tested it and makes Linux's releases the driver updates.
In a virtual machine the drivers stay untrusted and isolated, as Veda's
are: the processor's virtualization and the IOMMU confine them to their
memory and their devices.

## Decisions

1. **The boundary is Veda's own driver protocols.** Linux's drivers serve
   `audiodev`, `displaydev`, `netdev`, `gpu` and the rest as Veda's own
   drivers do, over [the bridge](#the-bridge), so Veda's services need no
   change. Virtio devices would need a translation layer per class and
   lose what Veda's protocols carry (audio clocks, flip timestamps, Wi-Fi
   state); Linux's user-space APIs would tie Veda's services to Linux.
2. **The bridge carries Veda's system calls into the guest.** A guest
   program uses `rt` built with `--cfg veda_guest`: its calls on Veda's
   objects become operations on `/dev/veda`, which the guest's kernel
   turns into hypercalls that the monitor carries out on the guest's own
   handle table. So `ipc` and `proto` work unchanged, and the guest
   reaches only the objects it was given.
3. **The guest is fully paravirtualized.** There is no emulated hardware
   (no PIC, PIT, I/O APIC, RTC, serial port or firmware code): processor
   start-up, the console, power, the time of day, PCI configuration and
   interrupts are hypercalls, and ACPI tables describe the devices. The
   hypervisor decodes no instruction (MMIO decoding is the largest source
   of hypervisor bugs), and Linux boots in about 0.1 s. A small patch
   modelled on Linux's Jailhouse and ACRN guests adds the *Veda platform*.
4. **The kernel keeps to mechanism.** It gives a monitor process
   `Guest` objects (guest-physical memory made of VMOs, and its devices'
   IOMMU domain) and `Vcpu` objects, and handles only what must be fast or
   belongs to the processor (the local APIC and its timer, `cpuid`, MSRs,
   `hlt`, control registers). Policy is the monitor's: `drivervm`, an
   ordinary process.
5. **Devices are given whole, behind the IOMMU.** A given PCI function's
   DMA reaches only the guest's memory, and its interrupts are remapped;
   without an IOMMU no PCI function is given away. A USB controller goes
   whole, with its devices.
6. **Memory is shared, not copied.** A VMO a guest program maps also
   appears in a window of guest-physical memory beyond the guest's RAM, so
   rings, pictures and command buffers cost no copy. Each side treats
   shared memory as untrusted.
7. **Time is shared.** The guest reads the host's TSC unchanged and
   computes Veda's monotonic time itself, so timestamps mean the same on
   both sides.
8. **The driver VM is restartable.** When Linux crashes, `drivervm` ends,
   and `devmgr` resets the devices and starts a new one (unless Linux
   powered it off), later after each run that fails within a minute, and
   not at all after a few.
9. **Linux is a port.** A pinned release, configured with only what the
   platform and the devices need (`implementation/ports/linux/veda.config`
   and the architecture's), and a platform patch. The guest's programs are
   static Rust on musl.

## The pieces

| Piece | Where | What it does |
|-------|-------|--------------|
| Hypervisor | `implementation/kernel/src/hv`, `implementation/kernel/src/architectures/amd64/hv` | `Guest` and `Vcpu` objects; on AMD64, VMX with EPT and the virtual x2APIC |
| IOMMU | `implementation/kernel/src/architectures/amd64/iommu` | Intel VT-d: DMA and interrupt remapping |
| `hv` | `implementation/libraries/hv` | The platform's and the bridge's ABIs, the guest's ACPI tables, a given function's configuration space; on AMD64 the virtual local APIC, `cpuid` and Linux's boot protocol |
| `iommu` | `implementation/libraries/iommu` | The DMAR table; the units' registers and structures |
| Device manager | `implementation/services/devmgr` | Picks the driver VM's devices, serves their `pcidev` channels, resets them, restarts the driver VM |
| Monitor | `implementation/services/drivervm` | The machine, its hypercalls and PCI functions, the bridge, the narrowed registry |
| `drivervm-core` | `implementation/services/drivervm/core` | The monitor's decisions that touch no object: the bridge's books, the guest's budget and window, where functions and BARs go, the keyboard controller's commands |
| Guest kernel | `implementation/ports/linux` | Linux with `veda.patch` (`hvc_veda.c`, `drivers/virt/veda/bridge.c` and `efi.c`, `gpio-veda.c`, an i915 change) and `architectures/amd64/veda.patch` (`arch/x86/kernel/cpu/veda.c`, `arch/x86/pci/veda.c`) |
| Guest runtime | `implementation/libraries/rt/src/guest.rs` | `rt`'s system calls through `/dev/veda`; watches |
| Guest programs | `implementation/guests/linux` | `init`; the drivers `input`, `alsa`, `net`, `wifi`, `kms` and `renderer`; `logkeeper`; the tests `bridgetest`, `pcitest`, `gpiotest`, `efivartest`, `timertest`, `kmspause`; shared code (`sys`, `netlink`) |
| Build | `automation/src/linux.rs`, `implementation/ports/linux/build.sh` | [`cargo automation linux`](#building) |

## The platform

A guest finds the platform with `cpuid`: leaf `0x40000000` returns
`VedaVedaVeda`, `0x40000001` the number of processors (local APIC ids 0
to n-1), `0x40000010` the TSC's and the APIC timer's frequencies. A
hypercall is `vmcall` with its number in `rax` and its arguments in
`rbx`, `rcx`, `rdx`, `rsi`, `rdi` (`hv::platform`):

| Hypercall | Does |
|-----------|------|
| `CONSOLE_WRITE` | Writes up to 32 bytes passed in registers, so it works however early |
| `START_CPU` | Starts a processor in 64-bit mode, on page tables that map the first 4 GiB |
| `POWER` | Off, restart, or crashed |
| `WALLCLOCK` | The time of day |
| `BRIDGE` | One operation of [the bridge](#the-bridge) |
| `PCI_CONFIG_READ`, `PCI_CONFIG_WRITE` | A given function's configuration space |
| `PCI_MSI` | Routes a function's MSI to a processor and vector; returns the message the function sends |
| `GSI` | Routes an interrupt line to a processor and vector, or masks it (vector 0) |
| `GPIO` | Reads or drives a GPIO pin the guest has, makes it an input, or reports its direction |
| `FIRMWARE_VARIABLE` | UEFI's `GetVariable` and `GetNextVariableName`, read-only |

GSIs are the keyboard controller's 1 and 12, the lines functions' INTx
are wired to, and from 256 the platform's own (GPIO pins' interrupts).
The ACPI tables (`hv::acpi`, at the boot parameters' `acpi_rsdp_addr`)
are an RSDP, an XSDT, a hardware-reduced FADT and a DSDT with the PCI
root, its functions and their INTx routing (`_PRT`), the keyboard
controller and the platform's GPIO controllers; there is no MADT.

On AMD64 the monitor boots Linux through the x86 boot protocol's 64-bit
entry (`implementation/services/drivervm/src/architectures/amd64/boot.rs`).
The memory map leaves out the first 64 KiB (the processors' start-up GDT
and page tables) and the legacy hole, where the ACPI tables are, from
0xE0000 as on a PC.

A processor's only timer is its local APIC's (in x2APIC mode, with the
TSC-deadline timer; there is no PIT or HPET), which keeps running while
the processor idles, as `cpuid`'s leaf 6 says (ARAT). Linux needs that to
run its timers in high-resolution, tickless mode: without it, it would
keep a 250 Hz tick on every processor and end every timer on one, a sleep
of 1 ms lasting 4 ms, and idle processors would wake 250 times a second.
Under QEMU a sleep of 1 ms ends 0.1 to 0.3 ms late (nested
virtualization's cost), and an idle processor takes a few timer
interrupts a second; `timertest` checks that timers end on time
(`architectures/amd64/drivervm.vts`).

## The hypervisor

On AMD64 (`implementation/kernel/src/architectures/amd64/hv`), `vcpu_run`
enters the guest on the calling thread with the BKL released; host
interrupts and the scheduler preempt it as they do user code, and `hlt`
blocks it until an interrupt or the guest's timer. While the guest runs,
the VMX-preemption timer brings it back at its timer's deadline; while it
halts, its thread sleeps until that deadline, however near (a processor's
timer is armed for the next deadline itself). The guest's FPU and the
MSRs it shares with the host are swapped, and its VMCS cleared, only when
it leaves the kernel's loop, so it may run next on any CPU, which first
drops stale translations (`invvpid`, `invept`). Unmapping guest memory
shoots the translations down on every CPU before the pages are reused.

## Devices

Every PCI function goes to the driver VM except those Veda keeps
(`veda_keeps` in `implementation/services/devmgr/src/main.rs`): storage
controllers, functions Veda has drivers of its own for, and the
platform's own (bridges, system peripherals, the SMBus, the firmware's
flash controller). A laptop's I2C and SPI controllers, with its touchpad
and speaker amplifiers, are Linux's. `drivervm.devices=VID:DID,...` adds
functions Veda would keep; `drivervm=off` gives nothing.

The driver VM runs where the processors run virtual machines (VMX with
EPT) and the image has its Linux, and gets PCI functions only where an
IOMMU confines them (the kernel tells `devmgr` both, `abi::platform`).

### How a function is given

`devmgr` starts no driver of Veda's for the function, and hands the
monitor its `pcidev` channel, which gives only what giving it away takes:
its configuration space, BARs, MSIs, the firmware's reserved memory, and
a resource that names it (`resource_kind::PCI`, by requester id), which
only the driver VM's functions have. A function is given whole or not at
all: its BARs and reserved memory are mapped, then its DMA joins the
guest's domain, and a failure undoes all of it. A function the monitor
could not give stays `devmgr`'s, neither reset when the driver VM ends
nor offered to the next.

**DMA.** Every device starts in the host's domain, which passes requests
through. `guest_attach_device` puts a function in its guest's domain,
whose page tables mirror the guest's memory: `guest_map` and
`guest_unmap` update the EPT and the domain together, and an unmap
invalidates the IOTLB before the pages are reused. Functions are attached
before the guest runs; when it ends, their context entries go, so they
reach nothing until another guest takes them.

**Interrupts.** The IOMMU remaps every interrupt in Veda through one
table (entry V raises vector V of the boot processor) and blocks the
compatibility format; an MSI's entry names its function, so a function
raises only its own interrupts, whatever the guest writes. A guest's MSI
is an `Interrupt` object bound to a virtual processor
(`vcpu_bind_interrupt`) and raised at its local APIC without the monitor,
on a vector from the pool all devices share (freed when the guest routes
it to vector 0). Interrupt lines are bound the same way; a
level-triggered one stays masked from when it fires until the guest's
end-of-interrupt.

**PCI in the guest.** The guest sees a function's own configuration space
(`hv::pci`) but for its memory BARs, which hold their guest-physical
addresses (3 GiB to 4 GiB, or above the bridge's window); there is no
expansion ROM or I/O BAR. A function on the host's bus 0 keeps its device
number where it can. `arch/x86/pci/veda.c` gives Linux configuration
space through hypercalls and an MSI domain, modelled on Hyper-V's root
partition, in which composing a message routes the MSI (`PCI_MSI`). A
function's INTx is the line the PC's firmware wires it to (from the
root's `_PRT`). Linux's command line gives each function's host addresses
(`veda.device=00:01.0,0:0x80000000,...`), which drivers name to services
such as the compositor.

**Power, errors and PCI Express.** A function woken to D0 resets, BARs
included, unless it reports No Soft Reset, so `devmgr`, which carries out
the guest's power state writes, puts its BARs back. SERR# and PCI
Express error reporting stay off whatever the guest writes, as a PC may
turn a function's errors into NMIs that stop Veda. As in VFIO, the guest
writes only device control and status and device control 2: phantom
functions stay off (their DMA would carry other requester ids), the
maximum payload size stays the host's, and the maximum read request size
never goes below it. A function-level reset would lose BARs the guest
cannot put back, so it is neither advertised nor passed on; Linux resets
the function through D3hot instead.

| Limit | |
|-------|-|
| Interrupts | MSI or MSI-X, or an INTx the firmware routes on a root bus (QEMU's `e1000` has only that); behind a bridge, MSIs |
| BARs | Memory BARs of whole pages; I/O BARs are not given |
| Configuration space | The first 256 bytes (`devmgr` reaches no more yet) |
| MSIs | 128 for all the guest's functions at once |
| Guest memory | At most 3 GiB |

### What the firmware ties to devices

**Reserved memory.** Memory the firmware keeps using with a device (an
RMRR: a GPU's stolen memory, a USB controller's for legacy emulation) is
mapped into the guest at the PC's addresses, marked reserved, before the
function joins the guest's domain; the kernel attaches no device whose
domain lacks it. A function whose reserved memory is where the guest has
RAM or BARs is not given.

**Described devices.** A laptop's touchpad and touchscreen are HID
devices on I2C controllers that only the firmware's ACPI tables describe.
`devmgr` reads the present devices below a function's ACPI companion, and
the monitor writes them below the function in the guest's tables: ids,
`I2cSerialBusV2` or `SpiSerialBusV2` connections retargeted to the
function, interrupts (GSIs), the `_DSM` answer HID over I2C needs,
`_DSD`, and the controller's timing (`SSCN`, `FMCN`, `FPCN`, `HSCN`).
What the guest does not get is left out, and logged.

**GPIO pins.** The guest gets only the pins described devices are wired
to (`GpioIo`, `GpioInt`), on platform GPIO controllers (`VEDA0001`) with
the PC's pin numbers, so the connections stay the firmware's but for the
controller. `devmgr` drives the pads and raises the pins' interrupts
(GSIs from 256; a level-triggered one stays masked until the guest's
end-of-interrupt). Linux's driver is `drivers/gpio/gpio-veda.c`.

**Firmware variables.** Veda runs no firmware code: its loader copies the
UEFI variables that have runtime access before it leaves boot services,
and they reach the guest read-only through `FIRMWARE_VARIABLE` and
`drivers/virt/veda/efi.c` (`efi.get_variable`, and efivarfs for
programs). A laptop's amplifier driver reads its factory calibration
there. The monitor logs each variable the guest reads.

**Audio DSP links.** Linux records a laptop's digital microphones on an
Intel DSP through Sound Open Firmware, which it picks when the
controller's NHLT table lists them; the NHLT goes to the guest with the
controller (`pcidev`'s `acpi_tables`).

**Intel's integrated GPUs.** The stolen memory is an RMRR at the PC's
address, so i915 reads GGC and BDSM as on the PC and finds what the
firmware left there. The OpRegion is copied to 0xC0000 (`hv::igd`),
without the SWSCI mailbox to the firmware. The GPU stays at 00:02.0, and
i915 finds a read-only stand-in for the host bridge at 00:00.0. Once i915
has the display, the compositor stops drawing into the firmware's
framebuffer, which is in the GPU's aperture. Not handled yet: a
compositor started again while i915 has the display draws its first
frames there until `kms` attaches.

### The keyboard controller

The PC's i8042 goes to the driver VM too, without an IOMMU (it does no
DMA): `devmgr` hands over its two ports and two interrupts, the guest's
tables describe it as a PC's do (`PNP0303`, `PNP0F13`, GSIs 1 and 12),
and Linux's i8042, atkbd and psmouse drive it. Its output port drives the
processor's reset line and the A20 gate, so the monitor, which carries
out every access to the two ports, passes only the commands Linux's
driver gives (none touches the output port) and drops the rest: a data
byte is a command's parameter or the keyboard's, never the output port's
(`drivervm_core::arch::i8042`). The guest does not reach the ports around
them, such as 0x61 with the NMI switches.

## Veda's drivers for Linux

A driver for Linux is a guest program written as a Veda driver is,
against Linux's device interfaces. The guest's `init` starts those its
devices need (by PCI class; `input` always), and again when one ends: a
second after, twice as long after each end soon after a start, up to a
minute. A driver polls Linux's files and Veda's handles together through
*watches* (`rt::guest::Watch`: files readable while a handle's signals
are). When a watch fails (the monitor's limits, or the handle gone) or
the service goes away, the driver ends its session and attaches again,
later after each failure in a row (`guest_sys::retry`).

**Input** (`implementation/guests/linux/input`). Every evdev device is
grabbed, so Linux acts on no key, and what a device reports up to its
`SYN_REPORT` goes to the compositor's `input` service in one batch:
evdev's key codes (which are Veda's), relative motion, and absolute
positions as fractions of their range. Touchpad fingers become pointer
motion, two-finger scrolling and tap clicks.

**Sound** (`implementation/guests/linux/alsa`). Linux's first playback
device that is not a display's is the audio service's output (`audiodev`),
through ALSA's kernel interface without alsa-lib. The driver plays the
service's mixed output a period (10 ms) at a time, a few periods ahead,
paced by the card's interrupts; an underrun makes the queue a period
deeper. Positions come from the card's own count, stamped with Veda's
clock, and the card's mixer stays at 0 dB, as the volume is Veda's. Its
capture device is the input (on an Intel DSP, SOF's `DMIC` device, made
stereo). A card that cannot play 48 kHz 16-bit stereo waits for Linux's
cards to change. Linux drives HD Audio (`snd-hda-intel`, with Realtek's
and the generic codec drivers), virtio sound, and a laptop's CS35L41
amplifiers on SPI as the codec's side codecs (`serial-multi-instantiate`,
`cs35l41_hda`).

**Network** (`implementation/guests/linux/net`). Each Ethernet interface,
of a PCI or USB device, is a `netdev` device of Veda's network service:
frames go between a raw packet socket and the device's ring, a copy each
way. Linux has no IP stack here; it moves frames and answers nothing.

**Wi-Fi** (`implementation/guests/linux/wifi`). Each radio is a *managed*
radio of Veda's Wi-Fi service ([Networking](NETWORKING.md)): Linux's
802.11 stack is its MLME through nl80211, while the service decides what
to join, does SAE and the key handshakes, and gives Linux only the session
keys. There is no supplicant in the guest. The driver's netlink socket
owns the connection, so if the driver ends, Linux leaves the network.

**Display** (`implementation/guests/linux/kms`). The display whose
memory holds the firmware's framebuffer (named by a BAR's host address,
`veda.device=`) is the screen the compositor flips (`displaydev`). At
the first frame the driver sets, with atomic modesetting, the
compositor's screen size if the display has it, else the display's own
mode, scaled and centred. That commit keeps the display as the firmware
set it up, so a laptop's panel stays on: Veda's i915 change keeps the
firmware's bits a colour (8 at least), and `kms` leaves `max bpc` alone,
as either change would take a full mode set. The compositor's pictures
are Veda's memory, imported as dma-bufs and scanned out through the
IOMMU without snooping the processor's caches, so they are
write-combining memory (QEMU's standard VGA copies them, as does a
driver that cannot import). Flip timestamps, on Linux's monotonic clock,
are Veda's too: both follow the TSC.

**GPU** (`implementation/guests/linux/renderer`). The renderer serves
`gpu`, in which `gl` sends virgl's commands, on Mesa's Gallium driver
over the GPU's Linux driver: iris on i915, virgl on QEMU's virtio-gpu
(which renders on the host's GPU), or softpipe for tests. Each connection
gets a context and shared memory, bounded per process and in total. The
decoder (`decoder/`, C) checks every command before Gallium carries it
out, and adapts shaders to each driver's conventions
(`decoder/conventions.c`). The renderer polls a watch per connection (65
of the monitor's waits at most) with the GPU driver's sync_file of each
context's oldest pending fence, so it wakes when a client asks or a fence
signals. The compositor's pictures reach the GPU as dma-bufs, so it
composes the screen in place (with virgl, the compositor composes on the
processor). A renderer that ends is started again, and the compositor sets
the GPU up again, three times at most.

**USB.** Linux drives the controllers, their hubs and every device on
them: keyboards, mice and tablets for `input`, network adapters for `net`
(CDC Ethernet and NCM, Realtek's and ASIX's), and Bluetooth adapters,
which no service of Veda's uses yet. USB disks it reads only to find the
live system's stick; disks are Veda's, which has no USB disk driver yet.

**The system's log** (`implementation/guests/linux/logkeeper`). Started
first where there is a USB controller, `logkeeper` keeps each start's logs
on the live system's stick (a FAT32 file system named `VEDA` with a
`VEDA/LOGS` directory): Veda's log, read through the bridge (`LOG_READ`),
and Linux's whole log (`/dev/kmsg`, DRM debugging messages included), in
`VEDA/LOGS/0007/VEDA.TXT` and `LINUX.TXT`, synced every second, for the
eight latest starts.

## The bridge

| Guest program | Guest kernel (`/dev/veda`) | Monitor |
|---------------|----------------------------|---------|
| `Channel::write(bytes, handles)` | `VEDA_IOC_CHANNEL_WRITE`: copies the buffers in, checks the file holds the handles | Moves the handles out of the guest's table, writes the message |
| `wait_many(items, deadline)` | `VEDA_IOC_WAIT`: finished at once, or sleeps until the ring says | Polls, or keeps the wait for its waiter thread, which writes the result to the ring and raises the callback vector |
| `Mapping::new(vmo)` | `VEDA_IOC_VMO_MAP`, then `mmap` | Maps the VMO into the guest-physical window |
| `rt::guest::Watch::new(handle, signals)` | `VEDA_IOC_WATCH`: a file whose `poll` makes a wait and reads its result | An ordinary wait, cancelled when the file closes |
| `rt::guest::dmabuf(vmo, offset, len, writable)` | `VEDA_IOC_VMO_DMABUF`: a dma-buf of the window range, for devices or the processor | Maps the VMO into the window until the dma-buf goes |
| `rt::object::log_read(offset, buf)` | `VEDA_IOC_LOG_READ` | Reads Veda's log |

The guest's kernel lets a file use only the handles it made or received,
and closes them when the file closes. The guest's registry
(`implementation/services/drivervm/src/registry.rs`) lets it connect only
to `audiodev`, `displaydev`, `netdev`, `wlanphy` and `input`, and
register only `gpu`, through 32 channels at most (one per program).

Whatever Linux does, what it makes Veda keep is bounded; an operation
beyond a bound is refused (`LimitReached`):

| What | Bound |
|------|-------|
| Handles | 16384, those in messages it sent itself included |
| Waits that go on | Their items fit one kernel wait (`abi::WAIT_MANY_MAX`, less the waiter's event); with the finished waits not yet in the ring, at most what the ring holds |
| Veda's memory | A quarter of the PC's, 512 MiB at least: the VMOs it made (by size, while it or a message on its way to it holds them; pages are committed as used), the messages it sent itself until it reads them, and its window mappings (`drivervm_core::ledger`) |
| Window mappings | 4096 (`drivervm_core::window`) |

Closing a handle a wait names ends the wait, so a closed channel's peer
sees it closed.

> [!IMPORTANT]
> The narrowed registry passes what it allows through the monitor's own
> registry channel, a copy of `devmgr`'s, so services see `devmgr` as the
> guest's client. A service that trusts `devmgr` with some calls (the
> compositor's and the audio service's `expect`, which say what drivers
> are coming) would take them from the guest too: the narrowing sees only
> the registry's calls. The fix is for `init` to attribute a registry
> channel to the monitor itself, which `devmgr` would hand it instead.

## What others learned

Drivers have run in their own operating system in a virtual machine
since L4Ka's DD/OS, within a few percent of native network throughput
([LeVasseur et al., OSDI 2004](https://www.usenix.org/conference/osdi-04/unmodified-device-driver-reuse-and-improved-system-dependability-virtual-machines)).
Xen's driver domains, Qubes, Ghaf, Spectrum and seL4's LionsOS do it
today; Genode (DDE Linux) and HongMeng port the drivers instead, HongMeng
rejecting virtual machines for managing memory and scheduling twice.
Veda is the host and the hypervisor at once, and its drivers' protocols
are the boundary.

| Project | Lesson | What Veda does |
|---------|--------|----------------|
| DD/OS; ChromeOS's DRM native contexts | Cut at the coarsest interface: a generic layer inside Linux gives the lowest common denominator; forwarding a GPU's API reaches 12 to 86% of native, its kernel interface 99% on Qualcomm's GPUs | Its own asynchronous, shared-memory protocols; Mesa's own driver runs in the guest, so a GPU Linux and Mesa drive needs no work from Veda |
| Xen ([XSA-155](https://xenbits.xen.org/xsa/advisory-155.html), [XSA-396](https://xenbits.xen.org/xsa/advisory-396.html)) | What the guest writes is hostile: values read twice from shared rings, races in revoking grants | The monitor copies a request out before checking it, keeps the ring's indices itself, and bounds what the guest makes it keep |
| Qubes; Linux's virtio | No ambient access to the host's memory: grants instead | The guest reaches only the VMOs it was given, in its window |
| Xen | Map bulk data, keep the mappings, copy small things: TLB shootdowns dominate | Rings and buffers stay mapped while used; messages are copied |
| Xen, Hyper-V's VMBus, seL4 | Signal only when the peer waits | One interrupt for all the finished waits; how often events are signalled is not yet measured |
| Xen ([CVE-2011-1898](https://nvd.nist.gov/vuln/detail/CVE-2011-1898), [XSA-59](https://xenbits.xen.org/xsa/advisory-59.txt)) | Without interrupt remapping a driver domain forges MSIs; with it, system errors still raise the host's NMIs | Both remappings for every given device; every interrupt checked against its source; given functions report no errors |
| Ghaf | The firmware ties devices to it: RMRRs, Intel's graphics (OpRegion, stolen memory, 00:02.0), the NHLT | RMRRs where the PC has them; the firmware's tables go with their devices |
| Qubes; Xoar | Reset is the weak point (a function-level reset takes 100 ms, many devices lack one, AMD's GPUs need their own); restarting Xen's network backend cost 140 to 260 ms | `devmgr` resets each device (function-level reset, D3hot, or neither, logged) and starts a new guest: a network card works again about two seconds after Linux crashes. Device-specific resets and hung guests are later |
| Qubes; Ghaf | Suspend device by device | Later |
| Qubes; Firecracker | Fixed memory (passthrough rules out ballooning), fast boot (init in 125 ms) | One committed VMO; Linux reaches its init in about 0.1 s |
| Genode; Ghaf | Narrow, typed control: a declarative Wi-Fi configuration, a filtered D-Bus | Linux's Wi-Fi stack gets session keys, never a password, and decides nothing |
| virtio-gpu; Xen | Own the presentation clock: no system found exports a GPU VM's display with its real vertical blank (virtio-gpu's emulated one has been wrong, Xen's is fixed at 60 Hz) | `displaydev` carries KMS's real flip timestamps, on Veda's clock |
| seL4's libvmm | Few driver VMs to start with: IOMMU groups bundle devices, and combining driver VMs is hard | One driver VM; nothing in the design assumes one |

## Status

Veda's only driver of its own is for disks (NVMe).
The scripts are in `verification/ui`.

| Phase | State |
|-------|-------|
| Linux runs in Veda: hypervisor, monitor, platform, its timers, console, SMP, power | Done (`architectures/amd64/drivervm.vts`) |
| The bridge: Veda's IPC in the guest | Done (`drivervm-bridge.vts`) |
| Devices: IOMMU (DMA and interrupt remapping), PCI functions given to the guest | Done (`architectures/amd64/iommu.vts`, `drivervm-pci.vts`) |
| Audio: ALSA → `audiodev`, playback and recording; virtio's sound devices | Done (`drivervm-audio.vts`, `drivervm-mic.vts`, `music.vts`, `volume.vts`) |
| Network and Wi-Fi: Linux's cards → `netdev`, nl80211 → `wlanphy` (managed radios) | Done (`drivervm-net.vts`, `e1000e.vts`, `network-cable.vts`); Wi-Fi tested on PCs |
| Display: KMS → `displaydev`, the compositor's pictures shown as they are | Done (`drivervm-display.vts`, `drivervm-display-restart.vts`, `drivervm-display-crash.vts`) |
| GPU: the renderer on Mesa's Gallium drivers (virgl under QEMU, iris, softpipe) | Done (`drivervm-gpu.vts`, `drivervm-renderer.vts`, `drivervm-compose.vts`); Intel's integrated GPUs below |
| Input and USB: event devices → `input`; USB controllers whole (keyboards, mice, network and Bluetooth adapters) | Done (`usb-input.vts`, `drivervm-usb.vts`, `live-usb.vts`); touchpads too (`architectures/amd64/drivervm-described.vts`) |
| Every device Veda does not keep goes to the driver VM, which starts with Veda | Done (every script, `architectures/amd64/iommu.vts`) |
| Restart and device reset | Done (`drivervm-restart.vts`); hangs and suspend later |
| PS/2: the keyboard controller through the monitor, its interrupts routed; touchpads as a pointer | Done (every script's keyboard, `architectures/amd64/window-keys.vts`) |
| ACPI for the guest: the platform's tables; interrupt lines (GSIs), level-triggered ones too; functions' INTx | Done (`drivervm-intx.vts`, `architectures/amd64/window-keys.vts`) |
| Devices the firmware describes below functions, in the guest's tables; a laptop's I2C controllers and HID devices (touchpads, touchscreens) to Linux | Done under QEMU (`architectures/amd64/drivervm-described.vts`); not yet tried on a PC |
| GPIO: the platform's controllers, the pins described devices are wired to, their interrupts; devices on SPI controllers | Done under QEMU (`architectures/amd64/drivervm-gpio.vts`, a simulated controller) |
| Sound: HD Audio controllers and codecs, a laptop's speaker amplifiers on SPI with their pins | Done under QEMU (`drivervm-audio.vts`, `drivervm-mic.vts`, `drivervm-pci.vts`, `startup.vts`); the amplifiers not yet tried on a PC |
| The PC's firmware variables, read-only (the amplifiers' calibration) | Done under QEMU (`drivervm-variables.vts`, variables added to OVMF's); not yet tried on a PC |
| The audio DSP of built-in microphones: SOF from Tiger Lake to Raptor Lake, with the NHLT | Done under QEMU (`architectures/amd64/drivervm-nhlt.vts`: the NHLT only, as QEMU has no DSP); not yet tried on a PC |
| Firmware-reserved memory (RMRRs) where the PC has it | Done; QEMU's machine has none, so not yet tried on a PC |
| Intel's integrated GPUs: stolen memory, OpRegion, the host bridge's stand-in, firmware; HDMI and DisplayPort audio | Done; QEMU has no Intel GPU, so not yet tried on a PC |

**GPUs.** The guest's Linux has i915 and virtio-gpu, and its Mesa iris,
virgl and softpipe. AMD's and NVIDIA's (amdgpu and radeonsi, nouveau and
NVK) are not in yet, and go in with their firmware.

## Building

`cargo automation linux [--jobs N]` builds the guest's kernel, a C and
C++ toolchain for its programs (Linux on musl) and Mesa into
`target/linux`. It needs the packages the
[README](../README.md#quick-start) installs, and checks for `gcc`, `g++`,
`make`, `m4`, `flex`, `bison`, `bc`, `perl`, `tar`, `xz`, `patch`,
`curl`, `meson` and `ninja`. The first image build runs it where those
tools are present; image builds say when a change in
`implementation/ports/` needs it run again. Every image build packs the
guest's programs and the firmware into the image's
`linux/initramfs.cpio`, beside `linux/kernel`; an image without them
drives only its disks.

The firmware is what `implementation/ports/linux/firmware.txt` lists and,
for devices only the architecture's machines have,
`implementation/ports/linux/architectures/amd64/firmware.txt` (i915's
DMC, GuC and HuC, the audio DSP's): pinned releases of linux-firmware,
wireless-regdb and sof-bin, each file checked against its SHA-256 and
under the licence the list names for it, whose text the system image
carries in `/system/licences/firmware`. A device whose firmware is not
listed does not work in the driver VM, so a driver and its firmware go in
together.

Under QEMU, Veda's hypervisor needs VMX from KVM, which takes nested
virtualization on an Intel host (`kvm_intel nested=1`):

```bash
cat /sys/module/kvm_intel/parameters/nested    # Y or 1: it is on
echo "options kvm_intel nested=1" | sudo tee /etc/modprobe.d/kvm-nested.conf
sudo modprobe -r kvm_intel && sudo modprobe kvm_intel   # reload it, with no VM running
```

## Testing

QEMU's machine has an Intel IOMMU that remaps interrupts, with the virtio
functions behind it as a PC's are (`iommu_platform`, so modern ones
only; Veda's virtio drivers accept `VIRTIO_F_ACCESS_PLATFORM`). Every
script runs the driver VM, and the GUI scripts need it:
`cargo automation test --ui` says why when it cannot run.

```bash
cargo automation script verification/ui/drivervm-restart.vts   # Linux crashes, then its monitor is killed; the wired card comes back
cargo automation run --no-iommu                  # no IOMMU in QEMU: no PCI function goes to the driver VM
cargo automation run --cmdline "drivervm=off"    # no driver VM at all
```

Its options are on the kernel command line:

| Option | Effect |
|--------|--------|
| `drivervm=off` | No driver VM |
| `drivervm` | Start it even without devices |
| `drivervm.devices=VID:DID,...` | Give it these functions too, which Veda would keep |
| `drivervm.memory=MIB` | Its memory: 256 by default; 1024 with an integrated GPU, whose memory is the guest's (less where the firmware's reserved memory starts lower) |
| `drivervm.cpus=N` | Its processors: 2 by default |
| `drivervm.run=PROGRAM,...` | Programs of the guest's `/bin` for its `init` to run |
| `drivervm.poweroff` | Power off once they are done |
| `drivervm.crash=SECONDS` | Linux crashes once, that long after it starts |
| `drivervm.cmdline=...` | Added to Linux's command line, commas for spaces; `veda.kill=NAME:SECONDS` ends a guest program by SIGKILL |
| `drivervm.renderer=softpipe` | The renderer renders on softpipe, without a GPU |

Scripts (`automation/src/script.rs`) can add a firmware variable to
OVMF's store (`firmware-variable`) or an ACPI table (`acpi-table`), a USB
network adapter (`usb net`), USB keyboards, mice and a tablet in place of
PS/2 and the virtio tablet (`input usb`), and virtio-gpu (`gpu on`),
which renders on the host's GPU the firmware showed its screen on, or the
one `VEDA_RENDERNODE` names.

What touches no hardware is unit-tested on the host: `hv`, `iommu`,
`drivervm-core`, and the guest programs' logic (touchpads, sound cards'
devices, display modes, the stick's logs, network interfaces, retries).
The renderer's decoder is tested through `gl`'s tests, on softpipe and on
Mesa's virgl over virglrenderer's test server
([Developing](DEVELOPING.md)).

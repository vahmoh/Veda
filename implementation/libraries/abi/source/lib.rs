// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Veda kernel ABI.
//!
//! This crate is the single source of truth for the interface between the
//! microkernel and user space: system call numbers and calling convention,
//! status codes, handle rights, object signals, and the plain-data structures
//! exchanged through system calls. It contains no code that depends on being
//! in the kernel or in user space.
//!
//! What depends on the processor's architecture is in [`architecture`]
//! (`implementation/libraries/abi/source/architectures/<architecture>/`),
//! and re-exported here: how system calls are made, the page size, the user
//! address space, how threads start, and the registers and exits of virtual
//! processors.
//!
//! # Calling convention
//!
//! A system call takes a call number (see [`nr`]) and up to six arguments,
//! all of them machine words, in the registers [`architecture`] names. It
//! returns a status: non-negative values are success results, negative
//! values are `-(error code)` (see [`Error`]). Some calls return a second
//! value.
//!
//! # Objects and handles
//!
//! Kernel objects (processes, threads, channels, sockets, events, memory
//! objects, interrupts, I/O port ranges, resources, virtual machines and
//! their processors) are only reachable through *handles*: per-process
//! 32-bit names that carry a set of [`Rights`].
//! Handles are capabilities — the only way to gain access to an object is to
//! be given a handle to it (through a channel message or at process start).

#![no_std]

#[cfg(target_arch = "x86_64")]
#[path = "architectures/amd64/mod.rs"]
pub mod architecture;
pub mod startup;

pub use architecture::{
    PAGE_SIZE, PROGRAM_BASE, PROGRAM_SPAN, ThreadState, USER_SPACE_END, USER_SPACE_START, VcpuSegment, VcpuState,
    entry_stack_pointer, vcpu_exit,
};

use core::fmt;

/// Where a position-independent program whose image takes `len` bytes is
/// loaded, for the random number `random`: a page of [`PROGRAM_SPAN`]
/// bytes above [`PROGRAM_BASE`] that leaves room for it.
pub fn program_base(len: usize, random: u64) -> usize {
    let pages = (PROGRAM_SPAN.saturating_sub(len.next_multiple_of(PAGE_SIZE)) / PAGE_SIZE).max(1);
    PROGRAM_BASE + (random % pages as u64) as usize * PAGE_SIZE
}

/// A handle value as seen by user space. `0` is never a valid handle.
pub type RawHandle = u32;

/// The invalid handle.
pub const INVALID_HANDLE: RawHandle = 0;

/// Deadline value meaning "wait forever".
pub const DEADLINE_INFINITE: u64 = u64::MAX;

/// Maximum number of data bytes in one channel message.
pub const CHANNEL_MAX_BYTES: usize = 64 * 1024;
/// Maximum number of handles in one channel message.
pub const CHANNEL_MAX_HANDLES: usize = 64;
/// Maximum number of items in one `object_wait_many` call.
pub const WAIT_MANY_MAX: usize = 1024;
/// Maximum number of registrations a port holds: those waiting, and those
/// done whose packets are not yet taken (`object_wait_async`).
pub const PORT_MAX_REGISTRATIONS: usize = 1 << 16;
/// Maximum number of packets one `port_wait` call takes.
pub const PORT_WAIT_MAX: usize = 256;
/// Maximum length of process and thread names.
pub const NAME_MAX: usize = 32;

/// System call numbers.
pub mod nr {
    // --- misc ---------------------------------------------------------
    /// `debug_write(ptr, len)`: append text to the kernel log.
    pub const DEBUG_WRITE: usize = 0;
    /// `log_read(offset, buf, len) -> bytes` (second value: the next offset).
    pub const LOG_READ: usize = 1;
    /// `clock_get(clock_id) -> nanoseconds`.
    pub const CLOCK_GET: usize = 2;
    /// `sleep(deadline_ns)`.
    pub const SLEEP: usize = 3;
    /// `yield()`.
    pub const YIELD: usize = 4;
    /// `system_info(buf: *mut SystemInfo)`.
    pub const SYSTEM_INFO: usize = 5;
    /// `system_power(resource, action)`.
    pub const SYSTEM_POWER: usize = 6;
    /// `random(buf, len)`: fill a buffer (at most 4096 bytes) with output of
    /// the kernel's cryptographically secure generator.
    pub const RANDOM: usize = 7;
    /// `clock_info(out: *mut ClockInfo)`: how the clocks follow the
    /// processor's clock counter.
    pub const CLOCK_INFO: usize = 8;

    // --- handles and objects ------------------------------------------
    /// `handle_close(h)`.
    pub const HANDLE_CLOSE: usize = 10;
    /// `handle_duplicate(h, rights) -> h2`.
    pub const HANDLE_DUPLICATE: usize = 11;
    /// `handle_replace(h, rights) -> h2` (closes `h`).
    pub const HANDLE_REPLACE: usize = 12;
    /// `object_info(h, topic, buf, len) -> bytes`.
    pub const OBJECT_INFO: usize = 13;
    /// `object_signal(h, clear_mask, set_mask)`: user signals only.
    pub const OBJECT_SIGNAL: usize = 14;
    /// `object_wait_one(h, signals, deadline) -> observed`.
    pub const OBJECT_WAIT_ONE: usize = 15;
    /// `object_wait_many(items: *mut WaitItem, count, deadline) -> satisfied`.
    pub const OBJECT_WAIT_MANY: usize = 16;

    // --- channels -----------------------------------------------------
    /// `channel_create(out: *mut [RawHandle; 2], flags)` (see
    /// [`channel_flags`](crate::channel_flags)).
    pub const CHANNEL_CREATE: usize = 20;
    /// `channel_write(h, bytes, n_bytes, handles, n_handles)`. The handles
    /// are consumed once their list is read, closed if the write fails.
    /// Neither end of the channel may travel on it, nor a channel that
    /// could reach it through queued messages (`NotSupported`).
    pub const CHANNEL_WRITE: usize = 21;
    /// `channel_read(h, bytes, bytes_cap, handles, handles_cap, out: *mut [u32; 2])`.
    pub const CHANNEL_READ: usize = 22;

    // --- sockets ------------------------------------------------------
    /// `socket_create(out: *mut [RawHandle; 2])`.
    pub const SOCKET_CREATE: usize = 23;
    /// `socket_write(h, buf, len, handles, n_handles) -> bytes written`
    /// (see [`SOCKET_ATOMIC_WRITE`](crate::SOCKET_ATOMIC_WRITE)). The
    /// handles (at most [`CHANNEL_MAX_HANDLES`](crate::CHANNEL_MAX_HANDLES),
    /// none for `n_handles` 0) go with the first byte, which there must be,
    /// as a channel's message carries them: consumed once their list is
    /// read, closed if no byte goes. Neither endpoint of the socket may go,
    /// nor what could reach it through what is queued (`NotSupported`); a
    /// direction holds at most 1024 handles waiting (`LimitReached`).
    pub const SOCKET_WRITE: usize = 24;
    /// `socket_read(h, buf, len, handles, handles_cap, out: *mut [u32; 2])
    /// -> bytes read`; `PeerClosed` at the end of the stream. A read takes
    /// the handles of one write at most (those of the write whose first
    /// byte it reads) and ends before the next write's; up to `handles_cap`
    /// of them go into the caller's table (their values at `handles`), the
    /// rest are closed (all of them for `handles_cap` 0). `out` (if not 0)
    /// gets how many went in and how many were closed.
    pub const SOCKET_READ: usize = 25;
    /// `socket_shutdown(h)`: this endpoint writes no more.
    pub const SOCKET_SHUTDOWN: usize = 26;
    /// `socket_peek(h, buf, len) -> bytes`: what `socket_read` would read,
    /// left to be read (the handles stay with it).
    pub const SOCKET_PEEK: usize = 27;

    // --- events -------------------------------------------------------
    /// `event_create() -> h`.
    pub const EVENT_CREATE: usize = 30;

    // --- memory objects -----------------------------------------------
    /// `vmo_create(size, flags) -> h`.
    pub const VMO_CREATE: usize = 40;
    /// `vmo_read(h, offset, buf, len)`.
    pub const VMO_READ: usize = 41;
    /// `vmo_write(h, offset, buf, len)`.
    pub const VMO_WRITE: usize = 42;
    /// `vmo_get_size(h) -> size`.
    pub const VMO_GET_SIZE: usize = 43;
    /// `vmo_create_physical(resource, paddr, size, cache_policy) -> h`.
    pub const VMO_CREATE_PHYSICAL: usize = 44;
    /// `vmo_create_contiguous(resource, size, flags) -> h` (DMA memory;
    /// flags from [`dma_flags`](crate::dma_flags)).
    pub const VMO_CREATE_CONTIGUOUS: usize = 45;
    /// `vmo_phys_addr(h, offset) -> paddr` (contiguous/physical VMOs only).
    pub const VMO_PHYS_ADDR: usize = 46;
    /// `vmo_pages(resource, h, offset, count, out) -> count`: the physical
    /// addresses of `count` pages of a VMO from `offset` (page-aligned),
    /// committed first, written to `out` (a `u64` each), for a device to
    /// reach them (a DMA resource). They stay the VMO's while it lives:
    /// pages never move, and only private memory gives pages back.
    pub const VMO_PAGES: usize = 47;
    /// `vmo_set_size(h, size)`: grows a VMO of anonymous memory to `size`
    /// bytes (rounded up to pages), which read as zeros. VMOs never shrink:
    /// no mapping of one ever covers memory it no longer has. Needs
    /// `WRITE`.
    pub const VMO_SET_SIZE: usize = 48;
    /// `vmo_create_slice(h, offset, size) -> h`: a VMO for `size` bytes
    /// (rounded up to pages) of a physical VMO's memory from `offset`
    /// (page-aligned), with the handle's rights: the same memory, which
    /// neither VMO owns, as a program shares the pages of the system image
    /// it runs from. Needs `DUPLICATE`; `NotSupported` for a VMO that owns
    /// its pages, `OutOfRange` past its end.
    pub const VMO_CREATE_SLICE: usize = 49;

    // --- address spaces -----------------------------------------------
    /// `vm_map(process, vmo, vmo_offset, len, addr, flags) -> addr`.
    pub const VM_MAP: usize = 50;
    /// `vm_unmap(process, addr, len)`.
    pub const VM_UNMAP: usize = 51;
    /// `vm_protect(process, addr, len, flags)`.
    pub const VM_PROTECT: usize = 52;
    /// `vm_allocate(process, len, addr, flags) -> addr`: maps `len` bytes
    /// of new zero-filled memory that belongs to the mapping alone (no VMO
    /// handle reaches it), so its pages are freed as soon as they are
    /// unmapped or decommitted. `addr` and `flags` as for `vm_map`.
    pub const VM_ALLOCATE: usize = 53;
    /// `vm_decommit(process, addr, len)`: frees the pages of
    /// `[addr, addr+len)`, which `vm_allocate` memory must cover completely;
    /// they read as zeros from then on. The mappings stay.
    pub const VM_DECOMMIT: usize = 54;

    // --- processes and threads ----------------------------------------
    /// `process_create(name, name_len, max_priority) -> h`: a process whose
    /// threads run at `max_priority` at most (see
    /// [`priority`](crate::priority)), or at the caller's maximum if that is
    /// lower; 0 gives it the caller's (`InvalidArgs` above
    /// [`REALTIME`](crate::priority::REALTIME)).
    pub const PROCESS_CREATE: usize = 60;
    /// `process_start(process, thread, entry, stack, arg_handle, arg1)`.
    /// The bootstrap handle `arg_handle` is consumed, closed if the start
    /// fails.
    pub const PROCESS_START: usize = 61;
    /// `process_exit(code) -> !`.
    pub const PROCESS_EXIT: usize = 62;
    /// `process_kill(h, flags, signal)`: ends the process (flags from
    /// [`kill_flags`](crate::kill_flags)): as if by POSIX signal `signal`
    /// (1 to 64), its exit code [`EXIT_CODE_SIGNALED`](crate::EXIT_CODE_SIGNALED)
    /// less the signal; with 0, killed ([`EXIT_CODE_KILLED`](crate::EXIT_CODE_KILLED)).
    pub const PROCESS_KILL: usize = 63;
    /// `thread_create(process, name, name_len) -> h`: a thread of the
    /// process at [`priority::NORMAL`](crate::priority::NORMAL), or at the
    /// process's maximum if that is lower.
    pub const THREAD_CREATE: usize = 64;
    /// `thread_start(thread, entry, stack, arg0, arg1)`.
    pub const THREAD_START: usize = 65;
    /// `thread_exit() -> !`.
    pub const THREAD_EXIT: usize = 66;
    /// `thread_set_priority(h, priority)`; `h` 0 is the calling thread.
    /// `priority` is from 1 to [`priority::REALTIME`](crate::priority::REALTIME),
    /// and at most the maximum of the thread's process and of the caller's
    /// (`AccessDenied` above it, see [`process_create`](PROCESS_CREATE)).
    pub const THREAD_SET_PRIORITY: usize = 67;
    /// `thread_set_tls(value)`: the calling thread's thread pointer, through
    /// which it finds its thread-local storage (see
    /// [`architecture`](crate::architecture)).
    pub const THREAD_SET_TLS: usize = 68;
    /// `process_list(buf: *mut ProcessInfo, cap) -> total`.
    pub const PROCESS_LIST: usize = 69;
    /// `process_open(resource, koid) -> h`.
    pub const PROCESS_OPEN: usize = 70;
    /// `thread_set_exit_futex(addr)`: when the calling thread ends with
    /// `thread_exit`, the kernel stores zero to the 32-bit word at `addr`
    /// and wakes one `futex_wait`er on it (0 = nothing to do). Thread
    /// libraries join with this: the word changes only once the thread no
    /// longer runs on its stack.
    pub const THREAD_SET_EXIT_FUTEX: usize = 71;
    /// `process_restrict(flags)`: takes abilities from the calling process,
    /// for good ([`restrict`](crate::restrict)). What it holds already
    /// (handles, mappings) stays.
    pub const PROCESS_RESTRICT: usize = 72;
    /// `thread_suspend(h)`: stops a thread, which runs no user code from
    /// then on until as many `thread_resume`s as suspensions; returns once
    /// it is stopped (signal [`SUSPENDED`](crate::signals::SUSPENDED)), its
    /// user registers saved for `thread_read_state`. Not the caller itself
    /// (`InvalidArgs`); needs `MANAGE`. A thread stopped in a system call
    /// finishes the call first.
    pub const THREAD_SUSPEND: usize = 73;
    /// `thread_resume(h)`: undoes one `thread_suspend` (`BadState` if none
    /// is in force); the thread runs again once none is.
    pub const THREAD_RESUME: usize = 74;
    /// `thread_read_state(h, buf: *mut ThreadState)`: the user registers of
    /// a thread `thread_suspend` stopped (`BadState` otherwise), as it left
    /// user code: the architecture's [`ThreadState`](crate::ThreadState).
    /// Needs `MANAGE`.
    pub const THREAD_READ_STATE: usize = 77;
    /// `thread_self() -> h`: a handle to the calling thread, with `MANAGE`
    /// (a thread library keeps its threads' handles; this gives one to the
    /// thread the process started with).
    pub const THREAD_SELF: usize = 78;
    /// `process_set_memory_class(h, class)`: what the system ends first
    /// when memory runs out ([`memory_class`](crate::memory_class)); `h` 0
    /// is the calling process, another needs `MANAGE`. A process starts in
    /// its creator's class, and only moves on towards
    /// [`EXPENDABLE`](crate::memory_class::EXPENDABLE) (`AccessDenied`
    /// back): no process can shield one it starts, or itself, more than it
    /// is shielded.
    pub const PROCESS_SET_MEMORY_CLASS: usize = 79;

    // --- futexes ------------------------------------------------------
    /// `futex_wait(addr, expected, deadline)`: waits on the futex of the
    /// 32-bit word at `addr` while the word holds `expected` (`ShouldWait`
    /// at once if it does not), until a `futex_wake` or the deadline. A
    /// futex is the word in the memory that holds it: a word of a VMO is
    /// one futex in every mapping of the VMO, in every process that maps
    /// it (what process-shared locks and semaphores wait on); one of
    /// private memory (`vm_allocate`) is the process's.
    pub const FUTEX_WAIT: usize = 75;
    /// `futex_wake(addr, count) -> woken`: wakes up to `count` threads
    /// waiting on the futex of the word at `addr`.
    pub const FUTEX_WAKE: usize = 76;

    // --- hardware access (requires resources) -------------------------
    /// `resource_create(parent, kind, base, size) -> h`.
    pub const RESOURCE_CREATE: usize = 80;
    /// `ioport_create(resource, base, count) -> h`.
    pub const IOPORT_CREATE: usize = 81;
    /// `ioport_read(h, port, width) -> value`.
    pub const IOPORT_READ: usize = 82;
    /// `ioport_write(h, port, width, value)`.
    pub const IOPORT_WRITE: usize = 83;
    /// `irq_create(resource, irq, flags) -> h`.
    pub const IRQ_CREATE: usize = 84;
    /// `irq_ack(h)`: re-arm an interrupt after handling it.
    pub const IRQ_ACK: usize = 85;
    /// `msi_create(resource, device, limit, out: *mut MsiInfo) -> h`: an
    /// MSI of PCI function `device` (its requester id: bus << 8 | device <<
    /// 3 | function, on segment 0), which the resource must name
    /// ([`resource_kind::PCI`](crate::resource_kind::PCI)). When the IOMMU
    /// remaps interrupts, only that function can raise it. `LimitReached`
    /// if the function has `limit` MSIs already (one whose last handle is
    /// closed no longer counts), or no vector is free.
    pub const MSI_CREATE: usize = 86;
    /// `irq_create_software(flags, ended) -> h`: an interrupt the caller
    /// raises (`irq_raise`), for the lines of an interrupt controller it
    /// drives itself (a GPIO controller's pins): an edge, or with
    /// [`irq_flags::LEVEL`](crate::irq_flags::LEVEL) a level-triggered
    /// one, raised until it is ended (`irq_ack`, or a guest's
    /// end-of-interrupt while it is bound to a virtual processor), which
    /// signals event `ended` (a handle with [`Rights::SIGNAL`]; 0 for
    /// none). The handle has `SIGNAL`, which raising takes: a duplicate
    /// without it is for whoever the interrupt is for.
    pub const IRQ_CREATE_SOFTWARE: usize = 87;
    /// `irq_raise(h)`: raises an interrupt made by `irq_create_software`, as
    /// a line raises its own.
    pub const IRQ_RAISE: usize = 88;

    // --- virtual machines (requires a hypervisor resource) ------------
    /// `guest_create(resource, cpus) -> h`: a virtual machine with an
    /// empty guest-physical address space, which will have `cpus`
    /// processors, numbered 0 to `cpus` - 1 (what the processors tell the
    /// guest).
    pub const GUEST_CREATE: usize = 90;
    /// `guest_map(guest, vmo, vmo_offset, len, gpa, flags)`: makes `len`
    /// bytes of a VMO the guest's memory at guest-physical address `gpa`
    /// (page-aligned), with the access `flags` give (READ, WRITE,
    /// EXECUTE of [`map_flags`](crate::map_flags)), cached as the VMO is.
    /// Every page is committed and stays so while it is mapped.
    pub const GUEST_MAP: usize = 91;
    /// `guest_unmap(guest, gpa, len)`: removes the guest's memory in
    /// `[gpa, gpa+len)`; no processor or device reaches it afterwards.
    pub const GUEST_UNMAP: usize = 92;
    /// `vcpu_create(guest, id, state: *const VcpuState) -> h`: virtual
    /// processor number `id` of the guest (as its interrupt controller
    /// knows it), in `state`.
    pub const VCPU_CREATE: usize = 93;
    /// `vcpu_run(vcpu, exit: *mut VcpuExit)`: runs the virtual processor
    /// until it needs its virtual machine monitor (see [`VcpuExit`]), on
    /// the calling thread.
    pub const VCPU_RUN: usize = 94;
    /// `vcpu_interrupt(vcpu, vector)`: raises an interrupt at the virtual
    /// processor's interrupt controller (from any thread).
    pub const VCPU_INTERRUPT: usize = 95;
    /// `vcpu_read_state(vcpu, out: *mut VcpuState)`: the virtual
    /// processor's registers, while no thread runs it.
    pub const VCPU_READ_STATE: usize = 96;
    /// `guest_attach_device(guest, resource, device)`: gives the guest PCI
    /// function `device` (a requester id, which the resource must name):
    /// its DMA reaches the guest's memory from now on, translated as the
    /// guest's processors' accesses are, and nothing else; when the guest
    /// ends, it reaches nothing. Needs the IOMMU (`NotSupported`); `Busy`
    /// if another guest has the function.
    pub const GUEST_ATTACH_DEVICE: usize = 97;
    /// `vcpu_bind_interrupt(vcpu, interrupt, vector)`: the interrupt
    /// raises `vector` (an interrupt's, not an exception's) at the virtual
    /// processor's interrupt controller from now on, instead of
    /// signaling: an edge (an MSI, an
    /// edge-triggered line) as an edge; a level-triggered line as a
    /// level-triggered interrupt, the line masked from when it fires until
    /// the guest's end-of-interrupt of the vector (if it is still bound
    /// then; `irq_ack` unmasks it too). Binding it again moves it; vector
    /// 0 unbinds it (it signals again).
    pub const VCPU_BIND_INTERRUPT: usize = 98;
    /// `port_create() -> h`: a port, where the signals of many objects are
    /// waited for at once ([`super::PortPacket`]). Its handle cannot be
    /// transferred: what it hears of is its process's.
    pub const PORT_CREATE: usize = 99;
    /// `object_wait_async(h, port, key, signals)`: once any of `signals`
    /// is active on `h`'s object (at once, if one is), the port gets a
    /// packet with `key` and the signals active then, and the
    /// registration is done: one packet each, registered again for more.
    /// `h` needs `WAIT`, the port `WRITE`. `LimitReached` if the port holds
    /// [`super::PORT_MAX_REGISTRATIONS`] already; a port cannot be waited
    /// for so (`NotSupported`).
    pub const OBJECT_WAIT_ASYNC: usize = 100;
    /// `object_wait_async_cancel(h, port, key)`: undoes the registrations
    /// on `h`'s object with `key`, and drops the packets of `key` the port
    /// holds. It is not an error that there are none.
    pub const OBJECT_WAIT_ASYNC_CANCEL: usize = 101;
    /// `port_wait(port, packets: *mut PortPacket, max, deadline) -> count`:
    /// waits until the port holds a packet, or `deadline`; takes up to
    /// `max` (at most [`super::PORT_WAIT_MAX`]), oldest first. The port
    /// needs `READ`.
    pub const PORT_WAIT: usize = 102;
}

/// Error codes returned (negated) by system calls.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// An argument was invalid.
    InvalidArgs = 1,
    /// The handle does not exist in the caller's handle table.
    BadHandle = 2,
    /// The handle refers to an object of the wrong type.
    WrongType = 3,
    /// The handle lacks the rights required for the operation.
    AccessDenied = 4,
    /// Not enough memory.
    NoMemory = 5,
    /// The requested entity does not exist.
    NotFound = 6,
    /// The operation would block (non-blocking call, empty queue, ...).
    ShouldWait = 7,
    /// The deadline passed.
    TimedOut = 8,
    /// The other end of a channel was closed.
    PeerClosed = 9,
    /// The supplied buffer is too small.
    BufferTooSmall = 10,
    /// An offset or address lies outside the valid range.
    OutOfRange = 11,
    /// The entity already exists.
    AlreadyExists = 12,
    /// The operation is not supported by this object or system.
    NotSupported = 13,
    /// The object is in the wrong state for the operation.
    BadState = 14,
    /// The operation was cancelled.
    Canceled = 15,
    /// A resource limit was reached.
    LimitReached = 16,
    /// The resource is busy.
    Busy = 17,
    /// An I/O error occurred.
    Io = 18,
    /// Internal kernel error.
    Internal = 19,
    /// A user pointer was invalid.
    Fault = 20,
    /// Unknown system call number.
    UnknownSyscall = 21,
}

impl Error {
    /// All error codes, for lookups.
    pub const ALL: [Error; 21] = [
        Error::InvalidArgs,
        Error::BadHandle,
        Error::WrongType,
        Error::AccessDenied,
        Error::NoMemory,
        Error::NotFound,
        Error::ShouldWait,
        Error::TimedOut,
        Error::PeerClosed,
        Error::BufferTooSmall,
        Error::OutOfRange,
        Error::AlreadyExists,
        Error::NotSupported,
        Error::BadState,
        Error::Canceled,
        Error::LimitReached,
        Error::Busy,
        Error::Io,
        Error::Internal,
        Error::Fault,
        Error::UnknownSyscall,
    ];

    /// Converts a positive error code back into an [`Error`].
    pub fn from_code(code: i32) -> Option<Error> {
        Error::ALL.iter().copied().find(|e| *e as i32 == code)
    }

    /// Encodes the error as a raw syscall return value.
    pub const fn as_return(self) -> isize {
        -(self as i32 as isize)
    }

    /// Decodes a raw syscall return value.
    pub fn from_return(ret: isize) -> core::result::Result<usize, Error> {
        if ret >= 0 { Ok(ret as usize) } else { Err(Error::from_code((-ret) as i32).unwrap_or(Error::Internal)) }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Error::InvalidArgs => "invalid arguments",
            Error::BadHandle => "bad handle",
            Error::WrongType => "wrong object type",
            Error::AccessDenied => "access denied",
            Error::NoMemory => "out of memory",
            Error::NotFound => "not found",
            Error::ShouldWait => "operation would block",
            Error::TimedOut => "timed out",
            Error::PeerClosed => "peer closed",
            Error::BufferTooSmall => "buffer too small",
            Error::OutOfRange => "out of range",
            Error::AlreadyExists => "already exists",
            Error::NotSupported => "not supported",
            Error::BadState => "bad state",
            Error::Canceled => "canceled",
            Error::LimitReached => "limit reached",
            Error::Busy => "busy",
            Error::Io => "I/O error",
            Error::Internal => "internal error",
            Error::Fault => "invalid user pointer",
            Error::UnknownSyscall => "unknown system call",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.description())
    }
}

/// Result type used throughout the ABI.
pub type Result<T> = core::result::Result<T, Error>;

/// The rights attached to a handle.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rights(pub u32);

impl Rights {
    pub const NONE: Rights = Rights(0);
    /// May be duplicated.
    pub const DUPLICATE: Rights = Rights(1 << 0);
    /// May be sent to another process through a channel.
    pub const TRANSFER: Rights = Rights(1 << 1);
    /// May read (channel messages, VMO contents, port input).
    pub const READ: Rights = Rights(1 << 2);
    /// May write (channel messages, VMO contents, port output).
    pub const WRITE: Rights = Rights(1 << 3);
    /// VMO may be mapped executable.
    pub const EXECUTE: Rights = Rights(1 << 4);
    /// VMO may be mapped.
    pub const MAP: Rights = Rights(1 << 5);
    /// May query object information.
    pub const GET_INFO: Rights = Rights(1 << 6);
    /// May change user signals.
    pub const SIGNAL: Rights = Rights(1 << 7);
    /// May wait on the object.
    pub const WAIT: Rights = Rights(1 << 8);
    /// May control a process/thread (map into it, start, kill, prioritise).
    pub const MANAGE: Rights = Rights(1 << 9);

    pub const BASIC: Rights = Rights(Self::DUPLICATE.0 | Self::TRANSFER.0 | Self::GET_INFO.0 | Self::WAIT.0);
    pub const IO: Rights = Rights(Self::READ.0 | Self::WRITE.0);
    pub const ALL: Rights = Rights(0x3FF);

    pub const fn contains(self, other: Rights) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Rights) -> Rights {
        Rights(self.0 | other.0)
    }

    pub const fn intersect(self, other: Rights) -> Rights {
        Rights(self.0 & other.0)
    }
}

impl core::ops::BitOr for Rights {
    type Output = Rights;
    fn bitor(self, rhs: Rights) -> Rights {
        self.union(rhs)
    }
}

impl fmt::Debug for Rights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rights({:#x})", self.0)
    }
}

/// Object signal bits, observed through `object_wait_*`.
pub mod signals {
    /// A channel has a message queued, or a socket has bytes to read.
    pub const READABLE: u32 = 1 << 0;
    /// A channel can accept another message, or a socket another
    /// [`SOCKET_ATOMIC_WRITE`](crate::SOCKET_ATOMIC_WRITE) bytes.
    pub const WRITABLE: u32 = 1 << 1;
    /// The other end of the channel or socket has been closed (every
    /// handle to it).
    pub const PEER_CLOSED: u32 = 1 << 2;
    /// Event or interrupt is signalled.
    pub const SIGNALED: u32 = 1 << 3;
    /// Process or thread has terminated.
    pub const TERMINATED: u32 = 1 << 4;
    /// The peer of a socket writes no more: what is buffered is the rest
    /// of the stream.
    pub const PEER_WRITE_DISABLED: u32 = 1 << 5;
    /// A thread is stopped for a `thread_suspend`: it runs no user code,
    /// and its user registers are saved, until it is resumed.
    pub const SUSPENDED: u32 = 1 << 6;
    /// Signals reserved for applications (settable with `object_signal`).
    pub const USER_ALL: u32 = 0xFF00_0000;
    pub const USER_0: u32 = 1 << 24;
    pub const USER_1: u32 = 1 << 25;
    pub const USER_2: u32 = 1 << 26;
    pub const USER_3: u32 = 1 << 27;
}

/// What a port says of a registration that is done (`port_wait`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PortPacket {
    /// The key it was registered with.
    pub key: u64,
    /// The object's signals then.
    pub observed: u32,
    pub _reserved: u32,
}

/// One entry of an `object_wait_many` call.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct WaitItem {
    pub handle: RawHandle,
    /// Signals of interest.
    pub signals: u32,
    /// Signals active when the wait returned (output).
    pub observed: u32,
    pub _reserved: u32,
}

/// Clock identifiers for `clock_get`.
pub mod clock {
    /// Nanoseconds since boot; never goes backwards.
    pub const MONOTONIC: usize = 0;
    /// Nanoseconds since 1970-01-01 00:00:00 in the machine's local time zone
    /// (the RTC time Veda was booted with, advanced by the monotonic clock).
    pub const REALTIME: usize = 1;
    /// Nanoseconds since 1970-01-01 00:00:00 UTC.
    pub const UTC: usize = 2;
}

/// How the clocks follow the processor's clock counter (`clock_info`),
/// which every processor reads alike, and user space too (see
/// [`architecture`]): the monotonic clock is
/// `((ticks - ticks_at_zero) * ns_per_tick) >> 32` nanoseconds; the
/// real-time clocks are it plus their offsets. It never changes once the
/// system has started, so that a program asks for it once and reads the
/// clocks itself (`rt::time`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClockInfo {
    /// The counter when the monotonic clock read 0.
    pub ticks_at_zero: u64,
    /// Nanoseconds per tick, in 32.32 fixed point.
    pub ns_per_tick: u64,
    /// The counter's frequency.
    pub ticks_hz: u64,
    /// [`clock::REALTIME`] and [`clock::UTC`] at monotonic 0.
    pub realtime_at_zero: u64,
    pub utc_at_zero: u64,
}

/// Kernel object types (reported by `object_info`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Process = 1,
    Thread = 2,
    Channel = 3,
    Event = 4,
    Vmo = 5,
    Interrupt = 6,
    IoPorts = 7,
    Resource = 8,
    Socket = 9,
    Guest = 10,
    Vcpu = 11,
    Port = 12,
}

/// Topics for `object_info`.
pub mod info_topic {
    /// [`super::HandleBasicInfo`] for any handle.
    pub const HANDLE_BASIC: usize = 1;
    /// [`super::ProcessInfo`] for a process handle.
    pub const PROCESS: usize = 2;
    /// [`super::VmoInfo`] for a VMO handle.
    pub const VMO: usize = 3;
    /// [`super::ThreadInfo`] for a thread handle.
    pub const THREAD: usize = 4;
    /// [`super::SocketInfo`] for a socket handle.
    pub const SOCKET: usize = 5;
}

/// Information about a socket endpoint.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SocketInfo {
    /// Bytes waiting to be read from this endpoint.
    pub readable: u64,
    /// Bytes a write to this endpoint would accept now.
    pub writable: u64,
}

/// Writes to a socket of at most this many bytes are never split: they go
/// in whole or not at all (POSIX `PIPE_BUF`).
pub const SOCKET_ATOMIC_WRITE: usize = 4096;

/// Bytes a socket holds each way: a writer waits for room within them (a
/// pipe's size, as POSIX programs see it).
pub const SOCKET_CAPACITY: usize = 256 * 1024;

/// Basic information about any handle. `object_info` on
/// [`INVALID_HANDLE`] describes the calling process (topics
/// [`info_topic::HANDLE_BASIC`] and [`info_topic::PROCESS`]).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HandleBasicInfo {
    /// Kernel object id, unique for the lifetime of the system.
    pub koid: u64,
    pub object_type: u32,
    pub rights: u32,
}

/// What `process_restrict` takes from a process: a program that parses
/// what it cannot trust (the media decoder) gives them up before it does.
pub mod restrict {
    /// Reading the kernel log.
    pub const NO_LOG: usize = 1 << 0;
    /// Creating, listing and opening processes (threads stay).
    pub const NO_PROCESSES: usize = 1 << 1;
    /// New executable memory: mapping, allocating or protecting pages as
    /// executable, in any process.
    pub const NO_EXECUTE: usize = 1 << 2;
    pub const ALL: usize = NO_LOG | NO_PROCESSES | NO_EXECUTE;
}

/// What the system ends first when memory runs out
/// ([`nr::PROCESS_SET_MEMORY_CLASS`]). Memory is committed as it is
/// touched, so it runs out at a page fault, which no call can return an
/// error from: the kernel then ends a process to make room (of the
/// processes above [`SYSTEM`](memory_class::SYSTEM), one of the most
/// expendable class, the one of it that holds the most), and the access is
/// made again. Only when no other may go does the process that touched the
/// page end itself.
pub mod memory_class {
    /// The system's own processes: never ended to make room for another.
    pub const SYSTEM: usize = 0;
    /// Applications and what they start.
    pub const APPLICATION: usize = 1;
    /// What costs least to lose and is started again as needed, such as a
    /// web page's process: ended first.
    pub const EXPENDABLE: usize = 2;
}

/// Flags of `process_kill`.
pub mod kill_flags {
    /// The processes it started too, those they started, and so on: a
    /// job, as a terminal ends one.
    pub const DESCENDANTS: usize = 1 << 0;
}

/// Process states reported in [`ProcessInfo::state`].
pub mod process_state {
    pub const RUNNING: u32 = 1;
    pub const EXITED: u32 = 2;
    pub const KILLED: u32 = 3;
    pub const CRASHED: u32 = 4;
    /// Ended by the system because memory ran out
    /// ([`EXIT_CODE_OUT_OF_MEMORY`](crate::EXIT_CODE_OUT_OF_MEMORY)).
    pub const OUT_OF_MEMORY: u32 = 5;
}

/// Information about a process.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessInfo {
    pub koid: u64,
    pub parent_koid: u64,
    pub name: [u8; NAME_MAX],
    pub state: u32,
    pub threads: u32,
    pub exit_code: i64,
    /// Resident memory (committed pages) in bytes.
    pub memory_bytes: u64,
    /// Total CPU time consumed by all threads, those that have ended too, in
    /// nanoseconds: each thread's as [`ThreadInfo::cpu_time_ns`] has it.
    pub cpu_time_ns: u64,
    pub handles: u32,
    /// The highest priority its threads may run at (see
    /// [`nr::PROCESS_CREATE`]).
    pub max_priority: u32,
    /// What the system ends first when memory runs out
    /// ([`memory_class`]).
    pub memory_class: u32,
    pub _reserved: u32,
}

impl Default for ProcessInfo {
    fn default() -> Self {
        ProcessInfo {
            koid: 0,
            parent_koid: 0,
            name: [0; NAME_MAX],
            state: 0,
            threads: 0,
            exit_code: 0,
            memory_bytes: 0,
            cpu_time_ns: 0,
            handles: 0,
            max_priority: 0,
            memory_class: 0,
            _reserved: 0,
        }
    }
}

impl ProcessInfo {
    pub fn name(&self) -> &str {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(NAME_MAX);
        core::str::from_utf8(&self.name[..len]).unwrap_or("?")
    }
}

/// Information about a thread.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadInfo {
    pub koid: u64,
    pub state: u32,
    pub priority: u32,
    /// The processor time it has used, in nanoseconds, up to the moment of
    /// asking: a thread running on a processor has its turn there so far
    /// counted too.
    pub cpu_time_ns: u64,
}

/// Information about a VMO.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VmoInfo {
    pub size: u64,
    pub committed_bytes: u64,
    pub flags: u32,
    pub _reserved: u32,
}

/// Flags for `channel_create`.
pub mod channel_flags {
    /// Both endpoints may be duplicated (`handle_duplicate`), as a
    /// socket's may: several processes can hold one, each message written
    /// on the other end is read by one of them (as processes share a
    /// listening socket's queue of connections), and the other end sees
    /// `PEER_CLOSED` once every handle to it is closed. Without it, an
    /// endpoint has one handle at a time.
    pub const SHARED: u32 = 1 << 0;
}

/// Flags for `vmo_create`.
pub mod vmo_flags {
    /// Commit (allocate) all pages immediately instead of on first touch.
    pub const COMMIT: usize = 1 << 0;
    /// Map the memory write-combining, for a device that reads it without
    /// snooping the CPU's caches (a display engine scanning a picture out):
    /// what the CPU writes goes to memory, not into its caches. Reading the
    /// memory from the CPU is slow.
    pub const WRITE_COMBINING: usize = 1 << 1;
}

/// Flags for `vmo_create_contiguous`.
pub mod dma_flags {
    /// Allocate the memory below 4 GiB, for devices that can only put
    /// 32-bit addresses on the bus.
    pub const BELOW_4G: usize = 1 << 0;
    /// Map the memory write-combining, for a device that reads it without
    /// snooping the CPU's caches (a display engine scanning a picture out):
    /// what the CPU writes goes to memory, not into its caches. Reading the
    /// memory from the CPU is slow.
    pub const WRITE_COMBINING: usize = 1 << 1;
}

/// Flags for `vm_map` / `vm_protect`.
pub mod map_flags {
    pub const READ: usize = 1 << 0;
    pub const WRITE: usize = 1 << 1;
    pub const EXECUTE: usize = 1 << 2;
    /// Map at exactly the given address (fail if occupied).
    pub const FIXED: usize = 1 << 3;
    /// Fault in all pages immediately.
    pub const COMMIT: usize = 1 << 4;
}

/// Cache policies for physical VMOs.
pub mod cache_policy {
    pub const WRITE_BACK: usize = 0;
    pub const WRITE_COMBINING: usize = 1;
    pub const UNCACHED: usize = 2;
}

/// Thread priorities (0 = lowest, 31 = highest). The scheduler runs the
/// highest-priority threads that are ready, so a thread that never waits
/// keeps every thread below it from its processor: a process's threads run
/// at its maximum at most ([`nr::PROCESS_CREATE`]), which `init` gives the
/// applications it starts as `NORMAL`, the system services' priority.
pub mod priority {
    pub const IDLE: usize = 0;
    pub const LOW: usize = 8;
    /// Where threads start, and the most an application's may run at.
    pub const NORMAL: usize = 16;
    pub const HIGH: usize = 24;
    /// The most a thread may be given.
    pub const REALTIME: usize = 30;
    pub const MAX: usize = 31;
}

/// Kinds of hardware [`ObjectType::Resource`]s.
pub mod resource_kind {
    /// Grants everything; held by `init` only.
    pub const ROOT: usize = 0;
    /// A range of I/O ports.
    pub const IOPORT: usize = 1;
    /// A range of interrupt lines (GSIs).
    pub const IRQ: usize = 2;
    /// A range of physical MMIO addresses.
    pub const MMIO: usize = 3;
    /// Allocation of DMA-capable (contiguous) memory.
    pub const DMA: usize = 4;
    /// Power management (shutdown/reboot).
    pub const POWER: usize = 5;
    /// Enumerating and opening arbitrary processes.
    pub const PROCESS: usize = 6;
    /// Creating virtual machines.
    pub const HYPERVISOR: usize = 7;
    /// PCI functions of segment 0, by requester id (bus << 8 | device << 3
    /// | function): making their MSIs, and giving them to virtual machines.
    pub const PCI: usize = 8;
    /// The last kind.
    pub const LAST: usize = PCI;
}

/// Flags for `irq_create`.
pub mod irq_flags {
    /// `irq` is a legacy ISA IRQ number (0-15, a PC's) rather than a GSI;
    /// the kernel applies the firmware's interrupt source overrides.
    pub const ISA: usize = 1 << 0;
    /// Level triggered (default: edge).
    pub const LEVEL: usize = 1 << 1;
    /// Active low (default: active high).
    pub const ACTIVE_LOW: usize = 1 << 2;
}

/// Result of `msi_create`: program these into the device's MSI capability.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MsiInfo {
    pub address: u64,
    pub data: u32,
    pub vector: u32,
}

/// Why `vcpu_run` returned, and what the monitor answers ([`vcpu_exit`]).
///
/// The structure goes both ways: `vcpu_run` fills it in when the virtual
/// processor needs the monitor, and the next `vcpu_run` on that processor
/// takes the monitor's answer from it (the result of a hypercall, or the
/// value an I/O port read).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VcpuExit {
    pub reason: u32,
    pub _reserved: u32,
    pub data: [u64; 7],
}

/// Actions for `system_power`.
pub mod power_action {
    pub const POWER_OFF: usize = 1;
    pub const REBOOT: usize = 2;
}

/// Global system information returned by `system_info`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SystemInfo {
    /// Kernel version string (NUL padded).
    pub version: [u8; 32],
    pub cpu_count: u32,
    pub page_size: u32,
    pub total_memory: u64,
    /// The memory programs may still take: what is free, but for the
    /// kernel's reserve, which no process is given (a page fault the
    /// system has no page for ends the process). It reaches 0 as memory
    /// runs out.
    pub free_memory: u64,
    pub uptime_ns: u64,
    pub process_count: u32,
    pub thread_count: u32,
    /// Accumulated idle time across all CPUs, up to the moment of asking
    /// (for CPU usage graphs).
    pub idle_time_ns: u64,
    /// The processor's name, as it gives it (NUL padded).
    pub cpu_brand: [u8; 48],
    /// Timer interrupts taken across all CPUs since boot: how often they
    /// were woken or interrupted for a deadline or the end of a time slice.
    pub timer_interrupts: u64,
}

impl Default for SystemInfo {
    fn default() -> Self {
        SystemInfo {
            version: [0; 32],
            cpu_count: 0,
            page_size: 0,
            total_memory: 0,
            free_memory: 0,
            uptime_ns: 0,
            process_count: 0,
            thread_count: 0,
            idle_time_ns: 0,
            cpu_brand: [0; 48],
            timer_interrupts: 0,
        }
    }
}

/// Exit code reported for processes terminated by an unhandled CPU fault.
pub const EXIT_CODE_CRASHED: i64 = -1001;
/// Exit code reported for processes killed with `process_kill`, but by
/// a signal.
pub const EXIT_CODE_KILLED: i64 = -1002;
/// Exit code of a program that panicked (set by the runtime's panic handler).
pub const EXIT_CODE_PANICKED: i64 = -1003;
/// Exit code of a process the system ended because memory ran out, to give
/// another process's access a page (see [`memory_class`]).
pub const EXIT_CODE_OUT_OF_MEMORY: i64 = -1004;
/// Exit codes of POSIX programs ended by a signal they did not handle
/// (`abort` raises `SIGABRT`): `EXIT_CODE_SIGNALED - signal`. See
/// [`exit_signal`].
pub const EXIT_CODE_SIGNALED: i64 = -2000;

/// The signal number (1-64) a process was ended by, if its exit code says
/// so: [`EXIT_CODE_SIGNALED`] codes, and the kernel's own codes as the
/// signals that stand for them (a crash is `SIGSEGV`; a kill `SIGKILL`, and
/// so is being ended for memory, as Linux's OOM killer ends a process; a
/// Rust panic `SIGABRT`).
pub const fn exit_signal(code: i64) -> Option<u32> {
    match code {
        EXIT_CODE_CRASHED => Some(11),
        EXIT_CODE_KILLED | EXIT_CODE_OUT_OF_MEMORY => Some(9),
        EXIT_CODE_PANICKED => Some(6),
        c if c < EXIT_CODE_SIGNALED && c >= EXIT_CODE_SIGNALED - 64 => Some((EXIT_CODE_SIGNALED - c) as u32),
        _ => None,
    }
}

/// Whether a process that ended with `code` crashed: a CPU fault, a panic
/// (`rt`'s), or for a POSIX program a signal that a fault of its own
/// raises: `SIGABRT` (from `abort`, as a Rust panic in a program on the
/// standard library raises it), `SIGSEGV`, `SIGBUS`, `SIGILL` or `SIGFPE`.
/// A process ended by another (killed) did not crash.
pub const fn crashed(code: i64) -> bool {
    match code {
        EXIT_CODE_CRASHED | EXIT_CODE_PANICKED => true,
        c if c < EXIT_CODE_SIGNALED && c >= EXIT_CODE_SIGNALED - 64 => {
            matches!(EXIT_CODE_SIGNALED - c, 4 | 6 | 7 | 8 | 11)
        }
        _ => false,
    }
}

/// Boot-time information the kernel hands to `init` in a VMO.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KernelBootInfo {
    pub framebuffer_width: u32,
    pub framebuffer_height: u32,
    /// Bytes per scan line.
    pub framebuffer_pitch: u32,
    /// 1 = BGRX, 2 = RGBX.
    pub framebuffer_format: u32,
    pub framebuffer_size: u64,
    /// Where the framebuffer is in physical memory (for a display driver's
    /// handover, which must take over the very picture the firmware set).
    pub framebuffer_phys: u64,
    pub initrd_size: u64,
    pub cmdline: [u8; 256],
    pub cmdline_len: u32,
    pub cpu_count: u32,
    /// Physical address of the ACPI RSDP (0: none).
    pub acpi_rsdp: u64,
    /// The firmware's ACPI memory (its tables and their variables), as
    /// `[start, end)` ranges: RAM, to be mapped cached.
    pub acpi_memory: [[u64; 2]; ACPI_MEMORY_RANGES],
    pub acpi_memory_count: u32,
    /// What the machine offers beyond what Veda needs ([`platform`]).
    pub platform: u32,
}

/// Ranges of [`KernelBootInfo::acpi_memory`].
pub const ACPI_MEMORY_RANGES: usize = 16;

/// Bits of [`KernelBootInfo::platform`].
pub mod platform {
    /// The processors run virtual machines: a hypervisor resource makes
    /// guests.
    pub const VIRTUALIZATION: u32 = 1 << 0;
    /// An IOMMU confines devices' DMA and remaps their interrupts: PCI
    /// functions can be given to guests.
    pub const IOMMU: u32 = 1 << 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faults_are_crashes_and_being_ended_is_not() {
        assert!(crashed(EXIT_CODE_CRASHED) && crashed(EXIT_CODE_PANICKED));
        // `abort` (a Rust panic on the standard library), a bad access.
        assert!(crashed(EXIT_CODE_SIGNALED - 6) && crashed(EXIT_CODE_SIGNALED - 11));
        // Killed, ended for memory or by another's signal; a plain exit.
        for code in
            [EXIT_CODE_KILLED, EXIT_CODE_OUT_OF_MEMORY, EXIT_CODE_SIGNALED - 9, EXIT_CODE_SIGNALED - 15, 0, 1, -1]
        {
            assert!(!crashed(code), "{code}");
        }
        assert_eq!(exit_signal(EXIT_CODE_SIGNALED - 6), Some(6));
        // Ended for memory: SIGKILL, as Linux's OOM killer ends a process.
        assert_eq!(exit_signal(EXIT_CODE_OUT_OF_MEMORY), Some(9));
    }
}

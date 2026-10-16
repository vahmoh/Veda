// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Safe, owning wrappers around kernel object handles.
//!
//! Every wrapper closes its handle when dropped. Use `into_raw`/`from_raw`
//! to pass handles across FFI or protocol boundaries.

use alloc::vec::Vec;

use vabi::{Error, HandleBasicInfo, ProcessInfo, RawHandle, Rights, ThreadInfo, WaitItem, info_topic, nr};

use crate::sys::{self, call, call2};

/// An owned handle of any type.
#[derive(Debug)]
#[repr(transparent)]
pub struct Handle(RawHandle);

impl Handle {
    /// Takes ownership of a raw handle value.
    ///
    /// # Safety
    /// `raw` must be a valid handle owned by nobody else (or 0).
    pub const unsafe fn from_raw(raw: RawHandle) -> Handle {
        Handle(raw)
    }

    pub const fn invalid() -> Handle {
        Handle(vabi::INVALID_HANDLE)
    }

    pub fn is_valid(&self) -> bool {
        self.0 != vabi::INVALID_HANDLE
    }

    pub fn raw(&self) -> RawHandle {
        self.0
    }

    /// Releases ownership without closing.
    pub fn into_raw(self) -> RawHandle {
        let raw = self.0;
        core::mem::forget(self);
        raw
    }

    /// Duplicates the handle, optionally with fewer rights.
    pub fn duplicate(&self, rights: Option<Rights>) -> Result<Handle, Error> {
        let r = rights.map(|r| r.0).unwrap_or(u32::MAX);
        call(nr::HANDLE_DUPLICATE, [self.0 as usize, r as usize, 0, 0, 0, 0]).map(|h| Handle(h as RawHandle))
    }

    /// Waits until any of `signals` is active or `deadline` (monotonic ns)
    /// passes. Returns the active signals.
    pub fn wait(&self, signals: u32, deadline: u64) -> Result<u32, Error> {
        call(nr::OBJECT_WAIT_ONE, [self.0 as usize, signals as usize, deadline as usize, 0, 0, 0]).map(|s| s as u32)
    }

    /// Sets/clears user signals on the object.
    pub fn signal(&self, clear: u32, set: u32) -> Result<(), Error> {
        call(nr::OBJECT_SIGNAL, [self.0 as usize, clear as usize, set as usize, 0, 0, 0]).map(|_| ())
    }

    pub fn basic_info(&self) -> Result<HandleBasicInfo, Error> {
        let mut info = HandleBasicInfo::default();
        call(
            nr::OBJECT_INFO,
            [
                self.0 as usize,
                info_topic::HANDLE_BASIC,
                &mut info as *mut _ as usize,
                core::mem::size_of_val(&info),
                0,
                0,
            ],
        )?;
        Ok(info)
    }

    pub fn koid(&self) -> u64 {
        self.basic_info().map(|i| i.koid).unwrap_or(0)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.0 != vabi::INVALID_HANDLE {
            let _ = sys::handle_close(self.0);
        }
    }
}

macro_rules! wrapper {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug)]
        #[repr(transparent)]
        pub struct $name(pub Handle);

        impl $name {
            pub fn raw(&self) -> RawHandle {
                self.0.raw()
            }
            pub fn into_handle(self) -> Handle {
                self.0
            }
            pub fn from_handle(h: Handle) -> Self {
                $name(h)
            }
            pub fn wait(&self, signals: u32, deadline: u64) -> Result<u32, Error> {
                self.0.wait(signals, deadline)
            }
        }

        impl From<$name> for Handle {
            fn from(v: $name) -> Handle {
                v.0
            }
        }
    };
}

wrapper!(
    /// A channel endpoint.
    Channel
);
wrapper!(
    /// A socket endpoint: one end of a byte stream (see `vabi::nr::SOCKET_CREATE`).
    Socket
);
wrapper!(
    /// A memory object.
    Vmo
);
wrapper!(
    /// An event (signal word).
    Event
);
wrapper!(
    /// A process.
    Process
);
wrapper!(
    /// A thread.
    Thread
);
wrapper!(
    /// A hardware interrupt bound to this driver.
    Interrupt
);
wrapper!(
    /// A range of I/O ports.
    IoPorts
);
wrapper!(
    /// A hardware resource capability.
    Resource
);
wrapper!(
    /// A port: where the signals of many objects are waited for at once
    /// ([`wait_async`], [`Port::wait_packets`]).
    Port
);
wrapper!(
    /// A virtual machine: a guest-physical address space and its virtual
    /// processors.
    Guest
);
wrapper!(
    /// A virtual processor of a [`Guest`].
    Vcpu
);

/// The bytes [`Channel::read`] first makes room for: most messages fit.
pub const READ_BYTES: usize = 4096;
/// The handles [`Channel::read`] first makes room for.
pub const READ_HANDLES: usize = 16;

/// A message read from a channel.
#[derive(Debug, Default)]
pub struct Message {
    pub bytes: Vec<u8>,
    pub handles: Vec<Handle>,
}

impl Channel {
    pub fn create() -> Result<(Channel, Channel), Error> {
        Channel::create_with(0)
    }

    /// A channel whose endpoints may be duplicated ([`Handle::duplicate`]),
    /// so that several processes hold one: each message written on the
    /// other end is read by one of them (see
    /// [`vabi::channel_flags::SHARED`]).
    pub fn create_shared() -> Result<(Channel, Channel), Error> {
        Channel::create_with(vabi::channel_flags::SHARED)
    }

    fn create_with(flags: u32) -> Result<(Channel, Channel), Error> {
        let mut out = [0u32; 2];
        call(nr::CHANNEL_CREATE, [out.as_mut_ptr() as usize, flags as usize, 0, 0, 0, 0])?;
        // SAFETY: the kernel just gave us both handles.
        Ok(unsafe { (Channel(Handle::from_raw(out[0])), Channel(Handle::from_raw(out[1]))) })
    }

    /// Sends bytes and transfers ownership of `handles` (consumed even on
    /// error, mirroring the kernel's semantics).
    pub fn write(&self, bytes: &[u8], handles: Vec<Handle>) -> Result<(), Error> {
        let raws: Vec<RawHandle> = handles.into_iter().map(Handle::into_raw).collect();
        call(
            nr::CHANNEL_WRITE,
            [self.raw() as usize, bytes.as_ptr() as usize, bytes.len(), raws.as_ptr() as usize, raws.len(), 0],
        )
        .map(|_| ())
    }

    /// Reads one message into the given buffers. On `BufferTooSmall`, returns
    /// the required sizes in the error payload via [`Channel::read`].
    pub fn read_into(
        &self,
        bytes: &mut [u8],
        handles: &mut [RawHandle],
    ) -> Result<(usize, usize), (Error, usize, usize)> {
        let mut actual = [0u32; 2];
        let r = call(
            nr::CHANNEL_READ,
            [
                self.raw() as usize,
                bytes.as_mut_ptr() as usize,
                bytes.len(),
                handles.as_mut_ptr() as usize,
                handles.len(),
                actual.as_mut_ptr() as usize,
            ],
        );
        match r {
            Ok(_) => Ok((actual[0] as usize, actual[1] as usize)),
            Err(e) => Err((e, actual[0] as usize, actual[1] as usize)),
        }
    }

    /// Reads one message of any size without blocking. The buffers are
    /// sized for most messages ([`READ_BYTES`], [`READ_HANDLES`]); a larger
    /// one takes a second read into buffers of exactly its size. Neither is
    /// cleared first: they are filled from what the kernel writes.
    pub fn read(&self) -> Result<Message, Error> {
        let mut bytes: Vec<u8> = Vec::with_capacity(READ_BYTES);
        let mut raws: Vec<RawHandle> = Vec::with_capacity(READ_HANDLES);
        loop {
            let (byte_room, handle_room) = (bytes.capacity(), raws.capacity());
            let mut actual = [0u32; 2];
            let r = call(
                nr::CHANNEL_READ,
                [
                    self.raw() as usize,
                    bytes.as_mut_ptr() as usize,
                    byte_room,
                    raws.as_mut_ptr() as usize,
                    handle_room,
                    actual.as_mut_ptr() as usize,
                ],
            );
            let (n, h) = (actual[0] as usize, actual[1] as usize);
            match r {
                Ok(_) => {
                    if n > byte_room || h > handle_room {
                        // Not what the kernel does; nothing to trust.
                        return Err(Error::Internal);
                    }
                    // SAFETY: the kernel wrote the message's `n` bytes and
                    // `h` handles, which fit (checked above), to the start
                    // of the buffers.
                    unsafe {
                        bytes.set_len(n);
                        raws.set_len(h);
                    }
                    // SAFETY: the kernel transferred these handles to us.
                    let handles = raws.into_iter().map(|r| unsafe { Handle::from_raw(r) }).collect();
                    return Ok(Message { bytes, handles });
                }
                Err(Error::BufferTooSmall) if n > byte_room || h > handle_room => {
                    // The message stays queued: room for all of it.
                    bytes.reserve_exact(n);
                    raws.reserve_exact(h);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Blocks until a message arrives (or the peer closes), then reads it.
    pub fn read_blocking(&self, deadline: u64) -> Result<Message, Error> {
        loop {
            match self.read() {
                Err(Error::ShouldWait) => {
                    let s = self.wait(vabi::signals::READABLE | vabi::signals::PEER_CLOSED, deadline)?;
                    if s & vabi::signals::READABLE == 0 && s & vabi::signals::PEER_CLOSED != 0 {
                        return Err(Error::PeerClosed);
                    }
                }
                other => return other,
            }
        }
    }
}

impl Socket {
    /// A connected pair of endpoints.
    pub fn create() -> Result<(Socket, Socket), Error> {
        let mut out = [0u32; 2];
        call(nr::SOCKET_CREATE, [out.as_mut_ptr() as usize, 0, 0, 0, 0, 0])?;
        // SAFETY: the kernel just gave us both handles.
        Ok(unsafe { (Socket(Handle::from_raw(out[0])), Socket(Handle::from_raw(out[1]))) })
    }

    /// Writes as much of `data` as fits without blocking and returns how
    /// much that was (`ShouldWait` if nothing fits; writes of at most
    /// `vabi::SOCKET_ATOMIC_WRITE` bytes go in whole or not at all).
    pub fn write(&self, data: &[u8]) -> Result<usize, Error> {
        call(nr::SOCKET_WRITE, [self.raw() as usize, data.as_ptr() as usize, data.len(), 0, 0, 0])
    }

    /// Reads what is buffered, up to `buf.len()` bytes, without blocking.
    /// `PeerClosed` marks the end of the stream. Handles that came with the
    /// bytes are closed.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, Error> {
        call(nr::SOCKET_READ, [self.raw() as usize, buf.as_mut_ptr() as usize, buf.len(), 0, 0, 0])
    }

    /// [`write`](Self::write), with `handles` going with the first byte of
    /// `data` (which there must be): they come out with the read that
    /// reaches it. They are consumed even on error (closed when no byte
    /// goes).
    pub fn write_with(&self, data: &[u8], handles: Vec<Handle>) -> Result<usize, Error> {
        let raws: Vec<RawHandle> = handles.into_iter().map(Handle::into_raw).collect();
        call(
            nr::SOCKET_WRITE,
            [self.raw() as usize, data.as_ptr() as usize, data.len(), raws.as_ptr() as usize, raws.len(), 0],
        )
    }

    /// [`read`](Self::read), and the handles that came with the bytes it
    /// read (those of one write at most: the read ends before the next
    /// write's): up to `cap` of them, and how many more were closed.
    pub fn read_with(&self, buf: &mut [u8], cap: usize) -> Result<(usize, Vec<Handle>, usize), Error> {
        let cap = cap.min(vabi::CHANNEL_MAX_HANDLES);
        let mut raws: Vec<RawHandle> = alloc::vec![vabi::INVALID_HANDLE; cap];
        let mut actual = [0u32; 2];
        let n = call(
            nr::SOCKET_READ,
            [
                self.raw() as usize,
                buf.as_mut_ptr() as usize,
                buf.len(),
                raws.as_mut_ptr() as usize,
                cap,
                actual.as_mut_ptr() as usize,
            ],
        )?;
        let got = (actual[0] as usize).min(cap);
        // SAFETY: the kernel put these handles in our table for us.
        let handles = raws[..got].iter().map(|&r| unsafe { Handle::from_raw(r) }).collect();
        Ok((n, handles, actual[1] as usize))
    }

    /// Copies what [`read`](Self::read) would read into `buf`, and leaves
    /// it to be read.
    pub fn peek(&self, buf: &mut [u8]) -> Result<usize, Error> {
        call(nr::SOCKET_PEEK, [self.raw() as usize, buf.as_mut_ptr() as usize, buf.len(), 0, 0, 0])
    }

    /// Writes all of `data`, waiting for room as needed.
    pub fn write_all(&self, mut data: &[u8]) -> Result<(), Error> {
        use vabi::signals::{PEER_CLOSED, WRITABLE};
        while !data.is_empty() {
            match self.write(data) {
                Ok(n) => data = &data[n..],
                Err(Error::ShouldWait) => {
                    self.wait(WRITABLE | PEER_CLOSED, vabi::DEADLINE_INFINITE)?;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Reads at least one byte, waiting until there is one; `Ok(0)` at the
    /// end of the stream.
    pub fn read_blocking(&self, buf: &mut [u8]) -> Result<usize, Error> {
        use vabi::signals::{PEER_CLOSED, PEER_WRITE_DISABLED, READABLE};
        loop {
            match self.read(buf) {
                Err(Error::ShouldWait) => {
                    self.wait(READABLE | PEER_CLOSED | PEER_WRITE_DISABLED, vabi::DEADLINE_INFINITE)?;
                }
                Err(Error::PeerClosed) => return Ok(0),
                other => return other,
            }
        }
    }

    /// Stops writing from this endpoint (the peer reads the end of the
    /// stream once it has read what is buffered).
    pub fn shutdown(&self) -> Result<(), Error> {
        call(nr::SOCKET_SHUTDOWN, [self.raw() as usize, 0, 0, 0, 0, 0]).map(|_| ())
    }

    /// Bytes buffered for reading here, and bytes a write would accept now.
    pub fn info(&self) -> Result<vabi::SocketInfo, Error> {
        let mut info = vabi::SocketInfo::default();
        call(
            nr::OBJECT_INFO,
            [self.raw() as usize, info_topic::SOCKET, &mut info as *mut _ as usize, size_of_val(&info), 0, 0],
        )?;
        Ok(info)
    }

    /// Another handle to the same endpoint, with all rights or `rights`.
    pub fn duplicate(&self, rights: Option<Rights>) -> Result<Socket, Error> {
        self.0.duplicate(rights).map(Socket)
    }
}

impl Vmo {
    pub fn create(size: usize) -> Result<Vmo, Error> {
        call(nr::VMO_CREATE, [size, 0, 0, 0, 0, 0]).map(|h| Vmo(Handle(h as RawHandle)))
    }

    pub fn create_committed(size: usize) -> Result<Vmo, Error> {
        Vmo::create_with(size, vabi::vmo_flags::COMMIT)
    }

    /// Memory as `flags` ([`vabi::vmo_flags`]) ask: committed now,
    /// write-combining.
    pub fn create_with(size: usize, flags: usize) -> Result<Vmo, Error> {
        call(nr::VMO_CREATE, [size, flags, 0, 0, 0, 0]).map(|h| Vmo(Handle(h as RawHandle)))
    }

    pub fn create_physical(res: &Resource, paddr: u64, size: usize, cache: usize) -> Result<Vmo, Error> {
        call(nr::VMO_CREATE_PHYSICAL, [res.raw() as usize, paddr as usize, size, cache, 0, 0])
            .map(|h| Vmo(Handle(h as RawHandle)))
    }

    pub fn create_contiguous(res: &Resource, size: usize) -> Result<Vmo, Error> {
        call(nr::VMO_CREATE_CONTIGUOUS, [res.raw() as usize, size, 0, 0, 0, 0]).map(|h| Vmo(Handle(h as RawHandle)))
    }

    /// Like [`Vmo::create_contiguous`], below 4 GiB (for devices that can
    /// only address 32 bits).
    pub fn create_contiguous_below_4g(res: &Resource, size: usize) -> Result<Vmo, Error> {
        Vmo::create_dma(res, size, vabi::dma_flags::BELOW_4G)
    }

    /// Physically contiguous memory for a device, as `flags`
    /// ([`vabi::dma_flags`]) ask: below 4 GiB, write-combining.
    pub fn create_dma(res: &Resource, size: usize, flags: usize) -> Result<Vmo, Error> {
        call(nr::VMO_CREATE_CONTIGUOUS, [res.raw() as usize, size, flags, 0, 0, 0]).map(|h| Vmo(Handle(h as RawHandle)))
    }

    pub fn size(&self) -> Result<usize, Error> {
        call(nr::VMO_GET_SIZE, [self.raw() as usize, 0, 0, 0, 0, 0])
    }

    pub fn read(&self, offset: usize, buf: &mut [u8]) -> Result<(), Error> {
        call(nr::VMO_READ, [self.raw() as usize, offset, buf.as_mut_ptr() as usize, buf.len(), 0, 0]).map(|_| ())
    }

    pub fn write(&self, offset: usize, buf: &[u8]) -> Result<(), Error> {
        call(nr::VMO_WRITE, [self.raw() as usize, offset, buf.as_ptr() as usize, buf.len(), 0, 0]).map(|_| ())
    }

    pub fn phys_addr(&self, offset: usize) -> Result<u64, Error> {
        call(nr::VMO_PHYS_ADDR, [self.raw() as usize, offset, 0, 0, 0, 0]).map(|a| a as u64)
    }

    /// The physical addresses of the pages from `offset` (page-aligned),
    /// one per element of `out`, committed first: for a device to reach
    /// them (`res` is a DMA resource).
    pub fn pages(&self, res: &Resource, offset: usize, out: &mut [u64]) -> Result<(), Error> {
        call(nr::VMO_PAGES, [res.raw() as usize, self.raw() as usize, offset, out.len(), out.as_mut_ptr() as usize, 0])
            .map(|_| ())
    }

    /// Maps the VMO into the current process; see [`crate::vm::map`].
    pub fn map(&self, offset: usize, len: usize, flags: usize) -> Result<usize, Error> {
        crate::vm::map(None, self, offset, len, 0, flags)
    }
}

impl Port {
    pub fn create() -> Result<Port, Error> {
        call(nr::PORT_CREATE, [0; 6]).map(|h| Port(Handle(h as RawHandle)))
    }

    /// Waits until the port holds a packet or `deadline` passes, and takes
    /// as many as `out` holds (at most `vabi::PORT_WAIT_MAX`), oldest
    /// first: how many. `TimedOut` if the deadline passed.
    pub fn wait_packets(&self, out: &mut [vabi::PortPacket], deadline: u64) -> Result<usize, Error> {
        let max = out.len().min(vabi::PORT_WAIT_MAX);
        call(nr::PORT_WAIT, [self.raw() as usize, out.as_mut_ptr() as usize, max, deadline as usize, 0, 0])
    }
}

/// Has `port` get a packet with `key` once any of `signals` is active on
/// `handle`'s object (at once, if one is): one packet, then register again
/// for more (`vabi::nr::OBJECT_WAIT_ASYNC`).
pub fn wait_async(handle: RawHandle, port: &Port, key: u64, signals: u32) -> Result<(), Error> {
    call(nr::OBJECT_WAIT_ASYNC, [handle as usize, port.raw() as usize, key as usize, signals as usize, 0, 0])
        .map(|_| ())
}

/// Undoes the registrations of `key` with `port` on `handle`'s object, and
/// drops the packets of `key` the port holds.
pub fn wait_async_cancel(handle: RawHandle, port: &Port, key: u64) -> Result<(), Error> {
    call(nr::OBJECT_WAIT_ASYNC_CANCEL, [handle as usize, port.raw() as usize, key as usize, 0, 0, 0]).map(|_| ())
}

impl Event {
    pub fn create() -> Result<Event, Error> {
        call(nr::EVENT_CREATE, [0; 6]).map(|h| Event(Handle(h as RawHandle)))
    }

    pub fn signal(&self) -> Result<(), Error> {
        self.0.signal(0, vabi::signals::SIGNALED)
    }

    pub fn clear(&self) -> Result<(), Error> {
        self.0.signal(vabi::signals::SIGNALED, 0)
    }
}

impl Thread {
    /// The thread's state, priority and processor time.
    pub fn info(&self) -> Result<ThreadInfo, Error> {
        let mut info = ThreadInfo::default();
        call(
            nr::OBJECT_INFO,
            [
                self.raw() as usize,
                info_topic::THREAD,
                &mut info as *mut _ as usize,
                core::mem::size_of_val(&info),
                0,
                0,
            ],
        )?;
        Ok(info)
    }
}

impl Process {
    pub fn info(&self) -> Result<ProcessInfo, Error> {
        let mut info = ProcessInfo::default();
        call(
            nr::OBJECT_INFO,
            [
                self.raw() as usize,
                info_topic::PROCESS,
                &mut info as *mut _ as usize,
                core::mem::size_of_val(&info),
                0,
                0,
            ],
        )?;
        Ok(info)
    }

    pub fn kill(&self) -> Result<(), Error> {
        call(nr::PROCESS_KILL, [self.raw() as usize, 0, 0, 0, 0, 0]).map(|_| ())
    }

    /// Kills the process and every process it started that still runs
    /// (and those they started): its job, as a terminal's Ctrl+C ends it.
    pub fn kill_tree(&self) -> Result<(), Error> {
        call(nr::PROCESS_KILL, [self.raw() as usize, vabi::kill_flags::DESCENDANTS, 0, 0, 0, 0]).map(|_| ())
    }

    /// Waits for the process to terminate and returns its exit code.
    pub fn join(&self, deadline: u64) -> Result<i64, Error> {
        self.wait(vabi::signals::TERMINATED, deadline)?;
        Ok(self.info()?.exit_code)
    }
}

impl Interrupt {
    pub fn create(res: &Resource, irq: usize, flags: usize) -> Result<Interrupt, Error> {
        call(nr::IRQ_CREATE, [res.raw() as usize, irq, flags, 0, 0, 0]).map(|h| Interrupt(Handle(h as RawHandle)))
    }

    /// Allocates an MSI vector for PCI function `device` (its requester
    /// id, which the PCI resource names), of which it may have `limit` at
    /// once; returns the interrupt and the address/data pair to program
    /// into the device.
    pub fn create_msi(res: &Resource, device: u16, limit: u32) -> Result<(Interrupt, vabi::MsiInfo), Error> {
        let mut info = vabi::MsiInfo::default();
        let args = [res.raw() as usize, device as usize, limit as usize, &mut info as *mut _ as usize, 0, 0];
        let h = call(nr::MSI_CREATE, args)?;
        Ok((Interrupt(Handle(h as RawHandle)), info))
    }

    /// An interrupt the caller raises itself ([`Interrupt::raise`]), for a
    /// line of a controller it drives: an edge, or a level-triggered one,
    /// raised until it is ended, which signals `ended`. Whoever it is for
    /// gets a duplicate without `SIGNAL`, which raising takes.
    pub fn create_software(level: bool, ended: Option<&Event>) -> Result<Interrupt, Error> {
        let flags = if level { vabi::irq_flags::LEVEL } else { 0 };
        let ended = ended.map_or(vabi::INVALID_HANDLE, |e| e.raw());
        call(nr::IRQ_CREATE_SOFTWARE, [flags, ended as usize, 0, 0, 0, 0]).map(|h| Interrupt(Handle(h as RawHandle)))
    }

    /// Raises an interrupt made by [`Interrupt::create_software`].
    pub fn raise(&self) -> Result<(), Error> {
        call(nr::IRQ_RAISE, [self.raw() as usize, 0, 0, 0, 0, 0]).map(|_| ())
    }

    /// Re-arms the interrupt after servicing the device.
    pub fn ack(&self) -> Result<(), Error> {
        call(nr::IRQ_ACK, [self.raw() as usize, 0, 0, 0, 0, 0]).map(|_| ())
    }

    /// Blocks until the interrupt fires.
    pub fn wait_irq(&self, deadline: u64) -> Result<(), Error> {
        self.wait(vabi::signals::SIGNALED, deadline).map(|_| ())
    }
}

impl IoPorts {
    pub fn create(res: &Resource, base: u16, count: u16) -> Result<IoPorts, Error> {
        call(nr::IOPORT_CREATE, [res.raw() as usize, base as usize, count as usize, 0, 0, 0])
            .map(|h| IoPorts(Handle(h as RawHandle)))
    }

    pub fn in8(&self, port: u16) -> u8 {
        call(nr::IOPORT_READ, [self.raw() as usize, port as usize, 1, 0, 0, 0]).unwrap_or(0xFF) as u8
    }

    pub fn in16(&self, port: u16) -> u16 {
        call(nr::IOPORT_READ, [self.raw() as usize, port as usize, 2, 0, 0, 0]).unwrap_or(0xFFFF) as u16
    }

    pub fn in32(&self, port: u16) -> u32 {
        call(nr::IOPORT_READ, [self.raw() as usize, port as usize, 4, 0, 0, 0]).unwrap_or(u32::MAX as usize) as u32
    }

    pub fn out8(&self, port: u16, v: u8) {
        let _ = call(nr::IOPORT_WRITE, [self.raw() as usize, port as usize, 1, v as usize, 0, 0]);
    }

    pub fn out16(&self, port: u16, v: u16) {
        let _ = call(nr::IOPORT_WRITE, [self.raw() as usize, port as usize, 2, v as usize, 0, 0]);
    }

    pub fn out32(&self, port: u16, v: u32) {
        let _ = call(nr::IOPORT_WRITE, [self.raw() as usize, port as usize, 4, v as usize, 0, 0]);
    }
}

impl Resource {
    /// Derives a narrower resource (see `vabi::resource_kind`).
    pub fn create(&self, kind: usize, base: u64, size: u64) -> Result<Resource, Error> {
        call(nr::RESOURCE_CREATE, [self.raw() as usize, kind, base as usize, size as usize, 0, 0])
            .map(|h| Resource(Handle(h as RawHandle)))
    }

    pub fn duplicate(&self) -> Result<Resource, Error> {
        self.0.duplicate(None).map(Resource)
    }
}

impl Guest {
    /// A virtual machine that will have `cpus` processors (a hypervisor
    /// resource allows it).
    pub fn create(res: &Resource, cpus: u32) -> Result<Guest, Error> {
        call(nr::GUEST_CREATE, [res.raw() as usize, cpus as usize, 0, 0, 0, 0]).map(|h| Guest(Handle(h as RawHandle)))
    }

    /// Makes `len` bytes of `vmo` from `offset` the guest's memory at
    /// `gpa`, with the access `flags` give (`vabi::map_flags`).
    pub fn map(&self, vmo: &Vmo, offset: usize, len: usize, gpa: u64, flags: usize) -> Result<(), Error> {
        call(nr::GUEST_MAP, [self.raw() as usize, vmo.raw() as usize, offset, len, gpa as usize, flags]).map(|_| ())
    }

    /// Removes the guest's memory in `[gpa, gpa+len)` (whole mappings).
    pub fn unmap(&self, gpa: u64, len: usize) -> Result<(), Error> {
        call(nr::GUEST_UNMAP, [self.raw() as usize, gpa as usize, len, 0, 0, 0]).map(|_| ())
    }

    /// A virtual processor with id `id` (its interrupt controller's: the
    /// local APIC's on AMD64), in `state`.
    pub fn create_vcpu(&self, id: u32, state: &vabi::VcpuState) -> Result<Vcpu, Error> {
        call(nr::VCPU_CREATE, [self.raw() as usize, id as usize, state as *const _ as usize, 0, 0, 0])
            .map(|h| Vcpu(Handle(h as RawHandle)))
    }

    /// Gives the guest PCI function `device` (a requester id, which the
    /// PCI resource names): its DMA reaches the guest's memory, and only
    /// that, until the guest ends.
    pub fn attach_device(&self, res: &Resource, device: u16) -> Result<(), Error> {
        call(nr::GUEST_ATTACH_DEVICE, [self.raw() as usize, res.raw() as usize, device as usize, 0, 0, 0]).map(|_| ())
    }
}

impl Vcpu {
    /// Runs the virtual processor until it needs the monitor; `exit` brings
    /// the answer to the last exit and takes the next (see
    /// `vabi::VcpuExit`).
    pub fn run(&self, exit: &mut vabi::VcpuExit) -> Result<(), Error> {
        call(nr::VCPU_RUN, [self.raw() as usize, exit as *mut _ as usize, 0, 0, 0, 0]).map(|_| ())
    }

    /// Raises interrupt `vector` at the virtual processor's interrupt
    /// controller.
    pub fn interrupt(&self, vector: u8) -> Result<(), Error> {
        call(nr::VCPU_INTERRUPT, [self.raw() as usize, vector as usize, 0, 0, 0, 0]).map(|_| ())
    }

    /// Makes `irq` raise `vector` at the virtual processor's interrupt
    /// controller, rather than signal, from now on (a level-triggered line
    /// masked until the guest's end-of-interrupt); vector 0 makes it signal
    /// again.
    pub fn bind_interrupt(&self, irq: &Interrupt, vector: u8) -> Result<(), Error> {
        call(nr::VCPU_BIND_INTERRUPT, [self.raw() as usize, irq.raw() as usize, vector as usize, 0, 0, 0]).map(|_| ())
    }

    /// The registers, while no thread runs the virtual processor.
    pub fn state(&self) -> Result<vabi::VcpuState, Error> {
        let mut state = vabi::VcpuState::default();
        call(nr::VCPU_READ_STATE, [self.raw() as usize, &mut state as *mut _ as usize, 0, 0, 0, 0])?;
        Ok(state)
    }
}

/// Waits on several objects at once. Returns the number of satisfied items;
/// each item's `observed` field is filled in.
pub fn wait_many(items: &mut [WaitItem], deadline: u64) -> Result<usize, Error> {
    call(nr::OBJECT_WAIT_MANY, [items.as_mut_ptr() as usize, items.len(), deadline as usize, 0, 0, 0])
}

/// Reads the kernel log starting at `offset`; returns bytes read and the next
/// offset.
pub fn log_read(offset: u64, buf: &mut [u8]) -> Result<(usize, u64), Error> {
    call2(nr::LOG_READ, [offset as usize, buf.as_mut_ptr() as usize, buf.len(), 0, 0, 0])
        .map(|(n, next)| (n, next as u64))
}

/// Information about the calling process.
pub fn process_self_info() -> Result<ProcessInfo, Error> {
    let mut info = ProcessInfo::default();
    let len = core::mem::size_of_val(&info);
    call(
        nr::OBJECT_INFO,
        [vabi::INVALID_HANDLE as usize, info_topic::PROCESS, &mut info as *mut _ as usize, len, 0, 0],
    )?;
    Ok(info)
}

/// Lists processes (up to `out.len()`); returns the total number.
pub fn process_list(out: &mut [ProcessInfo]) -> Result<usize, Error> {
    call(nr::PROCESS_LIST, [out.as_mut_ptr() as usize, out.len(), 0, 0, 0, 0])
}

pub fn system_info() -> Result<vabi::SystemInfo, Error> {
    let mut info = vabi::SystemInfo::default();
    call(nr::SYSTEM_INFO, [&mut info as *mut _ as usize, 0, 0, 0, 0, 0])?;
    Ok(info)
}

pub fn power(resource: &Resource, action: usize) -> Result<(), Error> {
    call(nr::SYSTEM_POWER, [resource.raw() as usize, action, 0, 0, 0, 0]).map(|_| ())
}

/// The most bytes one `random` system call gives.
const RANDOM_CHUNK: usize = 4096;

/// Fills `buf` with output of the kernel's cryptographically secure
/// generator. A shortage of kernel memory is waited out briefly (a few
/// tries, a millisecond apart); then, or for any other error, the error is
/// returned and `buf` must not be used.
pub fn try_random_bytes(buf: &mut [u8]) -> Result<(), Error> {
    const TRIES: u32 = 5;
    for chunk in buf.chunks_mut(RANDOM_CHUNK) {
        let mut tries = 0;
        loop {
            match call(nr::RANDOM, [chunk.as_mut_ptr() as usize, chunk.len(), 0, 0, 0, 0]) {
                Ok(_) => break,
                Err(Error::NoMemory) if tries + 1 < TRIES => {
                    tries += 1;
                    crate::time::sleep(crate::time::Duration::from_millis(1));
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

/// Fills `buf` with output of the kernel's cryptographically secure
/// generator, or ends the program if there is none to be had
/// ([`try_random_bytes`] says why): it never returns with bytes that are
/// not random, which keys, nonces and address-space layouts made from them
/// would not survive.
pub fn random_bytes(buf: &mut [u8]) {
    if let Err(e) = try_random_bytes(buf) {
        crate::io::log_without_allocating(format_args!("cannot get random bytes ({e:?}): ending the program\n"));
        crate::sys::process_exit(vabi::EXIT_CODE_PANICKED);
    }
}

pub fn process_open(resource: &Resource, koid: u64) -> Result<Process, Error> {
    call(nr::PROCESS_OPEN, [resource.raw() as usize, koid as usize, 0, 0, 0, 0])
        .map(|h| Process(Handle(h as RawHandle)))
}

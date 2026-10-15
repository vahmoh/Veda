// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The bridge, the monitor's side: the guest's handles, and the operations
//! on them (`vhv::bridge` describes them).
//!
//! The guest's handles are numbers that this table gives meaning to: each
//! stands for a handle of this process that the guest was given or made.
//! Operations are the system calls of their names, carried out on those
//! handles; handles that messages carry move between the table and Veda's
//! channels. What the guest makes Veda keep is bounded: its handles, and
//! its budget (`drivervm_core::ledger`) of Veda's memory (the VMOs it makes,
//! which commit their pages as they are used, the messages it sends itself)
//! and of mappings in its window (`drivervm_core::window`).
//!
//! A wait that cannot finish at once goes on in the waiter thread
//! (`run_waits`), which waits on all of them at once (`drivervm_core::waits`),
//! and when one finishes writes what it observed into the guest's request,
//! reports it in the notification ring and raises the guest's interrupt.
//! Pending waits name the guest's handles as the guest does, and are
//! resolved against the table at each round: closing a handle a wait names
//! ends the wait (`BadHandle`), and the round, as cancelling a wait does,
//! so that what the waiter waited on goes at once if that was its last
//! handle (a channel's peer sees it closed).

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::mem::ManuallyDrop;

use drivervm_core::ledger::Ledger;
use drivervm_core::waits::{self, Pending, Round, Wait};
use drivervm_core::window::Window;
use vabi::{Error, RawHandle, Rights, WaitItem, map_flags, signals, vmo_flags};
use vhv::bridge::{self as b, op};
use vrt::object::{Channel, Event, Guest, Handle, Vcpu, Vmo};
use vrt::println;
use vrt::sync::Mutex;

use crate::memory::{GuestMemory, Plain};
use crate::registry::Registry;

/// The most handles a guest holds at a time (those in messages it sent
/// itself among them).
const MAX_HANDLES: usize = 16384;
/// The most bytes a VMO the guest makes may have.
const MAX_VMO: u64 = 1 << 30;
/// The flags a VMO the guest makes may have: write-combining (a display's
/// pictures). Its pages are committed as they are used, within its budget.
const VMO_FLAGS: u32 = vmo_flags::WRITE_COMBINING as u32;
/// The most mappings in the guest's window.
const MAX_MAPPINGS: u32 = 4096;
/// The guest's budget of Veda's memory is a quarter of the PC's, and at
/// least this (a display's pictures and a few contexts of the renderer's,
/// counted as made and as mapped, on a small machine).
const MIN_BUDGET: u64 = 512 << 20;
/// VMOs the guest maps go in this range of its physical address space,
/// beyond its RAM.
pub const WINDOW_BASE: u64 = 64 << 30;
pub const WINDOW_SIZE: u64 = 64 << 30;
const PAGE: u64 = 4096;

/// The bridge: the guest's handles and its books, its waits, its window
/// and its registry.
pub struct Bridge {
    table: Mutex<Table>,
    waits: Mutex<Waits>,
    /// Signalled when the waits change, for the waiter thread.
    wake: Event,
    windows: Mutex<Window>,
    registry: Registry,
}

/// A handle of the guest's: this process's, and the kernel's id of its
/// object where the ledger may know it (0 elsewhere).
struct Entry {
    handle: Handle,
    koid: u64,
}

/// The guest's handles, by the numbers it knows them by, and its books.
struct Table {
    next: u32,
    handles: BTreeMap<u32, Entry>,
    ledger: Ledger,
}

impl Table {
    /// Whether `n` more handles fit (those in messages the guest sent
    /// itself are its too).
    fn room(&self, n: usize) -> Result<(), Error> {
        if self.handles.len() + self.ledger.queued_handles() as usize + n > MAX_HANDLES {
            return Err(Error::LimitReached);
        }
        Ok(())
    }

    fn insert(&mut self, handle: Handle, koid: u64) -> Result<u32, Error> {
        self.room(1)?;
        loop {
            self.next = self.next.wrapping_add(1).max(1);
            if !self.handles.contains_key(&self.next) {
                self.handles.insert(self.next, Entry { handle, koid });
                return Ok(self.next);
            }
        }
    }

    fn entry(&self, id: u32) -> Result<&Entry, Error> {
        self.handles.get(&id).ok_or(Error::BadHandle)
    }

    fn raw(&self, id: u32) -> Result<RawHandle, Error> {
        self.entry(id).map(|e| e.handle.raw())
    }

    /// Takes handle `id` out of the guest's table, closed: the ledger is
    /// told, and the handle is the caller's to drop.
    fn close(&mut self, id: u32) -> Result<Handle, Error> {
        let e = self.handles.remove(&id).ok_or(Error::BadHandle)?;
        self.ledger.closed(e.koid);
        Ok(e.handle)
    }

    /// `items` with this process's handles for the guest's, nothing
    /// observed yet.
    fn resolve(&self, items: &[WaitItem]) -> Result<Vec<WaitItem>, Error> {
        items.iter().map(|i| Ok(WaitItem { handle: self.raw(i.handle)?, observed: 0, ..*i })).collect()
    }
}

/// The waits that go on, those finished, and the ring they are reported in.
struct Waits {
    pending: Pending,
    /// Finished waits the ring has had no room for yet.
    done: VecDeque<b::Completion>,
    /// The ring (guest-physical) and the interrupt that announces it.
    ring: Option<(u64, u8)>,
    head: u32,
}

/// A handle of the table, seen as an object of type `T` (not closed when
/// the view goes).
fn view<T>(raw: RawHandle, wrap: fn(Handle) -> T) -> ManuallyDrop<T> {
    // SAFETY: the table owns the handle; the view never closes it.
    ManuallyDrop::new(wrap(unsafe { Handle::from_raw(raw) }))
}

/// Reads Veda's log for a program of the guest ([`op::LOG_READ`]).
fn log_read(memory: &GuestMemory, r: &mut b::LogRead) -> Result<(), Error> {
    if r.len > b::MAX_COPY {
        return Err(Error::InvalidArgs);
    }
    if !memory.contains(r.buffer, r.len) {
        return Err(Error::Fault);
    }
    let mut data = alloc::vec![0u8; r.len as usize];
    let (n, next) = vrt::object::log_read(r.offset, &mut data)?;
    memory.write(r.buffer, &data[..n]);
    (r.len, r.next) = (n as u64, next);
    Ok(())
}

fn status(r: Result<(), Error>) -> u32 {
    match r {
        Ok(()) => b::OK,
        Err(e) => e as u32,
    }
}

/// The guest's budget of Veda's memory: a quarter of the PC's, at least
/// [`MIN_BUDGET`].
fn budget() -> u64 {
    let total = vrt::object::system_info().map(|i| i.total_memory).unwrap_or(0);
    (total / 4).max(MIN_BUDGET)
}

impl Bridge {
    pub fn new() -> Result<Bridge, Error> {
        let budget = budget();
        println!("the guest may keep {} MiB of Veda's memory through the bridge", budget >> 20);
        Ok(Bridge {
            table: Mutex::new(Table { next: 0, handles: BTreeMap::new(), ledger: Ledger::new(budget, MAX_MAPPINGS) }),
            waits: Mutex::new(Waits { pending: Pending::default(), done: VecDeque::new(), ring: None, head: 0 }),
            wake: Event::create()?,
            windows: Mutex::new(Window::new(WINDOW_BASE..WINDOW_BASE + WINDOW_SIZE)),
            registry: Registry::new()?,
        })
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Carries out operation `op` with its request at `gpa`: 0, or the
    /// platform's error if the request is not in the guest's memory.
    pub fn call(&self, memory: &GuestMemory, guest: &Guest, op: u32, gpa: u64) -> u64 {
        fn request<T: Plain>(memory: &GuestMemory, gpa: u64, f: impl FnOnce(&mut T)) -> u64 {
            let Some(mut r) = memory.read_obj::<T>(gpa) else { return vhv::platform::error::INVALID };
            f(&mut r);
            if memory.write_obj(gpa, &r) { 0 } else { vhv::platform::error::INVALID }
        }
        match op {
            op::SETUP => request(memory, gpa, |r: &mut b::Setup| r.status = status(self.setup(memory, r))),
            op::CLOSE => request(memory, gpa, |r: &mut b::Close| r.status = status(self.close(r.handle))),
            op::DUPLICATE => request(memory, gpa, |r: &mut b::Duplicate| r.status = status(self.duplicate(r))),
            op::OBJECT_INFO => request(memory, gpa, |r: &mut b::ObjectInfo| r.status = status(self.object_info(r))),
            op::SIGNAL => request(memory, gpa, |r: &mut b::Signal| {
                let table = self.table.lock();
                r.status = status(table.raw(r.handle).and_then(|raw| view(raw, |h| h).signal(r.clear, r.set)));
            }),
            op::WAIT => request(memory, gpa, |r: &mut b::Wait| {
                if let Err(e) = self.wait(memory, r) {
                    r.status = e as u32;
                }
            }),
            op::CANCEL => request(memory, gpa, |r: &mut b::Cancel| r.status = status(self.cancel(r.key))),
            op::CHANNEL_CREATE => request(memory, gpa, |r: &mut b::ChannelCreate| {
                r.status = status(self.channel_create(r));
            }),
            op::CHANNEL_WRITE => {
                request(memory, gpa, |r: &mut b::ChannelWrite| r.status = status(self.channel_write(memory, r)))
            }
            op::CHANNEL_READ => {
                request(memory, gpa, |r: &mut b::ChannelRead| r.status = status(self.channel_read(memory, r)))
            }
            op::EVENT_CREATE => request(memory, gpa, |r: &mut b::EventCreate| {
                r.status = status(Event::create().and_then(|e| self.insert(e.into_handle())).map(|id| r.out = id));
            }),
            op::VMO_CREATE => request(memory, gpa, |r: &mut b::Vmo| r.status = status(self.vmo_create(r))),
            op::VMO_SIZE => request(memory, gpa, |r: &mut b::Vmo| {
                let table = self.table.lock();
                let size = table.raw(r.handle).and_then(|raw| view(raw, Vmo::from_handle).size());
                r.status = status(size.map(|s| r.size = s as u64));
            }),
            op::VMO_READ | op::VMO_WRITE => request(memory, gpa, |r: &mut b::VmoCopy| {
                r.status = status(self.vmo_copy(memory, r, op == op::VMO_WRITE));
            }),
            op::VMO_MAP => request(memory, gpa, |r: &mut b::VmoMap| r.status = status(self.vmo_map(guest, r))),
            op::VMO_UNMAP => request(memory, gpa, |r: &mut b::VmoMap| r.status = status(self.vmo_unmap(guest, r))),
            op::BOOTSTRAP => request(memory, gpa, |r: &mut b::Bootstrap| r.status = status(self.bootstrap(r))),
            op::CLOCK => request(memory, gpa, |r: &mut b::Clock| {
                r.status = status(vrt::time::clock_info().map(|info| r.info = info));
            }),
            op::LOG_READ => request(memory, gpa, |r: &mut b::LogRead| r.status = status(log_read(memory, r))),
            _ => vhv::platform::error::UNKNOWN,
        }
    }

    /// A handle the ledger does not know into the guest's table.
    fn insert(&self, h: Handle) -> Result<u32, Error> {
        self.table.lock().insert(h, 0)
    }

    fn setup(&self, memory: &GuestMemory, r: &b::Setup) -> Result<(), Error> {
        if !r.ring.is_multiple_of(PAGE) || !memory.contains(r.ring, PAGE) || !crate::arch::is_interrupt_vector(r.vector)
        {
            return Err(Error::InvalidArgs);
        }
        let mut waits = self.waits.lock();
        waits.ring = Some((r.ring, r.vector as u8));
        waits.head = 0;
        drop(waits);
        // Waits that finished before there was a ring go into it.
        let _ = self.wake.signal();
        Ok(())
    }

    /// Closes the guest's handle `id`: a wait that names it ends.
    fn close(&self, id: u32) -> Result<(), Error> {
        let handle = self.table.lock().close(id)?;
        drop(handle);
        if self.waits.lock().pending.names(id) {
            let _ = self.wake.signal();
        }
        Ok(())
    }

    fn duplicate(&self, r: &mut b::Duplicate) -> Result<(), Error> {
        let mut table = self.table.lock();
        let (raw, koid) = table.entry(r.handle).map(|e| (e.handle.raw(), e.koid))?;
        table.room(1)?;
        let rights = (r.rights != 0).then_some(Rights(r.rights));
        let h = view(raw, |h| h).duplicate(rights)?;
        r.out = table.insert(h, koid)?;
        table.ledger.duplicated(koid);
        Ok(())
    }

    fn object_info(&self, r: &mut b::ObjectInfo) -> Result<(), Error> {
        let table = self.table.lock();
        let info = view(table.raw(r.handle)?, |h| h).basic_info()?;
        r.koid = info.koid;
        r.object_type = info.object_type;
        r.rights = info.rights;
        Ok(())
    }

    fn channel_create(&self, r: &mut b::ChannelCreate) -> Result<(), Error> {
        let mut table = self.table.lock();
        table.room(2)?;
        let (a, c) = Channel::create()?;
        let (ka, kc) = (a.0.basic_info()?.koid, c.0.basic_info()?.koid);
        let first = table.insert(a.into_handle(), ka)?;
        match table.insert(c.into_handle(), kc) {
            Ok(second) => {
                table.ledger.made_channel(ka, kc);
                r.out = [first, second];
                Ok(())
            }
            Err(e) => {
                table.handles.remove(&first);
                Err(e)
            }
        }
    }

    fn channel_write(&self, memory: &GuestMemory, r: &mut b::ChannelWrite) -> Result<(), Error> {
        if r.bytes_len > b::MAX_BYTES || r.handles_len > b::MAX_HANDLES {
            return Err(Error::InvalidArgs);
        }
        let mut bytes = alloc::vec![0u8; r.bytes_len as usize];
        let mut ids = alloc::vec![0u8; r.handles_len as usize * 4];
        if !memory.read(r.bytes, &mut bytes) || !memory.read(r.handles, &mut ids) {
            return Err(Error::Fault);
        }
        let ids: Vec<u32> = ids.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect();
        let mut table = self.table.lock();
        let (channel, channel_koid) = table.entry(r.handle).map(|e| (e.handle.raw(), e.koid))?;
        // Every handle checked before any moves, as the kernel does.
        for (i, id) in ids.iter().enumerate() {
            if *id == r.handle || ids[..i].contains(id) {
                return Err(Error::InvalidArgs);
            }
            table.entry(*id)?;
        }
        // A message to the guest itself is charged; nothing moves if it
        // does not fit its budget.
        table.ledger.may_send(channel_koid, r.bytes_len)?;
        let moved: Vec<Entry> = ids.iter().filter_map(|id| table.handles.remove(id)).collect();
        let koids: Vec<u64> = moved.iter().map(|e| e.koid).collect();
        let handles: Vec<Handle> = moved.into_iter().map(|e| e.handle).collect();
        r.consumed = 1;
        let written = view(channel, Channel::from_handle).write(&bytes, handles);
        table.ledger.sent(channel_koid, r.bytes_len, &koids, written.is_ok());
        drop(table);
        // A wait that names a handle that went is over, as if it was closed.
        if self.waits.lock().pending.names_any(&ids) {
            let _ = self.wake.signal();
        }
        written
    }

    fn channel_read(&self, memory: &GuestMemory, r: &mut b::ChannelRead) -> Result<(), Error> {
        let (cap_bytes, cap_handles) = (r.bytes_capacity.min(b::MAX_BYTES), r.handles_capacity.min(b::MAX_HANDLES));
        // The guest's buffers first: a message read is never lost to them.
        if !memory.contains(r.bytes, cap_bytes as u64) || !memory.contains(r.handles, cap_handles as u64 * 4) {
            return Err(Error::Fault);
        }
        let mut bytes = alloc::vec![0u8; cap_bytes as usize];
        let mut raws = alloc::vec![0 as RawHandle; cap_handles as usize];
        let mut table = self.table.lock();
        let (raw, channel_koid) = table.entry(r.handle).map(|e| (e.handle.raw(), e.koid))?;
        match view(raw, Channel::from_handle).read_into(&mut bytes, &mut raws) {
            Ok((n, h)) => {
                // A message the guest sent itself: its handles were the
                // guest's already, and are again.
                let known = table.ledger.read(channel_koid, n as u32, h as u32);
                // The objects of the ledger's a message may bring back are
                // known by their ids.
                let look = table.ledger.knows_any();
                let mut ids = Vec::with_capacity(h);
                for &raw in &raws[..h] {
                    // SAFETY: the kernel gave us this handle.
                    let handle = unsafe { Handle::from_raw(raw) };
                    let koid = if look { handle.basic_info().map_or(0, |i| i.koid) } else { 0 };
                    table.ledger.received(koid);
                    // A handle that does not fit the table is dropped.
                    ids.push(table.insert(handle, koid).unwrap_or_else(|_| {
                        table.ledger.closed(koid);
                        0
                    }));
                }
                table.ledger.arrived(known);
                drop(table);
                let id_bytes: Vec<u8> = ids.iter().flat_map(|id| id.to_le_bytes()).collect();
                memory.write(r.bytes, &bytes[..n]);
                memory.write(r.handles, &id_bytes);
                r.bytes_len = n as u32;
                r.handles_len = h as u32;
                Ok(())
            }
            Err((e, n, h)) => {
                r.bytes_len = n as u32;
                r.handles_len = h as u32;
                Err(e)
            }
        }
    }

    /// A VMO for the guest, within its budget: write-combining if it asks,
    /// committed as it is used (never at once).
    fn vmo_create(&self, r: &mut b::Vmo) -> Result<(), Error> {
        if r.size == 0 || r.size > MAX_VMO || r.flags & !VMO_FLAGS != 0 {
            return Err(Error::InvalidArgs);
        }
        let size = r.size.next_multiple_of(PAGE);
        let mut table = self.table.lock();
        table.room(1)?;
        if !table.ledger.fits(size) {
            return Err(Error::LimitReached);
        }
        let vmo = Vmo::create_with(size as usize, r.flags as usize)?;
        let koid = vmo.0.basic_info()?.koid;
        table.ledger.made_vmo(koid, size)?;
        r.handle = table.insert(vmo.into_handle(), koid).inspect_err(|_| table.ledger.closed(koid))?;
        Ok(())
    }

    fn vmo_copy(&self, memory: &GuestMemory, r: &b::VmoCopy, write: bool) -> Result<(), Error> {
        if r.len > b::MAX_COPY {
            return Err(Error::InvalidArgs);
        }
        let mut data = alloc::vec![0u8; r.len as usize];
        let table = self.table.lock();
        let vmo = view(table.raw(r.handle)?, Vmo::from_handle);
        if write {
            if !memory.read(r.buffer, &mut data) {
                return Err(Error::Fault);
            }
            vmo.write(r.offset as usize, &data)
        } else {
            if !memory.contains(r.buffer, r.len) {
                return Err(Error::Fault);
            }
            vmo.read(r.offset as usize, &mut data)?;
            memory.write(r.buffer, &data);
            Ok(())
        }
    }

    /// Maps a VMO into the window, within the guest's budget.
    fn vmo_map(&self, guest: &Guest, r: &mut b::VmoMap) -> Result<(), Error> {
        if r.len == 0 || !r.len.is_multiple_of(PAGE) || !r.offset.is_multiple_of(PAGE) {
            return Err(Error::InvalidArgs);
        }
        let flags = (r.flags as usize & map_flags::WRITE) | map_flags::READ;
        let mut table = self.table.lock();
        let vmo = view(table.raw(r.handle)?, Vmo::from_handle);
        table.ledger.map(r.len)?;
        let Some(gpa) = self.windows.lock().take(r.len) else {
            table.ledger.unmapped(r.len);
            return Err(Error::NoMemory);
        };
        if let Err(e) = guest.map(&vmo, r.offset as usize, r.len as usize, gpa, flags) {
            let mut windows = self.windows.lock();
            windows.unuse(gpa, r.len);
            windows.free(gpa, r.len);
            table.ledger.unmapped(r.len);
            return Err(e);
        }
        r.gpa = gpa;
        Ok(())
    }

    /// Undoes a mapping of the window, of exactly its range: no other
    /// mapping takes the range, and no other unmapping finds it, until it
    /// is unmapped (or, if it could not be, it is the mapping's again).
    fn vmo_unmap(&self, guest: &Guest, r: &b::VmoMap) -> Result<(), Error> {
        if !self.windows.lock().unuse(r.gpa, r.len) {
            return Err(Error::NotFound);
        }
        match guest.unmap(r.gpa, r.len as usize) {
            Ok(()) => {
                self.windows.lock().free(r.gpa, r.len);
                self.table.lock().ledger.unmapped(r.len);
                Ok(())
            }
            Err(e) => {
                self.windows.lock().reuse(r.gpa, r.len);
                Err(e)
            }
        }
    }

    fn bootstrap(&self, r: &mut b::Bootstrap) -> Result<(), Error> {
        if r.role != vabi::startup::role::REGISTRY {
            return Err(Error::NotFound);
        }
        r.out = self.insert(self.registry.connect()?.into_handle())?;
        Ok(())
    }

    /// Gives up a pending wait: the waiter waits on it no more.
    fn cancel(&self, key: u64) -> Result<(), Error> {
        if !self.waits.lock().pending.cancel(key) {
            return Err(Error::NotFound);
        }
        let _ = self.wake.signal();
        Ok(())
    }

    /// A wait: finished at once if it can be, else it goes on in the
    /// waiter thread ([`b::PENDING`]).
    fn wait(&self, memory: &GuestMemory, r: &mut b::Wait) -> Result<(), Error> {
        if r.count == 0 || r.count > b::MAX_WAIT_ITEMS {
            return Err(Error::InvalidArgs);
        }
        let size = core::mem::size_of::<WaitItem>() as u64;
        // The items are the guest's memory (so their addresses below are).
        if !memory.contains(r.items, r.count as u64 * size) {
            return Err(Error::Fault);
        }
        let mut items: Vec<WaitItem> = Vec::with_capacity(r.count as usize);
        for i in 0..r.count as u64 {
            items.push(memory.read_obj::<WaitItem>(r.items + i * size).ok_or(Error::Fault)?);
        }
        let mut resolved = self.table.lock().resolve(&items)?;
        let polled = vrt::object::wait_many(&mut resolved, 0);
        let now = vrt::time::now_ns();
        match polled {
            Ok(n) if n > 0 => return finish(memory, r, &resolved, n as u32, b::OK),
            Ok(_) | Err(Error::TimedOut) if r.deadline <= now => {
                return finish(memory, r, &resolved, 0, Error::TimedOut as u32);
            }
            Ok(_) | Err(Error::TimedOut) => {}
            Err(e) => return Err(e),
        }
        let mut waits = self.waits.lock();
        let finished = waits.done.len();
        waits.pending.admit(Wait::new(r.key, r.items, items, r.deadline), finished)?;
        drop(waits);
        r.status = b::PENDING;
        let _ = self.wake.signal();
        Ok(())
    }

    /// The waiter thread: waits on every pending wait at once (and on the
    /// event that says they changed), finishes those whose signals came,
    /// whose deadline passed or whose handles went, and reports them.
    pub fn run_waits(&self, memory: &GuestMemory, notify: &Vcpu) -> ! {
        // Whether finished waits wait for room in the ring.
        let mut unposted = false;
        let mut said = false;
        loop {
            // The waits as they are now (never with the table's lock held).
            let snapshot: Vec<Wait> = self.waits.lock().pending.waits().to_vec();
            let mut items =
                alloc::vec![WaitItem { handle: self.wake.raw(), signals: signals::SIGNALED, ..WaitItem::default() }];
            // Each wait's items among them, by the wait's place in the
            // snapshot; and the waits that cannot be waited on.
            let mut spans = Vec::with_capacity(snapshot.len());
            let mut failed: Vec<Option<Error>> = alloc::vec![None; snapshot.len()];
            // Finished waits the ring had no room for are posted again
            // soon, as the guest takes others from it.
            let mut deadline = if unposted { vrt::time::now_ns().saturating_add(1_000_000) } else { u64::MAX };
            {
                let table = self.table.lock();
                for (i, w) in snapshot.iter().enumerate() {
                    match table.resolve(&w.items) {
                        Ok(resolved) => {
                            spans.push(items.len()..items.len() + resolved.len());
                            items.extend(resolved);
                            deadline = deadline.min(w.deadline);
                        }
                        Err(e) => {
                            spans.push(0..0);
                            failed[i] = Some(e);
                        }
                    }
                }
            }
            // (Admitted, the items fit one wait: `waits::MAX_ITEMS`.)
            let waited = if failed.iter().all(Option::is_none) {
                vrt::object::wait_many(&mut items, deadline)
            } else {
                // A handle closed meanwhile: the waits it ends end now.
                Ok(0)
            };
            let _ = self.wake.clear();
            match waited {
                Ok(_) | Err(Error::TimedOut) => {}
                // A handle closed after it was resolved: the next round
                // finds it gone.
                Err(Error::BadHandle) => {}
                // A wait names what cannot be waited on, or the kernel
                // could not wait: each wait is polled alone.
                Err(e) => {
                    let mut any = false;
                    for i in 0..spans.len() {
                        if failed[i].is_some() {
                            continue;
                        }
                        match vrt::object::wait_many(&mut items[spans[i].clone()], 0) {
                            Ok(_) | Err(Error::TimedOut) => {}
                            Err(e) => {
                                failed[i] = Some(e);
                                any = true;
                            }
                        }
                    }
                    if !any {
                        if !said {
                            println!("the bridge's waiter cannot wait ({e}); it tries again");
                            said = true;
                        }
                        vrt::time::sleep(vrt::time::Duration::from_millis(10));
                    }
                }
            }
            let now = vrt::time::now_ns();
            let mut waits = self.waits.lock();
            for mut w in waits.pending.take() {
                // Its place in the snapshot, if it was in it.
                let at = snapshot.iter().position(|s| s.id() == w.id());
                let round = match at {
                    None => Round::Missed,
                    Some(i) => failed[i].map_or(Round::Waited, Round::Failed),
                };
                let observed = at.map_or(&[][..], |i| &items[spans[i].clone()]);
                match w.outcome(round, observed, now) {
                    None => waits.pending.restore(w),
                    Some(done) => {
                        if round == Round::Waited {
                            for (i, item) in w.items.iter().enumerate() {
                                // Only the observed signals go back; the
                                // guest's handle numbers stay as it wrote
                                // them.
                                if let Some(at) = waits::observed_at(w.items_gpa, i) {
                                    memory.write_obj(at, &item.observed);
                                }
                            }
                        }
                        waits.done.push_back(done);
                    }
                }
            }
            if post(&mut waits, memory)
                && let Some((_, vector)) = waits.ring
            {
                let _ = notify.interrupt(vector);
            }
            unposted = waits.ring.is_some() && !waits.done.is_empty();
        }
    }
}

/// Moves finished waits into the ring as far as it has room: the entries
/// first, then the head that says they are there, released after them;
/// whether any moved.
fn post(waits: &mut Waits, memory: &GuestMemory) -> bool {
    let Some((ring, _)) = waits.ring else { return false };
    let Some(tail) = memory.load_acquire(ring + b::ring::TAIL as u64) else { return false };
    let mut head = waits.head;
    while head.wrapping_sub(tail) < b::ring::CAPACITY
        && let Some(c) = waits.done.pop_front()
    {
        let slot = (head % b::ring::CAPACITY) as u64;
        memory.write_obj(ring + b::ring::ENTRIES as u64 + slot * b::ring::ENTRY_SIZE as u64, &c);
        head = head.wrapping_add(1);
    }
    if head == waits.head {
        return false;
    }
    waits.head = head;
    memory.store_release(ring + b::ring::HEAD as u64, head)
}

/// Finishes a wait at once: what was observed goes back into the guest's
/// items.
fn finish(memory: &GuestMemory, r: &mut b::Wait, items: &[WaitItem], satisfied: u32, st: u32) -> Result<(), Error> {
    for (i, item) in items.iter().enumerate() {
        let at = waits::observed_at(r.items, i).ok_or(Error::Fault)?;
        if !memory.write_obj(at, &item.observed) {
            return Err(Error::Fault);
        }
    }
    r.satisfied = satisfied;
    r.status = st;
    Ok(())
}

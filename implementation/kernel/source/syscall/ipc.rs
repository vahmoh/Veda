// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Handles, waiting, channels, sockets and events.

use alloc::vec::Vec;

use abi::signals::{self, SIGNALED, USER_ALL};
use abi::{Error, HandleBasicInfo, PortPacket, RawHandle, Rights, WaitItem, channel_flags, info_topic};

use super::{SysResult, current_process, deadline, get_channel, get_port, get_socket, handle, insert, ok};
use crate::capability::handle::Handle;
use crate::capability::{KObject, new_koid};
use crate::ipc::channel::{self, Message};
use crate::ipc::event::Event;
use crate::ipc::port::{Observer, Port};
use crate::ipc::socket;
use crate::memory::user;
use crate::scheduler::{self, Interruptible, WakeReason};

const CHANNEL_RIGHTS: Rights = Rights(
    Rights::TRANSFER.0 | Rights::READ.0 | Rights::WRITE.0 | Rights::WAIT.0 | Rights::SIGNAL.0 | Rights::GET_INFO.0,
);
/// Socket endpoints may be duplicated (shared by several processes, as a
/// pipe or terminal is); a channel's only when it was made shared
/// ([`channel_flags::SHARED`]).
const SOCKET_RIGHTS: Rights = Rights(CHANNEL_RIGHTS.0 | Rights::DUPLICATE.0);
const SHARED_CHANNEL_RIGHTS: Rights = SOCKET_RIGHTS;

pub fn handle_close(raw: RawHandle) -> SysResult {
    let h = current_process()?.handles.lock().remove(raw)?;
    drop(h);
    ok(0)
}

fn reduce(rights: Rights, requested: u32) -> Result<Rights, Error> {
    if requested == u32::MAX {
        return Ok(rights);
    }
    let r = Rights(requested);
    if !rights.contains(r) {
        return Err(Error::AccessDenied);
    }
    Ok(r)
}

pub fn handle_duplicate(raw: RawHandle, rights: u32) -> SysResult {
    let h = handle(raw, Rights::DUPLICATE)?;
    let rights = reduce(h.rights, rights)?;
    ok(insert(h.object, rights)? as usize)
}

pub fn handle_replace(raw: RawHandle, rights: u32) -> SysResult {
    let p = current_process()?;
    let mut table = p.handles.lock();
    let h = table.get(raw)?.clone();
    let rights = reduce(h.rights, rights)?;
    table.remove(raw)?;
    ok(table.insert(Handle { object: h.object, rights })? as usize)
}

pub fn object_info(raw: RawHandle, topic: usize, buf: usize, len: usize) -> SysResult {
    // The invalid handle names the calling process (it may always learn
    // about itself).
    let h = if raw == abi::INVALID_HANDLE {
        if !matches!(topic, info_topic::HANDLE_BASIC | info_topic::PROCESS) {
            return Err(Error::InvalidArgs);
        }
        Handle { object: KObject::Process(current_process()?), rights: Rights::GET_INFO }
    } else {
        handle(raw, Rights::NONE)?
    };
    let write = |bytes: &[u8]| -> SysResult {
        if len < bytes.len() {
            return Err(Error::BufferTooSmall);
        }
        user::copy_to_user(buf as u64, bytes)?;
        ok(bytes.len())
    };
    fn as_bytes<T>(v: &T) -> &[u8] {
        // SAFETY: plain #[repr(C)] ABI structures.
        unsafe { core::slice::from_raw_parts(v as *const T as *const u8, core::mem::size_of::<T>()) }
    }
    match topic {
        info_topic::HANDLE_BASIC => {
            let info = HandleBasicInfo {
                koid: h.object.koid(),
                object_type: h.object.object_type() as u32,
                rights: h.rights.0,
            };
            write(as_bytes(&info))
        }
        info_topic::PROCESS => match &h.object {
            KObject::Process(p) => write(as_bytes(&p.info())),
            _ => Err(Error::WrongType),
        },
        info_topic::VMO => match &h.object {
            KObject::Vmo(v) => {
                let info =
                    abi::VmoInfo { size: v.size(), committed_bytes: v.committed_bytes(), flags: 0, _reserved: 0 };
                write(as_bytes(&info))
            }
            _ => Err(Error::WrongType),
        },
        info_topic::THREAD => match &h.object {
            KObject::Thread(t) => {
                let (state, priority) = {
                    let s = t.sched.lock();
                    (s.state, s.priority)
                };
                // Not under the thread's lock, which it takes.
                let cpu_time_ns = scheduler::cpu_time_ns(t);
                let info =
                    abi::ThreadInfo { koid: t.koid, state: state as u32, priority: priority as u32, cpu_time_ns };
                write(as_bytes(&info))
            }
            _ => Err(Error::WrongType),
        },
        info_topic::SOCKET => match &h.object {
            KObject::Socket(s) => {
                let info = abi::SocketInfo { readable: s.readable_bytes() as u64, writable: s.writable_bytes() as u64 };
                write(as_bytes(&info))
            }
            _ => Err(Error::WrongType),
        },
        _ => Err(Error::InvalidArgs),
    }
}

pub fn object_signal(raw: RawHandle, clear: u32, set: u32) -> SysResult {
    let h = handle(raw, Rights::SIGNAL)?;
    let allowed = match h.object {
        KObject::Event(_) => USER_ALL | SIGNALED,
        KObject::Channel(_) | KObject::Socket(_) => USER_ALL,
        _ => return Err(Error::NotSupported),
    };
    if (clear | set) & !allowed != 0 {
        return Err(Error::InvalidArgs);
    }
    h.object.signals().ok_or(Error::NotSupported)?.update(clear, set);
    ok(0)
}

/// Waits until `object` has one of `signals`, or `d` (an ABI deadline)
/// passes: for calls that wait for an object themselves, which an
/// interruption does not end.
pub(super) fn wait_for(object: &KObject, signals: u32, d: u64) -> Result<(), Error> {
    wait_objects(core::slice::from_ref(object), &[signals], &mut [0], d, Interruptible::No).map(|_| ())
}

/// Shared implementation of the wait calls. Returns the number of satisfied
/// items; `observed` is filled for every item. An interruption ends the
/// wait (`Interrupted`) as `how` says.
fn wait_objects(
    objects: &[KObject],
    masks: &[u32],
    observed: &mut [u32],
    d: u64,
    how: Interruptible,
) -> Result<usize, Error> {
    let cur = scheduler::current();
    let wait_id = new_koid();
    loop {
        let mut satisfied = 0;
        for (i, o) in objects.iter().enumerate() {
            observed[i] = o.signals().map(|s| s.active()).unwrap_or(0);
            if observed[i] & masks[i] != 0 {
                satisfied += 1;
            }
        }
        if satisfied > 0 {
            return Ok(satisfied);
        }
        if d == 0 {
            return Err(Error::TimedOut);
        }
        for (i, o) in objects.iter().enumerate() {
            if let Some(s) = o.signals() {
                s.add_waiter(wait_id, cur.clone(), masks[i]);
            }
        }
        let reason = scheduler::block_interruptible(deadline(d), how);
        for o in objects {
            if let Some(s) = o.signals() {
                s.remove_waiter(wait_id);
            }
        }
        match reason {
            WakeReason::Killed => return Err(Error::Canceled),
            WakeReason::Interrupted => return Err(Error::Interrupted),
            WakeReason::TimedOut => {
                // Report the final state, then time out.
                for (i, o) in objects.iter().enumerate() {
                    observed[i] = o.signals().map(|s| s.active()).unwrap_or(0);
                }
                return if objects.iter().zip(masks).zip(observed.iter()).any(|((_, m), o)| o & m != 0) {
                    Ok(1)
                } else {
                    Err(Error::TimedOut)
                };
            }
            _ => {}
        }
    }
}

pub fn wait_one(raw: RawHandle, signals: u32, d: u64, flags: usize) -> SysResult {
    let how = Interruptible::from_flags(flags, 0)?;
    let h = handle(raw, Rights::WAIT)?;
    if h.object.signals().is_none() {
        return Err(Error::NotSupported);
    }
    let mut observed = [0u32];
    match wait_objects(core::slice::from_ref(&h.object), &[signals], &mut observed, d, how) {
        Ok(_) => ok(observed[0] as usize),
        Err(e) => Err(e),
    }
}

pub fn wait_many(items_ptr: usize, count: usize, d: u64, flags: usize) -> SysResult {
    let how = Interruptible::from_flags(flags, 0)?;
    if count == 0 || count > abi::WAIT_MANY_MAX {
        return Err(Error::InvalidArgs);
    }
    let size = core::mem::size_of::<WaitItem>();
    let bytes = user::read_vec(items_ptr as u64, count * size, abi::WAIT_MANY_MAX * size)?;
    let mut items: Vec<WaitItem> = bytes
        .chunks_exact(size)
        // SAFETY: WaitItem is plain data valid for any bit pattern.
        .map(|c| unsafe { core::ptr::read_unaligned(c.as_ptr() as *const WaitItem) })
        .collect();
    let mut objects = Vec::with_capacity(count);
    for it in &items {
        let h = handle(it.handle, Rights::WAIT)?;
        if h.object.signals().is_none() {
            return Err(Error::NotSupported);
        }
        objects.push(h.object);
    }
    let masks: Vec<u32> = items.iter().map(|i| i.signals).collect();
    let mut observed = alloc::vec![0u32; count];
    let result = wait_objects(&objects, &masks, &mut observed, d, how);
    for (it, o) in items.iter_mut().zip(&observed) {
        it.observed = *o;
    }
    // SAFETY: viewing plain data as bytes.
    let out = unsafe { core::slice::from_raw_parts(items.as_ptr() as *const u8, count * size) };
    user::copy_to_user(items_ptr as u64, out)?;
    result.map(|n| (n, None))
}

/// What a port's handle may do: wait and be waited on, have registrations
/// made with it and packets taken; never travel to another process, which
/// would hear of objects it does not have.
const PORT_RIGHTS: Rights =
    Rights(Rights::DUPLICATE.0 | Rights::GET_INFO.0 | Rights::WAIT.0 | Rights::READ.0 | Rights::WRITE.0);

pub fn port_create() -> SysResult {
    ok(insert(KObject::Port(Port::new()), PORT_RIGHTS)? as usize)
}

/// Kernel memory a registration may cost: it is made only while that much
/// is left outside the kernel's reserve, as an object is.
const REGISTRATION_BYTES: u64 = 64;

pub fn wait_async(raw: RawHandle, port_raw: RawHandle, key: u64, signals: u32) -> SysResult {
    let h = handle(raw, Rights::WAIT)?;
    let port = get_port(port_raw, Rights::WRITE)?;
    if matches!(h.object, KObject::Port(_)) {
        return Err(Error::NotSupported);
    }
    let s = h.object.signals().ok_or(Error::NotSupported)?;
    if signals == 0 {
        return Err(Error::InvalidArgs);
    }
    if !crate::memory::phys::user_may_use(REGISTRATION_BYTES) {
        return Err(Error::NoMemory);
    }
    if !port.reserve() {
        return Err(Error::LimitReached);
    }
    if let Err((o, active)) = s.observe(Observer::new(&port, key, signals)) {
        o.fire(active);
    }
    ok(0)
}

pub fn wait_async_cancel(raw: RawHandle, port_raw: RawHandle, key: u64) -> SysResult {
    let h = handle(raw, Rights::WAIT)?;
    let port = get_port(port_raw, Rights::WRITE)?;
    if let Some(s) = h.object.signals() {
        s.forget(&port, key);
    }
    port.purge(key);
    ok(0)
}

pub fn port_wait(port_raw: RawHandle, packets_ptr: usize, max: usize, d: u64, flags: usize) -> SysResult {
    let how = Interruptible::from_flags(flags, 0)?;
    let port = get_port(port_raw, Rights::READ)?;
    if max == 0 || max > abi::PORT_WAIT_MAX {
        return Err(Error::InvalidArgs);
    }
    let size = core::mem::size_of::<PortPacket>();
    loop {
        let packets = port.take(max);
        if !packets.is_empty() {
            // SAFETY: viewing plain data as bytes.
            let bytes = unsafe { core::slice::from_raw_parts(packets.as_ptr() as *const u8, packets.len() * size) };
            if let Err(e) = user::copy_to_user(packets_ptr as u64, bytes) {
                // Not given: kept for the next wait.
                port.put_back(packets);
                return Err(e);
            }
            return ok(packets.len());
        }
        let object = KObject::Port(port.clone());
        let mut observed = [0u32];
        wait_objects(core::slice::from_ref(&object), &[signals::READABLE], &mut observed, d, how)?;
    }
}

pub fn channel_create(out: usize, flags: u32) -> SysResult {
    if flags & !channel_flags::SHARED != 0 {
        return Err(Error::InvalidArgs);
    }
    super::may_make_object()?;
    let rights = if flags & channel_flags::SHARED != 0 { SHARED_CHANNEL_RIGHTS } else { CHANNEL_RIGHTS };
    let (a, b) = channel::create();
    let p = current_process()?;
    let mut table = p.handles.lock();
    let ha = table.insert(Handle { object: KObject::Channel(a), rights })?;
    let hb = match table.insert(Handle { object: KObject::Channel(b), rights }) {
        Ok(h) => h,
        Err(e) => {
            let _ = table.remove(ha);
            return Err(e);
        }
    };
    drop(table);
    user::write(out as u64, &[ha, hb])?;
    ok(0)
}

/// Queues a message on a channel. Its handles are the kernel's as soon as
/// their list has been read, whatever happens next: a write that fails
/// closes them (as Zircon's does, and as `rt`'s `Channel::write` expects),
/// so no failure leaves them in the caller's table without an owner.
/// Neither endpoint of the channel may travel on it, nor any channel or
/// socket that could reach it through what is queued
/// ([`crate::ipc::would_cycle`]): `NotSupported`.
pub fn channel_write(raw: RawHandle, bytes: usize, n_bytes: usize, handles_ptr: usize, n_handles: usize) -> SysResult {
    let ch = get_channel(raw, Rights::WRITE);
    let raws = user::read_u32s(handles_ptr as u64, n_handles, abi::CHANNEL_MAX_HANDLES)?;
    let taken = take_handles(&raws)?;
    // From here on, returning drops what was taken: the handles are closed.
    let ch = ch?;
    let moved = transferable(&raws, taken)?;
    if crate::ipc::would_cycle(ch.queues(), &moved) {
        return Err(Error::NotSupported);
    }
    let data = user::read_vec(bytes as u64, n_bytes, abi::CHANNEL_MAX_BYTES)?;
    match ch.write(Message { data, handles: moved }) {
        Ok(()) => ok(0),
        Err((e, msg)) => {
            drop(msg);
            Err(e)
        }
    }
}

pub fn channel_read(
    raw: RawHandle,
    bytes: usize,
    bytes_cap: usize,
    handles_ptr: usize,
    handles_cap: usize,
    actual: usize,
) -> SysResult {
    let ch = get_channel(raw, Rights::READ)?;
    let p = current_process()?;
    // Validate the output buffers up front so a dequeued message is never
    // lost to a bad pointer.
    let aspace = p.aspace().ok_or(Error::BadState)?;
    let check = |ptr: usize, len: usize| aspace.ensure_range(ptr as u64, len as u64, true);
    let out = user::UserRange::writable(bytes as u64, bytes_cap.min(abi::CHANNEL_MAX_BYTES))?;
    if !check(actual, 8) || !check(handles_ptr, handles_cap * 4) {
        return Err(Error::Fault);
    }
    let msg = match ch.read(bytes_cap, handles_cap) {
        Ok(m) => m,
        Err(e) => {
            if e.error == Error::BufferTooSmall {
                user::write(actual as u64, &[e.bytes as u32, e.handles as u32])?;
            }
            return Err(e.error);
        }
    };
    let mut values = Vec::with_capacity(msg.handles.len());
    {
        let mut table = p.handles.lock();
        for h in msg.handles {
            match table.insert(h) {
                Ok(v) => values.push(v),
                // Out of handle slots: the remaining handles are dropped.
                Err(_) => values.push(abi::INVALID_HANDLE),
            }
        }
    }
    out.write(0, &msg.data)?;
    let hv: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    user::copy_to_user(handles_ptr as u64, &hv)?;
    user::write(actual as u64, &[msg.data.len() as u32, values.len() as u32])?;
    ok(msg.data.len())
}

pub fn socket_create(out: usize) -> SysResult {
    super::may_make_object()?;
    let (a, b) = socket::create();
    let p = current_process()?;
    let mut table = p.handles.lock();
    let ha = table.insert(Handle { object: KObject::Socket(a), rights: SOCKET_RIGHTS })?;
    let hb = match table.insert(Handle { object: KObject::Socket(b), rights: SOCKET_RIGHTS }) {
        Ok(h) => h,
        Err(e) => {
            let _ = table.remove(ha);
            return Err(e);
        }
    };
    drop(table);
    user::write(out as u64, &[ha, hb])?;
    ok(0)
}

/// The most one `socket_read` or `socket_write` moves, straight between the
/// caller's memory and the socket's buffer: it bounds how long one call
/// holds the kernel's lock. Larger requests are partly done (writes of up
/// to [`socket::ATOMIC_WRITE`] bytes stay whole).
const SOCKET_CHUNK: usize = 64 * 1024;

/// Takes the handles `raws` names out of the caller's table (`None` for
/// one it does not have).
fn take_handles(raws: &[RawHandle]) -> Result<Vec<Option<Handle>>, Error> {
    let p = current_process()?;
    let mut table = p.handles.lock();
    Ok(raws.iter().map(|&r| table.remove(r).ok()).collect())
}

/// The handles taken for a transfer, if each was the caller's, named once,
/// and may be transferred.
fn transferable(raws: &[RawHandle], taken: Vec<Option<Handle>>) -> Result<Vec<Handle>, Error> {
    let mut moved = Vec::with_capacity(taken.len());
    for (i, h) in taken.into_iter().enumerate() {
        let Some(h) = h else {
            // Named twice (taken at its first mention), or not the caller's.
            return Err(if raws[..i].contains(&raws[i]) { Error::InvalidArgs } else { Error::BadHandle });
        };
        if !h.rights.contains(Rights::TRANSFER) {
            return Err(Error::AccessDenied);
        }
        moved.push(h);
    }
    Ok(moved)
}

/// Writes to a socket, and with the first byte the handles named (as a
/// channel's message carries them: the kernel's once their list is read,
/// closed if no byte goes). None of them may reach the socket through what
/// is queued ([`crate::ipc::would_cycle`]): `NotSupported`.
pub fn socket_write(raw: RawHandle, buf: usize, len: usize, handles_ptr: usize, n_handles: usize) -> SysResult {
    let s = get_socket(raw, Rights::WRITE);
    let raws = user::read_u32s(handles_ptr as u64, n_handles, abi::CHANNEL_MAX_HANDLES)?;
    let taken = take_handles(&raws)?;
    // From here on, returning drops what was taken: the handles are closed.
    let s = s?;
    let moved = transferable(&raws, taken)?;
    if crate::ipc::would_cycle(s.queues(), &moved) {
        return Err(Error::NotSupported);
    }
    let n = len.min(SOCKET_CHUNK);
    let from = user::UserRange::readable(buf as u64, n)?;
    ok(s.write_with(n, moved, |at, to| from.read(at, to))?)
}

/// Reads from a socket, and the handles that came with the bytes read, of
/// which up to `handles_cap` go into the caller's table (their values at
/// `handles_ptr`) and the rest are closed; `actual` (if not 0) gets how many
/// went in and how many were closed (`[u32; 2]`).
pub fn socket_read(
    raw: RawHandle,
    buf: usize,
    len: usize,
    handles_ptr: usize,
    handles_cap: usize,
    actual: usize,
) -> SysResult {
    let s = get_socket(raw, Rights::READ)?;
    let n = len.min(SOCKET_CHUNK).min(s.readable_bytes().max(1));
    // Validate the buffers first so that what is taken from the socket is
    // never lost to a bad pointer.
    let p = current_process()?;
    let aspace = p.aspace().ok_or(Error::BadState)?;
    let check = |ptr: usize, len: usize| len == 0 || aspace.ensure_range(ptr as u64, len as u64, true);
    let handles_cap = handles_cap.min(abi::CHANNEL_MAX_HANDLES);
    let out = user::UserRange::writable(buf as u64, n)?;
    if !check(handles_ptr, handles_cap * 4) || !check(actual, if actual == 0 { 0 } else { 8 }) {
        return Err(Error::Fault);
    }
    let (got, mut handles) = s.read_with(n, |at, bytes| out.write(at, bytes))?;
    let closed = handles.split_off(handles_cap.min(handles.len()));
    let mut values = Vec::with_capacity(handles.len());
    {
        let mut table = p.handles.lock();
        for h in handles {
            match table.insert(h) {
                Ok(v) => values.push(v),
                // Out of handle slots: the remaining handles are closed.
                Err(_) => values.push(abi::INVALID_HANDLE),
            }
        }
    }
    let dropped = closed.len();
    drop(closed);
    if !values.is_empty() {
        let hv: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        user::copy_to_user(handles_ptr as u64, &hv)?;
    }
    if actual != 0 {
        user::write(actual as u64, &[values.len() as u32, dropped as u32])?;
    }
    ok(got)
}

/// Copies what a socket holds to be read, leaving it there (`socket_read`
/// would read as much), without the handles that came with it.
pub fn socket_peek(raw: RawHandle, buf: usize, len: usize) -> SysResult {
    let s = get_socket(raw, Rights::READ)?;
    let n = len.min(SOCKET_CHUNK).min(s.readable_bytes().max(1));
    let out = user::UserRange::writable(buf as u64, n)?;
    ok(s.peek_with(n, |at, bytes| out.write(at, bytes))?)
}

pub fn socket_shutdown(raw: RawHandle) -> SysResult {
    get_socket(raw, Rights::WRITE)?.shutdown();
    ok(0)
}

pub fn event_create() -> SysResult {
    let rights = Rights(Rights::BASIC.0 | Rights::SIGNAL.0);
    ok(insert(KObject::Event(Event::new()), rights)? as usize)
}

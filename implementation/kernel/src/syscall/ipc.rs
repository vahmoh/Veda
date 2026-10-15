// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Handles, waiting, channels, sockets and events.

use alloc::vec::Vec;

use vabi::signals::{self, SIGNALED, USER_ALL};
use vabi::{Error, HandleBasicInfo, PortPacket, RawHandle, Rights, WaitItem, info_topic};

use super::{SysResult, current_process, deadline, get_channel, get_port, get_socket, handle, insert, ok};
use crate::mm::user;
use crate::object::channel::{self, Message};
use crate::object::event::Event;
use crate::object::handle::Handle;
use crate::object::port::{Observer, Port};
use crate::object::socket;
use crate::object::{KObject, new_koid};
use crate::sched::{self, WakeReason};

const CHANNEL_RIGHTS: Rights = Rights(
    Rights::TRANSFER.0 | Rights::READ.0 | Rights::WRITE.0 | Rights::WAIT.0 | Rights::SIGNAL.0 | Rights::GET_INFO.0,
);
/// Socket endpoints, unlike channels, may be duplicated (shared by several
/// processes, as a pipe or terminal is).
const SOCKET_RIGHTS: Rights = Rights(CHANNEL_RIGHTS.0 | Rights::DUPLICATE.0);

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
    let h = if raw == vabi::INVALID_HANDLE {
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
                    vabi::VmoInfo { size: v.size(), committed_bytes: v.committed_bytes(), flags: 0, _reserved: 0 };
                write(as_bytes(&info))
            }
            _ => Err(Error::WrongType),
        },
        info_topic::THREAD => match &h.object {
            KObject::Thread(t) => {
                let s = t.sched.lock();
                let info = vabi::ThreadInfo {
                    koid: t.koid,
                    state: s.state as u32,
                    priority: s.priority as u32,
                    cpu_time_ns: t.cpu_time_ns.load(core::sync::atomic::Ordering::Relaxed),
                };
                drop(s);
                write(as_bytes(&info))
            }
            _ => Err(Error::WrongType),
        },
        info_topic::SOCKET => match &h.object {
            KObject::Socket(s) => {
                let info =
                    vabi::SocketInfo { readable: s.readable_bytes() as u64, writable: s.writable_bytes() as u64 };
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

/// Shared implementation of the wait calls. Returns the number of satisfied
/// items; `observed` is filled for every item.
fn wait_objects(objects: &[KObject], masks: &[u32], observed: &mut [u32], d: u64) -> Result<usize, Error> {
    let cur = sched::current();
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
        let reason = sched::block(deadline(d));
        for o in objects {
            if let Some(s) = o.signals() {
                s.remove_waiter(wait_id);
            }
        }
        match reason {
            WakeReason::Killed => return Err(Error::Canceled),
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

pub fn wait_one(raw: RawHandle, signals: u32, d: u64) -> SysResult {
    let h = handle(raw, Rights::WAIT)?;
    if h.object.signals().is_none() {
        return Err(Error::NotSupported);
    }
    let mut observed = [0u32];
    match wait_objects(core::slice::from_ref(&h.object), &[signals], &mut observed, d) {
        Ok(_) => ok(observed[0] as usize),
        Err(e) => Err(e),
    }
}

pub fn wait_many(items_ptr: usize, count: usize, d: u64) -> SysResult {
    if count == 0 || count > vabi::WAIT_MANY_MAX {
        return Err(Error::InvalidArgs);
    }
    let size = core::mem::size_of::<WaitItem>();
    let bytes = user::read_vec(items_ptr as u64, count * size, vabi::WAIT_MANY_MAX * size)?;
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
    let result = wait_objects(&objects, &masks, &mut observed, d);
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
    if !crate::mm::phys::user_may_use(REGISTRATION_BYTES) {
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

pub fn port_wait(port_raw: RawHandle, packets_ptr: usize, max: usize, d: u64) -> SysResult {
    let port = get_port(port_raw, Rights::READ)?;
    if max == 0 || max > vabi::PORT_WAIT_MAX {
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
        wait_objects(core::slice::from_ref(&object), &[signals::READABLE], &mut observed, d)?;
    }
}

pub fn channel_create(out: usize) -> SysResult {
    super::may_make_object()?;
    let (a, b) = channel::create();
    let p = current_process()?;
    let mut table = p.handles.lock();
    let ha = table.insert(Handle { object: KObject::Channel(a), rights: CHANNEL_RIGHTS })?;
    let hb = match table.insert(Handle { object: KObject::Channel(b), rights: CHANNEL_RIGHTS }) {
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
/// closes them (as Zircon's does, and as `vrt`'s `Channel::write` expects),
/// so no failure leaves them in the caller's table without an owner.
/// Neither endpoint of the channel may travel on it, nor any channel that
/// could reach it through queued messages ([`channel::would_cycle`]):
/// `NotSupported`.
pub fn channel_write(raw: RawHandle, bytes: usize, n_bytes: usize, handles_ptr: usize, n_handles: usize) -> SysResult {
    let ch = get_channel(raw, Rights::WRITE);
    let raws = user::read_u32s(handles_ptr as u64, n_handles, vabi::CHANNEL_MAX_HANDLES)?;
    let p = current_process()?;
    let taken: Vec<Option<Handle>> = {
        let mut table = p.handles.lock();
        raws.iter().map(|&r| table.remove(r).ok()).collect()
    };
    // From here on, returning drops what was taken: the handles are closed.
    let ch = ch?;
    let mut moved = Vec::with_capacity(taken.len());
    for (i, h) in taken.into_iter().enumerate() {
        let Some(h) = h else {
            // Named twice (taken at its first mention), or not the caller's.
            return Err(if raws[..i].contains(&raws[i]) { Error::InvalidArgs } else { Error::BadHandle });
        };
        if !h.rights.contains(Rights::TRANSFER) {
            return Err(Error::AccessDenied);
        }
        if let KObject::Channel(end) = &h.object
            && end.same_channel(&ch)
        {
            return Err(Error::NotSupported);
        }
        moved.push(h);
    }
    if channel::would_cycle(&ch, &moved) {
        return Err(Error::NotSupported);
    }
    let data = user::read_vec(bytes as u64, n_bytes, vabi::CHANNEL_MAX_BYTES)?;
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
    if !check(actual, 8)
        || !check(bytes, bytes_cap.min(vabi::CHANNEL_MAX_BYTES))
        || !check(handles_ptr, handles_cap * 4)
    {
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
                Err(_) => values.push(vabi::INVALID_HANDLE),
            }
        }
    }
    user::copy_to_user(bytes as u64, &msg.data)?;
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

/// The most one `socket_read` or `socket_write` moves: the bytes go through
/// a kernel buffer. Larger requests are partly done (writes of up to
/// [`socket::ATOMIC_WRITE`] bytes stay whole).
const SOCKET_CHUNK: usize = 64 * 1024;

pub fn socket_write(raw: RawHandle, buf: usize, len: usize) -> SysResult {
    let s = get_socket(raw, Rights::WRITE)?;
    let n = len.min(SOCKET_CHUNK);
    let data = user::read_vec(buf as u64, n, SOCKET_CHUNK)?;
    ok(s.write(&data)?)
}

pub fn socket_read(raw: RawHandle, buf: usize, len: usize) -> SysResult {
    let s = get_socket(raw, Rights::READ)?;
    let n = len.min(SOCKET_CHUNK).min(s.readable_bytes().max(1));
    // Validate the buffer first so that bytes taken from the socket are
    // never lost to a bad pointer.
    let aspace = current_process()?.aspace().ok_or(Error::BadState)?;
    if !aspace.ensure_range(buf as u64, n as u64, true) {
        return Err(Error::Fault);
    }
    let mut data = user::buffer(n)?;
    let got = s.read(&mut data)?;
    user::copy_to_user(buf as u64, &data[..got])?;
    ok(got)
}

pub fn socket_shutdown(raw: RawHandle) -> SysResult {
    get_socket(raw, Rights::WRITE)?.shutdown();
    ok(0)
}

pub fn event_create() -> SysResult {
    let rights = Rights(Rights::BASIC.0 | Rights::SIGNAL.0);
    ok(insert(KObject::Event(Event::new()), rights)? as usize)
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `eventfd`: a counter that threads wake each other with (what async
//! runtimes wake their reactor with). It is the layer's own: a count behind
//! a lock, and an event object whose signals say what a read or a write
//! would do, for `poll` and for reads and writes that wait.

use abi::signals::{SIGNALED, USER_0};
use rt::object::Event;
use rt::sync::Mutex;

use crate::error::{self, SysResult};
use crate::fd::{self, Description, Object};
use crate::linux::errno::{EAGAIN, EINVAL};
use crate::linux::{efd, o};

/// The largest count.
const MAX: u64 = u64::MAX - 1;
/// The event's signal while the count can be read (it is not 0).
pub const READABLE: u32 = SIGNALED;
/// The event's signal while something can be added to the count.
pub const WRITABLE: u32 = USER_0;

pub struct EventFd {
    count: Mutex<u64>,
    /// A read takes 1 rather than the whole count (`EFD_SEMAPHORE`).
    semaphore: bool,
    pub event: Event,
}

impl EventFd {
    pub fn new(initial: u32, semaphore: bool) -> Result<EventFd, isize> {
        let event = Event::create().map_err(error::kernel)?;
        let e = EventFd { count: Mutex::new(initial as u64), semaphore, event };
        e.signal(initial as u64);
        Ok(e)
    }

    fn signal(&self, count: u64) {
        let readable = if count > 0 { READABLE } else { 0 };
        let writable = if count < MAX { WRITABLE } else { 0 };
        let _ = self.event.0.signal(READABLE | WRITABLE, readable | writable);
    }

    /// Whether a read, and a write of 1, would not wait.
    pub fn readiness(&self) -> (bool, bool) {
        let count = *self.count.lock();
        (count > 0, count < MAX)
    }

    /// `read`: the count (or 1 of it) as 8 bytes, waiting for it not to be
    /// 0 unless `nonblock`.
    pub fn read(&self, buf: &mut [u8], nonblock: bool) -> SysResult {
        if buf.len() < 8 {
            return Err(EINVAL);
        }
        loop {
            {
                let mut count = self.count.lock();
                if *count > 0 {
                    let taken = if self.semaphore { 1 } else { *count };
                    *count -= taken;
                    self.signal(*count);
                    buf[..8].copy_from_slice(&taken.to_ne_bytes());
                    return Ok(8);
                }
            }
            if nonblock {
                return Err(EAGAIN);
            }
            self.event.wait(READABLE, abi::DEADLINE_INFINITE).map_err(error::kernel)?;
        }
    }

    /// `write`: adds the 8 bytes' number to the count, waiting for room
    /// unless `nonblock`.
    pub fn write(&self, data: &[u8], nonblock: bool) -> SysResult {
        let Some(bytes) = data.get(..8) else { return Err(EINVAL) };
        let mut value = [0u8; 8];
        value.copy_from_slice(bytes);
        let value = u64::from_ne_bytes(value);
        if value == u64::MAX {
            return Err(EINVAL);
        }
        loop {
            {
                let mut count = self.count.lock();
                if MAX - *count >= value {
                    *count += value;
                    self.signal(*count);
                    return Ok(8);
                }
            }
            if nonblock {
                return Err(EAGAIN);
            }
            self.event.wait(WRITABLE, abi::DEADLINE_INFINITE).map_err(error::kernel)?;
        }
    }
}

/// `eventfd2` (and `eventfd`).
pub fn eventfd2(initial: u32, flags: u32) -> SysResult {
    if flags & !(efd::SEMAPHORE | efd::CLOEXEC | efd::NONBLOCK) != 0 {
        return Err(EINVAL);
    }
    let e = EventFd::new(initial, flags & efd::SEMAPHORE != 0)?;
    let desc = Description::new(Object::EventFd(e), o::RDWR | (flags & efd::NONBLOCK));
    fd::insert(desc, flags & efd::CLOEXEC != 0, 0).map(|n| n as usize)
}

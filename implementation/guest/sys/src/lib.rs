// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the driver VM's programs need of Linux beyond Rust's `std` and
//! `rustix` (which make their other system calls): socket options `rustix`
//! does not name ([`setsockopt`]: netlink's, packet sockets'), an `ioctl`
//! that reads into a buffer of a length chosen at run time ([`ReadInto`]),
//! timeouts in milliseconds as `poll(2)` takes them ([`poll_timeout`]),
//! and, in [`netif`], what the network drivers share: interfaces switched
//! on and off, raw packet sockets.

use std::ffi::c_void;
use std::io;
use std::os::fd::{AsFd, AsRawFd};

use rustix::event::Timespec;
use rustix::ioctl::{Direction, Ioctl, IoctlOutput, Opcode, opcode};

pub mod netif;

/// The timeout `rustix::event::poll` takes for `ms` milliseconds, as
/// `poll(2)` counts them: negative waits for ever.
pub fn poll_timeout(ms: i32) -> Option<Timespec> {
    (ms >= 0).then(|| Timespec { tv_sec: (ms / 1000).into(), tv_nsec: (ms % 1000 * 1_000_000).into() })
}

/// `setsockopt(fd, level, name, value)` for an option `rustix` does not
/// name, `value` being of the option's C type.
pub fn setsockopt<T: Copy>(fd: impl AsFd, level: i32, name: i32, value: T) -> io::Result<()> {
    // SAFETY: the kernel reads `size_of::<T>()` bytes of `value`, which
    // lives through the call.
    let ret = unsafe {
        libc::setsockopt(
            fd.as_fd().as_raw_fd(),
            level,
            name,
            (&raw const value).cast(),
            size_of::<T>() as libc::socklen_t,
        )
    };
    if ret == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// An `ioctl` that reads into a buffer, its request number made for the
/// buffer's length (`_IOC(_IOC_READ, group, number, len)`, as evdev's
/// `EVIOCGNAME(len)` and `EVIOCGBIT(type, len)` are). Its output is the
/// `ioctl`'s result: for those, the bytes written.
pub struct ReadInto<'a> {
    opcode: Opcode,
    buf: &'a mut [u8],
}

impl<'a> ReadInto<'a> {
    pub fn new(group: u8, number: u8, buf: &'a mut [u8]) -> ReadInto<'a> {
        ReadInto { opcode: opcode::from_components(Direction::Read, group, number, buf.len()), buf }
    }
}

// SAFETY: the request's length is the buffer's, so the kernel writes into
// the buffer and nowhere else; the result is a number.
unsafe impl Ioctl for ReadInto<'_> {
    type Output = usize;
    const IS_MUTATING: bool = true;

    fn opcode(&self) -> Opcode {
        self.opcode
    }

    fn as_ptr(&mut self) -> *mut c_void {
        self.buf.as_mut_ptr().cast()
    }

    unsafe fn output_from_ptr(out: IoctlOutput, _: *mut c_void) -> rustix::io::Result<usize> {
        Ok(out as usize)
    }
}

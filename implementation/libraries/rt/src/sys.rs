// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Raw system calls. Prefer the safe wrappers in [`crate::object`] and the
//! other modules; these exist for completeness and for low-level code.

pub use abi::nr;
use abi::{Error, RawHandle};

/// Result of a raw system call.
pub type SysResult = Result<usize, Error>;

#[cfg(not(veda_guest))]
#[inline(always)]
fn decode(ret: isize) -> SysResult {
    Error::from_return(ret)
}

#[cfg(not(veda_guest))]
pub use crate::arch::syscall6;

#[cfg(not(veda_guest))]
#[inline(always)]
pub fn call(n: usize, a: [usize; 6]) -> SysResult {
    // SAFETY: system calls validate all of their arguments; memory passed by
    // pointer is owned by the safe wrappers that call this.
    decode(unsafe { syscall6(n, a[0], a[1], a[2], a[3], a[4], a[5]) }.0)
}

#[cfg(not(veda_guest))]
#[inline(always)]
pub fn call2(n: usize, a: [usize; 6]) -> Result<(usize, usize), Error> {
    // SAFETY: as in `call`.
    let (ret, rdx) = unsafe { syscall6(n, a[0], a[1], a[2], a[3], a[4], a[5]) };
    decode(ret).map(|v| (v, rdx))
}

/// In a guest of the driver VM, through the bridge (see `crate::guest`).
#[cfg(veda_guest)]
pub fn call(n: usize, a: [usize; 6]) -> SysResult {
    crate::guest::call(n, a).map(|(v, _)| v)
}

#[cfg(veda_guest)]
pub fn call2(n: usize, a: [usize; 6]) -> Result<(usize, usize), Error> {
    crate::guest::call(n, a)
}

pub fn debug_write(text: &[u8]) {
    let _ = call(nr::DEBUG_WRITE, [text.as_ptr() as usize, text.len(), 0, 0, 0, 0]);
}

pub fn handle_close(h: RawHandle) -> SysResult {
    call(nr::HANDLE_CLOSE, [h as usize, 0, 0, 0, 0, 0])
}

pub fn clock_get(id: usize) -> u64 {
    call(nr::CLOCK_GET, [id, 0, 0, 0, 0, 0]).unwrap_or(0) as u64
}

pub fn process_exit(code: i64) -> ! {
    let _ = call(nr::PROCESS_EXIT, [code as usize, 0, 0, 0, 0, 0]);
    // The kernel never returns from process_exit.
    loop {
        core::hint::spin_loop();
    }
}

pub fn thread_exit() -> ! {
    let _ = call(nr::THREAD_EXIT, [0; 6]);
    loop {
        core::hint::spin_loop();
    }
}

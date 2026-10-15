// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Threads, thread-local storage and futexes.
//!
//! musl creates threads with `__clone`, which the C library hands to
//! [`__veda_clone`]: a kernel thread in this process that starts in
//! `__veda_thread_entry` (the architecture's, `arch`), which installs the
//! thread pointer and the thread's exit futex and calls the thread
//! function. When a thread ends, the kernel clears its exit futex word and
//! wakes a waiter, which is how musl knows a joined thread is gone for good
//! (`CLONE_CHILD_CLEARTID`). A detached thread ends by freeing its own
//! stack ([`__veda_unmapself`]).
//!
//! Each thread has an id of its own in the process (`signal`), which musl
//! keeps as the owner of the locks it holds: the main thread's is the
//! process id; `gettid` gives the caller's.

use core::ffi::c_void;

use vabi::{Error, nr};
use vrt::object::{Handle, Thread};
use vrt::sys::call;

use crate::error::{self, SysResult};
use crate::linux::errno::*;
use crate::linux::{Timespec, clone, futex};
use crate::{time, user};

/// The start of a new thread, at the top of its stack.
#[repr(C)]
struct Start {
    func: usize,
    arg: usize,
    tls: usize,
    exit_futex: usize,
}

/// `__clone(func, stack, flags, arg, ptid, tls, ctid)`: a new thread in this
/// process. Processes are made with `posix_spawn` instead.
///
/// # Safety
/// The arguments are musl's: `stack` the top of a stack for the thread,
/// the pointers valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __veda_clone(
    func: usize,
    stack: usize,
    flags: i32,
    arg: usize,
    ptid: usize,
    tls: usize,
    ctid: usize,
) -> i32 {
    let flags = flags as u32;
    // A thread of this process, with thread-local storage of its own (which
    // is how it is known: the C library's every thread has).
    let needed = clone::VM | clone::THREAD | clone::SETTLS;
    if flags & needed != needed || stack == 0 || tls == 0 {
        return -(ENOSYS as i32);
    }
    let name = "pthread";
    let t = match call(nr::THREAD_CREATE, [0, name.as_ptr() as usize, name.len(), 0, 0, 0]) {
        Ok(h) => h,
        Err(e) => return -(error::kernel(e) as i32),
    };
    // SAFETY: fresh handle from the kernel.
    let thread = Thread(unsafe { Handle::from_raw(t as u32) });
    let tid = match crate::signal::thread_created(tls) {
        Ok(tid) => tid,
        Err(e) => return -(e as i32),
    };
    let block = (stack & !15) - size_of::<Start>();
    let start = Start { func, arg, tls, exit_futex: if flags & clone::CHILD_CLEARTID != 0 { ctid } else { 0 } };
    // SAFETY: the top of the thread's stack, which is ours until it runs.
    unsafe { core::ptr::write(block as *mut Start, start) };
    // The thread may use its id as soon as it runs.
    if flags & clone::PARENT_SETTID != 0 && ptid != 0 {
        // SAFETY: musl passes where to store the id.
        unsafe { core::ptr::write_volatile(ptid as *mut i32, tid) };
    }
    if flags & clone::CHILD_SETTID != 0 && ctid != 0 {
        // SAFETY: as above.
        unsafe { core::ptr::write_volatile(ctid as *mut i32, tid) };
    }
    let entry = crate::arch::thread_entry_address();
    match call(nr::THREAD_START, [thread.raw() as usize, entry, block, block, 0, 0]) {
        Ok(_) => tid,
        Err(e) => {
            crate::signal::thread_not_started(tls);
            -(error::kernel(e) as i32)
        }
    }
}

/// `__set_thread_area(p)`: the calling thread's thread pointer.
#[unsafe(no_mangle)]
pub extern "C" fn __veda_set_thread_area(p: *mut c_void) -> i32 {
    match call(nr::THREAD_SET_TLS, [p as usize, 0, 0, 0, 0, 0]) {
        Ok(_) => 0,
        Err(e) => -(error::kernel(e) as i32),
    }
}

/// `set_tid_address`: the word the kernel clears when the calling thread
/// ends; returns the thread's id. musl calls it once, on the main thread,
/// whose id is the process id.
pub fn set_tid_address(addr: usize) -> SysResult {
    call(nr::THREAD_SET_EXIT_FUTEX, [addr, 0, 0, 0, 0, 0]).map_err(error::kernel)?;
    Ok(crate::signal::current_tid() as usize)
}

/// Ends the calling thread.
pub fn exit_thread() -> ! {
    crate::signal::thread_exited();
    vrt::sys::thread_exit()
}

/// Where a thread `__veda_clone` started goes when its function returns
/// (musl's never do: they end with `exit` or [`__veda_unmapself`]): it
/// ends.
#[cfg(target_os = "none")]
pub(crate) extern "C" fn thread_returned() -> ! {
    exit_thread()
}

/// `__unmapself(base, size)`: frees the stack the calling thread runs on,
/// `size` bytes at `base`, and ends the thread (musl's detached threads end
/// so); nothing touches the stack once it is unmapped.
///
/// # Safety
/// The range must be the calling thread's stack, which nothing else uses.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __veda_unmapself(base: usize, size: usize) -> ! {
    crate::signal::thread_exited();
    // SAFETY: per the caller.
    unsafe { vrt::thread::exit_unmapping(base, size) }
}

/// `futex(addr, op, val, timeout, addr2, val3)`.
pub unsafe fn futex(addr: usize, op: u32, val: u32, timeout: usize, val2: usize, val3: u32) -> SysResult {
    let cmd = op & futex::CMD_MASK;
    match cmd {
        futex::WAIT | futex::WAIT_BITSET => {
            let deadline = if timeout == 0 {
                vabi::DEADLINE_INFINITE
            } else {
                // SAFETY: the program passed a timespec.
                let t: Timespec = unsafe { user::read(timeout)? };
                let ns = t.to_ns().ok_or(EINVAL)?;
                if cmd == futex::WAIT {
                    time::monotonic_ns().saturating_add(ns)
                } else if op & futex::CLOCK_REALTIME != 0 {
                    time::monotonic_ns().saturating_add(ns.saturating_sub(time::realtime_ns()))
                } else {
                    ns
                }
            };
            if cmd == futex::WAIT_BITSET && val3 == 0 {
                return Err(EINVAL);
            }
            // A signal for this thread ends the wait, as on Linux: the C
            // library takes `EINTR` for a spurious wake-up, or acts on a
            // cancellation, after the handler ran.
            if !crate::signal::futex_wait_begins(addr) {
                return Err(EINTR);
            }
            let r = call(nr::FUTEX_WAIT, [addr, val as usize, deadline as usize, 0, 0, 0]);
            if crate::signal::futex_wait_ends() {
                return Err(EINTR);
            }
            match r {
                Ok(_) => Ok(0),
                Err(Error::ShouldWait) => Err(EAGAIN),
                Err(Error::TimedOut) => Err(ETIMEDOUT),
                Err(Error::Canceled) => Err(EINTR),
                Err(e) => Err(error::kernel(e)),
            }
        }
        futex::WAKE | futex::WAKE_BITSET => {
            call(nr::FUTEX_WAKE, [addr, val as usize, 0, 0, 0, 0]).map_err(error::kernel)
        }
        // Requeueing waiters onto another word is an optimisation: waking
        // them (who then wait on the other word themselves) is correct.
        futex::REQUEUE | futex::CMP_REQUEUE => {
            if cmd == futex::CMP_REQUEUE {
                // SAFETY: the futex word is the program's.
                let now = unsafe { core::ptr::read_volatile(addr as *const u32) };
                if now != val3 {
                    return Err(EAGAIN);
                }
            }
            let n = (val as usize).saturating_add(timeout.min(i32::MAX as usize));
            call(nr::FUTEX_WAKE, [addr, n, 0, 0, 0, 0]).map_err(error::kernel)
        }
        _ => {
            let _ = val2;
            Err(ENOSYS)
        }
    }
}

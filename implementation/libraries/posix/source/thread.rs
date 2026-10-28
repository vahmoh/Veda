// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Threads, thread-local storage and futexes.
//!
//! musl creates threads with `__clone`, which the C library hands to
//! [`__veda_clone`]: a kernel thread in this process that starts in
//! `__veda_thread_entry` (the architecture's, `architecture`), which
//! installs the thread pointer and the thread's exit futex and calls the
//! thread function. When a thread ends, the kernel clears its exit futex
//! word and wakes a waiter, which is how musl knows a joined thread is gone
//! for good (`CLONE_CHILD_CLEARTID`). A detached thread ends by freeing its
//! own stack ([`__veda_unmapself`]).
//!
//! Futexes are the kernel's, each the word in the memory that holds it,
//! whatever Linux's `FUTEX_PRIVATE_FLAG` says: the locks, conditions and
//! semaphores C programs keep in memory they share with other processes (a
//! memory file's, a shared memory object's) wait and wake across them, and
//! a word of the C library's own that it waits on both ways (its thread
//! list's lock, which a thread's exit futex wakes as it ends) is one futex,
//! as on Linux.
//!
//! Each thread has an id of its own in the process (`signal`), which musl
//! keeps as the owner of the locks it holds: the main thread's is the
//! process id; `gettid` gives the caller's.
//!
//! The layer keeps each thread's kernel handle while it lives, by its id:
//! with it a thread interrupts another, for a signal ([`interrupt`]), and
//! stops another, reads its registers and lets it go on
//! ([`veda_thread_suspend`], `<veda/thread.h>`), as a garbage collector
//! that scans other threads' stacks does (JavaScriptCore's).
//!
//! A futex wait is a blocking call's: a signal for the thread ends it
//! (`EINTR` for a timed wait, else as `SA_RESTART` says), which is how the
//! C library's cancellation and `__synccall` reach a thread that waits.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};

use abi::{Error, nr};
use runtime::object::{Handle, Thread};
use runtime::sync::Mutex;
use runtime::sys::call;

use crate::error::{self, SysResult};
use crate::interrupt::Layer;
use crate::linux::errno::*;
use crate::linux::{Timespec, clone, futex};
use crate::{interrupt, time, user};

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
    let _layer = Layer::enter();
    let flags = flags as u32;
    // A thread of this process, with thread-local storage of its own (which
    // is how it is known: the C library's every thread has).
    let needed = clone::VM | clone::THREAD | clone::SETTLS;
    if flags & needed != needed || stack == 0 || tls == 0 {
        return -(ENOSYS as i32);
    }
    // Its word starts clear (the C library copied this thread's), before
    // the signal state knows it and may say it has signals ready.
    #[cfg(target_os = "none")]
    // SAFETY: `tls` is the new thread's, which has not started.
    unsafe {
        crate::architecture::signal::word_of(tls).store(0, Ordering::SeqCst)
    };
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
    let entry = crate::architecture::thread_entry_address();
    // Kept from before it runs: it may end at once, and forget it. It runs
    // at its process's priority.
    let thread = Arc::new(thread);
    {
        let mut handles = HANDLES.lock();
        let priority = PRIORITY.load(Ordering::Relaxed);
        if priority != 0 {
            let _ = call(nr::THREAD_SET_PRIORITY, [thread.raw() as usize, priority, 0, 0, 0, 0]);
        }
        handles.insert(tid, thread.clone());
    }
    match call(nr::THREAD_START, [thread.raw() as usize, entry, block, block, 0, 0]) {
        Ok(_) => tid,
        Err(e) => {
            HANDLES.lock().remove(&tid);
            crate::signal::thread_not_started(tls);
            -(error::kernel(e) as i32)
        }
    }
}

/// The main thread's stack, its top and size, as the startup message says
/// (both 0 until it does).
static MAIN_STACK: Mutex<(usize, usize)> = Mutex::new((0, 0));

/// Records where the main thread's stack is.
pub fn set_main_stack(top: usize, size: usize) {
    *MAIN_STACK.lock() = (top, size);
}

/// `__veda_main_stack(top, size)`: the main thread's stack, for musl's
/// `pthread_getattr_np` (which on Linux probes it with `mremap`): 0, or -1
/// if the startup message did not say.
///
/// # Safety
/// `top` and `size` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __veda_main_stack(top: *mut usize, size: *mut usize) -> i32 {
    let _layer = Layer::enter();
    let (t, s) = *MAIN_STACK.lock();
    if s == 0 {
        return -1;
    }
    // SAFETY: per the caller.
    unsafe {
        top.write(t);
        size.write(s);
    }
    0
}

/// The kernel handles of this process's threads, by thread id.
static HANDLES: Mutex<BTreeMap<i32, Arc<Thread>>> = Mutex::new(BTreeMap::new());

/// The kernel priority this process's threads run at (`crate::sched`; 0:
/// where the kernel starts them), which each new one is given as it is
/// kept in [`HANDLES`].
static PRIORITY: AtomicUsize = AtomicUsize::new(0);

/// Has every thread of this process, and each it starts from now on, run
/// at kernel priority `priority`: `EACCES`, and no thread changed, if the
/// process may not run that high.
pub fn set_priority(priority: usize) -> Result<(), isize> {
    let most = runtime::object::process_self_info().map_err(error::kernel)?.max_priority as usize;
    if priority > most {
        return Err(EACCES);
    }
    let handles = HANDLES.lock();
    for t in handles.values() {
        // One that is ending, as it is changed, runs no more.
        let _ = call(nr::THREAD_SET_PRIORITY, [t.raw() as usize, priority, 0, 0, 0, 0]);
    }
    PRIORITY.store(priority, Ordering::Relaxed);
    Ok(())
}

/// Whether `tid` is a thread of this process's.
pub fn exists(tid: i32) -> bool {
    HANDLES.lock().contains_key(&tid)
}

/// Keeps the main thread's handle, as [`__veda_clone`] keeps the others':
/// called by it, once it has an id.
pub fn main_thread_ready() {
    if let Ok(h) = call(nr::THREAD_SELF, [0; 6]) {
        // SAFETY: a fresh handle from the kernel.
        let thread = Thread(unsafe { Handle::from_raw(h as u32) });
        HANDLES.lock().insert(crate::signal::current_tid(), Arc::new(thread));
    }
}

/// The calling thread is ending: its handle goes.
fn forget_current() {
    let tid = crate::signal::current_tid();
    let gone = HANDLES.lock().remove(&tid);
    drop(gone);
}

/// The handle of thread `tid` of this process.
fn handle_of(tid: i32) -> Result<Arc<Thread>, i32> {
    HANDLES.lock().get(&tid).cloned().ok_or(ESRCH as i32)
}

/// Interrupts thread `tid` of this process (`THREAD_INTERRUPT`): a signal
/// it is to act on came (`crate::interrupt`). Nothing for a thread that
/// has ended.
pub fn interrupt(tid: i32) {
    if let Ok(t) = handle_of(tid) {
        let _ = call(nr::THREAD_INTERRUPT, [t.raw() as usize, 0, 0, 0, 0, 0]);
    }
}

/// The processor time thread `tid` of this process has used; `None` for
/// no such thread.
pub fn cpu_time_ns(tid: i32) -> Option<u64> {
    handle_of(tid).ok()?.info().ok().map(|i| i.cpu_time_ns)
}

/// What a failed suspension call says, as an `errno` value.
fn suspension_errno(e: Error) -> i32 {
    match e {
        Error::BadHandle | Error::NotFound => ESRCH as i32,
        e => error::kernel(e) as i32,
    }
}

/// Stops thread `tid` of this process, another than the caller: it runs
/// none of the program's code until [`veda_thread_resume`] lets it go on
/// (as many times as it was stopped). Returns once it has stopped: 0, or
/// an `errno` value (`ESRCH`: no such thread, or it has ended; `EINVAL`:
/// the caller itself).
#[unsafe(no_mangle)]
pub extern "C" fn veda_thread_suspend(tid: i32) -> i32 {
    let _layer = Layer::enter();
    let thread = match handle_of(tid) {
        Ok(t) => t,
        Err(e) => return e,
    };
    match call(nr::THREAD_SUSPEND, [thread.raw() as usize, 0, 0, 0, 0, 0]) {
        Ok(_) => 0,
        Err(Error::BadState) => ESRCH as i32,
        Err(e) => suspension_errno(e),
    }
}

/// Lets thread `tid`, which [`veda_thread_suspend`] stopped, go on: 0, or
/// an `errno` value (`EINVAL`: it is not stopped).
#[unsafe(no_mangle)]
pub extern "C" fn veda_thread_resume(tid: i32) -> i32 {
    let _layer = Layer::enter();
    let thread = match handle_of(tid) {
        Ok(t) => t,
        Err(e) => return e,
    };
    match call(nr::THREAD_RESUME, [thread.raw() as usize, 0, 0, 0, 0, 0]) {
        Ok(_) => 0,
        Err(Error::BadState) => EINVAL as i32,
        Err(e) => suspension_errno(e),
    }
}

/// The registers of thread `tid`, which [`veda_thread_suspend`] stopped,
/// as it left the program's code, stored at `out` as the architecture's
/// `mcontext_t` has them (its general registers; no floating point): 0,
/// or an `errno` value (`EINVAL`: it is not stopped).
///
/// # Safety
/// `out` must point at a writable `mcontext_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veda_thread_registers(tid: i32, out: *mut c_void) -> i32 {
    let _layer = Layer::enter();
    let thread = match handle_of(tid) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let mut state = abi::ThreadState::default();
    let r = call(nr::THREAD_READ_STATE, [thread.raw() as usize, &raw mut state as usize, 0, 0, 0, 0]);
    match r {
        Ok(_) => {
            // SAFETY: per the caller.
            unsafe { crate::architecture::signal::store_mcontext(&state, out) };
            0
        }
        Err(Error::BadState) => EINVAL as i32,
        Err(e) => suspension_errno(e),
    }
}

/// `__set_thread_area(p)`: the calling thread's thread pointer (set before
/// the layer is, by `__init_tp`).
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
    forget_current();
    crate::signal::thread_exited();
    runtime::sys::thread_exit()
}

/// Where a thread `__veda_clone` started goes when its function returns
/// (musl's never do: they end with `exit` or [`__veda_unmapself`]): it
/// ends, in the layer until the end.
#[cfg(target_os = "none")]
pub(crate) extern "C" fn thread_returned() -> ! {
    core::mem::forget(Layer::enter());
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
    // In the layer until the end: no handler runs on a stack being freed.
    core::mem::forget(Layer::enter());
    forget_current();
    crate::signal::thread_exited();
    // SAFETY: per the caller.
    unsafe { runtime::thread::exit_unmapping(base, size) }
}

/// `futex(addr, op, val, timeout, addr2, val3)`.
pub unsafe fn futex(addr: usize, op: u32, val: u32, timeout: usize, val2: usize, val3: u32) -> SysResult {
    let cmd = op & futex::CMD_MASK;
    match cmd {
        futex::WAIT | futex::WAIT_BITSET => {
            let deadline = if timeout == 0 {
                abi::DEADLINE_INFINITE
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
            // A signal for this thread ends the wait, as on Linux: started
            // again after the handler if it had `SA_RESTART` and the wait
            // no timeout (which would then start again), else `EINTR`
            // (which the C library takes for a spurious wake-up, or acts on
            // a cancellation after).
            match interrupt::futex_wait(addr, val, deadline) {
                Ok(()) => Ok(0),
                Err(Error::ShouldWait) => Err(EAGAIN),
                Err(Error::TimedOut) => Err(ETIMEDOUT),
                Err(Error::Interrupted) if timeout == 0 => Err(ERESTARTSYS),
                Err(Error::Interrupted | Error::Canceled) => Err(EINTR),
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

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Foreign code: a thread that runs code of another system's ABI
//! (`FOREIGN_RUN`), whose system calls and exceptions the kernel gives
//! back to the code that ran it, its *host*, in the same process (see
//! `abi`'s "Foreign code").
//!
//! The host runs foreign code with a system call: its registers wait in
//! the thread ([`Host`]) while the trap frame holds the foreign code's,
//! which the return to user code resumes, with the foreign code's thread
//! pointer ([`enter`]). Whatever brings the thread back into the kernel
//! then finds it in foreign mode. Interrupts and preemption return to the
//! foreign code, as to any code; a system call, an exception the kernel
//! does not resolve and an interruption return to the host instead
//! ([`leave`]): the foreign registers are written where the host said,
//! and the host's restored, its thread pointer with them, as though its
//! call returned. So foreign code reaches the kernel only through its
//! host, with the process's handles and nothing more.

use abi::{Error, ForeignState, foreign_exit};

use super::{Interruptible, current, takes_interruptions};
use crate::architecture::thread::{is_thread_pointer, tls_of};
use crate::architecture::trap::TrapFrame;
use crate::memory::user;
use crate::scheduler::Thread;

/// What a thread in foreign mode goes back to: its host.
pub struct Host {
    /// The host's registers as its `FOREIGN_RUN` entered the kernel.
    frame: TrapFrame,
    /// Its thread pointer then.
    tls: u64,
    /// Where the foreign code's registers go as it stops.
    state: u64,
}

/// Why foreign code stops, for its host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// It made a system call.
    SystemCall,
    /// It took an exception that the kernel did not resolve.
    Exception,
    /// The thread was interrupted (`THREAD_INTERRUPT`, a signal).
    Interrupted,
}

/// Whether `t` runs foreign code.
pub fn active(t: &Thread) -> bool {
    t.foreign.lock().is_some()
}

/// The thread pointer of `t`'s own code: its host's while it runs foreign
/// code, by which its signal entry finds its state.
pub fn host_tls(t: &Thread) -> u64 {
    match t.foreign.lock().as_ref() {
        Some(host) => host.tls,
        // SAFETY: the context's thread pointer, read under the BKL.
        None => unsafe { (*t.ctx_ptr()).tls() },
    }
}

/// Has the current thread run its foreign code (`FOREIGN_RUN`) from the
/// registers at `at`, as the return to user code through `frame` (the
/// call's) will, its host's registers kept meanwhile. `Interrupted`, and
/// nothing run, if the thread was interrupted, or deferred an
/// interruption and `flags` has `wait_flags::DEFERRED`, as a wait would
/// end; `InvalidArgs` for registers user code cannot have.
pub fn enter(frame: &mut TrapFrame, at: usize, flags: usize) -> Result<(), Error> {
    let how = Interruptible::from_flags(flags, 0)?;
    let cur = current();
    if takes_interruptions(&cur)
        && (cur.interrupt.load(core::sync::atomic::Ordering::Acquire)
            || (how == Interruptible::OrDeferred && cur.deferred.load(core::sync::atomic::Ordering::Acquire)))
    {
        return Err(Error::Interrupted);
    }
    let state: ForeignState = user::read(at as u64)?;
    // Foreign code may set its own thread pointer as it likes (as user code
    // can, `is_thread_pointer`), which comes back here as it stops.
    let tls = tls_of(&state.registers);
    if !is_thread_pointer(tls) {
        return Err(Error::InvalidArgs);
    }
    let host = *frame;
    // Checked before anything changes: the frame is the host's until it
    // takes the foreign registers.
    frame.resume(&state.registers)?;
    // SAFETY: the current thread's context, under the BKL.
    let ctx = unsafe { &mut *cur.ctx_ptr() };
    let host_tls = ctx.running_tls();
    ctx.set_tls(tls);
    *cur.foreign.lock() = Some(Host { frame: host, tls: host_tls, state: at as u64 });
    Ok(())
}

/// Stops the current thread's foreign code, if it runs any, for `why`:
/// its registers as it left them go where its host said (with the
/// exception, for [`Stop::Exception`]), and `frame` becomes the host's
/// again, its `FOREIGN_RUN` returning why. A host that left no memory
/// there to write ends its process, as a crash of its own would: then
/// this returns `false`, and the caller ends the thread
/// (`super::exit_current`) once it holds no reference to it, whose
/// process and memory go with the last.
#[must_use]
pub fn leave(frame: &mut TrapFrame, why: Stop) -> bool {
    let cur = current();
    let Some(host) = cur.foreign.lock().take() else { return true };
    // SAFETY: the current thread's context, under the BKL.
    let ctx = unsafe { &mut *cur.ctx_ptr() };
    let state = frame.foreign_state(ctx.running_tls(), why == Stop::Exception);
    ctx.set_tls(host.tls);
    *frame = host.frame;
    let result = match why {
        Stop::SystemCall => foreign_exit::SYSCALL as u64,
        Stop::Exception => foreign_exit::EXCEPTION as u64,
        Stop::Interrupted => Error::Interrupted.as_return() as u64,
    };
    frame.set_syscall_result(result, None);
    if user::write(host.state, &state).is_err() {
        crate::kerror!(
            "{} (thread {}): the registers of its foreign code cannot be kept at {:#x}",
            cur.process_name(),
            cur.koid,
            host.state
        );
        if let Some(p) = &cur.process {
            p.kill(abi::EXIT_CODE_CRASHED);
        }
        return false;
    }
    true
}

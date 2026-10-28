// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! How signals reach a thread, whatever it is doing.
//!
//! A signal a thread is to act on comes with an interruption of the
//! thread (`THREAD_INTERRUPT`, or the kernel's own for a signal from
//! another process): a wait it is in ends, and as it next returns to user
//! code the kernel sends it to the signal entry ([`signal_entry`],
//! `architecture::signal`'s `__veda_signal_entry`) with its registers on
//! its stack. Handlers run where Linux runs them: in a thread running the
//! program's code, there and then; otherwise as the system call the thread
//! is in returns ([`crate::signal::returning`]). Never inside the layer,
//! whose locks the thread may hold.
//!
//! So each thread has a word (`architecture::signal`'s, in the C library's
//! structure of the thread):
//!
//! * [`IN_LAYER`]: the thread runs the layer's code. Each way in sets it
//!   (`__veda_syscall`'s gate, the C library's other hooks and
//!   `<veda/*.h>`: [`Layer`]) and the way out clears it, and handlers run
//!   with it clear ([`outside_layer`]);
//! * [`DEFERRED`]: the signal entry found the thread in the layer, and put
//!   its interruption off (`signal_return_flags::DEFER`);
//! * [`READY`]: a signal is ready for the thread (pending, not blocked),
//!   which the thread that made it so says (`crate::signal`).
//!
//! The way out of the layer clears [`IN_LAYER`], then looks at the other
//! two ([`leave`]): an interruption that comes after it looked finds the
//! thread out of the layer, and acts at once. And a blocking call's wait
//! ([`wait_one`], [`wait_many`], [`port_wait`], [`futex_wait`],
//! [`sleep_until`]) ends at once while an interruption is put off
//! (`wait_flags::DEFERRED`): the thread may begin it after the
//! interruption came, which then ended nothing. Such a wait ends when the
//! thread has a signal to act on (`Interrupted`), and the call returns
//! what it did, or `ERESTARTSYS` (or `EINTR`, for the calls Linux never
//! starts again), which the gate acts on.
//!
//! Before the C library has set up thread-local storage there is no word:
//! no signal is acted on then (the kernel interrupts no thread before the
//! process has a signal entry, which `__veda_init` sets up afterwards).

use core::sync::atomic::Ordering;

use abi::{Error, WaitItem, nr};
use runtime::object::{Handle, Port};
use runtime::sys::call;

use crate::architecture::signal as arch;
use crate::signal;

/// The thread runs the layer's code.
pub const IN_LAYER: usize = 1 << 0;
/// The signal entry put the thread's interruption off.
pub const DEFERRED: usize = 1 << 1;
/// A signal is ready for the thread.
pub const READY: usize = 1 << 2;

/// The flags of a blocking call's waits: an interruption the thread put
/// off ends them too.
const WAIT_FLAGS: usize = abi::wait_flags::DEFERRED;

/// The calling thread enters the layer: whether it acts on signals as it
/// leaves (it has a word, and was not in the layer already).
pub fn enter() -> bool {
    if !signal::threads_are_ready() {
        return false;
    }
    let inside = arch::word() & IN_LAYER != 0;
    arch::set_word(IN_LAYER);
    !inside
}

/// The calling thread leaves the layer: `true` if nothing is to be acted
/// on; else it is in the layer again, to act on it (and leave again).
pub fn leave() -> bool {
    arch::clear_word(IN_LAYER);
    if arch::word() & (DEFERRED | READY) == 0 {
        return true;
    }
    arch::set_word(IN_LAYER);
    false
}

/// Whether the calling thread may have something to act on: a signal
/// ready, or an interruption put off.
pub fn anything_to_do() -> bool {
    arch::word() & (DEFERRED | READY) != 0
}

/// Whether the signal entry put the calling thread's interruption off
/// (what other processes sent may wait in the kernel).
pub fn put_off() -> bool {
    signal::threads_are_ready() && arch::word() & DEFERRED != 0
}

/// The calling thread takes its interruption up: it is about to take what
/// other processes sent, and look at its signals.
pub fn taking_incoming() {
    if signal::threads_are_ready() {
        arch::clear_word(DEFERRED);
    }
}

/// Runs `f`, the program's code (a signal handler), outside the layer: a
/// signal that comes meanwhile is acted on at once, and a handler that
/// never returns (`siglongjmp`) leaves the thread outside.
pub fn outside_layer<R>(f: impl FnOnce() -> R) -> R {
    arch::clear_word(IN_LAYER);
    let r = f();
    arch::set_word(IN_LAYER);
    r
}

/// Says whether thread `tp` has signals ready ([`READY`]).
pub fn mark_ready(tp: usize, ready: bool) {
    // SAFETY: the thread is alive: the signal state knows it, and forgets
    // it before it ends.
    let word = unsafe { arch::word_of(tp) };
    if ready {
        word.fetch_or(READY, Ordering::SeqCst);
    } else {
        word.fetch_and(!READY, Ordering::SeqCst);
    }
}

/// A way into the layer other than a system call (`__veda_clone`,
/// `__veda_spawn`, `<veda/*.h>`...): the thread is in the layer while it
/// lives, and acts on what came meanwhile as it ends, as a system call
/// does, without the registers of the code it returns to.
pub struct Layer {
    acts: bool,
}

impl Layer {
    pub fn enter() -> Layer {
        Layer { acts: enter() }
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        if !self.acts {
            return;
        }
        while !leave() {
            signal::take_incoming();
            signal::deliver(None);
        }
    }
}

/// Where the kernel sends an interrupted thread, its registers `regs` as
/// it left user code (from `__veda_signal_entry`, which resumes them, with
/// the flags returned): one in the layer has its interruption put off,
/// another acts on its signals now, with those registers as its
/// handlers' context (where it resumes is what they leave there).
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
pub extern "C" fn signal_entry(regs: &mut abi::ThreadState) -> usize {
    if arch::word() & IN_LAYER != 0 {
        arch::set_word(DEFERRED);
        return abi::signal_return_flags::DEFER;
    }
    arch::set_word(IN_LAYER);
    loop {
        signal::take_incoming();
        let before = *regs;
        signal::deliver(Some(regs));
        arch::redirected(&before, regs);
        if leave() {
            return 0;
        }
    }
}

/// Waits as a blocking call does until `handle` has one of `signals`, or
/// `deadline` (monotonic ns) passes (`TimedOut`), or the calling thread
/// has a signal to act on (`Interrupted`; an interruption that brought
/// none goes unnoticed).
pub fn wait_one(handle: &Handle, signals: u32, deadline: u64) -> Result<u32, Error> {
    interruptible(|| {
        call(nr::OBJECT_WAIT_ONE, [handle.raw() as usize, signals as usize, deadline as usize, WAIT_FLAGS, 0, 0])
    })
    .map(|s| s as u32)
}

/// [`wait_one`] on several objects (`object_wait_many`): how many are
/// ready, each item's `observed` filled in.
pub fn wait_many(items: &mut [WaitItem], deadline: u64) -> Result<usize, Error> {
    let (ptr, len) = (items.as_mut_ptr() as usize, items.len());
    interruptible(|| call(nr::OBJECT_WAIT_MANY, [ptr, len, deadline as usize, WAIT_FLAGS, 0, 0]))
}

/// [`wait_one`] for a port's packets (`port_wait`): as many as `out`
/// holds, oldest first.
pub fn port_wait(port: &Port, out: &mut [abi::PortPacket], deadline: u64) -> Result<usize, Error> {
    let (ptr, max) = (out.as_mut_ptr() as usize, out.len().min(abi::PORT_WAIT_MAX));
    interruptible(|| call(nr::PORT_WAIT, [port.raw() as usize, ptr, max, deadline as usize, WAIT_FLAGS, 0]))
}

/// [`wait_one`] for a wake of the futex at `addr`, if it holds `expected`
/// (`ShouldWait` if not).
pub fn futex_wait(addr: usize, expected: u32, deadline: u64) -> Result<(), Error> {
    interruptible(|| call(nr::FUTEX_WAIT, [addr, expected as usize, deadline as usize, WAIT_FLAGS, 0, 0])).map(|_| ())
}

/// Sleeps until `deadline`, unless the calling thread has a signal to act
/// on first (`Interrupted`).
pub fn sleep_until(deadline: u64) -> Result<(), Error> {
    interruptible(|| call(nr::SLEEP, [deadline as usize, WAIT_FLAGS, 0, 0, 0, 0])).map(|_| ())
}

/// Makes the wait `wait` again while it is interrupted with no signal for
/// the calling thread to act on.
fn interruptible(mut wait: impl FnMut() -> Result<usize, Error>) -> Result<usize, Error> {
    loop {
        match wait() {
            Err(Error::Interrupted) if !signal::interrupted() => {}
            r => return r,
        }
    }
}

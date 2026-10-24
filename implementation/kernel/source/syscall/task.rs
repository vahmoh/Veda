// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Processes and threads.

use core::sync::atomic::Ordering;

use abi::{Error, ProcessInfo, RawHandle, Rights, SentSignals, resource_kind, restrict};

use super::{SysResult, current_process, get_process, get_resource, get_thread, insert, ok, read_str, target_process};
use crate::architecture::trap::TrapFrame;
use crate::memory::user;
use crate::object::KObject;
use crate::object::handle::Handle;
use crate::object::process::{self, Process, ProcessState};
use crate::scheduler::{self, State, Thread};

const PROCESS_RIGHTS: Rights = Rights(Rights::BASIC.0 | Rights::MANAGE.0 | Rights::GET_INFO.0 | Rights::SIGNAL.0);
const THREAD_RIGHTS: Rights = Rights(Rights::BASIC.0 | Rights::MANAGE.0 | Rights::GET_INFO.0);

fn is_user_address(a: usize) -> bool {
    (abi::USER_SPACE_START..abi::USER_SPACE_END).contains(&a)
}

/// Refuses what the calling process gave up (`flag`, [`restrict`]).
pub(super) fn allowed(flag: usize) -> Result<(), Error> {
    if current_process()?.restricted(flag) { Err(Error::AccessDenied) } else { Ok(()) }
}

pub fn process_restrict(flags: usize) -> SysResult {
    if flags & !restrict::ALL != 0 {
        return Err(Error::InvalidArgs);
    }
    current_process()?.restrict(flags);
    ok(0)
}

/// Creates a process whose threads run at `max_priority` at most (0: the
/// caller's maximum), and never above the caller's maximum: no process can
/// give one it starts more than it has. It is in the caller's memory
/// class, which [`process_set_memory_class`] can move on.
pub fn process_create(name: usize, len: usize, max_priority: usize) -> SysResult {
    allowed(restrict::NO_PROCESSES)?;
    if max_priority > abi::priority::REALTIME {
        return Err(Error::InvalidArgs);
    }
    let name = read_str(name, len, abi::NAME_MAX * 4)?;
    let parent = current_process()?;
    process::may_create(parent.koid)?;
    // An address space and a handle table, from the kernel's memory.
    if !crate::memory::phys::user_may_use(64 * 1024) {
        return Err(Error::NoMemory);
    }
    let most = match max_priority {
        0 => parent.max_priority,
        p => parent.max_priority.min(p as u8),
    };
    let p = Process::new(&name, parent.koid, most, parent.memory_class()).ok_or(Error::NoMemory)?;
    ok(insert(KObject::Process(p), PROCESS_RIGHTS)? as usize)
}

/// Moves a process (0: the caller) to a memory class
/// ([`abi::memory_class`]), only ever towards the most expendable: what is
/// ended first when memory runs out ([`crate::memory::oom`]).
pub fn process_set_memory_class(raw: RawHandle, class: usize) -> SysResult {
    target_process(raw)?.set_memory_class(class)?;
    ok(0)
}

fn start_thread(t: &Thread, entry: usize, stack: usize, arg0: u64, arg1: u64) -> Result<(), Error> {
    if !is_user_address(entry) || !is_user_address(stack) {
        return Err(Error::InvalidArgs);
    }
    if t.state() != State::New {
        return Err(Error::BadState);
    }
    t.prepare_user_entry(entry as u64, stack as u64, arg0, arg1);
    Ok(())
}

/// Starts a process's first thread, moving the bootstrap handle `arg` into
/// the process. The bootstrap handle is the kernel's whatever happens, as
/// with `channel_write`: a start that fails closes it.
pub fn process_start(
    proc: RawHandle,
    thread: RawHandle,
    entry: usize,
    stack: usize,
    arg: RawHandle,
    arg1: usize,
) -> SysResult {
    let bootstrap: Option<Handle> = match arg {
        abi::INVALID_HANDLE => None,
        arg => Some(current_process()?.handles.lock().remove(arg)?),
    };
    let p = get_process(proc, Rights::MANAGE)?;
    let t = get_thread(thread, Rights::MANAGE)?;
    if !t.process.as_ref().is_some_and(|tp| tp.koid == p.koid) {
        return Err(Error::InvalidArgs);
    }
    if !is_user_address(entry) || !is_user_address(stack) || t.state() != State::New {
        return Err(Error::InvalidArgs);
    }
    let child_handle = match bootstrap {
        Some(h) => p.handles.lock().insert(h)?,
        None => abi::INVALID_HANDLE,
    };
    start_thread(&t, entry, stack, child_handle as u64, arg1 as u64)?;
    p.mark_started();
    scheduler::make_ready(&t);
    ok(0)
}

pub fn process_exit(code: i64) -> SysResult {
    let p = current_process()?;
    p.kill(code);
    scheduler::exit_current()
}

/// Ends the process; with `kill_flags::DESCENDANTS`, the processes it
/// started (and so on) as well: its handle manages its whole job. They
/// end as POSIX signal `signal` ends a program (1 to 64), or killed (0).
pub fn process_kill(raw: RawHandle, flags: usize, signal: usize) -> SysResult {
    if flags & !abi::kill_flags::DESCENDANTS != 0 || signal > 64 {
        return Err(Error::InvalidArgs);
    }
    let code = if signal == 0 { abi::EXIT_CODE_KILLED } else { abi::EXIT_CODE_SIGNALED - signal as i64 };
    let p = get_process(raw, Rights::MANAGE)?;
    let mut victims = alloc::vec![p.clone()];
    if flags & abi::kill_flags::DESCENDANTS != 0 {
        victims.extend(process::descendants(p.koid));
    }
    let me = current_process()?.koid;
    for v in &victims {
        v.kill(code);
    }
    if victims.iter().any(|v| v.koid == me) {
        scheduler::exit_current();
    }
    ok(0)
}

/// Sets where the calling process's threads go when interrupted, and has
/// the calling thread take the signals other processes send it
/// (`nr::SIGNAL_ENTRY_SET`).
pub fn signal_entry_set(entry: usize) -> SysResult {
    if !is_user_address(entry) {
        return Err(Error::InvalidArgs);
    }
    current_process()?.set_signal_entry(entry as u64, &scheduler::current());
    ok(0)
}

/// Sends a process POSIX signal `signal`, and with
/// `kill_flags::DESCENDANTS` the processes it started, and so on
/// (`nr::PROCESS_SIGNAL`).
pub fn process_signal(raw: RawHandle, signal: usize, flags: usize) -> SysResult {
    if signal == 0 || signal > abi::posix_signal::MAX || flags & !abi::kill_flags::DESCENDANTS != 0 {
        return Err(Error::InvalidArgs);
    }
    let job = flags & abi::kill_flags::DESCENDANTS != 0;
    let rights = if job { Rights(Rights::SIGNAL.0 | Rights::MANAGE.0) } else { Rights::SIGNAL };
    let p = get_process(raw, rights)?;
    let mut targets = alloc::vec![p.clone()];
    if job {
        targets.extend(process::descendants(p.koid));
    }
    let me = current_process()?;
    for t in &targets {
        t.signal(signal, me.koid);
    }
    // Ended by its own signal: it goes now, as with `process_kill`.
    if me.ending_with().is_some() && targets.iter().any(|t| t.koid == me.koid) {
        scheduler::exit_current();
    }
    ok(0)
}

/// Sets the signals a process not started yet inherited as ignored and as
/// blocked (`nr::PROCESS_SIGNALS_INHERITED`).
pub fn process_signals_inherited(raw: RawHandle, ignored: u64, blocked: u64) -> SysResult {
    let p = get_process(raw, Rights::MANAGE)?;
    if p.state() != ProcessState::Created {
        return Err(Error::BadState);
    }
    p.set_inherited_signals(ignored, blocked);
    ok(0)
}

/// Takes the signals other processes sent the calling process
/// (`nr::SIGNAL_TAKE`): those written to `out` are taken, and no others.
/// An interruption the thread deferred is over.
pub fn signal_take(out: usize) -> SysResult {
    let p = current_process()?;
    let sent: SentSignals = p.sent_signals();
    user::write(out as u64, &sent)?;
    p.take_signals(sent.set);
    scheduler::current().deferred.store(false, Ordering::Release);
    ok(sent.set.count_ones() as usize)
}

/// Interrupts a thread of the calling process (`nr::THREAD_INTERRUPT`).
pub fn thread_interrupt(raw: RawHandle) -> SysResult {
    let t = get_thread(raw, Rights::MANAGE)?;
    let me = current_process()?;
    if !t.process.as_ref().is_some_and(|p| p.koid == me.koid) {
        return Err(Error::AccessDenied);
    }
    scheduler::interrupt(&t);
    ok(0)
}

/// Resumes the user registers at `at`, which the signal entry was given
/// (`nr::SIGNAL_RETURN`), as the return through `frame` will; with
/// `signal_return_flags::DEFER`, the thread deferred its interruption.
pub fn signal_return(frame: &mut TrapFrame, at: usize, flags: usize) -> Result<(), Error> {
    if flags & !abi::signal_return_flags::DEFER != 0 {
        return Err(Error::InvalidArgs);
    }
    let state: abi::ThreadState = user::read(at as u64)?;
    frame.resume(&state)?;
    if flags & abi::signal_return_flags::DEFER != 0 {
        scheduler::current().deferred.store(true, Ordering::Release);
    }
    Ok(())
}

/// A handle to the calling process (`nr::PROCESS_SELF`).
pub fn process_self() -> SysResult {
    ok(insert(KObject::Process(current_process()?), PROCESS_RIGHTS)? as usize)
}

pub fn thread_create(proc: RawHandle, name: usize, len: usize) -> SysResult {
    let p = target_process(proc)?;
    let name = read_str(name, len, abi::NAME_MAX * 4)?;
    let t = Thread::new_user(&p, &name).ok_or(Error::NoMemory)?;
    p.add_thread(t.clone())?;
    ok(insert(KObject::Thread(t), THREAD_RIGHTS)? as usize)
}

pub fn thread_start(raw: RawHandle, entry: usize, stack: usize, arg0: usize, arg1: usize) -> SysResult {
    let t = get_thread(raw, Rights::MANAGE)?;
    start_thread(&t, entry, stack, arg0 as u64, arg1 as u64)?;
    if let Some(p) = &t.process {
        p.mark_started();
    }
    scheduler::make_ready(&t);
    ok(0)
}

pub fn thread_set_exit_futex(addr: usize) -> SysResult {
    if addr != 0 && (!is_user_address(addr) || !addr.is_multiple_of(4)) {
        return Err(Error::InvalidArgs);
    }
    scheduler::current().exit_futex.store(addr as u64, Ordering::Relaxed);
    ok(0)
}

/// Ends the calling thread. Its exit futex word (if any) is cleared and a
/// waiter woken now that the thread is off its user stack for good; a bad
/// address is ignored, as there is no one left to report it to.
pub fn thread_exit() -> SysResult {
    let addr = scheduler::current().exit_futex.swap(0, Ordering::Relaxed);
    if addr != 0 && user::write(addr, &0u32).is_ok() {
        let _ = crate::synchronization::futex::wake(addr, 1);
    }
    scheduler::exit_current()
}

/// A handle to the calling thread.
pub fn thread_self() -> SysResult {
    ok(insert(KObject::Thread(scheduler::current()), THREAD_RIGHTS)? as usize)
}

/// Stops another thread (`nr::THREAD_SUSPEND`), waiting until it has
/// stopped if it is running: until it has entered the kernel (its user
/// registers saved), or ended. A caller killed meanwhile undoes its
/// suspension, so that none outlives it.
pub fn thread_suspend(raw: RawHandle) -> SysResult {
    let t = get_thread(raw, Rights::MANAGE)?;
    if t.koid == scheduler::current().koid {
        return Err(Error::InvalidArgs);
    }
    if t.is_dead() {
        return Err(Error::BadState);
    }
    scheduler::suspend(&t)?;
    let stopped = abi::signals::SUSPENDED | abi::signals::TERMINATED;
    if t.signals.active() & stopped == 0
        && let Err(e) = super::ipc::wait_for(&KObject::Thread(t.clone()), stopped, abi::DEADLINE_INFINITE)
    {
        let _ = scheduler::resume(&t);
        return Err(e);
    }
    ok(0)
}

/// Undoes one `thread_suspend` (`nr::THREAD_RESUME`).
pub fn thread_resume(raw: RawHandle) -> SysResult {
    let t = get_thread(raw, Rights::MANAGE)?;
    scheduler::resume(&t)?;
    ok(0)
}

/// The user registers of a stopped thread (`nr::THREAD_READ_STATE`).
pub fn thread_read_state(raw: RawHandle, buf: usize) -> SysResult {
    let t = get_thread(raw, Rights::MANAGE)?;
    if t.process.is_none()
        || t.suspensions.load(Ordering::Acquire) == 0
        || t.signals.active() & abi::signals::SUSPENDED == 0
    {
        return Err(Error::BadState);
    }
    // SAFETY: the thread is stopped (it runs no user code until resumed)
    // and alive while `t` is, and its context is read under the BKL.
    let state = unsafe { crate::architecture::thread::user_state(t.kernel_stack_top, &*t.ctx_ptr()) };
    user::write(buf as u64, &state)?;
    ok(0)
}

/// Sets a thread's priority; handle 0 is the calling thread's (a thread
/// library that keeps its threads' handles to itself lets a thread set its
/// own). Never above its process's maximum, nor above the caller's: a
/// thread above the others ready keeps them from its processor for as long
/// as it runs, which is for its process's creator to allow.
pub fn thread_set_priority(raw: RawHandle, prio: usize) -> SysResult {
    let t = if raw == abi::INVALID_HANDLE { scheduler::current() } else { get_thread(raw, Rights::MANAGE)? };
    if prio == 0 || prio > abi::priority::REALTIME {
        return Err(Error::InvalidArgs);
    }
    if prio > usize::from(t.max_priority().min(current_process()?.max_priority)) {
        return Err(Error::AccessDenied);
    }
    t.sched.lock().priority = prio as u8;
    ok(0)
}

/// Sets the calling thread's thread pointer: where it finds its
/// thread-local storage.
pub fn thread_set_tls(value: usize) -> SysResult {
    if value != 0 && !is_user_address(value) {
        return Err(Error::InvalidArgs);
    }
    let cur = scheduler::current();
    // SAFETY: the current thread's context, under the BKL.
    unsafe { (*cur.ctx_ptr()).set_tls(value as u64) };
    ok(0)
}

pub fn process_list(buf: usize, cap: usize) -> SysResult {
    allowed(restrict::NO_PROCESSES)?;
    let all = process::all();
    let size = core::mem::size_of::<ProcessInfo>();
    for (i, p) in all.iter().take(cap).enumerate() {
        user::write((buf + i * size) as u64, &p.info())?;
    }
    ok(all.len())
}

pub fn process_open(res: RawHandle, koid: u64) -> SysResult {
    allowed(restrict::NO_PROCESSES)?;
    let r = get_resource(res, Rights::NONE)?;
    if !r.permits(resource_kind::PROCESS, 0, 0) {
        return Err(Error::AccessDenied);
    }
    let p = process::find(koid).ok_or(Error::NotFound)?;
    ok(insert(KObject::Process(p), PROCESS_RIGHTS)? as usize)
}

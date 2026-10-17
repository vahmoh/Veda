// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Processes and threads.

use core::sync::atomic::Ordering;

use vabi::{Error, ProcessInfo, RawHandle, Rights, resource_kind, restrict};

use super::{SysResult, current_process, get_process, get_resource, get_thread, insert, ok, read_str, target_process};
use crate::memory::user;
use crate::object::KObject;
use crate::object::handle::Handle;
use crate::object::process::{self, Process};
use crate::scheduler::{self, State, Thread};

const PROCESS_RIGHTS: Rights = Rights(Rights::BASIC.0 | Rights::MANAGE.0 | Rights::GET_INFO.0);
const THREAD_RIGHTS: Rights = Rights(Rights::BASIC.0 | Rights::MANAGE.0 | Rights::GET_INFO.0);

fn is_user_address(a: usize) -> bool {
    (vabi::USER_SPACE_START..vabi::USER_SPACE_END).contains(&a)
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

pub fn process_create(name: usize, len: usize) -> SysResult {
    allowed(restrict::NO_PROCESSES)?;
    let name = read_str(name, len, vabi::NAME_MAX * 4)?;
    let parent = current_process()?;
    process::may_create(parent.koid)?;
    // An address space and a handle table, from the kernel's memory.
    if !crate::memory::phys::user_may_use(64 * 1024) {
        return Err(Error::NoMemory);
    }
    let p = Process::new(&name, parent.koid).ok_or(Error::NoMemory)?;
    ok(insert(KObject::Process(p), PROCESS_RIGHTS)? as usize)
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
        vabi::INVALID_HANDLE => None,
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
        None => vabi::INVALID_HANDLE,
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
/// started (and so on) as well: its handle manages its whole job.
pub fn process_kill(raw: RawHandle, flags: usize) -> SysResult {
    if flags & !vabi::kill_flags::DESCENDANTS != 0 {
        return Err(Error::InvalidArgs);
    }
    let p = get_process(raw, Rights::MANAGE)?;
    let mut victims = alloc::vec![p.clone()];
    if flags & vabi::kill_flags::DESCENDANTS != 0 {
        victims.extend(process::descendants(p.koid));
    }
    let me = current_process()?.koid;
    for v in &victims {
        v.kill(vabi::EXIT_CODE_KILLED);
    }
    if victims.iter().any(|v| v.koid == me) {
        scheduler::exit_current();
    }
    ok(0)
}

pub fn thread_create(proc: RawHandle, name: usize, len: usize) -> SysResult {
    let p = target_process(proc)?;
    let name = read_str(name, len, vabi::NAME_MAX * 4)?;
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

/// Sets a thread's priority; handle 0 is the calling thread's (a thread
/// library that keeps its threads' handles to itself lets a thread set its
/// own).
pub fn thread_set_priority(raw: RawHandle, prio: usize) -> SysResult {
    let t = if raw == vabi::INVALID_HANDLE { scheduler::current() } else { get_thread(raw, Rights::MANAGE)? };
    if prio == 0 || prio > vabi::priority::REALTIME {
        return Err(Error::InvalidArgs);
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

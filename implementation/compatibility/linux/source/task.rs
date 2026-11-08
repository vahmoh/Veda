// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Linux program's threads: each a kernel thread of this process that
//! runs the program's code in foreign mode and, as that code stops, does
//! what it asked ([`run`]): a system call (`crate::syscall`), a signal for
//! a fault (`crate::signal`), and the signals ready for it, before running
//! it again.
//!
//! A thread's own state is a [`Task`]: its foreign registers, its thread
//! block (which its thread pointer points into, as the POSIX layer reads
//! it: `posix::host::ThreadBlock`), its alternate signal stack, the system
//! call a signal may start again, and its robust futex list. `clone` makes
//! one ([`clone`]), on a stack of its own for Linux compatibility's code,
//! which goes once the thread has ended ([`reap`]); the program's own
//! stack is the program's, as on Linux, and the new thread starts with the
//! vector registers of the one that made it, as there.
//!
//! `fork`, and `clone` without `CLONE_VM`, make a process that is a copy of
//! this one ([`fork`]): the kernel copies its memory, the program's and
//! Linux compatibility's alike, the POSIX layer what it keeps
//! (`posix::host::fork`), and the copy's one thread is the calling one,
//! whose task goes on there as its main thread.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use abi::{Error, ForeignState, foreign_exit, map_flags, nr, signals};
use posix::host::ThreadBlock;
use runtime::object::{Handle, Process, Thread};
use runtime::sync::Mutex;
use runtime::sys::call;
use runtime::vm;

use crate::architecture;
use crate::linux::clone::*;
use crate::linux::errno::*;
use crate::signal::{self, AltStack, Fault};

/// The stack each thread runs Linux compatibility's code on: the POSIX
/// layer's calls, and the signal entry's saving of the vector registers.
const HOST_STACK: usize = 256 << 10;
/// Inaccessible pages below it.
const GUARD: usize = 16 << 10;

/// A system call a signal ended (`crate::signal::deliver`).
#[derive(Debug, Clone, Copy)]
pub struct Restart {
    /// Its number.
    pub call: usize,
    /// Its first argument, as it was made: on some architectures its
    /// result's register (ARM64's `x0`), which a call made again needs.
    pub first: usize,
    /// What it returned: an `ERESTART` code.
    pub code: isize,
}

/// A thread of the Linux program.
pub struct Task {
    /// What its thread pointer points into (it must not move: a task is
    /// boxed for its life).
    pub block: ThreadBlock,
    /// Its registers in the program's code.
    pub state: ForeignState,
    pub tid: i32,
    pub altstack: AltStack,
    /// The system call a signal ended, to be made again unless a handler
    /// says otherwise.
    pub restart: Option<Restart>,
    /// The fault whose signal is pending, for its frame's context.
    pub fault: Option<Fault>,
    /// Its robust futex list (`set_robust_list`): head and length.
    pub robust_list: (u64, u64),
    /// Its kernel thread, to know when it has ended.
    handle: Option<Thread>,
    /// The stack of Linux compatibility's code (with its guard), 0 for the
    /// main thread's, which its loader made.
    host_stack: usize,
    /// If it is a child `vfork` made: a process of its own.
    pub apart: Option<Apart>,
}

/// A child `vfork` made (`clone` with `CLONE_VM` and without
/// `CLONE_THREAD`): a process of its own, running in its parent's memory
/// on a thread of this process, until it replaces its program (in a
/// process of Veda's made for it, whose id it has) or ends (as its parent
/// waits for it to, with `CLONE_VFORK`).
pub struct Apart {
    pub pid: i32,
    /// The process its `execve` starts its program in.
    pub process: Process,
    /// Set (and woken) once it has replaced its program or ended: what its
    /// parent waits on.
    done: Arc<AtomicU32>,
}

impl Apart {
    /// The child has replaced its program or ended: its parent goes on.
    pub fn finished(&self) {
        self.done.store(1, Ordering::Release);
        let _ = call(nr::FUTEX_WAKE, [self.done.as_ptr() as usize, usize::MAX >> 1, 0, 0, 0, 0]);
    }
}

impl Task {
    fn new(state: ForeignState) -> Box<Task> {
        Box::new(Task {
            block: ThreadBlock::new(),
            state,
            tid: 0,
            altstack: AltStack::default(),
            restart: None,
            fault: None,
            robust_list: (0, 0),
            handle: None,
            host_stack: 0,
            apart: None,
        })
    }
}

/// The program's threads that live: when the last ends, the process does.
static LIVE: AtomicUsize = AtomicUsize::new(0);

/// Whether the threads but one are to end (`execve`, [`stop_others`]).
static STOPPING: AtomicBool = AtomicBool::new(false);

/// Tasks that have ended, whose stacks go once their kernel threads have:
/// each where it was, as its thread pointer still points into it.
#[allow(clippy::vec_box)]
static ENDED: Mutex<Vec<Box<Task>>> = Mutex::new(Vec::new());

/// Makes the calling thread, the process's first, the program's main
/// thread: its task, its thread pointer set (which the POSIX layer finds
/// the thread by, and `posix::host::start` needs). The program's registers
/// are for its loader to set; [`run`] runs it.
pub fn main_thread() -> &'static mut Task {
    let task = Box::leak(Task::new(ForeignState::default()));
    let tp = task.block.thread_pointer();
    let _ = call(nr::THREAD_SET_TLS, [tp, 0, 0, 0, 0, 0]);
    LIVE.store(1, Ordering::Release);
    task
}

/// Runs thread `t`'s code, doing what it asks, until it ends.
pub fn run(t: &mut Task) -> ! {
    loop {
        if STOPPING.load(Ordering::Acquire) && crate::exec::stopping_others(t.tid) {
            end_quietly(t);
        }
        match runtime::foreign::run(&mut t.state, abi::wait_flags::DEFERRED) {
            Ok(foreign_exit::SYSCALL) => crate::syscall::handle(t),
            Ok(foreign_exit::EXCEPTION) => signal::fault(t),
            Ok(_) | Err(Error::Interrupted) => {}
            // Registers no code can have (a frame's that `rt_sigreturn`
            // took back): as Linux, a fault.
            Err(Error::InvalidArgs) => {
                let fault = architecture::refused(&t.state);
                t.fault = Some(fault);
                let info = posix::host::Info { code: fault.code, addr: Some(fault.addr), ..Default::default() };
                posix::host::force(fault.signal, info);
            }
            Err(e) => {
                runtime::println!("{}: its code cannot run: {e}", crate::process_name());
                posix::host::exit_by_signal(crate::linux::sig::KILL);
            }
        }
        signal::deliver(t);
    }
}

/// `clone(flags, stack, parent_tid, child_tid, tls)`: a new thread of
/// this process (`CLONE_VM | CLONE_THREAD | CLONE_SIGHAND`, the files and
/// the working directory shared too, as every thread library asks), or a
/// child that shares its memory (`CLONE_VM` alone, `vfork`'s), each
/// returning 0 from the call on `stack` with the caller's vector registers
/// (its floating-point environment, which POSIX has a thread inherit), its
/// thread pointer `tls` with `CLONE_SETTLS`; or, without `CLONE_VM`, a
/// process of memory of its own ([`fork`]). Sharing the program cannot
/// have (a thread with descriptors of its own) is refused (`EINVAL`). A
/// pidfd asked for (`CLONE_PIDFD`) goes to `pidfd`: `clone`'s
/// `parent_tid`, `clone3`'s own field.
#[allow(clippy::too_many_arguments)] // the call's own
pub fn clone(
    t: &mut Task,
    flags: u64,
    stack: u64,
    parent_tid: u64,
    child_tid: u64,
    tls: u64,
    pidfd: u64,
) -> Result<usize, isize> {
    let known = VM
        | FS
        | FILES
        | SIGHAND
        | THREAD
        | SYSVSEM
        | SETTLS
        | PARENT_SETTID
        | CHILD_CLEARTID
        | DETACHED
        | CHILD_SETTID
        | VFORK
        | PIDFD
        | SIGNAL;
    if flags & !known != 0 {
        return Err(EINVAL);
    }
    if flags & VM == 0 {
        return fork(t, flags, parent_tid, child_tid, pidfd);
    }
    let thread = flags & THREAD != 0;
    if thread && (flags & (SIGHAND | FS | FILES) != (SIGHAND | FS | FILES) || flags & (VFORK | PIDFD) != 0) {
        return Err(EINVAL);
    }
    // A thread shares its process's handlers, which `CLONE_SIGHAND` needs
    // memory shared for.
    reap();
    let tls = (flags & SETTLS != 0).then_some(tls);
    let mut child = Task::new(architecture::child(&t.state, stack, tls));
    let apart = if thread {
        None
    } else {
        let (pid, process) = posix::host::reserve_child()?;
        Some(Apart { pid, process, done: Arc::new(AtomicU32::new(0)) })
    };
    let id = start(&mut child, apart, flags, parent_tid, child_tid)?;
    // A parent waits for the child `vfork` made to replace its program or
    // end, whatever comes meanwhile, as Linux's does.
    if let Some(done) = id.1 {
        while done.load(Ordering::Acquire) == 0 {
            let _ = call(nr::FUTEX_WAIT, [done.as_ptr() as usize, 0, abi::DEADLINE_INFINITE as usize, 0, 0, 0]);
        }
        if flags & PIDFD != 0 {
            write_pidfd(pidfd, id.0);
        }
    }
    Ok(id.0 as usize)
}

/// Writes a descriptor of child `pid` (`pidfd_open`'s; -1 if it cannot be
/// had) to the program's `at`, for `CLONE_PIDFD`.
fn write_pidfd(at: u64, pid: i32) {
    let pidfd_open = linux_raw_sys::general::__NR_pidfd_open as usize;
    // SAFETY: a call of our own.
    let fd = unsafe { posix::host::syscall(pidfd_open, [pid as usize, 0, 0, 0, 0, 0]) }.map_or(-1, |fd| fd as i32);
    // SAFETY: the program's memory, which a fault writing it ends.
    unsafe { crate::user::write(at as usize, fd) };
}

/// `fork`, and `clone` without `CLONE_VM` (`flags`, which name `SIGCHLD`
/// as what the child's end sends): a process that is a copy of this one,
/// whose one thread is the calling one, going on from the call there with
/// 0 (`posix::host::fork`, which says what it has). Returns its process
/// id. The ids asked for are written as Linux writes them: the child's id
/// to `parent_tid` here (`CLONE_PARENT_SETTID`), and there to `child_tid`
/// (`CLONE_CHILD_SETTID`), which its thread's end clears and wakes
/// (`CLONE_CHILD_CLEARTID`); its pidfd to `pidfd` (`CLONE_PIDFD`). What
/// two processes of memory of their own cannot share (descriptors, a
/// working directory, handlers, a thread pointer), another signal at its
/// end, and waiting for it as `vfork` does, are refused (`EINVAL`).
pub fn fork(t: &mut Task, flags: u64, parent_tid: u64, child_tid: u64, pidfd: u64) -> Result<usize, isize> {
    let allowed = PARENT_SETTID | CHILD_SETTID | CHILD_CLEARTID | PIDFD | DETACHED | SYSVSEM | SIGNAL;
    if flags & !allowed != 0 || flags & SIGNAL != u64::from(crate::linux::sig::CHLD) {
        return Err(EINVAL);
    }
    // `clone` has one word for both.
    if flags & PIDFD != 0 && flags & PARENT_SETTID != 0 && pidfd == parent_tid {
        return Err(EINVAL);
    }
    reap();
    let heap = runtime::heap();
    // What the program was started with, its areas (in the order `/proc`
    // takes them), and the heap (last), as the copy is to have them: whole.
    let hold = || (crate::proc::hold(), crate::memory::hold(), heap.map(|h| h.hold()));
    let Some(pid) = posix::host::fork(hold)? else {
        forked(t);
        if flags & CHILD_SETTID != 0 {
            // SAFETY: the program's memory, which a fault writing it ends.
            unsafe { crate::user::write(child_tid as usize, t.tid) };
        }
        if flags & CHILD_CLEARTID != 0 {
            let _ = call(nr::THREAD_SET_EXIT_FUTEX, [child_tid as usize, 0, 0, 0, 0, 0]);
        }
        return Ok(0);
    };
    if flags & PARENT_SETTID != 0 {
        // SAFETY: as above.
        unsafe { crate::user::write(parent_tid as usize, pid) };
    }
    if flags & PIDFD != 0 {
        write_pidfd(pidfd, pid);
    }
    Ok(pid as usize)
}

/// In the process [`fork`] made: the calling thread, `t`, is its one
/// thread, its main one, whose id is the process's; the tasks of the
/// threads it does not have are forgotten (their memory is the original's
/// copy), and so are the areas it was not given (`MADV_DONTFORK`); as on
/// Linux its robust futex list is gone (the C libraries set it again).
fn forked(t: &mut Task) {
    LIVE.store(1, Ordering::Release);
    STOPPING.store(false, Ordering::Release);
    // SAFETY: the copy, before it has another thread; the tasks of the
    // original's other threads are forgotten, not dropped.
    unsafe { ENDED.reset_after_fork(Vec::new()) };
    crate::exec::forked();
    crate::memory::forked();
    t.tid = posix::host::pid();
    t.robust_list = (0, 0);
    t.restart = None;
    t.fault = None;
    t.handle = None;
}

/// Starts thread `child` of [`clone`] (a child `vfork` made, if `apart`):
/// its stack, its kernel thread, the ids the program asked for, the
/// calling thread's vector registers. Returns its id (a thread's, or the
/// child's process id), and what to wait on for a `CLONE_VFORK` child.
fn start(
    child: &mut Box<Task>,
    apart: Option<Apart>,
    flags: u64,
    parent_tid: u64,
    child_tid: u64,
) -> Result<(i32, Option<Arc<AtomicU32>>), isize> {
    // Its own code's stack, below it a guard.
    let rw = map_flags::READ | map_flags::WRITE;
    let base = vm::allocate(None, GUARD + HOST_STACK, 0, rw).map_err(|_| ENOMEM)?;
    let _ = vm::protect(None, base, GUARD, 0);
    child.host_stack = base;
    let undo = |tp: Option<usize>| {
        if let Some(tp) = tp {
            posix::host::thread_not_started(tp);
        }
        let _ = vm::unmap(None, base, GUARD + HOST_STACK);
    };
    let tp = child.block.thread_pointer();
    let tid = match posix::host::thread_created(tp) {
        Ok(tid) => tid,
        Err(e) => {
            undo(None);
            return Err(e);
        }
    };
    child.tid = tid;
    let name: &[u8] = if apart.is_some() { b"vfork" } else { b"thread" };
    let made = call(nr::THREAD_CREATE, [0, name.as_ptr() as usize, name.len(), 0, 0, 0]);
    // SAFETY: a fresh handle from the kernel.
    let handle = match made.map(|h| Thread(unsafe { Handle::from_raw(h as u32) })) {
        Ok(h) => h,
        Err(_) => {
            undo(Some(tp));
            return Err(EAGAIN);
        }
    };
    // The layer keeps one handle (to interrupt the thread); the task the
    // other (to know when it has ended).
    let Ok(kept) = handle.0.duplicate(None) else {
        undo(Some(tp));
        return Err(EAGAIN);
    };
    child.handle = Some(Thread(kept));
    // A child `vfork` made is a process: its id is its process's, and its
    // descriptors, directory and handlers its own unless shared.
    let id = apart.as_ref().map_or(tid, |a| a.pid);
    let done = apart.as_ref().map(|a| a.done.clone());
    if let Some(a) = &apart {
        posix::host::process_apart(tp, a.pid, flags & FILES != 0, flags & FS != 0, flags & SIGHAND != 0);
    }
    child.apart = apart;
    // The ids the program asked for, written before the thread runs.
    // SAFETY: the program's memory, which a fault writing it ends.
    let written = unsafe {
        (flags & PARENT_SETTID == 0 || flags & PIDFD != 0 || crate::user::write(parent_tid as usize, id).is_some())
            && (flags & CHILD_SETTID == 0 || crate::user::write(child_tid as usize, id).is_some())
    };
    if !written {
        undo(Some(tp));
        return Err(EFAULT);
    }
    let clear = if flags & CHILD_CLEARTID != 0 { child_tid } else { 0 };
    let child = Box::leak(core::mem::replace(child, Task::new(ForeignState::default())));
    let vector = architecture::fpu::saved();
    let start = Box::leak(Box::new(Start { task: child, thread: Some(handle), clear_tid: clear, vector }));
    let top = base + GUARD + HOST_STACK;
    LIVE.fetch_add(1, Ordering::AcqRel);
    let raw = start.thread.as_ref().map_or(0, |h| h.raw() as usize);
    let entry = thread_entry as *const () as usize;
    let started =
        call(nr::THREAD_START, [raw, entry, abi::entry_stack_pointer(top), start as *mut Start as usize, 0, 0]);
    if started.is_err() {
        LIVE.fetch_sub(1, Ordering::AcqRel);
        undo(Some(tp));
        return Err(EAGAIN);
    }
    Ok((id, done))
}

/// What a new thread starts with.
struct Start {
    task: &'static mut Task,
    thread: Option<Thread>,
    /// Its `CLONE_CHILD_CLEARTID` word.
    clear_tid: u64,
    /// Its vector registers: those of the thread that made it, as it made
    /// the call.
    vector: architecture::fpu::Registers,
}

/// Where a thread [`clone`] makes starts, on its own stack: its thread
/// pointer, its word cleared as it ends, the layer's knowledge of it, its
/// vector registers, then the program's code.
extern "C" fn thread_entry(start: *mut Start) -> ! {
    // The box goes as it is taken apart, and the registers as they are
    // loaded: what this function holds is never dropped, as `run` does
    // not return.
    // SAFETY: `clone` leaked it for this thread alone.
    let Start { task, thread, clear_tid, vector } = *unsafe { Box::from_raw(start) };
    let tp = task.block.pointer();
    let _ = call(nr::THREAD_SET_TLS, [tp, 0, 0, 0, 0, 0]);
    let _ = call(nr::THREAD_SET_EXIT_FUTEX, [clear_tid as usize, 0, 0, 0, 0, 0]);
    if let Some(thread) = thread {
        posix::host::thread_ready(task.tid, thread);
    }
    // Nothing of this program's touches them before the program's code
    // runs.
    architecture::fpu::load(vector);
    run(task)
}

/// `exit`: ends the calling thread (its robust futexes released, its
/// `CLEARTID` word cleared and woken by the kernel), or the process with
/// `code` if it is the last. A child `vfork` made ends as a process
/// ([`end_apart`]).
pub fn exit(t: &mut Task, code: i32) -> ! {
    if t.apart.is_some() {
        end_apart(t, i64::from(code & 0xff));
    }
    release_robust_futexes(t);
    if LIVE.fetch_sub(1, Ordering::AcqRel) == 1 {
        posix::host::exit(code);
    }
    end(t)
}

/// `exit_group`: ends the process with `code`; for a child `vfork` made,
/// the child alone.
pub fn exit_group(t: &mut Task, code: i32) -> ! {
    if t.apart.is_some() {
        end_apart(t, i64::from(code & 0xff));
    }
    posix::host::exit(code)
}

/// Ends the child `vfork` made that thread `t` is: as a process that ended
/// with `code` (a Veda exit code) before it started a program, which its
/// parent may wait for, its parent going on.
pub fn end_apart(t: &mut Task, code: i64) -> ! {
    if let Some(apart) = &t.apart {
        posix::host::child_ended(apart.pid, code);
    }
    replaced(t)
}

/// The child `vfork` made that thread `t` is has started its program in a
/// process of its own (or has ended): its thread ends, and its parent goes
/// on.
pub fn replaced(t: &mut Task) -> ! {
    LIVE.fetch_sub(1, Ordering::AcqRel);
    let _ = call(nr::THREAD_SET_EXIT_FUTEX, [0, 0, 0, 0, 0, 0]);
    if let Some(apart) = &t.apart {
        apart.finished();
    }
    end(t)
}

/// Ends thread `t`, another than the process's last: what the layer knows
/// of it goes, and its stack once the kernel thread has ended.
fn end(t: &mut Task) -> ! {
    posix::host::thread_ending();
    // SAFETY: every task but the main thread's was leaked from a box,
    // and nothing else refers to it once the thread ends.
    if t.host_stack != 0 {
        let task = unsafe { Box::from_raw(t as *mut Task) };
        ENDED.lock().push(task);
    }
    runtime::sys::thread_exit()
}

/// Ends thread `t` for an `execve` of another (without its robust
/// futexes, whose memory goes).
fn end_quietly(t: &mut Task) -> ! {
    LIVE.fetch_sub(1, Ordering::AcqRel);
    let _ = call(nr::THREAD_SET_EXIT_FUTEX, [0, 0, 0, 0, 0, 0]);
    end(t)
}

/// Frees what threads that have ended left: their stacks.
pub fn reap() {
    let mut ended = ENDED.lock();
    ended.retain(|task| {
        let gone = task
            .handle
            .as_ref()
            .is_none_or(|h| h.0.wait(signals::TERMINATED, 0).is_ok_and(|s| s & signals::TERMINATED != 0));
        if gone {
            let _ = vm::unmap(None, task.host_stack, GUARD + HOST_STACK);
        }
        !gone
    });
}

/// How many of the program's threads live.
pub fn live() -> usize {
    LIVE.load(Ordering::Acquire)
}

/// Has the threads but the calling one end (`execve`), or stop having them
/// end (`on` false).
pub fn set_stopping(on: bool) {
    STOPPING.store(on, Ordering::Release);
}

/// `set_robust_list(head, len)`: Linux's list is three words.
pub fn set_robust_list(t: &mut Task, head: u64, len: u64) -> Result<usize, isize> {
    if len != 24 {
        return Err(EINVAL);
    }
    t.robust_list = (head, len);
    Ok(0)
}

/// `get_robust_list(pid, head, len)`, of the calling thread alone.
pub fn get_robust_list(t: &Task, pid: i32, head: usize, len: usize) -> Result<usize, isize> {
    if pid != 0 && pid != t.tid {
        return Err(EPERM);
    }
    // SAFETY: the program's memory, which a fault writing it ends.
    unsafe {
        crate::user::write(head, t.robust_list.0).ok_or(EFAULT)?;
        crate::user::write(len, t.robust_list.1).ok_or(EFAULT)?;
    }
    Ok(0)
}

/// As a thread ends, what Linux does for its robust futexes: each lock it
/// holds is marked as its owner's having died, and a waiter woken.
fn release_robust_futexes(t: &Task) {
    const OWNER_DIED: u32 = 0x4000_0000;
    const WAITERS: u32 = 0x8000_0000;
    const TID_MASK: u32 = 0x3fff_ffff;
    /// How many entries Linux follows at most (`ROBUST_LIST_LIMIT`).
    const LIMIT: usize = 2048;
    let (head, _) = t.robust_list;
    if head == 0 {
        return;
    }
    let read = |at: u64| -> Option<u64> {
        // SAFETY: the program's memory, which a fault reading it ends.
        unsafe { crate::user::read::<u64>(at as usize) }
    };
    let (Some(first), Some(offset), Some(pending)) =
        (read(head), read(head.wrapping_add(8)), read(head.wrapping_add(16)))
    else {
        return;
    };
    let release = |entry: u64| {
        let word = (entry & !1).wrapping_add(offset) as usize;
        if !word.is_multiple_of(4) {
            return;
        }
        // Whether a waiter is to be woken: the word marked, if this thread
        // held the lock.
        let marked = crate::memory::while_accessible(word, 4, true, || {
            // SAFETY: the program's lock word, writable, held meanwhile.
            let held = unsafe { &*(word as *const core::sync::atomic::AtomicU32) };
            let mut value = held.load(Ordering::SeqCst);
            loop {
                if value & TID_MASK != t.tid as u32 {
                    return false;
                }
                match held.compare_exchange(value, (value & WAITERS) | OWNER_DIED, Ordering::SeqCst, Ordering::SeqCst) {
                    Ok(_) => return value & WAITERS != 0,
                    Err(now) => value = now,
                }
            }
        });
        if marked == Some(true) {
            let _ = call(nr::FUTEX_WAKE, [word, 1, 0, 0, 0, 0]);
        }
    };
    let mut entry = first;
    for _ in 0..LIMIT {
        if entry == head || entry == 0 {
            break;
        }
        if entry != pending {
            release(entry);
        }
        match read(entry) {
            Some(next) => entry = next,
            None => break,
        }
    }
    if pending != 0 {
        release(pending);
    }
}

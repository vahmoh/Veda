// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The layer for a program that hosts foreign code: Veda's Linux
//! compatibility, which runs a Linux program in its own process (in
//! foreign mode, `abi`'s "Foreign code") and carries out its system calls
//! with this layer, as the C library's calls are carried out.
//!
//! The layer is the same, but for what C programs have of their own:
//!
//! * **Threads.** The layer knows a thread by its thread pointer and keeps
//!   a word for it where the C library's thread structure has room
//!   (`architecture::signal`). A host's threads have a [`ThreadBlock`]
//!   instead, which their thread pointers point into: the foreign code's
//!   own thread pointer is its own business, which the kernel switches.
//! * **Signals.** A host's threads are always inside the layer: an
//!   interruption that comes is deferred, as the signal entry defers one in
//!   a system call, and the host acts on it as foreign code's system call
//!   returns, or as the kernel ends the foreign code for it. It takes the
//!   signals one at a time ([`take_next`]) and runs their handlers in the
//!   foreign code its own way (a Linux signal frame); the layer keeps the
//!   rest (dispositions, masks, what is pending, who gets what).
//! * **Paths.** A Linux program names paths in a tree of its own, which
//!   the host describes ([`set_namespace`]).
//!
//! The program's memory, its system calls' arguments included, is the
//! host's process's, as a C program's is: pointers are used in place.

pub use crate::error::SysResult;
pub use crate::linux::{Sigaction, Siginfo};
pub use crate::path::{Namespace, program_path, set_namespace, vfs_path};
pub use crate::process::Program;
pub use crate::signal::{Info, Next, block_for_handler, force, force_default, restore_mask, take_incoming, take_next};

/// What the layer reads through a host thread's thread pointer: the words
/// of the C library's thread structure it uses ([`ThreadBlock::thread_pointer`]).
#[repr(C, align(64))]
pub struct ThreadBlock([usize; 32]);

impl Default for ThreadBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadBlock {
    pub const fn new() -> ThreadBlock {
        ThreadBlock([0; 32])
    }

    /// Readies the block for a thread: its word clear, and the thread
    /// pointer where the layer looks for it (at `%fs:0` on AMD64, as the C
    /// library's thread structure has it). Returns the thread pointer
    /// ([`pointer`](Self::pointer)). The block must not move while a
    /// thread has it.
    pub fn thread_pointer(&mut self) -> usize {
        self.0 = [0; 32];
        let tp = self.pointer();
        #[cfg(target_arch = "x86_64")]
        {
            self.0[0] = tp;
        }
        tp
    }

    /// The thread pointer of the thread the block is for.
    pub fn pointer(&self) -> usize {
        let block = self.0.as_ptr() as usize;
        // The structure ends where the thread pointer is on ARM64, the word
        // 176 bytes below (`architecture::signal`); it starts there on
        // AMD64, the word 32 bytes in.
        if cfg!(target_arch = "aarch64") { block + 200 } else { block }
    }
}

/// Sets the layer up in a host's main thread, which has its thread
/// pointer, a [`ThreadBlock`]'s, and has read its startup message
/// (`runtime`'s entry point): what the startup message says (the working
/// directory, the descriptors, the terminal, the registry, what is
/// inherited that is no handle), the signals, and the signal entry. The
/// thread stays in the layer for ever, as every thread [`thread_ready`]
/// readies does.
pub fn start() {
    #[cfg(feature = "signal-entry")]
    crate::architecture::signal::init();
    crate::signal::threads_ready();
    crate::interrupt::enter();
    crate::thread::main_thread_ready();
    crate::adopt_startup();
    let (ignored, mask, nice) = *crate::INHERITED.lock();
    crate::signal::inherit(ignored, mask);
    crate::sched::inherit(nice);
    #[cfg(feature = "signal-entry")]
    let _ =
        runtime::sys::call(abi::nr::SIGNAL_ENTRY_SET, [crate::architecture::signal::entry_address(), 0, 0, 0, 0, 0]);
}

/// Carries out Linux system call `n` with arguments `a`, as for a C
/// program; a blocking call a signal ended returns `ERESTARTSYS` (or
/// `EINTR`, for the calls Linux never starts again), which the host acts
/// on as Linux does.
///
/// # Safety
/// The arguments are the foreign code's: pointers must be valid for the
/// call.
pub unsafe fn syscall(n: usize, a: [usize; 6]) -> SysResult {
    // SAFETY: per the caller.
    unsafe { crate::syscall::dispatch(n, a) }
}

/// What a system call returns for `r`: its value, or the `errno` value
/// negated.
pub fn encode(r: SysResult) -> isize {
    crate::error::encode(r)
}

/// A thread whose thread pointer will be `tp` is about to start: its id,
/// and the signal mask and name of the calling thread, as a thread
/// inherits them on Linux. `EAGAIN` if no id is free.
pub fn thread_created(tp: usize) -> Result<i32, isize> {
    crate::signal::thread_created(tp)
}

/// The thread [`thread_created`] made ready for `tp` did not start.
pub fn thread_not_started(tp: usize) {
    crate::own::end(tp);
    crate::signal::thread_not_started(tp)
}

/// Readies the calling thread, which [`thread_created`] made ready and
/// which has its thread pointer: it is in the layer for ever, and the
/// layer keeps its handle (`thread`), by which other threads interrupt it.
pub fn thread_ready(tid: i32, thread: runtime::object::Thread) {
    crate::interrupt::enter();
    crate::thread::keep(tid, thread);
}

/// The calling thread is ending: its id is free again, its handle and
/// signal state gone (with the signals pending for it alone), and what it
/// had of its own as a process of its own ([`process_apart`]).
pub fn thread_ending() {
    crate::own::end(crate::architecture::thread_pointer());
    crate::thread::forget_current();
    crate::signal::thread_exited();
}

/// Makes a process that is a copy of this one, as Linux's `fork` does (the
/// layer's part of it: its descriptors, signals, threads and services, as
/// for a C program): `Some` of its process id here, `None` in the copy,
/// whose one thread is the calling one. `hold` is called just before the
/// copy is made, and what it returns dropped just after, in both: it holds
/// still what the host keeps that the copy must find whole (its heap, last;
/// nothing may allocate meanwhile).
pub fn fork<H>(hold: impl FnOnce() -> H) -> Result<Option<i32>, isize> {
    match crate::fork::fork(hold)? {
        crate::fork::Forked::Parent(pid) => Ok(Some(pid)),
        crate::fork::Forked::Child => Ok(None),
    }
}

/// A process for a child `vfork` makes (`clone` with `CLONE_VM`), made and
/// not started: its id, which the child has from now on, and the process,
/// which the child's `execve` starts its program in ([`start_as_exec`]).
pub fn reserve_child() -> Result<(i32, runtime::object::Process), isize> {
    crate::process::reserve_child()
}

/// Makes the thread whose thread pointer is `tp`, about to start, a child
/// `vfork` made: a process of its own, `pid`, in this process's memory,
/// with copies of the calling thread's descriptors, working directory and
/// signal dispositions, but those it shares (`CLONE_FILES`, `CLONE_FS`,
/// `CLONE_SIGHAND`); it takes none of this process's signals.
pub fn process_apart(tp: usize, pid: i32, files: bool, fs: bool, handlers: bool) {
    crate::own::begin(tp, pid, files, fs, handlers);
}

/// The child `vfork` made, `pid`, ended with `code` (a Veda exit code)
/// before it started a program: it is this process's to wait for.
pub fn child_ended(pid: i32, code: i64) {
    crate::process::child_ended(pid, code);
}

/// The calling thread's id (the process's, for its main thread).
pub fn current_tid() -> i32 {
    crate::signal::current_tid()
}

/// This process's id.
pub fn pid() -> i32 {
    crate::process::pid()
}

/// Whether the calling thread has a signal to act on, after taking what
/// other processes sent if it put that off.
pub fn signals_ready() -> bool {
    crate::signal::has_ready()
}

/// The calling thread's signal mask.
pub fn mask() -> u64 {
    crate::signal::current_mask()
}

/// Ends the process with `code`.
pub fn exit(code: i32) -> ! {
    crate::process::exit(code)
}

/// Ends the process as signal `s` ends it.
pub fn exit_by_signal(s: u32) -> ! {
    crate::process::exit_by_signal(s)
}

/// The program's path for the name `name` relative to the directory
/// `dirfd` (`AT_FDCWD`: the working directory): absolute, normalised, in
/// the program's own tree (as `/proc` is named), not the VFS's.
pub fn program_path_at(dirfd: i32, name: &[u8]) -> Result<alloc::string::String, isize> {
    crate::vfs::program_path_at(dirfd, name)
}

/// The working directory, as the program names it.
pub fn cwd() -> alloc::string::String {
    crate::path::program_path(&crate::vfs::cwd())
}

/// What descriptor `fd` is open on, as Linux's `/proc/self/fd` link says
/// it: the program's path of a file or directory, `/dev/null`, the
/// terminal, or the kind and inode number of what has no path
/// (`pipe:[N]`, `socket:[N]`, `anon_inode:[eventfd]`). A file whose path
/// this process does not know (one it was given) is `/dev/fd/N`, which
/// names the same file.
pub fn fd_link(fd: i32) -> Result<alloc::string::String, isize> {
    use crate::fd::Object;
    use alloc::format;
    use alloc::string::String;
    let desc = crate::fd::get(fd)?;
    // The inode numbers are those `fstat` gives (`crate::fs::stat_desc`).
    Ok(match &desc.object {
        Object::File(f) => match f.path() {
            Some(path) => program_path(path),
            None => format!("/dev/fd/{fd}"),
        },
        Object::Dir(d) => program_path(&d.path),
        Object::Stream(s) if s.is_tty() => String::from("/dev/tty"),
        Object::Stream(s) => format!("pipe:[{}]", s.koid),
        Object::Inet(s) => format!("socket:[{}]", s.id()),
        Object::Unix(s) => format!("socket:[{}]", s.id()),
        // Linux names one by what its creator called it, which is not kept.
        Object::Memfd(_) => String::from("/memfd: (deleted)"),
        Object::EventFd(_) => String::from("anon_inode:[eventfd]"),
        Object::TimerFd(_) => String::from("anon_inode:[timerfd]"),
        Object::Epoll(_) => String::from("anon_inode:[eventpoll]"),
        Object::Pidfd(_) => String::from("anon_inode:[pidfd]"),
        Object::Null => String::from("/dev/null"),
        // The system's log, where Linux's would be its console.
        Object::Log(_) => String::from("/dev/console"),
    })
}

/// The file of the program `name` names, as `execve` runs it: its path
/// (the VFS's), its bytes, and the arguments that go before the program's
/// own for a script, its interpreter's.
pub fn find_program(name: &[u8]) -> Result<Program, isize> {
    crate::process::find_program(name)
}

/// The bytes of the file at the program's path `path`, and how many.
pub fn read_file(path: &str) -> Result<(runtime::object::Vmo, usize), isize> {
    let vfs = crate::path::vfs_path(path);
    let (vmo, size) = crate::error::ipc(crate::vfs::with(|c| c.read_file(vfs))?)?;
    Ok((vmo, usize::try_from(size).map_err(|_| crate::linux::errno::EFBIG)?))
}

/// Starts the program `name` names with arguments `args` and environment
/// `env`, as `execve` would replace this process with it (its descriptors
/// but those marked close-on-exec, its working directory, its signals
/// ignored and blocked; those of the calling thread, if it is a process
/// of its own): its process id, a child of this process's. `into` is the
/// process to start it in, if one was made for it ([`reserve_child`]).
pub fn start_as_exec(
    name: &[u8],
    args: alloc::vec::Vec<&[u8]>,
    env: alloc::vec::Vec<&[u8]>,
    into: Option<runtime::object::Process>,
) -> Result<i32, isize> {
    crate::process::start_as_exec(name, args, env, into)
}

/// What `execve` does to the program's descriptors and signals as it
/// replaces the program in place: the descriptors marked close-on-exec are
/// closed, and the signals handled go back to their default action (those
/// ignored stay ignored, the mask and what is pending stay).
pub fn exec_in_place() {
    for (n, _, cloexec) in crate::fd::snapshot() {
        if cloexec {
            let _ = crate::fd::remove(n);
        }
    }
    crate::signal::reset_handlers();
}

/// The DNS servers the network service uses, as text, for a resolver of
/// the program's own (`/etc/resolv.conf`).
pub fn dns_servers() -> alloc::vec::Vec<alloc::string::String> {
    use alloc::string::ToString;
    crate::network::with(|c| c.status())
        .map(|s| s.dns_servers.iter().map(|ip| ip.to_string()).collect())
        .unwrap_or_default()
}

/// Has the threads but the calling one end what they wait for in the
/// layer, as a signal would end it, while `on` (`execve`, which ends
/// them): each is interrupted, and finds itself to be so.
pub fn stop_others(on: bool) {
    crate::signal::set_stopping(on);
    if on {
        crate::thread::interrupt_all_but_current();
    }
}

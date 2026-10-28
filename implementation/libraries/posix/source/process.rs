// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Processes: ids, starting programs (`posix_spawn`), waiting for them,
//! `kill` and exit.
//!
//! There is no `fork`: Veda creates a process from a program image, and
//! gives it what `exec` keeps on Linux: the descriptors not marked
//! close-on-exec, as the file actions leave them, the working directory,
//! the signals ignored and the signal mask, as the attributes change them
//! (`inherit` says how what is not a handle travels), and a handle to this
//! process, with which it may signal it. A process id is the process's
//! kernel object id, cut to the 30 bits thread ids have ([`pid_of`]: the
//! main thread's id is its process's); the processes this one started are
//! its children, the only ones it can wait for, and, with the process that
//! started it, the only others it can signal. Cut so, ids repeat after
//! 2^30 kernel objects: a child never gets the id of another child this
//! one has not waited for (the process made for it is dropped before it
//! runs, and another made in its place).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use abi::startup::role;
use abi::{WaitItem, signals};
use runtime::object::{Handle, Process, Vmo};
use runtime::process::{Spawn, SpawnError};
use runtime::sync::Mutex;

use crate::error::{self, SysResult};
use crate::fd::{self, Description, Object};
use crate::inherit::{Inherited, Pseudo};
use crate::interrupt::Layer;
use crate::linux::errno::*;
use crate::linux::{Rusage, Siginfo, Timeval, o, sig, wait};
use crate::{fs, path, signal, tty, user, vfs};

/// The process id for a process's kernel object id (positive, below 2^30:
/// it is its main thread's id too, and musl keeps flags in the high bits of
/// words that hold thread ids).
pub fn pid_of(koid: u64) -> i32 {
    ((koid & 0x3fff_ffff) as i32).max(1)
}

static KOID: AtomicU64 = AtomicU64::new(0);

/// This process's kernel object id: what services know it by (0 if it
/// cannot be had).
pub fn koid() -> u64 {
    let k = KOID.load(Ordering::Relaxed);
    if k != 0 {
        return k;
    }
    let k = runtime::object::process_self_info().map_or(0, |i| i.koid);
    KOID.store(k, Ordering::Relaxed);
    k
}

/// This process's id.
pub fn pid() -> i32 {
    match koid() {
        0 => 1,
        k => pid_of(k),
    }
}

/// The id of the process that started this one.
pub fn ppid() -> i32 {
    runtime::object::process_self_info().map_or(0, |i| if i.parent_koid == 0 { 0 } else { pid_of(i.parent_koid) })
}

/// The processes this one started and has not waited for.
static CHILDREN: Mutex<BTreeMap<i32, Process>> = Mutex::new(BTreeMap::new());

/// The process that started this one, if it gave this one a handle to it
/// (a POSIX program's parent does), to signal it with.
static PARENT: Mutex<Option<Process>> = Mutex::new(None);

/// Keeps the handle to the process that started this one (`role::PARENT`).
pub fn adopt_parent(parent: Process) {
    *PARENT.lock() = Some(parent);
}

/// Whether `pid` is a child of this process not waited for yet.
pub fn is_child(pid: i32) -> bool {
    CHILDREN.lock().contains_key(&pid)
}

/// Ends the process with `code` (`exit`).
pub fn exit(code: i32) -> ! {
    crate::fd::flush_logs();
    runtime::sys::process_exit((code & 0xff) as i64)
}

/// Ends the process as killed by signal `s`.
pub fn exit_by_signal(s: u32) -> ! {
    crate::fd::flush_logs();
    runtime::sys::process_exit(abi::EXIT_CODE_SIGNALED - s as i64)
}

/// A wait status (`WIFEXITED`, `WIFSIGNALED`) for an exit code.
fn wait_status(code: i64) -> i32 {
    match abi::exit_signal(code) {
        Some(s) => s as i32,
        None => ((code & 0xff) as i32) << 8,
    }
}

// --- posix_spawn ---------------------------------------------------------

/// musl's `struct fdop` (a file action of `posix_spawn`).
#[repr(C)]
struct FdOp {
    next: *const FdOp,
    prev: *const FdOp,
    cmd: i32,
    fd: i32,
    srcfd: i32,
    oflag: i32,
    mode: u32,
    // The path follows: `char path[]`, right after `mode` (before the
    // padding that rounds the struct's size up).
}

/// Where the path of a file action starts.
const FDOP_PATH: usize = core::mem::offset_of!(FdOp, mode) + size_of::<u32>();

const FDOP_CLOSE: i32 = 1;
const FDOP_DUP2: i32 = 2;
const FDOP_OPEN: i32 = 3;
const FDOP_CHDIR: i32 = 4;
const FDOP_FCHDIR: i32 = 5;

/// musl's `posix_spawnattr_t`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SpawnAttr {
    flags: i32,
    pgrp: i32,
    /// The signals to have their default disposition (`SETSIGDEF`).
    default: [u64; 16],
    /// The signal mask to start with (`SETSIGMASK`).
    mask: [u64; 16],
    prio: i32,
    pol: i32,
    func: usize,
    pad: [u8; 56],
}

/// `POSIX_SPAWN_SETSIGDEF`: signals of [`SpawnAttr::default`] are not
/// ignored in the child.
const SETSIGDEF: i32 = 4;
/// `POSIX_SPAWN_SETSIGMASK`: the child's mask is [`SpawnAttr::mask`], not
/// the caller's.
const SETSIGMASK: i32 = 8;
// The other attributes change nothing on Veda: there are no process groups
// or sessions to put a process in (`SETPGROUP`, `SETSID`), one user
// (`RESETIDS`), and no scheduling policies (`SETSCHEDPARAM`,
// `SETSCHEDULER`, which musl does not apply on Linux either).

impl SpawnAttr {
    /// The signals a child ignores, and its mask, when the caller ignores
    /// `ignored` and has `mask` itself.
    fn signals(&self, ignored: u64, mask: u64) -> (u64, u64) {
        let ignored = if self.flags & SETSIGDEF != 0 { ignored & !self.default[0] } else { ignored };
        let mask = if self.flags & SETSIGMASK != 0 { self.mask[0] } else { mask };
        (ignored, mask)
    }
}

/// The descriptors and directory of a process about to start.
struct ChildState {
    fds: BTreeMap<i32, (Arc<Description>, bool)>,
    cwd: String,
}

impl ChildState {
    /// Applies one file action, as the child would before running.
    ///
    /// # Safety
    /// `op` must be one of musl's file actions.
    unsafe fn apply(&mut self, op: &FdOp) -> Result<(), isize> {
        // SAFETY: the path follows the action.
        let name = || unsafe { user::cstr(op as *const FdOp as usize + FDOP_PATH) };
        match op.cmd {
            FDOP_CLOSE => {
                self.fds.remove(&op.fd);
            }
            FDOP_DUP2 => {
                let (d, _) = self.fds.get(&op.srcfd).cloned().ok_or(EBADF)?;
                self.fds.insert(op.fd, (d, false));
            }
            FDOP_OPEN => {
                let r = path::resolve(&self.cwd, name()?)?;
                let d = fs::open_resolved(&r, op.oflag as u32)?;
                self.fds.insert(op.fd, (d, false));
            }
            FDOP_CHDIR => {
                let r = path::resolve(&self.cwd, name()?)?;
                if !vfs::stat(&r.path)?.is_dir {
                    return Err(ENOTDIR);
                }
                self.cwd = r.path;
            }
            FDOP_FCHDIR => {
                let (d, _) = self.fds.get(&op.fd).ok_or(EBADF)?;
                let Object::Dir(dir) = &d.object else { return Err(ENOTDIR) };
                self.cwd = dir.path.clone();
            }
            _ => return Err(EINVAL),
        }
        Ok(())
    }

    /// What carries the child's descriptors: handles, and what it inherits
    /// otherwise. A descriptor numbered beyond what a handle's role can say
    /// makes it fail (`EMFILE`) rather than go missing.
    fn handles(&self, inherited: &mut Inherited) -> Result<Vec<(u32, Handle)>, isize> {
        let mut out = Vec::new();
        for (&n, (d, cloexec)) in &self.fds {
            if *cloexec {
                continue;
            }
            if n < 0 || n as u32 >= role::MAX_FDS {
                return Err(EMFILE);
            }
            let h = match &d.object {
                // The access the descriptor has and no more: the read end of
                // a pipe cannot be written in the child either.
                Object::Stream(s) => s.shared_handle(d.readable(), d.writable())?,
                Object::File(f) => f.duplicate()?.into_handle(),
                // A socket or a memory file: the handles that carry it
                // (none, one, or a connection's two), and what makes them
                // what it is.
                Object::Unix(_) | Object::Inet(_) | Object::Memfd(_) => {
                    let Some(socket) = crate::carry::for_child(d)? else { continue };
                    inherited.carried.push((n, socket.entry));
                    let mut handles = socket.handles.into_iter();
                    let Some(first) = handles.next() else { continue };
                    if let Some(second) = handles.next() {
                        out.push((role::FD_EXTRA + n as u32, second));
                    }
                    first
                }
                // What no handle carries.
                Object::Dir(dir) => {
                    inherited.fds.push((n, Pseudo::Dir(dir.path.clone())));
                    continue;
                }
                Object::Null => {
                    inherited.fds.push((n, Pseudo::Null));
                    continue;
                }
                Object::Log(_) => {
                    inherited.fds.push((n, Pseudo::Log));
                    continue;
                }
                // An eventfd, a timer, an epoll instance and a child's
                // descriptor are the program's alone.
                Object::EventFd(_) | Object::TimerFd(_) | Object::Epoll(_) | Object::Pidfd(_) => continue,
            };
            out.push((role::FD + n as u32, h));
        }
        // The standard descriptors it is not given it starts without, as
        // they are here, not with those of a program nobody gave any.
        let given =
            |n: i32| out.iter().any(|&(r, _)| r == role::FD + n as u32) || inherited.fds.iter().any(|f| f.0 == n);
        inherited.closed = (0..=2).filter(|&n| !given(n)).collect();
        Ok(out)
    }
}

/// The `errno` for a failure to start a program.
fn spawn_errno(e: SpawnError) -> isize {
    match e {
        SpawnError::NotExecutable
        | SpawnError::BadElf(_)
        | SpawnError::Incompatible
        | SpawnError::OtherDynamicLinker => ENOEXEC,
        // As Linux says of a program whose dynamic linker is not there.
        SpawnError::NoDynamicLinker => ENOENT,
        SpawnError::TooLarge => E2BIG,
        SpawnError::Refused => EAGAIN,
        SpawnError::Kernel(abi::Error::NoMemory) => ENOMEM,
        SpawnError::Kernel(e) => error::kernel(e),
    }
}

/// What runs a program file: the path of the program that runs (the file
/// itself, or the interpreter of a script), its contents, and the
/// arguments a script's interpreter is given before the script's own.
struct Program {
    path: String,
    vmo: Vmo,
    size: usize,
    /// The arguments that replace the first one (none for a program that is
    /// no script).
    prefix: Vec<Vec<u8>>,
}

/// The executable file `name`, from the directory `cwd`: its absolute
/// path, or `EACCES` if it is a directory or no program.
fn executable(cwd: &str, name: &[u8]) -> Result<String, isize> {
    let path = path::resolve(cwd, name)?.path;
    let st = vfs::stat(&path)?;
    if st.is_dir || !st.executable {
        return Err(EACCES);
    }
    Ok(path)
}

/// What runs the executable file `name`, from the directory `cwd`: the
/// file, or for a script the interpreter its first line names (`#!`), as
/// Linux runs one: given its argument, if the line has one, then the
/// script as `name` names it, before the arguments that follow the
/// program's own name. An interpreter may be a script too,
/// `runtime::process::SCRIPT_DEPTH` deep (`ELOOP` beyond); a line that
/// names none, or does not end soon enough, is `ENOEXEC`. Each file is the
/// system image's pages, or a copy of its own that nothing else changes.
fn program(cwd: &str, name: &[u8]) -> Result<Program, isize> {
    let mut path = executable(cwd, name)?;
    let mut given = name.to_vec();
    let mut prefix: Vec<Vec<u8>> = Vec::new();
    let mut depth = 0;
    loop {
        let (vmo, size): (Vmo, u64) = error::ipc(vfs::with(|c| c.read_file(path.clone()))?)?;
        let size = usize::try_from(size).map_err(|_| ENOEXEC)?;
        let mut start = [0u8; runtime::process::SCRIPT_LINE_MAX];
        let start = &mut start[..size.min(runtime::process::SCRIPT_LINE_MAX)];
        vmo.read(0, start).map_err(error::kernel)?;
        if !start.starts_with(b"#!") {
            return Ok(Program { path, vmo, size, prefix });
        }
        if depth == runtime::process::SCRIPT_DEPTH {
            return Err(ELOOP);
        }
        depth += 1;
        let interpreter = runtime::process::interpreter(start).ok_or(ENOEXEC)?;
        // What came before stays after the script: the interpreter's
        // arguments go first, the script's name replacing the program's.
        let mut args = alloc::vec![interpreter.path.to_vec()];
        args.extend(interpreter.argument.map(<[u8]>::to_vec));
        args.push(core::mem::replace(&mut given, interpreter.path.to_vec()));
        args.extend(prefix.drain(1.min(prefix.len())..));
        prefix = args;
        path = executable(cwd, interpreter.path)?;
    }
}

/// Starts the program at `name` with the descriptors and working directory
/// left by the file actions `ops`, which are carried out first, as the
/// child would (a relative `name` is found from the directory they leave),
/// and the signal state the attributes `attr` leave. A script runs with its
/// interpreter ([`program`]); its process is named after the script, and
/// knows the interpreter's file as its own (`AT_EXECFN`).
///
/// # Safety
/// The arguments are those of musl's `posix_spawn`.
unsafe fn spawn(res: usize, name: usize, ops: usize, attr: usize, argv: usize, envp: usize) -> Result<(), isize> {
    // SAFETY: musl passes its attributes (zero if the program gave none).
    let attr: SpawnAttr = unsafe { user::read(attr)? };
    let (ignored, mask) = attr.signals(signal::ignored(), signal::current_mask());
    let mut inherited = Inherited { ignored, mask, nice: crate::sched::nice(), ..Inherited::default() };
    let mut child = ChildState {
        fds: fd::snapshot().into_iter().map(|(n, d, cloexec)| (n, (d, cloexec))).collect(),
        cwd: vfs::cwd(),
    };
    if ops != 0 {
        // The list is built backwards: start from its tail.
        let mut op = ops as *const FdOp;
        // SAFETY: musl's list of file actions.
        unsafe {
            while !(*op).next.is_null() {
                op = (*op).next;
            }
            while !op.is_null() {
                child.apply(&*op)?;
                op = (*op).prev;
            }
        }
    }
    // SAFETY: the program passed a path.
    let given = unsafe { user::cstr(name)? };
    let program = program(&child.cwd, given)?;

    let mut handles = child.handles(&mut inherited)?;
    // A registry connection of its own; the terminal's state; this process,
    // to signal it (`kill(getppid(), ...)`), and no more.
    let registry = proto::with_registry(|r| r.clone_registry()).map_err(|_| EIO)?;
    match registry {
        Ok(Ok(ch)) => handles.push((role::REGISTRY, ch.into_handle())),
        _ => return Err(EIO),
    }
    if let Some(vmo) = tty::vmo_for_child() {
        handles.push((role::TERMINAL, vmo.into_handle()));
    }
    if let Ok(me) = runtime::object::Process::current() {
        // Sent through the channel, it is to be transferable.
        let rights = abi::Rights(
            abi::Rights::SIGNAL.0 | abi::Rights::WAIT.0 | abi::Rights::GET_INFO.0 | abi::Rights::TRANSFER.0,
        );
        if let Ok(h) = me.0.duplicate(Some(rights)) {
            handles.push((role::PARENT, h));
        }
    }
    if handles.len() > abi::CHANNEL_MAX_HANDLES {
        return Err(EMFILE);
    }

    // SAFETY: the program passed NULL-terminated arrays.
    let (mut args, env) = unsafe { (user::cstr_array(argv), user::cstr_array(envp)) };
    if !program.prefix.is_empty() {
        let given: Vec<&[u8]> = args.drain(1.min(args.len())..).collect();
        args = program.prefix.iter().map(Vec::as_slice).chain(given).collect();
    }
    let name = path::resolve(&child.cwd, given)?.path;
    let name = path::file_name(&name);
    let name = &name[..name.floor_char_boundary(abi::NAME_MAX)];
    let entries = inherited.entries();
    let mut spawn = Spawn::new(name).path(&program.path).cwd(&child.cwd);
    spawn.args = args;
    spawn.env = env;
    spawn.handles = handles;
    spawn.inherited = entries.iter().map(String::as_str).collect();
    // Until it takes signals itself, another process's are as it inherits
    // them.
    spawn.signals_inherited = (ignored, mask);
    let unused = |koid: u64| !CHILDREN.lock().contains_key(&pid_of(koid));
    spawn.accept = Some(&unused);
    // A program linked dynamically starts with the system's dynamic
    // linker, which the system image's pages are.
    let linker = |path: &str| {
        let (vmo, size) = error::ipc(vfs::with(|c| c.read_file(path.into())).ok()?).ok()?;
        Some((vmo, usize::try_from(size).ok()?))
    };
    spawn.dynamic_linker = Some(&linker);
    let process = spawn.start_file(&program.vmo, program.size).map_err(spawn_errno)?;
    let pid = pid_of(process.0.koid());
    CHILDREN.lock().insert(pid, process);
    if res != 0 {
        // SAFETY: the program passed where to store the id.
        unsafe { user::write(res, pid)? };
    }
    Ok(())
}

/// musl's `posix_spawn` without the `PATH` search (see the C library's
/// `src/process/posix_spawn.c`). Returns 0 or an `errno` value.
///
/// # Safety
/// The arguments are those of `posix_spawn`, with `ops` the list of file
/// actions and `attr` never null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __veda_spawn(
    res: usize,
    path: usize,
    ops: usize,
    attr: usize,
    argv: usize,
    envp: usize,
) -> i32 {
    let _layer = Layer::enter();
    // SAFETY: per the caller.
    match unsafe { spawn(res, path, ops, attr, argv, envp) } {
        Ok(()) => 0,
        Err(e) => e as i32,
    }
}

// --- waiting ---------------------------------------------------------------

/// `pidfd_open`: a descriptor of child `pid`, readable once it has ended,
/// which `poll` and `epoll` can wait for (as tokio waits for its children).
/// Only a child has one: the other processes are not this one's to watch.
/// It is close-on-exec, as on Linux.
pub fn pidfd_open(pid: i32, flags: u32) -> SysResult {
    if flags & !o::NONBLOCK != 0 {
        return Err(EINVAL);
    }
    let child = CHILDREN.lock().get(&pid).map(|p| p.0.duplicate(None));
    let handle = child.ok_or(ESRCH)?.map_err(error::kernel)?;
    let desc = Description::new(Object::Pidfd(Process(handle)), o::RDWR | (flags & o::NONBLOCK));
    fd::insert(desc, true, 0).map(|n| n as usize)
}

/// `veda_child_ended_for_memory(pid, ended)` (`<veda/memory.h>`): writes to
/// `ended` 1 if child `pid` was ended by the system because memory ran out
/// (`abi::EXIT_CODE_OUT_OF_MEMORY`), and 0 if it ended otherwise or still
/// runs: which a wait status cannot say, as being ended for memory is
/// `SIGKILL` there, as Linux's OOM killer's is. Returns 0, ECHILD if `pid`
/// is no child of this process's not waited for yet, or EFAULT.
///
/// # Safety
/// `ended` has room for an int.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veda_child_ended_for_memory(pid: i32, ended: *mut core::ffi::c_int) -> core::ffi::c_int {
    let _layer = Layer::enter();
    if ended.is_null() {
        return EFAULT as core::ffi::c_int;
    }
    let state = CHILDREN.lock().get(&pid).map(|p| p.info().map(|i| i.state));
    match state {
        None => ECHILD as core::ffi::c_int,
        Some(Err(e)) => error::kernel(e) as core::ffi::c_int,
        Some(Ok(state)) => {
            // SAFETY: as the caller promises.
            unsafe { ended.write(core::ffi::c_int::from(state == abi::process_state::OUT_OF_MEMORY)) };
            0
        }
    }
}

/// The children `pid` selects (`wait4`'s convention: -1 or a process
/// group means any).
fn select_children(pid: i32) -> Result<Vec<(i32, Process)>, isize> {
    let children = CHILDREN.lock();
    let picked: Vec<(i32, Process)> = children
        .iter()
        .filter(|&(&p, _)| pid <= 0 || p == pid)
        .filter_map(|(&p, proc)| proc.0.duplicate(None).ok().map(|h| (p, Process(h))))
        .collect();
    if picked.is_empty() {
        return Err(ECHILD);
    }
    Ok(picked)
}

/// Waits for one of the children `pid` selects to end; returns its id and
/// exit code, or `None` if `nohang` and none has. A signal for the thread
/// ends the wait (`ERESTARTSYS`).
fn wait_child(pid: i32, nohang: bool, reap: bool) -> Result<Option<(i32, i64)>, isize> {
    let children = select_children(pid)?;
    let mut items: Vec<WaitItem> = children
        .iter()
        .map(|(_, p)| WaitItem { handle: p.raw(), signals: signals::TERMINATED, observed: 0, _reserved: 0 })
        .collect();
    let deadline = if nohang { 0 } else { abi::DEADLINE_INFINITE };
    match crate::interrupt::wait_many(&mut items, deadline) {
        Ok(_) => {}
        Err(abi::Error::TimedOut) => return Ok(None),
        Err(abi::Error::Interrupted) => return Err(ERESTARTSYS),
        Err(e) => return Err(error::kernel(e)),
    }
    let Some(i) = items.iter().position(|it| it.observed & signals::TERMINATED != 0) else { return Ok(None) };
    let (p, proc) = &children[i];
    let code = proc.info().map_err(error::kernel)?.exit_code;
    if reap {
        CHILDREN.lock().remove(p);
    }
    Ok(Some((*p, code)))
}

pub unsafe fn wait4(pid: i32, status: usize, options: u32, rusage: usize) -> SysResult {
    if options & !(wait::NOHANG | wait::UNTRACED | 8) != 0 {
        return Err(EINVAL);
    }
    let Some((p, code)) = wait_child(pid, options & wait::NOHANG != 0, true)? else { return Ok(0) };
    if status != 0 {
        // SAFETY: the program passed an int.
        unsafe { user::write(status, wait_status(code))? };
    }
    if rusage != 0 {
        // SAFETY: the program passed a struct rusage.
        unsafe { user::write(rusage, Rusage::default())? };
    }
    Ok(p as usize)
}

pub unsafe fn waitid(kind: u32, id: i32, info: usize, options: u32) -> SysResult {
    let pid = match kind {
        wait::P_ALL => -1,
        wait::P_PID => id,
        wait::P_PGID => -1,
        _ => return Err(EINVAL),
    };
    if options & wait::EXITED == 0 {
        return Err(EINVAL);
    }
    let reap = options & wait::NOWAIT == 0;
    let found = wait_child(pid, options & wait::NOHANG != 0, reap)?;
    let si = match found {
        None => Siginfo::default(),
        Some((p, code)) => {
            let (si_code, status) = match abi::exit_signal(code) {
                Some(s) => (wait::CLD_KILLED, s as i32),
                None => (wait::CLD_EXITED, (code & 0xff) as i32),
            };
            Siginfo { signo: sig::CHLD as i32, code: si_code, pid: p, uid: vfs::UID, status, ..Default::default() }
        }
    };
    if info != 0 {
        // SAFETY: the program passed a siginfo_t.
        unsafe { user::write(info, si)? };
    }
    Ok(0)
}

/// `kill`: this process, a child, the process that started this one, or
/// (`-1`) every child; signal 0 only checks that they exist. Another
/// process gets the signal by way of the kernel, which ends one that takes
/// no signals yet (it has not started, or is no POSIX program) if the
/// signal ends a process by default. Each process is the one member of its
/// process group (its id is the group's, `getpgid`), so `-pid` means
/// `pid`.
pub fn kill(pid: i32, s: u32) -> SysResult {
    if s >= sig::NSIG {
        return Err(EINVAL);
    }
    let me = self::pid();
    if pid == 0 || pid == me || pid == -me {
        return signal::send_self(s);
    }
    let send = |p: &Process| if s == 0 { Ok(()) } else { p.signal(s as usize, 0).map_err(error::kernel) };
    if pid.wrapping_abs() == ppid() {
        let parent = PARENT.lock();
        return match parent.as_ref() {
            Some(p) => send(p).map(|()| 0),
            None => Err(EPERM),
        };
    }
    let children = CHILDREN.lock();
    let targets: Vec<&Process> =
        if pid == -1 { children.values().collect() } else { children.get(&pid.wrapping_abs()).into_iter().collect() };
    if targets.is_empty() {
        return Err(ESRCH);
    }
    for p in targets {
        send(p)?;
    }
    Ok(0)
}

/// `getrusage`: CPU time of this process (`RUSAGE_SELF`) or none.
pub unsafe fn getrusage(who: i32, buf: usize) -> SysResult {
    let mut r = Rusage::default();
    if who == 0 {
        let ns = runtime::object::process_self_info().map_or(0, |i| i.cpu_time_ns);
        r.utime = Timeval { sec: (ns / 1_000_000_000) as i64, usec: ((ns % 1_000_000_000) / 1000) as i64 };
    }
    // SAFETY: the program passed a struct rusage.
    unsafe { user::write(buf, r)? };
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_actions_are_laid_out_as_musl_has_them() {
        // musl's struct fdop: two pointers, five ints, then `char path[]`.
        assert_eq!(FDOP_PATH, 36);
        assert_eq!(size_of::<FdOp>(), 40);
    }

    #[test]
    fn spawn_attributes_are_laid_out_as_musl_has_them() {
        use core::mem::offset_of;
        // Two ints, two 128-byte sigset_ts, two ints, a pointer and its
        // padding to 64 bytes.
        assert_eq!(offset_of!(SpawnAttr, default), 8);
        assert_eq!(offset_of!(SpawnAttr, mask), 136);
        assert_eq!(offset_of!(SpawnAttr, func), 272);
        assert_eq!(size_of::<SpawnAttr>(), 336);
    }

    #[test]
    fn children_inherit_signals_as_the_attributes_say() {
        let pipe = 1 << (sig::PIPE - 1);
        let chld = 1 << (sig::CHLD - 1);
        let none =
            SpawnAttr { flags: 0, pgrp: 0, default: [0; 16], mask: [0; 16], prio: 0, pol: 0, func: 0, pad: [0; 56] };
        // Without attributes: what the caller ignores and blocks.
        assert_eq!(none.signals(pipe | chld, chld), (pipe | chld, chld));
        // Some back to their default, another mask.
        let mut attr = none;
        attr.flags = SETSIGDEF | SETSIGMASK;
        attr.default[0] = pipe;
        attr.mask[0] = pipe;
        assert_eq!(attr.signals(pipe | chld, chld), (chld, pipe));
        // Set, but not asked for: no effect.
        attr.flags = 0;
        assert_eq!(attr.signals(pipe, 0), (pipe, 0));
    }

    #[test]
    fn wait_statuses() {
        // WIFEXITED, WEXITSTATUS
        assert_eq!(wait_status(0), 0);
        assert_eq!(wait_status(3), 3 << 8);
        assert_eq!(wait_status(256 + 7), 7 << 8);
        // WIFSIGNALED, WTERMSIG
        assert_eq!(wait_status(abi::EXIT_CODE_SIGNALED - 6), 6);
        assert_eq!(wait_status(abi::EXIT_CODE_CRASHED), 11);
        assert_eq!(wait_status(abi::EXIT_CODE_KILLED), 9);
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `execve`: the program replaced by another.
//!
//! A Linux program is loaded in place, as Linux does it: the process, its
//! id and its descriptors stay (but those marked close-on-exec), its other
//! threads end, its memory and its signals' handlers go, and the new
//! program starts with its arguments and environment ([`execve`]). What
//! could fail is checked first (the file, its format, its dynamic linker),
//! so that a call that fails leaves the program as it was; once the old
//! program is gone, a failure ends the process, as on Linux.
//!
//! Veda's own programs (and its POSIX programs) run in a process of their
//! own, with what `execve` would give them, which this process waits for
//! and ends as, standing in for it: a parent that waits for this process
//! has what it ended with.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, Ordering};

use elf::{Elf, Kind};
use runtime::object::Process;

use crate::linux::errno::*;
use crate::loader::{self, File};
use crate::syscall::Outcome;
use crate::task::{self, Task};
use crate::{architecture, memory, namespace, proc, user};

/// The most bytes of arguments and environment `execve` takes (Linux's
/// limit for an 8 MiB stack).
const ARG_MAX: usize = 2 << 20;

/// The thread that replaces the program, while it ends the others (0: none
/// does).
static REPLACING: AtomicI32 = AtomicI32::new(0);

/// In a process `fork` made, whose one thread is the calling one: no other
/// is replacing the program.
pub fn forked() {
    REPLACING.store(0, Ordering::Release);
}

/// Whether the thread `tid` is to end, another replacing the program.
pub fn stopping_others(tid: i32) -> bool {
    let by = REPLACING.load(Ordering::Acquire);
    by != 0 && by != tid
}

/// `execve(path, argv, envp)`, `execveat` with the directory `dirfd`.
pub fn execve(t: &mut Task, dirfd: i32, path: usize, argv: usize, envp: usize, flags: u32) -> Outcome {
    match replace(t, dirfd, path, argv, envp, flags) {
        Ok(()) => Outcome::Done,
        Err(e) => Outcome::Return(-e),
    }
}

fn replace(t: &mut Task, dirfd: i32, path: usize, argv: usize, envp: usize, flags: u32) -> Result<(), isize> {
    if flags & !(crate::linux::at::SYMLINK_NOFOLLOW | crate::linux::at::EMPTY_PATH) != 0 {
        return Err(EINVAL);
    }
    // Everything is copied before anything goes: the old program's memory
    // holds it.
    // SAFETY: the program's memory, which a fault reading it ends.
    let (name, args, env) = unsafe {
        (
            user::string(path, 4095).ok_or(EFAULT)?,
            user::strings(argv, ARG_MAX).ok_or(E2BIG)?,
            user::strings(envp, ARG_MAX).ok_or(E2BIG)?,
        )
    };
    if name.is_empty() {
        return Err(ENOENT);
    }
    let named = posix::host::program_path_at(dirfd, &name)?;
    let program = posix::host::find_program(named.as_bytes())?;
    // A script runs with its interpreter, given the script's name.
    let prefix = program.prefix;
    let args: Vec<Vec<u8>> =
        if prefix.is_empty() { args } else { prefix.into_iter().chain(args.into_iter().skip(1)).collect() };
    let file = File { vmo: program.vmo, len: program.size, path: namespace::to_program(&program.path) };
    let kind = loader::with_image(&file, |image| Ok(Elf::parse(image).map_err(|_| ENOEXEC)?.kind()))?;
    let args: Vec<&[u8]> = args.iter().map(Vec::as_slice).collect();
    let env: Vec<&[u8]> = env.iter().map(Vec::as_slice).collect();
    // A child `vfork` made starts its program in the process made for it,
    // whose id it has, with its own descriptors and directory: its parent's
    // program stays.
    if let Some(apart) = &t.apart {
        let into = Process(apart.process.0.duplicate(None).map_err(|_| ENOMEM)?);
        posix::host::start_as_exec(named.as_bytes(), args, env, Some(into))?;
        task::replaced(t);
    }
    match kind {
        Kind::Linux => in_place(t, file, &args, &env),
        Kind::Veda | Kind::Posix => stand_in(t, named.as_bytes(), args, env),
        Kind::OtherVeda => Err(ENOEXEC),
    }
}

/// Replaces the Linux program with `file`, a Linux program too.
fn in_place(t: &mut Task, file: File, args: &[&[u8]], env: &[&[u8]]) -> Result<(), isize> {
    let linker = loader::with_image(&file, loader::check)?;
    end_others(t)?;
    // The point of no return: the old program goes.
    memory::clear();
    posix::host::exec_in_place();
    t.altstack = Default::default();
    t.robust_list = (0, 0);
    t.restart = None;
    t.fault = None;
    let _ = runtime::sys::call(abi::nr::THREAD_SET_EXIT_FUTEX, [0; 6]);
    crate::name_thread(&file.path);
    match loader::load(&file, linker.as_ref(), args, env) {
        Ok(loaded) => {
            proc::started(proc::Started {
                args: args.iter().map(|a| a.to_vec()).collect(),
                env: env.iter().map(|e| e.to_vec()).collect(),
                auxv: loaded.auxv,
                exe: file.path,
            });
            t.state = architecture::start(loaded.entry, loaded.sp);
            Ok(())
        }
        Err(e) => {
            runtime::println!("{}: {} could not be loaded ({e}): the process ends", crate::process_name(), file.path);
            posix::host::exit_by_signal(crate::linux::sig::SEGV)
        }
    }
}

/// Ends the program's other threads, as `execve` does: each ends as it
/// next comes back from its code or a call (which ends as a signal ends
/// it). `EAGAIN` if one does not in a few seconds (it is in a call that
/// does not end).
fn end_others(t: &Task) -> Result<(), isize> {
    if task::live() == 1 {
        return Ok(());
    }
    REPLACING.store(t.tid, Ordering::Release);
    task::set_stopping(true);
    posix::host::stop_others(true);
    let deadline = runtime::time::now_ns() + 5_000_000_000;
    while task::live() > 1 && runtime::time::now_ns() < deadline {
        runtime::time::sleep(runtime::time::Duration::from_millis(1));
    }
    let ended = task::live() == 1;
    posix::host::stop_others(false);
    task::set_stopping(false);
    REPLACING.store(0, Ordering::Release);
    task::reap();
    if ended { Ok(()) } else { Err(EAGAIN) }
}

/// Runs a program of Veda's in this process's place: started with what
/// `execve` gives, as this process's child, which this process waits for,
/// then ends as it ended.
fn stand_in(t: &mut Task, name: &[u8], args: Vec<&[u8]>, env: Vec<&[u8]>) -> Result<(), isize> {
    end_others(t)?;
    let pid = posix::host::start_as_exec(name, args, env, None)?;
    let mut status = 0i32;
    loop {
        let wait4 = linux_raw_sys::general::__NR_wait4 as usize;
        // SAFETY: a status of our own.
        match unsafe { posix::host::syscall(wait4, [pid as usize, &raw mut status as usize, 0, 0, 0, 0]) } {
            Ok(_) => break,
            Err(e) if e == EINTR || e == crate::linux::errno::ERESTARTSYS => continue,
            Err(_) => posix::host::exit(127),
        }
    }
    // As it ended: with its code, or by its signal.
    if status & 0x7f == 0 {
        posix::host::exit((status >> 8) & 0xff)
    }
    posix::host::exit_by_signal((status & 0x7f) as u32)
}

const E2BIG: isize = linux_raw_sys::errno::E2BIG as isize;

/// The program's path for the VFS's `path`, for messages.
pub fn shown(path: &str) -> String {
    namespace::to_program(path)
}

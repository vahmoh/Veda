// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `linux-compatibility` — Linux programs, as they were built for Linux,
//! on Veda.
//!
//! A Linux program runs in a process of its own, as any program does,
//! with this program beside it: started in its place
//! (`runtime::process::Spawn`, for any executable that is not Veda's own),
//! it loads the Linux program and its dynamic linker into the process
//! ([`loader`]) and runs its threads in foreign mode (`abi`'s "Foreign
//! code"). The kernel carries out none of the program's system calls: each
//! comes back to this program, which carries it out as Linux would, with
//! the process's handles and Veda's services, as the POSIX layer does for
//! Veda's C programs, whose Linux system calls it is
//! (`posix::host`). So a Linux program can do nothing its process could
//! not, and is confined, limited and identified as any program is.
//!
//! What is Linux's own, beyond what the POSIX layer does for C programs:
//!
//! | Linux | Here |
//! |-------|------|
//! | `execve` of a Linux program | loaded in place ([`loader`], [`exec`]) |
//! | its threads (`clone`), their thread pointers, robust futexes | kernel threads that run its code ([`task`]) |
//! | signal handlers and their frames, `sigaltstack`, faults' signals | Linux's frame on its stack, `rt_sigreturn` ([`signal`], `architecture`) |
//! | its memory's areas, `brk`, `mremap` | kept apart from this program's own ([`memory`]) |
//! | the vDSO, which reads the time without a system call (and where ARM64's handlers return) | an image of Linux's, and the clocks' scale ([`vdso`]) |
//! | `/proc`, `/sys`'s processors, `/etc/resolv.conf` | made as they are opened ([`proc`]) |
//! | its tree: `/lib`, `/usr`, `/etc` | a Linux root, the user's files Veda's own ([`namespace`]) |
//! | `uname`, `personality`, `prctl`, capabilities | Linux's answers, those of a program without privileges ([`syscall`]) |
//!
//! This program touches none of the vector registers (it is built for the
//! architecture's soft-float target), which are the Linux program's: the
//! kernel need not switch them between the two.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(target_arch = "x86_64")]
#[path = "architectures/amd64/mod.rs"]
pub mod architecture;
#[cfg(target_arch = "aarch64")]
#[path = "architectures/arm64/mod.rs"]
pub mod architecture;
// ARM64's signal frame, faults' signals and `/proc/cpuinfo`, which are
// plain data: the host's tests check them, whatever the host is.
#[cfg(all(test, not(target_arch = "aarch64")))]
#[path = "architectures/arm64/cpu.rs"]
mod arm64_cpu;
#[cfg(all(test, not(target_arch = "aarch64")))]
#[path = "architectures/arm64/fault.rs"]
mod arm64_fault;
#[cfg(all(test, not(target_arch = "aarch64")))]
#[path = "architectures/arm64/frame.rs"]
mod arm64_frame;
pub mod exec;
pub mod linux;
pub mod loader;
pub mod memory;
pub mod namespace;
pub mod proc;
pub mod signal;
pub mod syscall;
pub mod task;
pub mod user;
pub mod vdso;

use alloc::string::String;
use alloc::vec::Vec;

use abi::startup::{linux::LaunchView, role};
use runtime::object::Vmo;

use crate::loader::File;

/// The Linux release `uname` says the system is: one recent enough for what
/// programs check (the C libraries' oldest kernels, the system calls they
/// try first), of Veda's.
pub const RELEASE: &str = "6.6.0-veda";

/// The program's name, for the log.
pub fn process_name() -> String {
    runtime::object::process_self_info()
        .map(|i| String::from(i.name()))
        .unwrap_or_else(|_| String::from("linux-compatibility"))
}

/// Names the calling thread after the program at `path`, as Linux names a
/// program's threads (`/proc/self/comm`).
pub fn name_thread(path: &str) {
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut bytes: Vec<u8> = name.bytes().take(15).collect();
    bytes.push(0);
    const SET_NAME: usize = 15;
    // SAFETY: our own name.
    let _ = unsafe { posix::host::syscall(linux::nr::PRCTL, [SET_NAME, bytes.as_ptr() as usize, 0, 0, 0, 0]) };
}

/// What starts the program: its file, path, arguments and environment, as
/// the one that started it gave them (`abi::startup::linux`), or as this
/// program's own arguments name them (`linux-compatibility PROGRAM
/// ARGUMENTS...`, run by hand).
struct Launch {
    file: Vmo,
    len: usize,
    /// The VFS's path of the program.
    path: String,
    args: Vec<Vec<u8>>,
    env: Vec<Vec<u8>>,
}

fn launch() -> Result<Launch, String> {
    if let (Some(program), Some(launch)) =
        (runtime::env::take_handle(role::PROGRAM), runtime::env::take_handle(role::LAUNCH))
    {
        let launch = Vmo::from_handle(launch);
        let size = launch.size().map_err(|e| alloc::format!("its launch: {e}"))?;
        let size = size.min(abi::startup::linux::MAX_LEN);
        let mut bytes = alloc::vec![0u8; size];
        launch.read(0, &mut bytes).map_err(|e| alloc::format!("its launch: {e}"))?;
        let view = LaunchView::parse(&bytes).map_err(|e| alloc::format!("its launch: {e:?}"))?;
        return Ok(Launch {
            file: Vmo::from_handle(program),
            len: usize::try_from(view.file_len).map_err(|_| String::from("its file is too large"))?,
            path: String::from_utf8_lossy(view.path()).into_owned(),
            args: view.args().map(<[u8]>::to_vec).collect(),
            env: view.env().map(<[u8]>::to_vec).collect(),
        });
    }
    // By hand: the program and its arguments after this one's name.
    let args = runtime::env::args();
    let Some(name) = args.get(1) else { return Err(String::from("usage: linux-compatibility PROGRAM [ARGUMENT]...")) };
    let program = posix::host::find_program(name.as_bytes()).map_err(|e| alloc::format!("{name}: error {e}"))?;
    Ok(Launch {
        file: program.vmo,
        len: program.size,
        path: program.path,
        args: args[1..].iter().map(|a| a.as_bytes().to_vec()).collect(),
        env: runtime::env::vars().into_iter().map(|(k, v)| alloc::format!("{k}={v}").into_bytes()).collect(),
    })
}

/// The Linux root the program's tree is (`crate::namespace`): its
/// environment's `VEDA_LINUX_ROOT`, or the default, if it is a directory.
fn linux_root(env: &[Vec<u8>]) -> String {
    let prefix = [namespace::ROOT_VARIABLE, b"="].concat();
    if let Some(named) = env.iter().find_map(|e| e.strip_prefix(prefix.as_slice())) {
        return String::from_utf8_lossy(named).into_owned();
    }
    let path = alloc::ffi::CString::new(namespace::DEFAULT_ROOT).unwrap_or_default();
    let mut st = [0u8; 256];
    // SAFETY: our own path and buffer.
    let found = unsafe {
        posix::host::syscall(
            linux::nr::NEWFSTATAT,
            [linux::at::FDCWD as usize, path.as_ptr() as usize, st.as_mut_ptr() as usize, 0, 0, 0],
        )
    };
    if found.is_ok() { String::from(namespace::DEFAULT_ROOT) } else { String::new() }
}

/// Runs the Linux program this process was started for; never returns.
pub fn start() -> ! {
    let main = task::main_thread();
    posix::host::start();
    architecture::fpu::init();
    let launch = match launch() {
        Ok(l) => l,
        Err(why) => {
            runtime::println!("linux-compatibility: {why}");
            posix::host::exit(127);
        }
    };
    namespace::set_root(&linux_root(&launch.env));
    let file = File { vmo: launch.file, len: launch.len, path: namespace::to_program(&launch.path) };
    let args: Vec<&[u8]> = launch.args.iter().map(Vec::as_slice).collect();
    let env: Vec<&[u8]> = launch.env.iter().map(Vec::as_slice).collect();
    let loaded =
        loader::with_image(&file, loader::check).and_then(|linker| loader::load(&file, linker.as_ref(), &args, &env));
    let loaded = match loaded {
        Ok(l) => l,
        Err(e) => {
            // As a shell says of a program it cannot run.
            runtime::println!("{}: {} cannot run: error {e}", process_name(), file.path);
            posix::host::exit(if e == linux::errno::ENOENT { 127 } else { 126 });
        }
    };
    name_thread(&file.path);
    proc::started(proc::Started {
        args: launch.args.clone(),
        env: launch.env.clone(),
        auxv: loaded.auxv,
        exe: file.path,
    });
    main.state = architecture::start(loaded.entry, loaded.sp);
    main.tid = posix::host::pid();
    task::run(main)
}

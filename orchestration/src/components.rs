// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Cross-building the individual Veda components with cargo.
//!
//! * The UEFI loader is built for the architecture's UEFI target
//!   (`arch::UEFI_TARGET`), whose programs are PE/COFF images, as the
//!   firmware starts them.
//! * The kernel is built for its freestanding target
//!   (`arch::KERNEL_TARGET`): an ELF64 executable, which its `build.rs`
//!   links at the kernel's address.
//! * User-space programs are `no_std` position-independent ELF64
//!   executables built for its user target (`arch::USER_TARGET`), linked by
//!   LLVM's linker, which Rust ships (`rust-lld`; Cargo's configuration
//!   names it). Each program's `build.rs` (via the `vbuild` helper crate)
//!   supplies the Veda-specific linker options.
//! * Programs on Rust's standard library are built for Veda's own target
//!   (`arch::STD_TARGET`) by the toolchain's Rust, and linked by its C
//!   toolchain's driver with the C library, whose POSIX layer is brought up
//!   to date first (`toolchain::std_rust`). They are POSIX programs: the C
//!   library starts them.
//!
//! Every build names the build machine's directories by fixed names in
//! what it compiles (`util::cargo_for`), so that none of them goes into the
//! image.

use std::path::PathBuf;

use crate::arch::{KERNEL_TARGET, STD_TARGET, UEFI_TARGET, USER_TARGET};
use crate::toolchain;
use crate::util::{self, Result};

/// The linker of the kernel and of user-space programs (and of the loader,
/// in its Microsoft flavour).
pub const USER_LINKER: &str = "rust-lld";

/// A user-space program and where it is installed in the initrd.
#[derive(Debug, Clone, Copy)]
pub struct Program {
    /// Cargo package name.
    pub package: &'static str,
    /// Binary (executable) name produced by the package.
    pub binary: &'static str,
    /// Built on Rust's standard library, for Veda's own target; otherwise
    /// `no_std`, on `vrt`, for the user target.
    pub std: bool,
}

impl Program {
    /// A program on `vrt` alone (`no_std`).
    const fn native(package: &'static str) -> Program {
        Program { package, binary: package, std: false }
    }

    /// A program on Rust's standard library.
    const fn std(package: &'static str) -> Program {
        Program { package, binary: package, std: true }
    }
}

/// Every user-space program that ships in the initrd, installed as
/// `bin/<binary>`.
pub const PROGRAMS: &[Program] = &[
    Program::native("init"),
    Program::native("vfs"),
    Program::native("systest"),
    Program::native("nettest"),
    Program::native("testmic"),
    Program::native("audiotest"),
    Program::native("fakedec"),
    Program::std("stdcrates"),
    Program::native("devmgr"),
    Program::native("drivervm"),
    Program::native("virtio-blk"),
    Program::native("nvme"),
    Program::native("netd"),
    Program::native("wlan"),
    Program::native("compositor"),
    Program::std("about"),
    Program::std("interceptor"),
    Program::native("audio"),
    Program::std("agent"),
    Program::std("music"),
    Program::std("videos"),
    Program::std("photos"),
    Program::std("terminal"),
    Program::std("taskmgr"),
    Program::std("files"),
    Program::std("settings"),
    Program::std("editor"),
    Program::std("calculator"),
    Program::std("paint"),
    Program::std("shell"),
    Program::std("lock"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Debug,
    Release,
}

impl Profile {
    fn cargo_flag(self) -> &'static [&'static str] {
        match self {
            Profile::Debug => &[],
            Profile::Release => &["--release"],
        }
    }

    pub fn dir(self) -> &'static str {
        match self {
            Profile::Debug => "debug",
            Profile::Release => "release",
        }
    }
}

/// Paths of everything produced by [`build_all`].
pub struct Artifacts {
    pub bootloader: PathBuf,
    pub kernel: PathBuf,
    pub programs: Vec<(Program, PathBuf)>,
}

fn target_dir() -> PathBuf {
    util::target_dir()
}

/// Builds `package` for `target`; returns the path of its executable, `file`.
fn build(package: &str, target: &str, profile: Profile, file: &str) -> Result<PathBuf> {
    util::status("Building", format!("{package} ({target})"));
    util::run(
        util::cargo_for(target).args(["build", "--package", package, "--target", target]).args(profile.cargo_flag()),
    )?;
    Ok(target_dir().join(target).join(profile.dir()).join(file))
}

/// Builds every program except those named in `skip`.
fn build_programs(profile: Profile, skip: &[String]) -> Result<Vec<(Program, PathBuf)>> {
    let programs: Vec<Program> = PROGRAMS.iter().copied().filter(|p| !skip.iter().any(|s| s == p.package)).collect();
    let (on_std, native): (Vec<Program>, Vec<Program>) = programs.into_iter().partition(|p| p.std);
    let mut built = Vec::new();
    if !native.is_empty() {
        util::status("Building", format!("{} user-space programs ({USER_TARGET})", native.len()));
        let mut cmd = util::cargo_for(USER_TARGET);
        cmd.args(["build", "--target", USER_TARGET]).args(profile.cargo_flag());
        for p in &native {
            cmd.args(["--package", p.package]);
        }
        util::run(&mut cmd)?;
        let dir = target_dir().join(USER_TARGET).join(profile.dir());
        built.extend(native.iter().map(|p| (*p, dir.join(p.binary))));
    }
    if !on_std.is_empty() {
        built.extend(build_std_programs(&on_std, profile)?);
    }
    Ok(built)
}

/// Builds programs on Rust's standard library, for Veda's own target, with
/// the toolchain's Rust (which `cargo orchestration toolchain` builds).
fn build_std_programs(programs: &[Program], profile: Profile) -> Result<Vec<(Program, PathBuf)>> {
    let (_lock, rust) = toolchain::std_rust()?;
    let dir = toolchain::std_target_dir().join(STD_TARGET).join(profile.dir());
    // Linked again when the C library's POSIX layer has changed.
    toolchain::forget_old_links(&dir, &programs.iter().map(|p| p.binary).collect::<Vec<_>>())?;
    util::status("Building", format!("{} programs on the standard library ({STD_TARGET})", programs.len()));
    let mut cmd = rust.cargo_for_programs();
    cmd.args(["build", "--target", STD_TARGET]).args(profile.cargo_flag());
    for p in programs {
        cmd.args(["--package", p.package]);
    }
    util::run(&mut cmd)?;
    Ok(programs.iter().map(|p| (*p, dir.join(p.binary))).collect())
}

/// Builds the loader, the kernel and the user-space programs (all but `skip`).
pub fn build_all(profile: Profile, skip: &[String]) -> Result<Artifacts> {
    let bootloader = build("vboot", UEFI_TARGET, profile, "vboot.efi")?;
    let kernel = build("vkernel", KERNEL_TARGET, profile, "vkernel")?;
    let programs = build_programs(profile, skip)?;
    Ok(Artifacts { bootloader, kernel, programs })
}

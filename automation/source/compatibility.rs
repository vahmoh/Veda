// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Linux programs that check Veda's Linux compatibility
//! (`verification/linux`), built for Linux by Linux's own toolchains, for
//! `systest` to run inside Veda (`/system/tests/linux`):
//!
//! | Program | Built by | Linked |
//! |---------|----------|--------|
//! | `checks-musl` | the driver VM's toolchain for Linux on musl (`cargo automation linux`) | statically |
//! | `checks-musl-dynamic` | the same, with musl's dynamic linker built from the same source | dynamically, its root `musl` |
//! | `checks-glibc-static` | this machine's GCC and glibc, where they are the architecture's | statically |
//! | `checks-glibc` | the same | dynamically, its root `glibc`: this machine's dynamic linker and C library |
//! | `checks-rust` | Rust, for Linux on musl as Rust ships it | statically |
//!
//! A dynamically linked one runs with the Linux root beside it
//! (`VEDA_LINUX_ROOT`), which holds its dynamic linker and C library where
//! it looks for them. They are Linux's programs for the architecture Veda
//! is built for, x86-64's or AArch64's, which Linux compatibility runs:
//! the driver VM's toolchain is that architecture's, and Rust builds for
//! it on any machine (linked by LLVM's linker, which Rust ships, where the
//! machine is another's). This machine's glibc is the architecture's only
//! on a machine of it.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::architecture;
use crate::util::{self, Recipe, Result};

/// Whether the image has the Linux programs on musl, which the GUI
/// scripts run: the driver VM's toolchain builds them.
pub fn built() -> bool {
    musl_gcc().exists()
}

/// Linux's name for the architecture's processor (`uname`'s machine):
/// what musl's dynamic linker, GCC's target for glibc and Rust's for musl
/// are named after (`x86_64`, `aarch64`).
fn machine() -> &'static str {
    std::str::from_utf8(architecture::current().machine.platform).expect("a processor's name")
}

/// The driver VM's C compiler, for Linux on musl.
fn musl_gcc() -> PathBuf {
    crate::linux::root().join("toolchain").join("bin").join(format!("{}-gcc", architecture::current().guest_c_target))
}

/// Files for the image: each one's path and bytes.
type Files = Vec<(String, Vec<u8>)>;

/// Where the tests are built, for the architecture.
fn out() -> PathBuf {
    util::target_dir().join("linux-compatibility").join(architecture::current().name)
}

/// The files of `/system/tests/linux` (relative to `/system`): the test
/// programs and their roots, those that can be built here.
pub fn image_files() -> Result<Files> {
    let out = out();
    util::create_dir_all(&out)?;
    let source = util::workspace_root().join("verification").join("linux");
    let mut files = Vec::new();
    match musl(&out, &source.join("checks.c"))? {
        Some(built) => files.extend(built),
        None => util::status("Note", "no Linux programs on musl in the tests: the driver VM's toolchain is not built"),
    }
    match glibc(&out, &source.join("checks.c"))? {
        Some(built) => files.extend(built),
        None => util::status(
            "Note",
            format!(
                "no Linux programs on glibc in the tests: this machine's GCC has no glibc for {} to link",
                machine()
            ),
        ),
    }
    files.extend(rust(&out, &source.join("checks.rs"))?);
    Ok(files.into_iter().map(|(name, data)| (format!("tests/linux/{name}"), data)).collect())
}

/// Compiles `cc` into `exe` from `inputs`, unless it is made already.
fn compile(mut cc: Command, exe: &Path, inputs: &[&Path]) -> Result<Vec<u8>> {
    cc.arg("-o").arg(exe);
    let recipe = Recipe::of(exe, &cc, inputs);
    if !recipe.fresh() {
        util::run(&mut cc)?;
        recipe.record()?;
    }
    util::read(exe)
}

/// The C flags of every test program: as Veda's own C tests are built.
fn c_flags(cc: &mut Command) {
    cc.args(["-O2", "-Wall", "-Wextra", "-Werror", "-g0", "-s", "-pthread"]).arg(util::c_path_remap());
}

/// The programs on musl, by the driver VM's toolchain, and musl's dynamic
/// linker for the one linked dynamically, built from the source that
/// toolchain's C library was built from.
fn musl(out: &Path, source: &Path) -> Result<Option<Files>> {
    let gcc = musl_gcc();
    if !gcc.exists() {
        return Ok(None);
    }
    let mut files = Vec::new();
    let mut cc = Command::new(&gcc);
    c_flags(&mut cc);
    cc.arg("-static").arg(source);
    files.push(("checks-musl".to_string(), compile(cc, &out.join("checks-musl"), &[source, &gcc])?));
    // musl's shared library, which is its dynamic linker too.
    let musl = crate::linux::root().join("src").join("musl");
    let build = out.join("musl");
    let library = build.join("lib").join("libc.so");
    let recipe = Recipe::new(&library, &["musl shared".into()], &[&gcc, &musl.join("VERSION")]);
    if !recipe.fresh() {
        util::status("Building", "musl's dynamic linker, for the Linux tests");
        let _ = std::fs::remove_dir_all(&build);
        util::create_dir_all(&build)?;
        let mut configure = Command::new(musl.join("configure"));
        configure.current_dir(&build).env("CC", &gcc).args(["--prefix=/", "--disable-static"]);
        util::run(&mut configure)?;
        let jobs = std::thread::available_parallelism().map_or(4, |n| n.get());
        util::run(Command::new("make").current_dir(&build).arg(format!("-j{jobs}")).arg("lib/libc.so"))?;
        recipe.record()?;
    }
    let linker = format!("/lib/ld-musl-{}.so.1", machine());
    let mut cc = Command::new(&gcc);
    c_flags(&mut cc);
    cc.arg("-L").arg(build.join("lib")).arg(format!("-Wl,-dynamic-linker,{linker}")).arg(source);
    let exe = out.join("checks-musl-dynamic");
    files.push(("checks-musl-dynamic".to_string(), compile(cc, &exe, &[source, &gcc, &library])?));
    files.push((format!("musl{linker}"), util::read(&library)?));
    Ok(Some(files))
}

/// Where a Linux system keeps glibc's terms: Debian's and Ubuntu's, then
/// Fedora's.
const GLIBC_TERMS: [&str; 2] = ["/usr/share/doc/libc6/copyright", "/usr/share/licenses/glibc/COPYING.LIB"];

/// The programs on glibc, by this machine's GCC, and its dynamic linker and
/// C library for the one linked dynamically, with glibc's licence. None if
/// this machine's GCC is not glibc's for the architecture, or cannot link
/// statically.
fn glibc(out: &Path, source: &Path) -> Result<Option<Files>> {
    let triple = format!("{}-linux-gnu", machine());
    let target = Command::new("gcc").arg("-dumpmachine").output().ok();
    let target = target.filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if target.as_deref() != Some(triple.as_str()) {
        return Ok(None);
    }
    // glibc's terms, which go with it.
    let Some(terms) = GLIBC_TERMS.iter().map(Path::new).find(|p| p.exists()) else { return Ok(None) };
    let gcc = PathBuf::from("gcc");
    let mut files = vec![("licences/glibc/copyright".to_string(), util::read(terms)?)];
    let mut cc = Command::new(&gcc);
    c_flags(&mut cc);
    cc.arg("-static").arg(source);
    match compile(cc, &out.join("checks-glibc-static"), &[source]) {
        Ok(exe) => files.push(("checks-glibc-static".to_string(), exe)),
        // No static C library (libc6-dev without libc.a): none of glibc's.
        Err(_) => return Ok(None),
    }
    let mut cc = Command::new(&gcc);
    c_flags(&mut cc);
    cc.arg(source);
    let exe = compile(cc, &out.join("checks-glibc"), &[source])?;
    // Its dynamic linker, where it names it, and its C library, where the
    // dynamic linker looks for it.
    let elf = elf::Elf::parse_for(&exe, architecture::current().machine).map_err(|e| format!("checks-glibc: {e}"))?;
    let interpreter = elf.interpreter().map(|p| String::from_utf8_lossy(p).into_owned());
    let interpreter = interpreter.ok_or("checks-glibc: no dynamic linker")?;
    let libc = Command::new(&gcc).arg("-print-file-name=libc.so.6").output().map_err(|e| format!("gcc: {e}"))?;
    let libc = PathBuf::from(String::from_utf8_lossy(&libc.stdout).trim());
    let libc = libc.canonicalize().map_err(|e| format!("{}: {e}", libc.display()))?;
    let relative = |p: &Path| p.to_string_lossy().trim_start_matches('/').to_string();
    files.push(("checks-glibc".to_string(), exe));
    files.push((format!("glibc/{}", relative(Path::new(&interpreter))), util::read(Path::new(&interpreter))?));
    // Where Debian's and Ubuntu's dynamic linkers look first.
    files.push((format!("glibc/lib/{triple}/libc.so.6"), util::read(&libc)?));
    Ok(Some(files))
}

/// The program on Rust's standard library, for Linux on musl as Rust ships
/// it (linked statically, with the C library Rust ships): by this
/// machine's C compiler, or for another machine's processor by LLVM's
/// linker, which Rust ships too.
fn rust(out: &Path, source: &Path) -> Result<Files> {
    let exe = out.join("checks-rust");
    let mut rustc = Command::new("rustc");
    let target = format!("{}-unknown-linux-musl", machine());
    rustc.args(["--edition", "2024", "-O", "--target", &target, "-C", "strip=symbols"]);
    if architecture::current().machine != elf::NATIVE {
        rustc.args(["-C", "linker=rust-lld"]);
    }
    // No path of this machine's in what panics say.
    rustc.arg(format!("--remap-path-prefix={}=/veda", util::workspace_root().display()));
    rustc.arg(source);
    Ok(vec![("checks-rust".to_string(), compile(rustc, &exe, &[source])?)])
}

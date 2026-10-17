// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The toolchain: GCC, binutils and musl for Veda, the media libraries, and
//! Rust.
//!
//! `cargo orchestration toolchain` builds, from the sources in
//! `implementation/ports/`:
//!
//! * a **cross toolchain** that runs on this machine and makes Veda
//!   programs (`<target>-gcc` and friends, for the architecture's target,
//!   `arch::C_TARGET`, with a sysroot laid out like Veda's `/system`), used
//!   to build the C tests and ports;
//! * the **native toolchain**, the same compiler built to run inside Veda,
//!   which the system image installs in `/system` (`gcc`, `as`, `ld`, the C
//!   library and its headers);
//! * the **media libraries**, dav1d and FFmpeg built with the cross
//!   toolchain, which `mediadec` (the decoder of Videos) is linked with;
//! * **Rust**: its compiler, which runs on this machine, and its standard
//!   library for this machine and for Veda's own target
//!   (`arch::STD_TARGET`), where it stands on the C library (see
//!   `documentation/RUST.md`), with its Clippy and its standard library's
//!   source; it builds Veda's programs on the standard library
//!   ([`std_rust`]) and the Rust test programs.
//!
//! Everything lives under `target/toolchain` (`$VEDA_TOOLCHAIN` overrides
//! it): `downloads/` (the verified source archives), `src/` (unpacked and
//! patched), `build/`, `cross/`, `native/`, `media/` and `rust/`.
//! Everything is built with `implementation/ports/build.sh`.
//!
//! An image build makes things with the toolchain too (the C library's
//! POSIX layer, the test programs, the media decoder), into the
//! toolchain's directory, which builds at once share: it holds the
//! toolchain's lock meanwhile, as `cargo orchestration toolchain` does
//! while it builds. What it makes is made again when the command that
//! made it changes, not only its inputs (each output's `util::Recipe`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch::{self, BARE_TARGET};
use crate::ports::{self, Port};
use crate::util::{self, Recipe, Result};

/// Where the toolchain is built.
pub fn root() -> PathBuf {
    util::env_dir("VEDA_TOOLCHAIN", &["target", "toolchain"])
}

/// The lock over the toolchain's directory.
fn lock() -> Result<util::Lock> {
    util::Lock::exclusive(&root().join(".lock"), "the toolchain")
}

/// Whether the `cross toolchain`, the `native toolchain`, the `media
/// libraries` or the `rust toolchain` has been built.
pub fn built(what: &str) -> bool {
    match what {
        "cross toolchain" => cross_gcc().is_some(),
        "media libraries" => cross_gcc().is_some() && media_libraries().is_some(),
        "rust toolchain" => cross_gcc().is_some() && rustc().is_some(),
        _ => root().join("native").join("system").join("bin").join("gcc").is_file(),
    }
}

/// For `cargo orchestration doctor`: whether the toolchain, which is
/// optional, has been built (`Ok`), and if not, whether it can be (`Err`).
pub fn status() -> std::result::Result<String, String> {
    if built("native toolchain") && built("cross toolchain") {
        return Ok(format!("built ({})", root().display()));
    }
    let how = match shell() {
        Ok(_) => "`cargo orchestration toolchain` builds it".to_string(),
        Err(e) => format!("`cargo orchestration toolchain` builds it, but {e}"),
    };
    Err(format!("not built (optional: C and GCC in the image); {how}"))
}

/// Where Rust is installed, as its releases install.
pub fn rust_root() -> PathBuf {
    root().join("rust")
}

/// Rust's compiler, if it has been built with the standard library for
/// Veda's target.
pub fn rustc() -> Option<PathBuf> {
    let rustc = rust_root().join("bin").join("rustc");
    let std = rust_root().join("lib").join("rustlib").join(arch::STD_TARGET).join("lib");
    (rustc.is_file() && std.is_dir()).then_some(rustc)
}

/// What builds Veda's programs on Rust's standard library: the toolchain's
/// compiler, and the C toolchain's driver, which links them with the C
/// library.
pub struct StdRust {
    rustc: PathBuf,
    linker: PathBuf,
}

impl StdRust {
    /// Cargo for Veda's own target (`arch::STD_TARGET`) with the toolchain's
    /// compiler (build scripts and procedural macros too) and linker, in
    /// [`std_target_dir`], the build machine's paths remapped as for Veda's
    /// other programs.
    pub fn cargo_for_programs(&self) -> Command {
        let mut cmd = util::cargo_for(arch::STD_TARGET);
        cmd.env("RUSTC", &self.rustc)
            .env(cargo_target_env(arch::STD_TARGET, "LINKER"), &self.linker)
            .env("CARGO_TARGET_DIR", std_target_dir());
        c_compilers(&mut cmd);
        cmd
    }

    /// `cargo clippy` with the toolchain's Clippy (`cargo-clippy` runs
    /// Cargo with its driver in place of the compiler), for Veda's own
    /// target, which the build machine's Clippy does not know.
    pub fn cargo_clippy(&self) -> Result<Command> {
        let clippy = self.rustc.with_file_name("cargo-clippy");
        if !clippy.is_file() {
            return Err(format!(
                "the toolchain has no Clippy ({}): `cargo orchestration toolchain` builds it again with it",
                clippy.display()
            ));
        }
        let mut cmd = Command::new(clippy);
        cmd.current_dir(util::workspace_root())
            .arg("clippy")
            .env("RUSTC", &self.rustc)
            .env(cargo_target_env(arch::STD_TARGET, "LINKER"), &self.linker)
            .env("CARGO_TARGET_DIR", std_target_dir());
        c_compilers(&mut cmd);
        Ok(cmd)
    }

    /// What tells one build of the toolchain's Rust from another: its
    /// compiler's and its standard library's files for Veda (names, sizes,
    /// times), which a build of the same release does not otherwise change.
    fn fingerprint(&self) -> Result<String> {
        let libs = rust_root().join("lib").join("rustlib").join(arch::STD_TARGET).join("lib");
        let mut files = util::files_under(&libs)?;
        files.push(self.rustc.clone());
        files.sort();
        let mut print = String::new();
        for f in files {
            let meta = std::fs::metadata(&f).map_err(|e| format!("{}: {e}", f.display()))?;
            let time = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
            print.push_str(&format!("{} {} {}\n", f.display(), meta.len(), time.map_or(0, |t| t.as_nanos())));
        }
        Ok(print)
    }
}

/// Where Veda's programs on the standard library are built: a target
/// directory of their own, as what the toolchain's compiler builds for this
/// machine (build scripts, procedural macros) is its own too.
pub fn std_target_dir() -> PathBuf {
    util::target_dir().join("std")
}

/// Empties [`std_target_dir`] if the toolchain's Rust has been built again
/// since it was filled: crates compiled against another build of the
/// standard library do not link with this one, and Cargo, which knows a
/// compiler by its version alone, would keep them.
fn forget_other_builds(rust: &StdRust) -> Result {
    let dir = std_target_dir();
    let note = dir.join("toolchain-fingerprint");
    let now = rust.fingerprint()?;
    if std::fs::read_to_string(&note).is_ok_and(|built| built == now) {
        return Ok(());
    }
    if dir.exists() {
        util::status("Removing", format!("{}: built with another build of the toolchain's Rust", dir.display()));
        std::fs::remove_dir_all(&dir).map_err(|e| format!("removing {}: {e}", dir.display()))?;
    }
    util::create_dir_all(&dir)?;
    util::write_atomically(&note, &now)
}

/// Removes `binaries`, programs on the standard library linked in
/// `profile_dir`, if they were linked with another POSIX layer than the C
/// library has now, so that Cargo links them again with it: it does not see
/// the C library change. What it linked is `deps/<binary>-<hash>`, which
/// the binary is a copy of: without it, Cargo builds the program again (but
/// not the crates the program depends on). A note in `profile_dir` says
/// which layer they were linked with.
pub fn forget_old_links(profile_dir: &Path, binaries: &[&str]) -> Result {
    let note = profile_dir.join("posix-layer");
    let object = posix_layer_object();
    let meta = std::fs::metadata(&object).map_err(|e| format!("{}: {e}", object.display()))?;
    let time = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
    let now = format!("{} {}\n", meta.len(), time.map_or(0, |t| t.as_nanos()));
    if std::fs::read_to_string(&note).is_ok_and(|linked| linked == now) {
        return Ok(());
    }
    let linked = |name: &str| -> bool {
        // `<binary>-<hash>`, the crate's name as rustc has it.
        name.rsplit_once('-').is_some_and(|(stem, hash)| {
            hash.len() == 16
                && hash.bytes().all(|b| b.is_ascii_hexdigit())
                && binaries.iter().any(|b| b.replace('-', "_") == stem)
        })
    };
    let deps = profile_dir.join("deps");
    let mut gone: Vec<PathBuf> = binaries.iter().map(|b| profile_dir.join(b)).collect();
    if let Ok(entries) = std::fs::read_dir(&deps) {
        for entry in entries.flatten() {
            if entry.file_name().to_str().is_some_and(linked) {
                gone.push(entry.path());
            }
        }
    }
    for path in gone {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("removing {}: {e}", path.display())),
        }
    }
    util::create_dir_all(profile_dir)?;
    util::write_atomically(&note, &now)
}

/// Names the C toolchain's compilers and archiver for Veda's own target
/// where the `cc` crate looks for them (`CC_<target>` and the others), so
/// that a crate that compiles C or C++ for it compiles them for Veda.
fn c_compilers(cmd: &mut Command) {
    let target = arch::STD_TARGET.replace('-', "_");
    for (variable, tool) in [("CC", "gcc"), ("CXX", "g++"), ("AR", "ar")] {
        cmd.env(format!("{variable}_{target}"), cross_tool(tool));
    }
}

/// Cargo's environment variable for a setting of `target`'s
/// (`CARGO_TARGET_<TRIPLE>_<SETTING>`).
fn cargo_target_env(target: &str, setting: &str) -> String {
    format!("CARGO_TARGET_{}_{setting}", target.to_uppercase().replace(['-', '.'], "_"))
}

/// The toolchain's Rust, for Veda's programs on the standard library; without
/// it, an error that says how to build it.
pub fn std_rust_tools() -> Result<StdRust> {
    match (rustc(), cross_gcc()) {
        (Some(rustc), Some(linker)) => Ok(StdRust { rustc, linker }),
        _ => Err(format!(
            "Veda's programs on Rust's standard library are built with the toolchain's Rust, which is not built \
             ({}): build it once with `cargo orchestration toolchain` (about half an hour; documentation/RUST.md)",
            rust_root().display()
        )),
    }
}

/// The toolchain's Rust for building Veda's programs on the standard
/// library, with the C library's POSIX layer, which they link, up to date;
/// the toolchain's lock is held until the build has done with it.
pub fn std_rust() -> Result<(util::Lock, StdRust)> {
    let rust = std_rust_tools()?;
    let lock = lock()?;
    check_ports();
    refresh_posix_layer()?;
    forget_other_builds(&rust)?;
    Ok((lock, rust))
}

/// For `cargo orchestration doctor`: whether Rust for Veda's own target and
/// the C toolchain that links its programs have been built (`Ok`), as
/// Veda's programs on the standard library need, and if not, whether they
/// can be (`Err`).
pub fn rust_status() -> std::result::Result<String, String> {
    if let (Some(rustc), Some(_)) = (rustc(), cross_gcc()) {
        let version = Port::named("rust").map(|p| p.version).unwrap_or_default();
        let clippy = if rustc.with_file_name("clippy-driver").is_file() {
            ""
        } else {
            ", without Clippy: `cargo orchestration toolchain` builds it again with it"
        };
        return Ok(format!("Rust {version} for {} ({}){clippy}", arch::STD_TARGET, rust_root().display()));
    }
    let how = match shell() {
        Ok(_) => "`cargo orchestration toolchain` builds it (about half an hour)".to_string(),
        Err(e) => format!("`cargo orchestration toolchain` builds it, but {e}"),
    };
    Err(format!("not built (Veda's programs on Rust's standard library need it); {how}"))
}

/// Where the media libraries (dav1d, FFmpeg) and their tests are.
pub fn media_root() -> PathBuf {
    root().join("media")
}

/// The media libraries' static libraries, in link order, if all are built.
pub fn media_libraries() -> Option<Vec<PathBuf>> {
    let lib = |port: &str, name: &str| media_root().join(port).join("system").join("lib").join(format!("lib{name}.a"));
    let libs = vec![
        lib("ffmpeg", "avformat"),
        lib("ffmpeg", "avcodec"),
        lib("ffmpeg", "swscale"),
        lib("ffmpeg", "swresample"),
        lib("ffmpeg", "avutil"),
        lib("dav1d", "dav1d"),
    ];
    libs.iter().all(|l| l.is_file()).then_some(libs)
}

/// For `cargo orchestration doctor`: whether the media libraries, which
/// videos need, have been built.
pub fn media_status() -> std::result::Result<String, String> {
    if media_libraries().is_some() {
        let version = |p: &str| Port::named(p).map(|p| p.version).unwrap_or_default();
        return Ok(format!("FFmpeg {} and dav1d {} ({})", version("ffmpeg"), version("dav1d"), media_root().display()));
    }
    let how = match shell() {
        Ok(_) => "`cargo orchestration toolchain` builds them".to_string(),
        Err(e) => format!("`cargo orchestration toolchain` builds them, but {e}"),
    };
    Err(format!("not built (optional: videos play with them); {how}"))
}

/// The cross compiler, if it has been built.
pub fn cross_gcc() -> Option<PathBuf> {
    let gcc = root().join("cross").join("bin").join(format!("{}-gcc", arch::C_TARGET));
    gcc.is_file().then_some(gcc)
}

/// A tool of the cross toolchain (`ld`, `objcopy`, `ar`, ...).
fn cross_tool(name: &str) -> PathBuf {
    root().join("cross").join("bin").join(format!("{}-{name}", arch::C_TARGET))
}

/// The symbols the POSIX layer gives the C library (and, for the `veda_`
/// ones, C programs: `<veda/ipc.h>`); all else in it stays private.
const POSIX_LAYER_EXPORTS: [&str; 25] = [
    "__veda_syscall",
    "__veda_init",
    "__veda_spawn",
    "__veda_lookup_name",
    "__veda_dns_servers",
    "__veda_clone",
    "__veda_set_thread_area",
    "__veda_unmapself",
    "__veda_bootstrap",
    "veda_close",
    "veda_duplicate",
    "veda_startup_handle",
    "veda_registry",
    "veda_service_register",
    "veda_service_accept",
    "veda_service_connect",
    "veda_channel_write",
    "veda_channel_read",
    "veda_wait",
    "veda_now_ns",
    "veda_vmo_create",
    "veda_vmo_map",
    "veda_vmo_unmap",
    "veda_event_create",
    "veda_object_signal",
];

/// Builds the POSIX layer (`implementation/libraries/posix`) as a static
/// library for the architecture's bare target; returns its path.
pub fn posix_library() -> Result<PathBuf> {
    util::status("Building", format!("the POSIX layer ({BARE_TARGET})"));
    util::run(util::cargo_for(BARE_TARGET).args([
        "rustc",
        "--package",
        "posix",
        "--target",
        BARE_TARGET,
        "--release",
        "--crate-type",
        "staticlib",
    ]))?;
    Ok(util::target_dir().join(BARE_TARGET).join("release").join("libposix.a"))
}

/// Links the POSIX layer's static library into the one relocatable object
/// the C library holds, which exports [`POSIX_LAYER_EXPORTS`] and nothing
/// else (no Rust or compiler-builtins symbol can clash with the C
/// library's), unless it is made already. `implementation/ports/build.sh`
/// does the same for the first build.
fn posix_object(lib: &Path, out: &Path) -> Result {
    isolated_object(lib, out, &POSIX_LAYER_EXPORTS)
}

/// Links a Rust static library into one relocatable object that defines
/// `exports` and nothing else globally: its Rust runtime, compiler builtins
/// (`memcpy` ...) and allocator stay its own, beside the C library's.
/// Symbols it uses but does not define stay global, for the final link.
/// Made again when the library, the tools or the exports change.
fn isolated_object(lib: &Path, out: &Path, exports: &[&str]) -> Result {
    let mut words: Vec<OsString> = vec!["isolated object".into()];
    words.extend(exports.iter().map(OsString::from));
    let (ld, objcopy) = (cross_tool("ld"), cross_tool("objcopy"));
    let recipe = Recipe::new(out, &words, &[lib, &ld, &objcopy]);
    if recipe.fresh() {
        return Ok(());
    }
    let whole = out.with_extension("all.o");
    let mut ld = Command::new(&ld);
    ld.args(["-r", "--gc-sections"]);
    for s in exports {
        ld.args(["-u", s]);
    }
    util::run(ld.arg("-o").arg(&whole).arg(lib))?;
    let mut objcopy = Command::new(&objcopy);
    objcopy.args(["--strip-debug", "--remove-section=.llvmbc", "--remove-section=.llvmcmd"]);
    for s in exports {
        objcopy.arg(format!("--keep-global-symbol={s}"));
    }
    util::run(objcopy.arg(&whole).arg(out))?;
    util::remove_file(&whole)?;
    recipe.record()
}

/// The C library's object of the POSIX layer, as last made.
fn posix_layer_object() -> PathBuf {
    root().join("build").join("veda.o")
}

/// Puts the current POSIX layer into the cross toolchain's C library (and
/// the native toolchain's, if built), so that C programs built from now
/// on get it.
fn refresh_posix_layer() -> Result {
    let object = posix_layer_object();
    let library = posix_library()?;
    util::create_dir_all(&root().join("build"))?;
    posix_object(&library, &object)?;
    let headers =
        util::workspace_root().join("implementation").join("libraries").join("posix").join("include").join("veda");
    for sysroot in [root().join("cross").join("sysroot"), root().join("native")] {
        let libc = sysroot.join("system").join("lib").join("libc.a");
        if libc.is_file() && !is_newer(&libc, &[&object]) {
            util::run(Command::new(cross_tool("ar")).arg("rcs").arg(&libc).arg(&object))?;
        }
        // Its own header, `<veda/ipc.h>`.
        if libc.is_file() {
            let dir = sysroot.join("system").join("include").join("veda");
            util::create_dir_all(&dir)?;
            for header in util::files_under(&headers)? {
                let to = dir.join(header.file_name().unwrap_or_default());
                if !is_newer(&to, &[&header]) {
                    util::copy(&header, &to)?;
                }
            }
        }
    }
    Ok(())
}

/// How each C test program is linked: once at a fixed address, as GCC links
/// by default, and once position-independent. The suffix of the
/// executable's name, and the flags.
const C_TEST_VARIANTS: &[(&str, &[&str])] = &[("", &[]), ("-pie", &["-fPIE", "-static-pie"])];

/// Compiles the test programs in C and C++ (`verification/c/*.c` and
/// `*.cc`, each in every one of [`C_TEST_VARIANTS`]) for the system image:
/// `(name, executable)`. None without a cross compiler. What they expect of
/// the architecture is in the headers of
/// `verification/c/architectures/<architecture>/`.
fn c_tests() -> Result<Vec<(String, Vec<u8>)>> {
    let Some(gcc) = cross_gcc() else { return Ok(Vec::new()) };
    let gxx = cross_tool("g++");
    let libc = root().join("cross").join("sysroot").join("system").join("lib").join("libc.a");
    let src = util::workspace_root().join("verification").join("c");
    let arch_headers = src.join("architectures").join(arch::NAME);
    let headers = util::files_under(&arch_headers)?;
    let out = root().join("tests");
    util::create_dir_all(&out)?;
    let sources = util::files_in(&src, &["c", "cc"])?;
    let mut programs = Vec::new();
    for source in sources {
        let stem = source.file_stem().unwrap().to_string_lossy().into_owned();
        let compiler = if source.extension().is_some_and(|x| x == "cc") { &gxx } else { &gcc };
        for (suffix, flags) in C_TEST_VARIANTS {
            let name = format!("{stem}{suffix}");
            let exe = out.join(&name);
            let inputs: Vec<&Path> =
                [source.as_path(), compiler, &libc].into_iter().chain(headers.iter().map(PathBuf::as_path)).collect();
            let mut cc = Command::new(compiler);
            cc.args(["-O2", "-Wall", "-Wextra", "-Werror", "-g0", "-s"])
                .arg(util::c_path_remap())
                .arg("-I")
                .arg(&arch_headers)
                .args(*flags)
                .arg("-o")
                .arg(&exe)
                .arg(&source);
            let recipe = Recipe::of(&exe, &cc, &inputs);
            if !recipe.fresh() {
                util::run(&mut cc)?;
                recipe.record()?;
            }
            programs.push((name, util::read(&exe)?));
        }
    }
    Ok(programs)
}

/// How each Rust test program is linked: position-independent, as Rust's
/// target for Veda links programs by default, and at a fixed address. The
/// suffix of the executable's name, and the flags.
const RUST_TEST_VARIANTS: &[(&str, &[&str])] = &[("", &[]), ("-fixed", &["-C", "relocation-model=static"])];

/// Compiles the test programs in Rust (`verification/rust/*.rs`, each in
/// every one of [`RUST_TEST_VARIANTS`]) for the system image: `(name,
/// executable)`. None without Rust. They keep their symbols and line
/// tables, which their backtraces show.
fn rust_tests() -> Result<Vec<(String, Vec<u8>)>> {
    let (Some(rustc), Some(gcc)) = (rustc(), cross_gcc()) else { return Ok(Vec::new()) };
    let libc = root().join("cross").join("sysroot").join("system").join("lib").join("libc.a");
    let src = util::workspace_root().join("verification").join("rust");
    let out = root().join("tests").join("rust");
    util::create_dir_all(&out)?;
    let sources = util::files_in(&src, &["rs"])?;
    let mut programs = Vec::new();
    for source in sources {
        let stem = source.file_stem().unwrap().to_string_lossy().into_owned();
        for (suffix, flags) in RUST_TEST_VARIANTS {
            let name = format!("{stem}{suffix}");
            let exe = out.join(&name);
            let mut compile = Command::new(&rustc);
            compile
                .args(["--edition", "2024", "--target", arch::STD_TARGET, "-D", "warnings"])
                .args(["-C", "opt-level=2", "-C", "debuginfo=line-tables-only"])
                .arg(rust_path_remap())
                .arg("-C")
                .arg(format!("linker={}", gcc.display()))
                .args(*flags)
                .arg("-o")
                .arg(&exe)
                .arg(&source);
            let recipe = Recipe::of(&exe, &compile, &[&source, &rustc, &gcc, &libc]);
            if !recipe.fresh() {
                util::run(&mut compile)?;
                recipe.record()?;
            }
            programs.push((name, util::read(&exe)?));
        }
    }
    Ok(programs)
}

/// What rustc is given for the test programs' paths: the checkout's by
/// the fixed name the build gives it (`util::path_remaps`), so that their
/// panics name no directory of this machine's.
fn rust_path_remap() -> OsString {
    let mut flag = OsString::from("--remap-path-prefix=");
    flag.push(util::workspace_root());
    flag.push("=/veda");
    flag
}

/// The flags the media decoder's shim is compiled with.
const SHIM_CFLAGS: &[&str] =
    &["-std=gnu17", "-O2", "-fPIE", "-Wall", "-Wextra", "-Werror", "-fstack-protector-strong", "-g0"];

/// C source without its comments (`/* */` and `//`), each replaced by a
/// space.
fn without_c_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let block = rest.find("/*");
        let line = rest.find("//");
        match (block, line) {
            (Some(b), l) if l.is_none_or(|l| b < l) => {
                out.push_str(&rest[..b]);
                out.push(' ');
                rest = rest[b + 2..].find("*/").map_or("", |e| &rest[b + 2 + e + 2..]);
            }
            (_, Some(l)) => {
                out.push_str(&rest[..l]);
                out.push(' ');
                rest = rest[l..].find('\n').map_or("", |e| &rest[l + e..]);
            }
            _ => {
                out.push_str(rest);
                return out;
            }
        }
    }
}

/// The media decoder (`implementation/services/mediadec`) for the system
/// image, if the cross toolchain and the media libraries are built: its
/// Rust half (built for the architecture's bare target and made one object
/// exporting `mediadec_main` alone), its C shim, FFmpeg and dav1d, linked
/// by the cross GCC as a static position-independent program. Built again
/// when any of it changed.
fn mediadec() -> Result<Option<Vec<u8>>> {
    let (Some(gcc), Some(libraries)) = (cross_gcc(), media_libraries()) else { return Ok(None) };
    let crate_dir = util::workspace_root().join("implementation").join("services").join("mediadec");
    let shim = crate_dir.join("shim");
    let header = shim.join("vmd.h");
    // No floating point crosses the shim: the Rust half passes it otherwise.
    let text = util::read_to_string(&header)?;
    let code = without_c_comments(&text);
    if code.split(|c: char| !c.is_alphanumeric() && c != '_').any(|w| w == "float" || w == "double") {
        return Err(format!("{}: no `float` or `double` may cross the shim", header.display()));
    }
    util::status("Building", format!("the media decoder ({BARE_TARGET} and C)"));
    util::run(util::cargo_for(BARE_TARGET).args([
        "rustc",
        "--package",
        "mediadec",
        "--target",
        BARE_TARGET,
        "--release",
        "--crate-type",
        "staticlib",
    ]))?;
    let rust_lib = util::target_dir().join(BARE_TARGET).join("release").join("libmediadec.a");
    let out = root().join("build").join("mediadec");
    util::create_dir_all(&out)?;
    let rust_obj = out.join("mediadec-rs.o");
    {
        isolated_object(&rust_lib, &rust_obj, &["mediadec_main"])?;
        // What it defines globally must be that and nothing else.
        let nm = Command::new(cross_tool("nm"))
            .args(["-g", "--defined-only", "--format=just-symbols"])
            .arg(&rust_obj)
            .output()
            .map_err(|e| format!("running nm: {e}"))?;
        let defined: Vec<String> = String::from_utf8_lossy(&nm.stdout).lines().map(str::to_string).collect();
        if defined != ["mediadec_main"] {
            util::remove_file(&rust_obj)?;
            return Err(format!("the media decoder's Rust half exports more than mediadec_main: {defined:?}"));
        }
    }
    let media = media_root();
    let includes =
        [media.join("ffmpeg").join("system").join("include"), media.join("dav1d").join("system").join("include")];
    let libc = root().join("cross").join("sysroot").join("system").join("lib").join("libc.a");
    let sources = util::files_in(&shim, &["c"])?;
    let headers: Vec<PathBuf> = ["vmd.h", "vmd_internal.h"].iter().map(|h| shim.join(h)).collect();
    let mut objects = Vec::new();
    for source in &sources {
        let object = out.join(source.file_name().unwrap()).with_extension("o");
        let mut inputs: Vec<&Path> = vec![source, &gcc];
        inputs.extend(headers.iter().map(PathBuf::as_path));
        inputs.extend(libraries.iter().map(PathBuf::as_path));
        let mut cc = Command::new(&gcc);
        cc.args(SHIM_CFLAGS).arg(util::c_path_remap());
        for i in &includes {
            cc.arg("-I").arg(i);
        }
        cc.arg("-c").arg("-o").arg(&object).arg(source);
        let recipe = Recipe::of(&object, &cc, &inputs);
        if !recipe.fresh() {
            util::run(&mut cc)?;
            recipe.record()?;
        }
        objects.push(object);
    }
    let exe = out.join("mediadec");
    let unstripped = out.join("mediadec.debug");
    let mut inputs: Vec<&Path> = vec![&rust_obj, &libc, &gcc];
    inputs.extend(objects.iter().map(PathBuf::as_path));
    inputs.extend(libraries.iter().map(PathBuf::as_path));
    let mut link = Command::new(&gcc);
    link.args(["-static-pie", "-pthread", "-Wl,--gc-sections", "-Wl,-z,stack-size=2097152", "-o"]).arg(&unstripped);
    link.args(&objects).arg(&rust_obj).arg("-Wl,--start-group").args(&libraries).arg("-Wl,--end-group").arg("-lm");
    let recipe = Recipe::of(&unstripped, &link, &inputs);
    if !recipe.fresh() {
        util::run(&mut link)?;
        recipe.record()?;
    }
    // The unstripped program stays, for addr2line.
    let mut strip = Command::new(cross_tool("strip"));
    strip.arg("--strip-all").arg("-o").arg(&exe).arg(&unstripped);
    let recipe = Recipe::of(&exe, &strip, &[&unstripped]);
    if !recipe.fresh() {
        util::run(&mut strip)?;
        recipe.record()?;
    }
    Ok(Some(util::read(&exe)?))
}

/// The media libraries' tests for the system image (`/system/tests/media`):
/// FFmpeg's and dav1d's checkasm.
fn media_tests() -> Result<Vec<(String, Vec<u8>)>> {
    let dir = media_root().join("tests");
    let mut tests = Vec::new();
    for name in ["checkasm-dav1d", "checkasm-ffmpeg"] {
        let path = dir.join(name);
        if path.is_file() {
            tests.push((name.to_string(), util::read(&path)?));
        }
    }
    Ok(tests)
}

/// Where [`command`] notes the POSIX layer the native programs were linked
/// with: the SHA-256 of its object.
fn native_layer_note() -> PathBuf {
    root().join("build").join("native-posix-layer")
}

/// The native toolchain's programs are C programs too, with the POSIX layer
/// linked in: when it has changed since they were linked, links them again
/// (`implementation/ports/build.sh` redoes only that). Without the build
/// tools it says so and leaves them as they are.
fn relink_native() -> Result {
    let object = posix_layer_object();
    if !built("native toolchain") || !object.is_file() {
        return Ok(());
    }
    let current = util::sha256_file(&object)?;
    if std::fs::read_to_string(native_layer_note()).is_ok_and(|linked| linked.trim() == current) {
        return Ok(());
    }
    if let Err(e) = shell() {
        util::status("Note", format!("the C toolchain in the image has an older POSIX layer. {e}"));
        return Ok(());
    }
    util::status("Linking", "the C toolchain's programs with the current POSIX layer");
    build(std::thread::available_parallelism().map_or(4, |n| n.get()))
}

/// The native toolchain's files for the system image: paths relative to
/// `/system` and their contents. None if it has not been built.
fn native_files() -> Result<Vec<(String, Vec<u8>)>> {
    let base = root().join("native").join("system");
    let mut files = Vec::new();
    for path in util::files_under(&base)? {
        let rel = path.strip_prefix(&base).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        files.push((rel, util::read(&path)?));
    }
    Ok(files)
}

/// What the toolchain gives the system image, at their paths in it: the C
/// and Rust test programs (`tests/c`, `tests/rust`), the native toolchain
/// (as `/system` has it), and the media decoder (`bin/mediadec`) and its
/// libraries' tests (`tests/media`) if `with_mediadec`. The POSIX layer
/// goes into the C library once, first; all of it is made while this
/// build holds the toolchain's lock. Says what is missing, and whether
/// the toolchain is older than its ports.
pub fn image_files(with_mediadec: bool) -> Result<Vec<(String, Vec<u8>)>> {
    if cross_gcc().is_none() && !built("native toolchain") {
        util::status("Note", "no C toolchain, C tests or media decoder in the image (`cargo orchestration toolchain`)");
        return Ok(Vec::new());
    }
    let _lock = lock()?;
    check_ports();
    if cross_gcc().is_some() {
        refresh_posix_layer()?;
    }
    relink_native()?;
    let mut files = Vec::new();
    files.extend(c_tests()?.into_iter().map(|(name, exe)| (format!("tests/c/{name}"), exe)));
    files.extend(rust_tests()?.into_iter().map(|(name, exe)| (format!("tests/rust/{name}"), exe)));
    let native = native_files()?;
    if native.is_empty() {
        util::status("Note", "no C toolchain in the image (build it with `cargo orchestration toolchain`)");
    }
    files.extend(native);
    if with_mediadec {
        match mediadec()? {
            Some(exe) => files.push(("bin/mediadec".into(), exe)),
            None => util::status(
                "Note",
                "no media decoder in the image: videos do not play (`cargo orchestration toolchain` builds FFmpeg and \
                 dav1d)",
            ),
        }
        files.extend(media_tests()?.into_iter().map(|(name, exe)| (format!("tests/media/{name}"), exe)));
    }
    Ok(files)
}

/// The ports the toolchain is built from, and what builds them, whose
/// fingerprint `cargo orchestration toolchain` notes.
fn ports_fingerprint() -> Result<String> {
    let dir = ports::dir();
    let names = ["binutils", "gcc", "gmp", "mpfr", "mpc", "musl", "dav1d", "ffmpeg", "rust"];
    ports::fingerprint(&names, &[dir.join("build.sh"), dir.join("common.sh"), ports::settings()])
}

/// Where the fingerprint of the ports the toolchain was built from is.
fn ports_note() -> PathBuf {
    root().join("build").join("ports-fingerprint")
}

/// Says so if the toolchain was built from other ports than
/// `implementation/ports/` has now.
fn check_ports() {
    let now = match ports_fingerprint() {
        Ok(print) => print,
        Err(e) => return util::status("Note", format!("the toolchain's ports: {e}")),
    };
    match std::fs::read_to_string(ports_note()) {
        Ok(built) if built == now => {}
        Ok(_) => util::status(
            "Note",
            "the toolchain was built from other ports than implementation/ports/ has now: `cargo orchestration \
             toolchain` builds it again",
        ),
        Err(_) => util::status(
            "Note",
            "which ports the toolchain was built from is not known (it was built before that was noted): `cargo \
             orchestration toolchain` notes it",
        ),
    }
}

/// The ports the toolchain is built from: C's, the media libraries' and
/// Rust's.
fn toolchain_ports() -> Result<Vec<Port>> {
    ["binutils", "gcc", "gmp", "mpfr", "mpc", "musl", "dav1d", "ffmpeg", "rust"]
        .iter()
        .map(|p| Port::named(p))
        .collect()
}

/// Rust's name for this machine (its target), as the Rust that runs
/// `orchestration` gives it.
fn rust_host() -> Result<String> {
    let out = Command::new("rustc").arg("-vV").output().map_err(|e| format!("running rustc: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::to_owned)
        .ok_or_else(|| "rustc -vV names no host".into())
}

/// The `key=value` lines of the rust port's `src/stage0`, which pins the
/// release of Rust that builds it, its stage 0 (its archives and their
/// SHA-256). It is read from the port's archive once, and kept beside it.
fn rust_stage0_pins(port: &Port, downloads: &Path) -> Result<Vec<(String, String)>> {
    let archive = port.archive(downloads);
    let name = archive.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let kept = downloads.join(format!("{name}.stage0"));
    if !is_newer(&kept, &[&archive]) {
        // The archive's one top directory is named after it.
        let top = name.strip_suffix(".tar.xz").ok_or_else(|| format!("{}: not a .tar.xz", archive.display()))?;
        let out = Command::new("tar")
            .arg("-xJOf")
            .arg(&archive)
            .arg(format!("{top}/src/stage0"))
            .output()
            .map_err(|e| format!("running tar: {e}"))?;
        if !out.status.success() {
            return Err(format!("{}: no {top}/src/stage0", archive.display()));
        }
        util::write(&kept, &out.stdout)?;
    }
    let text = util::read_to_string(&kept)?;
    Ok(stage0_pins(&text))
}

/// The `key=value` lines of a `src/stage0`, without its comments.
fn stage0_pins(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// Downloads and checks the archives of Rust's stage 0 for this machine
/// (`host`): its compiler, standard library and Cargo, as the port's
/// release pins them. Returns their names, in `downloads`.
fn fetch_rust_stage0(downloads: &Path, host: &str) -> Result<Vec<String>> {
    let port = Port::named("rust")?;
    let pins = rust_stage0_pins(&port, downloads)?;
    let pinned_by = format!("src/stage0 of Rust {}", port.version);
    let pin = |key: &str| {
        pins.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).ok_or_else(|| format!("{pinned_by}: no {key}"))
    };
    let (server, date, version) = (pin("dist_server")?, pin("compiler_date")?, pin("compiler_version")?);
    let mut names = Vec::new();
    for component in ["rust-std", "rustc", "cargo"] {
        let name = format!("{component}-{version}-{host}.tar.xz");
        let path = format!("dist/{date}/{name}");
        ports::download(&format!("{server}/{path}"), &pin(&path)?, &pinned_by, downloads)?;
        names.push(name);
    }
    Ok(names)
}

/// The programs the build needs from the system: compilers and GNU tools,
/// curl for the downloads, meson and ninja for the media libraries, CMake
/// and Python for Rust's (its LLVM, its build program); and the
/// architecture's own (`arch::PORT_TOOLS`). `documentation/C.md` names the
/// packages.
const BUILD_TOOLS: [&str; 12] =
    ["gcc", "g++", "make", "m4", "bison", "flex", "patch", "curl", "meson", "ninja", "cmake", "python3"];

/// The shell that runs `implementation/ports/build.sh`, once the build tools
/// are there.
fn shell() -> Result<Command> {
    let missing: Vec<&str> =
        BUILD_TOOLS.iter().chain(arch::PORT_TOOLS).copied().filter(|t| util::find_on_path(t).is_none()).collect();
    match missing.as_slice() {
        [] => arch::check_port_tools().map(|()| Command::new("bash")),
        [tool] => {
            Err(format!("{tool} is missing: install it from the distribution's packages (see documentation/C.md)"))
        }
        [tools @ .., last] => Err(format!(
            "{} and {last} are missing: install them from the distribution's packages (see documentation/C.md)",
            tools.join(", ")
        )),
    }
}

/// `cargo orchestration toolchain [--jobs N]`.
pub fn command(args: &[String]) -> Result {
    let mut jobs = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--jobs" | "-j" => jobs = util::jobs(it.next())?,
            other => return Err(format!("unknown option '{other}' (toolchain takes --jobs N)")),
        }
    }
    // One build at a time (a second would empty the first's build
    // directories under it), and no image build meanwhile.
    let _lock = lock()?;
    build(jobs)
}

/// Builds the toolchain (`jobs` at a time), holding its lock.
fn build(jobs: usize) -> Result {
    let started = std::time::Instant::now();
    // The tools first: the downloads are large.
    let mut sh = shell()?;
    let downloads = root().join("downloads");
    for port in toolchain_ports()? {
        ports::fetch(&port, &downloads)?;
    }
    let rust_host = rust_host()?;
    let rust_stage0 = fetch_rust_stage0(&downloads, &rust_host)?;
    // The script makes the C library's object of the POSIX layer once its
    // cross linker exists.
    let library = posix_library()?;
    let script = ports::dir().join("build.sh");
    sh.arg(&script)
        .env("VEDA_PORTS", ports::dir())
        .env("VEDA_ARCH", arch::NAME)
        .env("VEDA_TOOLCHAIN", root())
        .env("VEDA_POSIX_LIB", &library)
        .env("VEDA_POSIX_EXPORTS", POSIX_LAYER_EXPORTS.join(" "))
        .env("VEDA_RUST_HOST", &rust_host)
        .env("VEDA_RUST_STAGE0", rust_stage0.join(" "))
        .env("JOBS", jobs.to_string());
    util::run(&mut sh)?;
    // The native programs now have the POSIX layer's object as it is, and
    // the toolchain the ports as they are.
    let object = posix_layer_object();
    util::write(&native_layer_note(), util::sha256_file(&object)?)?;
    util::write(&ports_note(), ports_fingerprint()?)?;
    util::status("Finished", format!("the toolchain in {:.0} min", started.elapsed().as_secs_f32() / 60.0));
    Ok(())
}

/// Whether `target` exists and is newer than every input (for what a
/// command that never changes makes).
fn is_newer(target: &Path, inputs: &[&Path]) -> bool {
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let Some(t) = modified(target) else { return false };
    inputs.iter().all(|i| modified(i).is_some_and(|m| m <= t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage0_pins_are_its_key_value_lines() {
        let text = "dist_server=https://static.rust-lang.org\n# A comment = not a pin.\n\ncompiler_version=1.98.0\n\
                    dist/2026-08-20/cargo-1.98.0-x86_64-unknown-linux-gnu.tar.xz=2f512d17\n";
        let pins = stage0_pins(text);
        let pin = |k: &str| pins.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(pins.len(), 3);
        assert_eq!(pin("dist_server"), Some("https://static.rust-lang.org"));
        assert_eq!(pin("compiler_version"), Some("1.98.0"));
        assert_eq!(pin("dist/2026-08-20/cargo-1.98.0-x86_64-unknown-linux-gnu.tar.xz"), Some("2f512d17"));
    }

    #[test]
    fn comments_are_left_out() {
        let c = "int a; /* a float */ int b; // a double\nint c; /* no end";
        assert_eq!(without_c_comments(c), "int a;   int b;  \nint c;  ");
        assert_eq!(without_c_comments("x // y"), "x  ");
        assert_eq!(without_c_comments("/**/f"), " f");
    }
}

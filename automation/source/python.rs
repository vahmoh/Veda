// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Python: CPython for Veda, its interpreter and standard library (see
//! `documentation/PYTHON.md`).
//!
//! `cargo automation python` builds it from `implementation/ports/` with
//! `implementation/ports/python/build.sh`, in `target/python`
//! (`$VEDA_PYTHON` overrides it), once the toolchain is built (its cross
//! compiler builds it): first the tools for this machine that its build
//! runs (pkgconf, and the same release of Python), then the C libraries its
//! modules stand on, for Veda, static and position-independent, then
//! CPython, the standard library's modules built into the interpreter,
//! which is linked dynamically: it loads extension modules, and ctypes's
//! libraries. Once it is built, every image has it in `/system`
//! (`/system/bin/python3`, the standard library in
//! `/system/lib/python3.15`, its C API in `/system/include/python3.15`),
//! and an image built for tests has its tests too.

use std::path::PathBuf;
use std::process::Command;

use crate::ports::{self, Port};
use crate::util::{self, Result};
use crate::{architecture, certificates, toolchain};

/// The ports Python is built from: the tools for this machine, the
/// libraries in the order the build makes them, then CPython.
pub const PORTS: &[&str] =
    &["pkgconf", "zlib", "bzip2", "xz", "zstd", "mpdecimal", "expat", "sqlite", "openssl", "libffi", "python"];

/// The libraries built into the interpreter, whose licence texts the image
/// has beside Python's (`/system/licences/python/<port>`).
const LIBRARIES: &[&str] = &["zlib", "bzip2", "xz", "zstd", "mpdecimal", "expat", "sqlite", "openssl", "libffi"];

/// What the build runs from this machine beyond a C compiler: Autoconf's
/// shell and make, CMake and Ninja (for XZ Utils and Expat), Perl (for
/// OpenSSL's configuration), and the tools that unpack the archives.
/// pkgconf and the Python that CPython's build runs are built from their
/// ports.
const BUILD_TOOLS: [&str; 11] = ["gcc", "make", "patch", "tar", "xz", "gzip", "curl", "cmake", "ninja", "perl", "sed"];

/// Where Python is built.
pub fn root() -> PathBuf {
    util::env_dir("VEDA_PYTHON", &["target", "python"])
}

/// The lock over Python's build.
fn lock() -> Result<util::Lock> {
    util::Lock::exclusive(&root().join(".lock"), "Python's build")
}

/// The ports and what builds them, whose fingerprint `cargo automation
/// python` notes: CPython is configured for the C library's too.
fn ports_fingerprint() -> Result<String> {
    let mut extra = ports::library_build_files()?;
    extra.push(ports::settings());
    let names: Vec<&str> = PORTS.iter().copied().chain(["musl"]).collect();
    ports::fingerprint(&names, &extra)
}

fn ports_note() -> PathBuf {
    root().join("build").join("ports-fingerprint")
}

/// Whether Python has been built.
pub fn built() -> bool {
    ports_note().is_file()
}

fn tools() -> Result {
    let missing: Vec<&str> = BUILD_TOOLS.into_iter().filter(|t| util::find_on_path(t).is_none()).collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("{} missing: install them from the distribution's packages", missing.join(", ")))
    }
}

/// The toolchain's cross compiler, which builds Python.
fn cross_toolchain() -> Result {
    if toolchain::built("cross toolchain") {
        Ok(())
    } else {
        Err("the toolchain is not built (`cargo automation toolchain`)".into())
    }
}

/// For `cargo automation doctor`: whether Python has been built (`Ok`), and
/// if not, whether it can be (`Err`).
pub fn status() -> std::result::Result<String, String> {
    if built() {
        let version = Port::named("python").map(|p| p.version).unwrap_or_default();
        let note = if is_current() {
            ""
        } else {
            ", from other ports than implementation/ports/ has now: `cargo automation python` builds it again"
        };
        return Ok(format!("Python {version} ({}){note}", root().display()));
    }
    let how = match tools().and_then(|()| cross_toolchain()) {
        Ok(()) => "`cargo automation python` builds it (a few minutes)".to_string(),
        Err(e) => format!("`cargo automation python` builds it, but {e}"),
    };
    Err(format!("not built (optional: Python in the image); {how}"))
}

/// Whether Python was built from the ports as they are now.
fn is_current() -> bool {
    match (std::fs::read_to_string(ports_note()), ports_fingerprint()) {
        (Ok(built), Ok(now)) => built == now,
        _ => false,
    }
}

/// The files under `dir`, at their paths relative to it, after `prefix`.
fn tree(dir: &std::path::Path, prefix: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    for path in util::files_under(dir)? {
        let rel = path.strip_prefix(dir).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        files.push((format!("{prefix}{rel}"), util::read(&path)?));
    }
    Ok(files)
}

/// What Python gives the system image, at their paths in it, once it is
/// built: the interpreter, its standard library and its C API (as
/// `/system` has them), the standard library's tests and their extension
/// modules if `tests` (images built for testing), the checks of Python on
/// Veda (`verification/python`, in `/system/tests/python`), and the
/// licence texts of Python and of the libraries built into it
/// (`licences/python/<port>`), each with where its source is. The
/// interpreter stands on the C library's shared object, which the image
/// has as it is now.
pub fn image_files(tests: bool) -> Result<Vec<(String, Vec<u8>)>> {
    if !built() {
        return Ok(Vec::new());
    }
    if !is_current() {
        util::status(
            "Note",
            "Python was built from other ports than implementation/ports/ has now: `cargo automation python` \
             builds it again",
        );
    }
    let mut files = tree(&root().join("python").join("system"), "")?;
    let Some((_, interpreter)) = files.iter().find(|(path, _)| path == "bin/python3") else {
        return Err("Python's build has no interpreter: `cargo automation python` makes it".into());
    };
    // Its other names, which the image stores once with it.
    let version = Port::named("python")?.version;
    let short = version.rsplit_once('.').map_or(version.as_str(), |(short, _)| short);
    let interpreter = interpreter.clone();
    files.push((format!("bin/python{short}"), interpreter.clone()));
    files.push(("bin/python".into(), interpreter));
    if tests {
        files.extend(tree(&root().join("tests").join("system"), "")?);
    }
    files.extend(tree(&util::workspace_root().join("verification").join("python"), "tests/python/")?);
    let licences = root().join("licences");
    for port in std::iter::once("python").chain(LIBRARIES.iter().copied()) {
        let dir = licences.join(port);
        let texts = tree(&dir, &format!("licences/python/{port}/"))?;
        if texts.is_empty() && port != "sqlite" {
            return Err(format!(
                "Python's {port} has no licence texts ({}): `cargo automation python` gathers them",
                dir.display()
            ));
        }
        files.extend(texts);
        files.push((format!("licences/python/{port}/SOURCE"), Port::named(port)?.source_note().into_bytes()));
    }
    Ok(files)
}

/// `cargo automation python [--jobs N]`.
pub fn command(args: &[String]) -> Result {
    let mut jobs = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--jobs" | "-j" => jobs = util::jobs(it.next())?,
            other => return Err(format!("unknown option '{other}' (python takes --jobs N)")),
        }
    }
    tools()?;
    cross_toolchain()?;
    let started = std::time::Instant::now();
    let _machine = util::heavy_build_lock("Python")?;
    let _lock = lock()?;
    // Python is configured and linked with the C library as the POSIX layer
    // has it now.
    toolchain::refresh_c_library()?;
    // The ports, and the certificate authorities its TLS trusts, which the
    // image has with it.
    let downloads = root().join("downloads");
    for name in PORTS.iter().chain(&[certificates::PORT]) {
        ports::fetch(&Port::named(name)?, &downloads)?;
    }
    build(jobs)?;
    util::status("Finished", format!("Python in {:.0} min", started.elapsed().as_secs_f32() / 60.0));
    Ok(())
}

/// Runs Python's build (`jobs` at a time), its ports downloaded, holding its
/// lock, and notes the ports it was built from.
fn build(jobs: usize) -> Result {
    let ports = ports::dir();
    util::run(
        Command::new("bash")
            .arg(ports.join("python").join("build.sh"))
            .env("VEDA_PORTS", &ports)
            .env("VEDA_ARCH", architecture::NAME)
            .env("VEDA_PYTHON", root())
            .env("VEDA_TOOLCHAIN", toolchain::root())
            .env("JOBS", jobs.to_string()),
    )?;
    util::write(&ports_note(), ports_fingerprint()?)
}

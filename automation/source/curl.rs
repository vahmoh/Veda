// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! curl: the `curl` command and libcurl, the library C programs transfer
//! data by URL with (see `documentation/CURL.md`).
//!
//! `cargo automation toolchain` builds it once the toolchain is built, with
//! `implementation/ports/curl/build.sh`, in `target/toolchain/curl`
//! ([`root`]): the libraries libcurl holds (OpenSSL, nghttp2, zlib, Brotli,
//! Zstandard, libpsl and libidn2), static and position-independent, then
//! curl, its library shared. Every image built with the toolchain has it in
//! `/system` (`/system/bin/curl`, `/system/lib/libcurl.so.4`,
//! `/system/include/curl`), with the certificate authorities it trusts
//! (`/system/etc/ssl/cert.pem`, `certificates`), and its checks
//! (`verification/curl`, in `/system/tests/curl`), which `systest` runs.
//! The program and the library stand on the C library's shared object, as
//! the image has it: a new POSIX layer needs no new build of curl.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::ports::{self, Port};
use crate::util::{self, Recipe, Result};
use crate::{architecture, certificates, toolchain};

/// The ports curl is built from: pkgconf for this machine, the libraries in
/// the order the build makes them, then curl.
pub const PORTS: &[&str] = &["pkgconf", "zlib", "zstd", "brotli", "nghttp2", "libpsl", "libidn2", "openssl", "curl"];

/// The libraries libcurl holds, whose licence texts the image has beside
/// curl's (`/system/licences/curl/<port>`).
const LIBRARIES: &[&str] = &["zlib", "zstd", "brotli", "nghttp2", "libpsl", "libidn2", "openssl"];

/// What the build runs from this machine beyond the toolchain's tools:
/// Perl, which configures OpenSSL and makes curl's manual.
pub const BUILD_TOOLS: &[&str] = &["perl"];

/// Where curl is built: in the toolchain's directory, which its builds share.
pub fn root() -> PathBuf {
    toolchain::root().join("curl")
}

/// curl as the image installs it in `/system`.
fn system() -> PathBuf {
    root().join("curl").join("system")
}

/// Whether curl has been built.
pub fn built() -> bool {
    system().join("bin").join("curl").is_file()
}

/// For `cargo automation doctor`: whether curl has been built (`Ok`), and
/// if not, what builds it (`Err`).
pub fn status() -> std::result::Result<String, String> {
    if built() {
        let version = Port::named("curl").map(|p| p.version).unwrap_or_default();
        return Ok(format!("curl {version} ({})", root().display()));
    }
    Err("not built (curl, and libcurl for C programs, in the image); `cargo automation toolchain` builds it".into())
}

/// Downloads and checks the ports curl is built from, and the certificate
/// authorities the image has with it, into `downloads`.
pub fn fetch(downloads: &Path) -> Result {
    for name in PORTS.iter().chain(&[certificates::PORT]) {
        ports::fetch(&Port::named(name)?, downloads)?;
    }
    Ok(())
}

/// Runs curl's build (`jobs` at a time), its ports downloaded into the
/// toolchain's `downloads`, the toolchain built.
pub fn build(jobs: usize) -> Result {
    let ports = ports::dir();
    util::run(
        Command::new("bash")
            .arg(ports.join("curl").join("build.sh"))
            .env("VEDA_PORTS", &ports)
            .env("VEDA_ARCH", architecture::current().name)
            .env("VEDA_CURL", root())
            .env("VEDA_TOOLCHAIN", toolchain::root())
            .env("JOBS", jobs.to_string()),
    )
}

/// The files under `dir`, at their paths relative to it, after `prefix`.
fn tree(dir: &Path, prefix: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    for path in util::files_under(dir)? {
        let rel = path.strip_prefix(dir).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        files.push((format!("{prefix}{rel}"), util::read(&path)?));
    }
    Ok(files)
}

/// What the built-in Public Suffix List's licence asks, beside libpsl's
/// own texts: libpsl builds the list into libcurl.
const PUBLIC_SUFFIX_NOTE: &str = "\nlibpsl builds in the Public Suffix List its release carries (list/public_suffix_list.dat), \
     under the Mozilla Public License 2.0, whose text is at https://mozilla.org/MPL/2.0/; the list is \
     https://publicsuffix.org/list/.\n";

/// What curl gives the system image, at their paths in it, once it is
/// built: the program, the library and its interface (as `/system` has
/// them), the checks of curl and libcurl (`tests/curl/checks`), and the
/// licence texts of curl and of the libraries libcurl holds
/// (`licences/curl/<port>`), each with where its source is. Nothing, and a
/// note that says so, if it is not built. The certificate authorities it
/// trusts are `certificates`', which the image takes beside.
pub fn image_files() -> Result<Vec<(String, Vec<u8>)>> {
    if !built() {
        util::status("Note", "no curl in the image (`cargo automation toolchain` builds it)");
        return Ok(Vec::new());
    }
    let mut files = tree(&system(), "")?;
    files.push(("tests/curl/checks".into(), checks()?));
    let licences = root().join("licences");
    for port in std::iter::once("curl").chain(LIBRARIES.iter().copied()) {
        let texts = tree(&licences.join(port), &format!("licences/curl/{port}/"))?;
        if texts.is_empty() {
            return Err(format!(
                "curl's {port} has no licence texts ({}): `cargo automation toolchain` gathers them",
                licences.join(port).display()
            ));
        }
        files.extend(texts);
        let mut source = Port::named(port)?.source_note();
        if port == "libpsl" {
            source.push_str(PUBLIC_SUFFIX_NOTE);
        }
        files.push((format!("licences/curl/{port}/SOURCE"), source.into_bytes()));
    }
    Ok(files)
}

/// The ports whose versions the checks expect of curl and of the libraries
/// libcurl holds, and the names of their macros (`VEDA_CURL_VERSION`...).
const CHECKED_VERSIONS: &[&str] = &["curl", "openssl", "nghttp2", "zlib", "brotli", "zstd", "libidn2", "libpsl"];

/// The checks of curl and libcurl (`verification/curl/checks.c`), linked
/// dynamically with libcurl, as programs that use it are, and statically
/// with the libraries its servers stand on (OpenSSL, nghttp2 and the
/// compressors), which libcurl keeps to itself. Built again when any of it
/// changed.
fn checks() -> Result<Vec<u8>> {
    let gcc = toolchain::cross_gcc().ok_or("curl's checks need the cross toolchain: `cargo automation toolchain`")?;
    let source = util::workspace_root().join("verification").join("curl").join("checks.c");
    let out = root().join("tests");
    util::create_dir_all(&out)?;
    let exe = out.join("checks");
    let libraries = root().join("system").join("lib");
    let archives: Vec<PathBuf> = ["ssl", "crypto", "nghttp2", "brotlienc", "brotlicommon", "zstd", "z"]
        .iter()
        .map(|name| libraries.join(format!("lib{name}.a")))
        .collect();
    let libcurl = system().join("lib").join("libcurl.so.4");
    let libc = toolchain::shared_c_library();
    let mut cc = Command::new(&gcc);
    cc.args(["-O2", "-Wall", "-Wextra", "-Werror", "-g0", "-s", "-fPIE", "-pie", "-pthread", "-Wl,--gc-sections"])
        .arg(util::c_path_remap());
    for port in CHECKED_VERSIONS {
        cc.arg(format!("-DVEDA_{}_VERSION=\"{}\"", port.to_uppercase(), Port::named(port)?.version));
    }
    cc.arg("-I")
        .arg(system().join("include"))
        .arg("-I")
        .arg(root().join("system").join("include"))
        .arg("-o")
        .arg(&exe)
        .arg(&source)
        .arg("-L")
        .arg(system().join("lib"))
        .arg("-lcurl")
        .args(&archives);
    let mut inputs: Vec<&Path> = vec![&source, &gcc, &libc, &libcurl];
    inputs.extend(archives.iter().map(PathBuf::as_path));
    let recipe = Recipe::of(&exe, &cc, &inputs);
    if !recipe.fresh() {
        util::status("Building", "curl's checks");
        util::run(&mut cc)?;
        recipe.record()?;
    }
    util::read(&exe)
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The web engine: WPE WebKit and the libraries it stands on, for the
//! browser (see `documentation/BROWSER.md`).
//!
//! `cargo automation web` builds them from `implementation/ports/` with
//! `implementation/ports/wpewebkit/build.sh`, in `target/web`
//! (`$VEDA_WEB` overrides it), once the toolchain is built (its cross
//! compilers build them): first the tools for this machine that their
//! builds run (Meson, pkgconf, gperf, Ruby with libyaml, GLib's code
//! generators), then the libraries for Veda, static and
//! position-independent, in `target/web/system` (GStreamer and the codecs
//! it decodes with among them), then WebKit. Once they
//! are built, every image build links the browser with them.

use std::path::PathBuf;
use std::process::Command;

use crate::ports::{self, Port};
use crate::util::{self, Result};
use crate::{arch, toolchain};

/// The ports the web engine is built from: the tools for this machine,
/// then the libraries in the order their builds need them, then WebKit.
pub const PORTS: &[&str] = &[
    "meson",
    "pkgconf",
    "gperf",
    "libyaml",
    "ruby",
    "zlib",
    "libffi",
    "pcre2",
    "glib",
    "libpsl",
    "nghttp2",
    "brotli",
    "sqlite",
    "libxml2",
    "libxslt",
    "icu",
    "expat",
    "libpng",
    "libjpeg-turbo",
    "libwebp",
    "freetype",
    "harfbuzz",
    "fontconfig",
    "libgpg-error",
    "libgcrypt",
    "libtasn1",
    "openssl",
    "glib-networking",
    "libsoup",
    "egl-registry",
    "libepoxy",
    "libxkbcommon",
    "lcms2",
    "libogg",
    "opus",
    "dav1d",
    "ffmpeg",
    "gstreamer",
    "gst-plugins-base",
    "gst-plugins-good",
    "gst-plugins-bad",
    "gst-libav",
    "wpewebkit",
];

/// What the web engine has in the system image that is not built: the
/// certificate authorities its TLS trusts and the fonts of the world's
/// scripts. They are fetched with the engine, and a change of theirs
/// rebuilds none of it.
pub const DATA_PORTS: &[&str] = &["ca-certificates", "noto-fonts", "noto-cjk", "noto-emoji"];

/// What the build runs from this machine beyond a C compiler: the build
/// systems of the libraries (CMake and Ninja, Autoconf's shell and make),
/// NASM for the SIMD code of libjpeg-turbo, dav1d and FFmpeg, Perl for
/// OpenSSL's and WebKit's scripts, Python for Meson, GLib's and WebKit's
/// scripts, and Bison for libxkbcommon's parser. Meson, pkgconf, gperf, libyaml and Ruby are
/// built from their ports.
const BUILD_TOOLS: [&str; 14] =
    ["gcc", "g++", "make", "patch", "tar", "xz", "bzip2", "curl", "cmake", "ninja", "python3", "perl", "nasm", "bison"];

/// Where the web engine is built.
pub fn root() -> PathBuf {
    util::env_dir("VEDA_WEB", &["target", "web"])
}

/// The lock over the web engine's build.
fn lock() -> Result<util::Lock> {
    util::Lock::exclusive(&root().join(".lock"), "the web engine's build")
}

/// The ports and what builds them, whose fingerprint `cargo automation
/// web` notes.
fn ports_fingerprint() -> Result<String> {
    let dir = ports::dir();
    ports::fingerprint(PORTS, &[dir.join("common.sh"), ports::settings()])
}

fn ports_note() -> PathBuf {
    root().join("build").join("ports-fingerprint")
}

/// Whether the web engine has been built.
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

/// For `cargo automation doctor`: whether the web engine has been built
/// (`Ok`), and if not, whether it can be (`Err`).
pub fn status() -> std::result::Result<String, String> {
    if built() {
        let version = Port::named("wpewebkit").map(|p| p.version).unwrap_or_default();
        return Ok(format!("WPE WebKit {version} ({})", root().display()));
    }
    let how = match tools().and_then(|()| cross_toolchain()) {
        Ok(()) => "`cargo automation web` builds it".to_string(),
        Err(e) => format!("`cargo automation web` builds it, but {e}"),
    };
    Err(format!("not built (optional: the browser needs it); {how}"))
}

/// The toolchain's cross compilers, which build the web engine.
fn cross_toolchain() -> Result {
    if toolchain::built("cross toolchain") {
        Ok(())
    } else {
        Err("the toolchain is not built (`cargo automation toolchain`)".into())
    }
}

/// What the web engine gives the system image, at their paths in it, once
/// it is built: fontconfig's configuration (the system's fonts, as WebKit
/// finds them: `implementation/ports/fontconfig/fonts.conf`), the
/// certificate authorities its TLS trusts (the `ca-certificates` port),
/// Noto's fonts (`fonts/noto`, whose licence is the other fonts'
/// `fonts/OFL.txt`), and the licence texts of WebKit and its libraries
/// (`licences/web/PORT/...`, from `build.sh`'s `licences` stage), which the
/// browser goes nowhere without. Data ports not fetched yet are fetched.
pub fn image_files() -> Result<Vec<(String, Vec<u8>)>> {
    if !built() {
        return Ok(Vec::new());
    }
    let downloads = root().join("downloads");
    let config = ports::dir().join("fontconfig").join("fonts.conf");
    let mut files = vec![("etc/fonts/fonts.conf".to_string(), util::read(&config)?)];
    for name in DATA_PORTS {
        let port = Port::named(name)?;
        ports::fetch(&port, &downloads)?;
        for path in port.files(&downloads) {
            let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let at =
                if *name == "ca-certificates" { "etc/ssl/cert.pem".to_string() } else { format!("fonts/noto/{file}") };
            files.push((at, util::read(&path)?));
        }
    }
    let licences = root().join("system").join("share").join("licences");
    let texts = util::files_under(&licences)?;
    if texts.is_empty() {
        return Err("the web engine has no licence texts: `cargo automation web` makes them".into());
    }
    let mut ported = std::collections::BTreeSet::new();
    for path in texts {
        let rel = path.strip_prefix(&licences).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        if let Some((port, _)) = rel.split_once('/') {
            ported.insert(port.to_string());
        }
        files.push((format!("licences/web/{rel}"), util::read(&path)?));
    }
    // Where each one's source is; and of Mozilla's list of certificate
    // authorities, which the Mozilla Public License covers, where it and
    // its licence are, as that licence asks.
    for port in ported {
        files.push((format!("licences/web/{port}/SOURCE"), Port::named(&port)?.source_note().into_bytes()));
    }
    let note = Port::named("ca-certificates")?.source_note()
        + "\nThe list is Mozilla's (its first lines say where its source is), under the Mozilla Public License 2.0, \
           whose text is at https://mozilla.org/MPL/2.0/.\n";
    files.push(("licences/web/ca-certificates/SOURCE".into(), note.into_bytes()));
    Ok(files)
}

/// `cargo automation web [--jobs N]`.
pub fn command(args: &[String]) -> Result {
    let mut jobs = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--jobs" | "-j" => jobs = util::jobs(it.next())?,
            other => return Err(format!("unknown option '{other}' (web takes --jobs N)")),
        }
    }
    tools()?;
    cross_toolchain()?;
    let started = std::time::Instant::now();
    let _machine = util::heavy_build_lock("the web engine")?;
    let _lock = lock()?;
    // The engine's libraries use the C library's Veda interfaces
    // (<veda/thread.h>, <veda/memory.h>), as the POSIX layer has them now.
    toolchain::refresh_c_library()?;
    let downloads = root().join("downloads");
    for name in PORTS.iter().chain(DATA_PORTS) {
        ports::fetch(&Port::named(name)?, &downloads)?;
    }
    let ports = ports::dir();
    util::run(
        Command::new("bash")
            .arg(ports.join("wpewebkit").join("build.sh"))
            .env("VEDA_PORTS", &ports)
            .env("VEDA_ARCH", arch::NAME)
            .env("VEDA_WEB", root())
            .env("VEDA_TOOLCHAIN", toolchain::root())
            .env("JOBS", jobs.to_string()),
    )?;
    util::write(&ports_note(), ports_fingerprint()?)?;
    util::status("Finished", format!("the web engine in {:.0} min", started.elapsed().as_secs_f32() / 60.0));
    Ok(())
}

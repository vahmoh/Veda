// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Builds the web engine's bridge (`engine/`, C) with the C toolchain's
//! compiler for Veda, and links it with WPE WebKit and every library it
//! stands on, statically, as `cargo automation web` built them
//! (`$VEDA_WEB/system`, `target/web/system` by default): their flags and
//! libraries are what `wpe-webkit-static-2.0.pc` and `gstreamer-full-1.0.pc`
//! (GStreamer, whose plugins and audio sink the bridge registers) say,
//! through the web build's own pkg-config.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCES: [&str; 3] = ["engine/engine.c", "engine/platform.c", "engine/media.c"];
const HEADERS: [&str; 2] = ["engine/engine.h", "engine/internal.h"];
const PACKAGES: [&str; 2] = ["wpe-webkit-static-2.0", "gstreamer-full-1.0"];

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    for file in SOURCES.iter().chain(HEADERS.iter()) {
        println!("cargo:rerun-if-changed={file}");
    }
    println!("cargo:rerun-if-env-changed=VEDA_WEB");
    // Built for the host (cargo check, clippy on this machine), it has no
    // engine to link: only Veda's builds link it.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("veda") {
        return;
    }
    let web = env::var_os("VEDA_WEB").map(PathBuf::from).unwrap_or_else(|| manifest.join("../../../target/web"));
    // The web build's pkg-config for the C toolchain's target.
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let pkg_config = web.join("host/bin").join(format!("{arch}-veda-pkg-config"));
    for name in PACKAGES {
        let package = web.join("system/lib/pkgconfig").join(format!("{name}.pc"));
        if !package.is_file() {
            panic!("the web engine is not built ({} is missing): `cargo automation web` builds it", package.display());
        }
        println!("cargo:rerun-if-changed={}", package.display());
    }
    let flags = |what: &str| -> Vec<String> {
        let out = Command::new(&pkg_config).arg(what).args(PACKAGES).output().unwrap_or_else(|e| {
            panic!("running {}: {e}", pkg_config.display());
        });
        if !out.status.success() {
            panic!("{} {what} {PACKAGES:?}: {}", pkg_config.display(), String::from_utf8_lossy(&out.stderr));
        }
        String::from_utf8_lossy(&out.stdout).split_whitespace().map(str::to_string).collect()
    };
    let cflags = flags("--cflags");
    let libs = flags("--libs");

    let cc = compiler("CC", "gcc");
    let ar = compiler("AR", "ar");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let mut objects = Vec::new();
    for source in SOURCES {
        let object = out.join(Path::new(source).file_name().unwrap()).with_extension("o");
        let status = Command::new(&cc)
            .args(["-std=gnu17", "-O2", "-g0", "-fPIE", "-Wall", "-Wextra", "-Werror"])
            .args(["-ffunction-sections", "-fdata-sections", "-fstack-protector-strong"])
            .arg(format!("-ffile-prefix-map={}=browser", manifest.display()))
            .args(&cflags)
            .arg("-I")
            .arg(manifest.join("engine"))
            .arg("-c")
            .arg("-o")
            .arg(&object)
            .arg(manifest.join(source))
            .status()
            .unwrap_or_else(|e| panic!("running {}: {e}", cc.display()));
        if !status.success() {
            panic!("compiling {source} failed");
        }
        objects.push(object);
    }
    let archive = out.join("libengine.a");
    let _ = std::fs::remove_file(&archive);
    let status = Command::new(&ar).arg("rcs").arg(&archive).args(&objects).status().expect("running ar");
    assert!(status.success(), "archiving the engine's bridge failed");

    // Every library in one group: they need each other, in no one order.
    println!("cargo:rustc-link-arg=-Wl,--start-group");
    println!("cargo:rustc-link-arg={}", archive.display());
    for lib in &libs {
        println!("cargo:rustc-link-arg={lib}");
    }
    // The C library and the compiler's own, which they need in turn (Rust
    // names them before them).
    println!("cargo:rustc-link-arg=-lc");
    println!("cargo:rustc-link-arg=-lgcc");
    println!("cargo:rustc-link-arg=-Wl,--end-group");
    // A stack that cannot run code, whatever an object says; relocations
    // packed, which a process starts sooner with.
    println!("cargo:rustc-link-arg=-Wl,-z,noexecstack");
    println!("cargo:rustc-link-arg=-Wl,-z,pack-relative-relocs");
    // The libraries change when the engine is built again: link again.
    // GStreamer's plugins are in a directory of their own.
    for lib in libs.iter().filter_map(|l| l.strip_prefix("-l")) {
        for dir in ["system/lib", "system/lib/gstreamer-1.0"] {
            let path = web.join(dir).join(format!("lib{lib}.a"));
            if path.is_file() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
}

/// The C toolchain's `tool` for Veda: what Cargo's environment names for
/// the target (`CC_x86_64_unknown_veda`, as `cargo automation` sets), or
/// the toolchain's own.
fn compiler(variable: &str, tool: &str) -> PathBuf {
    let target = env::var("TARGET").unwrap_or_default().replace('-', "_");
    env::var_os(format!("{variable}_{target}")).map(PathBuf::from).unwrap_or_else(|| {
        let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        PathBuf::from(format!("{arch}-veda-{tool}"))
    })
}

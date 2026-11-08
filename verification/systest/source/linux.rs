// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Linux programs, as Linux's toolchains built them for the architecture
//! (`/system/tests/linux`, `automation/source/compatibility.rs`), run on
//! Veda's Linux compatibility: the checks of `verification/linux`, on musl
//! and glibc, linked statically and dynamically (with their own Linux
//! root), and on Rust's standard library; and `execve` in place.

use alloc::format;
use alloc::string::{String, ToString};

use crate::posix::run_with;
use crate::{TestResult, check, vfs_client};

fn s<E: core::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// The programs, and the Linux root each needs (its dynamic linker's).
const PROGRAMS: [(&str, Option<&str>); 5] = [
    ("checks-musl", None),
    ("checks-musl-dynamic", Some("/system/tests/linux/musl")),
    ("checks-glibc-static", None),
    ("checks-glibc", Some("/system/tests/linux/glibc")),
    ("checks-rust", None),
];

pub fn test_linux_programs() -> TestResult {
    let fs = vfs_client()?;
    let mut ran = 0;
    for (name, root) in PROGRAMS {
        let path = format!("/system/tests/linux/{name}");
        if fs.stat(path.clone()).map_err(s)?.is_err() {
            runtime::println!("linux programs: no {name} in this image (its toolchain was not built)");
            continue;
        }
        ran += 1;
        let dir = format!("/tmp/{name}-test");
        let _ = fs.mkdir(dir.clone());
        let root = root.map(|r| format!("VEDA_LINUX_ROOT={r}"));
        let env: alloc::vec::Vec<&str> = root.iter().map(String::as_str).collect();
        let (code, out) = run_with(&path, &[], &dir, &env, 120)?;
        for line in out.lines().filter(|l| !l.starts_with("ok - ")) {
            runtime::println!("{name}: {line}");
        }
        check(code == 0 && out.contains("PASS: 0 failed"), &format!("{name} checks failed (exit code {code})"))?;
        // The C checks replace themselves, in the same process.
        if name.starts_with("checks-musl") || name.starts_with("checks-glibc") {
            let (code, out) = run_with(&path, &["exec"], &dir, &env, 60)?;
            for line in out.lines().filter(|l| !l.starts_with("ok - ")) {
                runtime::println!("{name} exec: {line}");
            }
            let replaced = code == 0 && out.contains("ok - execve keeps the process");
            check(replaced, &format!("{name} exec failed (exit code {code})"))?;
        }
    }
    runtime::println!("linux programs: {ran} ran");
    Ok(())
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Where a Linux program's files are: its tree, as the POSIX layer maps
//! it onto the VFS's (`posix::host::Namespace`).
//!
//! A Linux program expects Linux's tree: its dynamic linker and libraries
//! in `/lib` and `/usr/lib`, its configuration in `/etc`. That is a *Linux
//! root*: a directory of the VFS's holding a Linux system's files (a
//! distribution's), which is the program's `/`; the user's files stay
//! where they are, so that a Linux program and Veda's applications share
//! them. So the program's `/home`, `/tmp`, `/dev` and `/system` are Veda's
//! own, and the rest of its tree is the root's. `/proc` and `/sys` are
//! Linux compatibility's to answer (`crate::proc`).
//!
//! The root is `VEDA_LINUX_ROOT`'s directory, if the program's environment
//! names one, or else `/home/linux`, if there is one. Without one, the
//! program's tree is Veda's own, with Veda's `/system/etc` as its `/etc`
//! (the users, groups and services there are Linux's databases), which is
//! enough for programs that need no files of a Linux system (statically
//! linked ones).

use alloc::string::String;
use core::sync::atomic::{AtomicPtr, Ordering};

use posix::host::Namespace;

/// Where the program's tree is Veda's own, whatever its root.
const VEDA: [&str; 4] = ["/home", "/tmp", "/dev", "/system"];

/// The default Linux root.
pub const DEFAULT_ROOT: &str = "/home/linux";

/// The variable of the program's environment that names its root.
pub const ROOT_VARIABLE: &[u8] = b"VEDA_LINUX_ROOT";

/// The VFS's path of the program's root (empty: Veda's own tree).
static ROOT: AtomicPtr<String> = AtomicPtr::new(core::ptr::null_mut());

fn root() -> &'static str {
    // SAFETY: only `set_root` stores here, a string it leaked.
    unsafe { ROOT.load(Ordering::Acquire).as_ref() }.map_or("", String::as_str)
}

/// Gives the program the root `root` (the VFS's absolute path of a
/// directory; empty for Veda's own tree), from now on.
pub fn set_root(root: &str) {
    let root = alloc::boxed::Box::leak(alloc::boxed::Box::new(String::from(root.trim_end_matches('/'))));
    ROOT.store(root, Ordering::Release);
    static NAMESPACE: Namespace = Namespace { to_vfs, to_program };
    posix::host::set_namespace(&NAMESPACE);
}

/// The program's root, as the VFS names it (empty: Veda's own tree).
pub fn current_root() -> &'static str {
    root()
}

/// Whether the program's path `path` is in `dir`, or is it.
fn under(path: &str, dir: &str) -> bool {
    path == dir || path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'))
}

/// The VFS's path of the program's path `path` (absolute, normalised).
pub fn to_vfs(path: &str) -> String {
    map_to_vfs(root(), path)
}

/// The program's path of the VFS's path `path` (absolute, normalised).
pub fn to_program(path: &str) -> String {
    map_to_program(root(), path)
}

fn map_to_vfs(root: &str, path: &str) -> String {
    if VEDA.iter().any(|dir| under(path, dir)) {
        return String::from(path);
    }
    if root.is_empty() {
        // Veda's own tree, with its databases where Linux has them.
        return match path.strip_prefix("/etc") {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => alloc::format!("/system/etc{rest}"),
            _ => String::from(path),
        };
    }
    if path == "/" { String::from(root) } else { alloc::format!("{root}{path}") }
}

fn map_to_program(root: &str, path: &str) -> String {
    if !root.is_empty()
        && let Some(rest) = path.strip_prefix(root)
    {
        if rest.is_empty() {
            return String::from("/");
        }
        if rest.starts_with('/') {
            return String::from(rest);
        }
    }
    String::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_holds_the_programs_tree_but_vedas_own_places() {
        let root = "/home/linux";
        assert_eq!(map_to_vfs(root, "/"), "/home/linux");
        assert_eq!(map_to_vfs(root, "/lib64/ld-linux-x86-64.so.2"), "/home/linux/lib64/ld-linux-x86-64.so.2");
        assert_eq!(map_to_vfs(root, "/etc/passwd"), "/home/linux/etc/passwd");
        assert_eq!(map_to_vfs(root, "/home/user/a.txt"), "/home/user/a.txt");
        assert_eq!(map_to_vfs(root, "/tmp"), "/tmp");
        assert_eq!(map_to_vfs(root, "/dev/null"), "/dev/null");
        assert_eq!(map_to_vfs(root, "/system/bin/gcc"), "/system/bin/gcc");
        // A name that only starts like one of Veda's places is the root's.
        assert_eq!(map_to_vfs(root, "/homes"), "/home/linux/homes");
        assert_eq!(map_to_program(root, "/home/linux"), "/");
        assert_eq!(map_to_program(root, "/home/linux/usr/bin"), "/usr/bin");
        assert_eq!(map_to_program(root, "/home/user"), "/home/user");
        assert_eq!(map_to_program(root, "/home/linuxx"), "/home/linuxx");
    }

    #[test]
    fn without_a_root_the_tree_is_vedas() {
        assert_eq!(map_to_vfs("", "/etc/passwd"), "/system/etc/passwd");
        assert_eq!(map_to_vfs("", "/etc"), "/system/etc");
        assert_eq!(map_to_vfs("", "/etcetera"), "/etcetera");
        assert_eq!(map_to_vfs("", "/home/user"), "/home/user");
        assert_eq!(map_to_vfs("", "/usr/lib"), "/usr/lib");
        assert_eq!(map_to_program("", "/system/etc/passwd"), "/system/etc/passwd");
    }
}

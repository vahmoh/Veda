// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Paths: resolving what a program names against its working directory.
//!
//! The file system service takes absolute, normalised UTF-8 paths. POSIX
//! paths are bytes, relative to the working directory (or a directory
//! descriptor), and may hold `.`, `..` and repeated or trailing slashes.
//! Veda has no symbolic links, so `..` can be resolved textually, once the
//! directories it climbs out of are known to be there, as POSIX resolves a
//! path component by component ([`Resolved::climbed`]: `missing/..` is no
//! path, nor is `file/..`).
//!
//! A program's paths are the VFS's, unless it runs in a namespace of its
//! own ([`Namespace`]): a Linux program's root is a directory of the VFS's,
//! which Veda's Linux compatibility arranges. Its names are resolved in
//! its own tree, where `..` stops at its root, and then become the VFS's;
//! the layer keeps the VFS's paths (the working directory, a directory's
//! descriptor), and gives the program its own ([`program_path`]).

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::linux::errno::{EILSEQ, ENAMETOOLONG, ENOENT};

/// Where a program's paths are in the VFS's tree, when the program has a
/// tree of its own: two maps between absolute, normalised paths, each the
/// other's inverse on the paths the program can name.
pub struct Namespace {
    /// The VFS's path of the program's path.
    pub to_vfs: fn(&str) -> String,
    /// The program's path of the VFS's path.
    pub to_program: fn(&str) -> String,
}

/// The program's namespace, if it has one of its own.
static NAMESPACE: AtomicPtr<Namespace> = AtomicPtr::new(core::ptr::null_mut());

/// Gives the program the tree `ns` describes, from now on: before it
/// names any path.
pub fn set_namespace(ns: &'static Namespace) {
    NAMESPACE.store(ns as *const Namespace as *mut Namespace, Ordering::Release);
}

fn namespace() -> Option<&'static Namespace> {
    // SAFETY: only `set_namespace` stores here, a reference that lives for
    // ever.
    unsafe { NAMESPACE.load(Ordering::Acquire).as_ref() }
}

/// The program's path for the VFS's `path` (the same, unless it has a
/// namespace of its own).
pub fn program_path(path: &str) -> String {
    match namespace() {
        Some(ns) => (ns.to_program)(path),
        None => String::from(path),
    }
}

/// The VFS's path for the program's absolute, normalised `path`.
pub fn vfs_path(path: &str) -> String {
    match namespace() {
        Some(ns) => (ns.to_vfs)(path),
        None => String::from(path),
    }
}

/// Longest path accepted (`PATH_MAX`, with its NUL).
pub const PATH_MAX: usize = 4096;
/// Longest name of a directory entry (`NAME_MAX`).
pub const NAME_MAX: usize = 255;

/// An absolute, normalised path, and whether the name ended with a slash
/// (which requires a directory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The VFS's path.
    pub path: String,
    pub dir_only: bool,
    /// The directories of the name that a `..` climbed out of, which must
    /// be there: the deepest of each climb (those it climbs to are its
    /// parents), as the VFS's paths.
    pub climbed: Vec<String>,
}

/// Resolves `name` against the absolute directory `base` (the VFS's path),
/// in the program's namespace.
pub fn resolve(base: &str, name: &[u8]) -> Result<Resolved, isize> {
    let Some(ns) = namespace() else { return resolve_in(base, name) };
    let mut r = resolve_in(&(ns.to_program)(base), name)?;
    r.path = (ns.to_vfs)(&r.path);
    for dir in &mut r.climbed {
        *dir = (ns.to_vfs)(dir);
    }
    Ok(r)
}

/// The program's own path for `name` against the absolute directory `base`
/// (the VFS's path), resolved in its namespace and left there.
pub fn resolve_program(base: &str, name: &[u8]) -> Result<String, isize> {
    resolve_in(&program_path(base), name).map(|r| r.path)
}

/// Resolves `name` against `base`, in one tree.
fn resolve_in(base: &str, name: &[u8]) -> Result<Resolved, isize> {
    if name.is_empty() {
        return Err(ENOENT);
    }
    if name.len() >= PATH_MAX {
        return Err(ENAMETOOLONG);
    }
    let name = core::str::from_utf8(name).map_err(|_| EILSEQ)?;
    let start = if name.starts_with('/') { "" } else { base };
    // The base is a directory that is there; the name's own components are
    // those after it.
    let mut comps: Vec<&str> = start.split('/').filter(|c| !c.is_empty()).collect();
    let mut known = comps.len();
    let mut climbed = Vec::new();
    let mut descended = false;
    for c in name.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                if descended && comps.len() > known {
                    climbed.push(join(&comps));
                }
                comps.pop();
                known = known.min(comps.len());
                descended = false;
            }
            c if c.len() > NAME_MAX => return Err(ENAMETOOLONG),
            c => {
                comps.push(c);
                descended = true;
            }
        }
    }
    let dir_only = name.ends_with('/') || name.ends_with("/.") || name.ends_with("/..") || name == "." || name == "..";
    let path = join(&comps);
    if path.len() >= PATH_MAX {
        return Err(ENAMETOOLONG);
    }
    Ok(Resolved { path, dir_only, climbed })
}

/// The absolute path of `comps` (`/` for none).
fn join(comps: &[&str]) -> String {
    let mut path = String::with_capacity(comps.iter().map(|c| c.len() + 1).sum::<usize>().max(1));
    for c in comps {
        path.push('/');
        path.push_str(c);
    }
    if path.is_empty() {
        path.push('/');
    }
    path
}

/// The directory holding `path` (`/` for `/` itself).
pub fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

/// The last component of `path` (empty for `/`).
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(base: &str, name: &str) -> Result<(String, bool), isize> {
        resolve(base, name.as_bytes()).map(|r| (r.path, r.dir_only))
    }

    #[test]
    fn resolves_relative_and_absolute_names() {
        assert_eq!(r("/home/user", "a.c"), Ok(("/home/user/a.c".into(), false)));
        assert_eq!(r("/home/user", "src/../b.c"), Ok(("/home/user/b.c".into(), false)));
        assert_eq!(r("/home/user", "/system//bin/./gcc"), Ok(("/system/bin/gcc".into(), false)));
        assert_eq!(r("/", ".."), Ok(("/".into(), true)));
        assert_eq!(r("/a/b", "../../../.."), Ok(("/".into(), true)));
        assert_eq!(r("/tmp", "."), Ok(("/tmp".into(), true)));
        assert_eq!(r("/tmp", "dir/"), Ok(("/tmp/dir".into(), true)));
        assert_eq!(r("/tmp", "dir/."), Ok(("/tmp/dir".into(), true)));
    }

    #[test]
    fn knows_the_directories_dot_dot_climbs_out_of() {
        let climbed = |base: &str, name: &str| resolve(base, name.as_bytes()).unwrap().climbed;
        assert_eq!(climbed("/home/user", "src/../b.c"), ["/home/user/src"]);
        // The deepest of a climb, and of each climb.
        assert_eq!(climbed("/tmp", "a/b/../../c/../d"), ["/tmp/a/b", "/tmp/c"]);
        // Nothing for the base's own (it is there), or a name without `..`.
        assert!(climbed("/home/user", "../..").is_empty());
        assert!(climbed("/", "/system/bin/gcc").is_empty());
        assert_eq!(climbed("/tmp", "/x/.."), ["/x"]);
    }

    #[test]
    fn refuses_bad_names() {
        assert_eq!(r("/", ""), Err(ENOENT));
        assert_eq!(resolve("/", b"\xff"), Err(EILSEQ));
        let long = "x".repeat(256);
        assert_eq!(r("/", &long), Err(ENAMETOOLONG));
        let deep = "a/".repeat(2100);
        assert_eq!(r("/", &deep), Err(ENAMETOOLONG));
    }

    #[test]
    fn namespaces_resolve_in_the_programs_tree() {
        use alloc::vec;
        // `/` of the program's is `/r`, but for `/home`.
        fn to_vfs(p: &str) -> String {
            if p == "/home" || p.starts_with("/home/") { p.into() } else { alloc::format!("/r{p}") }
        }
        fn to_program(p: &str) -> String {
            match p.strip_prefix("/r") {
                Some("") => "/".into(),
                Some(rest) if rest.starts_with('/') => rest.into(),
                _ => p.into(),
            }
        }
        let names = [("/r/usr", "../../../etc"), ("/r/usr", "/lib/x"), ("/home/user", "../../usr"), ("/r", "a/../b")];
        let mut out = Vec::new();
        for (base, name) in names {
            let ns = Namespace { to_vfs, to_program };
            let r = resolve_in(&(ns.to_program)(base), name.as_bytes()).unwrap();
            out.push(((ns.to_vfs)(&r.path), r.climbed.iter().map(|c| (ns.to_vfs)(c)).collect::<Vec<_>>()));
        }
        assert_eq!(out[0], ("/r/etc".into(), vec![]));
        assert_eq!(out[1], ("/r/lib/x".into(), vec![]));
        assert_eq!(out[2], ("/r/usr".into(), vec![]));
        assert_eq!(out[3], ("/r/b".into(), vec!["/r/a".into()]));
    }

    #[test]
    fn splits_paths() {
        assert_eq!(parent("/home/user/a.c"), "/home/user");
        assert_eq!(parent("/home"), "/");
        assert_eq!(parent("/"), "/");
        assert_eq!(file_name("/home/user/a.c"), "a.c");
        assert_eq!(file_name("/"), "");
    }
}

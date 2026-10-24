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

use alloc::string::String;
use alloc::vec::Vec;

use crate::linux::errno::{EILSEQ, ENAMETOOLONG, ENOENT};

/// Longest path accepted (`PATH_MAX`, with its NUL).
pub const PATH_MAX: usize = 4096;
/// Longest name of a directory entry (`NAME_MAX`).
pub const NAME_MAX: usize = 255;

/// An absolute, normalised path, and whether the name ended with a slash
/// (which requires a directory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub path: String,
    pub dir_only: bool,
    /// The directories of the name that a `..` climbed out of, which must
    /// be there: the deepest of each climb (those it climbs to are its
    /// parents).
    pub climbed: Vec<String>,
}

/// Resolves `name` against the absolute directory `base`.
pub fn resolve(base: &str, name: &[u8]) -> Result<Resolved, isize> {
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
    fn splits_paths() {
        assert_eq!(parent("/home/user/a.c"), "/home/user");
        assert_eq!(parent("/home"), "/");
        assert_eq!(parent("/"), "/");
        assert_eq!(file_name("/home/user/a.c"), "a.c");
        assert_eq!(file_name("/"), "");
    }
}

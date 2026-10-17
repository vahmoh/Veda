// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Where the agent's file actions may act.
//!
//! The agent service is a system service, and the file system lets it into
//! its own private directory (`/home/.private/agent`), where its settings,
//! the Deepgram key, memory and permissions live. Its file actions act for
//! the user, with paths that come from the language model, from documents
//! and from the Trash's records (which any program can write): every path
//! they act on — sources and destinations alike — is resolved here, and
//! none may lie in a private directory. What they create goes in the home
//! folder or `/tmp`.
//!
//! [`destination`] works out where a copy, move or rename puts an item
//! exactly as it is then done, so that what the agent is told before it
//! acts is what happens.

use alloc::format;
use alloc::string::String;

use files::path::{self, file_name, is_within, join, parent};

/// The user's home folder: `~`, and where relative paths start.
pub const HOME: &str = path::HOME;
/// The directory of the services' private directories.
const PRIVATE: &str = "/home/.private";

/// A path the agent may act on: `path` (`~/…`, absolute, or relative to the
/// home folder) normalised, outside every private directory.
pub fn resolve(path: &str) -> Result<String, String> {
    let p = path::resolve(HOME, path.trim());
    if is_within(&p, PRIVATE) {
        return Err("that location is private".into());
    }
    Ok(p)
}

/// A path the agent may create or change something at: as [`resolve`], in
/// the home folder or `/tmp`.
pub fn writable(path: &str) -> Result<String, String> {
    let p = resolve(path)?;
    if (is_within(&p, "/home") && p != "/home") || (is_within(&p, "/tmp") && p != "/tmp") {
        Ok(p)
    } else {
        Err("files can only be written in the home folder".into())
    }
}

/// Where copying, moving or renaming (`op`) the item at `from` (resolved)
/// to `to` puts it, as it is then done: a rename to a bare name stays in the
/// item's folder; a destination that is a folder (`is_dir`) receives the
/// item under its own name. The result is [`writable`].
pub fn destination(op: &str, from: &str, to: &str, is_dir: &dyn Fn(&str) -> bool) -> Result<String, String> {
    let to = to.trim();
    if to.is_empty() {
        return Err("where to?".into());
    }
    let dest = if op == "rename" && !to.contains('/') && !to.starts_with('~') {
        if to == "." || to == ".." {
            return Err(format!("{to} is not a name"));
        }
        writable(&join(parent(from), to))?
    } else {
        writable(to)?
    };
    if is_dir(&dest) {
        let name = file_name(from);
        if name.is_empty() {
            return Err("that cannot be moved".into());
        }
        return writable(&join(&dest, name));
    }
    Ok(dest)
}

/// Where restoring an item from the Trash puts it: the place it was deleted
/// from, as its record in the Trash says (a record any program can write),
/// which must be a path inside the home folder that the agent may act on.
pub fn restore_target(origin: &str) -> Result<String, String> {
    let unknown = || String::from("where it was deleted from is not known; restore it with the Files app");
    if !origin.starts_with('/') {
        return Err(unknown());
    }
    let p = resolve(origin).map_err(|_| unknown())?;
    if !is_within(&p, HOME) || p == HOME || file_name(&p).is_empty() {
        return Err(unknown());
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_directories_are_out_of_reach() {
        assert_eq!(resolve("~/Documents/a.txt").unwrap(), "/home/user/Documents/a.txt");
        assert_eq!(resolve("Documents").unwrap(), "/home/user/Documents");
        for p in [
            "/home/.private",
            "/home/.private/agent/permissions.json",
            "~/../.private/agent",
            "/home/user/../.private/agent/config.json",
            "../.private",
        ] {
            assert!(resolve(p).is_err(), "{p}");
        }
        assert!(writable("/tmp/x").is_ok() && writable("~/x").is_ok());
        assert!(writable("/system/apps/x").is_err() && writable("/home").is_err() && writable("/").is_err());
    }

    #[test]
    fn destinations_are_worked_out_as_they_are_done() {
        let dirs = |p: &str| p == "/home/user/Documents" || p == "/home" || p == "/home/.private";
        let d = |op, from, to| destination(op, from, to, &dirs);
        // A bare name renames in place; a folder receives the item.
        assert_eq!(d("rename", "/home/user/Pictures/a.png", "b.png").unwrap(), "/home/user/Pictures/b.png");
        assert_eq!(d("move", "/home/user/a.txt", "~/Documents").unwrap(), "/home/user/Documents/a.txt");
        assert_eq!(d("rename", "/home/user/a.txt", "Documents/b.txt").unwrap(), "/home/user/Documents/b.txt");
        assert_eq!(d("copy", "/home/user/a.txt", "/tmp/c.txt").unwrap(), "/tmp/c.txt");
        // Nothing reaches a private directory, however it is named.
        assert!(d("rename", "/home/agent", ".private").is_err());
        assert!(d("move", "/home/user/x.json", "/home/.private/agent/permissions.json").is_err());
        assert!(d("move", "/home/user/permissions.json", "~/../.private/agent").is_err());
        assert!(d("rename", "/home/user/a", "..").is_err());
        assert!(d("rename", "/home/user/a", "  ").is_err());
        // Out of the home folder only to /tmp.
        assert!(d("move", "/home/user/a", "/system/a").is_err());
    }

    #[test]
    fn restores_only_into_the_home_folder() {
        assert_eq!(restore_target("/home/user/Documents/Notes.txt").unwrap(), "/home/user/Documents/Notes.txt");
        for origin in [
            "/home/.private/agent/permissions.json",
            "/home/user/../.private/agent/timers.json",
            "/home/.private/agent/config.json",
            "/tmp/x",
            "/system/x",
            "/home/user",
            "relative/path",
            "",
        ] {
            assert!(restore_target(origin).is_err(), "{origin}");
        }
    }
}

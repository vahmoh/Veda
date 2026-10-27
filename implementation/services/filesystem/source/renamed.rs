// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Private directories under the old names of services that were renamed
//! (`netd` is `network` now, `packages` is `package-manager`). A home made
//! before keeps the firewall's settings in `/home/.private/netd` and what
//! is installed in `/home/.private/packages`, which only a service of that
//! name may reach, and none has the name any more. As the file system
//! starts, before any service reaches `/home`, each moves to its service's
//! new name ([`move_private`]).
//!
//! A directory moves only where nothing is lost: the new name is free, or
//! an empty directory has it (its service made it on a start before this
//! move was made). Otherwise both stay as they are. The move is a change
//! to `/home` like any other: until the home disk keeps it, the next start
//! makes it again.

use alloc::vec::Vec;

use proto::fs::FsError;

use crate::HOME;
use crate::access::PRIVATE;
use crate::tree::{Kind, Tree};

/// The services renamed: each one's old name, and its new one.
pub const RENAMED: [(&str, &str); 2] = [("netd", "network"), ("packages", "package-manager")];

/// A renamed service's old private directory, and what became of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moved {
    /// The service's old name.
    pub from: &'static str,
    /// Its new name.
    pub to: &'static str,
    /// Whether the directory has the new name now; if not, why (`Exists`:
    /// what has the new name holds something that would be lost).
    pub result: Result<(), FsError>,
}

/// Moves the old private directories that `home` (the writable file
/// system) has to their services' new names, in [`RENAMED`]'s order: each
/// that was there, moved or not.
pub fn move_private(home: &mut Tree) -> Vec<Moved> {
    let mut found = Vec::new();
    for (from, to) in RENAMED {
        let (old, new) = ([HOME, PRIVATE, from], [HOME, PRIVATE, to]);
        if home.lookup(&old).is_err() {
            continue;
        }
        // An empty directory with the new name gives way; anything else
        // with it stays, and so does the old directory.
        let empty = home
            .lookup(&new)
            .ok()
            .and_then(|id| home.node(id))
            .map(|n| matches!(&n.kind, Kind::Dir(entries) if entries.is_empty()));
        let result = match empty {
            Some(false) => Err(FsError::Exists),
            _ => home.rename(&old, &new, empty == Some(true)).map(|replaced| {
                // No service has started yet, so nothing has it open.
                if let Some(id) = replaced {
                    home.forget(id);
                }
            }),
        };
        found.push(Moved { from, to, result });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use core::sync::atomic::{AtomicU64, Ordering};

    use crate::snapshot::{self, Samples};
    use crate::tree::Data;

    fn clock() -> u64 {
        static NOW: AtomicU64 = AtomicU64::new(1_000);
        NOW.fetch_add(1, Ordering::Relaxed)
    }

    /// A writable file system as the file system starts with: `/tmp` and
    /// the user's home.
    fn fresh() -> Tree {
        let mut t = Tree::new(false, 1, clock);
        t.mkdir_all(&["tmp"]).unwrap();
        t.mkdir_all(&["home", "user"]).unwrap();
        t
    }

    /// A home as a system before the renaming left it: the firewall's
    /// settings, what is installed (and its files in `/home/packages`, the
    /// other `packages`), and the agent's directory, whose service kept its
    /// name.
    fn home_before() -> Tree {
        let mut t = fresh();
        for (path, bytes) in [
            (&["home", ".private", "netd", "firewall"][..], &b"settings"[..]),
            (&["home", ".private", "packages", "installed.json"], b"{}"),
            (&["home", ".private", "packages", "indexes", "system"], b"index"),
            (&["home", ".private", "agent", "memory.json"], b"[]"),
            (&["home", "packages", "bin", "hello"], b"program"),
        ] {
            t.mkdir_all(&path[..path.len() - 1]).unwrap();
            t.add_file(path, Data::owned(bytes.to_vec())).unwrap();
        }
        t
    }

    /// What the file at `path` holds, if there is one.
    fn file(t: &Tree, path: &[&str]) -> Option<Vec<u8>> {
        match &t.node(t.lookup(path).ok()?)?.kind {
            Kind::File(d) => Some(d.bytes().to_vec()),
            _ => None,
        }
    }

    fn moved(from: &'static str, to: &'static str) -> Moved {
        Moved { from, to, result: Ok(()) }
    }

    #[test]
    fn old_directories_take_their_services_new_names() {
        let mut t = home_before();
        assert_eq!(move_private(&mut t), [moved("netd", "network"), moved("packages", "package-manager")]);
        assert_eq!(file(&t, &["home", ".private", "network", "firewall"]), Some(b"settings".to_vec()));
        assert_eq!(file(&t, &["home", ".private", "package-manager", "installed.json"]), Some(b"{}".to_vec()));
        assert_eq!(file(&t, &["home", ".private", "package-manager", "indexes", "system"]), Some(b"index".to_vec()));
        assert!(t.lookup(&["home", ".private", "netd"]).is_err());
        assert!(t.lookup(&["home", ".private", "packages"]).is_err());
        // What is not a renamed service's stays where it is.
        assert_eq!(file(&t, &["home", ".private", "agent", "memory.json"]), Some(b"[]".to_vec()));
        assert_eq!(file(&t, &["home", "packages", "bin", "hello"]), Some(b"program".to_vec()));
        // Moved once, they stay moved.
        assert!(move_private(&mut t).is_empty());
    }

    #[test]
    fn a_home_without_old_names_is_left_alone() {
        let mut t = fresh();
        t.mkdir_all(&["home", ".private", "network"]).unwrap();
        assert!(move_private(&mut t).is_empty());
        assert!(t.lookup(&["home", ".private", "network"]).is_ok());
    }

    #[test]
    fn nothing_with_a_new_name_already_is_lost() {
        let mut t = home_before();
        // The network service saved settings of its own on a start before
        // the move; the package manager only made its directory.
        t.mkdir_all(&["home", ".private", "network"]).unwrap();
        t.add_file(&["home", ".private", "network", "firewall"], Data::owned(b"newer".to_vec())).unwrap();
        t.mkdir_all(&["home", ".private", "package-manager"]).unwrap();
        assert_eq!(
            move_private(&mut t),
            [Moved { from: "netd", to: "network", result: Err(FsError::Exists) }, moved("packages", "package-manager")]
        );
        assert_eq!(file(&t, &["home", ".private", "network", "firewall"]), Some(b"newer".to_vec()));
        assert_eq!(file(&t, &["home", ".private", "netd", "firewall"]), Some(b"settings".to_vec()));
        assert_eq!(file(&t, &["home", ".private", "package-manager", "installed.json"]), Some(b"{}".to_vec()));
        assert!(t.lookup(&["home", ".private", "packages"]).is_err());
        // A file with a new name is something too.
        let mut t = home_before();
        t.add_file(&["home", ".private", "network"], Data::owned(vec![1])).unwrap();
        assert_eq!(move_private(&mut t)[0].result, Err(FsError::Exists));
        assert_eq!(file(&t, &["home", ".private", "network"]), Some(vec![1]));
    }

    #[test]
    fn a_home_disk_written_before_is_moved_once_read() {
        // What a system before the renaming wrote to the home disk, read
        // back as the file system starts.
        let samples = Samples::new([]);
        let mut bytes = Vec::new();
        snapshot::write(&home_before(), &samples, &mut bytes);
        let mut t = fresh();
        snapshot::read(&mut &bytes[..], bytes.len() as u64, &mut t, &samples).unwrap();
        assert_eq!(move_private(&mut t), [moved("netd", "network"), moved("packages", "package-manager")]);
        // The home disk keeps the new names from the next save on.
        let mut saved = Vec::new();
        snapshot::write(&t, &samples, &mut saved);
        let mut next = fresh();
        snapshot::read(&mut &saved[..], saved.len() as u64, &mut next, &samples).unwrap();
        assert!(move_private(&mut next).is_empty());
        assert_eq!(file(&next, &["home", ".private", "network", "firewall"]), Some(b"settings".to_vec()));
        assert_eq!(file(&next, &["home", ".private", "package-manager", "installed.json"]), Some(b"{}".to_vec()));
    }
}

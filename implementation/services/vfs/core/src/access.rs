// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Who may reach what, and how much of the VFS a program may hold.
//!
//! **Private directories.** `/home/.private/<service>` belongs to the
//! system service of that name (as the registry identifies the client):
//! only it reaches what is inside, only services reach `/home/.private`
//! itself, and listings show a client no private directory but its own. A
//! service may give that up for a connection (`vfs::renounce_private`),
//! which is then treated as an application's.
//!
//! **Fixed places.** The root, `/home`, the user's home, `/tmp` and
//! `/home/.private` stay where they are: nothing removes, moves or replaces
//! them. Everything counts on them, and the private directories are
//! private only where the guard looks for them, so moving `/home` would
//! take them out from under it (and would leave the home disk nothing to
//! keep).
//!
//! **Connections.** Every connection, a client's or an open file's, is a
//! handle in the VFS's one wait set; [`Connections`] keeps their number
//! where the kernel waits on all of them together, and keeps any one
//! program from taking most of them.

use alloc::collections::BTreeMap;

use proto::fs::FsError;

use crate::{HOME, TMP, USER};

/// The directory in `/home` that holds the services' private directories.
pub const PRIVATE: &str = ".private";

/// Who a client is, as far as reaching private directories goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Client<'a> {
    /// A system service that has not renounced its private directory.
    pub service: bool,
    /// Its name: a service's private directory has it.
    pub name: &'a str,
}

impl Client<'_> {
    /// Whether the client may reach `comps`, a path of the writable file
    /// system.
    pub fn may_reach(&self, comps: &[&str]) -> bool {
        match comps {
            [HOME, PRIVATE] => self.service,
            [HOME, PRIVATE, owner, ..] => self.service && *owner == self.name,
            _ => true,
        }
    }

    /// Whether a listing of directory `comps` (of the writable file system)
    /// shows the client the entry `name`.
    pub fn sees(&self, comps: &[&str], name: &str) -> bool {
        match comps {
            [HOME] => name != PRIVATE || self.service,
            [HOME, PRIVATE] => self.service && name == self.name,
            _ => true,
        }
    }
}

/// Whether `comps` (a path of the writable file system) is one of the
/// places that stay where they are.
pub fn fixed(comps: &[&str]) -> bool {
    matches!(comps, [] | [HOME] | [HOME, USER] | [HOME, PRIVATE] | [TMP])
}

/// Connections the VFS serves at once, of all kinds. Each is a handle in its
/// one wait set, and the kernel waits on at most `abi::WAIT_MANY_MAX`
/// handles together (a `ipc::WaitSet` with more polls them), so this limit
/// follows the wait set's: a kernel wait port, which would take any number
/// of handles, would lift it. The listener for new clients takes one.
pub const MAX_CONNECTIONS: usize = abi::WAIT_MANY_MAX - 1;

/// Connections of one program (its client connections and the open files
/// it opened): a quarter of all, so that no program can take them all.
pub const MAX_PER_PROGRAM: usize = MAX_CONNECTIONS / 4;

/// How many connections each program has, by process id.
#[derive(Debug, Default)]
pub struct Connections {
    per_program: BTreeMap<u64, usize>,
    total: usize,
}

impl Connections {
    /// No connections yet.
    pub fn new() -> Connections {
        Connections::default()
    }

    /// Counts a new connection of program `koid`, or refuses it with
    /// `TooMany` when the program, or the VFS, has as many as it may.
    pub fn admit(&mut self, koid: u64) -> Result<(), FsError> {
        let mine = self.per_program.entry(koid).or_default();
        if self.total >= MAX_CONNECTIONS || *mine >= MAX_PER_PROGRAM {
            if *mine == 0 {
                self.per_program.remove(&koid);
            }
            return Err(FsError::TooMany);
        }
        *mine += 1;
        self.total += 1;
        Ok(())
    }

    /// Forgets a connection of program `koid` that closed.
    pub fn release(&mut self, koid: u64) {
        if let Some(n) = self.per_program.get_mut(&koid) {
            *n -= 1;
            if *n == 0 {
                self.per_program.remove(&koid);
            }
            self.total -= 1;
        }
    }

    /// The connections open now.
    pub fn total(&self) -> usize {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT: Client = Client { service: true, name: "agent" };
    const SHELL: Client = Client { service: true, name: "shell" };
    const APP: Client = Client { service: false, name: "agent" };

    #[test]
    fn private_directories_are_their_services_alone() {
        let key = [HOME, PRIVATE, "agent", "key"];
        assert!(AGENT.may_reach(&key) && AGENT.may_reach(&[HOME, PRIVATE]));
        assert!(!SHELL.may_reach(&key) && SHELL.may_reach(&[HOME, PRIVATE]));
        // An application named like the service, or the service once it
        // has renounced its access, is an application.
        let renounced = Client { service: false, ..AGENT };
        for c in [APP, renounced] {
            assert!(!c.may_reach(&key) && !c.may_reach(&[HOME, PRIVATE, "agent"]) && !c.may_reach(&[HOME, PRIVATE]));
            assert!(c.may_reach(&[HOME, USER, PRIVATE]) && c.may_reach(&[HOME]) && c.may_reach(&[]));
        }
        // Listings show each its own.
        assert!(AGENT.sees(&[HOME], PRIVATE) && !APP.sees(&[HOME], PRIVATE) && APP.sees(&[HOME], USER));
        assert!(AGENT.sees(&[HOME, PRIVATE], "agent") && !AGENT.sees(&[HOME, PRIVATE], "shell"));
        assert!(!APP.sees(&[HOME, PRIVATE], "agent"));
        assert!(APP.sees(&[HOME, USER], PRIVATE));
    }

    #[test]
    fn the_namespace_stays_put() {
        for fixed_place in [&[][..], &[HOME], &[HOME, USER], &[HOME, PRIVATE], &[TMP]] {
            assert!(fixed(fixed_place), "{fixed_place:?}");
        }
        for free in [&[HOME, USER, "Documents"][..], &[HOME, PRIVATE, "agent"], &[TMP, "x"], &["elsewhere"]] {
            assert!(!fixed(free), "{free:?}");
        }
    }

    #[test]
    fn connections_are_bounded_per_program_and_in_all() {
        let mut c = Connections::new();
        for _ in 0..MAX_PER_PROGRAM {
            c.admit(1).unwrap();
        }
        assert_eq!(c.admit(1), Err(FsError::TooMany));
        // Others still get theirs, until there are no more.
        let mut koid = 2;
        while c.total() < MAX_CONNECTIONS {
            if c.admit(koid).is_err() {
                koid += 1;
            }
        }
        assert_eq!(c.admit(koid + 1), Err(FsError::TooMany));
        // A connection closing makes room.
        c.release(1);
        assert_eq!(c.admit(1), Ok(()));
        assert_eq!(c.admit(koid + 1), Err(FsError::TooMany));
        c.release(koid);
        assert_eq!(c.admit(koid + 1), Ok(()));
        // Releasing what was never admitted changes nothing.
        let total = c.total();
        c.release(9_999);
        assert_eq!(c.total(), total);
    }
}

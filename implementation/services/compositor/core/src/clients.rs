// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The window system's clients: who each connection is, as `init` vouches
//! for it (`vproto::init::ClientIdentity`), and so what it may do.
//!
//! * The desktop shell (the system service named `shell`) draws the
//!   desktop, the taskbar and notifications, and manages every window:
//!   it lists, activates, minimises, arranges and closes them, and hears
//!   the shell's keys (the start menu's and the agent's).
//! * The voice agent (the system service named `agent`) may list the
//!   windows, to tell the user what is on the screen, lock the screen, and
//!   say that the user is there while they talk with it (see
//!   [`crate::idle`]).
//! * The lock screen (the system service named `lock`) draws the lock
//!   screen, and alone unlocks it (see [`crate::lock`]) and sets how long
//!   the screen waits without input before it locks (see [`crate::idle`]).
//! * Every other client has its own windows and nothing else (which may
//!   keep the screen awake while they can be seen).
//!
//! Which process a connection belongs to counts too: limits are each
//! process's, whatever number of connections it opens, and the clipboard
//! is the focused window's process's.

use alloc::string::String;

/// What a client may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Shell,
    Agent,
    Lock,
    App,
}

/// Who opened a connection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Identity {
    /// Its process (0 if `init` does not know it).
    pub koid: u64,
    /// Its process's name.
    pub name: String,
    /// Started by `init` as a system service.
    pub service: bool,
}

impl Identity {
    /// What it may do.
    pub fn role(&self) -> Role {
        match (self.service, self.name.as_str()) {
            (true, "shell") => Role::Shell,
            (true, "agent") => Role::Agent,
            (true, "lock") => Role::Lock,
            _ => Role::App,
        }
    }

    /// Whether it is the same process as `other` (known processes only).
    pub fn same_process(&self, other: &Identity) -> bool {
        self.koid != 0 && self.koid == other.koid
    }
}

/// A connection to the window system.
#[derive(Debug, Clone)]
pub struct Client {
    pub identity: Identity,
    pub role: Role,
    /// When it last asked for something (ns): it is alive then.
    pub last_request: u64,
}

impl Client {
    /// A client who connected at `now`.
    pub fn new(identity: Identity, now: u64) -> Client {
        Client { role: identity.role(), identity, last_request: now }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who(name: &str, service: bool, koid: u64) -> Identity {
        Identity { koid, name: name.into(), service }
    }

    #[test]
    fn only_system_services_have_their_roles() {
        assert_eq!(who("shell", true, 5).role(), Role::Shell);
        assert_eq!(who("agent", true, 6).role(), Role::Agent);
        assert_eq!(who("lock", true, 9).role(), Role::Lock);
        // An application of the same name is an application.
        assert_eq!(who("shell", false, 7).role(), Role::App);
        assert_eq!(who("lock", false, 10).role(), Role::App);
        assert_eq!(who("files", false, 8).role(), Role::App);
    }

    #[test]
    fn unknown_processes_are_nobody_else() {
        assert!(who("a", false, 5).same_process(&who("a", false, 5)));
        assert!(!who("a", false, 5).same_process(&who("a", false, 6)));
        assert!(!who("a", false, 0).same_process(&who("a", false, 0)));
    }
}

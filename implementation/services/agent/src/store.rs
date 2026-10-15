// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The agent's private directory, `/home/.private/agent`, on the home disk.
//! Only the agent service can open it (the file system checks who is
//! asking), so the Deepgram key and what the agent remembers about the user
//! stay out of reach of other programs.
//!
//! The store has a connection to the file system of its own, the only one
//! that needs the agent's access to its private directory: the actions the
//! agent runs for the user act through another (see `worker::user_files`),
//! which can give that access up.
//!
//! A file is replaced in one step: written under a temporary name, which
//! then takes the file's place (the file system's `replace`), so a crash
//! leaves the old file or the new one, never neither.

use alloc::string::String;

use vfiles::fs::{Error, Fs, FsError};

/// The agent's private directory.
pub const DIR: &str = "/home/.private/agent";

/// The stored files: the settings, the Deepgram key, memory, the
/// permissions and the timers.
pub const CONFIG: &str = "config.json";
/// The Deepgram key.
pub const KEY: &str = "deepgram-key";
/// What the agent remembers about the user.
pub const MEMORY: &str = "memory.json";
/// The actions the user always allows.
pub const PERMISSIONS: &str = "permissions.json";
/// Timers and reminders.
pub const TIMERS: &str = "timers.json";

/// The private directory, through a connection of its own.
pub struct Store {
    fs: Fs,
}

impl Store {
    /// Opens (and creates) the private directory.
    pub fn open() -> Store {
        let fs = Fs::connect();
        if let Err(e) = fs.mkdir_all(DIR) {
            vrt::println!("cannot create {}: {}", DIR, e);
        }
        Store { fs }
    }

    fn path(name: &str) -> String {
        alloc::format!("{DIR}/{name}")
    }

    /// Where a new version of `name` is written before it takes its place.
    fn temporary(name: &str) -> String {
        alloc::format!("{DIR}/{name}.new")
    }

    /// A stored text file, if it exists. A file that is missing while a new
    /// version of it is there was being replaced when the agent stopped (by
    /// an older agent, which removed the file before renaming its new
    /// version): the new version, complete, takes its place.
    pub fn read(&self, name: &str) -> Option<String> {
        let bytes = match self.fs.read(&Self::path(name)) {
            Ok(bytes) => bytes,
            Err(Error::Fs(FsError::NotFound)) => {
                let bytes = self.fs.read(&Self::temporary(name)).ok()?;
                vrt::println!("recovered {} from an interrupted save", name);
                self.replace(name);
                bytes
            }
            Err(_) => return None,
        };
        String::from_utf8(bytes).ok()
    }

    /// Replaces a stored file in one step (see the module documentation).
    pub fn write(&self, name: &str, text: &str) -> bool {
        if let Err(e) = self.fs.write(&Self::temporary(name), text.as_bytes()) {
            vrt::println!("cannot save {}: {}", name, e);
            return false;
        }
        self.replace(name)
    }

    /// Moves the new version of `name` into its place.
    fn replace(&self, name: &str) -> bool {
        let result = match self.fs.client() {
            Some(c) => match c.replace(Self::temporary(name), Self::path(name)) {
                Ok(Ok(())) => return true,
                Ok(Err(e)) => Error::Fs(e),
                Err(_) => Error::Ipc,
            },
            None => Error::Unavailable,
        };
        vrt::println!("cannot save {}: {}", name, result);
        false
    }

    /// Removes a stored file, and any new version of it a failed save left
    /// (which [`Store::read`] would otherwise take for the file).
    pub fn remove(&self, name: &str) {
        let _ = self.fs.remove(&Self::temporary(name));
        let _ = self.fs.remove(&Self::path(name));
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the lock service keeps, in its private directory
//! (`/home/.private/lock`), which no other program sees:
//!
//! * `password`: the password's record (`lock_core::password`), when one
//!   is set;
//! * `throttle`: the wrong passwords given since the right one, and when
//!   the wait they make ends (`lock_core::throttle`);
//! * `idle`: how long the screen waits without input before it locks, when
//!   the user has chosen (`lock_core::idle`).
//!
//! A file is written whole under another name first, then put in place
//! (`vfs::replace`), so that a record is never seen half written; the
//! password's record is read back once written, and taken only if it reads
//! as it was written.

use lock_core::idle;
use lock_core::password::{MAX_RECORD, Record, RecordError};
use lock_core::throttle::Throttle;
use proto::fs::FsError;
use proto::vfs;
use runtime::object::Vmo;
use runtime::println;

/// The lock service's private directory.
const DIR: &str = "/home/.private/lock";
const PASSWORD: &str = "/home/.private/lock/password";
const THROTTLE: &str = "/home/.private/lock/throttle";
const IDLE: &str = "/home/.private/lock/idle";
/// The longest throttle and idle records read (bytes).
const MAX_THROTTLE: u64 = 64;
const MAX_IDLE: u64 = 16;
/// How long the store waits for the file system before it gives up.
const VFS_TIMEOUT_NS: u64 = 10_000_000_000;

/// The password's record as it was found.
#[derive(Debug)]
pub enum Found {
    /// None is set.
    None,
    /// One is set.
    Set(Record),
    /// The file is there but is not a record that is taken: a password is
    /// set, which none typed matches (the screen stays locked rather than
    /// open to anyone).
    Damaged(RecordError),
}

/// The private directory.
pub struct Store {
    vfs: Option<vfs::Client>,
}

/// Why something could not be kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteError;

impl Store {
    /// Connects to the file system (waiting for it, as it starts first).
    pub fn open() -> Store {
        let vfs = proto::connect(vfs::NAME).ok().map(vfs::Client::new);
        if let Some(v) = &vfs {
            v.set_timeout(VFS_TIMEOUT_NS);
            let _ = v.mkdir(DIR.into());
        } else {
            println!("no file system: no password can be checked or kept");
        }
        Store { vfs }
    }

    /// The password's record. Without the file system, one may be set: as
    /// one that cannot be read, so that the screen stays locked.
    pub fn password(&self) -> Found {
        let Some(vfs) = &self.vfs else { return Found::Damaged(RecordError::Malformed) };
        match vfs.stat(PASSWORD.into()) {
            Ok(Err(FsError::NotFound)) => return Found::None,
            Ok(Ok(_)) => {}
            // The file system did not say: as damaged, so that the screen
            // stays locked.
            _ => return Found::Damaged(RecordError::Malformed),
        }
        let text = shell_common::read_file(vfs, PASSWORD, MAX_RECORD as u64).and_then(|b| String::from_utf8(b).ok());
        match text.as_deref().map(Record::parse) {
            Some(Ok(r)) => Found::Set(r),
            Some(Err(e)) => Found::Damaged(e),
            None => Found::Damaged(RecordError::Malformed),
        }
    }

    /// Keeps `record` as the password's (or removes the password's, for
    /// `None`).
    pub fn set_password(&self, record: Option<&Record>) -> Result<(), WriteError> {
        let vfs = self.vfs.as_ref().ok_or(WriteError)?;
        let Some(record) = record else {
            return match vfs.remove(PASSWORD.into()) {
                Ok(Ok(())) | Ok(Err(FsError::NotFound)) => Ok(()),
                _ => Err(WriteError),
            };
        };
        write(vfs, PASSWORD, record.as_str().as_bytes())?;
        // Read back: what is kept must be what was made.
        match self.password() {
            Found::Set(back) if back == *record => Ok(()),
            _ => Err(WriteError),
        }
    }

    /// The wrong passwords given and the wait they make, at `now` (Unix
    /// time, s).
    pub fn throttle(&self, now: u64) -> Throttle {
        let Some(vfs) = &self.vfs else { return Throttle::new() };
        match vfs.stat(THROTTLE.into()) {
            Ok(Err(FsError::NotFound)) => Throttle::new(),
            _ => {
                let text = shell_common::read_file(vfs, THROTTLE, MAX_THROTTLE).and_then(|b| String::from_utf8(b).ok());
                Throttle::parse(text.as_deref().unwrap_or(""), now)
            }
        }
    }

    /// Keeps the wrong passwords given and the wait they make.
    pub fn set_throttle(&self, t: &Throttle) -> Result<(), WriteError> {
        let vfs = self.vfs.as_ref().ok_or(WriteError)?;
        write(vfs, THROTTLE, t.record().as_bytes())
    }

    /// How long the user chose the screen waits without input before it
    /// locks (s), if they did. A record that cannot be read, or is not a
    /// time taken, counts as none chosen: the default holds.
    pub fn idle(&self) -> Option<u32> {
        let vfs = self.vfs.as_ref()?;
        if !matches!(vfs.stat(IDLE.into()), Ok(Ok(_))) {
            return None;
        }
        let text = shell_common::read_file(vfs, IDLE, MAX_IDLE).and_then(|b| String::from_utf8(b).ok());
        let chosen = text.as_deref().and_then(idle::parse);
        if chosen.is_none() {
            println!("the record of when the screen locks is damaged: it locks as it does by default");
        }
        chosen
    }

    /// Keeps `seconds` as the time the screen waits without input.
    pub fn set_idle(&self, seconds: u32) -> Result<(), WriteError> {
        let vfs = self.vfs.as_ref().ok_or(WriteError)?;
        write(vfs, IDLE, idle::record(seconds).as_bytes())?;
        // Read back: what is kept must be what was chosen.
        if self.idle() == Some(seconds) { Ok(()) } else { Err(WriteError) }
    }
}

/// Writes `data` as `path`, whole: under another name first, then put in
/// its place.
fn write(vfs: &vfs::Client, path: &str, data: &[u8]) -> Result<(), WriteError> {
    let new = format!("{path}.new");
    let vmo = Vmo::create(data.len().max(1)).map_err(|_| WriteError)?;
    vmo.write(0, data).map_err(|_| WriteError)?;
    let _ = vfs.mkdir(DIR.into());
    match vfs.write_file(new.clone(), vmo, data.len() as u64) {
        Ok(Ok(())) => {}
        _ => return Err(WriteError),
    }
    match vfs.replace(new, path.into()) {
        Ok(Ok(())) => Ok(()),
        _ => Err(WriteError),
    }
}

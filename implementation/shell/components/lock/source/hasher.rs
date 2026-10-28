// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The hasher: checks and hashes passwords on a thread of its own. Argon2
//! takes a fraction of a second on purpose (seconds under emulation), and
//! the lock screen keeps drawing meanwhile: the light runs round its ring.
//!
//! The passwords it is given are wiped from memory once it is done with
//! them (`Zeroizing`), and Argon2 wipes the memory it hashed them in (its
//! `zeroize` feature).

use std::collections::VecDeque;
use std::sync::Arc;

use abi::RawHandle;
use lock_core::password::{Record, SALT_LEN};
use runtime::object::Event;
use runtime::sync::{Condvar, Mutex};
use zeroize::Zeroizing;

/// A password to check or to set.
pub enum Job {
    /// Whether `password` is `record`'s. Should the record be outdated, the
    /// right password is hashed anew with `salt`.
    Check { ticket: u64, record: Record, password: Zeroizing<String>, salt: [u8; SALT_LEN] },
    /// Settings sets the password: `current` must be `record`'s, if one is
    /// set; then `new` is hashed with `salt` (an empty one removes it).
    Set {
        ticket: u64,
        record: Option<Record>,
        current: Zeroizing<String>,
        new: Zeroizing<String>,
        salt: [u8; SALT_LEN],
    },
}

/// Why a password could not be set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetError {
    /// `current` was not the password set.
    WrongPassword,
    /// The new one could not be hashed.
    Hashing,
}

/// A job done.
pub enum Done {
    /// Whether the password was right, and its record made anew if the one
    /// it was checked against was outdated.
    Checked { ticket: u64, right: bool, renewed: Option<Record> },
    /// The password's new record (`None`: it was removed).
    Set { ticket: u64, result: Result<Option<Record>, SetError> },
}

struct Shared {
    jobs: Mutex<VecDeque<Job>>,
    wake: Condvar,
    done: Mutex<Vec<Done>>,
}

/// The main loop's handle on the hasher: [`Hasher::event_handle`] is
/// signalled when jobs are done ([`Hasher::take`]).
pub struct Hasher {
    shared: Arc<Shared>,
    event: Event,
}

impl Hasher {
    /// Starts the thread. Argon2 takes 19 MiB of memory, from the heap;
    /// the thread's stack holds little.
    pub fn start() -> Option<Hasher> {
        let event = Event::create().ok()?;
        let theirs = Event::from_handle(event.0.duplicate(None).ok()?);
        let shared =
            Arc::new(Shared { jobs: Mutex::new(VecDeque::new()), wake: Condvar::new(), done: Mutex::new(Vec::new()) });
        let worker = shared.clone();
        runtime::thread::Builder::new()
            .name("hasher")
            .stack_size(256 * 1024)
            .spawn(move || work(&worker, &theirs))
            .ok()?;
        Some(Hasher { shared, event })
    }

    /// Hands `job` to the thread.
    pub fn give(&self, job: Job) {
        self.shared.jobs.lock().push_back(job);
        self.shared.wake.notify_one();
    }

    /// The jobs done since the last call.
    pub fn take(&self) -> Vec<Done> {
        let _ = self.event.clear();
        core::mem::take(&mut *self.shared.done.lock())
    }

    /// Signalled when a job is done.
    pub fn event_handle(&self) -> RawHandle {
        self.event.raw()
    }
}

/// The thread: does the jobs, one at a time.
fn work(shared: &Shared, done: &Event) {
    loop {
        let job = {
            let mut q = shared.jobs.lock();
            loop {
                if let Some(j) = q.pop_front() {
                    break j;
                }
                q = shared.wake.wait(q);
            }
        };
        let result = match job {
            Job::Check { ticket, record, password, salt } => {
                let right = record.verify(password.as_bytes());
                let renewed = (right && record.outdated()).then(|| Record::create(password.as_bytes(), &salt).ok());
                Done::Checked { ticket, right, renewed: renewed.flatten() }
            }
            Job::Set { ticket, record, current, new, salt } => {
                let result = if record.as_ref().is_some_and(|r| !r.verify(current.as_bytes())) {
                    Err(SetError::WrongPassword)
                } else if new.is_empty() {
                    Ok(None)
                } else {
                    Record::create(new.as_bytes(), &salt).map(Some).map_err(|_| SetError::Hashing)
                };
                Done::Set { ticket, result }
            }
        };
        shared.done.lock().push(result);
        let _ = done.signal();
    }
}

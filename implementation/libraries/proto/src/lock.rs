// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The lock screen's service (`lock`): what Settings shows and changes of
//! the lock screen, its password above all, and how long the screen waits
//! without input before it locks.
//!
//! The password is the lock service's alone. It keeps a salted Argon2id
//! hash of it in its private directory, and gives neither the password
//! nor the hash to anyone. Settings sets, changes and removes it, giving
//! the password set now each time, so that someone who has the unlocked
//! computer for a moment cannot change it; wrong passwords given there
//! count towards the same limit as those typed on the lock screen.
//!
//! Hashing a password takes a moment on purpose (a fraction of a second,
//! more under emulation), so the service answers
//! [`lock::Client::set_password`] once it is done, and [`LockLink`] calls
//! it from a background thread, for applications that must not wait.
//! Locking the screen is the window system's (`display::lock`).
//!
//! **Locking after a while without input** is the window system's too
//! (`display::set_idle_timeout`): the lock service tells it the time, one
//! of [`IDLE_CHOICES_S`], which Settings alone reads and chooses. Until a
//! time is chosen, the screen locks after [`DEFAULT_IDLE_S`] while a
//! password is set, and never while none is: a password says the computer
//! is to be kept from others, which an unattended unlocked screen defeats,
//! while without one the lock screen keeps nothing from anyone, and locking
//! it unasked would only interrupt.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use vabi::RawHandle;
use vipc::{enumeration, message, protocol};
use vrt::object::Event;
use vrt::sync::{Condvar, Mutex};

/// The fewest characters a password has.
pub const MIN_PASSWORD: usize = 4;
/// The most bytes a password has.
pub const MAX_PASSWORD: usize = 256;

/// The times the screen may wait without input before it locks (seconds),
/// as Settings offers them: never (0), or 1, 5, 15 or 30 minutes.
pub const IDLE_CHOICES_S: [u32; 5] = [0, 60, 300, 900, 1800];
/// How long it waits while a password is set and no time is chosen
/// (seconds); while none is set, it does not lock for want of input.
pub const DEFAULT_IDLE_S: u32 = 300;

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum LockError {
        /// The caller may not do this: only Settings sets the password and
        /// reads and chooses the time the screen locks after.
        Denied = 1,
        /// The password given as the one set now is not.
        WrongPassword = 2,
        /// The new password has fewer than [`MIN_PASSWORD`] characters.
        TooShort = 3,
        /// The new password has more than [`MAX_PASSWORD`] bytes.
        TooLong = 4,
        /// Too many wrong passwords: none is taken for a while
        /// ([`LockStatus::wait_s`] says how long).
        Throttled = 5,
        /// The lock service could not keep the password, or the time
        /// chosen (its store could not be written).
        Unavailable = 6,
        /// The time asked for is not one of [`IDLE_CHOICES_S`].
        Invalid = 7,
    }
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockError::Denied => f.write_str("Only Settings may change the lock screen."),
            LockError::WrongPassword => f.write_str("The current password is not right."),
            LockError::TooShort => write!(f, "The password is too short: use at least {MIN_PASSWORD} characters."),
            LockError::TooLong => write!(f, "The password is too long: use at most {MAX_PASSWORD} bytes."),
            LockError::Throttled => f.write_str("Too many wrong passwords were given. Wait a little, then try again."),
            LockError::Unavailable => f.write_str("The lock screen could not keep the change."),
            LockError::Invalid => f.write_str("The screen cannot lock after that time: choose one Settings offers."),
        }
    }
}

message! {
    /// The lock screen's state.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct LockStatus {
        /// A password is set: the lock screen asks for it.
        pub password: bool,
        /// The screen is locked.
        pub locked: bool,
        /// Seconds until another password is taken (0: one is taken now).
        pub wait_s: u32,
    }
}

protocol! {
    /// The lock screen.
    pub mod lock = "lock" {
        /// The lock screen's state.
        1 => fn status() -> LockStatus;
        /// Settings only: sets the password the lock screen asks for.
        /// `current` is the password set now (empty when none is); an
        /// empty `new` removes it. Answered once both are hashed.
        2 => fn set_password(current: String, new: String) -> Result<(), LockError>;
        /// Settings only: how long the screen waits without input before
        /// it locks (seconds; 0: never): the time chosen or, until one is,
        /// the default for whether a password is set.
        3 => fn idle_timeout() -> Result<u32, LockError>;
        /// Settings only: the screen waits `seconds` without input before
        /// it locks, from now on and after a restart: one of
        /// [`IDLE_CHOICES_S`] (`Invalid` for another).
        4 => fn set_idle_timeout(seconds: u32) -> Result<(), LockError>;
    }
}

/// How long [`LockLink`] waits for an answer: hashing two passwords under
/// emulation takes seconds.
const LINK_TIMEOUT_NS: u64 = 60_000_000_000;

/// Why [`LockLink`] got no answer, or the answer it got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkError {
    /// The lock service is not running.
    NotRunning,
    /// It stopped answering.
    NotResponding,
    /// It refused.
    Lock(LockError),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::NotRunning => f.write_str("The lock screen is not running."),
            LinkError::NotResponding => f.write_str("The lock screen is not responding."),
            LinkError::Lock(e) => e.fmt(f),
        }
    }
}

/// An answer from [`LockLink`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockReply {
    /// The outcome of [`LockLink::request_status`].
    Status(Result<LockStatus, LinkError>),
    /// The outcome of [`LockLink::set_password`].
    PasswordSet(Result<(), LinkError>),
    /// The outcome of [`LockLink::request_idle_timeout`] (seconds).
    IdleTimeout(Result<u32, LinkError>),
    /// The outcome of [`LockLink::set_idle_timeout`]: the time set, or why
    /// not.
    IdleTimeoutSet(Result<u32, LinkError>),
}

enum Request {
    Status,
    SetPassword { current: String, new: String },
    IdleTimeout,
    SetIdleTimeout(u32),
}

impl Request {
    /// The answer to this request when the lock service gives none.
    fn unanswered(self, e: LinkError) -> LockReply {
        match self {
            Request::Status => LockReply::Status(Err(e)),
            Request::SetPassword { .. } => LockReply::PasswordSet(Err(e)),
            Request::IdleTimeout => LockReply::IdleTimeout(Err(e)),
            Request::SetIdleTimeout(_) => LockReply::IdleTimeoutSet(Err(e)),
        }
    }
}

struct Shared {
    requests: Mutex<VecDeque<Request>>,
    wake: Condvar,
    replies: Mutex<Vec<LockReply>>,
}

/// Calls the lock service from a background thread. Requests are answered
/// in order through [`LockLink::take`]; [`LockLink::event_handle`] is
/// signalled when answers are waiting (return it from `App::wait_handles`).
///
/// The worker connects only once the registry lists the `lock` service
/// (connecting earlier would wait for it forever) and reconnects after the
/// service restarts.
pub struct LockLink {
    shared: Arc<Shared>,
    event: Event,
}

impl LockLink {
    /// Starts the worker thread.
    pub fn start() -> Option<LockLink> {
        let event = Event::create().ok()?;
        let theirs = Event::from_handle(event.0.duplicate(None).ok()?);
        let shared = Arc::new(Shared {
            requests: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            replies: Mutex::new(Vec::new()),
        });
        let s = shared.clone();
        vrt::thread::Builder::new().name("lock-link").spawn(move || worker(s, theirs)).ok()?;
        Some(LockLink { shared, event })
    }

    fn send(&self, r: Request) {
        self.shared.requests.lock().push_back(r);
        self.shared.wake.notify_one();
    }

    /// Asks for the lock screen's state ([`LockReply::Status`]).
    pub fn request_status(&self) {
        self.send(Request::Status);
    }

    /// Sets the password, `current` being the one set now (empty when none
    /// is) and an empty `new` removing it ([`LockReply::PasswordSet`]).
    pub fn set_password(&self, current: String, new: String) {
        self.send(Request::SetPassword { current, new });
    }

    /// Asks how long the screen waits without input before it locks
    /// ([`LockReply::IdleTimeout`]).
    pub fn request_idle_timeout(&self) {
        self.send(Request::IdleTimeout);
    }

    /// Chooses how long the screen waits without input before it locks:
    /// one of [`IDLE_CHOICES_S`] ([`LockReply::IdleTimeoutSet`]).
    pub fn set_idle_timeout(&self, seconds: u32) {
        self.send(Request::SetIdleTimeout(seconds));
    }

    /// The answers received so far.
    pub fn take(&self) -> Vec<LockReply> {
        let _ = self.event.clear();
        core::mem::take(&mut *self.shared.replies.lock())
    }

    /// Signalled when answers are waiting.
    pub fn event_handle(&self) -> RawHandle {
        self.event.raw()
    }
}

/// True if the lock service has registered.
fn lock_registered() -> bool {
    match crate::with_registry(|r| r.list()) {
        Ok(Ok(names)) => names.iter().any(|n| n == lock::NAME),
        _ => false,
    }
}

fn worker(shared: Arc<Shared>, event: Event) {
    let mut client: Option<lock::Client> = None;
    loop {
        let req = {
            let mut q = shared.requests.lock();
            loop {
                if let Some(r) = q.pop_front() {
                    break r;
                }
                q = shared.wake.wait(q);
            }
        };
        if client.is_none() && lock_registered() {
            client = crate::connect(lock::NAME).ok().map(lock::Client::new);
            if let Some(c) = &client {
                c.set_timeout(LINK_TIMEOUT_NS);
            }
        }
        let reply = match (&client, req) {
            (None, req) => req.unanswered(LinkError::NotRunning),
            (Some(c), Request::Status) => match c.status() {
                Ok(s) => LockReply::Status(Ok(s)),
                Err(_) => {
                    client = None;
                    LockReply::Status(Err(LinkError::NotResponding))
                }
            },
            (Some(c), Request::SetPassword { current, new }) => match c.set_password(current, new) {
                Ok(r) => LockReply::PasswordSet(r.map_err(LinkError::Lock)),
                Err(_) => {
                    client = None;
                    LockReply::PasswordSet(Err(LinkError::NotResponding))
                }
            },
            (Some(c), Request::IdleTimeout) => match c.idle_timeout() {
                Ok(r) => LockReply::IdleTimeout(r.map_err(LinkError::Lock)),
                Err(_) => {
                    client = None;
                    LockReply::IdleTimeout(Err(LinkError::NotResponding))
                }
            },
            (Some(c), Request::SetIdleTimeout(seconds)) => match c.set_idle_timeout(seconds) {
                Ok(r) => LockReply::IdleTimeoutSet(r.map(|()| seconds).map_err(LinkError::Lock)),
                Err(_) => {
                    client = None;
                    LockReply::IdleTimeoutSet(Err(LinkError::NotResponding))
                }
            },
        };
        shared.replies.lock().push(reply);
        let _ = event.signal();
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec::Vec;

    use vipc::{Decode, Decoder, Encode, Encoder};

    use super::*;
    use crate::display::LockNotice;

    fn roundtrip<T: Encode + Decode>(v: T) -> T {
        let mut e = Encoder::new();
        v.encode(&mut e);
        let mut d = Decoder::new(&e.bytes, Vec::new());
        let out = T::decode(&mut d).unwrap();
        d.finish().unwrap();
        out
    }

    #[test]
    fn the_lock_says_what_it_is_on_the_wire() {
        let s = LockStatus { password: true, locked: true, wait_s: 900 };
        assert_eq!(roundtrip(s), s);
        for locked in [false, true] {
            assert_eq!(roundtrip(LockNotice::Changed { locked }), LockNotice::Changed { locked });
        }
        let refused: Result<(), LockError> = Err(LockError::Throttled);
        assert_eq!(roundtrip(refused), refused);
        for idle in [Ok(1800), Err(LockError::Denied), Err(LockError::Invalid)] {
            let idle: Result<u32, LockError> = idle;
            assert_eq!(roundtrip(idle), idle);
        }
    }

    #[test]
    fn the_default_time_is_one_settings_offers() {
        assert!(IDLE_CHOICES_S.contains(&DEFAULT_IDLE_S));
        // Never, then shortest first, as Settings shows them.
        assert_eq!(IDLE_CHOICES_S[0], 0);
        assert!(IDLE_CHOICES_S.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn refusals_say_what_to_do() {
        assert!(LockError::TooShort.to_string().contains(&MIN_PASSWORD.to_string()));
        assert!(LockError::TooLong.to_string().contains(&MAX_PASSWORD.to_string()));
        let e = LinkError::Lock(LockError::WrongPassword);
        assert_eq!(e.to_string(), LockError::WrongPassword.to_string());
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The screen's lock. While the screen is locked, the agent neither listens
//! for its name nor holds a conversation: nobody at a locked computer can
//! ask it for what the lock keeps from them. It hears of the lock from the
//! window system (`display::watch_lock`), and takes the screen for locked
//! until the window system says otherwise: as the agent starts, and while
//! the window system is away (which starts locked again).
//!
//! What would have woken it meanwhile waits for the screen to be unlocked:
//! a reminder that came due is told then, and so is a conversation asked
//! for as the agent started (`agent.wake`, in tests).
//!
//! The agent locks the screen when the user asks it to
//! ([`Agent::lock_screen`]); the conversation ends as the lock's notice
//! comes back, once the call's result has gone to it.
//!
//! While the screen is unlocked, the agent tells the window system when
//! the user talks with it ([`Agent::user_active`]), which counts as input
//! for the idle lock, on the connection that watches the lock.

use vproto::display::{LOCK_NOTICE, LockNotice, display};
use vrt::object::Channel;
use vrt::println;

use crate::Agent;

/// How often the agent tries to watch the lock while the window system is
/// away.
const RETRY_NS: u64 = 1_000_000_000;
/// How long the agent waits for the window system to take that the user
/// is there: the request goes all the same, and the agent's loop has the
/// voice and the microphone to serve.
const ACTIVE_TIMEOUT_NS: u64 = 250_000_000;

/// The agent's watch on the lock.
#[derive(Default)]
pub(crate) struct LockWatch {
    /// The channel the window system tells the lock on ...
    channel: Option<Channel>,
    /// ... and the connection that asked for it.
    display: Option<display::Client>,
    /// When to try to watch again.
    next_try: u64,
}

impl LockWatch {
    /// The channel to wait on, while there is one.
    pub(crate) fn handle(&self) -> Option<vabi::RawHandle> {
        self.channel.as_ref().map(|c| c.raw())
    }

    /// When to try to watch again, while the agent does not.
    pub(crate) fn deadline(&self) -> u64 {
        if self.channel.is_some() { vabi::DEADLINE_INFINITE } else { self.next_try }
    }
}

/// A channel on which the window system tells the lock, if it runs, and
/// the connection that asked for it.
fn watch() -> Option<(Channel, display::Client)> {
    let names = vproto::with_registry(|r| r.list()).ok()?.ok()?;
    if !names.iter().any(|n| n == display::NAME) {
        return None;
    }
    let client = display::Client::new(vproto::connect(display::NAME).ok()?);
    client.set_timeout(2_000_000_000);
    let (ours, theirs) = Channel::create().ok()?;
    match client.watch_lock(theirs) {
        Ok(Ok(())) => {
            client.set_timeout(ACTIVE_TIMEOUT_NS);
            Some((ours, client))
        }
        _ => None,
    }
}

impl Agent {
    /// Locks the screen (the `system` function's `lock`).
    pub(crate) fn lock_screen(&self) -> Result<(), &'static str> {
        let client = vproto::connect(display::NAME).map(display::Client::new);
        let Ok(client) = client else { return Err("the window system is not available") };
        client.set_timeout(2_000_000_000);
        match client.lock() {
            Ok(Ok(())) => Ok(()),
            _ => Err("the screen could not be locked"),
        }
    }

    /// Tells the window system that the user is there, as input would (see
    /// `vagent::presence` for when): while it watches the lock, which it
    /// does whenever the window system runs.
    pub(crate) fn user_active(&mut self) {
        let Some(d) = &self.lock_watch.display else { return };
        // Not answered in time, or the window system went away (which the
        // watch hears of): nothing to do.
        if let Ok(Err(e)) = d.user_active() {
            println!("the window system refused to hear that the user is there ({e:?}): not telling it again");
            self.lock_watch.display = None;
        }
    }

    /// Watches the lock, if it does not yet (and it is time to try), and
    /// takes what the window system says of it.
    pub(crate) fn check_lock(&mut self, now: u64) {
        if self.lock_watch.channel.is_none() && now >= self.lock_watch.next_try {
            self.lock_watch.next_try = now + RETRY_NS;
            (self.lock_watch.channel, self.lock_watch.display) = watch().unzip();
        }
        let Some(ch) = &self.lock_watch.channel else { return };
        let mut locked = None;
        let gone = loop {
            match ch.read() {
                Ok(msg) => {
                    if let Ok((LOCK_NOTICE, LockNotice::Changed { locked: l })) = vipc::decode_event::<LockNotice>(msg)
                    {
                        locked = Some(l);
                    }
                }
                Err(vabi::Error::ShouldWait) => break false,
                Err(_) => break true,
            }
        };
        if gone {
            // The window system went away: it starts again locked.
            self.lock_watch.channel = None;
            self.lock_watch.display = None;
            self.lock_watch.next_try = now + RETRY_NS;
            locked = Some(true);
        }
        if let Some(l) = locked {
            self.set_locked(l);
        }
    }

    /// The screen is now `locked`, or not.
    fn set_locked(&mut self, locked: bool) {
        if locked == self.locked {
            return;
        }
        self.locked = locked;
        if locked {
            println!("the screen is locked: not listening");
            if self.session.is_some() || self.connector.is_some() {
                self.end_conversation(None);
            }
            self.update_listening();
            return;
        }
        println!("the screen is unlocked");
        self.update_listening();
        // What would have woken it meanwhile.
        let asked = core::mem::take(&mut self.wake_on_unlock);
        if asked || !self.on_wake.is_empty() {
            self.wake("the screen was unlocked", asked);
        }
    }
}

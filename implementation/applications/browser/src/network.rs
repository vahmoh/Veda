// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The network's state, as the network service tells it, so that pages
//! that could not load for want of a network load again when it changes
//! (as when the browser starts before the network is up, or an address of
//! the family a site needs comes after another's).

use abi::RawHandle;
use proto::net::{Connectivity, NetEvent};
use rt::object::Channel;

/// How long after the network service went away to watch it again (ns).
const REWATCH_NS: u64 = 5_000_000_000;

pub struct Network {
    /// The service's news, while it gives them.
    watch: Option<Channel>,
    /// When to watch again, if it does not.
    rewatch_at: u64,
    connectivity: Connectivity,
}

impl Network {
    pub fn new() -> Network {
        let mut n = Network { watch: None, rewatch_at: 0, connectivity: Connectivity::Offline };
        n.rewatch(0);
        n
    }

    /// What to wait on for the news.
    pub fn handle(&self) -> Option<RawHandle> {
        self.watch.as_ref().map(Channel::raw)
    }

    /// When to watch the service again, if it is not watched.
    pub fn rewatch_due(&self) -> Option<u64> {
        self.watch.is_none().then_some(self.rewatch_at)
    }

    /// Watches the service again, if it went away before `now`: whether
    /// there is a network (which may have changed meanwhile).
    pub fn rewatch(&mut self, now: u64) -> bool {
        if self.watch.is_some() || now < self.rewatch_at {
            return false;
        }
        self.watch = net::watch().ok();
        if self.watch.is_none() {
            self.rewatch_at = now + REWATCH_NS;
            return false;
        }
        if let Ok(s) = net::status() {
            self.connectivity = s.connectivity;
        }
        self.up()
    }

    /// Takes the service's news: whether the network changed and is up (a
    /// page that failed for want of it may load now).
    pub fn events(&mut self, now: u64) -> bool {
        let mut changed = false;
        while let Some(ch) = &self.watch {
            match net::next_event(ch) {
                Ok(Some(NetEvent::StatusChanged { status })) => {
                    self.connectivity = status.connectivity;
                    changed = true;
                }
                Ok(Some(NetEvent::InterfaceChanged { .. })) => changed = true,
                Ok(None) => break,
                // The service went away: watch it again later.
                Err(_) => {
                    self.watch = None;
                    self.rewatch_at = now + REWATCH_NS;
                }
            }
        }
        changed && self.up()
    }

    /// Whether there is a network to try.
    fn up(&self) -> bool {
        !matches!(self.connectivity, Connectivity::Offline)
    }
}

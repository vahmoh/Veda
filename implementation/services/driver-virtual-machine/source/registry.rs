// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The registry the guest sees: Veda's, narrowed to the services that
//! drivers attach to.
//!
//! Linux in the driver VM runs a great deal of code Veda did not write,
//! so it gets no more than its drivers need: it may connect to the
//! services drivers attach to (and provide the one it serves, `gpu`); it
//! cannot reach the files, the agent, or anything else. It has at most
//! [`MAX_CHANNELS`] channels to the registry at a time (each of its
//! programs takes one), which a thread of their own serves.
//!
//! The narrowed registry passes what it allows on through this process's
//! own registry channel, which `device-manager` started it with: a copy of
//! its own, which speaks for `device-manager` (`init` attributes every
//! registry channel to a process). So what the guest connects to, and
//! registers, is `device-manager`'s: a service sees `device-manager` as the
//! client (`ClientIdentity`), and the guest's connections that wait for
//! their services count among `device-manager`'s. The registry's calls are
//! all the narrowing sees, not the calls the guest then makes on its
//! connections: a service that trusts `device-manager` with some of them
//! (the compositor's and the audio service's `expect`, which say what
//! drivers are coming) would take them from the guest too, until the
//! monitor speaks for itself (see DRIVERVM.md).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use abi::{DEADLINE_INFINITE, Error, signals};
use ipc::WaitSet;
use proto::init::RegistryError;
use proto::registry;
use rt::object::{Channel, Event};
use rt::println;
use rt::sync::Mutex;

/// The services the guest may connect to: those drivers attach to.
const CONNECT: &[&str] = &[
    proto::audio::audiodev::NAME,
    proto::displaydev::displaydev::NAME,
    proto::net::netdev::NAME,
    proto::wlan::wlanphy::NAME,
    proto::input::NAME,
];
/// The services it may provide.
const REGISTER: &[&str] = &[proto::gpu::gpu::NAME];
/// The most registry channels the guest has at a time: one for each of its
/// programs, and some to spare.
pub const MAX_CHANNELS: usize = 32;

/// The guest's registry channels (our ends), by the keys the waiting set
/// knows them by; those added since it last looked.
struct Channels {
    by_key: BTreeMap<u64, Channel>,
    next: u64,
    added: Vec<u64>,
}

pub struct Registry {
    channels: Mutex<Channels>,
    /// Signalled when a channel is added.
    added: Event,
}

impl Registry {
    pub fn new() -> Result<Registry, Error> {
        Ok(Registry {
            channels: Mutex::new(Channels { by_key: BTreeMap::new(), next: 0, added: Vec::new() }),
            added: Event::create()?,
        })
    }

    /// A new registry channel for the guest: the end it is to hold
    /// (`LimitReached` when it has the most).
    pub fn connect(&self) -> Result<Channel, Error> {
        let mut c = self.channels.lock();
        if c.by_key.len() >= MAX_CHANNELS {
            return Err(Error::LimitReached);
        }
        let (ours, theirs) = Channel::create()?;
        c.next += 1;
        let key = c.next;
        c.by_key.insert(key, ours);
        c.added.push(key);
        drop(c);
        self.added.signal()?;
        Ok(theirs)
    }

    /// Serves the guest's registry channels (on a thread of its own),
    /// waiting on them all at once; the set it waits on changes as they
    /// come and go.
    pub fn run(&self) -> ! {
        const ADDED: u64 = 0;
        let mut ws = WaitSet::new();
        ws.add(self.added.raw(), signals::SIGNALED, ADDED);
        let mut said = false;
        loop {
            let ready = match ws.wait(DEADLINE_INFINITE) {
                Ok(ready) => ready,
                Err(e) => {
                    if !said {
                        println!("the guest's registry cannot wait ({e}); it tries again");
                        said = true;
                    }
                    rt::time::sleep(rt::time::Duration::from_millis(10));
                    continue;
                }
            };
            for (key, observed) in ready {
                if key == ADDED {
                    let _ = self.added.clear();
                    let mut c = self.channels.lock();
                    for k in core::mem::take(&mut c.added) {
                        if let Some(ch) = c.by_key.get(&k) {
                            ws.add(ch.raw(), signals::READABLE | signals::PEER_CLOSED, k);
                        }
                    }
                    continue;
                }
                if observed & signals::READABLE != 0 {
                    loop {
                        let msg = match self.channels.lock().by_key.get(&key).map(Channel::read) {
                            Some(Ok(msg)) => msg,
                            _ => break,
                        };
                        let reply = registry::dispatch(&mut Narrowed(self), msg);
                        if let (Ok(reply), Some(ch)) = (reply, self.channels.lock().by_key.get(&key)) {
                            let _ = reply.send(ch);
                        }
                    }
                } else if observed & signals::PEER_CLOSED != 0 {
                    ws.remove(key);
                    self.channels.lock().by_key.remove(&key);
                }
            }
        }
    }
}

/// The registry's calls, as far as the guest may make them.
struct Narrowed<'a>(&'a Registry);

impl registry::Server for Narrowed<'_> {
    fn connect(&mut self, name: String, server_end: Channel) -> Result<(), RegistryError> {
        if !CONNECT.contains(&name.as_str()) {
            return Err(RegistryError::Denied);
        }
        match proto::with_registry(|r| r.connect(name, server_end)) {
            Ok(Ok(result)) => result,
            _ => Err(RegistryError::Denied),
        }
    }

    fn register(&mut self, name: String, listener: Channel) -> Result<(), RegistryError> {
        if !REGISTER.contains(&name.as_str()) {
            return Err(RegistryError::Denied);
        }
        match proto::with_registry(|r| r.register(name, listener)) {
            Ok(Ok(result)) => result,
            _ => Err(RegistryError::Denied),
        }
    }

    fn list(&mut self) -> Vec<String> {
        let all = match proto::with_registry(|r| r.list()) {
            Ok(Ok(names)) => names,
            _ => Vec::new(),
        };
        all.into_iter().filter(|n| CONNECT.contains(&n.as_str()) || REGISTER.contains(&n.as_str())).collect()
    }

    fn clone_registry(&mut self) -> Result<Channel, RegistryError> {
        self.0.connect().map_err(|_| RegistryError::Denied)
    }

    /// The guest's channels are narrowed alike, all of them, and no one of
    /// them further: one that asks is refused, and so knows it is not.
    fn confine(&mut self, _names: Vec<String>) -> Result<(), RegistryError> {
        Err(RegistryError::Denied)
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! One browser at a time: the first registers the service `browser`; one
//! started later hands it the addresses it was given (none: a new tab) and
//! ends. Two would share one set of files (cookies, storage, history),
//! which nothing would keep them from writing at once.

use abi::{RawHandle, signals};
use ipc::Channel;

/// The service's name: the browser's own.
const NAME: &str = "browser";
/// The ordinal of a request to open addresses.
const OPEN: u32 = 1;

pub enum Claim {
    /// This is the browser: it serves the others.
    First(Instance),
    /// Another runs, which has the addresses.
    Running,
    /// No service could be had: this browser runs on its own.
    Alone,
}

/// Claims the browser for this process, or hands `urls` to the one that
/// has it.
pub fn claim(urls: &[String]) -> Claim {
    if let Ok(listener) = proto::register(NAME) {
        return Claim::First(Instance { listener, clients: Vec::new() });
    }
    match proto::connect(NAME) {
        Ok(channel) if ipc::send_event(&channel, OPEN, urls.to_vec()).is_ok() => Claim::Running,
        _ => Claim::Alone,
    }
}

/// The browser's service: the browsers started later, which say what to
/// open.
pub struct Instance {
    listener: Channel,
    clients: Vec<Channel>,
}

impl Instance {
    /// What to wait on.
    pub fn wait_handles(&self) -> Vec<(RawHandle, u32)> {
        let mut out = vec![(self.listener.raw(), signals::READABLE)];
        out.extend(self.clients.iter().map(|c| (c.raw(), signals::READABLE | signals::PEER_CLOSED)));
        out
    }

    /// The requests that came: each the addresses to open (none: a new
    /// tab).
    pub fn requests(&mut self) -> Vec<Vec<String>> {
        while let Some(client) = proto::accept(&self.listener) {
            self.clients.push(client);
        }
        let mut out = Vec::new();
        self.clients.retain(|client| {
            while let Ok(msg) = client.read() {
                if let Ok((OPEN, urls)) = ipc::decode_event::<Vec<String>>(msg) {
                    out.push(urls);
                }
            }
            // Each says one thing, then goes.
            !peer_closed(client)
        });
        out
    }
}

fn peer_closed(channel: &Channel) -> bool {
    let mut item = [abi::WaitItem { handle: channel.raw(), signals: signals::PEER_CLOSED, ..Default::default() }];
    rt::object::wait_many(&mut item, 0).is_ok() && item[0].observed & signals::PEER_CLOSED != 0
}

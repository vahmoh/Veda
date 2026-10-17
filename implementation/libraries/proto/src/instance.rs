// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! One instance of an application: the first to start registers a service
//! named after the application; a later one, asked to open a file (from
//! Files, the Terminal or the desktop), hands the file to it and exits, so
//! that the file opens where the user already is.
//!
//! The registry lets an application register only names of its own (its
//! process's name, or names under it), so the service is the application's
//! name: `videos`, `music`.

use alloc::string::String;
use alloc::vec::Vec;

use abi::{RawHandle, signals};
use rt::object::Channel;

ipc::protocol! {
    /// The running instance of an application, to which another hands what
    /// it was asked to open; registered under the application's name.
    pub mod instance = "instance" {
        /// Opens `path` (an absolute path); false if the application does
        /// not take it.
        1 => fn open(path: String) -> bool;
    }
}

/// How long a later instance waits for the running one to take its file.
pub const HAND_OVER_TIMEOUT_NS: u64 = 3_000_000_000;
/// Later instances served at once.
const MAX_CLIENTS: usize = 8;

/// How an application starts.
pub enum Start {
    /// As the instance later ones hand their files to; `None` when another
    /// holds the name and did not take a file (this one then runs alone).
    Serve(Option<Instance>),
    /// The running instance took the file: this one exits.
    HandedOver,
}

/// Starts application `name` (its own name), asked to open `file`: the
/// first instance serves later ones; a later one hands its file to the
/// first, unless that one does not take it in time (it is gone, or hangs),
/// in which case this one takes its place.
pub fn start(name: &str, file: Option<&str>) -> Start {
    if let Some(i) = Instance::register(name) {
        return Start::Serve(Some(i));
    }
    let Some(path) = file else { return Start::Serve(None) };
    if hand_over(name, path, HAND_OVER_TIMEOUT_NS) {
        return Start::HandedOver;
    }
    Start::Serve(Instance::register(name))
}

/// Hands `path` to the instance registered as `name`, waiting at most
/// `timeout_ns` for its answer: whether it took the file.
pub fn hand_over(name: &str, path: &str, timeout_ns: u64) -> bool {
    let Ok(ch) = crate::connect(name) else { return false };
    let client = instance::Client::new(ch);
    client.set_timeout(timeout_ns);
    client.open(path.into()).unwrap_or(false)
}

/// The running instance's end of its service.
pub struct Instance {
    listener: Channel,
    clients: Vec<Channel>,
}

/// Takes the files handed over that the application accepts.
struct Handler<'a, F: Fn(&str) -> bool> {
    accept: &'a F,
    files: &'a mut Vec<String>,
}

impl<F: Fn(&str) -> bool> instance::Server for Handler<'_, F> {
    fn open(&mut self, path: String) -> bool {
        let ok = path.starts_with('/') && (self.accept)(&path);
        if ok {
            self.files.push(path);
        }
        ok
    }
}

impl Instance {
    /// Registers the service `name`; `None` if another instance has it.
    pub fn register(name: &str) -> Option<Instance> {
        crate::register(name).ok().map(|listener| Instance { listener, clients: Vec::new() })
    }

    /// The kernel objects to wait on (the listener, then every client).
    pub fn wait_handles(&self, out: &mut Vec<(RawHandle, u32)>) {
        out.push((self.listener.raw(), signals::READABLE));
        for c in &self.clients {
            out.push((c.raw(), signals::READABLE | signals::PEER_CLOSED));
        }
    }

    /// Accepts connections and answers their requests: the files handed
    /// over (absolute paths) that `accept` takes, in order.
    pub fn poll(&mut self, accept: impl Fn(&str) -> bool) -> Vec<String> {
        while let Some(ch) = crate::accept(&self.listener) {
            if self.clients.len() < MAX_CLIENTS {
                self.clients.push(ch);
            }
        }
        let mut files = Vec::new();
        self.clients.retain(|c| {
            loop {
                match c.read() {
                    Ok(msg) => {
                        let mut handler = Handler { accept: &accept, files: &mut files };
                        if let Ok(reply) = instance::dispatch(&mut handler, msg) {
                            let _ = reply.send(c);
                        }
                    }
                    Err(abi::Error::ShouldWait) => break true,
                    Err(_) => break false,
                }
            }
        });
        files
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn request(path: &str) -> rt::Message {
        let mut e = ipc::Encoder::with_header(1, 5, ipc::FLAG_REQUEST);
        ipc::Encode::encode(path, &mut e);
        rt::Message { bytes: e.bytes, handles: vec![] }
    }

    fn answer(accept: &impl Fn(&str) -> bool, path: &str) -> (bool, Vec<String>) {
        let mut files = Vec::new();
        let reply = instance::dispatch(&mut Handler { accept, files: &mut files }, request(path)).unwrap();
        let mut msg = rt::Message { bytes: reply.bytes, handles: vec![] };
        let (_, mut d) = ipc::open(&mut msg).unwrap();
        (<bool as ipc::Decode>::decode(&mut d).unwrap(), files)
    }

    #[test]
    fn only_absolute_paths_the_application_takes_are_opened() {
        let videos = |p: &str| p.ends_with(".mkv");
        assert_eq!(answer(&videos, "/home/user/Videos/a.mkv"), (true, vec![String::from("/home/user/Videos/a.mkv")]));
        assert_eq!(answer(&videos, "a.mkv"), (false, vec![]), "not an absolute path");
        assert_eq!(answer(&videos, "/home/user/song.qoa"), (false, vec![]), "not one it takes");
    }
}

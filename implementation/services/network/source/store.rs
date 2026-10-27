// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The firewall's settings on the home disk, in the network service's
//! private directory (`/home/.private/network`), which the file system
//! opens to no other program: an application can neither read them nor
//! change them to let itself in.
//!
//! A thread of its own reads and writes them, so that the service never
//! waits for the file system, which waits for the home disk as the system
//! starts: until the settings are read, the defaults hold, which let
//! nothing in. The file is the settings' encoding after a format mark; a
//! new one is written beside the old and then takes its place, and one
//! that does not read back whole and valid is not used.

use alloc::vec::Vec;

use abi::signals::{PEER_CLOSED, READABLE};
use files::fs::Fs;
use ipc::{Decode, Decoder, Encode, Encoder};
use proto::net::FirewallConfig;
use rt::object::Channel;

const DIR: &str = "/home/.private/network";
const PATH: &str = "/home/.private/network/firewall";
const TEMP: &str = "/home/.private/network/firewall.new";
/// What the file starts with: what it is, and the version of its format.
const MAGIC: &[u8; 8] = b"VEDA-FW1";

/// What the store found on the disk.
pub enum Loaded {
    Saved(FirewallConfig),
    /// Nothing was ever saved.
    Missing,
    /// A file that is not the settings, or not valid ones.
    Unreadable,
}

/// The service's side of the store's thread.
pub struct Store {
    channel: Channel,
}

impl Store {
    /// Starts the store's thread, which reads the settings at once.
    pub fn start() -> Option<Store> {
        let (ours, theirs) = Channel::create().ok()?;
        rt::thread::Builder::new().name("firewall-store").spawn(move || serve(theirs)).ok()?;
        Some(Store { channel: ours })
    }

    /// The channel, readable once the settings are read.
    pub fn channel(&self) -> &Channel {
        &self.channel
    }

    /// What the store read, once it has.
    pub fn take_loaded(&self) -> Option<Loaded> {
        let msg = self.channel.read().ok()?;
        let mut d = Decoder::new(&msg.bytes, Vec::new());
        match u8::decode(&mut d).ok()? {
            0 => FirewallConfig::decode(&mut d).ok().map(Loaded::Saved),
            1 => Some(Loaded::Missing),
            _ => Some(Loaded::Unreadable),
        }
    }

    /// Has the store write `config` (the latest of several waiting wins).
    pub fn save(&self, config: &FirewallConfig) {
        let mut e = Encoder::new();
        config.clone().encode(&mut e);
        if self.channel.write(&e.bytes, Vec::new()).is_err() {
            rt::println!("firewall: the settings could not be handed to the store");
        }
    }
}

/// The store's thread: reads the settings, then writes those it is given.
fn serve(channel: Channel) {
    let fs = Fs::connect();
    let mut e = Encoder::new();
    match load(&fs) {
        Loaded::Saved(config) => {
            0u8.encode(&mut e);
            config.encode(&mut e);
        }
        Loaded::Missing => 1u8.encode(&mut e),
        Loaded::Unreadable => 2u8.encode(&mut e),
    }
    if channel.write(&e.bytes, Vec::new()).is_err() {
        return;
    }
    loop {
        match channel.wait(READABLE | PEER_CLOSED, abi::DEADLINE_INFINITE) {
            Ok(s) if s & READABLE != 0 => {}
            _ => return,
        }
        // Only the latest settings matter.
        let mut latest = None;
        while let Ok(msg) = channel.read() {
            let mut d = Decoder::new(&msg.bytes, Vec::new());
            if let Ok(config) = FirewallConfig::decode(&mut d) {
                latest = Some(config);
            }
        }
        if let Some(config) = latest
            && !save(&fs, &config)
        {
            rt::println!("firewall: the settings could not be saved");
        }
    }
}

/// Reads the settings (from a new file that never took the old one's
/// place, if that is all there is).
fn load(fs: &Fs) -> Loaded {
    let data = match fs.read(PATH) {
        Ok(data) => data,
        Err(_) => match fs.read(TEMP) {
            Ok(data) => {
                let _ = fs.rename(TEMP, PATH);
                data
            }
            Err(_) => return Loaded::Missing,
        },
    };
    match decode(&data) {
        Some(config) => Loaded::Saved(config),
        None => Loaded::Unreadable,
    }
}

/// The settings a file holds, if it holds valid ones and nothing else.
fn decode(data: &[u8]) -> Option<FirewallConfig> {
    let body = data.strip_prefix(MAGIC)?;
    let mut d = Decoder::new(body, Vec::new());
    let config = FirewallConfig::decode(&mut d).ok()?;
    d.finish().ok()?;
    config.check().ok()?;
    Some(config)
}

/// Writes the settings: to a new file first, which then takes the old
/// one's place, so that a failure part-way leaves the old settings.
fn save(fs: &Fs, config: &FirewallConfig) -> bool {
    let mut e = Encoder::new();
    config.clone().encode(&mut e);
    let mut data = Vec::with_capacity(MAGIC.len() + e.bytes.len());
    data.extend_from_slice(MAGIC);
    data.extend_from_slice(&e.bytes);
    if fs.mkdir_all(DIR).is_err() || fs.write(TEMP, &data).is_err() {
        return false;
    }
    if fs.rename(TEMP, PATH).is_ok() {
        return true;
    }
    // A file system that does not rename over a file: replace it.
    let _ = fs.remove(PATH);
    fs.rename(TEMP, PATH).is_ok()
}

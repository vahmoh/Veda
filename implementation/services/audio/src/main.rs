// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `audio` — the system audio service.
//!
//! * Registers `audio` (the public protocol, [`vproto::audio::audio`]):
//!   applications open playback streams backed by shared rings, pause,
//!   flush and adjust them, and set the master volume.
//! * Registers `audiodev` ([`vproto::audio::audiodev`]): a sound driver
//!   attaches its output device and receives the ring the mixer fills;
//!   devmgr says whether one is coming, which the status tells (the
//!   startup sound waits for it).
//! * Mixes every playing stream into the device format (see [`mixer`]).
//!   Without a device, audio is consumed silently in real time so that
//!   applications behave the same on machines without sound hardware.
//! * Delivers the microphone to capture streams, with echo cancellation
//!   for those that ask (see [`capture`]).
//!
//! **Who may do what** is decided by the registry's identity of each
//! connection. A device attached receives everything the computer plays
//! (the voice agent's voice included) or becomes every capture stream's
//! microphone (what the agent hears), so only the drivers devmgr starts
//! (they speak as devmgr) attach devices and say one is coming; any other
//! program's connection to `audiodev` is closed. The one exception is the
//! test microphone (`testmic`, when the kernel command line runs it for
//! automated tests, as only the machine's owner can: `requested`): it may
//! attach an input device, which a sound card's microphone then replaces.
//! Only the voice agent ducks the other sound.
//!
//! The service runs on one high-priority thread: IPC requests are short,
//! and mixing in the same loop avoids any locking. Once it runs, only
//! requests allocate (their messages): waiting, mixing and recording reuse
//! what they have.

#![no_std]
#![no_main]

extern crate alloc;

mod capture;
mod mixer;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use vabi::{WaitItem, signals};
use vproto::audio::{
    AudioError, AudioStatus, DeviceFormat, DeviceLink, InputHandle, InputLink, InputSpec, StreamHandle, StreamSpec,
    audio, audiodev,
};
use vproto::init::ClientIdentity;
use vrt::object::Channel;
use vrt::println;

use capture::Capture;
use mixer::Mixer;

vrt::entry!(main);

/// Wait-set keys.
const KEY_AUDIO_LISTENER: u64 = 1;
const KEY_DEV_LISTENER: u64 = 2;
const KEY_SPACE: u64 = 3;
const KEY_CAPTURE: u64 = 4;
const FIRST_CONN: u64 = 16;
/// Clients beyond this many are refused (their channel is closed).
const MAX_CLIENTS: usize = 48;
/// Connections to `audiodev` beyond this many are refused.
const MAX_DRIVERS: usize = 8;

/// Who a driver connection is (see the module documentation).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Driver {
    /// devmgr, or a driver it started.
    Devmgr,
    /// The test microphone.
    TestMicrophone,
    /// Anything else: it may attach nothing.
    Other,
}

impl Driver {
    fn of(who: &ClientIdentity) -> Driver {
        if who.service && who.name == "devmgr" {
            Driver::Devmgr
        } else if who.requested && who.name == "testmic" {
            Driver::TestMicrophone
        } else {
            Driver::Other
        }
    }
}

/// What a connection speaks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `audio`; the voice agent's or not.
    Client { agent: bool },
    /// `audiodev`.
    Driver(Driver),
}

struct Service {
    mixer: Mixer,
    capture: Capture,
    conns: BTreeMap<u64, (Kind, Channel)>,
    /// The driver connection whose output device is attached.
    attached: Option<u64>,
    /// The driver connection whose input device is attached, and whether it
    /// is the test microphone.
    input_attached: Option<(u64, Driver)>,
    /// The sound card devmgr said is coming, and on which connection.
    coming: Option<(u64, String)>,
    next_key: u64,
    /// What the loop waits on, kept from pass to pass.
    items: Vec<WaitItem>,
    keys: Vec<u64>,
    closed: Vec<u64>,
}

struct ClientSession<'a> {
    mixer: &'a mut Mixer,
    capture: &'a mut Capture,
    owner: u64,
    /// The client is the voice agent.
    agent: bool,
    /// No output device is attached, but one is coming.
    coming: bool,
}

impl audio::Server for ClientSession<'_> {
    fn open_output(&mut self, spec: StreamSpec) -> Result<StreamHandle, AudioError> {
        let r = self.mixer.open(self.owner, &spec);
        if let Ok(h) = &r {
            println!("stream {} \"{}\" opened ({} Hz, {} ch)", h.id, spec.name, spec.rate, spec.channels);
        }
        r
    }

    fn close(&mut self, stream: u32) -> Result<(), AudioError> {
        if stream & capture::ID_BIT != 0 {
            let r = self.capture.close(self.owner, stream);
            self.mixer.set_reference_wanted(self.capture.wants_reference());
            return r;
        }
        self.mixer.close(self.owner, stream)
    }

    fn set_paused(&mut self, stream: u32, paused: bool) -> Result<(), AudioError> {
        self.mixer.set_paused(self.owner, stream, paused)
    }

    fn set_volume(&mut self, stream: u32, volume: f32) -> Result<(), AudioError> {
        self.mixer.set_volume(self.owner, stream, volume)
    }

    fn flush(&mut self, stream: u32) -> Result<u64, AudioError> {
        self.mixer.flush(self.owner, stream)
    }

    fn status(&mut self) -> AudioStatus {
        let mut s = self.mixer.status();
        s.coming = self.coming;
        s.input_device = self.capture.device_name().into();
        s.input_rate = self.capture.device_rate();
        s.input_channels = self.capture.device_channels();
        s
    }

    fn set_master(&mut self, volume: f32, muted: bool) {
        self.mixer.master = if volume.is_finite() { volume.clamp(0.0, 1.0) } else { 1.0 };
        self.mixer.muted = muted;
    }

    fn open_input(&mut self, spec: InputSpec) -> Result<InputHandle, AudioError> {
        let r = self.capture.open(self.owner, &spec);
        if let Ok(h) = &r {
            println!(
                "capture stream {} \"{}\" opened ({} Hz, {} ch{})",
                h.id & !capture::ID_BIT,
                spec.name,
                spec.rate,
                spec.channels,
                if spec.echo_cancel { ", echo cancelled" } else { "" }
            );
        }
        self.mixer.set_reference_wanted(self.capture.wants_reference());
        r
    }

    fn set_ducking(&mut self, on: bool) -> Result<(), AudioError> {
        if !self.agent {
            return Err(AudioError::Denied);
        }
        self.mixer.set_ducking(self.owner, on);
        Ok(())
    }
}

struct DriverSession<'a> {
    mixer: &'a mut Mixer,
    capture: &'a mut Capture,
    attached: bool,
    input_attached: bool,
    /// The connection, and who it is.
    key: u64,
    driver: Driver,
    /// The input device attached now is the test microphone.
    test_input: bool,
    coming: &'a mut Option<(u64, String)>,
}

impl audiodev::Server for DriverSession<'_> {
    fn attach(&mut self, format: DeviceFormat) -> Result<DeviceLink, AudioError> {
        if self.driver != Driver::Devmgr {
            return Err(AudioError::Denied);
        }
        let link = self.mixer.attach(&format)?;
        println!(
            "output device \"{}\" attached ({} Hz, {} ch, {} frames per period)",
            format.name, format.rate, format.channels, format.period_frames
        );
        self.attached = true;
        Ok(link)
    }

    fn attach_input(&mut self, format: DeviceFormat) -> Result<InputLink, AudioError> {
        match self.driver {
            Driver::Other => return Err(AudioError::Denied),
            // A sound card's microphone takes the test microphone's place.
            Driver::Devmgr if self.test_input => {
                println!("the test microphone gives way to \"{}\"", format.name);
                self.capture.detach();
            }
            _ => {}
        }
        let link = self.capture.attach(&format)?;
        println!(
            "input device \"{}\" attached ({} Hz, {} ch, {} frames per period)",
            format.name, format.rate, format.channels, format.period_frames
        );
        self.input_attached = true;
        Ok(link)
    }

    fn expect(&mut self, device: Option<String>) -> Result<(), AudioError> {
        if self.driver != Driver::Devmgr {
            return Err(AudioError::Denied);
        }
        match &device {
            Some(device) => println!("a sound card is coming ({})", device),
            None if self.coming.is_some() => println!("no sound card is coming any more"),
            None => {}
        }
        *self.coming = device.map(|d| (self.key, d));
        Ok(())
    }
}

impl Service {
    fn accept(&mut self, listener: &Channel, drivers: bool) {
        while let Some((ch, who)) = vproto::accept_with_identity(listener) {
            let kind = if drivers {
                let driver = Driver::of(&who);
                if driver == Driver::Other {
                    // It could attach nothing, and would take a driver's place.
                    println!("{} may not attach sound devices; refused", who.name);
                    continue;
                }
                Kind::Driver(driver)
            } else {
                Kind::Client { agent: who.service && who.name == "agent" }
            };
            let same = self.conns.values().filter(|(k, _)| matches!(k, Kind::Driver(_)) == drivers).count();
            if same >= if drivers { MAX_DRIVERS } else { MAX_CLIENTS } {
                println!("too many connections; refusing one from {}", who.name);
                continue;
            }
            self.conns.insert(self.next_key, (kind, ch));
            self.next_key += 1;
        }
    }

    /// Serves every queued request on a connection.
    fn serve(&mut self, key: u64) {
        loop {
            let Some((kind, ch)) = self.conns.get(&key) else { return };
            let kind = *kind;
            let Ok(msg) = ch.read() else { return };
            let reply = match kind {
                Kind::Client { agent } => audio::dispatch(
                    &mut ClientSession {
                        mixer: &mut self.mixer,
                        capture: &mut self.capture,
                        owner: key,
                        agent,
                        coming: self.coming.is_some() && self.attached.is_none(),
                    },
                    msg,
                ),
                Kind::Driver(driver) => {
                    let mut s = DriverSession {
                        mixer: &mut self.mixer,
                        capture: &mut self.capture,
                        attached: false,
                        input_attached: false,
                        key,
                        driver,
                        test_input: self.input_attached.is_some_and(|(_, d)| d == Driver::TestMicrophone),
                        coming: &mut self.coming,
                    };
                    let r = audiodev::dispatch(&mut s, msg);
                    if s.attached {
                        self.attached = Some(key);
                    }
                    if s.input_attached {
                        self.input_attached = Some((key, driver));
                    }
                    r
                }
            };
            match reply {
                Ok(reply) => {
                    if let Some((_, ch)) = self.conns.get(&key) {
                        let _ = reply.send(ch);
                    }
                }
                Err(e) => println!("bad request: {}", e),
            }
        }
    }

    fn disconnect(&mut self, key: u64) {
        let Some((kind, _)) = self.conns.remove(&key) else { return };
        match kind {
            Kind::Client { .. } => {
                self.mixer.close_owner(key);
                self.capture.close_owner(key);
                self.mixer.set_reference_wanted(self.capture.wants_reference());
            }
            Kind::Driver(_) => {
                if self.coming.as_ref().is_some_and(|(k, _)| *k == key) {
                    self.coming = None;
                }
                if self.attached == Some(key) {
                    self.attached = None;
                    self.mixer.detach();
                    println!("output device detached; continuing without sound");
                }
                if self.input_attached.is_some_and(|(k, _)| k == key) {
                    self.input_attached = None;
                    self.capture.detach();
                    println!("input device detached");
                }
            }
        }
    }

    fn add(&mut self, handle: vabi::RawHandle, signals: u32, key: u64) {
        self.items.push(WaitItem { handle, signals, ..Default::default() });
        self.keys.push(key);
    }

    fn run(&mut self, audio_listener: Channel, dev_listener: Channel) -> ! {
        loop {
            let now = vrt::time::now_ns();
            self.items.clear();
            self.keys.clear();
            self.add(audio_listener.raw(), signals::READABLE, KEY_AUDIO_LISTENER);
            self.add(dev_listener.raw(), signals::READABLE, KEY_DEV_LISTENER);
            if let Some(ev) = self.mixer.space_event() {
                let raw = ev.raw();
                self.add(raw, signals::SIGNALED, KEY_SPACE);
            }
            if let Some(ev) = self.capture.data_event() {
                let raw = ev.raw();
                self.add(raw, signals::SIGNALED, KEY_CAPTURE);
            }
            for (&k, (_, ch)) in &self.conns {
                self.items.push(WaitItem {
                    handle: ch.raw(),
                    signals: signals::READABLE | signals::PEER_CLOSED,
                    ..Default::default()
                });
                self.keys.push(k);
            }
            let _ = vrt::object::wait_many(&mut self.items, self.mixer.deadline(now));
            self.closed.clear();
            for i in 0..self.items.len() {
                let (observed, key) = (self.items[i].observed, self.keys[i]);
                if observed == 0 {
                    continue;
                }
                match key {
                    KEY_AUDIO_LISTENER => self.accept(&audio_listener, false),
                    KEY_DEV_LISTENER => self.accept(&dev_listener, true),
                    KEY_SPACE => {
                        if let Some(ev) = self.mixer.space_event() {
                            let _ = ev.clear();
                        }
                    }
                    // Captured frames are taken after mixing, below.
                    KEY_CAPTURE => {}
                    k => {
                        if observed & signals::READABLE != 0 {
                            self.serve(k);
                        }
                        if observed & signals::PEER_CLOSED != 0 && observed & signals::READABLE == 0 {
                            self.closed.push(k);
                        }
                    }
                }
            }
            for i in 0..self.closed.len() {
                let k = self.closed[i];
                self.disconnect(k);
            }
            let now = vrt::time::now_ns();
            self.mixer.run(now);
            self.capture.run(&self.mixer, now);
        }
    }
}

fn main() -> i32 {
    let audio_listener = match vproto::register(audio::NAME) {
        Ok(l) => l,
        Err(e) => {
            println!("cannot register: {:?}", e);
            return 1;
        }
    };
    let dev_listener = match vproto::register(audiodev::NAME) {
        Ok(l) => l,
        Err(e) => {
            println!("cannot register the device endpoint: {:?}", e);
            return 1;
        }
    };
    println!("ready (mixing at {} Hz until a device attaches)", mixer::NULL_RATE);
    // Mixing is time-critical: run above normal applications.
    let worker = vrt::thread::Builder::new().name("mixer").priority(vabi::priority::HIGH).spawn(move || -> () {
        let mut svc = Service {
            mixer: Mixer::new(),
            capture: Capture::new(),
            conns: BTreeMap::new(),
            attached: None,
            input_attached: None,
            coming: None,
            next_key: FIRST_CONN,
            items: Vec::with_capacity(MAX_CLIENTS + MAX_DRIVERS + 4),
            keys: Vec::with_capacity(MAX_CLIENTS + MAX_DRIVERS + 4),
            closed: Vec::with_capacity(MAX_CLIENTS + MAX_DRIVERS),
        };
        svc.run(audio_listener, dev_listener)
    });
    match worker {
        Ok(handle) => {
            let _ = handle.join();
            0
        }
        Err(e) => {
            println!("cannot start the mixer thread: {}", e);
            1
        }
    }
}

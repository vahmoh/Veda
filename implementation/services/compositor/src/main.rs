// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `compositor` — the Veda window system.
//!
//! Owns the screen and composes client windows onto it. It provides three
//! services:
//!
//! * `display` — clients create windows, attach shared pixel buffers and
//!   present frames, and present the pictures of windows' subsurfaces
//!   (see `proto::display`);
//! * `input` — device drivers report keyboard and pointer events;
//! * `displaydev` — a display driver hands over pictures it flips between
//!   at the vertical blank (see `screen`).
//!
//! What it decides (windows, focus, input, who may do what) is
//! `compositor_core`'s, tested on the host; this program serves the
//! protocols, maps clients' memory and draws.
//!
//! Rendering is damage driven: only regions that changed are recomposited,
//! at most once per display frame: by the GPU, straight into the picture
//! the display shows next, where a driver flips and the GPU can draw into
//! its pictures (`gpu`); otherwise by the processor into the back buffer,
//! which is copied to the screen.
//!
//! One loop serves everything, so that no client can hold it: each wake-up
//! serves the input drivers first, then every client with requests waiting,
//! a few requests each in turn, composing a frame between them as soon as
//! one is due.

#![no_std]
#![no_main]

extern crate alloc;

mod decor;
mod gpu;
mod pixels;
mod render;
mod screen;
mod session;
mod startup;
mod state;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use abi::signals;
use compositor_core::Wm;
use compositor_core::clients::Identity;
use gfx::{Rect, Text};
use ipc::WaitSet;
use proto::display::{Cursor, display};
use proto::displaydev::{self, DisplayDevError, Link, displaydev as driver_protocol};
use proto::init::ClientIdentity;
use proto::input::InputEvent;
use rt::object::{Channel, Vmo};
use rt::println;
use rt::vm::Mapping;

use decor::Decor;
use screen::Screen;
use session::Session;
use startup::Startup;
use state::Compositor;

rt::entry!(main);

/// How many requests a client has served at a wake-up before the others,
/// how many reports an input driver, how many requests a display driver,
/// and how many connections each service accepts.
const CLIENT_BUDGET: usize = 16;
const INPUT_BUDGET: usize = 64;
const DRIVER_BUDGET: usize = 16;
const ACCEPT_BUDGET: usize = 16;

/// Reads a font file from the system image and leaks it (fonts live forever).
fn load_font(vfs: &proto::vfs::Client, path: &str) -> Option<&'static [u8]> {
    let (vmo, len) = vfs.read_file(path.into()).ok()?.ok()?;
    let mut data = alloc::vec![0u8; len as usize];
    vmo.read(0, &mut data).ok()?;
    Some(data.leak())
}

/// Whether a connection was opened by a driver: devmgr starts the drivers
/// (and the driver VM, whose programs connect through it) with its own
/// registry channel, so they speak as devmgr.
fn is_driver(who: &ClientIdentity) -> bool {
    who.service && who.name == "devmgr"
}

/// Logs `what` the first few times, then every hundredth (`count` counts
/// them), so that a client repeating it cannot flood the log.
fn log_now_and_then(count: &AtomicU32, what: impl FnOnce()) {
    let n = count.fetch_add(1, Ordering::Relaxed);
    if n < 3 || n.is_multiple_of(100) {
        what();
    }
}

static REFUSED_CONNECTIONS: AtomicU32 = AtomicU32::new(0);
static REFUSED_INPUT: AtomicU32 = AtomicU32::new(0);
static REFUSED_DRIVERS: AtomicU32 = AtomicU32::new(0);
static DROPPED_CLIENTS: AtomicU32 = AtomicU32::new(0);

/// A display driver's connection (`displaydev`): only drivers' are
/// accepted ([`is_driver`]).
struct DriverLink<'a> {
    screen: &'a mut Screen,
    key: u64,
    /// The firmware's framebuffer (physical address).
    framebuffer: u64,
}

impl driver_protocol::Server for DriverLink<'_> {
    fn attach(&mut self, screen: displaydev::Screen, link: Link) -> Result<(), DisplayDevError> {
        let name = screen.name.clone();
        let result = self.screen.attach(self.key, screen, link, self.framebuffer);
        // A mismatch is told in detail where it is found.
        if let Err(e) = result
            && e != DisplayDevError::Mismatch
        {
            println!("{} cannot have the screen: {}", name, e);
        }
        result
    }

    fn screen(&mut self) -> Result<displaydev::ScreenMode, DisplayDevError> {
        let r = self.screen.rect();
        Ok(displaydev::ScreenMode {
            width: r.w as u32,
            height: r.h as u32,
            rgbx: self.screen.rgb(),
            framebuffer: self.framebuffer,
        })
    }

    fn expect(&mut self, device: Option<String>) -> Result<(), DisplayDevError> {
        self.screen.expect(self.key, device);
        Ok(())
    }
}

/// Serves up to [`CLIENT_BUDGET`] requests of the display client on `key`,
/// at `now`.
fn serve_client(comp: &mut Compositor, key: u64, now: u64) {
    for _ in 0..CLIENT_BUDGET {
        let Some(msg) = comp.clients.get(&key).and_then(|c| c.read().ok()) else { return };
        comp.wm.request(key, now);
        let reply = display::dispatch(&mut Session { comp: &mut *comp, client: key, now }, msg);
        let sent = match (reply, comp.clients.get(&key)) {
            (Ok(reply), Some(c)) => reply.send(c),
            (Ok(_), None) => Ok(()),
            (Err(e), _) => Err(e),
        };
        match sent {
            Ok(()) => comp.settle(),
            // Refused (the client was told so): the compositor's answer was
            // larger than a message, which it never should be.
            Err(ipc::IpcError::TooLarge) => {
                println!("an answer to a client was too large to send");
                comp.settle();
            }
            // It left without its answer.
            Err(ipc::IpcError::PeerClosed) => {
                comp.drop_client(key);
                return;
            }
            // Answers it does not read (its channel is full), or a message
            // without a header: it is sent away, its windows with it.
            Err(e) => {
                let why = match e {
                    ipc::IpcError::Kernel(abi::Error::ShouldWait) => "it does not read its answers",
                    _ => "what it sent is not a request",
                };
                log_now_and_then(&DROPPED_CLIENTS, || {
                    let name = comp.wm.client(key).map_or("a client", |c| c.identity.name.as_str());
                    println!("{}'s connection to the window system is closed: {}", name, why);
                });
                comp.drop_client(key);
                return;
            }
        }
    }
}

/// Serves up to [`INPUT_BUDGET`] reports of the input driver on `key`, at
/// `now`.
fn serve_input(comp: &mut Compositor, inputs: &BTreeMap<u64, Channel>, key: u64, now: u64) {
    for _ in 0..INPUT_BUDGET {
        let Some(msg) = inputs.get(&key).and_then(|c| c.read().ok()) else { return };
        if let Ok((_, events)) = ipc::decode_event::<Vec<InputEvent>>(msg) {
            for ev in events {
                comp.wm.input(key, ev, now);
            }
        }
        comp.settle();
    }
}

fn main() -> i32 {
    use abi::startup::role;
    let (Some(fb_vmo), Some(info_vmo)) = (
        rt::env::take_handle(role::FRAMEBUFFER).map(Vmo::from_handle),
        rt::env::take_handle(role::BOOT_INFO).map(Vmo::from_handle),
    ) else {
        println!("no framebuffer");
        return 1;
    };
    let mut raw = [0u8; core::mem::size_of::<abi::KernelBootInfo>()];
    let _ = info_vmo.read(0, &mut raw);
    // SAFETY: plain data written by the kernel.
    let info: abi::KernelBootInfo = unsafe { core::ptr::read_unaligned(raw.as_ptr() as *const abi::KernelBootInfo) };
    let (width, height, pitch) =
        (info.framebuffer_width as i32, info.framebuffer_height as i32, info.framebuffer_pitch as usize);
    if width <= 0 || height <= 0 || !screen::pitch_fits(pitch, width) {
        println!("the firmware's framebuffer ({}x{}, {} bytes a row) is not a screen", width, height, pitch);
        return 1;
    }
    let fb_size = (pitch * height as usize).next_multiple_of(4096);
    let fb = match Mapping::new(fb_vmo, fb_size, abi::map_flags::READ | abi::map_flags::WRITE) {
        Ok(m) => m,
        Err(e) => {
            println!("cannot map the framebuffer: {}", e);
            return 1;
        }
    };

    let mut text = Text::new();
    // The fonts' indices: Inter SemiBold (titles), Inter Regular.
    let mut fonts = [None; 3];
    if let Ok(ch) = proto::connect(proto::vfs::NAME) {
        let vfs = proto::vfs::Client::new(ch);
        for (i, path) in
            ["/system/fonts/Inter-SemiBold.otf", "/system/fonts/Inter-Regular.otf", "/system/fonts/Lato-Regular.ttf"]
                .iter()
                .enumerate()
        {
            if let Some(data) = load_font(&vfs, path) {
                fonts[i] = text.add_font(data);
            }
        }
    }
    let title_font = fonts[0].unwrap_or(0);
    let regular_font = fonts[1].unwrap_or(title_font);
    if text.font_count() == 0 {
        println!("warning: no fonts available, titles will be blank");
    }

    let display_listener = proto::register(display::NAME).expect("cannot register the display service");
    let input_listener = proto::register(proto::input::NAME).expect("cannot register the input service");
    let driver_listener = proto::register(driver_protocol::NAME).expect("cannot register the display driver service");
    println!("display {}x{} ready", width, height);

    let started = rt::time::now_ns();
    let mut comp = Compositor {
        wm: Wm::new(Rect::new(0, 0, width, height)),
        screen: Screen::new(fb, pitch, info.framebuffer_format == 2, width, height, started),
        decor: Decor::new(text, title_font),
        events: BTreeMap::new(),
        subsurface_events: BTreeMap::new(),
        clients: BTreeMap::new(),
        cursor: (Cursor::Arrow, (i32::MIN, i32::MIN)),
        cursor_rect: Rect::default(),
        thumbs: None,
        last_frame: 0,
        startup: None,
        gpu: None,
        gpu_ready: None,
        gpu_setup: None,
        gpu_tried: None,
        gpu_losses: 0,
        lock_watchers: Vec::new(),
        told_locked: false,
    };
    // With the lock screen installed, the window system starts locked, at
    // system start and after a crash alike: the lock service unlocks it
    // (at once, if no password is set).
    if rt::env::args().iter().any(|a| a == "locked") {
        comp.wm.lock_at_start();
        comp.told_locked = true;
    }
    // At system start (not after a restart), the screen stays black until
    // the display is up, then the splash fades in, comes to life and
    // dissolves into the desktop once it has drawn itself.
    if rt::env::args().iter().any(|a| a == "splash") {
        let fonts = (title_font, regular_font);
        comp.startup = Some(Startup::new(width, height, &mut comp.decor.text, fonts, started));
    }
    comp.update_cursor();
    let screen = comp.screen_rect();
    comp.wm.add_damage(screen);
    comp.composite();

    // Input drivers' and display drivers' connections.
    let mut inputs: BTreeMap<u64, Channel> = BTreeMap::new();
    let mut drivers: BTreeMap<u64, Channel> = BTreeMap::new();
    let framebuffer = info.framebuffer_phys;
    let mut next_key = 10u64;
    const DISPLAY_KEY: u64 = 1;
    const INPUT_KEY: u64 = 2;
    const DRIVER_KEY: u64 = 3;
    const FLIP_DONE_KEY: u64 = 4;
    const GPU_SETUP_KEY: u64 = 5;
    const GPU_FENCE_KEY: u64 = 6;
    const INPUT_BASE: u64 = 1 << 40;
    const DRIVER_BASE: u64 = 1 << 41;
    const WRITABLE_BASE: u64 = 1 << 42;
    const SUBSURFACE_WRITABLE_BASE: u64 = 1 << 43;
    // Kept from one wait to the next: its handles stay registered with the
    // kernel, so that a wait costs what is ready, however many clients.
    let mut ws = WaitSet::new();
    loop {
        let now = rt::time::now_ns();
        comp.screen.check(now);
        // A driver that flips gives pictures the GPU may draw into: it is
        // set up for them (once for each driver's), and no longer for the
        // pictures of a driver that went away.
        if let Some(driver) = comp.screen.driver()
            && comp.gpu_tried != Some(driver)
        {
            comp.gpu_tried = Some(driver);
            comp.gpu_ready = None;
            comp.drop_gpu("another display driver attached");
            comp.gpu_setup = comp.screen.pictures().and_then(gpu::Pending::start);
        }
        // The splash fades in once the screen is up.
        if let Some(s) = &mut comp.startup {
            s.release(now, comp.screen.settled());
        }
        let mut deadline = abi::DEADLINE_INFINITE;
        let mut until = |t: Option<u64>| {
            if let Some(t) = t {
                deadline = deadline.min(t);
            }
        };
        until(comp.startup.as_ref().and_then(|s| s.deadline()));
        until(comp.wants_frame().then(|| comp.screen.next_frame(comp.last_frame).max(now)));
        until(comp.screen.deadline());
        until(comp.gpu_deadline());
        until(comp.wm.repeat_deadline());
        until(comp.wm.liveness_deadline());
        until(comp.wm.idle_deadline());
        ws.clear();
        for (&k, c) in &inputs {
            ws.add(c.raw(), signals::READABLE | signals::PEER_CLOSED, INPUT_BASE | k);
        }
        ws.add(display_listener.raw(), signals::READABLE, DISPLAY_KEY);
        ws.add(input_listener.raw(), signals::READABLE, INPUT_KEY);
        ws.add(driver_listener.raw(), signals::READABLE, DRIVER_KEY);
        if let Some(done) = comp.screen.done_event() {
            ws.add(done, signals::SIGNALED, FLIP_DONE_KEY);
        }
        if let Some(setup) = &comp.gpu_setup {
            ws.add(setup.event(), signals::SIGNALED, GPU_SETUP_KEY);
        }
        if let Some(g) = &comp.gpu
            && comp.screen.drawing().is_some()
        {
            ws.add(g.fence_event(), signals::SIGNALED, GPU_FENCE_KEY);
        }
        for (&k, c) in &drivers {
            ws.add(c.raw(), signals::READABLE | signals::PEER_CLOSED, DRIVER_BASE | k);
        }
        for (&k, c) in &comp.clients {
            ws.add(c.raw(), signals::READABLE | signals::PEER_CLOSED, k);
        }
        // Windows whose clients fell behind: their events go out as there
        // is room.
        for id in comp.wm.blocked() {
            if let Some(c) = comp.events.get(&id) {
                ws.add(c.raw(), signals::WRITABLE | signals::PEER_CLOSED, WRITABLE_BASE | u64::from(id));
            }
        }
        for id in comp.wm.blocked_subsurfaces() {
            if let Some(c) = comp.subsurface_events.get(&id) {
                ws.add(c.raw(), signals::WRITABLE | signals::PEER_CLOSED, SUBSURFACE_WRITABLE_BASE | u64::from(id));
            }
        }
        let mut ready = ws.wait(deadline).unwrap_or_default();
        // The input drivers first: what they report is served first.
        ready.sort_by_key(|&(key, _)| key & INPUT_BASE == 0);
        let now = rt::time::now_ns();
        for (key, observed) in ready {
            match key {
                DISPLAY_KEY => {
                    for _ in 0..ACCEPT_BUDGET {
                        let Some((ch, who)) = proto::accept_with_identity(&display_listener) else { break };
                        next_key += 1;
                        let name = who.name.clone();
                        let identity = Identity { koid: who.koid, name: who.name, service: who.service };
                        match comp.wm.connect(next_key, identity, now) {
                            Ok(()) => {
                                comp.clients.insert(next_key, ch);
                            }
                            Err(_) => log_now_and_then(&REFUSED_CONNECTIONS, || {
                                println!("{} has as many connections to the window system as it may", name)
                            }),
                        }
                    }
                }
                INPUT_KEY => {
                    for _ in 0..ACCEPT_BUDGET {
                        let Some((ch, who)) = proto::accept_with_identity(&input_listener) else { break };
                        if is_driver(&who) {
                            next_key += 1;
                            inputs.insert(next_key, ch);
                        } else {
                            log_now_and_then(&REFUSED_INPUT, || {
                                println!("{} may not report input: only drivers may", who.name)
                            });
                        }
                    }
                }
                DRIVER_KEY => {
                    for _ in 0..ACCEPT_BUDGET {
                        let Some((ch, who)) = proto::accept_with_identity(&driver_listener) else { break };
                        // Nobody else may attach, nor say what is coming: the
                        // connection is closed at once (rather than kept, to
                        // be refused each request).
                        if is_driver(&who) {
                            next_key += 1;
                            drivers.insert(next_key, ch);
                        } else {
                            log_now_and_then(&REFUSED_DRIVERS, || {
                                println!("{} cannot have the screen: only drivers may attach", who.name)
                            });
                        }
                    }
                }
                FLIP_DONE_KEY => comp.screen.flip_done(),
                GPU_SETUP_KEY => {
                    if let Some(result) = comp.gpu_setup.as_ref().and_then(|s| s.take()) {
                        comp.gpu_setup = None;
                        match result {
                            Ok(g) => comp.gpu_ready = Some(g),
                            Err(why) => println!("frames stay with the processor: {}", why),
                        }
                    }
                }
                GPU_FENCE_KEY => comp.gpu_signaled(now),
                k if k & WRITABLE_BASE != 0 => comp.writable((k & !WRITABLE_BASE) as u32),
                k if k & SUBSURFACE_WRITABLE_BASE != 0 => {
                    comp.subsurface_writable((k & !SUBSURFACE_WRITABLE_BASE) as u32)
                }
                k if k & DRIVER_BASE != 0 => {
                    let k = k & !DRIVER_BASE;
                    if observed & signals::READABLE != 0 {
                        for _ in 0..DRIVER_BUDGET {
                            let Some(Ok(msg)) = drivers.get(&k).map(|c| c.read()) else { break };
                            let mut link = DriverLink { screen: &mut comp.screen, key: k, framebuffer };
                            match driver_protocol::dispatch(&mut link, msg) {
                                Ok(reply) => {
                                    if let Some(c) = drivers.get(&k) {
                                        let _ = reply.send(c);
                                    }
                                }
                                Err(_) => {
                                    // Not the protocol: the connection goes.
                                    drivers.remove(&k);
                                    comp.screen.detach(k);
                                }
                            }
                        }
                    } else if observed & signals::PEER_CLOSED != 0 {
                        drivers.remove(&k);
                        comp.screen.detach(k);
                    }
                }
                k if k & INPUT_BASE != 0 => {
                    let k = k & !INPUT_BASE;
                    if observed & signals::READABLE != 0 {
                        serve_input(&mut comp, &inputs, k, now);
                    } else if observed & signals::PEER_CLOSED != 0 {
                        // What it held is released for it.
                        inputs.remove(&k);
                        comp.wm.source_gone(k, now);
                        comp.settle();
                    }
                }
                k => {
                    if observed & signals::READABLE != 0 {
                        serve_client(&mut comp, k, now);
                    } else if observed & signals::PEER_CLOSED != 0 {
                        comp.drop_client(k);
                    }
                    // A frame due is composed before the next client is
                    // served.
                    if comp.frame_due(rt::time::now_ns()) {
                        comp.composite();
                    }
                }
            }
        }
        let now = rt::time::now_ns();
        comp.wm.repeat(now);
        if comp.wm.liveness_deadline().is_some_and(|t| now >= t) {
            comp.wm.check_liveness(now);
        }
        // After everything that may have kept the screen awake, or stopped.
        if comp.wm.check_idle(now) {
            println!("no input for {} s: the screen locks", comp.wm.idle_timeout() / 1_000_000_000);
        }
        comp.settle();
        // A frame the GPU draws that takes too long.
        if comp.gpu_deadline().is_some_and(|t| now >= t) {
            comp.gpu_signaled(now);
        }
        comp.screen.check(now);
        if comp.frame_due(now) {
            comp.composite();
        }
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `fakedec` — a stand-in for the media decoder, `mediadec`: it speaks
//! [`vproto::media`] as `mediadec` does, with pictures it draws and a tone
//! in place of a file's, so that Videos' playback, pacing, seeking, crash
//! recovery and agent can be developed and tested without FFmpeg.
//!
//! Videos starts it as it starts `mediadec` (`/system/bin/fakedec`),
//! with nothing but the control channel ([`vproto::media::CONTROL_ROLE`]).
//! The videos it opens are text files ([`clip`]):
//!
//! ```text
//! FAKEVIDEO duration=12 fps=30 w=640 h=360 title=Test_Pattern
//! ```
//!
//! with faults to ask for: `crash=5` (the decoder crashes decoding 5 s),
//! `hang=7` (it stops answering and working for good at 7 s), `slow_ms=50`
//! (each picture takes 50 ms to decode). Anything else is refused as
//! damaged ([`vproto::media::MediaError::Corrupt`]).
//!
//! Each picture shows its time and number, and a marker that is at the
//! left edge on every whole second, when the sound (a quiet 440 Hz tone)
//! clicks: a picture and the sound heard with it can be told apart, and
//! their timing checked, on the screen and in the log of a test.
//!
//! * [`session`]: the protocol's calls and the work between them, as
//!   `mediadec`'s session does them.
//! * [`clip`]: what a fake video is, and the times of its pictures and
//!   sound.
//! * [`picture`]: the pictures, drawn.
//! * [`sound`]: the tone and its clicks.

#![no_std]
#![no_main]

extern crate alloc;

mod clip;
mod picture;
mod session;
mod sound;

use alloc::vec::Vec;

use vabi::{WaitItem, signals};
use vproto::media;
use vrt::object::Channel;
use vrt::println;
use vrt::time::Duration;

use crate::session::Session;

vrt::entry!(main);

/// The longest it waits without beating (ns), as `mediadec`.
const BEAT_NS: u64 = 1_000_000_000;

/// Answers the player and decodes until the player closes the control
/// channel (as `mediadec_main` does).
fn main() -> i32 {
    let Some(control) = vrt::env::take_handle(media::CONTROL_ROLE) else {
        println!("started without a control channel");
        return 1;
    };
    let control = Channel::from_handle(control);
    // As `mediadec` does, before reading anything.
    if let Err(e) = vrt::process::restrict(vabi::restrict::ALL) {
        println!("cannot give up what it does without ({e:?})");
        return 1;
    }
    let mut session = Session::new();
    loop {
        // Requests first: each waits at most for one picture.
        loop {
            match control.read() {
                Ok(msg) => match media::media::dispatch(&mut session, msg) {
                    Ok(reply) => {
                        if reply.send(&control).is_err() {
                            return 0;
                        }
                    }
                    Err(e) => {
                        println!("a request it cannot read ({e:?}): stopping");
                        return 1;
                    }
                },
                Err(vabi::Error::ShouldWait) => break,
                Err(_) => return 0,
            }
        }
        session.beat();
        if session.work() {
            continue;
        }
        let mut items: Vec<WaitItem> = Vec::with_capacity(3);
        items.push(WaitItem {
            handle: control.raw(),
            signals: signals::READABLE | signals::PEER_CLOSED,
            ..Default::default()
        });
        for wake in session.wakes() {
            items.push(WaitItem { handle: wake, signals: signals::SIGNALED, ..Default::default() });
        }
        // Idle, it still beats every second (the player takes a silent
        // heartbeat for a hung decoder).
        match vrt::object::wait_many(&mut items, vrt::time::now_ns() + BEAT_NS) {
            Ok(_) | Err(vabi::Error::TimedOut) => {}
            // A handle it cannot wait on (an event that is not one): poll
            // rather than spin.
            Err(_) => vrt::time::sleep(Duration::from_millis(10)),
        }
        if items[0].observed & signals::PEER_CLOSED != 0 && items[0].observed & signals::READABLE == 0 {
            return 0;
        }
        session.clear_wakes();
    }
}

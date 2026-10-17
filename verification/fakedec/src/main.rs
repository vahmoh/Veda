// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `fakedec` — a stand-in for the media decoder, `mediadec`: it speaks
//! [`proto::media`] as `mediadec` does, with pictures it draws and a tone
//! in place of a file's, so that Videos' playback, pacing, seeking, crash
//! recovery and agent can be developed and tested without FFmpeg.
//!
//! Videos starts it as it starts `mediadec` (`/system/bin/fakedec`),
//! with nothing but the control channel ([`proto::media::CONTROL_ROLE`]);
//! as `mediadec`, it ends when the player goes away, even hung.
//! The videos it opens are text files ([`clip`]):
//!
//! ```text
//! FAKEVIDEO duration=12 fps=30 w=640 h=360 title=Test_Pattern
//! ```
//!
//! with faults to ask for: `crash=5` (the decoder crashes decoding 5 s),
//! `hang=7` (it stops answering and working for good at 7 s), `slow_ms=50`
//! (each picture takes 50 ms to decode). Anything else is refused as
//! damaged ([`proto::media::MediaError::Corrupt`]).
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

use proto::media;
use rt::object::Channel;
use rt::println;

use crate::session::Session;

rt::entry!(main);

/// Answers the player and decodes until the player closes the control
/// channel (as `mediadec_main` does).
fn main() -> i32 {
    let Some(control) = rt::env::take_handle(media::CONTROL_ROLE) else {
        println!("started without a control channel");
        return 1;
    };
    let control = Channel::from_handle(control);
    // As `mediadec`'s watchdog: the player gone, the decoder ends, hung or
    // not.
    let handle = control.raw();
    let watchdog =
        rt::thread::Builder::new().name("watchdog").stack_size(16 << 10).spawn(move || media::watchdog(handle));
    if watchdog.is_err() {
        println!("cannot start its watchdog");
    }
    // As `mediadec` does, before reading anything.
    if let Err(e) = rt::process::restrict(abi::restrict::ALL) {
        println!("cannot give up what it does without ({e:?})");
        return 1;
    }
    let mut session = Session::new();
    media::serve(&control, &mut session)
}

//! `videos-core` — what the Videos app decides, apart from what it draws
//! and whom it talks to, so that it can be tested on the host.
//!
//! * [`clock`]: media time, from the sound as it is heard (or the system's
//!   clock for silent videos).
//! * [`pacing`]: when a picture presented now reaches the screen.
//! * [`select`]: which of the decoded pictures to show, and when the next is
//!   due.
//! * [`skip`]: when to ask the decoder to leave pictures out to keep up.
//! * [`stats`]: how playback went, for the log.
//! * [`layout`]: where a video goes in a window, and at what size it is
//!   decoded.
//! * [`store`]: what Videos remembers (volume, where each video was left).
//! * [`find`]: videos by their names, as people say them.
//! * [`text`]: times and descriptions for people.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod clock;
pub mod find;
pub mod layout;
pub mod pacing;
pub mod select;
pub mod skip;
pub mod stats;
pub mod store;
pub mod text;

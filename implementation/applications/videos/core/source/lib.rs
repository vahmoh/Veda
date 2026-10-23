// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `videos-core` — what the Videos app decides, apart from what it draws
//! and whom it talks to, so that it can be tested on the host.
//!
//! * [`playback`]: what the player does with what the engine, the audio
//!   thread and the pictures say, and with what the user asks.
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
//! * [`text`]: times and descriptions for people.
//!
//! And what Music shares with Videos, until it finds a home both depend on:
//!
//! * [`output`]: an output stream of the audio service that outlives the
//!   service (opened again when it comes back).
//! * [`control`]: what the voice agent's volume and seek requests come to.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod clock;
pub mod control;
pub mod layout;
pub mod output;
pub mod pacing;
pub mod playback;
pub mod select;
pub mod skip;
pub mod stats;
pub mod store;
pub mod text;

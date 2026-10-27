// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The logic of the voice agent that lives in Veda.
//!
//! The agent's program (`main.rs`) does the I/O — audio, the network,
//! talking to applications — and uses this library for everything that can
//! be decided without it, so that it can be tested on the host:
//!
//! * [`deepgram`]: the Deepgram Voice Agent protocol (the `Settings`
//!   message, server messages, client messages), streaming speech-to-text
//!   for hearing the agent's name, and the REST catalogs.
//! * [`tools`]: the functions the language model may call, their JSON
//!   schemas and how much each can affect the user ([`tools::Risk`]), and
//!   what applications say they offer, made safe to use.
//! * [`files`]: where the agent's file actions may act (never in a private
//!   directory).
//! * [`prompt`]: the agent's character and instructions, and the
//!   catalogue of applications.
//! * [`notice`]: what the computer tells the agent in a conversation, with
//!   what came from elsewhere as data.
//! * [`memory`]: what the agent remembers about the user.
//! * [`config`]: the agent's settings.
//! * [`policy`]: which actions need the user's approval, and the actions
//!   the user always allows.
//! * [`wake`]: recognising the agent's name in what it hears, and the
//!   budget of recognition.
//! * [`gate`]: keeping the agent from hearing its own voice.
//! * [`presence`]: what in a conversation tells the window system that the
//!   user is there, so that the screen does not lock while they talk.

#![no_std]

extern crate alloc;

pub mod config;
pub mod deepgram;
pub mod files;
pub mod gate;
pub mod memory;
pub mod notice;
pub mod policy;
pub mod presence;
pub mod prompt;
pub mod tools;
pub mod wake;

#[cfg(test)]
extern crate std;

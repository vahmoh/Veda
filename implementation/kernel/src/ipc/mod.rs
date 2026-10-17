// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Inter-process communication: the objects processes talk through.
//!
//! * [`channel`]: messages, with handles that move with them;
//! * [`socket`]: byte streams, with handles that go with the bytes;
//! * [`event`]: a bare signal word, for notification;
//! * [`port`]: where the signals of many objects are waited for at once.
//!
//! A handle queued in a channel or a socket keeps its object alive, so no
//! channel or socket may queue one through which it would reach itself
//! ([`would_cycle`]).

pub mod channel;
pub mod event;
pub mod port;
pub mod socket;

use alloc::vec::Vec;

use crate::object::KObject;
use crate::object::handle::Handle;

/// How many queued handles [`would_cycle`] looks at before it gives up,
/// refusing the transfer: the safe answer.
const CYCLE_SEARCH_LIMIT: usize = 4096;

/// Whether queuing `handles` in the channel or socket whose queues are
/// `target` (a message on a channel, handles with a socket's bytes) would
/// let it reach itself through what is queued: one of the handles is an
/// endpoint of it, or of another channel or socket whose queues hold
/// (directly, or through yet others' queues) an endpoint of it. Once every
/// process had closed its handles to them, such objects would keep each
/// other alive for ever, with nothing left to read what is queued. A
/// search that would look at more than [`CYCLE_SEARCH_LIMIT`] queued
/// handles answers yes.
pub fn would_cycle(target: *const (), handles: &[Handle]) -> bool {
    let mut seen: Vec<*const ()> = Vec::new();
    let mut pending: Vec<KObject> = Vec::new();
    // Notes an object to search; true if it is the target.
    let mut note = |object: &KObject, pending: &mut Vec<KObject>| -> bool {
        let Some(queues) = object.queues() else { return false };
        if queues == target {
            return true;
        }
        if !seen.contains(&queues) {
            seen.push(queues);
            pending.push(object.clone());
        }
        false
    };
    for h in handles {
        if note(&h.object, &mut pending) {
            return true;
        }
    }
    let mut looked = 0;
    while let Some(object) = pending.pop() {
        for queued in object.queued() {
            looked += 1;
            if looked > CYCLE_SEARCH_LIMIT || note(&queued, &mut pending) {
                return true;
            }
        }
    }
    false
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Channels: bidirectional, message-oriented IPC with handle transfer.
//!
//! A channel has two endpoints. Writing to one endpoint queues a message (up
//! to 64 KiB of data plus up to 64 handles) on the other. Messages are
//! copied into the kernel on write and out on read. Handles move: they leave
//! the sender's table on write and enter the receiver's table on read.
//!
//! A queued message keeps the objects its handles refer to alive, channels
//! too. No message may hold a channel that can reach, through queued
//! messages, the channel the message is queued on ([`would_cycle`]): once
//! every process had closed its handles to them, such channels would keep
//! each other alive for ever, with nothing left to read the messages.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use vabi::Error;
use vabi::signals::{PEER_CLOSED, READABLE, WRITABLE};

use super::handle::Handle;
use super::{KObject, Signals};
use crate::sync::SpinLock;

/// Messages queued per endpoint before writers get `ShouldWait`.
pub const MAX_QUEUED_MESSAGES: usize = 512;
/// Bytes queued per endpoint before writers get `ShouldWait`.
pub const MAX_QUEUED_BYTES: usize = 4 << 20;
/// How many queued handles [`would_cycle`] looks at before it gives up,
/// refusing the transfer: the safe answer.
const CYCLE_SEARCH_LIMIT: usize = 4096;

pub struct Message {
    pub data: Vec<u8>,
    pub handles: Vec<Handle>,
}

struct EndState {
    queue: VecDeque<Message>,
    bytes: usize,
    closed: bool,
}

struct Pair {
    ends: SpinLock<[EndState; 2]>,
    signals: [Signals; 2],
    koids: [u64; 2],
}

/// One endpoint of a channel.
pub struct ChannelEnd {
    pair: Arc<Pair>,
    side: usize,
}

/// Creates a connected pair of endpoints.
pub fn create() -> (Arc<ChannelEnd>, Arc<ChannelEnd>) {
    let empty = || EndState { queue: VecDeque::new(), bytes: 0, closed: false };
    let pair = Arc::new(Pair {
        ends: SpinLock::new([empty(), empty()]),
        signals: [Signals::new(WRITABLE), Signals::new(WRITABLE)],
        koids: [super::new_koid(), super::new_koid()],
    });
    (Arc::new(ChannelEnd { pair: pair.clone(), side: 0 }), Arc::new(ChannelEnd { pair, side: 1 }))
}

/// Error from [`ChannelEnd::read`], with the sizes needed when the buffers
/// were too small.
pub struct ReadError {
    pub error: Error,
    pub bytes: usize,
    pub handles: usize,
}

impl ChannelEnd {
    pub fn koid(&self) -> u64 {
        self.pair.koids[self.side]
    }

    pub fn signals(&self) -> &Signals {
        &self.pair.signals[self.side]
    }

    /// Whether `other` is this endpoint or its peer.
    pub fn same_channel(&self, other: &ChannelEnd) -> bool {
        Arc::ptr_eq(&self.pair, &other.pair)
    }

    /// Queues a message for the peer. On failure the message is handed back
    /// so the caller can restore any handles it carried.
    pub fn write(&self, msg: Message) -> Result<(), (Error, Message)> {
        let peer = 1 - self.side;
        let now_full;
        {
            let mut ends = self.pair.ends.lock();
            let pe = &mut ends[peer];
            if pe.closed {
                return Err((Error::PeerClosed, msg));
            }
            if pe.queue.len() >= MAX_QUEUED_MESSAGES || pe.bytes + msg.data.len() > MAX_QUEUED_BYTES {
                return Err((Error::ShouldWait, msg));
            }
            pe.bytes += msg.data.len();
            pe.queue.push_back(msg);
            now_full = pe.queue.len() >= MAX_QUEUED_MESSAGES || pe.bytes >= MAX_QUEUED_BYTES;
        }
        self.pair.signals[peer].update(0, READABLE);
        if now_full {
            self.pair.signals[self.side].update(WRITABLE, 0);
        }
        Ok(())
    }

    /// Dequeues the next message if it fits the given capacities.
    pub fn read(&self, max_bytes: usize, max_handles: usize) -> Result<Message, ReadError> {
        let peer = 1 - self.side;
        let (msg, now_empty) = {
            let mut ends = self.pair.ends.lock();
            let peer_closed = ends[peer].closed;
            let me = &mut ends[self.side];
            let Some(front) = me.queue.front() else {
                let error = if peer_closed { Error::PeerClosed } else { Error::ShouldWait };
                return Err(ReadError { error, bytes: 0, handles: 0 });
            };
            if front.data.len() > max_bytes || front.handles.len() > max_handles {
                return Err(ReadError {
                    error: Error::BufferTooSmall,
                    bytes: front.data.len(),
                    handles: front.handles.len(),
                });
            }
            let msg = me.queue.pop_front().unwrap();
            me.bytes -= msg.data.len();
            (msg, me.queue.is_empty())
        };
        if now_empty {
            self.pair.signals[self.side].update(READABLE, 0);
        }
        self.pair.signals[peer].update(0, WRITABLE);
        Ok(msg)
    }
}

/// Whether queuing a message with `handles` on the channel of `dest` would
/// let that channel reach itself through queued messages: one of the
/// handles is an endpoint of it, or of another channel whose queued
/// messages hold (directly, or through yet other channels' queues) an
/// endpoint of it. A search that would look at more than
/// [`CYCLE_SEARCH_LIMIT`] queued handles answers yes.
pub fn would_cycle(dest: &ChannelEnd, handles: &[Handle]) -> bool {
    /// Notes the channel of `end` to search; true if it is `target`.
    fn note(end: &ChannelEnd, target: *const Pair, seen: &mut Vec<*const Pair>, pending: &mut Vec<Arc<Pair>>) -> bool {
        let pair = Arc::as_ptr(&end.pair);
        if pair == target {
            return true;
        }
        if !seen.contains(&pair) {
            seen.push(pair);
            pending.push(end.pair.clone());
        }
        false
    }
    let target = Arc::as_ptr(&dest.pair);
    let mut seen: Vec<*const Pair> = Vec::new();
    let mut pending: Vec<Arc<Pair>> = Vec::new();
    for h in handles {
        if let KObject::Channel(end) = &h.object
            && note(end, target, &mut seen, &mut pending)
        {
            return true;
        }
    }
    let mut looked = 0;
    while let Some(pair) = pending.pop() {
        let ends = pair.ends.lock();
        for msg in ends.iter().flat_map(|e| e.queue.iter()) {
            looked += msg.handles.len();
            if looked > CYCLE_SEARCH_LIMIT {
                return true;
            }
            for h in &msg.handles {
                if let KObject::Channel(end) = &h.object
                    && note(end, target, &mut seen, &mut pending)
                {
                    return true;
                }
            }
        }
    }
    false
}

impl Drop for ChannelEnd {
    fn drop(&mut self) {
        let peer = 1 - self.side;
        let orphaned = {
            let mut ends = self.pair.ends.lock();
            ends[self.side].closed = true;
            ends[self.side].bytes = 0;
            core::mem::take(&mut ends[self.side].queue)
        };
        // Dropping queued messages may close further handles (even other
        // channels); do it without holding our lock.
        drop(orphaned);
        self.pair.signals[peer].update(WRITABLE, PEER_CLOSED);
    }
}

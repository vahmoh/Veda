// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Packets read ahead of their decoder, one queue a stream.
//!
//! Containers interleave video and sound, but the two are consumed at their
//! own pace: while the frame pool is full (the player is paused, or shows
//! pictures slowly) sound must still be read, and the reverse. Each stream's
//! packets wait in a queue of bounded size; when one is full, reading stops
//! until its decoder takes some.

use alloc::collections::VecDeque;

/// The most bytes of compressed video read ahead (a few seconds of the
/// highest rates).
pub const VIDEO_BYTES: usize = 64 << 20;
/// The same for sound.
pub const AUDIO_BYTES: usize = 4 << 20;
/// The most packets either queue holds.
pub const MAX_PACKETS: usize = 4096;

/// A stream's packets, oldest first. `P` is the packet (an owned pointer to
/// the shim's in the decoder; anything in tests).
pub struct PacketQueue<P> {
    packets: VecDeque<(P, usize)>,
    bytes: usize,
    limit: usize,
}

impl<P> PacketQueue<P> {
    /// A queue holding at most `limit` bytes (and [`MAX_PACKETS`]).
    pub fn new(limit: usize) -> PacketQueue<P> {
        PacketQueue { packets: VecDeque::new(), bytes: 0, limit }
    }

    pub fn push(&mut self, packet: P, bytes: usize) {
        self.bytes += bytes;
        self.packets.push_back((packet, bytes));
    }

    /// The oldest packet.
    pub fn front(&self) -> Option<&P> {
        self.packets.front().map(|(p, _)| p)
    }

    pub fn pop(&mut self) -> Option<P> {
        let (p, bytes) = self.packets.pop_front()?;
        self.bytes -= bytes;
        Some(p)
    }

    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    pub fn len(&self) -> usize {
        self.packets.len()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// No more may be read ahead for it.
    pub fn is_full(&self) -> bool {
        self.bytes >= self.limit || self.packets.len() >= MAX_PACKETS
    }

    /// Removes every packet, handing each to `free`.
    pub fn clear(&mut self, mut free: impl FnMut(P)) {
        while let Some(p) = self.pop() {
            free(p);
        }
    }
}

/// Whether to read another packet: some stream's decoder waits for input
/// that its queue does not have, and no queue is full (a full one waits for
/// its own decoder, which is held up by something else).
pub fn should_read(wanting: &[bool], empty: &[bool], full: &[bool]) -> bool {
    let waits = wanting.iter().zip(empty).any(|(w, e)| *w && *e);
    waits && !full.iter().any(|f| *f)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn queues_count_and_bound_what_they_hold() {
        let mut q = PacketQueue::new(100);
        q.push(1, 60);
        assert!(!q.is_full());
        q.push(2, 60);
        assert!(q.is_full() && q.bytes() == 120 && q.len() == 2);
        assert_eq!(q.front(), Some(&1));
        assert_eq!(q.pop(), Some(1));
        assert!(!q.is_full() && q.bytes() == 60);
        let mut freed = Vec::new();
        q.push(3, 1);
        q.clear(|p| freed.push(p));
        assert_eq!(freed, [2, 3]);
        assert!(q.is_empty() && q.bytes() == 0);
        let mut many = PacketQueue::new(usize::MAX);
        for i in 0..MAX_PACKETS {
            many.push(i, 0);
        }
        assert!(many.is_full(), "too many small packets");
    }

    #[test]
    fn reading_waits_for_a_full_queue() {
        // Video waits for input and has none: read.
        assert!(should_read(&[true, false], &[true, false], &[false, false]));
        // Nobody waits.
        assert!(!should_read(&[false, false], &[true, true], &[false, false]));
        // Sound waits, but the video queue is full: its decoder must take
        // some first.
        assert!(!should_read(&[false, true], &[false, true], &[true, false]));
        // A decoder that waits with packets queued is fed, not read for.
        assert!(!should_read(&[true, false], &[false, true], &[false, false]));
    }
}

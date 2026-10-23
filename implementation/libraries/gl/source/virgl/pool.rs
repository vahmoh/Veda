// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Buffers kept for reuse.
//!
//! Making a resource and destroying one each take a call to the renderer
//! (and destroying one, the submission of the commands that may name it),
//! which a buffer re-specified every frame, as streamed vertices are, would
//! make many times a frame. So the buffers the front end lets go of are
//! kept, and new buffers are made of them: once the GPU is done with them
//! (the fence after the last commands that could use them has signaled),
//! so that no draw still to run reads the new contents. Host buffers come
//! in sizes a little larger than asked for, so that buffers of sizes that
//! vary from frame to frame find one.

use alloc::vec::Vec;

/// The most buffers, and bytes of them, kept.
const KEPT: usize = 256;
const KEPT_BYTES: u64 = 32 << 20;
/// Buffers larger than this are not kept: they are made rarely.
const LARGEST: u32 = 4 << 20;

/// A buffer kept for reuse.
struct Kept {
    id: u32,
    /// Its bytes on the host.
    capacity: u32,
    /// The fence after the last commands that could use it, once one has
    /// been made.
    fence: Option<u64>,
}

/// The buffers kept.
#[derive(Default)]
pub(super) struct Pool {
    kept: Vec<Kept>,
    bytes: u64,
}

impl Pool {
    /// The bytes on the host of a buffer `size` bytes large: rounded up to
    /// an eighth of the power of two above it (a quarter more at most), so
    /// that sizes that vary a little share a size.
    pub fn capacity(size: u32) -> u32 {
        let size = size.max(1);
        if size > LARGEST {
            return size;
        }
        let step = (size.next_power_of_two() / 8).max(64);
        size.next_multiple_of(step)
    }

    /// A kept buffer `size` bytes can be made of, whose fence has signaled
    /// (`signaled`): the smallest at most twice as large. Returns its
    /// handle and capacity.
    pub fn take(&mut self, size: u32, mut signaled: impl FnMut(u64) -> bool) -> Option<(u32, u32)> {
        let fits = |k: &Kept| k.capacity >= size && u64::from(k.capacity) <= 2 * u64::from(size.max(1));
        let (i, _) = self
            .kept
            .iter()
            .enumerate()
            .filter(|(_, k)| fits(k))
            .filter(|(_, k)| k.fence.is_some_and(&mut signaled))
            .min_by_key(|(_, k)| k.capacity)?;
        let k = self.kept.remove(i);
        self.bytes -= u64::from(k.capacity);
        Some((k.id, k.capacity))
    }

    /// Keeps a buffer the front end let go of, if it is worth keeping.
    /// Returns the buffers no longer kept, to destroy: the oldest, to make
    /// room, or this one, too large to keep.
    pub fn keep(&mut self, id: u32, capacity: u32) -> Vec<u32> {
        if capacity > LARGEST {
            return alloc::vec![id];
        }
        let mut gone = Vec::new();
        while !self.kept.is_empty() && (self.kept.len() >= KEPT || self.bytes + u64::from(capacity) > KEPT_BYTES) {
            let k = self.kept.remove(0);
            self.bytes -= u64::from(k.capacity);
            gone.push(k.id);
        }
        self.kept.push(Kept { id, capacity, fence: None });
        self.bytes += u64::from(capacity);
        gone
    }

    /// Whether kept buffers wait for a fence.
    pub fn unfenced(&self) -> bool {
        self.kept.iter().any(|k| k.fence.is_none())
    }

    /// `fence` follows every command submitted so far: the buffers kept
    /// without one get it.
    pub fn fenced(&mut self, fence: u64) {
        for k in self.kept.iter_mut().filter(|k| k.fence.is_none()) {
            k.fence = Some(fence);
        }
    }

    /// How many buffers it keeps.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.kept.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_round_up_by_a_quarter_at_most() {
        assert_eq!(Pool::capacity(1), 64);
        assert_eq!(Pool::capacity(1000), 1024);
        assert_eq!(Pool::capacity(1025), 1280);
        assert_eq!(Pool::capacity(5000), 5120);
        for size in [3u32, 77, 4096, 4097, 100_000, LARGEST] {
            let c = Pool::capacity(size);
            assert!(c >= size && c - size < (size / 4).max(64), "{size}: {c}");
        }
        assert_eq!(Pool::capacity(LARGEST + 1), LARGEST + 1);
    }

    #[test]
    fn buffers_are_reused_once_their_fence_has_signaled() {
        let mut p = Pool::default();
        assert!(p.keep(1, 1024).is_empty());
        // No fence yet: not to be reused.
        assert_eq!(p.take(1000, |_| true), None);
        assert!(p.unfenced());
        p.fenced(5);
        assert!(!p.unfenced());
        assert_eq!(p.take(1000, |f| f < 5), None);
        // Too small, or more than twice as large: not taken.
        assert_eq!(p.take(1025, |_| true), None);
        assert_eq!(p.take(511, |_| true), None);
        assert_eq!(p.take(1000, |f| f <= 5), Some((1, 1024)));
        assert_eq!(p.len(), 0);
    }

    #[test]
    fn the_oldest_go_to_make_room() {
        let mut p = Pool::default();
        for id in 0..KEPT as u32 {
            assert!(p.keep(id, 64).is_empty());
        }
        assert_eq!(p.keep(1000, 64), [0]);
        assert_eq!(p.len(), KEPT);
        // Too large to keep at all.
        assert_eq!(p.keep(2000, LARGEST + 64), [2000]);
        // Room by bytes: eight of the largest fill it, a ninth pushes out
        // the oldest.
        let mut p = Pool::default();
        for id in 0..8 {
            assert!(p.keep(id, LARGEST).is_empty());
        }
        assert_eq!(p.keep(8, LARGEST), [0]);
        assert_eq!(p.keep(9, 64), [1]);
    }
}

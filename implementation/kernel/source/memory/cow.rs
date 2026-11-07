// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Copy-on-write: frames more than one VMO holds.
//!
//! A process `process_fork` makes has its parent's memory as it was: each
//! page of the memory that is the parent's own (its private memory, and
//! VMOs nothing else reaches) is one frame that both hold, which neither
//! writes in place: the first to write a page gets a copy of its own
//! (`Vmo::page_for_write`), and the frame stays the other's. Forked again,
//! a frame is held by more VMOs still. So the kernel counts, for each
//! frame, the VMOs beyond the first that hold it ([`share`]), and frees it
//! as the last lets it go ([`release`]).
//!
//! The counts are kept a frame's worth at a time: a frame of counts (a
//! *chunk*) for each 1024 frames of memory, made as the first of them is
//! shared and freed as the last is no longer, found through a table that
//! has a word for each chunk, made as the kernel starts (8 bytes for each
//! 4 MiB of memory). Memory that no two VMOs share costs nothing more, and
//! all of it shared costs 1/1024 of it. A chunk is memory taken on user
//! space's behalf: none is made within the kernel's reserve
//! (`phys::alloc_user_zeroed`), and then nothing is shared.

use super::{PAGE_SIZE, phys, phys_to_virt};
use crate::synchronization::SpinLock;

/// The counts a chunk holds: one 32-bit word for each frame.
const PER_CHUNK: u64 = PAGE_SIZE / 4;

/// The table of chunks: for each, the frame of its counts (0: none), and in
/// the bits below it (a frame is page-aligned) how many of its counts are
/// not 0, which is at most [`PER_CHUNK`].
struct Table {
    chunks: &'static mut [u64],
}

static TABLE: SpinLock<Option<Table>> = SpinLock::new(None);

/// The bits of a chunk's word that count its counts that are not 0.
const USED: u64 = PAGE_SIZE - 1;

/// Makes the table of chunks for the frames the allocator has
/// (`phys::frame_count`), all of them unshared.
pub fn init() {
    let chunks = phys::frame_count().div_ceil(PER_CHUNK);
    let pages = (chunks * 8).div_ceil(PAGE_SIZE).max(1);
    let base = phys::alloc_contiguous(pages, PAGE_SIZE, u64::MAX).expect("no memory for the copy-on-write table");
    // SAFETY: freshly allocated, zeroed, direct-mapped memory that nothing
    // else reaches, as long as the kernel runs.
    let chunks = unsafe { core::slice::from_raw_parts_mut(phys_to_virt(base) as *mut u64, chunks as usize) };
    *TABLE.lock() = Some(Table { chunks });
}

impl Table {
    /// Where `frame`'s count is: its chunk's index, and its place there.
    fn place(frame: u64) -> (usize, usize) {
        let n = frame / PAGE_SIZE;
        ((n / PER_CHUNK) as usize, (n % PER_CHUNK) as usize)
    }

    /// The counts of chunk `word`, if it has them.
    fn counts(word: u64) -> Option<&'static mut [u32; PER_CHUNK as usize]> {
        let frame = word & !USED;
        // SAFETY: a chunk's frame, which only the table reaches, under its
        // lock.
        (frame != 0).then(|| unsafe { &mut *(phys_to_virt(frame) as *mut [u32; PER_CHUNK as usize]) })
    }

    /// The VMOs beyond the first that hold `frame`.
    fn count(&self, frame: u64) -> u32 {
        let (chunk, at) = Self::place(frame);
        self.chunks.get(chunk).and_then(|&w| Self::counts(w)).map_or(0, |c| c[at])
    }
}

/// Counts one more VMO that holds `frame`, a frame of user memory: false,
/// and nothing counted, if there is no memory for its count, or if it is
/// held as often as can be counted (no process has that many children).
pub fn share(frame: u64) -> bool {
    let mut guard = TABLE.lock();
    let Some(table) = guard.as_mut() else { return false };
    let (chunk, at) = Table::place(frame);
    let Some(word) = table.chunks.get_mut(chunk) else { return false };
    if *word == 0 {
        match phys::alloc_user_zeroed() {
            Some(f) => *word = f,
            None => return false,
        }
    }
    let counts = Table::counts(*word).expect("a chunk has its counts");
    if counts[at] == u32::MAX {
        return false;
    }
    if counts[at] == 0 {
        *word += 1;
    }
    counts[at] += 1;
    true
}

/// Whether a VMO other than the one asking holds `frame` too: then neither
/// may write it in place.
pub fn is_shared(frame: u64) -> bool {
    TABLE.lock().as_ref().is_some_and(|t| t.count(frame) > 0)
}

/// A VMO lets `frame` go: freed if no other holds it, and otherwise one
/// fewer holds it (its chunk freed with its last count).
pub fn release(frame: u64) {
    let free_chunk = {
        let mut guard = TABLE.lock();
        let shared = guard.as_mut().and_then(|table| {
            let (chunk, at) = Table::place(frame);
            let word = table.chunks.get_mut(chunk)?;
            let counts = Table::counts(*word)?;
            if counts[at] == 0 {
                return None;
            }
            counts[at] -= 1;
            if counts[at] > 0 {
                return Some(None);
            }
            *word -= 1;
            if *word & USED != 0 {
                return Some(None);
            }
            let chunk_frame = *word;
            *word = 0;
            Some(Some(chunk_frame))
        });
        match shared {
            // Another VMO holds it still: its chunk, if that is no longer
            // needed, goes once the lock is let go.
            Some(chunk) => chunk,
            None => {
                drop(guard);
                phys::free(frame);
                return;
            }
        }
    };
    if let Some(chunk) = free_chunk {
        phys::free(chunk);
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `vframes` — searches of the kernel's bitmap of physical memory frames,
//! which has a bit for each 4 KiB frame, set while the frame is in use:
//! frame `f` is bit `f % 64` of word `f / 64`.
//!
//! The searches look at a word at a time, so 64 frames in use cost one test.
//! The kernel looks for runs of free frames from the bottom of memory (DMA
//! buffers, and its heap's large allocations: among them the buffer of every
//! system call that carries more than 2 KiB), and what is in use below the
//! first run can be most of memory. Looked at a frame at a time, as they
//! were, hundreds of MiB in use made each search take most of a
//! millisecond, with the kernel's lock held.

#![no_std]

#[cfg(test)]
extern crate std;

/// The first free frame of `from..frames`.
pub fn next_free(words: &[u64], from: u64, frames: u64) -> Option<u64> {
    let mut f = from;
    while f < frames {
        let w = (f / 64) as usize;
        // The word's frames below `f` count as in use.
        let used = words[w] | ((1u64 << (f % 64)) - 1);
        if used != u64::MAX {
            let free = w as u64 * 64 + (!used).trailing_zeros() as u64;
            return (free < frames).then_some(free);
        }
        f = (w as u64 + 1) * 64;
    }
    None
}

/// The first frame in use of `from..to`.
pub fn first_used(words: &[u64], from: u64, to: u64) -> Option<u64> {
    let mut f = from;
    while f < to {
        let w = (f / 64) as usize;
        let end = (w as u64 + 1) * 64;
        // The word's frames from `f` on, and before `to`.
        let mut used = words[w] & !((1u64 << (f % 64)) - 1);
        if to < end {
            used &= (1u64 << (to % 64)) - 1;
        }
        if used != 0 {
            return Some(w as u64 * 64 + used.trailing_zeros() as u64);
        }
        f = end;
    }
    None
}

/// The lowest run of `count` free frames (at least one) that starts at a
/// multiple of `align` and ends by frame `limit`: its first frame.
pub fn find_run(words: &[u64], count: u64, align: u64, limit: u64) -> Option<u64> {
    let align = align.max(1);
    let mut start = 0;
    loop {
        start = next_free(words, start, limit)?.next_multiple_of(align);
        let end = start.checked_add(count)?;
        if end > limit {
            return None;
        }
        match first_used(words, start, end) {
            // No run starting before the frame in use holds it.
            Some(used) => start = used + 1,
            None => return Some(start),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::vec;
    use std::vec::Vec;

    use super::*;

    fn is_used(words: &[u64], f: u64) -> bool {
        words[(f / 64) as usize] & (1 << (f % 64)) != 0
    }

    /// The search as the kernel made it before: a frame at a time.
    fn find_run_slowly(words: &[u64], count: u64, align: u64, limit: u64) -> Option<u64> {
        let align = align.max(1);
        let mut start = 0u64;
        while start + count <= limit {
            if !start.is_multiple_of(align) {
                start = start.next_multiple_of(align);
                continue;
            }
            match (start..start + count).find(|&f| is_used(words, f)) {
                Some(used) => start = (used + 1).next_multiple_of(align),
                None => return Some(start),
            }
        }
        None
    }

    /// A bitmap of `frames` frames, each in use with the chance `percent`
    /// (xorshift from `seed`).
    fn random(frames: u64, percent: u64, seed: u64) -> Vec<u64> {
        let mut x = seed | 1;
        let mut words = vec![0u64; frames.div_ceil(64) as usize];
        for f in 0..frames {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            if x % 100 < percent {
                words[(f / 64) as usize] |= 1 << (f % 64);
            }
        }
        words
    }

    #[test]
    fn single_frames_are_found_across_words() {
        let mut words = vec![u64::MAX; 4];
        words[2] = !(1 << 5);
        assert_eq!(next_free(&words, 0, 256), Some(133));
        assert_eq!(next_free(&words, 133, 256), Some(133));
        assert_eq!(next_free(&words, 134, 256), None);
        // Not past the end of memory, whatever the bitmap's last word says.
        words[3] = 0;
        assert_eq!(next_free(&words, 134, 200), Some(192));
        assert_eq!(next_free(&words, 134, 192), None);
        let mut words = vec![0u64; 4];
        words[1] = 1 << 63;
        words[3] = 1;
        assert_eq!(first_used(&words, 0, 256), Some(127));
        assert_eq!(first_used(&words, 0, 127), None);
        assert_eq!(first_used(&words, 128, 192), None);
        assert_eq!(first_used(&words, 128, 193), Some(192));
        assert_eq!(first_used(&words, 127, 128), Some(127));
    }

    #[test]
    fn runs_are_where_the_search_a_frame_at_a_time_found_them() {
        for (percent, seed) in [(0, 1), (10, 2), (50, 3), (90, 4), (97, 5), (100, 6)] {
            let frames = 4096 + 37;
            let words = random(frames, percent, seed);
            for count in [1, 2, 3, 16, 63, 64, 65, 200] {
                for align in [1, 2, 16, 64, 512] {
                    for limit in [frames, frames - 1, 2048, 64, 0] {
                        assert_eq!(
                            find_run(&words, count, align, limit),
                            find_run_slowly(&words, count, align, limit),
                            "{percent}% in use, {count} frames aligned to {align} below {limit}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_run_past_most_of_memory_in_use() {
        // 1 GiB of frames, all in use but for the last 1 MiB, and a free
        // frame now and then before it.
        let frames = 1 << 18;
        let mut words = vec![u64::MAX; (frames / 64) as usize];
        for f in (1000..frames - 256).step_by(4099) {
            words[(f / 64) as usize] &= !(1 << (f % 64));
        }
        for f in frames - 256..frames {
            words[(f / 64) as usize] &= !(1 << (f % 64));
        }
        assert_eq!(find_run(&words, 16, 1, frames), Some(frames - 256));
        assert_eq!(find_run(&words, 1, 1, frames), Some(1000));
        assert_eq!(find_run(&words, 257, 1, frames), None);
        assert_eq!(find_run(&words, 16, 1, frames - 256), None);
    }
}

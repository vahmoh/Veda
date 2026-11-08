// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `slabs` — the kernel heap's small allocations, of 2 KiB at most: blocks
//! of eight sizes, each size carved out of pages of its own, its *slabs*.
//!
//! A slab threads its free blocks into a list, each holding the offset of
//! the next; a size class threads its slabs that have blocks both free and
//! in use into a list, and takes blocks from the first. A slab that gets a
//! block back when it was full goes first, so that blocks freed are the
//! first given again. A slab whose blocks are all free leaves the list and
//! goes back to where pages come from ([`Pages`]), but for one of each
//! class, kept for that class's next blocks, so that a class whose blocks
//! come and go around a page's worth does not take a page and give it back
//! each time. A burst of allocations (the page maps of a hundred processes
//! `fork` made, say) thus leaves its memory free once it is over, not the
//! heap's for good, as when free blocks stayed in lists of their size,
//! whichever page they were in.
//!
//! What the heap knows of a slab ([`Entry`]: its free blocks, how many are
//! in use, its neighbours in its class's list) is kept apart from it, so
//! that a slab is all blocks: two of 2 KiB in a page, rather than one beside
//! a header. The entries are in a table with one for each page of memory, a
//! page of them for each 256 pages, made as the first of those becomes a
//! slab and given back with the last; a word for each 256 pages (8 bytes for
//! each MiB of memory) finds them.
//!
//! An allocation or a free takes a constant time: it touches its block, its
//! slab's entry and at most two other slabs' entries; a new slab threads its
//! blocks (writes all over its page), and one given back gives back its
//! page, and its page of entries with the last of them.
//!
//! The slabs are not thread-safe by themselves (wrap them in a lock), and
//! [`Slabs::check`] verifies what they keep, for tests.

#![no_std]

#[cfg(test)]
extern crate std;

use core::ptr::NonNull;

/// The size of a page, and of a slab.
pub const PAGE: usize = 4096;

/// The sizes of the classes' blocks: powers of two, so that a block, at a
/// multiple of its size in its page, is aligned to it.
pub const CLASSES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];

/// No slab: pages' numbers are below it.
const NONE: u32 = u32::MAX;

/// No block: the end of a slab's list of free blocks.
const END: u16 = u16::MAX;

/// The entries in a page of them.
const PER_PAGE: usize = PAGE / size_of::<Entry>();

/// The bits of a word of the table that count the slabs among its pages
/// (below the address of their page of entries, which is aligned).
const SLABS: usize = PAGE - 1;

/// Where the pages of slabs, and of their entries, come from.
pub trait Pages {
    /// A page, aligned to its size, or `None` if there is none.
    fn take(&mut self) -> Option<NonNull<u8>>;

    /// Gives back `page`, which [`Pages::take`] gave.
    fn give(&mut self, page: NonNull<u8>);
}

/// The class of an allocation of `size` bytes aligned to `align` (a power
/// of two): the first whose blocks hold it, aligned to their size; `None`
/// past 2 KiB, which slabs do not serve.
#[inline]
pub fn class(size: usize, align: usize) -> Option<usize> {
    let size = size.max(align).max(CLASSES[0]);
    let class = (usize::BITS - (size - 1).leading_zeros() - CLASSES[0].trailing_zeros()) as usize;
    (class < CLASSES.len()).then_some(class)
}

/// What the heap knows of a slab.
#[derive(Clone, Copy)]
#[repr(C)]
struct Entry {
    /// Where its first free block is in its page ([`END`]: none).
    free: u16,
    /// How many of its blocks are in use.
    used: u16,
    /// Its class.
    class: u32,
    /// The numbers of the slabs before and after it in its class's list
    /// ([`NONE`]: none).
    prev: u32,
    next: u32,
}

/// A size class.
#[derive(Clone, Copy)]
struct Class {
    /// The first of its slabs with blocks both free and in use, which gives
    /// the next block ([`NONE`]: none).
    first: u32,
    /// A slab of it whose blocks are all free, which gives blocks once no
    /// other has them ([`NONE`]: none).
    spare: u32,
}

/// The slabs among the pages from an address on.
pub struct Slabs<'a> {
    /// The address of page 0: page `n` is at `base + n * PAGE`.
    base: usize,
    /// For each 256 pages, the address of their page of entries (0: none),
    /// with how many of them are slabs in the bits below ([`SLABS`]).
    table: &'a mut [usize],
    /// The number of the first page the table does not cover.
    limit: usize,
    classes: [Class; CLASSES.len()],
    /// Pages that are slabs.
    slabs: usize,
    /// Pages of entries.
    entries: usize,
}

impl<'a> Slabs<'a> {
    /// The words that the table of [`Slabs::new`] needs for `pages` pages.
    pub const fn table_len(pages: usize) -> usize {
        pages.div_ceil(PER_PAGE)
    }

    /// Slabs among the pages from `base` (aligned to a page) on, as many as
    /// `table`, all zeroes, covers ([`Slabs::table_len`]).
    pub fn new(base: usize, table: &'a mut [usize]) -> Slabs<'a> {
        assert!(base.is_multiple_of(PAGE), "slabs' pages are aligned");
        debug_assert!(table.iter().all(|&w| w == 0), "a new table is all zeroes");
        let limit = (table.len() * PER_PAGE).min(NONE as usize);
        let none = Class { first: NONE, spare: NONE };
        Slabs { base, table, limit, classes: [none; CLASSES.len()], slabs: 0, entries: 0 }
    }

    /// How many pages the slabs hold: the slabs, and their entries' pages.
    pub fn pages(&self) -> usize {
        self.slabs + self.entries
    }

    /// A block of class `class` (an index of [`CLASSES`]), or `None` if it
    /// needed a new slab and `pages` had no page for it.
    #[inline]
    pub fn alloc(&mut self, class: usize, pages: &mut impl Pages) -> Option<NonNull<u8>> {
        let mut s = self.classes[class].first;
        if s == NONE {
            s = self.refill(class, pages)?;
        }
        let e = self.entry(s);
        // SAFETY: the entry of a slab in its class's list, so with a free
        // block, which holds where the next one is.
        unsafe {
            let block = self.page(s) + usize::from((*e).free);
            (*e).free = *(block as *const u16);
            (*e).used += 1;
            if (*e).free == END {
                // Full: it has nothing more to give.
                self.unlink(class, s);
            }
            Some(NonNull::new_unchecked(block as *mut u8))
        }
    }

    /// Gives back `block`, which [`Slabs::alloc`] gave for `class`.
    ///
    /// # Safety
    ///
    /// `block` is a block of class `class` in use, and nothing reaches it
    /// any more.
    #[inline]
    pub unsafe fn free(&mut self, block: NonNull<u8>, class: usize, pages: &mut impl Pages) {
        let at = block.as_ptr() as usize;
        let s = ((at - self.base) / PAGE) as u32;
        let e = self.entry(s);
        // SAFETY: the entry of the slab that gave `block`; the block is
        // free now, and holds where the next free one is.
        let (was_full, used) = unsafe {
            debug_assert!((*e).class as usize == class && (*e).used > 0, "a block freed twice, or as another size");
            let was_full = (*e).free == END;
            *(at as *mut u16) = (*e).free;
            (*e).free = (at % PAGE) as u16;
            (*e).used -= 1;
            (was_full, (*e).used)
        };
        if used == 0 {
            self.emptied(class, s, was_full, pages);
        } else if was_full {
            self.push(class, s);
        }
    }

    /// Gives class `class`, whose list is empty, a slab to take blocks from
    /// (its spare, or a new one), first in the list; `None` if there is no
    /// page for one.
    #[cold]
    #[inline(never)]
    fn refill(&mut self, class: usize, pages: &mut impl Pages) -> Option<u32> {
        let s = match self.classes[class].spare {
            NONE => self.make(class, pages)?,
            spare => {
                self.classes[class].spare = NONE;
                spare
            }
        };
        self.push(class, s);
        Some(s)
    }

    /// Slab `s` of class `class` has all its blocks free (`was_full`: and
    /// none a moment ago, so that it is in no list): it becomes its class's
    /// spare, or goes back if there is one.
    #[inline(never)]
    fn emptied(&mut self, class: usize, s: u32, was_full: bool, pages: &mut impl Pages) {
        if !was_full {
            self.unlink(class, s);
        }
        match self.classes[class].spare {
            NONE => self.classes[class].spare = s,
            _ => self.release(s, pages),
        }
    }

    /// A new slab of class `class`, its blocks all free, in no list; `None`
    /// if `pages` has no page for it, or for its entry.
    fn make(&mut self, class: usize, pages: &mut impl Pages) -> Option<u32> {
        let page = pages.take()?;
        let at = page.as_ptr() as usize;
        let n = at.wrapping_sub(self.base) / PAGE;
        if at < self.base || n >= self.limit {
            // Beyond the table, which covers the memory pages come from.
            pages.give(page);
            return None;
        }
        let word = n / PER_PAGE;
        if self.table[word] == 0 {
            let Some(entries) = pages.take() else {
                pages.give(page);
                return None;
            };
            self.table[word] = entries.as_ptr() as usize;
            self.entries += 1;
        }
        self.table[word] += 1;
        self.slabs += 1;
        // Each block holds where the next one is.
        let mut next = END;
        for offset in (0..PAGE).step_by(CLASSES[class]).rev() {
            // SAFETY: a block of the new slab, whose page is ours.
            unsafe { *((at + offset) as *mut u16) = next };
            next = offset as u16;
        }
        let s = n as u32;
        // SAFETY: the slab's entry, in its page of entries.
        unsafe { *self.entry(s) = Entry { free: 0, used: 0, class: class as u32, prev: NONE, next: NONE } };
        Some(s)
    }

    /// Gives back slab `s`, whose blocks are all free and which is in no
    /// list, and its page of entries if it was the last slab there.
    fn release(&mut self, s: u32, pages: &mut impl Pages) {
        let word = s as usize / PER_PAGE;
        self.table[word] -= 1;
        self.slabs -= 1;
        // SAFETY: the address of a page that `pages` gave.
        pages.give(unsafe { NonNull::new_unchecked(self.page(s) as *mut u8) });
        if self.table[word] & SLABS == 0 {
            let entries = core::mem::take(&mut self.table[word]);
            self.entries -= 1;
            // SAFETY: as above.
            pages.give(unsafe { NonNull::new_unchecked(entries as *mut u8) });
        }
    }

    /// Puts slab `s`, which is in no list, first in its class's.
    #[inline(never)]
    fn push(&mut self, class: usize, s: u32) {
        let first = core::mem::replace(&mut self.classes[class].first, s);
        // SAFETY: the entries of `s` and of the slab that was first.
        unsafe {
            let e = self.entry(s);
            (*e).prev = NONE;
            (*e).next = first;
            if first != NONE {
                (*self.entry(first)).prev = s;
            }
        }
    }

    /// Takes slab `s` out of its class's list.
    #[inline(never)]
    fn unlink(&mut self, class: usize, s: u32) {
        // SAFETY: the entries of `s` and of its neighbours, slabs too.
        unsafe {
            let e = self.entry(s);
            let (prev, next) = ((*e).prev, (*e).next);
            match prev {
                NONE => self.classes[class].first = next,
                prev => (*self.entry(prev)).next = next,
            }
            if next != NONE {
                (*self.entry(next)).prev = prev;
            }
        }
    }

    /// Where slab `s`'s entry is.
    #[inline]
    fn entry(&self, s: u32) -> *mut Entry {
        let s = s as usize;
        let entries = (self.table[s / PER_PAGE] & !SLABS) as *mut Entry;
        entries.wrapping_add(s % PER_PAGE)
    }

    /// The address of page `s`.
    #[inline]
    fn page(&self, s: u32) -> usize {
        self.base + s as usize * PAGE
    }

    /// Checks what the slabs keep: that each class's list has slabs of the
    /// class with blocks both free and in use, each linked to its
    /// neighbours, its spare none in use, each of those slabs as many free
    /// blocks as are not in use (in its page, at multiples of their size,
    /// each once), and the table its counts. For tests: it takes a time
    /// that grows with the blocks.
    pub fn check(&self) -> Result<(), &'static str> {
        let mut seen = 0;
        for (class, c) in self.classes.iter().enumerate() {
            let (mut prev, mut s) = (NONE, c.first);
            while s != NONE {
                seen += 1;
                if seen > self.slabs {
                    return Err("a class's list goes round");
                }
                let e = self.checked_entry(s, class)?;
                if e.prev != prev {
                    return Err("a slab's link back is wrong");
                }
                if e.used == 0 || e.free == END {
                    return Err("a listed slab has no block in use, or none free");
                }
                (prev, s) = (s, e.next);
            }
            if c.spare != NONE {
                seen += 1;
                if self.checked_entry(c.spare, class)?.used != 0 {
                    return Err("a spare slab has a block in use");
                }
            }
        }
        if seen > self.slabs {
            return Err("more slabs listed than there are");
        }
        let slabs: usize = self.table.iter().map(|w| w & SLABS).sum();
        let entries = self.table.iter().filter(|&&w| w != 0).count();
        if slabs != self.slabs || entries != self.entries {
            return Err("the table's counts are wrong");
        }
        Ok(())
    }

    /// Slab `s`'s entry, checked to be of class `class`, and its free
    /// blocks to be as many as are not in use.
    fn checked_entry(&self, s: u32, class: usize) -> Result<Entry, &'static str> {
        if s as usize >= self.limit || self.table[s as usize / PER_PAGE] & SLABS == 0 {
            return Err("a slab's number is not a slab's");
        }
        // SAFETY: an entry of a page of entries, which a slab has.
        let e = unsafe { *self.entry(s) };
        if e.class as usize != class {
            return Err("a slab in another class's list");
        }
        let size = CLASSES[class];
        let mut seen = [0u64; PAGE / CLASSES[0] / 64];
        let (mut free, mut offset) = (0, e.free);
        while offset != END {
            let at = usize::from(offset);
            if at >= PAGE || !at.is_multiple_of(size) {
                return Err("a free block is not a block of its slab");
            }
            let i = at / size;
            if seen[i / 64] & (1 << (i % 64)) != 0 {
                return Err("a free block is listed twice");
            }
            seen[i / 64] |= 1 << (i % 64);
            free += 1;
            // SAFETY: a free block of the slab, which holds where the next
            // one is.
            offset = unsafe { *((self.page(s) + at) as *const u16) };
        }
        if usize::from(e.used) + free != PAGE / size {
            return Err("a slab's blocks are miscounted");
        }
        Ok(e)
    }
}

#[cfg(test)]
mod tests {
    use std::alloc::{Layout, alloc_zeroed, dealloc};
    use std::collections::BTreeSet;
    use std::vec;
    use std::vec::Vec;

    use super::*;

    /// A random number generator for the tests (xorshift64).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() >> 11) as usize % n
        }
    }

    /// Pages for the tests: memory of their own, given in a random order as
    /// a frame allocator's are, at most `limit` out at once. It scribbles
    /// over each page it gives, as a page given back and taken again may
    /// hold anything, and refuses one back that it did not give.
    struct Arena {
        base: *mut u8,
        len: usize,
        free: Vec<usize>,
        out: Vec<bool>,
        limit: usize,
    }

    impl Arena {
        fn new(len: usize, seed: u64) -> Arena {
            // SAFETY: a layout of a non-zero size.
            let base = unsafe { alloc_zeroed(Layout::from_size_align(len * PAGE, PAGE).unwrap()) };
            assert!(!base.is_null());
            let mut free: Vec<usize> = (0..len).collect();
            let mut rng = Rng(seed);
            for i in (1..len).rev() {
                free.swap(i, rng.below(i + 1));
            }
            Arena { base, len, free, out: vec![false; len], limit: len }
        }

        fn table(&self) -> Vec<usize> {
            vec![0; Slabs::table_len(self.len)]
        }

        fn base(&self) -> usize {
            self.base as usize
        }

        fn out(&self) -> usize {
            self.len - self.free.len()
        }
    }

    impl Drop for Arena {
        fn drop(&mut self) {
            // SAFETY: as allocated.
            unsafe { dealloc(self.base, Layout::from_size_align(self.len * PAGE, PAGE).unwrap()) };
        }
    }

    impl Pages for Arena {
        fn take(&mut self) -> Option<NonNull<u8>> {
            if self.out() == self.limit {
                return None;
            }
            let i = self.free.pop()?;
            self.out[i] = true;
            // SAFETY: page `i` of the arena.
            let page = unsafe { self.base.add(i * PAGE) };
            // SAFETY: as above.
            unsafe { core::ptr::write_bytes(page, 0xa5, PAGE) };
            NonNull::new(page)
        }

        fn give(&mut self, page: NonNull<u8>) {
            let at = page.as_ptr() as usize - self.base();
            let i = at / PAGE;
            assert!(at.is_multiple_of(PAGE) && i < self.len && self.out[i], "a page given back that was not given");
            self.out[i] = false;
            self.free.push(i);
        }
    }

    /// A live block: where, its class, how much of it is filled, with what.
    #[derive(Clone, Copy)]
    struct Live {
        at: NonNull<u8>,
        class: usize,
        len: usize,
        fill: u8,
    }

    impl Live {
        fn fill(at: NonNull<u8>, class: usize, len: usize, fill: u8) -> Live {
            // SAFETY: a block of `CLASSES[class]` bytes, at least `len`.
            unsafe { core::ptr::write_bytes(at.as_ptr(), fill, len) };
            Live { at, class, len, fill }
        }

        fn intact(&self) -> bool {
            // SAFETY: as filled.
            unsafe { core::slice::from_raw_parts(self.at.as_ptr(), self.len) }.iter().all(|&b| b == self.fill)
        }
    }

    #[test]
    fn a_class_is_the_first_whose_blocks_hold_and_align() {
        for size in 0..=4200 {
            for align in (0..13).map(|b| 1usize << b) {
                // As the kernel's heap found it before, a class at a time.
                let slowly = CLASSES.iter().position(|&c| c >= size.max(align).max(1));
                assert_eq!(class(size, align), slowly, "{size} bytes aligned to {align}");
            }
        }
        assert_eq!(class(usize::MAX, 1), None);
    }

    #[test]
    fn blocks_are_aligned_apart_and_kept() {
        let mut arena = Arena::new(256, 1);
        let mut table = arena.table();
        let mut slabs = Slabs::new(arena.base(), &mut table);
        for (class, &size) in CLASSES.iter().enumerate() {
            let count = 3 * PAGE / size + 1;
            let mut live = Vec::new();
            let mut starts = BTreeSet::new();
            for i in 0..count {
                let at = slabs.alloc(class, &mut arena).unwrap();
                assert!((at.as_ptr() as usize).is_multiple_of(size), "a block of {size} bytes is not aligned to it");
                assert!(starts.insert(at.as_ptr() as usize), "a block given twice");
                live.push(Live::fill(at, class, size, i as u8));
            }
            slabs.check().unwrap();
            assert!(live.iter().all(Live::intact), "a block of {size} bytes was overwritten");
            for b in live {
                // SAFETY: a block in use, given back once.
                unsafe { slabs.free(b.at, b.class, &mut arena) };
            }
            slabs.check().unwrap();
        }
        // Each class keeps a slab, its blocks all free.
        assert_eq!(slabs.slabs, CLASSES.len());
        assert_eq!(arena.out(), slabs.pages());
    }

    #[test]
    fn empty_slabs_go_back_but_one_of_each_class() {
        let mut arena = Arena::new(64, 2);
        let mut table = arena.table();
        let mut slabs = Slabs::new(arena.base(), &mut table);
        let class = class(256, 8).unwrap();
        let per_slab = PAGE / 256;
        let blocks: Vec<_> = (0..10 * per_slab).map(|_| slabs.alloc(class, &mut arena).unwrap()).collect();
        assert_eq!(slabs.pages(), 10 + 1);
        for at in blocks {
            // SAFETY: a block in use, given back once.
            unsafe { slabs.free(at, class, &mut arena) };
        }
        slabs.check().unwrap();
        assert_eq!((slabs.slabs, slabs.pages(), arena.out()), (1, 2, 2), "all but a spare slab went back");
        // The spare gives the next blocks, without a page more.
        let again: Vec<_> = (0..per_slab).map(|_| slabs.alloc(class, &mut arena).unwrap()).collect();
        assert_eq!(arena.out(), 2);
        let more = slabs.alloc(class, &mut arena).unwrap();
        assert_eq!(slabs.slabs, 2);
        slabs.check().unwrap();
        for at in again.into_iter().chain([more]) {
            // SAFETY: a block in use, given back once.
            unsafe { slabs.free(at, class, &mut arena) };
        }
        assert_eq!(slabs.slabs, 1);
        slabs.check().unwrap();
    }

    #[test]
    fn a_block_freed_from_a_full_slab_is_the_next_given() {
        let mut arena = Arena::new(16, 3);
        let mut table = arena.table();
        let mut slabs = Slabs::new(arena.base(), &mut table);
        let class = CLASSES.len() - 1;
        let blocks: Vec<_> = (0..6).map(|_| slabs.alloc(class, &mut arena).unwrap()).collect();
        // Three full slabs: one gets its first block back.
        // SAFETY: a block in use, given back once.
        unsafe { slabs.free(blocks[2], class, &mut arena) };
        slabs.check().unwrap();
        assert_eq!(slabs.alloc(class, &mut arena), Some(blocks[2]));
        assert_eq!(slabs.slabs, 3);
    }

    #[test]
    fn a_burst_goes_back_whatever_lived_beside_it() {
        let mut arena = Arena::new(4096, 4);
        let mut table = arena.table();
        let mut slabs = Slabs::new(arena.base(), &mut table);
        let mut rng = Rng(5);
        let (mut kept, mut burst) = (Vec::new(), Vec::new());
        for i in 0..40_000 {
            let class = 3 + rng.below(3);
            let at = slabs.alloc(class, &mut arena).unwrap();
            let b = Live::fill(at, class, CLASSES[class], i as u8);
            match i % 1000 {
                0 => kept.push(b),
                _ => burst.push(b),
            }
        }
        let grown = slabs.pages();
        for b in burst {
            assert!(b.intact());
            // SAFETY: a block in use, given back once.
            unsafe { slabs.free(b.at, b.class, &mut arena) };
        }
        slabs.check().unwrap();
        assert!(kept.iter().all(Live::intact));
        // What is left: the slabs of the blocks kept, a spare of each class,
        // and their pages of entries.
        let page = |at: usize| (at - arena.base()) / PAGE;
        let holding: BTreeSet<_> = kept.iter().map(|b| page(b.at.as_ptr() as usize)).collect();
        let spares: BTreeSet<_> = slabs.classes.iter().filter(|c| c.spare != NONE).map(|c| c.spare as usize).collect();
        assert_eq!(spares.len(), 3);
        assert!(holding.is_disjoint(&spares));
        assert_eq!(slabs.slabs, holding.len() + spares.len(), "slabs left of {grown} pages");
        let groups: BTreeSet<_> = holding.union(&spares).map(|s| s / PER_PAGE).collect();
        assert_eq!(slabs.entries, groups.len());
        assert_eq!(arena.out(), slabs.pages());
        for b in kept {
            // SAFETY: a block in use, given back once.
            unsafe { slabs.free(b.at, b.class, &mut arena) };
        }
        assert_eq!(slabs.slabs, 3);
        slabs.check().unwrap();
    }

    #[test]
    fn no_page_for_a_slab_changes_nothing() {
        let mut arena = Arena::new(16, 6);
        let mut table = arena.table();
        let mut slabs = Slabs::new(arena.base(), &mut table);
        // A slab needs its page and a page of entries.
        arena.limit = 1;
        assert_eq!(slabs.alloc(0, &mut arena), None);
        slabs.check().unwrap();
        assert_eq!((slabs.pages(), arena.out()), (0, 0));
        arena.limit = 2;
        let blocks: Vec<_> = (0..PAGE / 16).map(|_| slabs.alloc(0, &mut arena).unwrap()).collect();
        assert_eq!(slabs.alloc(0, &mut arena), None, "the slab is full and there is no page for another");
        slabs.check().unwrap();
        arena.limit = 3;
        let more = slabs.alloc(0, &mut arena).unwrap();
        assert!(!blocks.contains(&more));
        for at in blocks.into_iter().chain([more]) {
            // SAFETY: a block in use, given back once.
            unsafe { slabs.free(at, 0, &mut arena) };
        }
        slabs.check().unwrap();
        assert_eq!((slabs.pages(), arena.out()), (2, 2));
    }

    #[test]
    fn pages_of_entries_go_back_with_their_last_slab() {
        // Pages given in a random order: slabs far apart, with entries in
        // pages of their own.
        let mut arena = Arena::new(2048, 7);
        let mut table = arena.table();
        let mut slabs = Slabs::new(arena.base(), &mut table);
        let blocks: Vec<_> = (0..200).map(|_| slabs.alloc(7, &mut arena).unwrap()).collect();
        assert_eq!(slabs.slabs, 100);
        assert_eq!(slabs.entries, Slabs::table_len(arena.len), "slabs among each 256 pages");
        for at in blocks {
            // SAFETY: a block in use, given back once.
            unsafe { slabs.free(at, 7, &mut arena) };
        }
        slabs.check().unwrap();
        assert_eq!((slabs.slabs, slabs.entries, arena.out()), (1, 1, 2));
    }

    #[test]
    fn randomized_stress() {
        // Smaller than what the phases that fill it would take.
        let mut arena = Arena::new(640, 8);
        let mut table = arena.table();
        let mut slabs = Slabs::new(arena.base(), &mut table);
        let mut rng = Rng(0x5eed_cafe);
        let mut live: Vec<Live> = Vec::new();
        let (mut fill, mut most, mut full) = (0u8, 0, 0);
        for step in 0..200_000 {
            fill = fill.wrapping_add(1);
            // Phases that fill memory, and phases that mostly empty it.
            let frees = if (step / 20_000) % 2 == 0 { 4 } else { 6 };
            if rng.below(10) < frees && !live.is_empty() {
                let b = live.swap_remove(rng.below(live.len()));
                assert!(b.intact(), "a block was overwritten (step {step})");
                // SAFETY: a block in use, given back once.
                unsafe { slabs.free(b.at, b.class, &mut arena) };
            } else {
                let size = rng.below(2048) + 1;
                let align = 1 << rng.below(8);
                let class = class(size, align).unwrap();
                match slabs.alloc(class, &mut arena) {
                    Some(at) => {
                        assert!((at.as_ptr() as usize).is_multiple_of(align));
                        live.push(Live::fill(at, class, size, fill));
                    }
                    None => full += 1,
                }
            }
            most = most.max(slabs.pages());
            if step % 997 == 0 {
                slabs.check().unwrap();
                assert_eq!(arena.out(), slabs.pages());
            }
        }
        assert!(full > 0 && most <= arena.len, "memory never ran out");
        for b in live {
            assert!(b.intact());
            // SAFETY: a block in use, given back once.
            unsafe { slabs.free(b.at, b.class, &mut arena) };
        }
        slabs.check().unwrap();
        // A spare for each class, and their pages of entries.
        assert!(slabs.slabs <= CLASSES.len() && slabs.pages() <= 2 * CLASSES.len());
        assert_eq!(arena.out(), slabs.pages());
    }
}

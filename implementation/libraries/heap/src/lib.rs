// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A TLSF (two-level segregated fit) allocator.
//!
//! TLSF gives O(1) allocation and deallocation with low fragmentation:
//! free blocks are kept in 2D-indexed segregated lists (first level = power
//! of two, second level = 32 linear subdivisions), with bitmaps to find a
//! suitable non-empty list in constant time. Adjacent free blocks are merged
//! immediately using boundary information.
//!
//! The allocator manages one contiguous region that can grow at its end
//! (`Tlsf::grow`); it is not thread-safe by itself (wrap it in a lock).
//!
//! Block layout (all blocks 16-byte aligned):
//!
//! ```text
//! +-----------------+------------------+---------------------------------+
//! | prev_phys (8 B) | size|flags (8 B) | payload (size bytes)            |
//! +-----------------+------------------+---------------------------------+
//!                                       free blocks keep their free-list
//!                                       links in the first 16 payload bytes
//! ```
//!
//! The region ends with a zero-size "sentinel" block that is always in use,
//! so the last real block never needs a bounds check when merging.
//!
//! Free blocks know two more things about their memory, for the program
//! that owns it:
//!
//! * **Zeroed**: the payload past the free-list links is all zeros, as
//!   fresh memory from the system is (whoever [grows](Tlsf::grow) the region
//!   says whether it is) and as memory given back to the system reads
//!   afterwards. A [zeroed allocation](Tlsf::alloc_zeroed) clears only what
//!   is not known to be zero: nothing at all for fresh memory, whose pages
//!   the system then need not provide until they are used.
//! * **Idle**: the block has stayed free, and unchanged, since the last
//!   pass of [`Tlsf::release_idle`]. That pass offers the whole pages of
//!   blocks that are idle for a whole interval back to the system, so
//!   memory freed and soon reused (a buffer allocated again and again) is
//!   never given back and faulted in again.
//!
//! [`Tlsf::free`] checks, cheaply, that what it is given is a block in use
//! (catching double frees and most wild pointers) before it changes
//! anything, and [`Tlsf::check`] verifies every invariant, for tests.

#![no_std]

#[cfg(test)]
extern crate std;

use core::ptr::{null_mut, write_bytes};

const ALIGN: usize = 16;
const HEADER: usize = 16;
const MIN_PAYLOAD: usize = 16;
/// The free-list links at the start of a free block's payload.
const LINKS: usize = 16;
const SL_BITS: u32 = 5;
const SL_COUNT: usize = 1 << SL_BITS;
const FL_SHIFT: u32 = SL_BITS + 4; // sizes below 2^9 use first level 0, linearly
const FL_COUNT: usize = 40;
const SMALL_LIMIT: usize = 1 << FL_SHIFT;

/// The block is free (in a free list).
const FREE: usize = 1;
/// A free block whose payload past its links is all zeros.
const ZEROED: usize = 2;
/// A free block that has not changed since the last pass of
/// `release_idle`.
const IDLE: usize = 4;
/// The bits of `size_flags` that are flags (block sizes are multiples of
/// `ALIGN`).
const FLAGS: usize = ALIGN - 1;
const SIZE_MASK: usize = !FLAGS;

#[repr(C)]
struct Block {
    prev_phys: *mut Block,
    size_flags: usize,
    // Valid only while free:
    next_free: *mut Block,
    prev_free: *mut Block,
}

impl Block {
    #[inline]
    fn size(&self) -> usize {
        self.size_flags & SIZE_MASK
    }

    #[inline]
    fn is_free(&self) -> bool {
        self.size_flags & FREE != 0
    }

    #[inline]
    fn is_zeroed(&self) -> bool {
        self.size_flags & ZEROED != 0
    }

    #[inline]
    unsafe fn payload(b: *mut Block) -> *mut u8 {
        // SAFETY: the payload follows the header.
        unsafe { (b as *mut u8).add(HEADER) }
    }

    #[inline]
    unsafe fn next_phys(b: *mut Block) -> *mut Block {
        // SAFETY: blocks tile the region; the sentinel terminates it.
        unsafe { (b as *mut u8).add(HEADER + (*b).size()) as *mut Block }
    }
}

/// The address just past block `b` (its next block's), unless its size
/// overflows the address space.
///
/// # Safety
/// `b` must be a readable header.
#[inline]
unsafe fn end_of(b: *mut Block) -> Option<usize> {
    // SAFETY: per the caller.
    (b as usize).checked_add(HEADER)?.checked_add(unsafe { (*b).size() })
}

/// Maps a size to its (first level, second level) list.
#[inline]
fn mapping(size: usize) -> (usize, usize) {
    if size < SMALL_LIMIT {
        (0, size / (SMALL_LIMIT / SL_COUNT))
    } else {
        let fl = usize::BITS - 1 - size.leading_zeros();
        let sl = (size >> (fl - SL_BITS)) & (SL_COUNT - 1);
        ((fl - FL_SHIFT + 1) as usize, sl)
    }
}

/// Rounds `size` up so that any block in the selected list is big enough;
/// `None` if that overflows (no block can be that big).
#[inline]
fn round_for_search(size: usize) -> Option<usize> {
    if size >= SMALL_LIMIT {
        let fl = usize::BITS - 1 - size.leading_zeros();
        size.checked_add((1usize << (fl - SL_BITS)) - 1)
    } else {
        Some(size)
    }
}

/// A TLSF allocator over one region of memory.
pub struct Tlsf {
    fl_bitmap: u64,
    sl_bitmap: [u32; FL_COUNT],
    lists: [[*mut Block; SL_COUNT]; FL_COUNT],
    /// The first block of the managed region (null if empty).
    first: *mut Block,
    /// The sentinel block at the end of the managed region (null if empty).
    sentinel: *mut Block,
    /// Bytes handed out (payload sizes of used blocks).
    used: usize,
    /// Total bytes of region managed.
    total: usize,
}

// SAFETY: the raw pointers refer to memory owned by the allocator; callers
// serialise access with a lock.
unsafe impl Send for Tlsf {}

/// Error returned when no block can satisfy a request; the caller should
/// [`Tlsf::grow`] the region and retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfMemory;

impl core::fmt::Display for OutOfMemory {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("no free block is large enough")
    }
}

/// Error returned by [`Tlsf::free`] for a pointer that is not an allocation
/// in use: one freed already, or one the allocator never handed out. Nothing
/// was changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidFree;

impl core::fmt::Display for InvalidFree {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("not an allocation in use (freed twice, or never allocated)")
    }
}

impl Default for Tlsf {
    fn default() -> Self {
        Self::new()
    }
}

impl Tlsf {
    /// An allocator with no memory yet ([`Tlsf::grow`] gives it some).
    pub const fn new() -> Tlsf {
        Tlsf {
            fl_bitmap: 0,
            sl_bitmap: [0; FL_COUNT],
            lists: [[null_mut(); SL_COUNT]; FL_COUNT],
            first: null_mut(),
            sentinel: null_mut(),
            used: 0,
            total: 0,
        }
    }

    /// Bytes handed out: the payload sizes of the blocks in use.
    pub fn used_bytes(&self) -> usize {
        self.used
    }

    /// Bytes of memory managed (what [`Tlsf::grow`] was given).
    pub fn total_bytes(&self) -> usize {
        self.total
    }

    unsafe fn insert(&mut self, b: *mut Block) {
        // SAFETY: `b` is a valid free block not in any list.
        unsafe {
            let (fl, sl) = mapping((*b).size());
            let head = self.lists[fl][sl];
            (*b).next_free = head;
            (*b).prev_free = null_mut();
            if !head.is_null() {
                (*head).prev_free = b;
            }
            self.lists[fl][sl] = b;
            self.fl_bitmap |= 1 << fl;
            self.sl_bitmap[fl] |= 1 << sl;
        }
    }

    unsafe fn remove(&mut self, b: *mut Block) {
        // SAFETY: `b` is a valid free block in the list for its size.
        unsafe {
            let (fl, sl) = mapping((*b).size());
            let (prev, next) = ((*b).prev_free, (*b).next_free);
            if !next.is_null() {
                (*next).prev_free = prev;
            }
            if prev.is_null() {
                self.lists[fl][sl] = next;
                if next.is_null() {
                    self.sl_bitmap[fl] &= !(1 << sl);
                    if self.sl_bitmap[fl] == 0 {
                        self.fl_bitmap &= !(1 << fl);
                    }
                }
            } else {
                (*prev).next_free = next;
            }
        }
    }

    /// Finds a free block of at least `size` bytes and removes it from its list.
    fn find(&mut self, size: usize) -> Option<*mut Block> {
        if let Some(b) = round_for_search(size).and_then(|rounded| self.find_good_fit(rounded)) {
            return Some(b);
        }
        // The rounded search skips the list `size` itself maps to, which may
        // still hold a block that is large enough: scan it (first fit).
        let (fl, sl) = mapping(size);
        if fl >= FL_COUNT {
            return None;
        }
        let mut b = self.lists[fl][sl];
        while !b.is_null() {
            // SAFETY: list entries are valid free blocks.
            unsafe {
                if (*b).size() >= size {
                    self.remove(b);
                    return Some(b);
                }
                b = (*b).next_free;
            }
        }
        None
    }

    /// Constant-time search for a block of at least `rounded` bytes, a size
    /// [`round_for_search`] made: any block in the list it maps to fits.
    fn find_good_fit(&mut self, rounded: usize) -> Option<*mut Block> {
        let (mut fl, sl) = mapping(rounded);
        if fl >= FL_COUNT {
            return None;
        }
        let mut sl_map = self.sl_bitmap[fl] & (!0u32 << sl);
        if sl_map == 0 {
            let fl_map = self.fl_bitmap & (!0u64).checked_shl(fl as u32 + 1).unwrap_or(0);
            if fl_map == 0 {
                return None;
            }
            fl = fl_map.trailing_zeros() as usize;
            sl_map = self.sl_bitmap[fl];
        }
        let sl = sl_map.trailing_zeros() as usize;
        let b = self.lists[fl][sl];
        // SAFETY: bitmaps only mark non-empty lists.
        unsafe { self.remove(b) };
        Some(b)
    }

    /// Cuts the end off the block `b` (in use, in no list) so that it keeps
    /// `size` bytes, if the rest can be a block of its own, which becomes
    /// free: zeroed if `zeroed` (its payload was part of a zeroed one's).
    unsafe fn split(&mut self, b: *mut Block, size: usize, zeroed: bool) {
        // SAFETY: `b` is a valid block not in any free list.
        unsafe {
            let total = (*b).size();
            if total >= size + HEADER + MIN_PAYLOAD {
                let rest = (Block::payload(b)).add(size) as *mut Block;
                (*rest).prev_phys = b;
                (*rest).size_flags = (total - size - HEADER) | FREE | if zeroed { ZEROED } else { 0 };
                (*b).size_flags = size | ((*b).size_flags & FLAGS);
                let after = Block::next_phys(rest);
                (*after).prev_phys = rest;
                self.merge_and_insert(rest);
            }
        }
    }

    /// Merges a free block (in no list, not idle) with free physical
    /// neighbours and inserts it. The result is zeroed if every part was:
    /// the headers and links that end up inside its payload are cleared.
    unsafe fn merge_and_insert(&mut self, mut b: *mut Block) {
        // SAFETY: neighbours are valid blocks (the sentinel bounds the end),
        // and the bytes cleared are headers and links that become payload.
        unsafe {
            let mut zeroed = (*b).is_zeroed();
            let next = Block::next_phys(b);
            if (*next).is_free() {
                self.remove(next);
                zeroed &= (*next).is_zeroed();
                (*b).size_flags = ((*b).size() + HEADER + (*next).size()) | FREE;
                (*Block::next_phys(b)).prev_phys = b;
                if zeroed {
                    write_bytes(next as *mut u8, 0, HEADER + LINKS);
                }
            }
            let prev = (*b).prev_phys;
            if !prev.is_null() && (*prev).is_free() {
                self.remove(prev);
                zeroed &= (*prev).is_zeroed();
                (*prev).size_flags = ((*prev).size() + HEADER + (*b).size()) | FREE;
                (*Block::next_phys(prev)).prev_phys = prev;
                if zeroed {
                    write_bytes(b as *mut u8, 0, HEADER + LINKS);
                }
                b = prev;
            }
            (*b).size_flags = (*b).size() | FREE | if zeroed { ZEROED } else { 0 };
            self.insert(b);
        }
    }

    /// Adds `[start, start + len)` to the region. The first call establishes
    /// the region; later calls must pass memory that starts exactly where the
    /// previous region ended (`end_of_region`). `zeroed`: the memory reads
    /// as zeros (fresh pages from the system), so zeroed allocations from
    /// it need not clear it.
    ///
    /// # Safety
    /// The memory must be valid, writable and unused for the allocator's
    /// life, and all zeros if `zeroed`.
    pub unsafe fn grow(&mut self, start: *mut u8, len: usize, zeroed: bool) {
        let start_addr = start as usize;
        assert!(start_addr.is_multiple_of(ALIGN) && len >= 2 * HEADER + MIN_PAYLOAD && len.is_multiple_of(ALIGN));
        let zeroed = if zeroed { ZEROED } else { 0 };
        // SAFETY: the caller hands us exclusive memory.
        unsafe {
            let block = if self.sentinel.is_null() {
                let b = start as *mut Block;
                (*b).prev_phys = null_mut();
                (*b).size_flags = (len - 2 * HEADER) | FREE | zeroed;
                self.first = b;
                b
            } else {
                // The old sentinel becomes the header of the new free block,
                // whose payload is all new memory.
                assert_eq!(self.sentinel as usize + HEADER, start_addr, "TLSF regions must be contiguous");
                let b = self.sentinel;
                (*b).size_flags = (len - HEADER) | FREE | zeroed;
                b
            };
            let sentinel = Block::next_phys(block);
            (*sentinel).prev_phys = block;
            (*sentinel).size_flags = 0; // used, size 0
            self.sentinel = sentinel;
            self.total += len;
            self.merge_and_insert(block);
        }
    }

    /// Address just past the end of the managed region.
    pub fn end_of_region(&self) -> *mut u8 {
        if self.sentinel.is_null() { null_mut() } else { (self.sentinel as usize + HEADER) as *mut u8 }
    }

    /// Allocates `size` bytes aligned to `align` (a power of two).
    pub fn alloc(&mut self, size: usize, align: usize) -> Result<*mut u8, OutOfMemory> {
        self.allocate(size, align, false)
    }

    /// Allocates `size` bytes aligned to `align` (a power of two), all
    /// zeros. Only what is not known to be zero is cleared (see the
    /// module's documentation).
    pub fn alloc_zeroed(&mut self, size: usize, align: usize) -> Result<*mut u8, OutOfMemory> {
        self.allocate(size, align, true)
    }

    fn allocate(&mut self, size: usize, align: usize, zero: bool) -> Result<*mut u8, OutOfMemory> {
        if !align.is_power_of_two() {
            return Err(OutOfMemory);
        }
        let asked = size;
        let size = size.max(MIN_PAYLOAD).checked_next_multiple_of(ALIGN).ok_or(OutOfMemory)?;
        if align <= ALIGN {
            let b = self.find(size).ok_or(OutOfMemory)?;
            // SAFETY: `b` is a free block of at least `size` bytes, taken
            // out of its list.
            unsafe {
                let zeroed = (*b).is_zeroed();
                (*b).size_flags = (*b).size();
                self.split(b, size, zeroed);
                self.used += (*b).size();
                let p = Block::payload(b);
                if zero {
                    // A zeroed block's payload is zero but for its links.
                    write_bytes(p, 0, if zeroed { LINKS.min(asked) } else { asked });
                }
                return Ok(p);
            }
        }
        // Over-allocate so an aligned payload with room for a leading free
        // block fits inside.
        let padded = size.checked_add(align).and_then(|s| s.checked_add(HEADER + MIN_PAYLOAD)).ok_or(OutOfMemory)?;
        let b = self.find(padded).ok_or(OutOfMemory)?;
        // SAFETY: as above; all pointers stay inside `b`, which `padded`
        // leaves room for.
        unsafe {
            let zeroed = (*b).is_zeroed();
            let payload = Block::payload(b) as usize;
            let mut aligned = payload.next_multiple_of(align);
            if aligned != payload && aligned - payload < HEADER + MIN_PAYLOAD {
                aligned += align;
            }
            // Bytes at the start of the payload to clear for a zeroed
            // allocation.
            let mut dirty = asked;
            let blk = if aligned != payload {
                // Carve the leading gap into its own free block (still
                // zeroed if `b` was: nothing past its links changed).
                let gap = aligned - payload; // >= HEADER + MIN_PAYLOAD
                let nb = (aligned - HEADER) as *mut Block;
                (*nb).prev_phys = b;
                (*nb).size_flags = (*b).size() - gap;
                (*b).size_flags = (gap - HEADER) | FREE | if zeroed { ZEROED } else { 0 };
                (*Block::next_phys(nb)).prev_phys = nb;
                self.insert(b);
                // The new block's payload was past `b`'s links.
                if zeroed {
                    dirty = 0;
                }
                nb
            } else {
                (*b).size_flags = (*b).size();
                if zeroed {
                    dirty = LINKS.min(asked);
                }
                b
            };
            self.split(blk, size, zeroed);
            self.used += (*blk).size();
            let p = Block::payload(blk);
            if zero {
                write_bytes(p, 0, dirty);
            }
            Ok(p)
        }
    }

    /// The block in use whose payload is at `p`, if `p` is one: checked
    /// without reading outside the region, by the block's flags and its
    /// neighbours' links. This catches a double free (the block is free, or
    /// was, and kept its header) and most wild pointers.
    fn used_block(&self, p: *mut u8) -> Option<*mut Block> {
        let addr = p as usize;
        if self.first.is_null() || addr < self.first as usize + HEADER {
            return None;
        }
        let b = (addr - HEADER) as *mut Block;
        // SAFETY: `is_block` reads nothing outside the region; then `b` is a
        // block's header.
        unsafe { (self.is_block(b) && (*b).size_flags & FLAGS == 0).then_some(b) }
    }

    /// Frees a pointer returned by [`Tlsf::alloc`] or [`Tlsf::alloc_zeroed`].
    /// A pointer that is not an allocation in use (freed already, or never
    /// allocated) is refused before anything changes.
    ///
    /// # Safety
    /// `p` must come from this allocator; a pointer into the region that it
    /// did not hand out may pass the checks.
    pub unsafe fn free(&mut self, p: *mut u8) -> Result<(), InvalidFree> {
        let b = self.used_block(p).ok_or(InvalidFree)?;
        // SAFETY: `b` is a block in use.
        unsafe {
            self.used -= (*b).size();
            (*b).size_flags = (*b).size() | FREE;
            self.merge_and_insert(b);
        }
        Ok(())
    }

    /// Attempts to resize an allocation in place. Returns `true` on success,
    /// `false` if it cannot (or `p` is not an allocation in use).
    ///
    /// # Safety
    /// `p` must be a live allocation from this allocator.
    pub unsafe fn resize_in_place(&mut self, p: *mut u8, new_size: usize) -> bool {
        let Some(new_size) = new_size.max(MIN_PAYLOAD).checked_next_multiple_of(ALIGN) else { return false };
        let Some(b) = self.used_block(p) else { return false };
        // SAFETY: `b` is a block in use; its neighbours are valid blocks.
        unsafe {
            let cur = (*b).size();
            if new_size <= cur {
                self.used -= cur;
                self.split(b, new_size, false);
                self.used += (*b).size();
                return true;
            }
            let next = Block::next_phys(b);
            if (*next).is_free() && cur + HEADER + (*next).size() >= new_size {
                self.remove(next);
                self.used -= cur;
                (*b).size_flags = cur + HEADER + (*next).size();
                (*Block::next_phys(b)).prev_phys = b;
                self.split(b, new_size, false);
                self.used += (*b).size();
                return true;
            }
            false
        }
    }

    /// Usable size of an allocation.
    ///
    /// # Safety
    /// `p` must be a live allocation from this allocator.
    pub unsafe fn usable_size(p: *mut u8) -> usize {
        // SAFETY: guaranteed by the caller.
        unsafe { (*(p.sub(HEADER) as *mut Block)).size() }
    }

    /// Offers back the memory of free blocks of at least `min` bytes that
    /// have stayed free, unchanged, since the previous call: a block is
    /// marked idle by one call and offered by the next if nothing touched it
    /// in between. `release(start, len)` is given the `granule`-aligned part
    /// (a power of two: a page) of such a block's payload past its links;
    /// when it returns `true` the memory reads as zeros from then on (the
    /// system took its pages back), so the rest of the payload is cleared
    /// too and the block becomes zeroed: it is neither offered again nor
    /// cleared by a zeroed allocation.
    pub fn release_idle(&mut self, granule: usize, min: usize, mut release: impl FnMut(*mut u8, usize) -> bool) {
        let (first_fl, _) = mapping(min);
        for fl in first_fl..FL_COUNT {
            if self.fl_bitmap & (1 << fl) == 0 {
                continue;
            }
            for sl in 0..SL_COUNT {
                let mut b = self.lists[fl][sl];
                while !b.is_null() {
                    // SAFETY: list entries are valid free blocks; only their
                    // flags and unused payload change here.
                    unsafe {
                        let next = (*b).next_free;
                        let (size, flags) = ((*b).size(), (*b).size_flags);
                        if size >= min && flags & ZEROED == 0 {
                            if flags & IDLE == 0 {
                                (*b).size_flags |= IDLE;
                            } else {
                                let start = Block::payload(b) as usize + LINKS;
                                let end = Block::payload(b) as usize + size;
                                let (from, to) = (start.next_multiple_of(granule), end & !(granule - 1));
                                if from < to && release(from as *mut u8, to - from) {
                                    write_bytes(start as *mut u8, 0, from - start);
                                    write_bytes(to as *mut u8, 0, end - to);
                                    (*b).size_flags = size | FREE | ZEROED;
                                }
                            }
                        }
                        b = next;
                    }
                }
            }
        }
    }

    /// Whether `b` is the start of a block of the region: inside it, and
    /// linked with its physical neighbours both ways. Reads nothing outside
    /// the region, whatever `b` is.
    ///
    /// # Safety
    /// The region must be set up.
    unsafe fn is_block(&self, b: *mut Block) -> bool {
        if !self.is_inside(b) {
            return false;
        }
        // SAFETY: `b` is inside the region, and so is every header read
        // below, checked before it is read.
        unsafe {
            let Some(next) = end_of(b) else { return false };
            if next > self.sentinel as usize || (*(next as *mut Block)).prev_phys != b {
                return false;
            }
            let prev = (*b).prev_phys;
            if prev.is_null() {
                return b == self.first;
            }
            (prev as usize) < b as usize && self.is_inside(prev) && end_of(prev) == Some(b as usize)
        }
    }

    /// Whether `b` is inside the region, before the sentinel, and 16-byte
    /// aligned: where a header can be read.
    fn is_inside(&self, b: *mut Block) -> bool {
        let at = b as usize;
        at.is_multiple_of(ALIGN) && at >= self.first as usize && at < self.sentinel as usize
    }

    /// Checks every invariant, for tests and debugging: the blocks tile the
    /// region with consistent links and no two free ones adjacent; the used
    /// and total counts; every free block in the list its size maps to, and
    /// nothing else in the lists; the bitmaps marking exactly the lists that
    /// hold blocks; and zeroed blocks reading as zeros (so this reads every
    /// byte of them).
    pub fn check(&self) -> Result<(), &'static str> {
        if self.sentinel.is_null() {
            let empty = self.first.is_null()
                && self.used == 0
                && self.total == 0
                && self.fl_bitmap == 0
                && self.sl_bitmap.iter().all(|&m| m == 0)
                && self.lists.iter().flatten().all(|b| b.is_null());
            return if empty { Ok(()) } else { Err("an empty allocator has state") };
        }
        let (mut used, mut bytes, mut free_blocks) = (0usize, 0usize, 0usize);
        // SAFETY: a read-only walk; every header read is checked to be in
        // the region first.
        unsafe {
            let mut prev: *mut Block = null_mut();
            let mut b = self.first;
            let mut prev_free = false;
            while b != self.sentinel {
                if !self.is_inside(b) {
                    return Err("the blocks run past the sentinel");
                }
                if (*b).prev_phys != prev {
                    return Err("prev_phys does not lead back to the previous block");
                }
                let (size, flags) = ((*b).size(), (*b).size_flags & FLAGS);
                let end = end_of(b).filter(|&e| e <= self.sentinel as usize).ok_or("a block runs past the sentinel")?;
                if size < MIN_PAYLOAD {
                    return Err("a block smaller than the smallest payload");
                }
                if flags & !(FREE | ZEROED | IDLE) != 0 {
                    return Err("a block with unknown flags");
                }
                if flags & FREE != 0 {
                    if prev_free {
                        return Err("two adjacent free blocks");
                    }
                    free_blocks += 1;
                    if flags & ZEROED != 0 {
                        let payload = core::slice::from_raw_parts(Block::payload(b), size);
                        if payload[LINKS..].iter().any(|&x| x != 0) {
                            return Err("a zeroed block holds something other than zeros");
                        }
                    }
                } else {
                    if flags != 0 {
                        return Err("a block in use has flags");
                    }
                    used += size;
                }
                bytes += HEADER + size;
                prev_free = flags & FREE != 0;
                prev = b;
                b = end as *mut Block;
            }
            if (*self.sentinel).prev_phys != prev || (*self.sentinel).size_flags != 0 {
                return Err("the sentinel is not the last block, in use and empty");
            }
            bytes += HEADER;
        }
        if used != self.used {
            return Err("the bytes in use do not add up");
        }
        if bytes != self.total {
            return Err("the blocks do not cover the region");
        }
        if self.fl_bitmap >> FL_COUNT != 0 {
            return Err("the first-level bitmap marks lists that do not exist");
        }
        let mut listed = 0usize;
        for fl in 0..FL_COUNT {
            if (self.sl_bitmap[fl] != 0) != (self.fl_bitmap & (1 << fl) != 0) {
                return Err("the first-level bitmap does not match the second level");
            }
            for sl in 0..SL_COUNT {
                let head = self.lists[fl][sl];
                if head.is_null() != (self.sl_bitmap[fl] & (1 << sl) == 0) {
                    return Err("the second-level bitmap does not match the lists");
                }
                let mut prev: *mut Block = null_mut();
                let mut b = head;
                while !b.is_null() {
                    if listed == free_blocks {
                        return Err("the free lists hold more than the free blocks");
                    }
                    // SAFETY: `is_block` reads only inside the region; the
                    // header is read once it holds.
                    unsafe {
                        if !self.is_block(b) || !(*b).is_free() {
                            return Err("a free list holds something other than a free block");
                        }
                        if mapping((*b).size()) != (fl, sl) {
                            return Err("a free block is in the wrong list");
                        }
                        if (*b).prev_free != prev {
                            return Err("a free list's back link is wrong");
                        }
                        prev = b;
                        b = (*b).next_free;
                    }
                    listed += 1;
                }
            }
        }
        if listed != free_blocks {
            return Err("a free block is missing from the free lists");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    #[repr(align(4096))]
    struct Arena([u8; 1 << 20]);

    fn arena() -> std::boxed::Box<Arena> {
        // SAFETY: zeroed bytes are a valid Arena.
        unsafe { std::boxed::Box::new_zeroed().assume_init() }
    }

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

    #[test]
    fn mapping_is_monotonic() {
        let mut last = (0, 0);
        for size in (16..1 << 22).step_by(16) {
            let m = mapping(size);
            assert!(m >= last, "mapping not monotonic at {size}");
            last = m;
        }
    }

    #[test]
    fn alloc_free_roundtrip() {
        let mut a = arena();
        let mut t = Tlsf::new();
        unsafe { t.grow(a.0.as_mut_ptr(), a.0.len(), true) };
        let mut ptrs = Vec::new();
        for i in 0..2000 {
            let size = (i * 37) % 400 + 1;
            let p = t.alloc(size, 16).unwrap();
            assert_eq!(p as usize % 16, 0);
            unsafe { core::ptr::write_bytes(p, (i % 251) as u8, size) };
            ptrs.push((p, size, (i % 251) as u8));
        }
        t.check().unwrap();
        for (i, &(p, size, v)) in ptrs.iter().enumerate() {
            let s = unsafe { core::slice::from_raw_parts(p, size) };
            assert!(s.iter().all(|&b| b == v), "corruption in block {i}");
        }
        // Free every other block, then the rest.
        for (p, _, _) in ptrs.iter().step_by(2) {
            unsafe { t.free(*p).unwrap() };
        }
        t.check().unwrap();
        for (p, _, _) in ptrs.iter().skip(1).step_by(2) {
            unsafe { t.free(*p).unwrap() };
        }
        t.check().unwrap();
        assert_eq!(t.used_bytes(), 0);
        // Everything merged back into one block: a near-full allocation fits.
        let big = t.alloc(a.0.len() - 4 * HEADER, 16).unwrap();
        unsafe { t.free(big).unwrap() };
    }

    #[test]
    fn aligned_allocations() {
        let mut a = arena();
        let mut t = Tlsf::new();
        unsafe { t.grow(a.0.as_mut_ptr(), a.0.len(), true) };
        let mut ptrs = Vec::new();
        for &align in &[32usize, 64, 128, 256, 4096] {
            for size in [1usize, 100, 5000] {
                let p = t.alloc(size, align).unwrap();
                assert_eq!(p as usize % align, 0);
                ptrs.push(p);
            }
        }
        t.check().unwrap();
        for p in ptrs {
            unsafe { t.free(p).unwrap() };
        }
        t.check().unwrap();
        assert_eq!(t.used_bytes(), 0);
    }

    #[test]
    fn grow_extends_region() {
        let mut a = arena();
        let mut t = Tlsf::new();
        let half = a.0.len() / 2;
        unsafe { t.grow(a.0.as_mut_ptr(), half, true) };
        assert!(t.alloc(half, 16).is_err());
        let end = t.end_of_region();
        assert_eq!(end as usize, a.0.as_ptr() as usize + half);
        unsafe { t.grow(end, half, true) };
        let p = t.alloc(half, 16).unwrap();
        t.check().unwrap();
        unsafe { t.free(p).unwrap() };
        t.check().unwrap();
    }

    #[test]
    fn resize_in_place_grows_into_free_neighbour() {
        let mut a = arena();
        let mut t = Tlsf::new();
        unsafe { t.grow(a.0.as_mut_ptr(), a.0.len(), true) };
        let p = t.alloc(100, 16).unwrap();
        let q = t.alloc(100, 16).unwrap();
        unsafe { t.free(q).unwrap() };
        assert!(unsafe { t.resize_in_place(p, 200) });
        assert!(unsafe { Tlsf::usable_size(p) } >= 200);
        assert!(unsafe { t.resize_in_place(p, 50) });
        t.check().unwrap();
        unsafe { t.free(p).unwrap() };
        assert_eq!(t.used_bytes(), 0);
    }

    #[test]
    fn double_and_wild_frees_are_refused() {
        let mut a = arena();
        let mut t = Tlsf::new();
        // Before there is a region, nothing is an allocation.
        assert_eq!(unsafe { t.free(a.0.as_mut_ptr()) }, Err(InvalidFree));
        unsafe { t.grow(a.0.as_mut_ptr(), a.0.len(), true) };
        let p = t.alloc(64, 16).unwrap();
        let q = t.alloc(64, 16).unwrap();
        let r = t.alloc(64, 16).unwrap();
        unsafe { t.free(q).unwrap() };
        // Freed already: as itself, and once merged into a neighbour.
        assert_eq!(unsafe { t.free(q) }, Err(InvalidFree));
        unsafe { t.free(p).unwrap() };
        assert_eq!(unsafe { t.free(q) }, Err(InvalidFree));
        assert_eq!(unsafe { t.free(p) }, Err(InvalidFree));
        // Not the start of an allocation, misaligned, outside the region.
        assert_eq!(unsafe { t.free(r.add(16)) }, Err(InvalidFree));
        assert_eq!(unsafe { t.free(r.add(1)) }, Err(InvalidFree));
        let mut elsewhere = [0u64; 8];
        assert_eq!(unsafe { t.free(elsewhere.as_mut_ptr().add(4) as *mut u8) }, Err(InvalidFree));
        assert_eq!(unsafe { t.free(t.end_of_region()) }, Err(InvalidFree));
        // Nothing changed: the heap is whole and `r` frees once.
        t.check().unwrap();
        assert!(!unsafe { t.resize_in_place(q, 32) });
        unsafe { t.free(r).unwrap() };
        assert_eq!(unsafe { t.free(r) }, Err(InvalidFree));
        t.check().unwrap();
        assert_eq!(t.used_bytes(), 0);
    }

    #[test]
    fn impossible_sizes_and_alignments_fail() {
        let mut a = arena();
        let mut t = Tlsf::new();
        unsafe { t.grow(a.0.as_mut_ptr(), a.0.len(), true) };
        for size in [usize::MAX, usize::MAX - 15, usize::MAX / 2, 1 << 62, (1 << 48) + 1] {
            assert_eq!(t.alloc(size, 16), Err(OutOfMemory), "size {size:#x}");
            assert_eq!(t.alloc(size, 4096), Err(OutOfMemory), "size {size:#x}, aligned");
            assert_eq!(t.alloc_zeroed(size, 64), Err(OutOfMemory), "size {size:#x}, zeroed");
        }
        assert_eq!(t.alloc(16, 1 << 62), Err(OutOfMemory));
        assert_eq!(t.alloc(16, 48), Err(OutOfMemory));
        let small = t.alloc(16, 16).unwrap();
        assert!(!unsafe { t.resize_in_place(small, usize::MAX) });
        t.check().unwrap();
    }

    #[test]
    fn check_finds_corruption() {
        let mut a = arena();
        let mut t = Tlsf::new();
        unsafe { t.grow(a.0.as_mut_ptr(), a.0.len(), true) };
        let p = t.alloc(100, 16).unwrap();
        let q = t.alloc(100, 16).unwrap();
        unsafe { t.free(p).unwrap() };
        t.check().unwrap();
        // The used count.
        t.used += 16;
        assert!(t.check().is_err());
        t.used -= 16;
        // A bitmap.
        t.sl_bitmap[3] ^= 1 << 7;
        assert!(t.check().is_err());
        t.sl_bitmap[3] ^= 1 << 7;
        t.fl_bitmap ^= 1 << 20;
        assert!(t.check().is_err());
        t.fl_bitmap ^= 1 << 20;
        // A free block taken out of its list.
        let freed = unsafe { p.sub(HEADER) } as *mut Block;
        unsafe { t.remove(freed) };
        assert!(t.check().is_err());
        unsafe { t.insert(freed) };
        t.check().unwrap();
        // Dirt in a zeroed block.
        let rest = unsafe { Block::next_phys(q.sub(HEADER) as *mut Block) };
        assert!(unsafe { (*rest).is_zeroed() });
        let byte = unsafe { Block::payload(rest).add(LINKS + 5) };
        unsafe { *byte = 1 };
        assert!(t.check().is_err());
        unsafe { *byte = 0 };
        // A header's size.
        let header = unsafe { q.sub(HEADER) } as *mut Block;
        unsafe { (*header).size_flags += 16 };
        assert!(t.check().is_err());
        unsafe { (*header).size_flags -= 16 };
        t.check().unwrap();
    }

    #[test]
    fn zeroed_allocations_read_as_zeros() {
        let mut a = arena();
        // Memory the allocator is told nothing about.
        a.0.fill(0xa5);
        let mut t = Tlsf::new();
        unsafe { t.grow(a.0.as_mut_ptr(), a.0.len() / 2, false) };
        let zeros = |p: *mut u8, n: usize| unsafe { core::slice::from_raw_parts(p, n) }.iter().all(|&b| b == 0);
        let p = t.alloc_zeroed(1000, 16).unwrap();
        assert!(zeros(p, 1000));
        // Dirty, freed, then reused.
        unsafe { core::ptr::write_bytes(p, 0x5a, 1000) };
        unsafe { t.free(p).unwrap() };
        let q = t.alloc_zeroed(3000, 64).unwrap();
        assert!(zeros(q, 3000));
        // Fresh memory, merged with the dirty end of the region.
        let half = a.0.len() / 2;
        a.0[half..].fill(0);
        unsafe { t.grow(t.end_of_region(), half, true) };
        t.check().unwrap();
        let big = t.alloc_zeroed(200_000, 16).unwrap();
        assert!(zeros(big, 200_000));
        let aligned = t.alloc_zeroed(5000, 4096).unwrap();
        assert!(zeros(aligned, 5000));
        t.check().unwrap();
    }

    #[test]
    fn zeroed_memory_is_not_cleared_again() {
        // Memory said to be zero (it is not, so that what a zeroed
        // allocation clears shows): only the free-list links are cleared,
        // and nothing of a block carved past them.
        let mut a = arena();
        a.0.fill(0xa5);
        let mut t = Tlsf::new();
        unsafe { t.grow(a.0.as_mut_ptr(), a.0.len(), true) };
        let p = t.alloc_zeroed(1000, 16).unwrap();
        let bytes = unsafe { core::slice::from_raw_parts(p, 1000) };
        assert!(bytes[..LINKS].iter().all(|&b| b == 0) && bytes[LINKS..].iter().all(|&b| b == 0xa5));
        let q = t.alloc_zeroed(1000, 4096).unwrap();
        assert!(unsafe { core::slice::from_raw_parts(q, 1000) }.iter().all(|&b| b == 0xa5));
    }

    #[test]
    fn idle_blocks_are_released_after_a_whole_interval() {
        // A region the six blocks fill exactly: 100 (as 112), 200000, 112,
        // 112, 300000 and 112 bytes, each with its header, and the sentinel.
        let mut a = arena();
        let mut t = Tlsf::new();
        unsafe { t.grow(a.0.as_mut_ptr(), 6 * HEADER + 4 * 112 + 500_000 + HEADER, false) };
        let keep = t.alloc(100, 16).unwrap();
        let touched = t.alloc(200_000, 16).unwrap();
        let neighbour = t.alloc(100, 16).unwrap();
        let apart = t.alloc(100, 16).unwrap();
        let idle = t.alloc(300_000, 16).unwrap();
        let guard = t.alloc(100, 16).unwrap();
        unsafe {
            core::ptr::write_bytes(touched, 3, 200_000);
            core::ptr::write_bytes(idle, 7, 300_000);
            t.free(touched).unwrap();
            t.free(idle).unwrap();
        }
        // A pass that gives memory back as the system would: the pages read
        // as zeros afterwards. Returns what it gave back.
        fn pass(t: &mut Tlsf) -> Vec<(usize, usize)> {
            let mut released = Vec::new();
            t.release_idle(4096, 64 << 10, |p, len| {
                unsafe { core::ptr::write_bytes(p, 0, len) };
                released.push((p as usize, len));
                true
            });
            released
        }
        let within = |(at, len): (usize, usize), block: *mut u8, size: usize| {
            at >= block as usize + LINKS && at + len <= block as usize + size
        };
        // The first pass only marks them.
        assert!(pass(&mut t).is_empty());
        // One changes in between (its neighbour is freed into it): it is
        // new again, and only the other is given back.
        unsafe { t.free(neighbour).unwrap() };
        let released = pass(&mut t);
        assert_eq!(released.len(), 1);
        let (at, len) = released[0];
        assert!(at.is_multiple_of(4096) && len.is_multiple_of(4096) && len >= 290_000);
        assert!(within(released[0], idle, 300_000));
        t.check().unwrap();
        // The changed one goes a pass later; the released one, zeroed now,
        // is not offered again.
        let released = pass(&mut t);
        assert_eq!(released.len(), 1);
        assert!(within(released[0], touched, 200_000 + HEADER + 112));
        assert!(pass(&mut t).is_empty());
        assert!(pass(&mut t).is_empty());
        t.check().unwrap();
        let z = t.alloc_zeroed(300_000, 16).unwrap();
        assert_eq!(z, idle);
        assert!(unsafe { core::slice::from_raw_parts(z, 300_000) }.iter().all(|&b| b == 0));
        unsafe {
            t.free(z).unwrap();
            t.free(keep).unwrap();
            t.free(apart).unwrap();
            t.free(guard).unwrap();
        }
        t.check().unwrap();
        assert_eq!(t.used_bytes(), 0);
    }

    #[test]
    fn a_failed_release_keeps_the_block_as_it_was() {
        // A region that holds the two blocks exactly.
        let mut a = arena();
        let mut t = Tlsf::new();
        unsafe { t.grow(a.0.as_mut_ptr(), 2 * HEADER + 500_000 + 16 + HEADER, false) };
        let p = t.alloc(500_000, 16).unwrap();
        let guard = t.alloc(16, 16).unwrap();
        unsafe { core::ptr::write_bytes(p, 9, 500_000) };
        unsafe { t.free(p).unwrap() };
        let mut offered = 0;
        for _ in 0..3 {
            t.release_idle(4096, 4096, |_, _| {
                offered += 1;
                false
            });
        }
        // Offered on every pass once idle, and never taken for zeroed: a
        // zeroed allocation clears it.
        assert_eq!(offered, 2);
        let z = t.alloc_zeroed(500_000, 16).unwrap();
        assert_eq!(z, p);
        assert!(unsafe { core::slice::from_raw_parts(z, 500_000) }.iter().all(|&b| b == 0));
        unsafe { t.free(guard).unwrap() };
        t.check().unwrap();
    }

    #[test]
    fn randomized_stress() {
        // The region starts small and grows when an allocation does not
        // fit, as the runtime's heap does.
        const STEP: usize = 64 << 10;
        let mut a = arena();
        let base = a.0.as_mut_ptr();
        let mut t = Tlsf::new();
        unsafe { t.grow(base, STEP, true) };
        let mut grown = STEP;
        // Each allocation, its size, and the byte it is filled with.
        let mut live: Vec<(*mut u8, usize, u8)> = Vec::new();
        let mut rng = Rng(0x1234_5678);
        let mut fill = 0u8;
        let filled = |p: *mut u8, n: usize, v: u8| unsafe { core::slice::from_raw_parts(p, n) }.iter().all(|&b| b == v);
        // What happened, to make sure every path was taken.
        let (mut resized, mut zeroed_allocs, mut full, mut released) = (0, 0, 0, 0);
        for step in 0..60_000 {
            fill = fill.wrapping_add(1).max(1);
            // Phases that fill the heap and phases that mostly empty it, so
            // that free blocks both get reused and stay idle.
            let frees = if (step / 6000) % 2 == 0 { 3 } else { 7 };
            match rng.below(10) {
                // Free one.
                r if r < frees && !live.is_empty() => {
                    let (p, n, v) = live.swap_remove(rng.below(live.len()));
                    assert!(filled(p, n, v), "an allocation was overwritten (step {step})");
                    unsafe { t.free(p).unwrap() };
                }
                // Resize one in place (or not), keeping what fits.
                r if r == frees && !live.is_empty() => {
                    let i = rng.below(live.len());
                    let (p, n, v) = live[i];
                    let m = rng.below(6000) + 1;
                    if unsafe { t.resize_in_place(p, m) } {
                        assert!(filled(p, n.min(m), v), "a resize lost the contents (step {step})");
                        unsafe { core::ptr::write_bytes(p, v, m) };
                        live[i].1 = m;
                        resized += 1;
                    }
                }
                // Allocate, aligned or zeroed sometimes; grow when full.
                _ => {
                    let large = rng.below(20) == 0;
                    let n = rng.below(if large { 60_000 } else { 4000 }) + 1;
                    let align = 1 << rng.below(13);
                    let zeroed = rng.below(4) == 0;
                    let p = loop {
                        let r = if zeroed { t.alloc_zeroed(n, align) } else { t.alloc(n, align) };
                        match r {
                            Ok(p) => break Some(p),
                            Err(OutOfMemory) if grown + STEP <= a.0.len() => {
                                unsafe { t.grow(t.end_of_region(), STEP, true) };
                                grown += STEP;
                            }
                            Err(OutOfMemory) => break None,
                        }
                    };
                    if p.is_none() {
                        full += 1;
                    }
                    if let Some(p) = p {
                        assert_eq!(p as usize % align.max(16), 0);
                        assert!(unsafe { Tlsf::usable_size(p) } >= n);
                        if zeroed {
                            assert!(filled(p, n, 0), "a zeroed allocation is not zero (step {step})");
                            zeroed_allocs += 1;
                        }
                        unsafe { core::ptr::write_bytes(p, fill, n) };
                        live.push((p, n, fill));
                    }
                }
            }
            if step % 499 == 0 {
                // Give back what stayed free, as the runtime does.
                t.release_idle(4096, 8192, |p, len| {
                    unsafe { core::ptr::write_bytes(p, 0, len) };
                    released += len;
                    true
                });
                t.check().unwrap();
                for &(p, n, v) in &live {
                    assert!(filled(p, n, v), "an allocation was overwritten (step {step})");
                }
            }
        }
        assert_eq!(t.end_of_region() as usize, base as usize + grown);
        assert!(grown > STEP, "the region never grew");
        assert!(resized > 0 && zeroed_allocs > 0 && full > 0 && released > 0);
        t.check().unwrap();
        for (p, n, v) in live {
            assert!(filled(p, n, v));
            unsafe { t.free(p).unwrap() };
        }
        assert_eq!(t.used_bytes(), 0);
        t.check().unwrap();
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The global allocator.
//!
//! Small and medium allocations come from a TLSF allocator (`heap`) over a
//! large reserved region of private memory (`vm::allocate`), whose pages the
//! kernel provides on first touch; the TLSF region grows through it as
//! needed. Allocations of 4 MiB and more get a private mapping each, so
//! freeing them returns the memory to the system immediately; the heap
//! keeps a record of them, and a free of one it has no record of (one
//! freed twice) ends the program, as TLSF's own checks do for the rest.
//!
//! Freed memory goes back to the system too, without the cost of giving
//! back what is soon used again: after every 16 MiB freed
//! (`RELEASE_INTERVAL`), the whole pages of free blocks that have stayed
//! free since the previous such pass are decommitted (`vm::decommit`; they
//! read as zeros afterwards, which the allocator remembers). Memory the
//! system provides is zero, so a zeroed allocation clears only what was
//! used before.
//!
//! The heap's lock is a [`Mutex`]: no system call when uncontended, and a
//! thread that waits for it sleeps rather than spinning, so a preempted
//! holder of lower priority runs again.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;

use abi::map_flags;
use heap::Tlsf;

use crate::sync::Mutex;
use crate::vm;

/// Address space reserved for the heap (committed lazily).
const HEAP_RESERVE: usize = 32 << 30;
/// Granularity of region growth.
const GROW_STEP: usize = 2 << 20;
/// Allocations at least this large get their own mapping.
const LARGE_THRESHOLD: usize = 4 << 20;
/// Bytes freed between two passes that give idle free memory back.
const RELEASE_INTERVAL: usize = 16 << 20;
/// The smallest free block whose pages a pass gives back.
const RELEASE_MIN: usize = 64 << 10;

struct HeapState {
    tlsf: Tlsf,
    base: usize,
    reserved_end: usize,
    /// Bytes freed since the last pass over idle free blocks.
    freed: usize,
    large: Large,
}

/// One large allocation in use: its address, and its size in whole pages.
type LargeEntry = (usize, usize);

/// The large allocations in use, by address: a sorted array in memory of
/// its own, which the heap does not give (taking it from the heap would
/// take the heap's lock again). A free of a large allocation not recorded
/// here is refused: unmapping it would take the memory of whatever was
/// mapped there since it was freed.
struct Large {
    /// Where the array is (0: nowhere yet), how many entries it has room
    /// for, and how many are in use.
    at: usize,
    cap: usize,
    len: usize,
}

impl Large {
    const fn new() -> Large {
        Large { at: 0, cap: 0, len: 0 }
    }

    fn entries(&self) -> &[LargeEntry] {
        if self.at == 0 {
            return &[];
        }
        // SAFETY: `at` holds `cap` entries, of which the first `len` are
        // written; nothing else reaches the array.
        unsafe { core::slice::from_raw_parts(self.at as *const LargeEntry, self.len) }
    }

    /// Records the allocation of `size` bytes at `addr`; false if there is
    /// no memory for the record. `map(bytes)` gives memory for a larger
    /// array (zero-filled, aligned for its entries) and `unmap(at, bytes)`
    /// takes the one it replaces back.
    fn insert(
        &mut self,
        addr: usize,
        size: usize,
        map: impl FnOnce(usize) -> Option<usize>,
        unmap: impl FnOnce(usize, usize),
    ) -> bool {
        const ENTRY: usize = core::mem::size_of::<LargeEntry>();
        if self.len == self.cap {
            let cap = (self.cap * 2).max(abi::PAGE_SIZE / ENTRY);
            let Some(at) = cap.checked_mul(ENTRY).and_then(map) else { return false };
            if self.at != 0 {
                // SAFETY: both arrays hold at least `len` entries, and
                // they are different memory.
                unsafe {
                    core::ptr::copy_nonoverlapping(self.at as *const LargeEntry, at as *mut LargeEntry, self.len)
                };
                unmap(self.at, self.cap * ENTRY);
            }
            (self.at, self.cap) = (at, cap);
        }
        let i = self.entries().partition_point(|&(a, _)| a < addr);
        let p = self.at as *mut LargeEntry;
        // SAFETY: the array has room for `len + 1` entries: the ones from
        // `i` move up one within it, and the new one goes at `i`.
        unsafe {
            core::ptr::copy(p.add(i), p.add(i + 1), self.len - i);
            p.add(i).write((addr, size));
        }
        self.len += 1;
        true
    }

    /// Takes the record of the allocation of `size` bytes at `addr`:
    /// false if there is none.
    fn remove(&mut self, addr: usize, size: usize) -> bool {
        let Ok(i) = self.entries().binary_search_by_key(&addr, |&(a, _)| a) else { return false };
        if self.entries()[i].1 != size {
            return false;
        }
        let p = self.at as *mut LargeEntry;
        // SAFETY: the entries after `i` move down one, within the array.
        unsafe { core::ptr::copy(p.add(i + 1), p.add(i), self.len - i - 1) };
        self.len -= 1;
        true
    }
}

/// The allocator: one per program, as its `#[global_allocator]`.
pub struct Heap {
    state: Mutex<HeapState>,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap {
    /// A heap with no memory yet: it reserves its region on first use.
    pub const fn new() -> Heap {
        Heap {
            state: Mutex::new(HeapState { tlsf: Tlsf::new(), base: 0, reserved_end: 0, freed: 0, large: Large::new() }),
        }
    }

    fn reserve(state: &mut HeapState) -> bool {
        // Private memory: no handle reaches it, so decommitting its pages
        // frees them.
        let Ok(addr) = vm::allocate(None, HEAP_RESERVE, 0, map_flags::READ | map_flags::WRITE) else { return false };
        state.base = addr;
        state.reserved_end = addr + HEAP_RESERVE;
        true
    }

    fn grow(state: &mut HeapState, at_least: usize) -> bool {
        if state.base == 0 && !Self::reserve(state) {
            return false;
        }
        let start = if state.tlsf.total_bytes() == 0 { state.base } else { state.tlsf.end_of_region() as usize };
        let Some(len) = at_least.checked_add(64).and_then(|n| n.checked_next_multiple_of(GROW_STEP)) else {
            return false;
        };
        if len > state.reserved_end - start {
            return false;
        }
        // SAFETY: a fresh, exclusively owned part of the reserved region,
        // never touched: the kernel provides it zero-filled.
        unsafe { state.tlsf.grow(start as *mut u8, len, true) };
        true
    }

    /// Allocates from the TLSF region (zero-filled if `zeroed`), growing it
    /// as needed.
    fn alloc_small(&self, layout: Layout, zeroed: bool) -> *mut u8 {
        let mut s = self.state.lock();
        loop {
            let r = if zeroed {
                s.tlsf.alloc_zeroed(layout.size(), layout.align())
            } else {
                s.tlsf.alloc(layout.size(), layout.align())
            };
            if let Ok(p) = r {
                return p;
            }
            let Some(need) = layout.size().checked_add(layout.align()) else { return null_mut() };
            if !Heap::grow(&mut s, need) {
                return null_mut();
            }
        }
    }
}

/// Whether an allocation of `layout` gets a mapping of its own.
fn is_large(layout: Layout) -> bool {
    layout.size() >= LARGE_THRESHOLD && layout.align() <= abi::PAGE_SIZE
}

/// The bytes a large allocation of `layout` takes: whole pages.
fn large_size(layout: Layout) -> Option<usize> {
    layout.size().checked_next_multiple_of(abi::PAGE_SIZE)
}

/// Private memory, zero-filled, its pages provided as they are touched.
fn map_private(bytes: usize) -> Option<usize> {
    vm::allocate(None, bytes, 0, map_flags::READ | map_flags::WRITE).ok()
}

impl Heap {
    /// A private mapping of its own for a large allocation, recorded.
    fn large_alloc(&self, layout: Layout) -> *mut u8 {
        let Some(size) = large_size(layout) else { return null_mut() };
        let Some(addr) = map_private(size) else { return null_mut() };
        let unmap = |at: usize, bytes: usize| {
            let _ = vm::unmap(None, at, bytes);
        };
        if !self.state.lock().large.insert(addr, size, map_private, unmap) {
            unmap(addr, size);
            return null_mut();
        }
        addr as *mut u8
    }

    /// Frees a large allocation, if it is one in use.
    fn large_free(&self, ptr: *mut u8, layout: Layout) {
        let size = large_size(layout);
        if !size.is_some_and(|size| self.state.lock().large.remove(ptr as usize, size)) {
            invalid_free(ptr);
        }
        let _ = vm::unmap(None, ptr as usize, size.unwrap_or(0));
    }
}

/// Ends the program after a free of something that is not an allocation in
/// use: the heap would be corrupted from then on, and a double free is
/// what an attack on it starts with.
#[cold]
fn invalid_free(ptr: *mut u8) -> ! {
    crate::io::log_without_allocating(format_args!("heap: {ptr:p} is not an allocation in use (freed twice?)\n"));
    crate::sys::process_exit(abi::EXIT_CODE_PANICKED)
}

// SAFETY: TLSF hands out disjoint, correctly aligned blocks (zero-filled
// for `alloc_zeroed`); large blocks are page-aligned private mappings, zero
// when made.
unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if is_large(layout) { self.large_alloc(layout) } else { self.alloc_small(layout, false) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // Fresh pages are zero already: nothing is written to them here.
        if is_large(layout) { self.large_alloc(layout) } else { self.alloc_small(layout, true) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if is_large(layout) {
            return self.large_free(ptr, layout);
        }
        let mut s = self.state.lock();
        // SAFETY: `ptr` came from this TLSF instance, which refuses it
        // (changing nothing) if it is not an allocation in use.
        if unsafe { s.tlsf.free(ptr) }.is_err() {
            drop(s);
            invalid_free(ptr);
        }
        s.freed += layout.size();
        if s.freed >= RELEASE_INTERVAL {
            s.freed = 0;
            s.tlsf.release_idle(abi::PAGE_SIZE, RELEASE_MIN, |p, len| vm::decommit(None, p as usize, len).is_ok());
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller guarantees that `new_size`, rounded up to the
        // alignment, does not overflow `isize`.
        let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        if !is_large(layout) && !is_large(new_layout) {
            // SAFETY: `ptr` is a live TLSF allocation; resizing in place
            // keeps its address, so its alignment.
            if unsafe { self.state.lock().tlsf.resize_in_place(ptr, new_size) } {
                return ptr;
            }
        }
        // SAFETY: standard allocate-copy-free fallback.
        unsafe {
            let new = self.alloc(new_layout);
            if !new.is_null() {
                core::ptr::copy_nonoverlapping(ptr, new, layout.size().min(new_size));
                self.dealloc(ptr, layout);
            }
            new
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{alloc_zeroed, dealloc};
    use std::collections::BTreeSet;
    use std::vec::Vec;

    fn layout(bytes: usize) -> Layout {
        Layout::from_size_align(bytes, 16).unwrap()
    }

    /// Memory for the record from the host's allocator, as the heap maps
    /// it in Veda; the arrays given back are counted.
    fn map(bytes: usize) -> Option<usize> {
        // SAFETY: a layout of non-zero size.
        Some(unsafe { alloc_zeroed(layout(bytes)) } as usize)
    }

    #[test]
    fn large_allocations_are_freed_once() {
        let mut large = Large::new();
        let mut gone = Vec::new();
        let page = abi::PAGE_SIZE;
        // More than the first array holds, in no order.
        let mut addrs: Vec<usize> = (0..1000usize).map(|i| (i * 7919 % 1000 + 1) * (8 << 20)).collect();
        for &a in &addrs {
            assert!(large.insert(a, 4 << 20, map, |at, bytes| gone.push((at, bytes))));
        }
        assert!(large.entries().windows(2).all(|w| w[0].0 < w[1].0), "kept in order");
        assert!(!gone.is_empty(), "the arrays it outgrew are given back");
        // Freed with another size, or where nothing is: refused.
        assert!(!large.remove(addrs[0], (4 << 20) + page));
        assert!(!large.remove(addrs[0] + page, 4 << 20));
        addrs.sort_unstable();
        let mut freed = BTreeSet::new();
        for (n, &a) in addrs.iter().enumerate() {
            if n % 3 == 0 {
                assert!(large.remove(a, 4 << 20));
                freed.insert(a);
            }
        }
        // Freed twice: refused; the others are still there.
        for &a in &addrs {
            assert_eq!(large.remove(a, 4 << 20), !freed.contains(&a), "{a:#x}");
        }
        assert!(large.entries().is_empty());
        // SAFETY: the arrays came from `map` with these sizes.
        unsafe {
            for (at, bytes) in gone {
                dealloc(at as *mut u8, layout(bytes));
            }
            dealloc(large.at as *mut u8, layout(large.cap * core::mem::size_of::<LargeEntry>()));
        }
    }

    #[test]
    fn no_memory_for_the_record_is_no_allocation() {
        let mut large = Large::new();
        assert!(!large.insert(8 << 20, 4 << 20, |_| None, |_, _| {}));
        assert!(!large.remove(8 << 20, 4 << 20));
    }
}

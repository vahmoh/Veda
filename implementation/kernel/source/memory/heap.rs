// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The kernel heap (`GlobalAlloc`).
//!
//! Small allocations (up to 2 KiB) are blocks of slabs, direct-mapped 4 KiB
//! frames each carved into blocks of one size (`slabs`). A slab whose
//! blocks are all free goes back to the frame allocator, but for one of
//! each size, kept for the next allocations: a burst of them, such as the
//! page maps of a hundred processes `process_fork` made, leaves its memory
//! to programs once it is over. Larger allocations take physically
//! contiguous frames directly, zeroed only when asked for zeroed memory
//! (`alloc_zeroed`): what is filled at once (a message's bytes, a socket's
//! buffer growing) is not written twice. All memory is addressed through
//! the direct map, so no page-table work is needed.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{NonNull, null_mut};

use slabs::Slabs;

use super::{PAGE_SIZE, phys, phys_to_virt, virt_to_phys};
use crate::synchronization::SpinLock;

// A slab is a frame.
const _: () = assert!(slabs::PAGE as u64 == PAGE_SIZE);

struct Heap {
    /// The slabs, made with the first small allocation: their table, a word
    /// for each MiB of memory, needs the frame allocator.
    slabs: Option<Slabs<'static>>,
    /// Bytes currently allocated (for diagnostics).
    in_use: usize,
}

pub struct KernelHeap {
    heap: SpinLock<Heap>,
}

/// The frame allocator, through the direct map: where slabs come from, and
/// go back to.
struct Frames;

impl slabs::Pages for Frames {
    fn take(&mut self) -> Option<NonNull<u8>> {
        NonNull::new(phys_to_virt(phys::alloc()?) as *mut u8)
    }

    fn give(&mut self, page: NonNull<u8>) {
        phys::free(virt_to_phys(page.as_ptr() as u64));
    }
}

/// Makes `slabs` slabs among all the frames of memory, unless there is no
/// memory for their table.
#[cold]
#[inline(never)]
fn make_slabs(slabs: &mut Option<Slabs<'static>>) {
    let words = Slabs::table_len(phys::frame_count() as usize);
    let frames = (words * size_of::<usize>()).div_ceil(PAGE_SIZE as usize).max(1);
    let Some(table) = phys::alloc_contiguous(frames as u64, PAGE_SIZE, u64::MAX) else { return };
    // SAFETY: fresh, zeroed, direct-mapped memory that nothing else
    // reaches, as long as the kernel runs.
    let table = unsafe { core::slice::from_raw_parts_mut(phys_to_virt(table) as *mut usize, words) };
    *slabs = Some(Slabs::new(phys_to_virt(0) as usize, table));
}

impl KernelHeap {
    pub const fn new() -> Self {
        KernelHeap { heap: SpinLock::new(Heap { slabs: None, in_use: 0 }) }
    }
}

// SAFETY: blocks handed out are disjoint and properly aligned (class sizes
// are powers of two, slab pages are page aligned, large allocations are
// page aligned).
unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if let Some(class) = slabs::class(layout.size(), layout.align()) {
            let mut heap = self.heap.lock();
            let heap = &mut *heap;
            if heap.slabs.is_none() {
                make_slabs(&mut heap.slabs);
            }
            let Some(block) = heap.slabs.as_mut().and_then(|s| s.alloc(class, &mut Frames)) else {
                return null_mut();
            };
            heap.in_use += slabs::CLASSES[class];
            return block.as_ptr();
        }
        let pages = (layout.size() as u64).div_ceil(PAGE_SIZE);
        match phys::alloc_contiguous_unzeroed(pages, (layout.align() as u64).max(PAGE_SIZE), u64::MAX) {
            Some(p) => {
                self.heap.lock().in_use += (pages * PAGE_SIZE) as usize;
                phys_to_virt(p) as *mut u8
            }
            None => null_mut(),
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if slabs::class(layout.size(), layout.align()).is_some() {
            // SAFETY: as `alloc`; a slab's block is zeroed here.
            let block = unsafe { self.alloc(layout) };
            if !block.is_null() {
                // SAFETY: the block holds `layout.size()` bytes.
                unsafe { core::ptr::write_bytes(block, 0, layout.size()) };
            }
            return block;
        }
        let pages = (layout.size() as u64).div_ceil(PAGE_SIZE);
        match phys::alloc_contiguous(pages, (layout.align() as u64).max(PAGE_SIZE), u64::MAX) {
            Some(p) => {
                self.heap.lock().in_use += (pages * PAGE_SIZE) as usize;
                phys_to_virt(p) as *mut u8
            }
            None => null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(class) = slabs::class(layout.size(), layout.align()) {
            let mut heap = self.heap.lock();
            let heap = &mut *heap;
            let made = heap.slabs.as_mut().expect("a block of the heap's slabs, which are made");
            // SAFETY: `ptr` was allocated from this class and is now free.
            unsafe { made.free(NonNull::new_unchecked(ptr), class, &mut Frames) };
            heap.in_use -= slabs::CLASSES[class];
            return;
        }
        let pages = (layout.size() as u64).div_ceil(PAGE_SIZE);
        phys::free_contiguous(virt_to_phys(ptr as u64), pages);
        self.heap.lock().in_use -= (pages * PAGE_SIZE) as usize;
    }
}

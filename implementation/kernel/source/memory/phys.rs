// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Physical page frame allocator.
//!
//! A bitmap with one bit per 4 KiB frame (1 = in use) covering all RAM. A
//! rolling search hint makes single-frame allocation fast in the common case;
//! contiguous allocations (DMA buffers, large kernel objects) look for the
//! lowest run, a word of the bitmap at a time (`frames`).
//!
//! It also remembers where the RAM is that the kernel owns (free RAM, its
//! own image, the boot data, the loader's memory it takes back): physical
//! VMOs, which drivers map devices' memory with, may not reach it.
//!
//! Memory that user space asks for (its VMOs' pages, DMA buffers, the
//! kernel buffers that carry its data) leaves a reserve of frames free for
//! the kernel's own needs (its heap, page tables, stacks): a process that
//! takes all the memory it can is refused more, and a page fault it cannot
//! be given ends it, before the kernel itself runs short, which would
//! stop the system.

use bootinfo::{MemoryKind, MemoryRegion};

use super::{PAGE_SIZE, phys_to_virt};
use crate::synchronization::SpinLock;

struct Bitmap {
    words: &'static mut [u64],
    frames: u64,
    free: u64,
    total_usable: u64,
    hint: usize,
}

static ALLOCATOR: SpinLock<Option<Bitmap>> = SpinLock::new(None);

/// The kernel's RAM, as ranges of physical addresses (adjacent regions
/// merged); past the last slot, the last range grows to cover the rest (more
/// is taken for RAM, never less).
static RAM: SpinLock<([(u64, u64); 64], usize)> = SpinLock::new(([(0, 0); 64], 0));

/// The kinds of the memory map's regions that are the kernel's RAM.
fn is_kernel_ram(kind: MemoryKind) -> bool {
    matches!(
        kind,
        MemoryKind::Usable
            | MemoryKind::LoaderReclaimable
            | MemoryKind::Kernel
            | MemoryKind::Initrd
            | MemoryKind::BootData
            | MemoryKind::FirmwareVariables
    )
}

/// Whether `[base, base + size)` reaches the kernel's RAM.
pub fn overlaps_ram(base: u64, size: u64) -> bool {
    let end = base.saturating_add(size);
    let ram = RAM.lock();
    ram.0[..ram.1].iter().any(|&(s, e)| base < e && s < end)
}

fn remember_ram(map: &[MemoryRegion]) {
    let mut regions: [(u64, u64); 256] = [(0, 0); 256];
    let mut n = 0;
    for r in map.iter().filter(|r| is_kernel_ram(r.kind)) {
        if n == regions.len() {
            // Even more regions: the last takes them in.
            regions[n - 1].1 = regions[n - 1].1.max(r.end());
            continue;
        }
        regions[n] = (r.base, r.end());
        n += 1;
    }
    regions[..n].sort_unstable();
    let mut ram = RAM.lock();
    let (slots, used) = &mut *ram;
    for &(s, e) in &regions[..n] {
        match *used {
            u if u > 0 && s <= slots[u - 1].1 => slots[u - 1].1 = slots[u - 1].1.max(e),
            u if u < slots.len() => {
                slots[u] = (s, e);
                *used += 1;
            }
            u => slots[u - 1].1 = slots[u - 1].1.max(e),
        }
    }
}

/// Physical memory below this address is never handed out: the
/// architecture's.
const LOW_RESERVED: u64 = crate::architecture::memory::RESERVED_BELOW;

impl Bitmap {
    fn set(&mut self, frame: u64, used: bool) {
        let (w, b) = ((frame / 64) as usize, frame % 64);
        let was = self.words[w] & (1 << b) != 0;
        if used && !was {
            self.words[w] |= 1 << b;
            self.free -= 1;
        } else if !used && was {
            self.words[w] &= !(1 << b);
            self.free += 1;
        }
    }

    fn is_used(&self, frame: u64) -> bool {
        self.words[(frame / 64) as usize] & (1 << (frame % 64)) != 0
    }

    fn alloc_one(&mut self) -> Option<u64> {
        let n = self.words.len();
        for i in 0..n {
            let w = (self.hint + i) % n;
            if self.words[w] != u64::MAX {
                let bit = (!self.words[w]).trailing_zeros() as u64;
                let frame = w as u64 * 64 + bit;
                if frame >= self.frames {
                    continue;
                }
                self.set(frame, true);
                self.hint = w;
                return Some(frame);
            }
        }
        None
    }

    fn alloc_run(&mut self, count: u64, align: u64, max_frame: u64) -> Option<u64> {
        let limit = self.frames.min(max_frame);
        let start = frames::find_run(self.words, count, align, limit)?;
        for f in start..start + count {
            self.set(f, true);
        }
        Some(start)
    }
}

/// Initialises the allocator from the boot memory map. Only `Usable` RAM is
/// made available now; boot-time regions are reclaimed later.
pub fn init(map: &[MemoryRegion]) {
    remember_ram(map);
    let top = map
        .iter()
        .filter(|r| {
            matches!(
                r.kind,
                MemoryKind::Usable | MemoryKind::LoaderReclaimable | MemoryKind::BootData | MemoryKind::AcpiReclaimable
            )
        })
        .map(|r| r.end())
        .max()
        .unwrap_or(0);
    let frames = top / PAGE_SIZE;
    let words = frames.div_ceil(64) as usize;
    let bytes = (words * 8) as u64;

    // Place the bitmap in the first usable region large enough to hold it.
    let region = map
        .iter()
        .find(|r| r.kind == MemoryKind::Usable && r.base >= LOW_RESERVED && r.pages * PAGE_SIZE >= bytes)
        .expect("no usable memory region can hold the frame bitmap");
    let bitmap_phys = region.base;
    // SAFETY: the region is usable RAM, mapped by the direct map, and not yet
    // handed out to anyone.
    let words_slice = unsafe { core::slice::from_raw_parts_mut(phys_to_virt(bitmap_phys) as *mut u64, words) };
    words_slice.fill(u64::MAX);

    let mut bm = Bitmap { words: words_slice, frames, free: 0, total_usable: 0, hint: 0 };
    for r in map.iter().filter(|r| r.kind == MemoryKind::Usable) {
        for f in r.base / PAGE_SIZE..r.end() / PAGE_SIZE {
            if f * PAGE_SIZE >= LOW_RESERVED {
                bm.set(f, false);
                bm.total_usable += 1;
            }
        }
    }
    for f in bitmap_phys / PAGE_SIZE..(bitmap_phys + bytes).div_ceil(PAGE_SIZE) {
        bm.set(f, true);
    }
    *ALLOCATOR.lock() = Some(bm);
}

/// Returns boot-time memory of `kind` to the allocator.
pub fn reclaim(map: &[MemoryRegion], kind: MemoryKind) -> u64 {
    let mut guard = ALLOCATOR.lock();
    let bm = guard.as_mut().expect("frame allocator not initialised");
    let mut n = 0;
    for r in map.iter().filter(|r| r.kind == kind) {
        for f in r.base / PAGE_SIZE..r.end() / PAGE_SIZE {
            if f * PAGE_SIZE >= LOW_RESERVED && f < bm.frames && bm.is_used(f) {
                bm.set(f, false);
                bm.total_usable += 1;
                n += 1;
            }
        }
    }
    n * PAGE_SIZE
}

impl Bitmap {
    /// Frames that allocations for user space leave free: 1/64 of RAM, at
    /// least 8 MiB and at most 64 MiB.
    fn reserve(&self) -> u64 {
        (self.total_usable / 64).clamp(2048, 16384)
    }

    /// Whether user space may still have `frames` more frames.
    fn user_may_take(&self, frames: u64) -> bool {
        self.free.saturating_sub(frames) >= self.reserve()
    }
}

/// Allocates one frame (contents undefined).
pub fn alloc() -> Option<u64> {
    ALLOCATOR.lock().as_mut()?.alloc_one().map(|f| f * PAGE_SIZE)
}

/// Whether user space may still have `bytes` of memory: false once that
/// would reach into the kernel's reserve. For memory taken on user space's
/// behalf from the kernel's heap (buffers for its data, the stacks of its
/// threads), which has no tier of its own.
pub fn user_may_use(bytes: u64) -> bool {
    ALLOCATOR.lock().as_ref().is_some_and(|bm| bm.user_may_take(bytes.div_ceil(PAGE_SIZE)))
}

/// Allocates one zero-filled frame for user space's memory (a VMO's page),
/// outside the kernel's reserve.
pub fn alloc_user_zeroed() -> Option<u64> {
    let frame = {
        let mut guard = ALLOCATOR.lock();
        let bm = guard.as_mut()?;
        if !bm.user_may_take(1) {
            return None;
        }
        bm.alloc_one()? * PAGE_SIZE
    };
    zero_frames(frame, 1);
    Some(frame)
}

/// Allocates one zero-filled frame.
pub fn alloc_zeroed() -> Option<u64> {
    let p = alloc()?;
    zero_frames(p, 1);
    Some(p)
}

/// Allocates `count` physically contiguous, zeroed frames aligned to
/// `align` bytes, entirely below `max_addr`.
pub fn alloc_contiguous(count: u64, align: u64, max_addr: u64) -> Option<u64> {
    let f = contiguous(count, align, max_addr, false)?;
    zero_frames(f, count);
    Some(f)
}

/// Like [`alloc_contiguous`], with the frames as they were: for memory its
/// user writes before reading (the kernel heap's allocations).
pub fn alloc_contiguous_unzeroed(count: u64, align: u64, max_addr: u64) -> Option<u64> {
    contiguous(count, align, max_addr, false)
}

/// Like [`alloc_contiguous`], for user space's memory (a DMA buffer):
/// outside the kernel's reserve.
pub fn alloc_user_contiguous(count: u64, align: u64, max_addr: u64) -> Option<u64> {
    let f = contiguous(count, align, max_addr, true)?;
    zero_frames(f, count);
    Some(f)
}

fn contiguous(count: u64, align: u64, max_addr: u64, user: bool) -> Option<u64> {
    let mut guard = ALLOCATOR.lock();
    let bm = guard.as_mut()?;
    if user && !bm.user_may_take(count) {
        return None;
    }
    let f = bm.alloc_run(count, (align / PAGE_SIZE).max(1), max_addr / PAGE_SIZE)?;
    Some(f * PAGE_SIZE)
}

pub fn free(phys: u64) {
    free_contiguous(phys, 1);
}

pub fn free_contiguous(phys: u64, count: u64) {
    let mut guard = ALLOCATOR.lock();
    let bm = guard.as_mut().expect("frame allocator not initialised");
    for f in phys / PAGE_SIZE..phys / PAGE_SIZE + count {
        debug_assert!(bm.is_used(f), "double free of frame {:#x}", f * PAGE_SIZE);
        bm.set(f, false);
    }
}

/// Fills frames with zeroes through the direct map.
pub fn zero_frames(phys: u64, count: u64) {
    crate::architecture::memory::zero(phys_to_virt(phys), count * PAGE_SIZE);
}

/// (total usable bytes, free bytes)
pub fn stats() -> (u64, u64) {
    match ALLOCATOR.lock().as_ref() {
        Some(bm) => (bm.total_usable * PAGE_SIZE, bm.free * PAGE_SIZE),
        None => (0, 0),
    }
}

/// Bytes of memory user space may still take: what is free beyond the
/// kernel's reserve, which no process is given.
pub fn available_to_user() -> u64 {
    ALLOCATOR.lock().as_ref().map_or(0, |bm| bm.free.saturating_sub(bm.reserve()) * PAGE_SIZE)
}

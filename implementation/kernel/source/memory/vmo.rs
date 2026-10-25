// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Virtual memory objects (VMOs): the unit of memory that can be mapped into
//! address spaces and shared between processes through handles.

use alloc::collections::{BTreeMap, LinkedList};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use abi::Error;

use super::paging::Cache;
use super::{PAGE_SIZE, phys, phys_to_virt};
use crate::synchronization::SpinLock;

/// Upper bound on a single VMO: 1 TiB, which keeps page indices and
/// arithmetic sane. A VMO costs only the pages committed to it, so the bound
/// is not about memory: it leaves room for the address space programs
/// reserve and fill as they need, such as a JavaScript engine's cages
/// (WebKit's take about 112 GiB with their alignment).
pub const MAX_VMO_SIZE: u64 = 1 << 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmoKind {
    /// Zero-filled memory whose pages are allocated on first touch.
    Anonymous,
    /// A fixed physical range that the VMO does not own (MMIO, framebuffer,
    /// boot modules).
    Physical { base: u64 },
    /// Physically contiguous memory owned by the VMO (DMA buffers).
    Contiguous { base: u64 },
}

pub struct Vmo {
    pub koid: u64,
    /// Its size, which only grows ([`Vmo::grow`]).
    size: AtomicU64,
    kind: VmoKind,
    cache: Cache,
    /// Committed pages of an anonymous VMO: page index -> frame.
    pages: SpinLock<BTreeMap<u64, u64>>,
    committed: AtomicU64,
}

impl Vmo {
    fn new(size: u64, kind: VmoKind, cache: Cache) -> Arc<Vmo> {
        Arc::new(Vmo {
            koid: crate::object::new_koid(),
            size: AtomicU64::new(size),
            kind,
            cache,
            pages: SpinLock::new(BTreeMap::new()),
            committed: AtomicU64::new(0),
        })
    }

    /// Zero-filled memory, mapped with `cache`.
    pub fn new_anonymous(size: u64, cache: Cache) -> Option<Arc<Vmo>> {
        let size = super::checked_page_align_up(size)?;
        if size == 0 || size > MAX_VMO_SIZE {
            return None;
        }
        Some(Vmo::new(size, VmoKind::Anonymous, cache))
    }

    pub fn new_physical(base: u64, size: u64, cache: Cache) -> Arc<Vmo> {
        Vmo::new(super::page_align_up(size), VmoKind::Physical { base: super::page_align_down(base) }, cache)
    }

    /// A VMO for `size` bytes (rounded up to pages) of this one's memory from
    /// `offset` (page-aligned), mapped with the same cache policy: the same
    /// memory, which neither owns, so that it outlives neither. Only a
    /// physical VMO's: another's pages are its own (`NotSupported`).
    pub fn slice(&self, offset: u64, size: u64) -> Result<Arc<Vmo>, Error> {
        let VmoKind::Physical { base } = self.kind else { return Err(Error::NotSupported) };
        let size = super::checked_page_align_up(size).ok_or(Error::OutOfRange)?;
        if size == 0 || !offset.is_multiple_of(PAGE_SIZE) {
            return Err(Error::InvalidArgs);
        }
        if offset.checked_add(size).is_none_or(|end| end > self.size()) {
            return Err(Error::OutOfRange);
        }
        Ok(Vmo::new(size, VmoKind::Physical { base: base + offset }, self.cache))
    }

    /// Physically contiguous, zeroed memory for DMA, entirely below
    /// `max_addr`, mapped with `cache`.
    pub fn new_contiguous(size: u64, max_addr: u64, cache: Cache) -> Option<Arc<Vmo>> {
        let size = super::checked_page_align_up(size)?;
        if size == 0 || size > (256 << 20) {
            return None;
        }
        let base = phys::alloc_user_contiguous(size / PAGE_SIZE, PAGE_SIZE, max_addr)?;
        let vmo = Vmo::new(size, VmoKind::Contiguous { base }, cache);
        vmo.flush(base, size);
        vmo.committed.store(size, Ordering::Relaxed);
        Some(vmo)
    }

    pub fn size(&self) -> u64 {
        self.size.load(Ordering::Acquire)
    }

    /// Grows an anonymous VMO to `size` bytes (rounded up to pages); its new
    /// pages read as zeros. `false` for a size it cannot have: smaller than
    /// it is (VMOs never shrink, so no mapping covers what one no longer
    /// has), beyond [`MAX_VMO_SIZE`], or for memory of another kind.
    pub fn grow(&self, size: u64) -> bool {
        let Some(size) = super::checked_page_align_up(size) else { return false };
        if self.kind != VmoKind::Anonymous || size > MAX_VMO_SIZE {
            return false;
        }
        self.size.try_update(Ordering::AcqRel, Ordering::Acquire, |now| (size >= now).then_some(size)).is_ok()
    }

    pub fn kind(&self) -> VmoKind {
        self.kind
    }

    pub fn cache(&self) -> Cache {
        self.cache
    }

    pub fn committed_bytes(&self) -> u64 {
        self.committed.load(Ordering::Relaxed)
    }

    /// If the VMO is RAM mapped without the caches: makes memory hold what
    /// the kernel wrote to `len` bytes of it at `phys` through the direct
    /// map (write-back), and no cache keep a line of them. Such a line
    /// would be written back later over what was written to the memory
    /// past the caches, or read in place of that.
    fn flush(&self, phys: u64, len: u64) {
        let ram = !matches!(self.kind, VmoKind::Physical { .. });
        if ram && self.cache != Cache::WriteBack {
            crate::architecture::mm::flush_cache_range(phys_to_virt(phys), len);
        }
    }

    /// Physical address of the page containing `offset`, committing a zeroed
    /// page for anonymous VMOs when `commit` is set. Returns `None` past the
    /// end, or for an uncommitted page when `commit` is false.
    pub fn page(&self, offset: u64, commit: bool) -> Option<u64> {
        if offset >= self.size() {
            return None;
        }
        let page = offset / PAGE_SIZE;
        match self.kind {
            VmoKind::Physical { base } | VmoKind::Contiguous { base } => Some(base + page * PAGE_SIZE),
            VmoKind::Anonymous => {
                let mut pages = self.pages.lock();
                if let Some(&f) = pages.get(&page) {
                    return Some(f);
                }
                if !commit {
                    return None;
                }
                let f = phys::alloc_user_zeroed()?;
                self.flush(f, PAGE_SIZE);
                pages.insert(page, f);
                self.committed.fetch_add(PAGE_SIZE, Ordering::Relaxed);
                Some(f)
            }
        }
    }

    /// Detaches the committed pages of `[offset, offset+len)` of an
    /// anonymous VMO, which reads as zeros there from then on. The frames
    /// are freed when the returned value is dropped: only once no page
    /// table maps them and no TLB remembers them.
    pub fn decommit(&self, offset: u64, len: u64) -> Frames {
        let mut frames = Frames::default();
        if self.kind != VmoKind::Anonymous || len == 0 {
            return frames;
        }
        let first = offset / PAGE_SIZE;
        let end = offset.saturating_add(len).div_ceil(PAGE_SIZE);
        let mut pages = self.pages.lock();
        let mut detached = 0;
        for (_, f) in pages.extract_if(first..end, |_, _| true) {
            frames.push(f);
            detached += 1;
        }
        self.committed.fetch_sub(detached * PAGE_SIZE, Ordering::Relaxed);
        frames
    }

    /// Physical address for DMA (contiguous and physical VMOs only).
    pub fn phys_addr(&self, offset: u64) -> Option<u64> {
        match self.kind {
            VmoKind::Physical { base } | VmoKind::Contiguous { base } if offset < self.size() => Some(base + offset),
            _ => None,
        }
    }

    /// Copies VMO contents into `out`; uncommitted pages read as zero.
    pub fn read(&self, offset: u64, out: &mut [u8]) -> bool {
        if offset.checked_add(out.len() as u64).is_none_or(|end| end > self.size()) {
            return false;
        }
        let mut done = 0usize;
        while done < out.len() {
            let pos = offset + done as u64;
            let in_page = (pos % PAGE_SIZE) as usize;
            let n = (PAGE_SIZE as usize - in_page).min(out.len() - done);
            match self.page(pos, false) {
                Some(f) => {
                    let at = f + in_page as u64;
                    self.flush(at, n as u64);
                    // SAFETY: the frame is direct-mapped and `n` stays in the
                    // page.
                    unsafe {
                        core::ptr::copy_nonoverlapping(phys_to_virt(at) as *const u8, out.as_mut_ptr().add(done), n)
                    }
                }
                None => out[done..done + n].fill(0),
            }
            done += n;
        }
        true
    }

    /// Copies `data` into the VMO, committing pages as needed: zeros for a
    /// page of anonymous memory that has none committed commit none, as it
    /// reads as zeros already (clearing what a file no longer holds takes
    /// no memory where it held nothing).
    pub fn write(&self, offset: u64, data: &[u8]) -> bool {
        if offset.checked_add(data.len() as u64).is_none_or(|end| end > self.size()) {
            return false;
        }
        let mut done = 0usize;
        while done < data.len() {
            let pos = offset + done as u64;
            let in_page = (pos % PAGE_SIZE) as usize;
            let n = (PAGE_SIZE as usize - in_page).min(data.len() - done);
            let zeros = self.kind == VmoKind::Anonymous && data[done..done + n].iter().all(|&b| b == 0);
            let Some(f) = self.page(pos, !zeros) else {
                if zeros {
                    done += n;
                    continue;
                }
                return false;
            };
            let at = f + in_page as u64;
            // SAFETY: as in `read`.
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr().add(done), phys_to_virt(at) as *mut u8, n) };
            self.flush(at, n as u64);
            done += n;
        }
        true
    }
}

/// Frames detached from a VMO ([`Vmo::decommit`]), freed on drop. They are
/// kept a page of them at a time ([`CHUNK`]): in one array they would need
/// an allocation growing with what was detached (2 MiB for 1 GiB of
/// memory), which the kernel's heap takes as physically contiguous memory,
/// and memory fragmented by use may have no such run free.
#[derive(Default)]
pub struct Frames(LinkedList<Vec<u64>>);

/// How many frames a chunk of [`Frames`] holds: a page of them.
const CHUNK: usize = PAGE_SIZE as usize / size_of::<u64>();

impl Frames {
    fn push(&mut self, frame: u64) {
        match self.0.back_mut() {
            Some(chunk) if chunk.len() < chunk.capacity() => chunk.push(frame),
            _ => {
                let mut chunk = Vec::with_capacity(CHUNK);
                chunk.push(frame);
                self.0.push_back(chunk);
            }
        }
    }

    /// Takes over `other`'s frames.
    pub fn append(&mut self, mut other: Frames) {
        self.0.append(&mut other.0);
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        for &f in self.0.iter().flatten() {
            phys::free(f);
        }
    }
}

impl Drop for Vmo {
    fn drop(&mut self) {
        match self.kind {
            VmoKind::Anonymous => {
                for (_, f) in core::mem::take(&mut *self.pages.lock()) {
                    phys::free(f);
                }
            }
            VmoKind::Contiguous { base } => phys::free_contiguous(base, self.size() / PAGE_SIZE),
            VmoKind::Physical { .. } => {}
        }
    }
}

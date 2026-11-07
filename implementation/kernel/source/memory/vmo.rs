// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Virtual memory objects (VMOs): the unit of memory that can be mapped into
//! address spaces and shared between processes through handles.
//!
//! An anonymous VMO's pages may be frames that other VMOs hold too: the
//! copies of a process's memory that `process_fork` makes
//! ([`Vmo::fork_copy`], [`super::cow`]). Such a frame is never written in
//! place: a page about to be written is made the VMO's own first
//! ([`Vmo::page_for_write`]), and a page that goes lets its frame go
//! ([`super::cow::release`]), which frees it once no VMO holds it.

use alloc::collections::{BTreeMap, LinkedList};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use abi::Error;

use super::paging::Cache;
use super::{PAGE_SIZE, cow, phys, phys_to_virt};
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
    /// Its frames may be other VMOs' too ([`Vmo::fork_copy`] made it, or a
    /// copy of it): each is looked up before it is written in place.
    shares: AtomicBool,
    /// Its pages were given to a device (`vmo_pages`), which reaches them
    /// by their addresses: they never move, so they are never copied on
    /// write either.
    pinned: AtomicBool,
}

impl Vmo {
    fn new(size: u64, kind: VmoKind, cache: Cache) -> Arc<Vmo> {
        Arc::new(Vmo {
            koid: crate::capability::new_koid(),
            size: AtomicU64::new(size),
            kind,
            cache,
            pages: SpinLock::new(BTreeMap::new()),
            committed: AtomicU64::new(0),
            shares: AtomicBool::new(false),
            pinned: AtomicBool::new(false),
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

    /// Its pages are given to a device (`vmo_pages`): from now on they
    /// never move ([`Vmo::copyable`]).
    pub fn pin(&self) {
        self.pinned.store(true, Ordering::Release);
    }

    /// Whether a process `process_fork` makes may have a copy of it that
    /// shares its frames until either writes them ([`Vmo::fork_copy`]):
    /// anonymous memory, cached as RAM is (a copy is made through the
    /// caches), whose pages no device was given.
    pub fn copyable(&self) -> bool {
        self.kind == VmoKind::Anonymous && self.cache == Cache::WriteBack && !self.pinned.load(Ordering::Acquire)
    }

    /// Whether its frames may be other VMOs' too: then a mapping of it is
    /// never made writable but a page at a time, as each is written
    /// ([`Vmo::page_for_write`]).
    pub fn shares(&self) -> bool {
        self.shares.load(Ordering::Acquire)
    }

    /// Whether `frame`, one of its pages, is another VMO's too, which
    /// neither may write in place.
    pub fn is_shared(&self, frame: u64) -> bool {
        self.shares() && cow::is_shared(frame)
    }

    /// A copy of the VMO for a process `process_fork` makes: of the same
    /// size, its committed pages the same frames, which neither writes in
    /// place from now on. `None` for a VMO that cannot be so copied
    /// ([`Vmo::copyable`]), or if there is no memory for the frames'
    /// counts (nothing is shared then).
    pub fn fork_copy(&self) -> Option<Arc<Vmo>> {
        if !self.copyable() {
            return None;
        }
        let copy = Vmo::new(self.size(), self.kind, self.cache);
        copy.shares.store(true, Ordering::Release);
        self.shares.store(true, Ordering::Release);
        {
            let pages = self.pages.lock();
            let mut theirs = copy.pages.lock();
            for (&page, &frame) in pages.iter() {
                if !cow::share(frame) {
                    // The frames shared so far are let go with the copy.
                    drop(theirs);
                    return None;
                }
                theirs.insert(page, frame);
            }
            copy.committed.store(theirs.len() as u64 * PAGE_SIZE, Ordering::Relaxed);
        }
        Some(copy)
    }

    /// How many pages the VMO has committed.
    pub fn committed_pages(&self) -> u64 {
        self.committed_bytes() / PAGE_SIZE
    }

    /// Whether the kernel reaches `phys` of the VMO through the direct map,
    /// which need not have a device's memory.
    fn reachable(&self, phys: u64) -> bool {
        !matches!(self.kind, VmoKind::Physical { .. }) || crate::architecture::memory::in_direct_map(phys)
    }

    /// If the VMO is RAM mapped without the caches: makes memory hold what
    /// the kernel wrote to `len` bytes of it at `phys` through the direct
    /// map (write-back), and no cache keep a line of them. Such a line
    /// would be written back later over what was written to the memory
    /// past the caches, or read in place of that.
    fn flush(&self, phys: u64, len: u64) {
        let ram = !matches!(self.kind, VmoKind::Physical { .. });
        if ram && self.cache != Cache::WriteBack {
            crate::architecture::memory::flush_cache_range(phys_to_virt(phys), len);
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

    /// The physical address of the page containing `offset`, which is about
    /// to be written: for anonymous memory, a page of its own, committed
    /// (zeroed) if it had none, or a copy of the frame it shares with
    /// other VMOs ([`Vmo::fork_copy`]), which they keep; the last to hold
    /// a frame takes it as it is. `None` past the end, or for no memory.
    pub fn page_for_write(&self, offset: u64) -> Option<u64> {
        if self.kind != VmoKind::Anonymous || !self.shares() {
            return self.page(offset, true);
        }
        if offset >= self.size() {
            return None;
        }
        let page = offset / PAGE_SIZE;
        let mut pages = self.pages.lock();
        let Some(&frame) = pages.get(&page) else {
            drop(pages);
            return self.page(offset, true);
        };
        if !cow::is_shared(frame) {
            return Some(frame);
        }
        // Copy-on-write VMOs are cached as RAM is: a copy through the
        // direct map is what each mapping reads.
        let copy = phys::alloc_user()?;
        // SAFETY: both frames are direct-mapped RAM; the copy is fresh, and
        // the shared one is written by no one (each holder copies it first).
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(frame) as *const u8,
                phys_to_virt(copy) as *mut u8,
                PAGE_SIZE as usize,
            )
        };
        pages.insert(page, copy);
        drop(pages);
        // Others hold it still: this only counts one fewer.
        cow::release(frame);
        Some(copy)
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
                    if !self.reachable(at) {
                        return false;
                    }
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
            // Zeros where nothing is committed leave it so; otherwise the
            // page is made the VMO's own to write.
            let found = match self.page(pos, false) {
                None if zeros => None,
                _ => self.page_for_write(pos),
            };
            let Some(f) = found else {
                if zeros {
                    done += n;
                    continue;
                }
                return false;
            };
            let at = f + in_page as u64;
            if !self.reachable(at) {
                return false;
            }
            // SAFETY: as in `read`.
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr().add(done), phys_to_virt(at) as *mut u8, n) };
            self.flush(at, n as u64);
            done += n;
        }
        true
    }
}

/// Frames detached from a VMO ([`Vmo::decommit`]), let go on drop (freed,
/// unless other VMOs hold them: [`cow::release`]). They are
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
            cow::release(f);
        }
    }
}

impl Drop for Vmo {
    fn drop(&mut self) {
        match self.kind {
            VmoKind::Anonymous => {
                for (_, f) in core::mem::take(&mut *self.pages.lock()) {
                    cow::release(f);
                }
            }
            VmoKind::Contiguous { base } => phys::free_contiguous(base, self.size() / PAGE_SIZE),
            VmoKind::Physical { .. } => {}
        }
    }
}

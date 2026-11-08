// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! User address spaces: a page-table hierarchy plus the list of VMO mappings
//! that it is lazily populated from on page faults.
//!
//! An address space can be copied for a process `process_fork` makes
//! ([`AddressSpace::fork`]): memory that is its own is copied on write, a
//! page at a time as either side first writes it (`super::cow`), and other
//! memory is shared, as `fork` has a Unix process's private and shared
//! mappings; a range may be kept from the copy, or wiped for it
//! ([`AddressSpace::inherit`]), as Linux's `madvise` has it.

use alloc::collections::btree_map::Entry;
use alloc::collections::{BTreeMap, BTreeSet, LinkedList};
use alloc::sync::Arc;

use abi::Error;

use super::paging::{self, Cache, Flags};
use super::vmo::{Frames, Vmo, VmoKind};
use super::{PAGE_SIZE, page_align_down};
use crate::synchronization::SpinLock;

const USER_START: u64 = abi::USER_SPACE_START as u64;
const USER_END: u64 = abi::USER_SPACE_END as u64;
/// Where non-fixed mappings are placed by default: from here, plus a
/// random number of pages ([`DYNAMIC_SPAN`]) chosen for each address space.
const DYNAMIC_BASE: u64 = 0x0000_1000_0000_0000;
/// How far above [`DYNAMIC_BASE`] an address space's mappings may start:
/// 2^28 pages (1 TiB), as many places as a program's image has.
const DYNAMIC_SPAN: u64 = 1 << 40;
/// Bytes of memory committed between two moments the other CPUs are let
/// into the kernel (`bkl::relax`).
const COMMIT_STEP: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Perms {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
}

impl Perms {
    pub fn from_map_flags(flags: usize) -> Perms {
        use abi::map_flags::*;
        Perms { read: flags & READ != 0, write: flags & WRITE != 0, exec: flags & EXECUTE != 0 }
    }

    /// Every permission.
    pub const ALL: Perms = Perms { read: true, write: true, exec: true };

    /// Whether `self` asks for nothing beyond `max`.
    pub fn within(self, max: Perms) -> bool {
        (!self.read || max.read) && (!self.write || max.write) && (!self.exec || max.exec)
    }

    /// No access at all (`PROT_NONE`): the pages must not be mapped.
    pub fn none(self) -> bool {
        !self.read && !self.write && !self.exec
    }

    /// What the page tables grant for these permissions: the hardware may
    /// allow more than asked (`architecture::paging::effective`).
    pub fn effective(self) -> Perms {
        crate::architecture::paging::effective(self)
    }
}

/// What became of a page fault ([`AddressSpace::handle_fault`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The page is mapped now: the access can be made again.
    Resolved,
    /// Nothing is mapped there, or not with the access made.
    Invalid,
    /// The access is allowed, but there is no memory left to give it a
    /// page (or a page table for it).
    OutOfMemory,
}

/// The end of `[addr, addr+len)` with `len` rounded up to whole pages (both
/// come from user space: any values).
fn range_end(addr: u64, len: u64) -> Result<u64, Error> {
    super::checked_page_align_up(len).and_then(|len| addr.checked_add(len)).ok_or(Error::InvalidArgs)
}

/// What a process `process_fork` makes has of a mapping
/// ([`AddressSpace::fork`]): as it was mapped, and as `vm_inherit` has
/// changed it since ([`AddressSpace::inherit`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inherit {
    /// Mapped with `map_flags::SHARED`: the VMO itself, even where nothing
    /// else reaches it, as `MAP_SHARED` memory.
    pub shared: bool,
    /// The handle it was mapped with may be transferred (`TRANSFER`): the
    /// VMO may reach another process.
    pub transferable: bool,
    /// Nothing at all (`inherit_flags::OMIT`), whatever else this says.
    pub omit: bool,
    /// New zero-filled memory (`inherit_flags::ZERO`), which only private
    /// memory ([`Mapping::private`]) has.
    pub zero: bool,
}

impl Inherit {
    /// Memory that is the address space's own: a fork's copy has a copy.
    pub const OWN: Inherit = Inherit::mapped(false, true);

    /// What a mapping made with `map_flags::SHARED` or not, through a
    /// handle that may be transferred or not, has until `vm_inherit`
    /// changes it.
    pub const fn mapped(shared: bool, transferable: bool) -> Inherit {
        Inherit { shared, transferable, omit: false, zero: false }
    }

    /// This, with the `inherit_flags` of `set` set and those of `clear`
    /// cleared.
    fn changed(self, set: usize, clear: usize) -> Inherit {
        use abi::inherit_flags::{OMIT, ZERO};
        let flag = |now: bool, bit: usize| set & bit != 0 || (now && clear & bit == 0);
        Inherit { omit: flag(self.omit, OMIT), zero: flag(self.zero, ZERO), ..self }
    }
}

/// What a fork's copy has of a mapping ([`AddressSpace::fork`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// Nothing.
    Absent,
    /// The same VMO.
    Shared,
    /// A copy of the VMO, which has the same frames until either writes
    /// them ([`Vmo::fork_copy`]).
    Copied,
    /// New zero-filled memory.
    Zeroed,
}

#[derive(Clone)]
pub struct Mapping {
    pub start: u64,
    pub len: u64,
    pub vmo: Arc<Vmo>,
    pub vmo_offset: u64,
    pub perms: Perms,
    /// The most `perms` may become: what the handle the VMO was mapped
    /// with allowed. `vm_protect` cannot go beyond it.
    pub max: Perms,
    /// The VMO was made for this mapping ([`AddressSpace::allocate`]) and
    /// no handle reaches it: each of its pages is mapped here and nowhere
    /// else, so a page is freed as soon as it is unmapped or decommitted.
    /// The mappings that are its parts (split by [`AddressSpace::protect`],
    /// say) map parts of the VMO apart.
    pub private: bool,
    /// What a fork's copy has of it.
    pub inherit: Inherit,
}

impl Mapping {
    fn end(&self) -> u64 {
        self.start + self.len
    }

    fn pte_flags(&self) -> Flags {
        Flags {
            writable: self.perms.write,
            executable: self.perms.exec,
            user: true,
            global: false,
            cache: self.vmo.cache(),
        }
    }

    /// The flags of its pages as `protect` and a fork leave them: never
    /// writable where the VMO's frames may be another's too, which only a
    /// write fault makes a page's own ([`AddressSpace::handle_fault`]).
    fn shareable_pte_flags(&self) -> Flags {
        Flags { writable: self.perms.write && !self.vmo.shares(), ..self.pte_flags() }
    }
}

pub struct AddressSpace {
    /// The page tables' root.
    root: u64,
    mappings: SpinLock<BTreeMap<u64, Mapping>>,
    /// Where mappings whose address the caller leaves to the kernel go:
    /// random for each address space, so that no process's stacks, heaps
    /// and shared memory are where another's (or the same program's last
    /// run's) were.
    dynamic_base: u64,
}

impl AddressSpace {
    pub fn new() -> Option<Arc<AddressSpace>> {
        let mut random = [0u8; 8];
        crate::random::fill(&mut random);
        let pages = u64::from_le_bytes(random) % (DYNAMIC_SPAN / PAGE_SIZE);
        Some(Arc::new(AddressSpace {
            root: paging::new_user_root()?,
            mappings: SpinLock::new(BTreeMap::new()),
            dynamic_base: DYNAMIC_BASE + pages * PAGE_SIZE,
        }))
    }

    /// Finds a free, page-aligned range of `len` bytes at or above `hint`
    /// (which comes from user space: any value), inside user space.
    fn find_free(map: &BTreeMap<u64, Mapping>, len: u64, hint: u64) -> Option<u64> {
        let mut candidate = hint.clamp(USER_START, USER_END);
        // A mapping that starts below `candidate` may still overlap it.
        if let Some((_, m)) = map.range(..candidate).next_back() {
            candidate = candidate.max(m.end());
        }
        for (_, m) in map.range(candidate..) {
            if m.start >= candidate.checked_add(len)? {
                break;
            }
            // One unmapped guard page between dynamic mappings.
            candidate = m.end() + PAGE_SIZE;
        }
        candidate.checked_add(len).is_some_and(|end| end <= USER_END).then_some(candidate)
    }

    fn overlaps(map: &BTreeMap<u64, Mapping>, start: u64, end: u64) -> bool {
        map.range(..end).next_back().is_some_and(|(_, m)| m.end() > start)
    }

    /// Maps `len` bytes of `vmo` starting at `vmo_offset`. Returns the
    /// address. `addr` is a hint unless `fixed` is set. The mapping may
    /// later be given at most the permissions `max`; a fork's copy has of
    /// it what `inherit` says.
    #[allow(clippy::too_many_arguments)] // mirrors the VM_MAP system call
    pub fn map(
        &self,
        vmo: Arc<Vmo>,
        vmo_offset: u64,
        len: u64,
        addr: u64,
        fixed: bool,
        perms: Perms,
        max: Perms,
        inherit: Inherit,
        commit: bool,
    ) -> Result<u64, Error> {
        if len == 0 || !vmo_offset.is_multiple_of(PAGE_SIZE) {
            return Err(Error::InvalidArgs);
        }
        let len = super::checked_page_align_up(len).ok_or(Error::OutOfRange)?;
        if vmo_offset.checked_add(len).is_none_or(|e| e > vmo.size()) {
            return Err(Error::OutOfRange);
        }
        let m = Mapping { start: 0, len, vmo, vmo_offset, perms, max, private: false, inherit };
        self.insert(m, addr, fixed, commit)
    }

    /// Maps `len` bytes of new zero-filled memory that only this mapping
    /// reaches (see [`Mapping::private`]). Returns the address; `addr` is
    /// a hint unless `fixed` is set.
    pub fn allocate(&self, len: u64, addr: u64, fixed: bool, perms: Perms, commit: bool) -> Result<u64, Error> {
        if len == 0 {
            return Err(Error::InvalidArgs);
        }
        let vmo = Vmo::new_anonymous(len, Cache::WriteBack).ok_or(Error::NoMemory)?;
        let len = vmo.size();
        self.insert(
            Mapping { start: 0, len, vmo, vmo_offset: 0, perms, max: Perms::ALL, private: true, inherit: Inherit::OWN },
            addr,
            fixed,
            commit,
        )
    }

    /// Places `m` (whose `start` is chosen here) at `addr`, or near it
    /// unless `fixed`, and maps the pages that are mapped eagerly.
    fn insert(&self, mut m: Mapping, addr: u64, fixed: bool, commit: bool) -> Result<u64, Error> {
        if !addr.is_multiple_of(PAGE_SIZE) {
            return Err(Error::InvalidArgs);
        }
        if !m.perms.within(m.max) {
            return Err(Error::AccessDenied);
        }
        // Physical memory is always mapped eagerly; anonymous memory on
        // request (otherwise on first touch). Inaccessible pages never are.
        let eager = (commit || !matches!(m.vmo.kind(), VmoKind::Anonymous)) && !m.perms.none();
        // Anonymous pages are committed (allocated and zeroed) first, before
        // the mappings are locked, letting the other CPUs into the kernel
        // between steps: their number is the caller's to choose.
        if eager && m.vmo.kind() == VmoKind::Anonymous {
            for off in (0..m.len).step_by(PAGE_SIZE as usize) {
                m.vmo.page(m.vmo_offset + off, true).ok_or(Error::NoMemory)?;
                if (off + PAGE_SIZE).is_multiple_of(COMMIT_STEP) {
                    crate::synchronization::bkl::relax();
                }
            }
        }
        let mut map = self.mappings.lock();
        m.start = if fixed {
            if addr < USER_START || addr.checked_add(m.len).is_none_or(|e| e > USER_END) {
                return Err(Error::OutOfRange);
            }
            if Self::overlaps(&map, addr, addr + m.len) {
                return Err(Error::AlreadyExists);
            }
            addr
        } else {
            let hint = if addr != 0 { addr } else { self.dynamic_base };
            Self::find_free(&map, m.len, hint)
                .or_else(|| Self::find_free(&map, m.len, USER_START))
                .ok_or(Error::NoMemory)?
        };
        if eager {
            for off in (0..m.len).step_by(PAGE_SIZE as usize) {
                let mapped = m
                    .vmo
                    .page(m.vmo_offset + off, true)
                    .is_some_and(|frame| paging::map(self.root, m.start + off, frame, m.pte_flags(), true).is_ok());
                if !mapped {
                    // No page table entry may outlive the failed mapping.
                    paging::unmap_range(self.root, m.start, m.start + off);
                    drop(map);
                    crate::architecture::tlb::shootdown_space(self.root, m.start, off);
                    return Err(Error::NoMemory);
                }
            }
        }
        let start = m.start;
        map.insert(start, m);
        Ok(start)
    }

    /// Removes all mappings (or parts of mappings) inside `[addr, addr+len)`.
    pub fn unmap(&self, addr: u64, len: u64) -> Result<(), Error> {
        if !addr.is_multiple_of(PAGE_SIZE) || len == 0 {
            return Err(Error::InvalidArgs);
        }
        let end = range_end(addr, len)?;
        let mut map = self.mappings.lock();
        // What goes waits for the shootdown: the mappings in a list, the
        // pages in `Frames`, neither in an allocation that grows with it.
        let mut removed = LinkedList::new();
        let mut freed = Frames::default();
        // The last mapping the range reaches, until none does: what is put
        // back of one is outside the range.
        while let Some(s) = map.range(..end).next_back().filter(|(_, m)| m.end() > addr).map(|(&s, _)| s) {
            let m = map.remove(&s).unwrap();
            let cut_start = m.start.max(addr);
            let cut_end = m.end().min(end);
            paging::unmap_range(self.root, cut_start, cut_end);
            if m.private {
                freed.append(m.vmo.decommit(m.vmo_offset + (cut_start - m.start), cut_end - cut_start));
            }
            if m.start < cut_start {
                let mut left = m.clone();
                left.len = cut_start - m.start;
                map.insert(left.start, left);
            }
            if m.end() > cut_end {
                let mut right = m.clone();
                right.vmo_offset += cut_end - m.start;
                right.start = cut_end;
                right.len = m.end() - cut_end;
                map.insert(right.start, right);
            }
            removed.push_back(m);
        }
        if removed.is_empty() {
            return Err(Error::NotFound);
        }
        drop(map);
        crate::architecture::tlb::shootdown_space(self.root, addr, end - addr);
        // Memory is freed only now that no TLB can reach it: the pages cut
        // from private memory, and the VMOs whose last mapping went.
        drop(freed);
        drop(removed);
        Ok(())
    }

    /// Frees the pages of `[addr, addr+len)`, which private memory
    /// ([`allocate`](Self::allocate)) must cover completely: they read as
    /// zeros from then on. The mappings and their permissions stay.
    pub fn decommit(&self, addr: u64, len: u64) -> Result<(), Error> {
        if !addr.is_multiple_of(PAGE_SIZE) || len == 0 {
            return Err(Error::InvalidArgs);
        }
        let end = range_end(addr, len)?;
        let map = self.mappings.lock();
        let first = Self::covering(&map, addr, end, |m| if m.private { Ok(()) } else { Err(Error::NotSupported) })?;
        let mut freed = Frames::default();
        for (_, m) in map.range(first..end) {
            let from = m.start.max(addr);
            let to = m.end().min(end);
            paging::unmap_range(self.root, from, to);
            freed.append(m.vmo.decommit(m.vmo_offset + (from - m.start), to - from));
        }
        drop(map);
        crate::architecture::tlb::shootdown_space(self.root, addr, end - addr);
        drop(freed);
        Ok(())
    }

    /// Checks that mappings cover `[addr, end)` without a gap (`NotFound`
    /// otherwise) and that `check` accepts each of them. Returns the start
    /// of the first.
    fn covering(
        map: &BTreeMap<u64, Mapping>,
        addr: u64,
        end: u64,
        check: impl Fn(&Mapping) -> Result<(), Error>,
    ) -> Result<u64, Error> {
        let first = map.range(..=addr).next_back().filter(|(_, m)| m.end() > addr).map_or(addr, |(&s, _)| s);
        let mut covered = addr;
        for (_, m) in map.range(first..end) {
            if m.start > covered {
                return Err(Error::NotFound);
            }
            check(m)?;
            covered = m.end();
        }
        if covered < end {
            return Err(Error::NotFound);
        }
        Ok(first)
    }

    /// Splits the mapping that straddles `at`, if any, so that a mapping
    /// boundary falls there.
    fn split_at(map: &mut BTreeMap<u64, Mapping>, at: u64) {
        let Some((&start, m)) = map.range(..at).next_back() else { return };
        if m.end() <= at {
            return;
        }
        let mut right = m.clone();
        right.vmo_offset += at - start;
        right.start = at;
        right.len = m.end() - at;
        map.get_mut(&start).unwrap().len = at - start;
        map.insert(at, right);
    }

    /// Makes the processors that have these page tables loaded, which run
    /// this address space's threads, go through a full memory barrier, and
    /// waits until each has (`vm_barrier`). The others run none of them:
    /// they go through the scheduler, and its barriers, before they do.
    pub fn barrier(&self) {
        crate::architecture::tlb::barrier_space(self.root);
    }

    /// Changes the permissions of `[addr, addr+len)`, which must be mapped
    /// completely, splitting mappings at its ends as needed. No mapping may
    /// get permissions beyond those it was mapped with.
    pub fn protect(&self, addr: u64, len: u64, perms: Perms) -> Result<(), Error> {
        if !addr.is_multiple_of(PAGE_SIZE) || len == 0 {
            return Err(Error::InvalidArgs);
        }
        let end = range_end(addr, len)?;
        let mut map = self.mappings.lock();
        // Check everything before changing anything.
        Self::covering(&map, addr, end, |m| if perms.within(m.max) { Ok(()) } else { Err(Error::AccessDenied) })?;
        Self::split_at(&mut map, addr);
        Self::split_at(&mut map, end);
        for (_, m) in map.range_mut(addr..end) {
            m.perms = perms;
            if perms.none() {
                // Faults on its pages are refused from now on.
                paging::unmap_range(self.root, m.start, m.end());
            } else {
                // A page whose frame another VMO holds too becomes writable
                // as it is written, a copy of its own.
                paging::protect_range(self.root, m.start, m.end(), m.shareable_pte_flags());
            }
        }
        drop(map);
        crate::architecture::tlb::shootdown_space(self.root, addr, end - addr);
        Ok(())
    }

    /// Changes what a process `process_fork` makes has of `[addr,
    /// addr+len)` ([`Inherit`]), which must be mapped completely, splitting
    /// mappings at its ends as needed: the `inherit_flags` of `set` are set
    /// there, and those of `clear` cleared. Only private memory may be
    /// zeroed for the copy (`NotSupported`). The pages stay as they are.
    pub fn inherit(&self, addr: u64, len: u64, set: usize, clear: usize) -> Result<(), Error> {
        if !addr.is_multiple_of(PAGE_SIZE) || len == 0 {
            return Err(Error::InvalidArgs);
        }
        let end = range_end(addr, len)?;
        let zero = set & abi::inherit_flags::ZERO != 0;
        let mut map = self.mappings.lock();
        // Check everything before changing anything.
        Self::covering(&map, addr, end, |m| if !zero || m.private { Ok(()) } else { Err(Error::NotSupported) })?;
        Self::split_at(&mut map, addr);
        Self::split_at(&mut map, end);
        for (_, m) in map.range_mut(addr..end) {
            m.inherit = m.inherit.changed(set, clear);
        }
        Ok(())
    }

    /// Where `addr` is in the VMO mapped there, if anything is: for a
    /// program's code, the offset into its image, which is the address in
    /// its executable for one linked at 0, as Veda's programs are (for the
    /// log of a crash).
    pub fn vmo_offset_of(&self, addr: u64) -> Option<u64> {
        let map = self.mappings.lock();
        let (_, m) = map.range(..=addr).next_back()?;
        (addr < m.end()).then(|| m.vmo_offset + (addr - m.start))
    }

    /// The VMO the byte at `addr` is in and its offset there, by the VMO's
    /// kernel object id: where every mapping of the VMO, in any address
    /// space, reaches the same byte (what a shared futex is known by).
    /// `None` where nothing is mapped, and for private memory
    /// ([`Mapping::private`]), which no other mapping reaches.
    pub fn shared_location(&self, addr: u64) -> Option<(u64, u64)> {
        let map = self.mappings.lock();
        let (_, m) = map.range(..=addr).next_back()?;
        (addr < m.end() && !m.private).then(|| (m.vmo.koid, m.vmo_offset + (addr - m.start)))
    }

    /// Resolves a user page fault.
    pub fn handle_fault(&self, addr: u64, write: bool, exec: bool) -> Fault {
        let map = self.mappings.lock();
        let Some((_, m)) = map.range(..=addr).next_back() else { return Fault::Invalid };
        // The access the page tables would allow once the page is mapped.
        let granted = m.perms.effective();
        let allowed = if write || exec { (!write || granted.write) && (!exec || granted.exec) } else { granted.read };
        if addr >= m.end() || !allowed {
            return Fault::Invalid;
        }
        let va = page_align_down(addr);
        let offset = m.vmo_offset + (va - m.start);
        let mut flags = m.pte_flags();
        // The page is inside the VMO (mappings never reach past their VMO,
        // which never shrinks): only memory can be missing. A write makes
        // the page the VMO's own (a copy of a frame another VMO holds too);
        // a read maps such a frame read-only, until it is written.
        let frame = if write && flags.writable {
            m.vmo.page_for_write(offset)
        } else {
            let frame = m.vmo.page(offset, true);
            if frame.is_some_and(|f| m.vmo.is_shared(f)) {
                flags.writable = false;
            }
            frame
        };
        let Some(frame) = frame else { return Fault::OutOfMemory };
        // A copy replaces the frame the page had: no processor may read the
        // old one through what its TLB remembers once the copy is written.
        // Nor may one still take the page for read-only once a write made
        // it writable with the frame it had (one no other VMO holds any
        // more, as after a fork whose copy has written or ended): the
        // kernel writes the pages it has validated (`ensure_range`) with no
        // fault to resolve, and a fault in kernel mode is fatal.
        let stale = paging::translate(self.root, va)
            .is_some_and(|(at, was)| page_align_down(at) != frame || (flags.writable && !was.writable));
        if paging::map(self.root, va, frame, flags, true).is_err() {
            return Fault::OutOfMemory;
        }
        drop(map);
        if stale {
            crate::architecture::tlb::shootdown_space(self.root, va, PAGE_SIZE);
        }
        Fault::Resolved
    }

    /// Ensures every page of `[addr, addr+len)` is mapped with the required
    /// access, faulting pages in as needed.
    pub fn ensure_range(&self, addr: u64, len: u64, write: bool) -> bool {
        if len == 0 {
            return true;
        }
        let Some(end) = addr.checked_add(len) else { return false };
        if addr < USER_START || end > USER_END {
            return false;
        }
        let mut va = page_align_down(addr);
        while va < end {
            let mut mapped = paging::translate(self.root, va);
            if !mapped.is_some_and(|(_, flags)| flags.user && (!write || flags.writable)) {
                if self.handle_fault(va, write, false) != Fault::Resolved {
                    return false;
                }
                mapped = paging::translate(self.root, va);
            }
            // A device's registers are for its driver, in the accesses it
            // chooses: not every architecture lets the kernel's copies touch
            // them.
            if mapped.is_none_or(|(_, flags)| !paging::kernel_copies(&flags)) {
                return false;
            }
            va += PAGE_SIZE;
        }
        true
    }

    /// Bytes of memory committed by the VMOs mapped here (shared VMOs are
    /// counted in every address space that maps them).
    pub fn committed_bytes(&self) -> u64 {
        let map = self.mappings.lock();
        let mut seen = BTreeSet::new();
        let mut total = 0;
        for m in map.values() {
            if matches!(m.vmo.kind(), VmoKind::Physical { .. }) || !seen.insert(m.vmo.koid) {
                continue;
            }
            total += m.vmo.committed_bytes();
        }
        total
    }

    /// A copy of this address space, for a process `process_fork` makes:
    /// the same mappings at the same places, with the same permissions,
    /// each of what its [`Inherit`] says ([`Fate`]):
    ///
    /// * none of a range kept from the copy (`omit`), and new zero-filled
    ///   memory for one wiped for it (`zero`): the copy holds nothing of
    ///   what either holds here, not even in a VMO it has of other parts;
    /// * memory that is the address space's own is copied: anonymous
    ///   memory that nothing reaches but its mappings here (its private
    ///   memory, and VMOs mapped through handles that have since closed),
    ///   not mapped `shared`. The copy and the original share the frames
    ///   the copy's mappings reach until either writes a page, which then
    ///   gets one of its own ([`Vmo::fork_copy`]): the writable pages here
    ///   that lead to them are made read-only until then;
    /// * other memory is the same VMO, if the handle it was mapped with may
    ///   be transferred, as the process could have given it the VMO;
    /// * the rest is not there.
    ///
    /// The copy's pages are mapped as they are first touched. `NoMemory` if
    /// there is none for the copy's tables or the frames' counts; nothing
    /// is shared then.
    pub fn fork(&self) -> Result<Arc<AddressSpace>, Error> {
        let map = self.mappings.lock();
        // How many mappings here each VMO has, and whether any is shared.
        let mut vmos: BTreeMap<u64, (usize, bool)> = BTreeMap::new();
        for m in map.values() {
            let (count, shared) = vmos.entry(m.vmo.koid).or_insert((0, false));
            *count += 1;
            *shared |= m.inherit.shared;
        }
        // Memory nothing reaches but these mappings: no handle, no other
        // address space, no guest holds its VMO, which counts a reference
        // for each. It is the same for every mapping of a VMO, and each
        // mapping's fate stays as it is while the copy is made: the BKL is
        // held throughout, and a VMO is referenced again (`Fate::Shared`)
        // only if it is not the address space's own.
        let own = |m: &Mapping| {
            let (count, shared) = vmos[&m.vmo.koid];
            m.vmo.copyable() && (m.private || (!shared && Arc::strong_count(&m.vmo) == count))
        };
        let fate = |m: &Mapping| match m.inherit {
            Inherit { omit: true, .. } => Fate::Absent,
            Inherit { zero: true, .. } => Fate::Zeroed,
            _ if own(m) => Fate::Copied,
            Inherit { transferable: true, .. } => Fate::Shared,
            _ => Fate::Absent,
        };
        // The parts of each VMO copied that the copy's mappings reach (the
        // VMO's id and an offset into it where a part starts, and where the
        // part ends), the frames the copy shares; mappings of other VMOs
        // with no part there need not be protected below.
        let mut reached: BTreeMap<(u64, u64), u64> = BTreeMap::new();
        for m in map.values().filter(|m| fate(m) == Fate::Copied) {
            let end = reached.entry((m.vmo.koid, m.vmo_offset)).or_insert(0);
            *end = (*end).max(m.vmo_offset + m.len);
        }
        let parts = |koid: u64| reached.range((koid, 0)..=(koid, u64::MAX)).map(|(&(_, from), &to)| (from, to));
        // The copies' tables of frames and counts, its mappings and what
        // they reach are kernel memory, taken on user space's behalf as
        // the frames are.
        let pages: u64 = map
            .values()
            .filter(|m| fate(m) == Fate::Copied)
            .map(|m| (m.vmo.koid, m.vmo.committed_pages()))
            .collect::<BTreeMap<u64, u64>>()
            .values()
            .sum();
        let tables = (map.len() as u64).saturating_mul(256);
        if !super::phys::user_may_use(pages.saturating_mul(32).saturating_add(tables).saturating_add(64 * 1024)) {
            return Err(Error::NoMemory);
        }
        let child = Arc::new(AddressSpace {
            root: paging::new_user_root().ok_or(Error::NoMemory)?,
            mappings: SpinLock::new(BTreeMap::new()),
            dynamic_base: self.dynamic_base,
        });
        // No writable page here may lead to a frame that is shared, so
        // they go read-only before any is, everywhere this address space
        // is loaded; a write faults then, and copies. Every mapping of a
        // VMO copied may reach a frame shared, but a part of private memory
        // that is not copied: no other mapping reaches its pages.
        let mut protected = false;
        for m in map.values().filter(|m| m.perms.write && parts(m.vmo.koid).next().is_some()) {
            if m.private && fate(m) != Fate::Copied {
                continue;
            }
            paging::protect_range(self.root, m.start, m.end(), Flags { writable: false, ..m.pte_flags() });
            protected = true;
        }
        if protected {
            crate::architecture::tlb::shootdown_space(self.root, USER_START, USER_END - USER_START);
        }
        // The VMOs made for the copy, by the VMO each stands for: copies,
        // and zero-filled memory of the same size, which its parts of the
        // VMO are at the same offsets in.
        let mut copies: BTreeMap<u64, Arc<Vmo>> = BTreeMap::new();
        let mut zeroed: BTreeMap<u64, Arc<Vmo>> = BTreeMap::new();
        let mut theirs = child.mappings.lock();
        for m in map.values() {
            let vmo = match fate(m) {
                Fate::Absent => continue,
                Fate::Shared => m.vmo.clone(),
                Fate::Copied => match copies.entry(m.vmo.koid) {
                    Entry::Occupied(copy) => copy.get().clone(),
                    Entry::Vacant(place) => {
                        place.insert(m.vmo.fork_copy(parts(m.vmo.koid)).ok_or(Error::NoMemory)?).clone()
                    }
                },
                Fate::Zeroed => match zeroed.entry(m.vmo.koid) {
                    Entry::Occupied(zero) => zero.get().clone(),
                    Entry::Vacant(place) => {
                        place.insert(Vmo::new_anonymous(m.vmo.size(), m.vmo.cache()).ok_or(Error::NoMemory)?).clone()
                    }
                },
            };
            theirs.insert(m.start, Mapping { vmo, ..m.clone() });
        }
        drop(theirs);
        Ok(child)
    }

    /// Loads this address space on the current CPU.
    pub fn activate(&self) {
        paging::activate(self.root);
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        // Mappings (and their VMO references) are dropped with the map; the
        // frames belong to the VMOs. Only the page tables are ours.
        self.mappings.lock().clear();
        paging::free_user_root(self.root);
    }
}

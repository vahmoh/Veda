// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Linux program's memory: what it has mapped (its image and its
//! dynamic linker's, its stack, its program break, what `mmap` gave it),
//! which Linux compatibility keeps account of, as Linux keeps a process's
//! areas (`/proc/self/maps` lists them).
//!
//! The program shares its process with Linux compatibility, whose own
//! memory (its image, its heap, its threads' stacks) is no area of the
//! program's: the program's calls change only its own areas. `MAP_FIXED`
//! replaces only what the program mapped there, `munmap` and `mprotect`
//! reach only its areas, and an area the kernel finds taken by Linux
//! compatibility's memory is refused (`ENOMEM`), where Linux would have
//! found nothing. So nothing the program does with its memory can take
//! Linux compatibility's from under it. What the program's memory holds
//! is read and written for it only where it has an area that allows the
//! access ([`accessible`]): a bad pointer in a system call is `EFAULT`, a
//! signal frame for a stack that has run out is a fault, as on Linux.
//!
//! What a child `fork` makes has of an area (`MADV_DONTFORK`,
//! `MADV_WIPEONFORK`) is the kernel's to say (`vm_inherit`), and an area
//! keeps account of it too: an area the child does not have is not its
//! own there ([`forked`]), and memory that takes an area's place
//! (`mremap`'s) is told the same.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use abi::{inherit_flags, map_flags};
use runtime::sync::Mutex;
use runtime::vm;

use crate::linux::errno::*;
use crate::linux::mman::*;
use crate::linux::nr;

const PAGE: u64 = abi::PAGE_SIZE as u64;

/// What an area holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A program's or a library's image (the dynamic linker's, for one
    /// the kernel loads), from its file.
    Image(String),
    /// Memory `mmap` gave: of nothing (`MAP_ANONYMOUS`) or of a file, the
    /// program's alone (`MAP_PRIVATE`) or shared.
    Mapped { anonymous: bool, shared: bool },
    /// The main thread's stack.
    Stack,
    /// The program break's (`brk`).
    Heap,
}

/// One of the program's areas: `[start, end)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Area {
    pub end: u64,
    /// `PROT_READ`, `PROT_WRITE` and `PROT_EXEC`.
    pub prot: u32,
    pub kind: Kind,
    /// What a child `fork` makes has of it, the kernel's `inherit_flags`:
    /// `OMIT` (`MADV_DONTFORK`), `ZERO` (`MADV_WIPEONFORK`).
    pub inherit: usize,
}

/// The program's areas, by where they start, and its program break.
struct Map {
    areas: BTreeMap<u64, Area>,
    /// Where the program break starts, and where it is.
    brk: (u64, u64),
}

static MAP: Mutex<Map> = Mutex::new(Map { areas: BTreeMap::new(), brk: (0, 0) });

/// The program's areas held still ([`hold`]).
pub struct Held {
    _map: runtime::sync::MutexGuard<'static, Map>,
}

/// Holds the program's areas still while a copy of the process is made
/// (`fork`), which has them as they are: none is being changed.
pub fn hold() -> Held {
    Held { _map: MAP.lock() }
}

impl Map {
    /// The areas that reach into `[start, end)`, and where each starts:
    /// none, if the range is empty.
    fn reaching(&self, start: u64, end: u64) -> impl Iterator<Item = (u64, &Area)> {
        self.areas.range(..end).filter(move |(_, a)| start < end && a.end > start).map(|(s, a)| (*s, a))
    }

    /// Takes `[start, end)` out of every area, splitting those it cuts.
    fn cut(&mut self, start: u64, end: u64) {
        let touched: Vec<u64> = self.reaching(start, end).map(|(s, _)| s).collect();
        for s in touched {
            let a = self.areas.remove(&s).unwrap();
            if s < start {
                self.areas.insert(s, Area { end: start, ..a.clone() });
            }
            if a.end > end {
                self.areas.insert(end, Area { end: a.end, ..a });
            }
        }
    }

    /// Notes `area`, from `start`, in place of any it covers.
    fn put(&mut self, start: u64, area: Area) {
        self.cut(start, area.end);
        self.areas.insert(start, area);
    }

    /// Changes the program's areas' pieces in `[start, end)` as `change`
    /// says, splitting the areas at its ends.
    fn change(&mut self, start: u64, end: u64, change: impl Fn(&mut Area)) {
        let pieces: Vec<(u64, Area)> =
            self.reaching(start, end).map(|(s, a)| (s.max(start), Area { end: a.end.min(end), ..a.clone() })).collect();
        self.cut(start, end);
        for (s, mut a) in pieces {
            change(&mut a);
            self.areas.insert(s, a);
        }
    }

    /// In a child `fork` made: the areas the kernel left out of it are
    /// gone.
    fn forked(&mut self) {
        self.areas.retain(|_, a| a.inherit & inherit_flags::OMIT == 0);
    }

    /// Notes the program break's new memory, `[old_top, new_top)`: one
    /// area for the whole break, as Linux keeps it, but after one the
    /// program protected otherwise, or kept from a child or wiped for it.
    /// The new memory is read-write, and neither kept nor wiped, as on
    /// Linux.
    fn break_grown(&mut self, old_top: u64, new_top: u64) {
        let rw = PROT_READ | PROT_WRITE;
        self.cut(old_top, new_top);
        let begin = self
            .areas
            .range(..=old_top)
            .next_back()
            .filter(|(_, a)| a.kind == Kind::Heap && a.end == old_top && a.prot == rw && a.inherit == 0);
        let first = begin.map_or(old_top, |(s, _)| *s);
        self.areas.insert(first, Area { end: new_top, prot: rw, kind: Kind::Heap, inherit: 0 });
    }

    /// The area that holds `[start, end)` whole, and where it starts.
    fn holding(&self, start: u64, end: u64) -> Option<(u64, &Area)> {
        self.areas.range(..=start).next_back().filter(|(_, a)| a.end >= end).map(|(s, a)| (*s, a))
    }

    /// Notes that the area at `start` grew in place to `end` (`mremap`'s):
    /// one area still, as Linux keeps it, its new memory like the rest of
    /// it.
    fn grown(&mut self, start: u64, end: u64) {
        if let Some(a) = self.areas.get(&start).cloned() {
            self.put(start, Area { end, ..a });
        }
    }

    /// The program's areas' pieces in `[start, end)`.
    fn pieces(&self, start: u64, end: u64) -> Vec<(u64, u64)> {
        self.reaching(start, end).map(|(s, a)| (s.max(start), a.end.min(end))).collect()
    }

    /// Whether the program may read (or with `write`, write) `len` bytes
    /// at `at`: its areas cover them, with that access.
    fn allows(&self, at: usize, len: usize, write: bool) -> bool {
        let Some(end) = (at as u64).checked_add(len as u64) else { return false };
        if len == 0 {
            return true;
        }
        let mut covered = at as u64;
        for (s, a) in self.areas.range(..end) {
            if a.end <= covered {
                continue;
            }
            if *s > covered {
                return false;
            }
            let allowed =
                if write { a.prot & PROT_WRITE != 0 } else { a.prot & (PROT_READ | PROT_WRITE | PROT_EXEC) != 0 };
            if !allowed {
                return false;
            }
            covered = a.end;
            if covered >= end {
                return true;
            }
        }
        covered >= end
    }

    /// Whether areas cover `[start, end)` without a gap.
    fn covers(&self, start: u64, end: u64) -> bool {
        let mut at = start;
        for (s, e) in self.pieces(start, end) {
            if s > at {
                return false;
            }
            at = at.max(e);
        }
        at >= end
    }
}

/// Notes a new area `[start, start + len)`, in place of any it covers.
pub fn record(start: u64, len: u64, prot: u32, kind: Kind) {
    MAP.lock().put(start, Area { end: start + len, prot, kind, inherit: 0 });
}

/// In a child `fork` made, whose memory has no area the kernel left out of
/// it (`MADV_DONTFORK`): nor has the program.
pub fn forked() {
    MAP.lock().forked();
}

/// Forgets every area, as `execve` leaves none: unmapping each.
pub fn clear() {
    let mut map = MAP.lock();
    for (s, a) in core::mem::take(&mut map.areas) {
        let _ = vm::unmap(None, s as usize, (a.end - s) as usize);
    }
    map.brk = (0, 0);
}

/// Runs `f` if the program may read (or with `write`, write) `len` bytes
/// at `at`, its areas covering them with that access; they are held
/// meanwhile, so that no other thread's `munmap` or `mprotect` takes the
/// memory away while `f` uses it (which would be this process's fault,
/// not the program's). `None` if the program may not.
pub fn while_accessible<R>(at: usize, len: usize, write: bool, f: impl FnOnce() -> R) -> Option<R> {
    let map = MAP.lock();
    if !map.allows(at, len, write) {
        return None;
    }
    let r = f();
    drop(map);
    Some(r)
}

/// Every area, for `/proc/self/maps`.
pub fn areas() -> Vec<(u64, Area)> {
    MAP.lock().areas.iter().map(|(s, a)| (*s, a.clone())).collect()
}

/// Where the program break starts (`start`, past the program's image),
/// where it is now as well.
pub fn set_break(start: u64) {
    MAP.lock().brk = (start, start);
}

/// The kernel's mapping flags for a protection.
fn flags_of(prot: u32) -> usize {
    let mut f = 0;
    if prot & PROT_READ != 0 {
        f |= map_flags::READ;
    }
    if prot & PROT_WRITE != 0 {
        f |= map_flags::READ | map_flags::WRITE;
    }
    if prot & PROT_EXEC != 0 {
        f |= map_flags::READ | map_flags::EXECUTE;
    }
    f
}

fn page_up(v: u64) -> Option<u64> {
    v.checked_next_multiple_of(PAGE)
}

/// `brk`: moves the program break to `to`, mapping memory up to it or
/// unmapping what is above it, and returns where it is (where it was, if
/// it cannot move: Linux's convention, which C libraries check).
pub fn brk(to: u64) -> isize {
    let mut map = MAP.lock();
    let (start, now) = map.brk;
    if to < start || start == 0 {
        return now as isize;
    }
    let (Some(old_top), Some(new_top)) = (page_up(now), page_up(to)) else { return now as isize };
    if new_top > old_top {
        let len = (new_top - old_top) as usize;
        let rw = map_flags::READ | map_flags::WRITE | map_flags::FIXED;
        if vm::allocate(None, len, old_top as usize, rw).is_err() {
            return now as isize;
        }
        map.break_grown(old_top, new_top);
    } else if new_top < old_top {
        // The program's areas there go: only they, as Linux
        // compatibility's own memory may be where the program unmapped
        // some of its heap.
        for (s, e) in map.pieces(new_top, old_top) {
            let _ = vm::unmap(None, s as usize, (e - s) as usize);
        }
        map.cut(new_top, old_top);
    }
    map.brk.1 = to;
    to as isize
}

/// `mmap`, for the program: the POSIX layer maps, Linux compatibility
/// keeps the area. `MAP_FIXED` replaces only the program's own areas.
///
/// # Safety
/// As the POSIX layer's `mmap`: the arguments are the program's.
pub unsafe fn mmap(addr: u64, len: u64, prot: u32, flags: u32, fd: usize, offset: u64) -> Result<u64, isize> {
    if len == 0 {
        return Err(EINVAL);
    }
    let len = page_up(len).ok_or(ENOMEM)?;
    // There are no huge pages to give, as on a Linux that reserved none.
    if flags & MAP_HUGETLB != 0 {
        return Err(ENOMEM);
    }
    let replace = flags & MAP_FIXED != 0 && flags & MAP_FIXED_NOREPLACE == 0;
    let mut given = flags;
    if replace {
        if !addr.is_multiple_of(PAGE) {
            return Err(EINVAL);
        }
        let end = addr.checked_add(len).ok_or(ENOMEM)?;
        unmap_own(&mut MAP.lock(), addr, end);
        given = (flags & !MAP_FIXED) | MAP_FIXED_NOREPLACE;
    }
    // Below 2 GiB where asked (`MAP_32BIT`), as Linux places it.
    let low = flags & MAP_32BIT != 0 && flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) == 0;
    let hint = if low && addr == 0 { 0x4000_0000 } else { addr };
    let args = [hint as usize, len as usize, prot as usize, (given & !MAP_32BIT) as usize, fd, offset as usize];
    // SAFETY: per the caller.
    let at = match unsafe { posix::host::syscall(nr::MMAP, args) } {
        Ok(at) => at as u64,
        // Linux compatibility's own memory is there: for the program,
        // there is no room.
        Err(EEXIST) if replace => return Err(ENOMEM),
        Err(e) => return Err(e),
    };
    if low && at.saturating_add(len) > 1 << 31 {
        let _ = vm::unmap(None, at as usize, len as usize);
        return Err(ENOMEM);
    }
    let anonymous = flags & MAP_ANONYMOUS != 0;
    let shared = matches!(flags & MAP_TYPE, MAP_SHARED | MAP_SHARED_VALIDATE);
    record(at, len, prot & (PROT_READ | PROT_WRITE | PROT_EXEC), Kind::Mapped { anonymous, shared });
    Ok(at)
}

/// Unmaps the program's areas in `[start, end)`, and forgets them.
fn unmap_own(map: &mut Map, start: u64, end: u64) {
    for (s, e) in map.pieces(start, end) {
        let _ = vm::unmap(None, s as usize, (e - s) as usize);
    }
    map.cut(start, end);
}

/// `munmap`: the program's areas in the range go; there is no error for
/// where it has none.
pub fn munmap(addr: u64, len: u64) -> Result<u64, isize> {
    if !addr.is_multiple_of(PAGE) || len == 0 {
        return Err(EINVAL);
    }
    let end = page_up(len).and_then(|l| addr.checked_add(l)).ok_or(EINVAL)?;
    unmap_own(&mut MAP.lock(), addr, end);
    Ok(0)
}

/// `madvise`: the POSIX layer's, for a range the program's areas cover
/// whole (`ENOMEM` otherwise, as Linux says of a range not mapped), so
/// that the memory it gives up (`MADV_DONTNEED`), or keeps from a child
/// (`MADV_DONTFORK`), is only ever the program's. The areas keep account
/// of what a child has of them.
pub fn madvise(addr: u64, len: u64, advice: u32) -> Result<usize, isize> {
    if !addr.is_multiple_of(PAGE) {
        return Err(EINVAL);
    }
    let end = page_up(len).and_then(|l| addr.checked_add(l)).ok_or(EINVAL)?;
    if end == addr {
        return Ok(0);
    }
    let mut map = MAP.lock();
    if !map.covers(addr, end) {
        return Err(ENOMEM);
    }
    // SAFETY: the program's arguments, for its own areas, which stay its
    // own meanwhile.
    let done = unsafe { posix::host::syscall(nr::MADVISE, [addr as usize, len as usize, advice as usize, 0, 0, 0]) }?;
    let (set, clear) = match advice {
        MADV_DONTFORK => (inherit_flags::OMIT, 0),
        MADV_DOFORK => (0, inherit_flags::OMIT),
        MADV_WIPEONFORK => (inherit_flags::ZERO, 0),
        MADV_KEEPONFORK => (0, inherit_flags::ZERO),
        _ => return Ok(done),
    };
    map.change(addr, end, |a| a.inherit = (a.inherit & !clear) | set);
    Ok(done)
}

/// `mprotect`: the range must be the program's areas, whole (`ENOMEM`
/// otherwise, as Linux says of a range with holes).
pub fn mprotect(addr: u64, len: u64, prot: u32) -> Result<u64, isize> {
    let prot = prot & !(PROT_GROWSDOWN | PROT_GROWSUP);
    if !addr.is_multiple_of(PAGE) || prot & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
        return Err(EINVAL);
    }
    if len == 0 {
        return Ok(0);
    }
    let end = page_up(len).and_then(|l| addr.checked_add(l)).ok_or(ENOMEM)?;
    let mut map = MAP.lock();
    if !map.covers(addr, end) {
        return Err(ENOMEM);
    }
    vm::protect(None, addr as usize, (end - addr) as usize, flags_of(prot)).map_err(|e| match e {
        abi::Error::AccessDenied => EACCES,
        _ => ENOMEM,
    })?;
    map.change(addr, end, |a| a.prot = prot);
    Ok(0)
}

/// Has the kernel give a child `fork` makes what `inherit` says of memory
/// `[at, at + len)`, new and private, which takes the place of an area's
/// (`mremap`'s), as Linux's moves keep it. If it will not, the memory goes
/// (`ENOMEM`).
fn inherits(at: u64, len: u64, inherit: usize) -> Result<(), isize> {
    if inherit == 0 {
        return Ok(());
    }
    vm::inherit(None, at as usize, len as usize, inherit, 0).map_err(|_| {
        let _ = vm::unmap(None, at as usize, len as usize);
        ENOMEM
    })
}

/// `mremap`: shrinks an area in place, grows one of private anonymous
/// memory in place where the range runs to the area's end and the pages
/// after it are free (one area still, as Linux keeps it), or else, with
/// `MREMAP_MAYMOVE`, moves it: its contents copied to new memory, as the
/// kernel's own moves are not had. Only private anonymous memory grows or
/// moves: what is added to a file's pages or to shared memory, or a copy
/// of them, would be the program's alone, not the file's or the others'
/// (`ENOMEM`, as when there is no room, which programs answer with a
/// mapping of their own). What a child has of the area (`MADV_DONTFORK`,
/// `MADV_WIPEONFORK`) it has of the new memory too. The areas are held
/// throughout, as Linux holds a process's: no other thread's call changes
/// the area, or takes away the memory copied, meanwhile.
pub fn mremap(old: u64, old_len: u64, new_len: u64, flags: u32, new_addr: u64) -> Result<u64, isize> {
    if !old.is_multiple_of(PAGE) || flags & !(MREMAP_MAYMOVE | MREMAP_FIXED | MREMAP_DONTUNMAP) != 0 {
        return Err(EINVAL);
    }
    if flags & (MREMAP_FIXED | MREMAP_DONTUNMAP) != 0 && flags & MREMAP_MAYMOVE == 0 {
        return Err(EINVAL);
    }
    let (Some(old_len), Some(new_len)) = (page_up(old_len), page_up(new_len)) else { return Err(EINVAL) };
    if new_len == 0 || old_len == 0 {
        return Err(EINVAL);
    }
    // What `MREMAP_DONTUNMAP` moves leaves its old place empty: the same
    // size, as Linux has it.
    if flags & MREMAP_DONTUNMAP != 0 && old_len != new_len {
        return Err(EINVAL);
    }
    let old_end = old.checked_add(old_len).ok_or(EINVAL)?;
    let mut map = MAP.lock();
    // The range must be one area of the program's.
    let (start, area) = map.holding(old, old_end).map(|(s, a)| (s, a.clone())).ok_or(EFAULT)?;
    let private = matches!(area.kind, Kind::Mapped { anonymous: true, shared: false } | Kind::Heap | Kind::Stack);
    if flags & MREMAP_FIXED == 0 {
        if new_len <= old_len {
            unmap_own(&mut map, old + new_len, old_end);
            return Ok(old);
        }
        // Private anonymous memory grows in place where the range runs to
        // the area's end and the pages after it are free.
        if private && area.kind != Kind::Stack && area.end == old_end {
            let grow = new_len - old_len;
            if vm::allocate(None, grow as usize, old_end as usize, flags_of(area.prot) | map_flags::FIXED).is_ok() {
                inherits(old_end, grow, area.inherit)?;
                map.grown(start, old_end + grow);
                return Ok(old);
            }
        }
        if flags & MREMAP_MAYMOVE == 0 {
            return Err(ENOMEM);
        }
    }
    if !private {
        return Err(ENOMEM);
    }
    // Moved: new memory, the old contents copied in.
    let placement = if flags & MREMAP_FIXED != 0 {
        if !new_addr.is_multiple_of(PAGE) {
            return Err(EINVAL);
        }
        let new_end = new_addr.checked_add(new_len).ok_or(EINVAL)?;
        // Not onto itself, as Linux refuses.
        if new_addr < old_end && old < new_end {
            return Err(EINVAL);
        }
        unmap_own(&mut map, new_addr, new_end);
        new_addr as usize
    } else {
        0
    };
    let fixed = if placement != 0 { map_flags::FIXED } else { 0 };
    let rw = map_flags::READ | map_flags::WRITE;
    let at = vm::allocate(None, new_len as usize, placement, rw | fixed).map_err(|_| ENOMEM)? as u64;
    inherits(at, new_len, area.inherit)?;
    // What the program cannot read is read all the same: it is its memory,
    // which goes with it.
    let unreadable = area.prot & PROT_READ == 0;
    if unreadable {
        let _ = vm::protect(None, old as usize, old_len as usize, map_flags::READ);
    }
    // SAFETY: the old area is the program's, held, and readable now, the
    // new one fresh; they do not overlap (the new one was free memory).
    unsafe { core::ptr::copy_nonoverlapping(old as *const u8, at as *mut u8, old_len.min(new_len) as usize) };
    if area.prot != PROT_READ | PROT_WRITE {
        let _ = vm::protect(None, at as usize, new_len as usize, flags_of(area.prot));
    }
    map.put(at, Area { end: at + new_len, ..area.clone() });
    if flags & MREMAP_DONTUNMAP == 0 {
        unmap_own(&mut map, old, old_end);
    } else {
        // The old place stays, its pages gone with what moved.
        let _ = vm::decommit(None, old as usize, old_len as usize);
        if unreadable {
            let _ = vm::protect(None, old as usize, old_len as usize, flags_of(area.prot));
        }
    }
    Ok(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(areas: &[(u64, u64)]) -> Map {
        let mut m = Map { areas: BTreeMap::new(), brk: (0, 0) };
        for &(s, e) in areas {
            let kind = Kind::Mapped { anonymous: true, shared: false };
            m.areas.insert(s, Area { end: e, prot: PROT_READ, kind, inherit: 0 });
        }
        m
    }

    #[test]
    fn changing_splits_the_areas_it_reaches() {
        let mut m = map(&[(0x1000, 0x5000), (0x6000, 0x8000)]);
        m.change(0x2000, 0x7000, |a| a.prot = PROT_READ | PROT_WRITE);
        let left: Vec<(u64, u64, u32)> = m.areas.iter().map(|(s, a)| (*s, a.end, a.prot)).collect();
        let rw = PROT_READ | PROT_WRITE;
        assert_eq!(
            left,
            [(0x1000, 0x2000, PROT_READ), (0x2000, 0x5000, rw), (0x6000, 0x7000, rw), (0x7000, 0x8000, PROT_READ)]
        );
    }

    #[test]
    fn a_child_has_no_area_kept_from_it() {
        let mut m = map(&[(0x1000, 0x5000), (0x6000, 0x8000)]);
        m.change(0x2000, 0x3000, |a| a.inherit |= inherit_flags::OMIT);
        m.change(0x6000, 0x8000, |a| a.inherit |= inherit_flags::ZERO);
        m.forked();
        let left: Vec<(u64, u64)> = m.areas.iter().map(|(s, a)| (*s, a.end)).collect();
        assert_eq!(left, [(0x1000, 0x2000), (0x3000, 0x5000), (0x6000, 0x8000)]);
    }

    #[test]
    fn the_break_grows_only_an_area_like_its_new_memory() {
        let rw = PROT_READ | PROT_WRITE;
        let mut m = map(&[]);
        m.break_grown(0x1000, 0x2000);
        m.break_grown(0x2000, 0x3000);
        // Not after memory the program protected otherwise, or wiped for a
        // child.
        m.change(0x2000, 0x3000, |a| a.prot = PROT_READ);
        m.break_grown(0x3000, 0x4000);
        m.change(0x3000, 0x4000, |a| a.prot = rw | PROT_EXEC);
        m.break_grown(0x4000, 0x5000);
        m.change(0x4000, 0x5000, |a| a.inherit = inherit_flags::ZERO);
        m.break_grown(0x5000, 0x6000);
        m.break_grown(0x6000, 0x7000);
        let left: Vec<(u64, u64, u32, usize)> = m.areas.iter().map(|(s, a)| (*s, a.end, a.prot, a.inherit)).collect();
        assert_eq!(
            left,
            [
                (0x1000, 0x2000, rw, 0),
                (0x2000, 0x3000, PROT_READ, 0),
                (0x3000, 0x4000, rw | PROT_EXEC, 0),
                (0x4000, 0x5000, rw, inherit_flags::ZERO),
                (0x5000, 0x7000, rw, 0),
            ]
        );
    }

    #[test]
    fn an_area_grown_in_place_is_one_area() {
        // Grown in place twice, as `mremap` grows it: one area, like the
        // one it grew from, which holds the whole, as `mremap` needs of a
        // range.
        let mut m = map(&[(0x1000, 0x2000), (0x5000, 0x6000)]);
        m.change(0x1000, 0x2000, |a| a.inherit = inherit_flags::ZERO);
        m.grown(0x1000, 0x3000);
        m.grown(0x1000, 0x4000);
        let whole = m.holding(0x1000, 0x4000).map(|(s, a)| (s, a.end, a.prot, a.inherit));
        assert_eq!(whole, Some((0x1000, 0x4000, PROT_READ, inherit_flags::ZERO)));
        let left: Vec<(u64, u64)> = m.areas.iter().map(|(s, a)| (*s, a.end)).collect();
        assert_eq!(left, [(0x1000, 0x4000), (0x5000, 0x6000)]);
        // Areas side by side do not hold a range together.
        assert!(map(&[(0x1000, 0x2000), (0x2000, 0x3000)]).holding(0x1000, 0x3000).is_none());
    }

    #[test]
    fn an_empty_range_reaches_no_area() {
        // What `mremap` to the same length unmaps after the range: nothing.
        let mut m = map(&[(0x1000, 0x5000)]);
        m.cut(0x2000, 0x2000);
        m.change(0x3000, 0x3000, |a| a.prot = PROT_READ | PROT_WRITE);
        assert!(m.pieces(0x2000, 0x2000).is_empty());
        let left: Vec<(u64, u64, u32)> = m.areas.iter().map(|(s, a)| (*s, a.end, a.prot)).collect();
        assert_eq!(left, [(0x1000, 0x5000, PROT_READ)]);
    }

    #[test]
    fn cutting_splits_the_areas_it_reaches() {
        let mut m = map(&[(0x1000, 0x5000), (0x6000, 0x8000)]);
        m.cut(0x2000, 0x7000);
        let left: Vec<(u64, u64)> = m.areas.iter().map(|(s, a)| (*s, a.end)).collect();
        assert_eq!(left, [(0x1000, 0x2000), (0x7000, 0x8000)]);
    }

    #[test]
    fn pieces_and_coverage() {
        let m = map(&[(0x1000, 0x3000), (0x3000, 0x4000), (0x6000, 0x8000)]);
        assert_eq!(m.pieces(0x2000, 0x7000), [(0x2000, 0x3000), (0x3000, 0x4000), (0x6000, 0x7000)]);
        assert!(m.covers(0x1000, 0x4000));
        assert!(!m.covers(0x1000, 0x7000));
        assert!(!m.covers(0x0, 0x2000));
    }
}

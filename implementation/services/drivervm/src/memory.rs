// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The guest's memory, as the monitor reaches it.
//!
//! The guest's RAM is one VMO, mapped here as it is mapped at
//! guest-physical address 0. The guest (another processor of it) may write
//! any of it at any moment: what the monitor reads from it, it copies out
//! first and checks after, and it keeps no reference into it. A word the
//! two share as they go (a ring's head and tail) is reached atomically, in
//! the order the protocol needs ([`GuestMemory::load_acquire`],
//! [`GuestMemory::store_release`]).

use core::sync::atomic::{AtomicU32, Ordering};

use rt::vm::Mapping;

/// Plain data: any bytes are a valid value (requests of the bridge).
///
/// # Safety
/// Implementors are `repr(C)` structures of integers, without padding.
pub unsafe trait Plain: Copy + Default {}

// SAFETY: integers and structures of them, without padding (the bridge's
// layouts are checked by size in `hv::bridge`).
unsafe impl Plain for u32 {}
// SAFETY: as above.
unsafe impl Plain for u64 {}
// SAFETY: as above.
unsafe impl Plain for abi::WaitItem {}
macro_rules! plain {
    ($($t:ty),*) => {$(
        // SAFETY: a request of the bridge's or the platform's: integers,
        // no padding.
        unsafe impl Plain for $t {}
    )*};
}
plain!(
    hv::bridge::Setup,
    hv::bridge::Close,
    hv::bridge::Duplicate,
    hv::bridge::ObjectInfo,
    hv::bridge::Signal,
    hv::bridge::Wait,
    hv::bridge::Cancel,
    hv::bridge::ChannelCreate,
    hv::bridge::ChannelWrite,
    hv::bridge::ChannelRead,
    hv::bridge::EventCreate,
    hv::bridge::Vmo,
    hv::bridge::VmoCopy,
    hv::bridge::VmoMap,
    hv::bridge::Bootstrap,
    hv::bridge::Clock,
    hv::bridge::LogRead,
    hv::bridge::Completion,
    hv::platform::Variable
);

/// The guest's RAM, mapped here.
pub struct GuestMemory {
    map: Mapping,
}

impl GuestMemory {
    /// The guest's RAM, as `map` maps it here.
    pub fn new(map: Mapping) -> GuestMemory {
        GuestMemory { map }
    }

    /// The VMO the guest's RAM is.
    pub fn vmo(&self) -> &rt::object::Vmo {
        self.map.vmo()
    }

    /// Its bytes.
    pub fn size(&self) -> u64 {
        self.map.len() as u64
    }

    /// Whether `[gpa, gpa+len)` is RAM.
    pub fn contains(&self, gpa: u64, len: u64) -> bool {
        gpa.checked_add(len).is_some_and(|end| end <= self.size())
    }

    /// Copies guest memory at `gpa` into `out` (`false`: not RAM).
    pub fn read(&self, gpa: u64, out: &mut [u8]) -> bool {
        if !self.contains(gpa, out.len() as u64) {
            return false;
        }
        // SAFETY: inside the mapping (checked); a copy, so what the guest
        // writes meanwhile changes only what was read.
        unsafe { core::ptr::copy_nonoverlapping(self.map.as_ptr().add(gpa as usize), out.as_mut_ptr(), out.len()) };
        true
    }

    /// Copies `data` into guest memory at `gpa` (`false`: not RAM).
    pub fn write(&self, gpa: u64, data: &[u8]) -> bool {
        if !self.contains(gpa, data.len() as u64) {
            return false;
        }
        // SAFETY: as in `read`.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), self.map.as_ptr().add(gpa as usize), data.len()) };
        true
    }

    /// Reads a value of plain data at `gpa`.
    pub fn read_obj<T: Plain>(&self, gpa: u64) -> Option<T> {
        let mut v = T::default();
        // SAFETY: `T` is plain data: its bytes may be anything.
        let bytes = unsafe { core::slice::from_raw_parts_mut(&mut v as *mut T as *mut u8, core::mem::size_of::<T>()) };
        self.read(gpa, bytes).then_some(v)
    }

    /// Writes a value of plain data at `gpa`.
    pub fn write_obj<T: Plain>(&self, gpa: u64, v: &T) -> bool {
        // SAFETY: viewing plain data as bytes.
        let bytes = unsafe { core::slice::from_raw_parts(v as *const T as *const u8, core::mem::size_of::<T>()) };
        self.write(gpa, bytes)
    }

    /// The word the guest shares at `gpa` (4-byte aligned, `None`: not RAM,
    /// or not aligned).
    fn word(&self, gpa: u64) -> Option<&AtomicU32> {
        if !gpa.is_multiple_of(4) || !self.contains(gpa, 4) {
            return None;
        }
        // SAFETY: an aligned word inside the mapping, which lives as long as
        // `self`; the guest writes it as it likes, so it is reached
        // atomically only.
        Some(unsafe { AtomicU32::from_ptr(self.map.as_ptr().add(gpa as usize).cast()) })
    }

    /// Reads the 32-bit word at `gpa` (4-byte aligned), which the guest
    /// writes as it goes: what it wrote before it released the word is seen
    /// after this (an acquire).
    pub fn load_acquire(&self, gpa: u64) -> Option<u32> {
        self.word(gpa).map(|w| w.load(Ordering::Acquire))
    }

    /// Writes the 32-bit word at `gpa` (4-byte aligned), which the guest
    /// reads as it goes: what was written before is seen by the guest once
    /// it sees this (a release). `false`: not RAM, or not aligned.
    pub fn store_release(&self, gpa: u64, value: u32) -> bool {
        self.word(gpa).map(|w| w.store(value, Ordering::Release)).is_some()
    }
}

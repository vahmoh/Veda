//! Page tables, as the rest of the kernel sees them: what a mapping allows
//! ([`Flags`]), how its memory is cached ([`Cache`]), and the kernel's own
//! address space. The tables themselves, and an address space's root, are
//! the architecture's (`arch::paging`).

pub use crate::arch::paging::{activate, free_user_root, kernel_root, map, new_user_root, protect, translate, unmap};

use super::PAGE_SIZE;
use crate::arch;

/// Memory type for a mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cache {
    WriteBack,
    WriteCombining,
    Uncached,
}

/// Leaf mapping attributes.
#[derive(Debug, Clone, Copy)]
pub struct Flags {
    pub writable: bool,
    pub executable: bool,
    pub user: bool,
    pub global: bool,
    pub cache: Cache,
}

impl Flags {
    pub const KERNEL_RW: Flags =
        Flags { writable: true, executable: false, user: false, global: true, cache: Cache::WriteBack };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    NoMemory,
    AlreadyMapped,
}

/// Builds the kernel address space: the direct map of physical memory
/// (`arch::paging::new_kernel_space`), and the kernel image with
/// per-section permissions. Then switches to it.
pub fn init_kernel_space(boot: &bootinfo::BootInfo) {
    let root = arch::paging::new_kernel_space(boot.hhdm_limit);
    // Kernel image: walk our own PE section table (the headers are mapped at
    // the image base).
    let k = &boot.kernel;
    // SAFETY: the loader mapped the whole image, headers included.
    let image = unsafe { core::slice::from_raw_parts(k.virt_base as *const u8, k.size as usize) };
    let pe = vpe::PeImage::parse(image).expect("kernel image headers are corrupt");
    let mut off = 0;
    while off < k.size {
        let rva = off as u32;
        let mut flags =
            Flags { writable: false, executable: false, user: false, global: true, cache: Cache::WriteBack };
        for s in pe.sections() {
            let end = s.virtual_address + s.virtual_size.next_multiple_of(PAGE_SIZE as u32);
            if rva >= s.virtual_address && rva < end {
                flags.writable |= s.writable();
                flags.executable |= s.executable();
            }
        }
        map(root, k.virt_base + off, k.phys_base + off, flags, true).expect("mapping the kernel image");
        off += PAGE_SIZE;
    }
    arch::paging::install_kernel_space(root);
}

/// Loads the kernel's own address space on this CPU (for threads that have
/// none of their own).
pub fn activate_kernel() {
    activate(kernel_root());
}

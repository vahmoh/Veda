// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Page tables, as the rest of the kernel sees them: what a mapping allows
//! ([`Flags`]), how its memory is cached ([`Cache`]), and the kernel's own
//! address space. The tables themselves, and an address space's root, are
//! the architecture's (`architecture::paging`).

pub use crate::architecture::paging::{
    activate, free_user_root, kernel_root, map, new_user_root, protect_range, translate, unmap, unmap_range,
};

use super::PAGE_SIZE;
use crate::architecture;

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
/// (`architecture::paging::new_kernel_space`), and the kernel image with
/// the permissions the loader found for its pages
/// (`bootinfo::KernelImage`). Then switches to it.
pub fn init_kernel_space(boot: &bootinfo::BootInfo) {
    let root = architecture::paging::new_kernel_space(boot.hhdm_limit);
    let k = &boot.kernel;
    for r in k.runs() {
        let flags = Flags {
            writable: r.writable,
            executable: r.executable,
            user: false,
            global: true,
            cache: Cache::WriteBack,
        };
        for off in (r.offset..r.offset + r.len).step_by(PAGE_SIZE as usize) {
            map(root, k.virt_base + off, k.phys_base + off, flags, true).expect("mapping the kernel image");
        }
    }
    architecture::paging::install_kernel_space(root);
}

/// Loads the kernel's own address space on this CPU (for threads that have
/// none of their own).
pub fn activate_kernel() {
    activate(kernel_root());
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The loader on AMD64: the page tables it builds for the kernel (x86-64's
//! four levels, `paging`), the framebuffer's memory type and painting it
//! write-combining (`memtype`, `blank`), COM1 (`serial`), the TSC, and the
//! jump into the kernel.

mod blank;
mod memtype;
mod paging;
pub mod serial;

pub use blank::clear_write_combining;
pub use memtype::memory_type;

use crate::FrameSource;

/// The direct map covers all RAM, and at least the low 4 GiB, where a PC's
/// devices' registers are (the MMIO hole).
pub const DIRECT_MAP_MIN: u64 = 4 << 30;

/// The clock counter's reading (the TSC), which the kernel's clock counts
/// too: the loader's times are in its ticks.
pub fn ticks() -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: reading the timestamp counter.
    unsafe { core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    (hi as u64) << 32 | lo as u64
}

fn read_msr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: reading an architectural MSR.
    unsafe { core::arch::asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    (hi as u64) << 32 | lo as u64
}

fn write_msr(msr: u32, value: u64) {
    // SAFETY: writing an architectural MSR with a valid value.
    unsafe {
        core::arch::asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nomem, nostack))
    };
}

fn supports_1g_pages() -> bool {
    let edx: u32;
    // SAFETY: CPUID is always available on x86-64. rbx is reserved by LLVM,
    // so it is saved manually.
    unsafe {
        core::arch::asm!("mov {tmp}, rbx", "cpuid", "mov rbx, {tmp}", tmp = out(reg) _,
            inout("eax") 0x8000_0001u32 => _, inout("ecx") 0 => _, out("edx") edx, options(nostack));
    }
    edx & (1 << 26) != 0
}

/// The kernel's address space, as the loader hands it over (see
/// `bootinfo::arch`): physical memory below the direct map's limit both
/// where it is (for the loader's last steps) and at `bootinfo::HHDM_BASE`,
/// the two sharing their tables, the latter not executable; and the kernel
/// image ([`KernelSpace::map_kernel_page`]).
pub struct KernelSpace<'a, F: FrameSource> {
    pt: paging::PageTables<'a, F>,
}

impl<'a, F: FrameSource> KernelSpace<'a, F> {
    pub fn new(frames: &'a mut F, direct_map_limit: u64) -> Result<Self, &'static str> {
        let mut pt = paging::PageTables::new(frames, supports_1g_pages())?;
        // Identity map (slot 0) and direct map (slot 256) share the same
        // tables; the direct map is additionally non-executable. The
        // identity map must not be global: the kernel will drop it.
        pt.map_linear(0, direct_map_limit, paging::WRITABLE)?;
        pt.alias_pml4_slot(256, 0, paging::NO_EXECUTE);
        Ok(KernelSpace { pt })
    }

    /// Maps a page of the kernel image (global: the kernel keeps it).
    pub fn map_kernel_page(
        &mut self,
        virt: u64,
        phys: u64,
        writable: bool,
        executable: bool,
    ) -> Result<(), &'static str> {
        let mut flags = paging::GLOBAL;
        if writable {
            flags |= paging::WRITABLE;
        }
        if !executable {
            flags |= paging::NO_EXECUTE;
        }
        self.pt.map_4k(virt, phys, flags)
    }

    /// The address space's root (its PML4), for [`enter_kernel`].
    pub fn root(&self) -> u64 {
        self.pt.pml4
    }
}

/// Enters the kernel at `entry`, which takes `info`, on the stack whose top
/// is `stack_top`, in the address space `root` ([`KernelSpace`]), as
/// `bootinfo::arch` says it is entered.
///
/// # Safety
/// After `ExitBootServices`; `root` maps this code where it is, the stack
/// and the kernel, and `entry` follows the documented contract.
pub unsafe fn enter_kernel(root: u64, stack_top: u64, entry: u64, info: u64) -> ! {
    // No-execute support (the tables' NX bits need it).
    const IA32_EFER: u32 = 0xC000_0080;
    write_msr(IA32_EFER, read_msr(IA32_EFER) | (1 << 11));
    // SAFETY: enabling supervisor write protection (CR0.WP) with interrupts
    // masked; the firmware's page tables are still active and writable.
    unsafe {
        core::arch::asm!("cli", "mov {tmp}, cr0", "or {tmp}, 0x10000", "mov cr0, {tmp}", tmp = out(reg) _, options(nostack));
    }
    // SAFETY: the new tables identity-map this code, map the stack and the
    // kernel, as the caller guarantees.
    unsafe {
        core::arch::asm!(
            "mov cr3, {cr3}",
            "mov rsp, {stack}",
            "xor ebp, ebp",
            "push 0",               // fake return address keeps the ABI alignment
            "jmp {entry}",
            cr3 = in(reg) root,
            stack = in(reg) stack_top,
            entry = in(reg) entry,
            in("rdi") info,
            options(noreturn)
        );
    }
}

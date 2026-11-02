// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The loader on ARM64: the translation tables it builds for the kernel
//! (`paging`), the generic timer's counter, the UART the firmware's tables
//! name (`serial`), and the jump into the kernel.
//!
//! The kernel is entered with the processor as `bootinfo::architecture`
//! says, wherever firmware left it, and with its MMU turned off on the way:
//! no two sets of translations of the same addresses are ever in use, nor
//! does a TLB entry of firmware's outlive its tables.

mod paging;
pub mod serial;

use core::arch::{asm, global_asm};
use core::fmt;

use bootinfo::architecture::{
    CNTHCTL_EL1, CPTR_EL1, HCR_HOST, HCR_RW, HHDM_BASE, ICC_SRE, MAIR, SCTLR, SCTLR_MMU_AND_CACHES, SCTLR_OFF,
    SPSR_EL1H, TCR,
};
use bootinfo::memory_type;

use crate::blank::Surface;
use crate::{FrameSource, Ram};

/// The direct map covers what the firmware's map calls memory, wherever it
/// is: this much of the address space, whatever the machine has.
pub const DIRECT_MAP_MIN: u64 = 1 << 30;

/// The clock counter's reading, which the kernel's clock counts too: the
/// loader's times are in its ticks.
pub fn ticks() -> u64 {
    let ticks: u64;
    // SAFETY: reading the generic timer's virtual counter.
    unsafe { asm!("isb", "mrs {}, cntvct_el0", out(reg) ticks, options(nomem, nostack, preserves_flags)) };
    ticks
}

/// The memory type of the page at `address` in the firmware's address
/// space, which the loader does not read back from its tables.
pub fn memory_type(_address: u64) -> u8 {
    memory_type::UNKNOWN
}

/// Clears the screen write-combining: not on ARM64, where the firmware maps
/// the framebuffer as it should already, as memory of its own.
pub fn clear_write_combining<F: FrameSource>(_frames: &mut F, _s: &Surface, _limit: u64) -> Result<u64, &'static str> {
    Err("the loader paints through the firmware's mapping")
}

/// The line size of the data and of the instruction caches, the smallest
/// each has.
fn cache_lines() -> (u64, u64) {
    let ctr: u64;
    // SAFETY: reading the cache type register.
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack, preserves_flags)) };
    (4 << ((ctr >> 16) & 0xF), 4 << (ctr & 0xF))
}

/// `[base, base + len)` of memory, identity-mapped, now holds code: makes
/// the instruction caches see what was written there, which they would not
/// by themselves.
pub fn code_written(base: u64, len: u64) {
    let (data, instruction) = cache_lines();
    let end = base + len;
    // SAFETY: cleaning and invalidating cache lines of mapped memory
    // changes nothing it holds.
    unsafe {
        for line in (base & !(data - 1)..end).step_by(data as usize) {
            asm!("dc cvau, {}", in(reg) line, options(nostack, preserves_flags));
        }
        asm!("dsb ish", options(nostack, preserves_flags));
        for line in (base & !(instruction - 1)..end).step_by(instruction as usize) {
            asm!("ic ivau, {}", in(reg) line, options(nostack, preserves_flags));
        }
        asm!("dsb ish", "isb", options(nostack, preserves_flags));
    }
}

/// What the processor's translation is entered on: the tables of the low
/// half of addresses and of the high half.
#[derive(Debug, Clone, Copy)]
pub struct Root {
    low: u64,
    high: u64,
}

impl fmt::LowerHex for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.high, f)
    }
}

/// The kernel's address space, as the loader hands it over (see
/// `bootinfo::architecture`): the memory `ram` holds both where it is (for
/// the loader's last steps) and at `bootinfo::HHDM_BASE`, the latter not
/// executable; and the kernel image ([`KernelSpace::map_kernel_page`]).
pub struct KernelSpace<'a, F: FrameSource> {
    frames: &'a mut F,
    low: paging::Tables,
    high: paging::Tables,
}

impl<'a, F: FrameSource> KernelSpace<'a, F> {
    pub fn new(frames: &'a mut F, _direct_map_limit: u64, ram: &Ram) -> Result<Self, &'static str> {
        let mut low = paging::Tables::new(frames)?;
        let mut high = paging::Tables::new(frames)?;
        for (start, end) in ram.ranges() {
            low.map_range(frames, start, start, end - start, paging::WRITABLE | paging::NOT_GLOBAL)?;
            high.map_range(frames, HHDM_BASE + start, start, end - start, paging::WRITABLE | paging::NO_EXECUTE)?;
        }
        Ok(KernelSpace { frames, low, high })
    }

    /// Maps a page of the kernel image (global: the kernel keeps it).
    pub fn map_kernel_page(
        &mut self,
        virt: u64,
        phys: u64,
        writable: bool,
        executable: bool,
    ) -> Result<(), &'static str> {
        let mut flags = 0;
        if writable {
            flags |= paging::WRITABLE;
        }
        if !executable {
            flags |= paging::NO_EXECUTE;
        }
        self.high.map_4k(self.frames, virt, phys, flags)
    }

    /// The address space's root, for [`enter_kernel`].
    pub fn root(&self) -> Root {
        Root { low: self.low.root(), high: self.high.root() }
    }
}

unsafe extern "C" {
    fn veda_enter_kernel(info: u64, stack_top: u64, entry: u64, low: u64, high: u64) -> !;
}

/// Enters the kernel at `entry`, which takes `info`, on the stack whose top
/// is `stack_top`, in the address space `root` ([`KernelSpace`]), as
/// `bootinfo::architecture` says it is entered.
///
/// # Safety
/// After `ExitBootServices`; `root` maps this code where it is, the stack
/// and the kernel, and `entry` follows the documented contract.
pub unsafe fn enter_kernel(root: Root, stack_top: u64, entry: u64, info: u64) -> ! {
    // SAFETY: as the caller guarantees.
    unsafe { veda_enter_kernel(info, stack_top, entry, root.low, root.high) }
}

// `x0` to `x4`: as `veda_enter_kernel` declares them. Memory has to hold
// this code, as it goes on with the caches off: the caches hold on to what
// they have meanwhile, which is all of the rest, tables included, for
// when they come on again.
global_asm!(
    r#"
    .section .text
    .balign 8
veda_enter_kernel:
    msr     daifset, #0xf
    adr     x5, veda_enter_kernel
    adr     x6, 9f
    mrs     x7, ctr_el0
    ubfx    x7, x7, #16, #4
    mov     x8, #4
    lsl     x7, x8, x7
    sub     x8, x7, #1
    bic     x5, x5, x8
1:
    dc      cvac, x5
    add     x5, x5, x7
    cmp     x5, x6
    b.lo    1b
    dsb     sy
    ldr     x6, ={mmu_and_caches}
    mrs     x5, CurrentEL
    cmp     x5, #(2 << 2)
    b.ne    2f
    mrs     x5, sctlr_el2
    bic     x5, x5, x6
    msr     sctlr_el2, x5
    b       3f
2:
    mrs     x5, sctlr_el1
    bic     x5, x5, x6
    msr     sctlr_el1, x5
3:
    isb
"#,
    bootinfo::set_up_processor!(),
    r#"
    mov     sp, x1
    mov     x29, #0
    mov     x30, #0
    br      x2
    .ltorg
9:
"#,
    mmu_and_caches = const SCTLR_MMU_AND_CACHES,
    hcr_host = const HCR_HOST,
    hcr_rw = const HCR_RW,
    icc_sre = const ICC_SRE,
    cnthctl_el1 = const CNTHCTL_EL1,
    cptr_el1 = const CPTR_EL1,
    spsr_el1h = const SPSR_EL1H,
    sctlr_off = const SCTLR_OFF,
    mair = const MAIR,
    tcr = const TCR,
    sctlr = const SCTLR,
);

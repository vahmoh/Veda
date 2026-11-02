// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The boot handoff protocol on ARM64.
//!
//! The loader enters the kernel through `extern "C" fn(&'static BootInfo)
//! -> !` (the `BootInfo` in `x0`), with every exception masked and the MMU
//! and the caches on, in one of two ways that the kernel's code does not
//! tell apart: at EL2 with the Virtualization Host Extensions, where the
//! `*_EL1` system registers name EL2's and the kernel can run virtual
//! machines, or at EL1, where firmware or a hypervisor has EL2, or the
//! processor has no VHE. [`set_up_processor!`] gets there, from wherever
//! firmware left the processor, for the boot processor and the others
//! alike.
//!
//! Addresses split the way the kernel's and programs' address spaces do:
//! `TTBR0` holds the low half (here the loader's map of memory where it
//! is), `TTBR1` the kernel's: the direct map, at [`HHDM_BASE`], covers the
//! memory the firmware's map calls RAM and nothing else, as a processor may
//! fetch from anything mapped as memory, and the kernel is in the top
//! 2 GiB, at [`KERNEL_BASE`]. Both are as on AMD64.

/// Virtual base address of the higher-half direct map of physical memory.
pub const HHDM_BASE: u64 = 0xFFFF_8000_0000_0000;

/// Link address of the kernel image.
pub const KERNEL_BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// The memory types, as indices of [`MAIR`] that translation table entries
/// name.
pub mod memory_attribute {
    /// Normal memory, write-back.
    pub const WRITE_BACK: u64 = 0;
    /// Normal memory, not cached: the writes of a burst may gather, as
    /// write-combining memory's do on other processors.
    pub const NON_CACHEABLE: u64 = 1;
    /// Device memory: devices' registers, whose accesses stay in order
    /// and whole. A write may be acknowledged on its way (Device-nGnRE).
    pub const DEVICE: u64 = 2;
}

/// `MAIR_EL1`: what each index of [`memory_attribute`] means: Normal
/// write-back (inner and outer, allocating), Normal non-cacheable,
/// Device-nGnRE.
pub const MAIR: u64 = 0xFF | 0x44 << 8 | 0x04 << 16;

/// `TCR_EL1`, but for the sizes that depend on the processor, which
/// [`set_up_processor!`] fills in: 48-bit virtual addresses in both halves
/// (`T0SZ`, `T1SZ`), 4 KiB granules (`TG0`, `TG1`), and page tables in
/// write-back memory, shared with the other processors (`IRGN`, `ORGN`,
/// `SH`), so that walks see what the caches hold.
pub const TCR: u64 = 16 | 1 << 8 | 1 << 10 | 3 << 12 | 16 << 16 | 1 << 24 | 1 << 26 | 3 << 28 | 2 << 30;

/// `SCTLR_EL1`: the MMU and the caches on (`M`, `C`, `I`), the stack
/// pointer's alignment checked (`SA`, `SA0`). Programs may wait for events
/// (`nTWE`, `nTWI`), zero and clean cache lines, read the cache's geometry
/// and make their own code run (`DZE`, `UCT`, `UCI`), as a compiler's run
/// time does. The rest is what the architecture wants written as 1
/// (`EOS`, `TSCXT`, `EIS`, `SPAN`, `nTLSMD`, `LSMAOE`); the kernel clears
/// `SPAN` where there is PAN, as [`set_up_processor!`] cannot tell.
pub const SCTLR: u64 = 1
    | 1 << 2
    | 1 << 3
    | 1 << 4
    | 1 << 11
    | 1 << 12
    | 1 << 14
    | 1 << 15
    | 1 << 16
    | 1 << 18
    | 1 << 20
    | 1 << 22
    | 1 << 23
    | 1 << 26
    | 1 << 28
    | 1 << 29;

/// `HCR_EL2` while the kernel runs at EL2: the Virtualization Host
/// Extensions (`E2H`), with this processor's exceptions and its programs
/// its own (`TGE`), and those programs 64-bit (`RW`). `HCR_EL2` to run EL1
/// on is `RW` alone.
pub const HCR_HOST: u64 = 1 << 34 | 1 << 31 | 1 << 27;
/// `HCR_EL2`'s `RW`: EL1 is 64-bit.
pub const HCR_RW: u64 = 1 << 31;

/// `ICC_SRE_EL2`: the GIC's system registers, for EL2 (`SRE`) and EL1
/// (`Enable`).
pub const ICC_SRE: u64 = 1 << 3 | 1;

/// `CNTHCTL_EL2` to run EL1 on: its access to the physical counter and
/// timer does not trap.
pub const CNTHCTL_EL1: u64 = 3;

/// `CPTR_EL2` to run EL1 on (without `E2H`): only what the architecture
/// wants written as 1, so that its floating point does not trap.
pub const CPTR_EL1: u64 = 0x33FF;

/// `SPSR_EL2` to enter EL1 with: its own stack pointer, every exception
/// masked.
pub const SPSR_EL1H: u64 = 0xF << 6 | 5;

/// `SCTLR_EL1` before EL1's MMU is on: what the architecture wants written
/// as 1 ([`SCTLR`]), and nothing else.
pub const SCTLR_OFF: u64 = 1 << 11 | 1 << 20 | 1 << 22 | 1 << 23 | 1 << 28 | 1 << 29;

/// `SCTLR`'s `M`, `C` and `I`: what turns the MMU and the caches on, at any
/// exception level.
pub const SCTLR_MMU_AND_CACHES: u64 = 1 | 1 << 2 | 1 << 12;

/// The assembly that takes a processor from where firmware left it, in
/// EL2 or EL1 with its MMU off, to where the kernel runs ([the module's
/// introduction](self)), its MMU on: with `x3` the table of addresses'
/// low half (`TTBR0`) and `x4` of their high half (`TTBR1`), each a
/// physical address, with an ASID of 0. It keeps `x0` to `x4`, and clobbers
/// `x5` to `x7`. It is `global_asm!` text, with these operands:
/// `hcr_host`, `hcr_rw`, `icc_sre`, `cnthctl_el1`, `cptr_el1`, `spsr_el1h`,
/// `sctlr_off`, `mair`, `tcr` and `sctlr` ([`HCR_HOST`] and the rest).
///
/// The registers it writes are only those of the exception level the kernel
/// leaves behind and those the `*_EL1` names lead to: at EL2 they take
/// EL2's meaning once `E2H` is set, which is why it is set first.
#[macro_export]
macro_rules! set_up_processor {
    () => {
        r#"
    mrs     x5, CurrentEL
    cmp     x5, #(2 << 2)
    b.ne    92f
    // At EL2. What EL2 keeps from EL1 and programs goes first.
    mov     x5, {icc_sre}
    mrs     x6, id_aa64pfr0_el1
    ubfx    x6, x6, #24, #4
    cbz     x6, 90f
    msr     s3_4_c12_c9_5, x5           // ICC_SRE_EL2
    isb
90:
    msr     cntvoff_el2, xzr
    msr     vttbr_el2, xzr
    msr     hstr_el2, xzr
    mrs     x5, id_aa64mmfr1_el1
    ubfx    x5, x5, #8, #4
    cbz     x5, 91f
    // The Virtualization Host Extensions: the kernel stays here.
    ldr     x5, ={hcr_host}
    msr     hcr_el2, x5
    isb
    b       92f
91:
    // None: EL1 is the kernel's, and EL2 lies unused.
    ldr     x5, ={hcr_rw}
    msr     hcr_el2, x5
    mov     x5, {cnthctl_el1}
    msr     cnthctl_el2, x5
    mov     x5, {cptr_el1}
    msr     cptr_el2, x5
    ldr     x5, ={sctlr_off}
    msr     sctlr_el1, x5
    mov     x5, {spsr_el1h}
    msr     spsr_el2, x5
    adr     x5, 92f
    msr     elr_el2, x5
    eret
92:
    // The same from here on, at EL2 with E2H set or at EL1. Physical
    // addresses are as wide as the processor has them, up to what 4 KiB
    // granules reach without LPA2; ASIDs as many as it has.
    ldr     x5, ={mair}
    msr     mair_el1, x5
    ldr     x5, ={tcr}
    mrs     x6, id_aa64mmfr0_el1
    ubfx    x7, x6, #0, #4
    cmp     x7, #5
    b.ls    93f
    mov     x7, #5
93:
    bfi     x5, x7, #32, #3
    ubfx    x7, x6, #4, #4
    cbz     x7, 94f
    orr     x5, x5, #(1 << 36)
94:
    msr     tcr_el1, x5
    msr     ttbr0_el1, x3
    msr     ttbr1_el1, x4
    isb
    tlbi    vmalle1
    ic      iallu
    dsb     nsh
    isb
    ldr     x5, ={sctlr}
    msr     sctlr_el1, x5
    isb
"#
    };
}

/// The processor's memory types, as the loader reports the framebuffer's.
pub mod memory_type {
    pub const UNCACHED: u8 = 0;
    pub const WRITE_COMBINING: u8 = 1;
    pub const WRITE_BACK: u8 = 6;
    /// The loader could not tell.
    pub const UNKNOWN: u8 = 0xFF;

    /// A name for the log.
    pub fn name(t: u8) -> &'static str {
        match t {
            UNCACHED => "uncached",
            WRITE_COMBINING => "write-combining",
            WRITE_BACK => "write-back",
            _ => "of an unknown memory type",
        }
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! CPU identification, system registers and per-CPU feature initialisation.
//!
//! The kernel runs at EL1, or at EL2 with the Virtualization Host Extensions
//! (see `bootinfo::architecture`): the code does not tell them apart, as
//! the `*_EL1` registers it names are then EL2's. What does differ (which
//! timer is the kernel's, whether it can run guests) depends on
//! [`at_el2`].

use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};

/// Reads a system register.
macro_rules! read_register {
    ($name:tt) => {{
        let value: u64;
        // SAFETY: reading the system registers this kernel names has no
        // side effects. (It may be in an `unsafe` block already.)
        #[allow(unused_unsafe)]
        unsafe {
            core::arch::asm!(concat!("mrs {}, ", $name), out(reg) value, options(nomem, nostack, preserves_flags))
        };
        value
    }};
}

/// Writes a system register, inside `unsafe`: what it changes is the
/// caller's to answer for. A barrier to the compiler, as what it changes
/// may read memory written before.
macro_rules! write_register {
    ($name:tt, $value:expr) => {
        core::arch::asm!(concat!("msr ", $name, ", {}"), in(reg) $value as u64, options(nostack, preserves_flags))
    };
}

pub(in crate::architecture) use {read_register, write_register};

/// The `CurrentEL` value of EL2.
const CURRENT_EL2: u64 = 2 << 2;

/// `CPACR_EL1.FPEN`: floating point and SIMD do not trap.
const CPACR_FP: u64 = 3 << 20;
/// `CNTKCTL_EL1.EL0VCTEN`: programs read the virtual counter.
const CNTKCTL_EL0_COUNTER: u64 = 1 << 1;
/// `SCTLR_EL1.SPAN`: 0 sets PAN on every exception taken.
const SCTLR_SPAN: u64 = 1 << 23;

#[inline]
pub fn halt() {
    // SAFETY: WFI waits for the next interrupt.
    unsafe { asm!("wfi", options(nomem, nostack, preserves_flags)) };
}

/// Waits for an interrupt: an interrupt that comes while they are masked
/// still ends `WFI`, so none can be lost before it. Then takes it, and
/// masks interrupts again. The interrupts' handlers change memory
/// meanwhile: a barrier to the compiler.
#[inline]
pub fn wait_for_interrupt() {
    // SAFETY: masked interrupts end WFI, and are taken when unmasked.
    unsafe { asm!("dsb sy", "wfi", "msr daifclr, #2", "isb", "msr daifset, #2", options(nostack)) };
}

/// Takes the interrupts that are pending (unmasks them for a moment), then
/// masks them again; their handlers change memory: a barrier to the
/// compiler.
#[inline]
pub(in crate::architecture) fn take_pending_interrupts() {
    // SAFETY: pending interrupts are taken and their handlers return here.
    unsafe { asm!("msr daifclr, #2", "isb", "msr daifset, #2", options(nostack)) };
}

/// A barrier to the compiler: what follows stays where interrupts are off.
#[inline]
pub fn disable_interrupts() {
    // SAFETY: masking interrupts is always safe.
    unsafe { asm!("msr daifset, #2", options(nostack)) };
}

#[inline]
pub fn interrupts_enabled() -> bool {
    read_register!("daif") & (1 << 7) == 0
}

/// Halts this CPU forever.
pub fn halt_forever() -> ! {
    loop {
        disable_interrupts();
        halt();
    }
}

/// What the boot processor has, which the others are taken to have too.
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::architecture) struct Features {
    /// The kernel runs at EL2.
    pub at_el2: bool,
    /// Privileged Access Never, the kernel's guard against reaching user
    /// memory by mistake.
    pub pan: bool,
    /// 16-bit ASIDs (otherwise 8-bit).
    pub asid16: bool,
    /// The random number registers (`RNDR`, `RNDRRS`).
    pub rng: bool,
}

static FEATURES_READY: AtomicBool = AtomicBool::new(false);
static mut FEATURES: Features = Features { at_el2: false, pan: false, asid16: false, rng: false };

/// Logs the features [`detect_features`] found.
pub fn report() {
    let f = features();
    crate::kinfo!(
        "cpu: {} pan={} asid-bits={} rng={}",
        if f.at_el2 { "EL2 with VHE" } else { "EL1" },
        f.pan,
        if f.asid16 { 16 } else { 8 },
        f.rng
    );
}

/// Returns the features detected by [`detect_features`].
pub(in crate::architecture) fn features() -> Features {
    debug_assert!(FEATURES_READY.load(Ordering::Acquire));
    // SAFETY: written once during early boot before any reader exists.
    unsafe { *core::ptr::addr_of!(FEATURES) }
}

/// Whether the kernel runs at EL2, where it can run guests.
pub(in crate::architecture) fn at_el2() -> bool {
    features().at_el2
}

/// Reads the ID registers once on the bootstrap processor.
pub(in crate::architecture) fn detect_features() {
    let mmfr0 = read_register!("id_aa64mmfr0_el1");
    let mmfr1 = read_register!("id_aa64mmfr1_el1");
    let isar0 = read_register!("id_aa64isar0_el1");
    let f = Features {
        at_el2: read_register!("CurrentEL") == CURRENT_EL2,
        pan: (mmfr1 >> 20) & 0xF != 0,
        asid16: (mmfr0 >> 4) & 0xF != 0,
        rng: (isar0 >> 60) & 0xF != 0,
    };
    // SAFETY: single writer during early boot.
    unsafe { *core::ptr::addr_of_mut!(FEATURES) = f };
    FEATURES_READY.store(true, Ordering::Release);
}

/// Configures the current CPU: where exceptions go, floating point and SIMD
/// for user space, programs reading the clock's counter, and PAN, on from
/// here on and set by every exception taken.
pub(in crate::architecture) fn init_cpu_features() {
    let f = features();
    // SAFETY: only what ID registers reported as present, and what the
    // kernel's exception vectors are ready for.
    unsafe {
        write_register!("vbar_el1", super::entry::vectors());
        write_register!("cpacr_el1", CPACR_FP);
        write_register!("cntkctl_el1", CNTKCTL_EL0_COUNTER);
        // Nothing of debugging or of counters' for programs, whatever
        // firmware left: `MDCR_EL2` goes further where the kernel is at EL2.
        write_register!("mdscr_el1", 0u64);
        if !matches!((read_register!("id_aa64dfr0_el1") >> 8) & 0xF, 0 | 0xF) {
            write_register!("pmuserenr_el0", 0u64);
        }
        if (read_register!("id_aa64pfr0_el1") >> 44) & 0xF != 0 {
            write_register!("s3_3_c13_c2_3", 0u64); // AMUSERENR_EL0
        }
        asm!("isb", options(nostack, preserves_flags));
        if f.pan {
            write_register!("sctlr_el1", read_register!("sctlr_el1") & !SCTLR_SPAN);
            set_pan(true);
        }
        asm!("isb", options(nostack, preserves_flags));
    }
}

/// Sets `PSTATE.PAN`, which keeps the kernel from user memory.
///
/// # Safety
/// The processor must have PAN.
#[inline]
pub(in crate::architecture) unsafe fn set_pan(on: bool) {
    // SAFETY: per the caller. The instructions (`MSR PAN, #imm`) are
    // spelled out, as the kernel's target says nothing of PAN. A barrier to
    // the compiler: the accesses it allows stay inside it.
    unsafe {
        if on {
            asm!(".inst 0xd500419f", options(nostack, preserves_flags));
        } else {
            asm!(".inst 0xd500409f", options(nostack, preserves_flags));
        }
    }
}

/// `MPIDR_EL1`'s affinity fields, which name the processor.
#[inline]
pub(in crate::architecture) fn affinity() -> u64 {
    read_register!("mpidr_el1") & 0xFF_00FF_FFFF
}

/// What programs are told of the processor (`abi::arm64::hwcap`).
pub fn program_features() -> [u64; 2] {
    abi::arm64::hwcap::of(&abi::arm64::hwcap::IdRegisters {
        pfr0: read_register!("id_aa64pfr0_el1"),
        pfr1: read_register!("id_aa64pfr1_el1"),
        isar0: read_register!("id_aa64isar0_el1"),
        isar1: read_register!("id_aa64isar1_el1"),
        mmfr2: read_register!("id_aa64mmfr2_el1"),
    })
}

/// Names the processor by `MIDR_EL1` ("Arm part d0c r3p1"), as ARM64
/// processors have no name of their own to give.
pub fn brand_string() -> [u8; 48] {
    use core::fmt::Write;

    struct Name {
        bytes: [u8; 48],
        len: usize,
    }
    impl Write for Name {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for &b in s.as_bytes() {
                if self.len < self.bytes.len() {
                    self.bytes[self.len] = b;
                    self.len += 1;
                }
            }
            Ok(())
        }
    }

    let midr = read_register!("midr_el1");
    let maker = match (midr >> 24) & 0xFF {
        0x00 => "QEMU",
        0x41 => "Arm",
        0x42 => "Broadcom",
        0x43 => "Cavium",
        0x46 => "Fujitsu",
        0x48 => "HiSilicon",
        0x4E => "NVIDIA",
        0x50 => "Applied Micro",
        0x51 => "Qualcomm",
        0x61 => "Apple",
        0xC0 => "Ampere",
        _ => "Unknown",
    };
    let (part, variant, revision) = ((midr >> 4) & 0xFFF, (midr >> 20) & 0xF, midr & 0xF);
    let mut name = Name { bytes: [0; 48], len: 0 };
    let _ = write!(name, "{maker} part {part:03x} r{variant}p{revision}");
    name.bytes
}

/// A source of random numbers of the processor's: its name, and how to read
/// 64 bits from it (`None`: the processor has none, or it keeps failing).
pub type RandomSource = (&'static str, fn() -> Option<u64>);

/// The processor's sources of random numbers, conditioned seeds first.
pub const RANDOM_SOURCES: [RandomSource; 2] = [("RNDRRS", rndrrs), ("RNDR", rndr)];

/// Reads one of the random number registers into `x0`: `Z` is clear if it
/// gave a number. The `mrs` is spelled by its encoding, as the kernel's
/// target says nothing of them.
macro_rules! random_register {
    ($encoding:literal) => {{
        let (v, failed): (u64, u64);
        // SAFETY: the register exists (the caller checked), and reading it
        // has no effect but on the flags.
        unsafe {
            asm!(concat!(".inst ", $encoding), "cset {failed}, eq", failed = out(reg) failed, out("x0") v,
                options(nomem, nostack));
        }
        (failed == 0).then_some(v)
    }};
}

/// Reads 64 bits a conditioned source has just seeded (`RNDRRS`), or `None`
/// if the processor has none or it keeps failing.
fn rndrrs() -> Option<u64> {
    if !features().rng {
        return None;
    }
    (0..64).find_map(|_| random_register!("0xd53b2420"))
}

/// Reads 64 bits of the processor's generator (`RNDR`), or `None` if the
/// processor has none or it keeps failing.
fn rndr() -> Option<u64> {
    if !features().rng {
        return None;
    }
    (0..10).find_map(|_| random_register!("0xd53b2400"))
}

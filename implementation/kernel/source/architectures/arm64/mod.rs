// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The ARM64 (AArch64) architecture.
//!
//! The loader jumps to [`kernel_entry`], which sets the boot processor up
//! enough to log and to take exceptions, and hands over to the kernel; the
//! kernel then reaches the processor and the machine only through the
//! modules `crate::architecture` exposes. The rest (the interrupt
//! controller, the assembly entry paths) stays in here.
//!
//! The machines are those of Arm's standards for operating systems: UEFI,
//! ACPI, GICv3, PSCI and, for devices given to guests, SMMUv3. QEMU's
//! `virt` machine is one.

mod acpi;
pub mod clock;
pub mod cpu;
mod entry;
pub(in crate::architecture) mod gic;
pub mod iommu;
pub mod ioport;
pub mod irq;
pub(in crate::architecture) mod its;
pub mod memory;
pub mod paging;
pub mod percpu;
pub mod power;
pub mod serial;
pub mod smp;
pub mod thread;
pub mod timer;
pub mod tlb;
pub mod trap;
pub mod virtualization;

use bootinfo::{BootInfo, KernelImage};

use crate::synchronization::Once;
use crate::time;

/// Where the kernel is, for what has to name it physically.
static KERNEL_IMAGE: Once<KernelImage> = Once::new();

fn kernel_image() -> &'static KernelImage {
    KERNEL_IMAGE.expect()
}

/// The kernel's entry point, where the loader jumps (see `link`): with
/// exceptions masked, on the loader's tables and stack (see `bootinfo`).
#[unsafe(no_mangle)]
pub extern "C" fn kernel_entry(boot: &'static BootInfo) -> ! {
    // SAFETY: per-CPU structures of the BSP, set up exactly once.
    unsafe { percpu::install(0) };
    cpu::detect_features();
    KERNEL_IMAGE.set(boot.kernel);
    crate::kernel_main(boot)
}

/// Sets the boot CPU up the way every CPU is (the others do it as they
/// start): its exceptions, its registers, the stack of its faults. Once
/// memory management is up.
pub fn init_cpu(id: usize) {
    smp::init_this_cpu(id);
}

/// Sets the platform up (after `crate::firmware::acpi::init`): the console,
/// the interrupt controller, and the clock.
pub fn init_platform() {
    acpi::init();
    let platform = acpi::PLATFORM.expect();
    serial::init(platform.uart);
    gic::init();
    clock::start();
}

/// Logs how the clock runs: there is nothing else to start, as the timer's
/// interrupt is on since [`init_platform`] and it fires once armed.
pub fn init_timer() {
    crate::kinfo!("time: the generic timer's counter at {} MHz", time::ticks_hz() / 1_000_000);
}

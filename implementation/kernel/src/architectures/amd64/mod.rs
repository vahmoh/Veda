// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The AMD64 (x86-64) architecture.
//!
//! The loader jumps to [`kernel_entry`], which sets the boot processor up
//! enough to log and to take exceptions, and hands over to the kernel; the
//! kernel then reaches the processor and the PC platform only through the
//! modules `crate::arch` exposes. The rest (descriptor tables, interrupt
//! controllers, the assembly entry paths) stays in here.

mod acpi;
mod apic;
pub mod clock;
pub mod cpu;
mod entry;
mod gdt;
pub mod hv;
mod idt;
pub mod iommu;
pub mod ioport;
pub mod irq;
pub mod mm;
pub mod paging;
pub mod percpu;
mod port;
pub mod power;
pub mod serial;
pub mod smp;
pub mod thread;
pub mod timer;
pub mod tlb;
pub mod trap;

use bootinfo::BootInfo;

use crate::time;

/// The kernel's entry point, where the loader jumps (see `link`): in long
/// mode, interrupts disabled, on the loader's page tables and stack (see
/// `bootinfo`).
#[unsafe(no_mangle)]
pub extern "sysv64" fn kernel_entry(boot: &'static BootInfo) -> ! {
    serial::init();
    // SAFETY: per-CPU structures of the BSP, set up exactly once.
    unsafe {
        let p = percpu::get(0);
        gdt::load(p.gdt.get(), p.tss.get());
        percpu::install(0, 0);
    }
    idt::init();
    idt::load();
    cpu::detect_features();
    crate::kernel_main(boot)
}

/// Sets the boot CPU up the way every CPU is (the others do it as they
/// start): its traps, control registers and system call entry, the stacks
/// of its exceptions. Once memory management is up.
pub fn init_cpu(id: usize) {
    smp::init_this_cpu(id);
}

/// Sets the platform up (after `crate::acpi::init`): the interrupt
/// controllers, and the clock.
pub fn init_platform() {
    acpi::init();
    let platform = acpi::PLATFORM.expect();
    apic::disable_pic();
    apic::select_mode(platform.lapic_phys, platform.x2apic_opt_out);
    apic::init_local();
    // SAFETY: recording the BSP's APIC id before any IPI is sent.
    unsafe { percpu::set_apic_id(0, apic::id()) };
    for io in &platform.ioapics {
        apic::add_ioapic(io.id, io.phys, io.gsi_base);
    }
    if let Some(hpet) = platform.hpet_phys {
        clock::set_hpet(hpet);
    }
    // The boot processor's TSC as all processors' will be (see `clock`).
    let firmware_adjust = clock::reset_boot_tsc();
    clock::calibrate();
    if let Some(adjust) = firmware_adjust {
        crate::kinfo!("time: the firmware's adjustment of CPU 0's TSC ({} ns) set to 0", time::ticks_to_ns(adjust));
    }
}

/// Starts the boot CPU's timer (after [`init_platform`]), and logs how the
/// clock and the processor run.
pub fn init_timer() {
    let features = cpu::features();
    if features.tsc_deadline {
        apic::use_tsc_deadline(true);
    } else {
        apic::calibrate_timer(&clock::busy_wait_ms);
    }
    crate::kinfo!(
        "time: TSC {} MHz (by the {}), {} timer, {} mode APIC",
        time::ticks_hz() / 1_000_000,
        clock::source(),
        if features.tsc_deadline { "TSC-deadline" } else { "one-shot" },
        if apic::is_x2apic() { "x2APIC" } else { "xAPIC" }
    );
    // How fast the processor runs: rated, and measured while busy.
    let rated = cpu::rated_mhz()
        .map_or(alloc::string::String::new(), |(base, top)| alloc::format!(", rated {} MHz (up to {} MHz)", base, top));
    let measured =
        cpu::measure_mhz(20_000).map_or(alloc::string::String::from("unknown"), |mhz| alloc::format!("{} MHz", mhz));
    let pstates = match cpu::hwp_range() {
        Some((lowest, highest)) => alloc::format!("hardware P-states on (performance {} to {})", lowest, highest),
        None => alloc::string::String::from("no hardware P-states: the firmware's speed"),
    };
    crate::kinfo!("cpu: running at {} while busy{}; {}", measured, rated, pstates);
}

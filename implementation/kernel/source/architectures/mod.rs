// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The processor architecture.
//!
//! Everything the kernel does that depends on the processor's architecture,
//! or on the platform that comes with it, is implemented in
//! `implementation/kernel/source/architectures/<architecture>/` (AMD64's in
//! `implementation/kernel/source/architectures/amd64/`). The rest of the
//! kernel reaches it only through the interface below, which every
//! architecture provides with the same names and meanings:
//!
//! | Item | What it provides |
//! |------|------------------|
//! | `init_cpu`, `init_platform`, `init_timer` | the architecture's steps of the boot sequence (its entry point, which the loader jumps to, calls `kernel_main`) |
//! | `cpu` | interrupts on and off, waiting for one, halting; the processor's name and features, for the log; its random number generators |
//! | `percpu` | the current CPU's index |
//! | `smp` | starting the other CPUs; making one reschedule; halting the others |
//! | `trap` | the registers a trap saves: a system call's arguments and results, page faults, where the code was, descriptions for the log |
//! | `thread` | a thread's saved context, and switching between threads |
//! | `paging` | page tables: mappings, address spaces, the kernel's own |
//! | `mm` | the layout of the kernel's address space, memory and registers the architecture keeps, caches, access to user memory |
//! | `tlb` | TLB shootdowns, of the processors' and of guests' translations |
//! | `irq` | devices' interrupts: their vectors, lines and MSIs |
//! | `timer`, `clock` | each CPU's one-shot timer; the clock's counter |
//! | `serial` | the serial console the kernel log goes to |
//! | `ioport` | I/O ports, for I/O port objects |
//! | `power` | powering off and resetting |
//! | `hv` | virtual machines: guests' address translation, virtual processors |
//! | `iommu` | the IOMMU: devices' DMA and interrupts remapped, devices given to guests |
//!
//! Code in `source/architectures/<architecture>/` may use the rest of the
//! kernel; the rest of the kernel uses nothing of it but this.

#[cfg(target_arch = "x86_64")]
mod amd64;
#[cfg(target_arch = "x86_64")]
pub use amd64::{
    clock, cpu, hv, init_cpu, init_platform, init_timer, iommu, ioport, irq, mm, paging, percpu, power, serial, smp,
    thread, timer, tlb, trap,
};

#[cfg(not(target_arch = "x86_64"))]
compile_error!("the kernel supports AMD64 only (implementation/kernel/source/architectures/amd64)");

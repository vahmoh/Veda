// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `vkernel` — the Veda microkernel.
//!
//! The kernel provides only what cannot safely live in user space:
//! scheduling, address spaces, capability-based kernel objects (processes,
//! threads, channels, events, VMOs, interrupts, I/O ports, resources) and
//! interrupt delivery. Drivers, file systems, the window system and all
//! applications are user-space processes that talk over channels.
//!
//! See `documentation/ARCHITECTURE.md` for the overall design.

#![no_std]
#![no_main]

extern crate alloc;

mod acpi;
mod arch;
mod futex;
mod hv;
mod loader;
mod log;
mod mm;
mod object;
mod panic;
mod percpu;
mod random;
mod sched;
mod sync;
mod syscall;
mod time;

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use bootinfo::{BootInfo, MemoryKind};
use sched::Thread;
use sync::bkl;

#[global_allocator]
static HEAP: mm::heap::KernelHeap = mm::heap::KernelHeap::new();

/// Options parsed from the kernel command line (`BOOT.CFG`).
struct Options {
    max_cpus: usize,
    /// `tz=+02:00`: local time's offset from UTC, in seconds.
    tz: Option<i64>,
}

fn parse_cmdline(cmdline: &str) -> Options {
    let mut o = Options { max_cpus: percpu::MAX_CPUS, tz: None };
    for arg in cmdline.split_whitespace() {
        match arg.split_once('=') {
            Some(("log", "debug")) => log::set_level(log::Level::Debug),
            Some(("log", "warn")) => log::set_level(log::Level::Warn),
            Some(("smp", "off")) | Some(("cpus", "1")) => o.max_cpus = 1,
            Some(("cpus", n)) => o.max_cpus = n.parse().unwrap_or(o.max_cpus).clamp(1, percpu::MAX_CPUS),
            Some(("tz", z)) => o.tz = time::parse_utc_offset(z),
            _ => {}
        }
    }
    o
}

/// Logs the firmware's screen modes, the one the loader chose, and the
/// display's own size (a panel's that the firmware stretches a mode of
/// another shape over, the window system's screen keeps that shape).
fn log_screen(s: &bootinfo::ScreenReport) {
    use core::fmt::Write;
    let mut modes = alloc::string::String::new();
    for (i, [w, h]) in s.modes.iter().take(s.count as usize).enumerate() {
        let sep = if i > 0 { ", " } else { "" };
        let _ = write!(modes, "{sep}{w}x{h}");
        modes.push_str(match (i as u8 == s.chosen, i as u8 == s.entered) {
            (true, true) => " (chosen, the firmware's)",
            (true, false) => " (chosen)",
            (false, true) => " (the firmware's)",
            (false, false) => "",
        });
    }
    if modes.is_empty() {
        return;
    }
    match s.own {
        [0, _] | [_, 0] => {
            kinfo!("boot: the firmware's screen modes: {}; the display's own size is not known (no EDID)", modes)
        }
        [w, h] => kinfo!("boot: the firmware's screen modes: {}; the display's own: {}x{}", modes, w, h),
    }
}

/// Logs how the loader cleared the screen: a real PC's framebuffer is often
/// uncached, which shows as the screen being painted from the top down,
/// until something makes it write-combining.
fn log_paint(s: &bootinfo::PaintReport) {
    use bootinfo::{made_wc, memory_type};
    if s.paint_ticks == 0 {
        return;
    }
    let ms = |ticks: u64| {
        let us = time::ticks_to_ns(ticks as i64) / 1000;
        alloc::format!("{}.{} ms", us / 1000, us % 1000 / 100)
    };
    let found = memory_type::name(s.found);
    let how = match s.made_wc {
        made_wc::ALREADY => alloc::format!("the framebuffer is {}", found),
        made_wc::PAGE_ATTRIBUTES => alloc::format!(
            "the firmware's framebuffer is {}, painted write-combining through the loader's page tables (made in {})",
            found,
            ms(s.setup_ticks)
        ),
        _ => alloc::format!("the framebuffer is {}, and could not be painted write-combining", found),
    };
    kinfo!("boot: the loader cleared the screen in {}; {}", ms(s.paint_ticks), how);
}

/// The kernel, once the architecture's entry code has the boot CPU ready
/// to log and take exceptions (`arch::kernel_entry`): sets up memory, the
/// platform and the other CPUs, and starts `init`.
fn kernel_main(boot: &'static BootInfo) -> ! {
    if !boot.is_valid() {
        panic!("invalid boot information (loader/kernel version mismatch)");
    }
    let opts = parse_cmdline(boot.cmdline());
    kinfo!("Veda kernel {} starting", env!("CARGO_PKG_VERSION"));
    arch::cpu::report();

    // Memory: frame allocator, kernel address space, heap.
    // SAFETY: the loader built a valid memory map.
    let memmap = unsafe { boot.memory_map.as_slice() };
    mm::phys::init(memmap);
    mm::paging::init_kernel_space(boot);
    let reclaimed = mm::phys::reclaim(memmap, MemoryKind::LoaderReclaimable);
    let (total, free) = mm::phys::stats();
    kinfo!(
        "memory: {} MiB usable, {} MiB free ({} KiB reclaimed from the loader)",
        total >> 20,
        free >> 20,
        reclaimed >> 10
    );
    arch::init_cpu(0);
    panic::set_framebuffer(&boot.framebuffer);

    // Platform: ACPI, interrupt controllers, clocks and timers.
    acpi::init(boot.rsdp_phys);
    arch::init_platform();
    log_screen(&boot.screen);
    log_paint(&boot.paint);
    time::set_boot_time(&boot.boot_time, opts.tz);
    arch::init_timer();

    random::init(boot.entropy());

    // Scheduler on the boot CPU; this boot context becomes CPU 0's idle
    // thread.
    sched::init_cpu(Thread::new_idle_current(0));
    percpu::get(0).online.store(true, Ordering::Release);

    let others = arch::smp::start_secondary_cpus(opts.max_cpus);
    kinfo!("smp: {} CPU(s) online", others + 1);
    arch::hv::report();
    arch::iommu::init();

    bkl::acquire();
    loader::spawn_init(boot);
    kinfo!("boot complete; handing the CPU to user space");
    sched::idle_loop()
}

/// A CPU other than the boot CPU, once its architecture has set it up
/// (`arch::smp`): it comes online with `idle` as its idle thread, says it
/// started (`started`), and takes work.
fn secondary_main(id: usize, idle: Arc<Thread>, started: impl FnOnce()) -> ! {
    sched::init_cpu(idle);
    percpu::get(id).online.store(true, Ordering::Release);
    started();
    bkl::acquire();
    sched::idle_loop()
}

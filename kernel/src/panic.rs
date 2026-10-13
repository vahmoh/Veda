// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Kernel panics.
//!
//! A panic stops every CPU, prints a report to the serial console and paints
//! a "stop screen" on the framebuffer. Fatal CPU exceptions are reported by
//! the architecture's trap handling, between [`begin`] and [`end`].

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::cpu;
use crate::log::emergency;
use crate::percpu;

static PANICKING: AtomicBool = AtomicBool::new(false);

/// Framebuffer used for the stop screen (virtual address, geometry).
static FB_ADDR: AtomicU64 = AtomicU64::new(0);
static FB_GEOMETRY: AtomicU64 = AtomicU64::new(0); // width | height << 20 | stride << 40

pub fn set_framebuffer(fb: &bootinfo::Framebuffer) {
    if fb.phys_base == 0 {
        return;
    }
    FB_ADDR.store(crate::mm::phys_to_virt(fb.phys_base), Ordering::Relaxed);
    FB_GEOMETRY.store(fb.width as u64 | (fb.height as u64) << 20 | (fb.stride as u64) << 40, Ordering::Relaxed);
}

/// Paints the stop screen: a deep blue background with a white frowning
/// face, drawn without any font so it works in any state.
fn paint_stop_screen() {
    let base = FB_ADDR.load(Ordering::Relaxed);
    let g = FB_GEOMETRY.load(Ordering::Relaxed);
    if base == 0 {
        return;
    }
    let (w, h, stride) = ((g & 0xFFFFF) as usize, ((g >> 20) & 0xFFFFF) as usize, (g >> 40) as usize);
    let px = base as *mut u32;
    let put = |x: usize, y: usize, c: u32| {
        if x < w && y < h {
            // SAFETY: inside the framebuffer.
            unsafe { px.add(y * stride + x).write_volatile(c) };
        }
    };
    for y in 0..h {
        for x in 0..w {
            put(x, y, 0x0010_3C8C);
        }
    }
    // ":(" — two eyes and a frown, scaled to the screen.
    let s = (h / 120).max(2);
    let (ox, oy) = (w / 10, h / 5);
    for dy in 0..6 * s {
        for dx in 0..6 * s {
            put(ox + dx, oy + dy, 0xFFFF_FFFF);
            put(ox + dx, oy + 14 * s + dy, 0xFFFF_FFFF);
        }
    }
    let (cx, cy, r) = (ox + 30 * s, oy + 22 * s, 16 * s);
    for a in 0..720 {
        // Upper half of a circle approximated without floats.
        let t = a as i64 - 360;
        let x = cx as i64 + (t * r as i64) / 360;
        let yy = r as i64 * r as i64 - (x - cx as i64) * (x - cx as i64);
        let mut ry = 0i64;
        while (ry + 1) * (ry + 1) <= yy {
            ry += 1;
        }
        for th in 0..(2 * s as i64) {
            put(x as usize, (cy as i64 - ry + th) as usize, 0xFFFF_FFFF);
        }
    }
}

/// Begins a panic: interrupts off on this CPU, and every other CPU halted.
/// A CPU that finds a panic under way already halts here.
pub fn begin() {
    cpu::disable_interrupts();
    if PANICKING.swap(true, Ordering::SeqCst) {
        // Another CPU is already panicking.
        cpu::halt_forever();
    }
    crate::arch::smp::halt_others();
}

/// Ends a panic: the stop screen, then this CPU halts for good.
pub fn end() -> ! {
    paint_stop_screen();
    cpu::halt_forever();
}

/// Whether a panic is under way.
pub fn panicking() -> bool {
    PANICKING.load(Ordering::SeqCst)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    begin();
    emergency(format_args!("\n\n*** VEDA KERNEL PANIC on CPU {} ***\n{}\n", percpu::cpu_id_or_boot(), info));
    if let Some(t) = crate::sched::try_current() {
        emergency(format_args!("current thread: {} of {}\n", t.koid, t.process_name()));
    }
    end()
}

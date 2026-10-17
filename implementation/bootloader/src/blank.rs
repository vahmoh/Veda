// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The screen while the system starts: blank. The loader clears the GOP
//! framebuffer to black the moment it has set its mode, whatever the
//! firmware left there (its logo, its boot menu), and the screen stays
//! black until the window system's splash fades in, once the display is
//! the one the desktop will be shown on. Where the architecture can, it
//! paints it write-combining (`arch::clear_write_combining`).
//!
//! Each row is written out of memory in 8-byte stores. No string
//! instruction is used: on uncached memory (where the loader cannot help
//! it) a string store stores a byte at a time.

#[derive(Clone, Copy)]
pub struct Surface {
    pub base: *mut u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

/// Paints the whole screen black.
pub fn clear(s: &Surface) {
    let w = s.width as usize;
    for y in 0..s.height {
        // SAFETY: row `y` of the framebuffer, `stride` pixels from the last.
        let line = unsafe { s.base.add((y * s.stride) as usize) };
        let mut at = 0;
        if (line as usize).is_multiple_of(8) {
            while at + 2 <= w {
                // SAFETY: two pixels inside row `y`, 8-byte aligned.
                unsafe { (line.add(at) as *mut u64).write_volatile(0) };
                at += 2;
            }
        }
        while at < w {
            // SAFETY: a pixel inside row `y`.
            unsafe { line.add(at).write_volatile(0) };
            at += 1;
        }
    }
}

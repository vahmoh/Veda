// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Clients' pixels, read where they are: in memory each client shares with
//! the compositor, and may be writing while the compositor reads it.
//!
//! Such memory is never borrowed as a Rust slice, which would promise that
//! nothing changes it while the slice lives (a promise the client can break
//! at any moment): its pixels are only copied out, through raw pointers, at
//! offsets that `Layout::span` has checked to lie in the buffer, which lies
//! in the mapping (checked as the buffers were attached, and again here).
//! What a client writes meanwhile can only make the picture tear.
//!
//! Also here: bitmaps whose size a client chooses, allocated so that a lack
//! of memory is an answer rather than the compositor's end.

use alloc::vec::Vec;

use compositor_core::buffers::{Layout, Surface};
use compositor_core::switcher;
use gfx::Bitmap;
use rt::vm::Mapping;

/// A bitmap of `w` x `h`, cleared; `None` if there is not the memory for it.
pub(crate) fn try_bitmap(w: i32, h: i32) -> Option<Bitmap> {
    let (w, h) = (w.max(0), h.max(0));
    let n = (w as usize).checked_mul(h as usize)?;
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(n).ok()?;
    pixels.resize(n, 0);
    Some(Bitmap { width: w, height: h, pixels })
}

/// One of a client's buffers: the one the screen shows of its window.
pub(crate) struct Pixels<'a> {
    layout: &'a Layout,
    base: *const u32,
    index: u8,
}

impl<'a> Pixels<'a> {
    /// The buffer the screen shows of `surface`, if it has one.
    pub(crate) fn shown(surface: &'a Surface<Mapping>) -> Option<Pixels<'a>> {
        let (buffers, index) = surface.shown()?;
        // The mapping covers the buffers (it was made so); a layout that did
        // not fit it would be read from nowhere.
        if buffers.layout.bytes() > buffers.memory.len() {
            return None;
        }
        Some(Pixels { layout: &buffers.layout, base: buffers.memory.as_ptr() as *const u32, index })
    }

    /// The buffer's width (pixels).
    pub(crate) fn width(&self) -> i32 {
        self.layout.width
    }

    /// The buffer's height (pixels).
    pub(crate) fn height(&self) -> i32 {
        self.layout.height
    }

    /// Copies the `out.len()` pixels of row `y` from `x` on into `out`;
    /// `false`, copying nothing, unless all of them are the buffer's.
    pub(crate) fn copy_row(&self, x: i32, y: i32, out: &mut [u32]) -> bool {
        let Ok(n) = i32::try_from(out.len()) else { return false };
        let Some(at) = self.layout.span(self.index, x, y, n) else { return false };
        // SAFETY: `span` checked that the pixels lie in the buffer, and the
        // buffers lie in the mapping (`shown`), which stays while `self`
        // borrows it. They are copied, never borrowed: the client may be
        // writing them.
        unsafe { core::ptr::copy_nonoverlapping(self.base.add(at), out.as_mut_ptr(), out.len()) };
        true
    }

    /// Copies `w` x `h` pixels from (`x`, `y`) into `out`, row after row;
    /// `false`, copying nothing, unless all of them are the buffer's.
    pub(crate) fn copy_rect(&self, (x, y, w, h): (i32, i32, i32, i32), out: &mut Vec<u32>) -> bool {
        if w <= 0 || h <= 0 || self.layout.span(self.index, x, y + h - 1, w).is_none() {
            return false;
        }
        let n = w as usize * h as usize;
        out.clear();
        if out.try_reserve(n).is_err() {
            return false;
        }
        // Each row straight into the spare room, which is not cleared first:
        // a window that changes whole (a game's, a video's) is copied every
        // frame.
        for (row, line) in out.spare_capacity_mut()[..n].chunks_exact_mut(w as usize).enumerate() {
            let Some(at) = self.layout.span(self.index, x, y + row as i32, w) else { return false };
            // SAFETY: as in `copy_row`, into room for `w` pixels.
            unsafe { core::ptr::copy_nonoverlapping(self.base.add(at), line.as_mut_ptr().cast::<u32>(), line.len()) };
        }
        // SAFETY: the loop wrote all of the first `n` pixels.
        unsafe { out.set_len(n) };
        true
    }

    /// Pixel (`x`, `y`), or nothing (0) outside the buffer.
    fn get(&self, x: i32, y: i32) -> u32 {
        match self.layout.span(self.index, x, y, 1) {
            // SAFETY: as in `copy_row`: a pixel of the buffer, read by value.
            Some(at) => unsafe { self.base.add(at).read() },
            None => 0,
        }
    }
}

/// A small copy of what `p` shows (each pixel the mean of 3 x 3 samples),
/// for the switcher; `None` if there is not the memory for it.
pub(crate) fn thumbnail(p: &Pixels) -> Option<Bitmap> {
    let (sw, sh) = (p.width(), p.height());
    let (tw, th) = switcher::thumbnail_size(sw, sh);
    let (columns, rows) = (switcher::taps(tw, sw), switcher::taps(th, sh));
    let mut out = try_bitmap(tw, th)?;
    for (y, line) in out.pixels.chunks_exact_mut(tw as usize).enumerate() {
        for (x, px) in line.iter_mut().enumerate() {
            let mut acc = [0u32; 4];
            for &sy in &rows[y * 3..y * 3 + 3] {
                for &sx in &columns[x * 3..x * 3 + 3] {
                    let s = p.get(sx, sy);
                    acc[0] += s >> 24;
                    acc[1] += (s >> 16) & 0xFF;
                    acc[2] += (s >> 8) & 0xFF;
                    acc[3] += s & 0xFF;
                }
            }
            *px = (acc[0] / 9) << 24 | (acc[1] / 9) << 16 | (acc[2] / 9) << 8 | (acc[3] / 9);
        }
    }
    Some(out)
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A window's pixel buffers: where they lie in the memory its client
//! shares, checked once as they are attached, and which of them the screen
//! shows.
//!
//! A client attaches `count` buffers back to back in one memory object,
//! each `height` rows of `stride` pixels, and presents one of them at a
//! time. Sizes are kept in `usize` and checked against the memory as they
//! arrive ([`Layout::new`]), so that no pixel the compositor reads later
//! can lie outside it ([`Layout::span`]).
//!
//! Buffers a client attaches wait until it presents one of them: until
//! then the screen goes on showing the ones it presented before, so that a
//! window being resized shows its last frame rather than the placeholder.

use vgfx::{Damage, Rect};
use vproto::display::DisplayError;

/// The most buffers a client attaches at once (triple buffering).
pub const MAX_BUFFERS: u8 = 3;

/// How many pixels a row may have beyond the buffer's width: rows aligned
/// to 256 bytes, as display engines like them, but no more.
pub const STRIDE_SLACK: u32 = 64;

/// Where a client's buffers lie in the memory it shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Each buffer's size (pixels).
    pub width: i32,
    pub height: i32,
    /// Pixels from one row to the next.
    pub stride: usize,
    /// How many buffers, back to back.
    pub count: u8,
}

impl Layout {
    /// Checks a client's buffers: `count` of them, each `width` x `height`
    /// pixels at most `max` and rows `stride` pixels apart, in `memory`
    /// bytes.
    pub fn new(
        width: u32,
        height: u32,
        stride: u32,
        count: u8,
        max: (i32, i32),
        memory: usize,
    ) -> Result<Layout, DisplayError> {
        let fits = |v: u32, max: i32| v >= 1 && v <= max.max(1) as u32;
        if !fits(width, max.0) || !fits(height, max.1) || count == 0 || count > MAX_BUFFERS {
            return Err(DisplayError::BadBuffer);
        }
        if stride < width || stride - width > STRIDE_SLACK {
            return Err(DisplayError::BadBuffer);
        }
        let layout = Layout { width: width as i32, height: height as i32, stride: stride as usize, count };
        let bytes = (layout.stride.checked_mul(layout.height as usize))
            .and_then(|pixels| pixels.checked_mul(4))
            .and_then(|bytes| bytes.checked_mul(count as usize))
            .ok_or(DisplayError::BadBuffer)?;
        if bytes > memory {
            return Err(DisplayError::BadBuffer);
        }
        Ok(layout)
    }

    /// The pixels of one buffer, rows and their slack.
    pub fn buffer_pixels(&self) -> usize {
        self.stride * self.height as usize
    }

    /// The bytes all the buffers take (at most the memory's size, which
    /// [`Layout::new`] checked).
    pub fn bytes(&self) -> usize {
        self.buffer_pixels() * 4 * self.count as usize
    }

    /// A buffer's pixels as a rectangle at the origin.
    pub fn rect(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// Where the `n` pixels of row `y` of buffer `index` from `x` on start,
    /// in pixels from the memory's start; `None` unless all of them are the
    /// buffer's.
    pub fn span(&self, index: u8, x: i32, y: i32, n: i32) -> Option<usize> {
        let inside = index < self.count
            && x >= 0
            && y >= 0
            && n >= 0
            && y < self.height
            && x.checked_add(n).is_some_and(|end| end <= self.width);
        inside.then(|| self.buffer_pixels() * index as usize + y as usize * self.stride + x as usize)
    }
}

/// Buffers a client attached: their layout, and the memory they are in.
pub struct Buffers<M> {
    pub layout: Layout,
    pub memory: M,
}

/// What a present did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Presented {
    /// The window's first frame.
    pub first: bool,
    /// The buffers attached last are shown from this frame on.
    pub new_buffers: bool,
}

/// A window's buffers: those the screen shows, and those attached since,
/// waiting for their first present.
pub struct Surface<M> {
    shown: Option<(Buffers<M>, u8)>,
    pending: Option<Buffers<M>>,
    /// A frame was presented and is to be acknowledged (`FrameDone`) once
    /// it has been composed, and the window can be seen.
    pub(crate) frame_owed: Option<u8>,
    /// What changed in the shown buffer since the GPU's copy of it was
    /// brought up to date; `upload_all`: everything did.
    pub upload: Damage,
    pub upload_all: bool,
}

impl<M> Default for Surface<M> {
    fn default() -> Surface<M> {
        Surface { shown: None, pending: None, frame_owed: None, upload: Damage::new(), upload_all: true }
    }
}

impl<M> Surface<M> {
    /// Buffers the client attached: shown from its next present on, in
    /// place of any attached before that were never presented.
    pub fn attach(&mut self, buffers: Buffers<M>) {
        self.pending = Some(buffers);
    }

    /// The client presents buffer `index` (of the buffers attached last),
    /// `damage` (in the buffer's pixels) having changed; everything if it
    /// is empty.
    pub fn present(&mut self, index: u8, damage: &[Rect]) -> Result<Presented, DisplayError> {
        let layout = match (&self.pending, &self.shown) {
            (Some(p), _) => p.layout,
            (None, Some((s, _))) => s.layout,
            (None, None) => return Err(DisplayError::BadBuffer),
        };
        if index >= layout.count {
            return Err(DisplayError::BadBuffer);
        }
        let first = self.shown.is_none();
        let new_buffers = self.pending.is_some();
        match self.pending.take() {
            Some(p) => self.shown = Some((p, index)),
            None => {
                if let Some((_, shown)) = &mut self.shown {
                    *shown = index;
                }
            }
        }
        // What the GPU's copy lacks now.
        if new_buffers || damage.is_empty() {
            self.upload.clear();
            self.upload_all = true;
        } else {
            for d in damage {
                self.upload.add(d.intersect(&layout.rect()));
            }
        }
        self.frame_owed = Some(index);
        Ok(Presented { first, new_buffers })
    }

    /// The buffers the screen shows, and which of them.
    pub fn shown(&self) -> Option<(&Buffers<M>, u8)> {
        self.shown.as_ref().map(|(b, i)| (b, *i))
    }

    /// The size of the buffer the screen shows.
    pub fn size(&self) -> Option<(i32, i32)> {
        self.shown.as_ref().map(|(b, _)| (b.layout.width, b.layout.height))
    }

    /// The memory the buffers take, shown and waiting.
    pub fn bytes(&self) -> usize {
        self.shown.as_ref().map_or(0, |(b, _)| b.layout.bytes()) + self.pending_bytes()
    }

    /// The memory the buffers waiting for their first present take.
    pub fn pending_bytes(&self) -> usize {
        self.pending.as_ref().map_or(0, |b| b.layout.bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: (i32, i32) = (1920, 1080);

    #[test]
    fn buffers_are_checked_against_their_memory() {
        let l = Layout::new(100, 50, 100, 2, MAX, 100 * 50 * 4 * 2).unwrap();
        assert_eq!((l.bytes(), l.buffer_pixels()), (40_000, 5000));
        assert_eq!(Layout::new(100, 50, 100, 2, MAX, 100 * 50 * 4 * 2 - 1), Err(DisplayError::BadBuffer));
        // Rows may be a little longer than the buffer is wide, no more.
        assert!(Layout::new(100, 50, 100 + STRIDE_SLACK, 1, MAX, usize::MAX).is_ok());
        assert!(Layout::new(100, 50, 101 + STRIDE_SLACK, 1, MAX, usize::MAX).is_err());
        assert!(Layout::new(100, 50, 99, 1, MAX, usize::MAX).is_err());
        // No buffers, too many, or larger than the screen.
        for (w, h, count) in [(100, 50, 0), (100, 50, MAX_BUFFERS + 1), (1921, 50, 1), (100, 1081, 1), (0, 50, 1)] {
            assert!(Layout::new(w, h, w, count, MAX, usize::MAX).is_err());
        }
    }

    #[test]
    fn rows_long_enough_to_wrap_are_refused() {
        // A 1-pixel-wide buffer whose rows are a million pixels apart:
        // their product overflowed `i32` and the compositor read gigabytes
        // before the memory. Its rows are far longer than it is wide.
        assert!(Layout::new(1, 2048, 1_048_577, 2, (8192, 8192), usize::MAX).is_err());
        assert!(Layout::new(u32::MAX, u32::MAX, u32::MAX, 3, (i32::MAX, i32::MAX), usize::MAX).is_err());
    }

    #[test]
    fn spans_stay_inside_their_buffer() {
        let l = Layout::new(10, 4, 12, 2, MAX, usize::MAX).unwrap();
        assert_eq!(l.span(0, 0, 0, 10), Some(0));
        assert_eq!(l.span(1, 2, 3, 8), Some(48 + 3 * 12 + 2));
        for (index, x, y, n) in
            [(2, 0, 0, 1), (0, 3, 0, 8), (0, -1, 0, 1), (0, 0, 4, 1), (0, 0, -1, 1), (0, 9, 0, i32::MAX)]
        {
            assert_eq!(l.span(index, x, y, n), None);
        }
        let last = l.span(1, 9, 3, 1).unwrap();
        assert!(last < l.bytes() / 4);
    }

    fn buffers(width: u32, height: u32, count: u8) -> Buffers<()> {
        Buffers { layout: Layout::new(width, height, width, count, MAX, usize::MAX).unwrap(), memory: () }
    }

    #[test]
    fn attached_buffers_wait_for_their_first_present() {
        let mut s = Surface::default();
        assert_eq!(s.present(0, &[]), Err(DisplayError::BadBuffer));
        s.attach(buffers(100, 50, 2));
        assert_eq!(s.size(), None);
        assert_eq!(s.present(0, &[]), Ok(Presented { first: true, new_buffers: true }));
        assert_eq!(s.size(), Some((100, 50)));
        // Resized: the new buffers wait while the old are shown.
        s.attach(buffers(200, 80, 2));
        assert_eq!((s.size(), s.bytes()), (Some((100, 50)), 100 * 50 * 8 + 200 * 80 * 8));
        assert_eq!(s.present(1, &[]), Ok(Presented { first: false, new_buffers: true }));
        assert_eq!((s.size(), s.shown().map(|(_, i)| i)), (Some((200, 80)), Some(1)));
        assert_eq!(s.bytes(), 200 * 80 * 8);
        // An index of the buffers presented.
        assert_eq!(s.present(2, &[]), Err(DisplayError::BadBuffer));
    }

    #[test]
    fn what_changed_is_kept_for_the_gpu() {
        let mut s = Surface::default();
        s.attach(buffers(100, 50, 2));
        s.present(0, &[]).unwrap();
        s.upload_all = false;
        s.present(1, &[Rect::new(90, 40, 50, 50)]).unwrap();
        assert!(!s.upload_all);
        assert_eq!(s.upload.rects(), &[Rect::new(90, 40, 10, 10)]);
        // New buffers: all of it.
        s.attach(buffers(100, 50, 2));
        s.present(0, &[Rect::new(0, 0, 1, 1)]).unwrap();
        assert!(s.upload_all && s.upload.is_empty());
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What clients may ask of the window system, checked where their requests
//! come in: positions, sizes, text, and how much each process may have.
//!
//! Every number a client sends is checked before the window manager does
//! arithmetic with it, in `i32` as `vgfx`'s rectangles do: positions are at
//! most [`COORD_MAX`] from the origin and sizes at most the screen's, so
//! that nothing derived from them (a frame, a shadow, a damaged rectangle)
//! can overflow. A window larger than the screen shows nothing more, and
//! is what would make the compositor's memory (title bars, textures, the
//! switcher) grow with what a client asks for.

use alloc::string::String;

use vgfx::Rect;
use vproto::display::{self as dp, DisplayError};

/// How far from the origin a position a client gives may be (pixels).
pub const COORD_MAX: i32 = 1 << 20;

/// The longest title or application id kept (bytes): longer ones are cut
/// at a character boundary.
pub const TEXT_MAX: usize = 256;

/// The longest text the clipboard holds (bytes): the protocol's.
pub use vproto::display::CLIPBOARD_MAX;

/// How many windows one process may have.
pub const WINDOWS_PER_PROCESS: usize = 64;
/// How many windows all applications together may have (the system's
/// services have only their own limit).
pub const WINDOWS_MAX: usize = 1024;

/// How many connections one process may have open.
pub const CONNECTIONS_PER_PROCESS: usize = 16;
/// How many connections all applications together may have open.
pub const CONNECTIONS_MAX: usize = 512;

/// How much pixel memory one process may attach, in screens' worth (a
/// maximised window double buffered is two; one being resized holds its
/// new buffers beside the old until it presents).
pub const BUFFER_SCREENS_PER_PROCESS: usize = 16;

/// The least width of a window the user resizes, whatever its client asks.
pub const MIN_WIDTH: i32 = 64;
/// The least height of a window the user resizes.
pub const MIN_HEIGHT: i32 = 32;

/// `v` as a position a client may give.
pub fn coordinate(v: i32) -> Result<i32, DisplayError> {
    if v.unsigned_abs() <= COORD_MAX as u32 { Ok(v) } else { Err(DisplayError::Invalid) }
}

/// `text` cut to at most `max` bytes, at a character boundary.
pub fn truncated(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// A rectangle a client gave (the damage of a frame), checked.
pub fn rect(r: dp::Rect) -> Result<Rect, DisplayError> {
    let size = |v: u32| if v <= COORD_MAX as u32 { Ok(v as i32) } else { Err(DisplayError::Invalid) };
    Ok(Rect::new(coordinate(r.x)?, coordinate(r.y)?, size(r.w)?, size(r.h)?))
}

/// The limits that follow from the screen's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The screen.
    pub screen: Rect,
}

impl Limits {
    /// The limits on `screen`.
    pub fn new(screen: Rect) -> Limits {
        Limits { screen }
    }

    /// The largest window (its client area): the screen's size.
    pub fn window(&self) -> (i32, i32) {
        (self.screen.w.max(1), self.screen.h.max(1))
    }

    /// The largest popup an application may open: half the screen each
    /// way (a menu, a list to choose from), so that none can pass for the
    /// screen or the shell's surfaces.
    pub fn popup(&self) -> (i32, i32) {
        ((self.screen.w / 2).max(1), (self.screen.h / 2).max(1))
    }

    /// The pixel memory one process may attach (bytes).
    pub fn buffer_bytes_per_process(&self) -> usize {
        let (w, h) = self.window();
        w as usize * h as usize * 4 * BUFFER_SCREENS_PER_PROCESS
    }

    /// A size a client asked for, made one a window may have (at most
    /// `max`, at least a pixel).
    pub fn size(width: u32, height: u32, max: (i32, i32)) -> (i32, i32) {
        let fit = |v: u32, max: i32| v.min(max.max(1) as u32).max(1) as i32;
        (fit(width, max.0), fit(height, max.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_are_kept_near_the_origin() {
        assert_eq!(coordinate(-COORD_MAX), Ok(-COORD_MAX));
        assert_eq!(coordinate(COORD_MAX), Ok(COORD_MAX));
        for v in [COORD_MAX + 1, -COORD_MAX - 1, i32::MAX, i32::MIN] {
            assert_eq!(coordinate(v), Err(DisplayError::Invalid));
        }
    }

    #[test]
    fn rectangles_are_checked_before_they_are_used() {
        assert_eq!(rect(dp::Rect::new(-5, 7, 10, 20)), Ok(Rect::new(-5, 7, 10, 20)));
        assert_eq!(rect(dp::Rect::new(0, 0, u32::MAX, 1)), Err(DisplayError::Invalid));
        assert_eq!(rect(dp::Rect::new(i32::MAX, 0, 1, 1)), Err(DisplayError::Invalid));
    }

    #[test]
    fn text_is_cut_at_a_character_boundary() {
        assert_eq!(truncated(String::from("short"), 8), "short");
        assert_eq!(truncated(String::from("abcdef"), 4), "abcd");
        // "é" is two bytes: the cut does not split it.
        assert_eq!(truncated(String::from("aé"), 2), "a");
        assert_eq!(truncated(String::from("ééé"), 5), "éé");
    }

    #[test]
    fn sizes_are_those_of_the_screen_at_most() {
        let l = Limits::new(Rect::new(0, 0, 1280, 800));
        assert_eq!(l.window(), (1280, 800));
        assert_eq!(l.popup(), (640, 400));
        assert_eq!(Limits::size(0, u32::MAX, l.window()), (1, 800));
        assert_eq!(Limits::size(300, 200, l.window()), (300, 200));
        assert_eq!(l.buffer_bytes_per_process(), 1280 * 800 * 4 * BUFFER_SCREENS_PER_PROCESS);
    }
}

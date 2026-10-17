// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Where everything on the lock screen goes, on a screen of any size: the
//! clock and the date in its upper part; the user's ring, their name, the
//! password field and the line under it in the lower; the hint at the
//! bottom. Sizes follow the screen's height (or its width, on a screen
//! taller than it is wide), so that the lock screen looks the same on a
//! laptop and on a 4K monitor.

use gfx::Rect;

/// The screen's height for which a unit is a pixel.
const BASE: f32 = 800.0;

/// The lock screen's layout on one screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    /// Pixels per unit: a unit is a pixel on a screen 800 pixels high.
    pub unit: f32,
    /// The time, and its font size.
    pub clock: Rect,
    pub clock_size: f32,
    /// The date under it.
    pub date: Rect,
    pub date_size: f32,
    /// The user's picture: its centre and radius; the ring around it, its
    /// radius and width.
    pub avatar: (i32, i32, i32),
    pub ring: f32,
    pub ring_width: f32,
    /// The user's name.
    pub name: Rect,
    pub name_size: f32,
    /// The password field (a pill), the round button at its right end,
    /// and the size of what is typed in it.
    pub field: Rect,
    pub submit: Rect,
    pub text_size: f32,
    /// The line under the field: the wait, a wrong password, Caps Lock.
    pub message: Rect,
    pub message_size: f32,
    /// The hint at the bottom of a resting screen.
    pub hint: Rect,
    pub hint_size: f32,
    /// How far the card rises as it comes into view, and how far the field
    /// swings when it shakes.
    pub rise: i32,
    pub shake: i32,
}

impl Layout {
    /// The layout on a `w` x `h` screen.
    pub fn new(w: i32, h: i32) -> Layout {
        let (w, h) = (w.max(1), h.max(1));
        let u = (h.min(w.saturating_mul(5) / 4) as f32 / BASE).clamp(0.6, 3.0);
        let px = |units: f32| (units * u + 0.5) as i32;
        let centred = |y: i32, width: i32, height: i32| Rect::new((w - width) / 2, y, width, height);

        let clock_size = 128.0 * u;
        let clock_h = px(140.0);
        let clock = centred(h * 24 / 100 - clock_h / 2, w, clock_h);
        let date_size = 22.0 * u;
        let date = centred(clock.bottom() + px(2.0), w, px(32.0));

        let r = px(46.0);
        let (cx, cy) = (w / 2, h * 56 / 100);
        let ring = r as f32 + 9.0 * u;
        let name_size = 21.0 * u;
        let name = centred(cy + ring as i32 + px(14.0), w, px(30.0));
        let field_w = px(320.0).min(w - px(48.0));
        let field_h = px(46.0);
        let field = centred(name.bottom() + px(16.0), field_w, field_h);
        let d = field_h - px(10.0);
        let submit = Rect::new(field.right() - px(5.0) - d, field.y + (field_h - d) / 2, d, d);
        let message_size = 14.0 * u;
        let message = centred(field.bottom() + px(12.0), w, px(24.0));

        let hint_size = 15.0 * u;
        let hint = centred(h - px(64.0), w, px(26.0));
        Layout {
            unit: u,
            clock,
            clock_size,
            date,
            date_size,
            avatar: (cx, cy, r),
            ring,
            ring_width: (2.0 * u).max(1.5),
            name,
            name_size,
            field,
            submit,
            text_size: 16.0 * u,
            message,
            message_size,
            hint,
            hint_size,
            rise: px(18.0),
            shake: px(12.0),
        }
    }

    /// Where the ring reaches (with its glow), for what is drawn around it.
    pub fn ring_bounds(&self) -> Rect {
        let (cx, cy, _) = self.avatar;
        let reach = (self.ring * 1.3 + self.ring_width) as i32 + 2;
        Rect::new(cx - reach, cy - reach, 2 * reach, 2 * reach)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREENS: [(i32, i32); 9] = [
        (640, 480),
        (800, 600),
        (1280, 800),
        (1366, 768),
        (1920, 1080),
        (2560, 1440),
        (3840, 2160),
        (3440, 1440),
        (1080, 1920),
    ];

    #[test]
    fn everything_is_on_the_screen_in_order() {
        for (w, h) in SCREENS {
            let l = Layout::new(w, h);
            let screen = Rect::new(0, 0, w, h);
            let ring_top = l.avatar.1 - l.ring as i32;
            for r in [l.clock, l.date, l.name, l.field, l.submit, l.message, l.hint, l.ring_bounds()] {
                assert!(screen.contains_rect(&r), "{w}x{h}: {r:?}");
            }
            // Top to bottom, nothing over anything else.
            assert!(l.clock.bottom() <= l.date.y, "{w}x{h}");
            assert!(l.date.bottom() < ring_top, "{w}x{h}");
            assert!(l.avatar.1 + l.ring as i32 <= l.name.y, "{w}x{h}");
            assert!(l.name.bottom() <= l.field.y && l.field.bottom() <= l.message.y, "{w}x{h}");
            assert!(l.message.bottom() < l.hint.y, "{w}x{h}");
            // The button is in the field.
            assert!(l.field.contains_rect(&l.submit), "{w}x{h}");
        }
    }

    #[test]
    fn sizes_follow_the_screen_and_stay_readable() {
        let small = Layout::new(640, 480);
        let usual = Layout::new(1280, 800);
        let big = Layout::new(3840, 2160);
        assert_eq!(usual.unit, 1.0);
        assert!(small.clock_size < usual.clock_size && usual.clock_size < big.clock_size);
        for l in [small, usual, big, Layout::new(1080, 1920)] {
            assert!(l.message_size >= 8.0 && l.field.w >= 180, "{l:?}");
        }
        // A tall screen sizes by its width.
        assert!(Layout::new(1080, 1920).unit < Layout::new(1920, 1920).unit);
        // Nothing breaks on a screen of nothing.
        let none = Layout::new(0, 0);
        assert!(none.unit > 0.0);
    }
}

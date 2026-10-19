// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The place of a subsurface in a window's frames ([`crate::Ui::subsurface`]):
//! a picture of its own that the window shows there (a web page), which its
//! client presents apart from the window's frames (see `proto::display`).
//! The window's buffer is kept transparent there but for what its frame
//! draws over the subsurface (a menu, a bar): the compositor shows the
//! subsurface there, and blends the window's buffer over it within what
//! was drawn (the frame's overlays), reading nothing more of it.
//!
//! A frame is drawn into one of the window's buffers, which still holds the
//! frame drawn into it last, two or three frames before. The place is
//! erased again only where that frame left it otherwise: where something was
//! drawn over the subsurface then, as the canvas noted while that frame was
//! drawn ([`gfx::Canvas::track`]); all of it in a buffer that does not hold
//! it (a new one, one cleared, one whose last frame had the subsurface
//! elsewhere or none). Where the new frame may differ from the one on the
//! screen there is known the same way, and only that is compared: the
//! window's frames cost what is drawn over the subsurface, never the
//! subsurface. That is kept of the first subsurface a frame shows; the
//! place of any other is erased whole each frame, and the window's buffer
//! blended over all of it.

use gfx::{Damage, Rect};

use crate::window::Buffers;

/// The most buffers a window has.
const MOST_BUFFERS: usize = 3;

/// The frame being drawn.
#[derive(Debug)]
struct Frame {
    buffers: Buffers,
    /// The area the canvas tracks while it is drawn: the last frame's
    /// place.
    tracked: Option<Rect>,
    /// Where it shows a subsurface, and where the frame on the screen
    /// differs from transparency there (but for what is drawn over it).
    shown: Option<(Rect, Damage)>,
    /// Where it shows others.
    others: Damage,
}

/// What a frame showed of a subsurface ([`Kept::end`]).
#[derive(Debug)]
pub(crate) struct Shown {
    /// Its place.
    pub rect: Rect,
    /// Where the frame may differ from the one on the screen there.
    pub maybe: Damage,
    /// What the frame drew over it: where the compositor blends the
    /// window's buffer over it.
    pub overlays: Damage,
}

/// What a window's buffers hold where it shows a subsurface, buffer by
/// buffer.
#[derive(Debug, Default)]
pub(crate) struct Kept {
    /// Where the last frame showed one.
    last: Option<Rect>,
    /// The surface whose buffers `held` describes.
    surface: u64,
    /// For each buffer that holds the place transparent, what was drawn
    /// over it there.
    held: [Option<Damage>; MOST_BUFFERS],
    frame: Option<Frame>,
}

/// All of `rect`, as damage.
fn all(rect: Rect) -> Damage {
    let mut d = Damage::new();
    d.add(rect);
    d
}

/// Where a buffer that holds `held` of the place `rect` is not transparent.
fn differs(held: Option<&Damage>, rect: Rect) -> Damage {
    match held {
        Some(covered) => {
            let mut d = Damage::new();
            for r in covered.rects() {
                d.add(r.intersect(&rect));
            }
            d
        }
        None => all(rect),
    }
}

impl Kept {
    /// A frame begins in `buffers` (cleared first if `cleared`): the area
    /// for the canvas to track while it is drawn.
    pub fn begin(&mut self, buffers: Buffers, cleared: bool) -> Option<Rect> {
        if buffers.surface != self.surface {
            self.surface = buffers.surface;
            self.last = None;
            self.held = Default::default();
        }
        if cleared && let Some(held) = self.held.get_mut(usize::from(buffers.back)) {
            *held = None;
        }
        self.frame = Some(Frame { buffers, tracked: self.last, shown: None, others: Damage::new() });
        self.last
    }

    /// The frame shows a subsurface in `rect`: where the frame's buffer is
    /// to be erased.
    pub fn show(&mut self, rect: Rect) -> Damage {
        // Outside a host's frames, nothing is known of the buffers.
        let Some(frame) = &mut self.frame else { return all(rect) };
        // A second in a frame: nothing is kept of it.
        if frame.shown.is_some() {
            frame.others.add(rect);
            return all(rect);
        }
        // Another place: no buffer holds it.
        if self.last != Some(rect) {
            self.held = Default::default();
        }
        let back = usize::from(frame.buffers.back);
        let erase = differs(self.held.get(back).and_then(Option::as_ref), rect);
        let against = match frame.buffers.shown.and_then(|s| self.held.get(usize::from(s))) {
            Some(held) => differs(held.as_ref(), rect),
            None => all(rect),
        };
        if let Some(held) = self.held.get_mut(back) {
            *held = Some(Damage::new());
        }
        frame.shown = Some((rect, against));
        erase
    }

    /// The frame is drawn; `touched` is where drawing touched the area the
    /// canvas tracked. What it showed of a subsurface, if it showed one.
    pub fn end(&mut self, touched: Option<Damage>) -> Option<Shown> {
        let frame = self.frame.take()?;
        let Some((rect, mut maybe)) = frame.shown else {
            // The buffer holds something else where the place was.
            self.last = None;
            return None;
        };
        // What was drawn over it, as the canvas noted it if it tracked the
        // place, and all of it if not.
        let mut overlays = match touched {
            Some(touched) if frame.tracked == Some(rect) => touched,
            _ => all(rect),
        };
        for r in overlays.rects() {
            maybe.add(*r);
        }
        if let Some(held) = self.held.get_mut(usize::from(frame.buffers.back)) {
            *held = Some(overlays.clone());
        }
        // The window's buffer over all of the others (compared whole, as
        // they lie outside `rect`).
        for r in frame.others.rects() {
            overlays.add(*r);
        }
        self.last = Some(rect);
        Some(Shown { rect, maybe, overlays })
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    const PAGE: Rect = Rect::new(0, 100, 800, 500);

    /// Two buffers, the frame drawn into `back`, the other on the screen
    /// once a frame was.
    fn buffers(back: u8, shown: bool) -> Buffers {
        Buffers { surface: 1, back, shown: shown.then_some(1 - back) }
    }

    fn rects(d: &Damage) -> Vec<Rect> {
        let mut v = d.rects().to_vec();
        v.sort_by_key(|r| (r.y, r.x));
        v
    }

    /// Draws a frame into `back` showing the page with `over` drawn over
    /// it: what was erased, where it may differ from the screen, and its
    /// overlays.
    fn frame(k: &mut Kept, back: u8, shown: bool, over: &[Rect]) -> (Vec<Rect>, Vec<Rect>, Vec<Rect>) {
        let tracked = k.begin(buffers(back, shown), false);
        let erase = k.show(PAGE);
        let touched = tracked.map(|area| {
            let mut d = Damage::new();
            for o in over {
                d.add(o.intersect(&area));
            }
            d
        });
        let s = k.end(touched).unwrap();
        assert_eq!(s.rect, PAGE);
        (rects(&erase), rects(&s.maybe), rects(&s.overlays))
    }

    /// The page shown in four frames, two in each buffer: the first frame
    /// that shows it is not tracked (the canvas tracks the last frame's
    /// place), so what it drew over it is taken as all of it.
    fn settled() -> Kept {
        let mut k = Kept::default();
        // Both buffers hold nothing of it at first.
        assert_eq!(frame(&mut k, 0, false, &[]), (alloc::vec![PAGE], alloc::vec![PAGE], alloc::vec![PAGE]));
        assert_eq!(frame(&mut k, 1, true, &[]), (alloc::vec![PAGE], alloc::vec![PAGE], alloc::vec![]));
        // Buffer 0's first frame was not tracked: all of it is erased again.
        assert_eq!(frame(&mut k, 0, true, &[]), (alloc::vec![PAGE], alloc::vec![], alloc::vec![]));
        assert_eq!(frame(&mut k, 1, true, &[]), (alloc::vec![], alloc::vec![], alloc::vec![]));
        k
    }

    #[test]
    fn what_was_drawn_over_it_is_overlaid_then_erased() {
        let mut k = settled();
        // A menu over the page in buffer 0, partly outside it.
        let menu = Rect::new(600, 80, 100, 200);
        let over = menu.intersect(&PAGE);
        assert_eq!(frame(&mut k, 0, true, &[menu]), (alloc::vec![], alloc::vec![over], alloc::vec![over]));
        // Buffer 1 never had it, but the screen did: it differs there.
        assert_eq!(frame(&mut k, 1, true, &[]), (alloc::vec![], alloc::vec![over], alloc::vec![]));
        // Buffer 0 is erased where the menu was.
        assert_eq!(frame(&mut k, 0, true, &[]), (alloc::vec![over], alloc::vec![], alloc::vec![]));
        assert_eq!(frame(&mut k, 1, true, &[]), (alloc::vec![], alloc::vec![], alloc::vec![]));
    }

    #[test]
    fn frames_that_draw_nothing_over_it_cost_nothing() {
        let mut k = settled();
        for back in [0, 1, 0, 1] {
            assert_eq!(frame(&mut k, back, true, &[]), (alloc::vec![], alloc::vec![], alloc::vec![]));
        }
    }

    #[test]
    fn another_place_surface_or_a_cleared_frame_erases_it_whole() {
        let mut k = settled();
        // Another place: and the canvas tracked the old one, so what was
        // drawn over it is not known.
        let moved = Rect::new(0, 0, 800, 600);
        k.begin(buffers(0, true), false);
        assert_eq!(rects(&k.show(moved)), [moved]);
        assert_eq!(rects(&k.end(Some(Damage::new())).unwrap().overlays), [moved]);
        // A frame without it, then it again: erased whole.
        k.begin(buffers(1, true), false);
        assert!(k.end(None).is_none());
        let tracked = k.begin(buffers(0, true), false);
        assert_eq!(tracked, None);
        assert_eq!(rects(&k.show(moved)), [moved]);
        k.end(None);
        // A new surface, whose buffers hold nothing.
        k.begin(Buffers { surface: 2, back: 0, shown: None }, false);
        assert_eq!(rects(&k.show(moved)), [moved]);
        k.end(Some(Damage::new()));
        k.begin(Buffers { surface: 2, back: 1, shown: Some(0) }, false);
        k.show(moved);
        k.end(Some(Damage::new()));
        // A frame cleared first.
        k.begin(Buffers { surface: 2, back: 0, shown: Some(1) }, true);
        assert_eq!(rects(&k.show(moved)), [moved]);
        k.end(Some(Damage::new()));
    }

    #[test]
    fn a_second_is_erased_and_overlaid_whole_and_the_first_kept() {
        let mut k = settled();
        let menu = Rect::new(600, 80, 100, 200);
        let over = menu.intersect(&PAGE);
        let other = Rect::new(0, 620, 800, 100);
        for (back, drawn, erased) in [(0, &[menu][..], None), (1, &[], None), (0, &[], Some(over))] {
            let tracked = k.begin(buffers(back, true), false);
            assert_eq!(tracked, Some(PAGE));
            assert_eq!(rects(&k.show(PAGE)), erased.into_iter().collect::<Vec<_>>());
            // The other is erased whole each frame, whatever was drawn.
            assert_eq!(rects(&k.show(other)), [other]);
            let mut touched = Damage::new();
            for o in drawn {
                touched.add(o.intersect(&PAGE));
            }
            let s = k.end(Some(touched)).unwrap();
            assert_eq!(s.rect, PAGE);
            // Over the first, what was drawn; over the other, all of it.
            let mut overlays: Vec<Rect> = drawn.iter().map(|o| o.intersect(&PAGE)).collect();
            overlays.push(other);
            overlays.sort_by_key(|r| (r.y, r.x));
            assert_eq!(rects(&s.overlays), overlays);
        }
    }

    #[test]
    fn outside_frames_it_is_erased_whole() {
        let mut k = Kept::default();
        assert_eq!(rects(&k.show(PAGE)), [PAGE]);
    }
}

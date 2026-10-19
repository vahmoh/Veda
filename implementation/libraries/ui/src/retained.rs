// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Pictures a window keeps from frame to frame ([`crate::Ui::retained`]):
//! a large one that changes in parts, as a web page does.
//!
//! A frame is drawn into one of the window's buffers, which still holds
//! the frame drawn into it last, two or three frames before. The picture is
//! copied into it again only where that frame left it otherwise: where the
//! picture changed since, and where something was drawn over it then (a
//! menu, a notice), as the canvas noted while that frame was drawn
//! ([`gfx::Canvas::track`]). Where the new frame may differ from the one on
//! the screen, there, is known the same way, and only that is compared, to
//! present what did change: drawing the frame costs what changed, not the
//! picture.

use gfx::{Damage, Rect};

use crate::window::Buffers;

/// The most buffers a window has.
const MOST_BUFFERS: usize = 3;

/// What a buffer holds of the picture.
#[derive(Debug, Clone, Default)]
struct Held {
    /// What changed of the picture since it was drawn into the buffer, in
    /// the window's coordinates; `None` when the buffer holds nothing of it.
    stale: Option<Damage>,
    /// What was drawn over it there.
    covered: Damage,
}

impl Held {
    /// Where the buffer's pixels are not the picture's, of `rect`.
    fn differs(&self, rect: Rect) -> Damage {
        let mut out = self.stale.clone().unwrap_or_else(|| all(rect));
        for r in self.covered.rects() {
            out.add(r.intersect(&rect));
        }
        out
    }
}

/// All of `rect`, as damage.
fn all(rect: Rect) -> Damage {
    let mut d = Damage::new();
    d.add(rect);
    d
}

/// The frame being drawn.
#[derive(Debug)]
struct Frame {
    buffers: Buffers,
    /// The area the canvas tracks while it is drawn: the last frame's
    /// picture's.
    tracked: Option<Rect>,
    /// What it shows: the picture's id and place, and where the frame on
    /// the screen differs from it there (but for what is drawn over it).
    shown: Option<(u64, Rect, Damage)>,
}

/// What a window keeps of the picture it retains, buffer by buffer.
#[derive(Debug, Default)]
pub(crate) struct Retained {
    /// The picture the last frame showed, and where.
    last: Option<(u64, Rect)>,
    /// The surface whose buffers `held` describes.
    surface: u64,
    held: [Held; MOST_BUFFERS],
    frame: Option<Frame>,
}

impl Retained {
    /// A frame begins in `buffers` (cleared first if `cleared`): the area
    /// for the canvas to track while it is drawn.
    pub fn begin(&mut self, buffers: Buffers, cleared: bool) -> Option<Rect> {
        if buffers.surface != self.surface {
            self.surface = buffers.surface;
            self.last = None;
        }
        if cleared && let Some(held) = self.held.get_mut(usize::from(buffers.back)) {
            held.stale = None;
        }
        let tracked = self.last.map(|(_, rect)| rect);
        self.frame = Some(Frame { buffers, tracked, shown: None });
        tracked
    }

    /// The frame shows picture `id` in `rect`, of which `changed` changed
    /// since the last frame (in the window's coordinates): where it must be
    /// copied into the frame's buffer.
    pub fn show(&mut self, id: u64, rect: Rect, changed: &[Rect]) -> Damage {
        // Outside a host's frames, nothing is known of the buffers.
        let Some(frame) = &mut self.frame else { return all(rect) };
        // Another picture, or another place, or the second in a frame: no
        // buffer holds it.
        if frame.shown.is_some() || self.last != Some((id, rect)) {
            self.held = Default::default();
        }
        for held in &mut self.held {
            if let Some(stale) = &mut held.stale {
                for r in changed {
                    stale.add(r.intersect(&rect));
                }
            }
        }
        let back = usize::from(frame.buffers.back);
        let copy = self.held.get(back).map_or_else(|| all(rect), |held| held.differs(rect));
        let against = match frame.buffers.shown.and_then(|s| self.held.get(usize::from(s))) {
            Some(held) => held.differs(rect),
            None => all(rect),
        };
        if let Some(held) = self.held.get_mut(back) {
            *held = Held { stale: Some(Damage::new()), covered: Damage::new() };
        }
        frame.shown = Some((id, rect, against));
        copy
    }

    /// The frame is drawn; `touched` is where drawing touched the area the
    /// canvas tracked. The picture's rectangle and where the frame may
    /// differ from the one on the screen there, if it showed the picture.
    pub fn end(&mut self, touched: Option<Damage>) -> Option<(Rect, Damage)> {
        let frame = self.frame.take()?;
        let Some((id, rect, mut damage)) = frame.shown else {
            // The buffer holds something else where the picture was.
            self.last = None;
            return None;
        };
        // What was drawn over the picture, as the canvas noted it if it
        // tracked the picture's place, and all of it if not.
        let covered = match touched {
            Some(touched) if frame.tracked == Some(rect) => touched,
            _ => all(rect),
        };
        for r in covered.rects() {
            damage.add(*r);
        }
        if let Some(held) = self.held.get_mut(usize::from(frame.buffers.back)) {
            held.covered = covered;
        }
        self.last = Some((id, rect));
        Some((rect, damage))
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

    /// Draws a frame into `back` showing the page, of which `changed`
    /// changed, with `over` drawn over it: what was copied, and the damage.
    fn frame(r: &mut Retained, back: u8, shown: bool, changed: &[Rect], over: &[Rect]) -> (Damage, Damage) {
        let tracked = r.begin(buffers(back, shown), false);
        let copy = r.show(7, PAGE, changed);
        let touched = tracked.map(|area| {
            let mut d = Damage::new();
            for o in over {
                d.add(o.intersect(&area));
            }
            d
        });
        let (rect, damage) = r.end(touched).unwrap();
        assert_eq!(rect, PAGE);
        (copy, damage)
    }

    /// The page shown in four frames, two in each buffer: the first frame
    /// that shows it is not tracked (the canvas tracks the last frame's
    /// picture), so what it drew over it is taken as all of it.
    fn settled() -> Retained {
        let mut r = Retained::default();
        // Both buffers hold nothing of it at first.
        let (copy, damage) = frame(&mut r, 0, false, &[], &[]);
        assert_eq!((rects(&copy), rects(&damage)), (alloc::vec![PAGE], alloc::vec![PAGE]));
        let (copy, damage) = frame(&mut r, 1, true, &[], &[]);
        assert_eq!((rects(&copy), rects(&damage)), (alloc::vec![PAGE], alloc::vec![PAGE]));
        // Buffer 0's first frame was not tracked; buffer 1's was.
        let (copy, damage) = frame(&mut r, 0, true, &[], &[]);
        assert_eq!((rects(&copy), rects(&damage)), (alloc::vec![PAGE], alloc::vec![]));
        let (copy, damage) = frame(&mut r, 1, true, &[], &[]);
        assert!(copy.is_empty() && damage.is_empty());
        r
    }

    #[test]
    fn a_buffer_gets_what_changed_since_it_was_drawn() {
        let mut r = settled();
        // What changed in the frames since each was drawn: buffer 0 missed
        // the changes of the last two frames.
        let a = Rect::new(10, 110, 20, 20);
        let b = Rect::new(400, 300, 30, 30);
        let (_, damage) = frame(&mut r, 0, true, &[a], &[]);
        assert_eq!(rects(&damage), [a]);
        let (copy, damage) = frame(&mut r, 1, true, &[b], &[]);
        assert_eq!(rects(&copy), [a, b]);
        assert_eq!(rects(&damage), [b]);
        // Nothing changed: nothing to copy, nothing to present.
        let (copy, damage) = frame(&mut r, 0, true, &[], &[]);
        assert_eq!(rects(&copy), [b]);
        assert_eq!(rects(&damage), []);
        let (copy, damage) = frame(&mut r, 1, true, &[], &[]);
        assert!(copy.is_empty() && damage.is_empty());
    }

    #[test]
    fn what_was_drawn_over_it_is_copied_again_and_presented() {
        let mut r = settled();
        // A menu over the page in buffer 0, partly outside it.
        let menu = Rect::new(600, 80, 100, 200);
        let (copy, damage) = frame(&mut r, 0, true, &[], &[menu]);
        assert!(copy.is_empty());
        let over = menu.intersect(&PAGE);
        assert_eq!(rects(&damage), [over]);
        // Buffer 1 never had it, but the screen did: it goes from the screen.
        let (copy, damage) = frame(&mut r, 1, true, &[], &[]);
        assert!(copy.is_empty());
        assert_eq!(rects(&damage), [over]);
        // Buffer 0 has the page put back where the menu was.
        let (copy, damage) = frame(&mut r, 0, true, &[], &[]);
        assert_eq!(rects(&copy), [over]);
        assert!(damage.is_empty());
    }

    #[test]
    fn frames_not_presented_keep_the_screen_s_buffer_stale() {
        let mut r = settled();
        // Two frames into buffer 0 (the first, say, was not presented): the
        // screen's buffer, 1, misses both changes.
        let (a, b) = (Rect::new(0, 100, 10, 10), Rect::new(50, 150, 10, 10));
        frame(&mut r, 0, true, &[a], &[]);
        let (copy, damage) = frame(&mut r, 0, true, &[b], &[]);
        assert_eq!(rects(&copy), [b]);
        assert_eq!(rects(&damage), [a, b]);
    }

    #[test]
    fn another_picture_place_or_surface_is_copied_whole() {
        let mut r = settled();
        // Another picture.
        r.begin(buffers(0, true), false);
        assert_eq!(rects(&r.show(8, PAGE, &[])), [PAGE]);
        r.end(Some(Damage::new()));
        // Another place: and the canvas tracked the old one, so what was
        // drawn over it is not known.
        let moved = Rect::new(0, 0, 800, 600);
        r.begin(buffers(1, true), false);
        assert_eq!(rects(&r.show(8, moved, &[])), [moved]);
        assert_eq!(rects(&r.end(Some(Damage::new())).unwrap().1), [moved]);
        // A frame without it, then it again: copied whole.
        r.begin(buffers(0, true), false);
        assert!(r.end(None).is_none());
        let tracked = r.begin(buffers(1, true), false);
        assert_eq!(tracked, None);
        assert_eq!(rects(&r.show(8, moved, &[])), [moved]);
        r.end(None);
        // A new surface, whose buffers hold nothing.
        let fresh = Buffers { surface: 2, back: 0, shown: None };
        r.begin(fresh, false);
        assert_eq!(rects(&r.show(8, moved, &[])), [moved]);
        r.end(Some(Damage::new()));
        // A frame cleared first.
        frame_cleared_copies_all(&mut r, moved);
    }

    fn frame_cleared_copies_all(r: &mut Retained, rect: Rect) {
        r.begin(Buffers { surface: 2, back: 1, shown: Some(0) }, false);
        r.show(8, rect, &[]);
        r.end(Some(Damage::new()));
        r.begin(Buffers { surface: 2, back: 0, shown: Some(1) }, true);
        assert_eq!(rects(&r.show(8, rect, &[])), [rect]);
        r.end(Some(Damage::new()));
    }

    #[test]
    fn outside_frames_it_is_copied_whole() {
        let mut r = Retained::default();
        assert_eq!(rects(&r.show(7, PAGE, &[])), [PAGE]);
    }
}

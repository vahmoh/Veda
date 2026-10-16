// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Where the picture is in its window: its zoom and its position, and the way between the
//! window's pixels and the picture's.

use vgfx::Rect;

/// The zoom levels the zoom keys step through.
pub const ZOOMS: [f32; 20] = [
    0.03125,
    0.0625,
    0.125,
    1.0 / 6.0,
    0.25,
    1.0 / 3.0,
    0.5,
    2.0 / 3.0,
    0.75,
    1.0,
    1.5,
    2.0,
    3.0,
    4.0,
    6.0,
    8.0,
    12.0,
    16.0,
    24.0,
    32.0,
];
pub const MIN_ZOOM: f32 = 0.03125;
pub const MAX_ZOOM: f32 = 32.0;
/// How much of the window around the picture can show beyond its edges (pixels).
pub const MARGIN: f32 = 48.0;

/// The zoom and where the picture's top left corner is, from the viewport's top left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub zoom: f32,
    pub origin: (f32, f32),
}

impl Default for View {
    fn default() -> View {
        View { zoom: 1.0, origin: (0.0, 0.0) }
    }
}

impl View {
    /// The view that shows all of a `w` by `h` picture in `viewport`, at most at 100%.
    pub fn fit(viewport: Rect, w: i32, h: i32) -> View {
        let room = |side: i32| (side as f32 - 2.0 * 24.0).max(16.0);
        let zoom = (room(viewport.w) / w.max(1) as f32).min(room(viewport.h) / h.max(1) as f32).min(1.0);
        let mut v = View { zoom: zoom.clamp(MIN_ZOOM, MAX_ZOOM), origin: (0.0, 0.0) };
        v.clamp(viewport, w, h);
        v
    }

    /// The window position of the picture's point (`x`, `y`).
    pub fn to_screen(&self, viewport: Rect, x: f32, y: f32) -> (f32, f32) {
        (viewport.x as f32 + self.origin.0 + x * self.zoom, viewport.y as f32 + self.origin.1 + y * self.zoom)
    }

    /// The picture's point at the window position (`sx`, `sy`).
    pub fn to_picture(&self, viewport: Rect, sx: f32, sy: f32) -> (f32, f32) {
        ((sx - viewport.x as f32 - self.origin.0) / self.zoom, (sy - viewport.y as f32 - self.origin.1) / self.zoom)
    }

    /// Where a `w` by `h` picture is in the window.
    pub fn picture_rect(&self, viewport: Rect, w: i32, h: i32) -> Rect {
        let (x0, y0) = self.to_screen(viewport, 0.0, 0.0);
        let (x1, y1) = self.to_screen(viewport, w as f32, h as f32);
        let px = |v: f32| libm::roundf(v.clamp(-1e7, 1e7)) as i32;
        Rect::new(px(x0), px(y0), px(x1) - px(x0), px(y1) - px(y0))
    }

    /// Where the rectangle `r` of the picture is in the window.
    pub fn rect_to_screen(&self, viewport: Rect, r: Rect) -> Rect {
        let (x0, y0) = self.to_screen(viewport, r.x as f32, r.y as f32);
        let (x1, y1) = self.to_screen(viewport, r.right() as f32, r.bottom() as f32);
        let px = |v: f32| libm::roundf(v.clamp(-1e7, 1e7)) as i32;
        Rect::new(px(x0), px(y0), px(x1) - px(x0), px(y1) - px(y0))
    }

    /// Keeps the picture in view: centred along a side it does not fill, its edges no further
    /// than [`MARGIN`] inside the viewport along one it does.
    pub fn clamp(&mut self, viewport: Rect, w: i32, h: i32) {
        let along = |origin: f32, view: i32, side: i32| {
            let size = side as f32 * self.zoom;
            let view = view as f32;
            if size <= view { libm::roundf((view - size) / 2.0) } else { origin.clamp(view - MARGIN - size, MARGIN) }
        };
        self.origin = (along(self.origin.0, viewport.w, w), along(self.origin.1, viewport.h, h));
    }

    /// Zooms to `zoom`, keeping the picture's point under (`sx`, `sy`) there.
    pub fn zoom_at(&mut self, viewport: Rect, zoom: f32, sx: f32, sy: f32) {
        let (px, py) = self.to_picture(viewport, sx, sy);
        self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.origin = (sx - viewport.x as f32 - px * self.zoom, sy - viewport.y as f32 - py * self.zoom);
    }

    /// The next zoom level in, or out.
    pub fn step(&self, inwards: bool) -> f32 {
        let next = if inwards {
            ZOOMS.iter().copied().find(|&z| z > self.zoom * 1.001)
        } else {
            ZOOMS.iter().rev().copied().find(|&z| z < self.zoom * 0.999)
        };
        next.unwrap_or(if inwards { MAX_ZOOM } else { MIN_ZOOM })
    }

    /// Scrolls by (`dx`, `dy`) window pixels.
    pub fn scroll(&mut self, dx: f32, dy: f32) {
        self.origin = (self.origin.0 + dx, self.origin.1 + dy);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEWPORT: Rect = Rect::new(100, 50, 800, 600);

    #[test]
    fn fitting_and_centring() {
        let v = View::fit(VIEWPORT, 400, 300);
        assert_eq!(v.zoom, 1.0, "small pictures at 100%");
        assert_eq!(v.picture_rect(VIEWPORT, 400, 300), Rect::new(300, 200, 400, 300));
        let v = View::fit(VIEWPORT, 3000, 1000);
        assert!(v.zoom < 0.26 && v.zoom > 0.25);
        let r = v.picture_rect(VIEWPORT, 3000, 1000);
        assert!(VIEWPORT.contains_rect(&r));
    }

    #[test]
    fn the_point_under_the_pointer_stays() {
        let mut v = View::fit(VIEWPORT, 400, 300);
        let before = v.to_picture(VIEWPORT, 420.0, 260.0);
        v.zoom_at(VIEWPORT, 4.0, 420.0, 260.0);
        let after = v.to_picture(VIEWPORT, 420.0, 260.0);
        assert!((before.0 - after.0).abs() < 1e-3 && (before.1 - after.1).abs() < 1e-3);
        let (sx, sy) = v.to_screen(VIEWPORT, after.0, after.1);
        assert!((sx - 420.0).abs() < 1e-3 && (sy - 260.0).abs() < 1e-3);
    }

    #[test]
    fn scrolling_stops_at_the_edges() {
        let mut v = View { zoom: 4.0, origin: (0.0, 0.0) };
        v.scroll(10_000.0, -10_000.0);
        v.clamp(VIEWPORT, 400, 300);
        assert_eq!(v.origin, (MARGIN, 600.0 - MARGIN - 1200.0));
    }

    #[test]
    fn zoom_steps() {
        let v = View { zoom: 1.0, origin: (0.0, 0.0) };
        assert_eq!(v.step(true), 1.5);
        assert_eq!(v.step(false), 0.75);
        assert_eq!(View { zoom: 0.9, ..v }.step(true), 1.0);
        assert_eq!(View { zoom: 32.0, ..v }.step(true), MAX_ZOOM);
        assert_eq!(View { zoom: MIN_ZOOM, ..v }.step(false), MIN_ZOOM);
    }
}

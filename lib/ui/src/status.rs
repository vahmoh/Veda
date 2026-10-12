//! Status glyphs: the network and the sound, as the taskbar's tray and the
//! panels it opens show them.
//!
//! The line icons ([`Icon`](crate::Icon)) scale to any size; these are
//! drawn for one. They are solid shapes on a grid of whole pixels, 20 high,
//! their straight edges on pixel boundaries and their strokes two pixels
//! wide, so that they stay crisp and clear at a tray's size on a 1x screen.
//! What is off is faint (the bars a weak signal lacks), and a slash or a
//! badge cuts a gap into what it crosses: each glyph is drawn into a
//! [`Layer`] first, where gaps can be cut, and then laid on the canvas.

use core::f32::consts::PI;

use vgfx::{Bitmap, Canvas, Color, FillRule, LineCap, LineJoin, Path, Rect, StrokeStyle, Transform};
use vraster::Rasterizer;

/// The height of a glyph in pixels.
pub const SIZE: i32 = 20;
/// The width of the speaker, whose waves need more room.
const SPEAKER_WIDTH: i32 = 22;
/// The width of strokes.
const STROKE: f32 = 2.0;
/// The gap a slash or a badge cuts around itself.
const GAP: f32 = 1.5;
/// How strongly what is off shows.
const FAINT: f32 = 0.3;

/// A transparent layer over part of a canvas: a glyph is drawn into it,
/// gaps are cut out of it, and then it is laid on the canvas at once.
pub struct Layer {
    bitmap: Bitmap,
    /// Where the layer's top left goes on the canvas.
    x: i32,
    y: i32,
    raster: Rasterizer,
}

impl Layer {
    /// A layer over `r` of a canvas.
    pub fn new(r: Rect) -> Layer {
        Layer { bitmap: Bitmap::new(r.w, r.h), x: r.x, y: r.y, raster: Rasterizer::new() }
    }

    /// The layer's canvas, its origin at the layer's top left.
    pub fn canvas(&mut self) -> Canvas<'_> {
        Canvas::for_bitmap(&mut self.bitmap)
    }

    /// Cuts `path` out of what is drawn: each pixel keeps only as much as
    /// the path leaves uncovered.
    pub fn cut(&mut self, path: &Path) {
        let (w, h) = (self.bitmap.width as u32, self.bitmap.height as u32);
        let pixels = &mut self.bitmap.pixels;
        self.raster.fill(path, &Transform::IDENTITY, FillRule::NonZero, (w, h), |span| {
            let start = (span.y * w + span.x) as usize;
            for i in 0..span.len {
                let cover = span.coverage_at(i) as u32;
                if cover > 0 {
                    let px = &mut pixels[start + i as usize];
                    *px = vgfx::color::scale(*px, 255 - cover);
                }
            }
        });
    }

    /// A slash from `from` to `to` with a gap cut along each side of it.
    pub fn slash(&mut self, from: (f32, f32), to: (f32, f32), color: Color) {
        let mut line = Path::new();
        line.move_to(from.0, from.1);
        line.line_to(to.0, to.1);
        self.cut(&vraster::stroke(&line, &round(STROKE + 2.0 * GAP), 0.05));
        self.canvas().stroke_path(&line, &round(STROKE), color);
    }

    /// A badge: a dot of `radius` at (`cx`, `cy`) with a gap cut around it.
    pub fn badge(&mut self, cx: f32, cy: f32, radius: f32, color: Color) {
        let mut around = Path::new();
        around.circle(cx, cy, radius + GAP);
        self.cut(&around);
        let mut dot = Path::new();
        dot.circle(cx, cy, radius);
        self.canvas().fill_path(&dot, color, FillRule::NonZero);
    }

    /// Lays the layer on `c`.
    pub fn draw(&self, c: &mut Canvas) {
        c.draw_bitmap(&self.bitmap, self.x, self.y, 255);
    }
}

/// A stroke style with round ends and corners.
fn round(width: f32) -> StrokeStyle {
    StrokeStyle::new(width).with_cap(LineCap::Round).with_join(LineJoin::Round)
}

/// The box of a glyph `w` pixels wide, centred in `r` on whole pixels.
fn glyph_box(r: Rect, w: i32) -> Rect {
    Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - SIZE) / 2, w, SIZE)
}

/// An arc of radius `r` around (`cx`, `cy`), `half` degrees either side
/// of the direction `towards` (in degrees, clockwise from the right).
fn arc(cx: f32, cy: f32, r: f32, towards: f32, half: f32) -> Path {
    let mut p = Path::new();
    p.arc(cx, cy, r, (towards - half) * PI / 180.0, 2.0 * half * PI / 180.0);
    p
}

/// A polygon whose corners are rounded, each by its own radius: the
/// points are (x, y, radius).
fn rounded_polygon(points: &[(f32, f32, f32)]) -> Path {
    let n = points.len();
    let mut p = Path::new();
    for i in 0..n {
        let (px, py, _) = points[(i + n - 1) % n];
        let (x, y, r) = points[i];
        let (nx, ny, _) = points[(i + 1) % n];
        let (ax, ay, bx, by) = (px - x, py - y, nx - x, ny - y);
        let (la, lb) = (vmath::FloatExt::sqrt(ax * ax + ay * ay), vmath::FloatExt::sqrt(bx * bx + by * by));
        // How far from the corner along each side a circle of radius `r`
        // touches them.
        let cos = ((ax * bx + ay * by) / (la * lb)).clamp(-1.0, 1.0);
        let half = vmath::FloatExt::acos(cos) / 2.0;
        let d = (r / vmath::FloatExt::tan(half)).min(la / 2.0).min(lb / 2.0);
        let (sx, sy) = (x + ax / la * d, y + ay / la * d);
        let (ex, ey) = (x + bx / lb * d, y + by / lb * d);
        if i == 0 {
            p.move_to(sx, sy);
        } else {
            p.line_to(sx, sy);
        }
        // Bézier handles 0.55 of the way to the corner: nearly a circle.
        const K: f32 = 0.55;
        p.cubic_to(sx + (x - sx) * K, sy + (y - sy) * K, ex + (x - ex) * K, ey + (y - ey) * K, ex, ey);
    }
    p.close();
    p
}

/// What the network glyph shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    /// Connected by Wi-Fi, with a signal of 0-4 bars.
    Wifi(u8),
    /// Joining a Wi-Fi network: the arcs light up one more at each step,
    /// then start again (`step` counts on for ever).
    Joining(u32),
    /// Wi-Fi is on but not connected: the whole fan faint.
    WifiIdle,
    /// Wi-Fi is switched off: the fan faint and crossed out.
    WifiOff,
    /// Connected by cable.
    Wired,
    /// No connection, and no Wi-Fi to make one with.
    Offline,
}

impl Network {
    /// Draws the glyph centred in `r`.
    pub fn draw(self, c: &mut Canvas, r: Rect, color: Color) {
        self.layer(r, 0, color).draw(c);
    }

    /// Draws the glyph with a padlock at its foot, for a network that
    /// needs a password. The glyph is where [`Network::draw`] puts it; the
    /// padlock reaches two pixels further right.
    pub fn draw_locked(self, c: &mut Canvas, r: Rect, color: Color) {
        let mut l = self.layer(r, PADLOCK_REACH, color);
        padlock(&mut l, color);
        l.draw(c);
    }

    /// The glyph centred in `r`, in a layer `extra` pixels wider on the
    /// right.
    fn layer(self, r: Rect, extra: i32, color: Color) -> Layer {
        let b = glyph_box(r, SIZE);
        let mut l = Layer::new(Rect::new(b.x, b.y, SIZE + extra, SIZE));
        let faint = color.fade(FAINT);
        match self {
            Network::Wifi(bars) => fan(&mut l, bars.min(3), color, faint),
            Network::Joining(step) => fan(&mut l, (step % 4) as u8, color, faint),
            Network::WifiIdle => fan(&mut l, 0, faint, faint),
            Network::WifiOff => {
                fan(&mut l, 0, faint, faint);
                l.slash((3.0, 2.5), (17.0, 16.5), color);
            }
            Network::Wired => wired(&mut l, color),
            Network::Offline => {
                wired(&mut l, faint);
                l.slash((3.0, 3.0), (17.0, 17.0), color);
            }
        }
        l
    }
}

/// The Wi-Fi fan: a dot under three arcs, the dot and the first `lit`
/// arcs in `on` and the others in `off`.
fn fan(l: &mut Layer, lit: u8, on: Color, off: Color) {
    // The centre of the arcs; the outer one's top is on a pixel boundary.
    const CX: f32 = 10.0;
    const CY: f32 = 15.5;
    let mut c = l.canvas();
    let mut dot = Path::new();
    dot.circle(CX, CY, 1.75);
    c.fill_path(&dot, on, FillRule::NonZero);
    for (i, r) in [4.5, 8.5, 12.5].into_iter().enumerate() {
        let color = if (i as u8) < lit { on } else { off };
        c.stroke_path(&arc(CX, CY, r, -90.0, 45.0), &round(STROKE), color);
    }
}

/// A wired connection: three boxes joined, the mark beside network
/// sockets.
fn wired(l: &mut Layer, color: Color) {
    let mut boxes = Path::new();
    for (x, y) in [(7.0, 2.0), (2.0, 13.0), (12.0, 13.0)] {
        boxes.rounded_rect(x, y, 6.0, 5.0, [1.25; 4]);
    }
    // The wires reach half a pixel into the boxes, so that no seam shows.
    let mut wires = Path::new();
    wires.move_to(10.0, 6.5);
    wires.line_to(10.0, 11.0);
    wires.move_to(5.0, 13.5);
    wires.line_to(5.0, 11.0);
    wires.line_to(15.0, 11.0);
    wires.line_to(15.0, 13.5);
    let mut c = l.canvas();
    c.fill_path(&boxes, color, FillRule::NonZero);
    c.stroke_path(&wires, &StrokeStyle::new(STROKE).with_join(LineJoin::Miter), color);
}

/// How far right of a glyph's box the padlock reaches.
const PADLOCK_REACH: i32 = 2;

/// A padlock at the bottom right, over the ends of the fan's arcs, with a
/// gap cut around it.
fn padlock(l: &mut Layer, color: Color) {
    let mut around = Path::new();
    around.rounded_rect(13.5, 8.75, 9.0, 11.75, [2.5; 4]);
    l.cut(&around);
    let mut body = Path::new();
    body.rounded_rect(15.0, 14.0, 6.0, 5.0, [1.0; 4]);
    let mut shackle = Path::new();
    shackle.move_to(16.25, 14.5);
    shackle.line_to(16.25, 12.75);
    shackle.arc(18.0, 12.75, 1.75, PI, PI);
    shackle.line_to(19.75, 14.5);
    let mut c = l.canvas();
    c.fill_path(&body, color, FillRule::NonZero);
    c.stroke_path(&shackle, &StrokeStyle::new(1.5), color);
}

/// Draws the speaker for `volume` (0-1) centred in `r`: one to three
/// waves as it rises, or a cross when muted or silent.
pub fn draw_volume(c: &mut Canvas, r: Rect, volume: f32, muted: bool, color: Color) {
    let mut l = Layer::new(glyph_box(r, SPEAKER_WIDTH));
    let mut k = l.canvas();
    let body = rounded_polygon(&[
        (1.0, 7.0, 1.0),
        (4.5, 7.0, 0.8),
        (9.0, 3.0, 1.2),
        (9.0, 17.0, 1.2),
        (4.5, 13.0, 0.8),
        (1.0, 13.0, 1.0),
    ]);
    k.fill_path(&body, color, FillRule::NonZero);
    if muted || volume <= 0.001 {
        let mut cross = Path::new();
        let (cx, cy, h) = (15.0, 10.0, 2.75);
        cross.move_to(cx - h, cy - h);
        cross.line_to(cx + h, cy + h);
        cross.move_to(cx + h, cy - h);
        cross.line_to(cx - h, cy + h);
        k.stroke_path(&cross, &round(STROKE), color);
    } else {
        let waves = vmath::FloatExt::ceil(volume * 3.0).clamp(1.0, 3.0) as usize;
        for r in [3.5, 7.25, 11.0].into_iter().take(waves) {
            k.stroke_path(&arc(9.0, 10.0, r, 0.0, 45.0), &round(STROKE), color);
        }
    }
    l.draw(c);
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Color = Color::WHITE;

    /// The alpha of each pixel of a glyph drawn into a transparent box.
    fn alpha(w: i32, draw: impl FnOnce(&mut Canvas, Rect)) -> alloc::vec::Vec<u8> {
        let mut b = Bitmap::new(w, SIZE);
        draw(&mut Canvas::for_bitmap(&mut b), Rect::new(0, 0, w, SIZE));
        b.pixels.iter().map(|p| (p >> 24) as u8).collect()
    }

    #[test]
    fn straight_edges_fall_on_whole_pixels() {
        // The wired mark: its middle wire covers columns 9 and 10 fully and
        // nothing beside them, as do the wires to the lower boxes.
        let a = alpha(SIZE, |c, r| Network::Wired.draw(c, r, WHITE));
        let at = |x: i32, y: i32| a[(y * SIZE + x) as usize];
        assert_eq!((at(8, 9), at(9, 9), at(10, 9), at(11, 9)), (0, 255, 255, 0));
        assert_eq!((at(3, 12), at(4, 12), at(5, 12), at(6, 12)), (0, 255, 255, 0));
        // The crossbar: rows 10 and 11, solid.
        assert_eq!((at(7, 9), at(7, 10), at(7, 11), at(7, 12)), (0, 255, 255, 0));
        // The speaker's box: solid from row 7 to row 12.
        let s = alpha(SPEAKER_WIDTH, |c, r| draw_volume(c, r, 1.0, false, WHITE));
        let col = |y: i32| s[(y * SPEAKER_WIDTH + 3) as usize];
        assert_eq!((col(6), col(7), col(12), col(13)), (0, 255, 255, 0));
    }

    #[test]
    fn faint_parts_show_less() {
        let a = alpha(SIZE, |c, r| Network::Wifi(1).draw(c, r, WHITE));
        // The top of each arc, in the middle column: the first lit, the
        // outer two faint.
        let top = |r: f32| a[((15.5 - r) as i32 * SIZE + 10) as usize];
        assert_eq!(top(4.5), 255);
        assert!(top(8.5) < 100 && top(8.5) > 50, "{}", top(8.5));
        assert_eq!(top(8.5), top(12.5));
    }

    #[test]
    fn a_slash_cuts_a_gap_on_both_sides() {
        let a = alpha(SIZE, |c, r| Network::WifiOff.draw(c, r, WHITE));
        let on = alpha(SIZE, |c, r| Network::WifiIdle.draw(c, r, WHITE));
        let at = |v: &[u8], x: i32, y: i32| v[(y * SIZE + x) as usize];
        // On the slash (from (3, 2.5) to (17, 16.5)): solid, but for a
        // corner of the pixel.
        assert!(at(&a, 10, 9) > 250);
        // Where it crosses the middle arc, the arc is cut away on either
        // side of it (pixels whose centres are 1.77 from its middle).
        assert!(at(&on, 10, 7) > 0 && at(&a, 10, 7) == 0);
        assert!(at(&on, 5, 7) > 0 && at(&a, 5, 7) == 0);
    }

    #[test]
    fn a_padlock_leaves_the_fan_where_it_was() {
        // In a box wider than both: the fan's left part is the same pixels,
        // and the padlock's body reaches past the fan's box (columns 4-23).
        let w = 28;
        let plain = alpha(w, |c, r| Network::Wifi(4).draw(c, r, WHITE));
        let locked = alpha(w, |c, r| Network::Wifi(4).draw_locked(c, r, WHITE));
        let column = |v: &[u8], x: i32| (0..SIZE).map(|y| v[(y * w + x) as usize]).collect::<alloc::vec::Vec<_>>();
        for x in 0..13 {
            assert_eq!(column(&plain, x), column(&locked, x), "column {x}");
        }
        assert_eq!((plain[(16 * w + 24) as usize], locked[(16 * w + 24) as usize]), (0, 255));
    }

    #[test]
    fn volume_shows_in_waves() {
        let count = |v: f32| {
            let a = alpha(SPEAKER_WIDTH, |c, r| draw_volume(c, r, v, false, WHITE));
            // Along the middle row, right of the cone: one run per wave.
            let row = &a[(10 * SPEAKER_WIDTH) as usize..(11 * SPEAKER_WIDTH) as usize];
            row[10..].split(|&x| x < 128).filter(|run| !run.is_empty()).count()
        };
        assert_eq!([count(0.05), count(0.5), count(1.0)], [1, 2, 3]);
        let muted = alpha(SPEAKER_WIDTH, |c, r| draw_volume(c, r, 1.0, true, WHITE));
        assert_eq!(muted[(10 * SPEAKER_WIDTH + 15) as usize], 255, "the cross's middle");
    }
}

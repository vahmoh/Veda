//! What the menus and the dashboard are drawn with: the fonts, the
//! palette, and a few shapes (stars, a padlock, the logo).

use vgfx::{Canvas, Color, FillRule, LineCap, LineJoin, Path, Rect, StrokeStyle, Text};

/// The fonts, by role.
pub struct Fonts {
    pub regular: usize,
    pub bold: usize,
    pub mono: usize,
    pub mono_bold: usize,
}

pub const WHITE: Color = Color(0xFFFFFFFF);
pub const DIM: Color = Color(0xB0E6ECF5);
pub const FAINT: Color = Color(0x70E6ECF5);
pub const ACCENT: Color = Color(0xFF3D8BFF);
pub const RED: Color = Color(0xFFFF3B3B);
pub const AMBER: Color = Color(0xFFFFB13D);
pub const GREEN: Color = Color(0xFF4CD964);
pub const GOLD: Color = Color(0xFFFFC93D);
/// Panels over the picture.
pub const PANEL: Color = Color(0xB806090F);
pub const PANEL_DARK: Color = Color(0xE006090F);

/// The scale of everything drawn, for a canvas `h` pixels high (designed
/// at 720).
pub fn scale(h: i32) -> f32 {
    (h as f32 / 720.0).max(0.6)
}

/// Draws `s` centred on `x`.
pub fn centered(c: &mut Canvas, text: &mut Text, font: usize, size: f32, x: f32, y: f32, s: &str, color: Color) {
    let w = text.measure(font, size, s);
    text.draw(c, font, size, x - w * 0.5, y, s, color);
}

/// Draws `s` ending at `x`.
pub fn right(c: &mut Canvas, text: &mut Text, font: usize, size: f32, x: f32, y: f32, s: &str, color: Color) {
    let w = text.measure(font, size, s);
    text.draw(c, font, size, x - w, y, s, color);
}

/// A rectangle from floats.
pub fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::new(x as i32, y as i32, w.max(0.0) as i32, h.max(0.0) as i32)
}

/// A five-pointed star `r` across from its middle, filled or hollow.
pub fn star(c: &mut Canvas, cx: f32, cy: f32, r: f32, filled: bool, color: Color) {
    let mut p = Path::new();
    for k in 0..10 {
        let a = -core::f32::consts::FRAC_PI_2 + k as f32 * core::f32::consts::PI / 5.0;
        let rr = if k % 2 == 0 { r } else { r * 0.42 };
        let (x, y) = (cx + vmath::FloatExt::cos(a) * rr, cy + vmath::FloatExt::sin(a) * rr);
        if k == 0 {
            p.move_to(x, y);
        } else {
            p.line_to(x, y);
        }
    }
    p.close();
    if filled {
        c.fill_path(&p, color, FillRule::NonZero);
    } else {
        let style =
            StrokeStyle { width: (r * 0.16).max(1.0), cap: LineCap::Round, join: LineJoin::Round, miter_limit: 4.0 };
        c.stroke_path(&p, &style, color);
    }
}

/// `n` stars of 3 in a row from (`x`, `cy`), `r` each.
pub fn stars(c: &mut Canvas, x: f32, cy: f32, r: f32, n: u8) {
    for k in 0..3u8 {
        let on = k < n;
        star(c, x + r + k as f32 * r * 2.4, cy, r, on, if on { GOLD } else { Color(0x60E6ECF5) });
    }
}

/// A padlock `h` high from (`x`, `y`) at its top left.
pub fn padlock(c: &mut Canvas, x: f32, y: f32, h: f32, color: Color) {
    let w = h * 0.8;
    let mut shackle = Path::new();
    shackle.arc(x + w * 0.5, y + h * 0.42, w * 0.3, core::f32::consts::PI, core::f32::consts::PI);
    let style = StrokeStyle { width: h * 0.12, cap: LineCap::Butt, join: LineJoin::Round, miter_limit: 4.0 };
    c.stroke_path(&shackle, &style, color);
    c.fill_rounded_rect(rect(x, y + h * 0.42, w, h * 0.58), h * 0.1, color);
}

/// The game's name with a light bar's red and blue either side of it, its
/// middle at `cx`, its baseline at `y`, `size` high.
pub fn logo(c: &mut Canvas, text: &mut Text, f: &Fonts, x: f32, y: f32, size: f32, centred: bool) {
    let name = "INTERCEPTOR";
    let w = text.measure(f.bold, size, name);
    let left = if centred { x - w * 0.5 } else { x };
    text.draw(c, f.bold, size, left, y, name, WHITE);
    let bar = size * 0.09;
    let by = y + size * 0.22;
    c.fill_rounded_rect(rect(left, by, w * 0.5 - size * 0.06, bar), bar * 0.5, RED);
    c.fill_rounded_rect(rect(left + w * 0.5 + size * 0.06, by, w * 0.5 - size * 0.06, bar), bar * 0.5, ACCENT);
    let tag = "VEDA BAY HIGHWAY PATROL";
    let ts = size * 0.26;
    let tw = text.measure(f.bold, ts, tag);
    let tx = if centred { x - tw * 0.5 } else { left };
    text.draw(c, f.bold, ts, tx, by + bar + ts * 1.5, tag, DIM);
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Drawing of window decorations, client surfaces and the mouse cursor:
//! by the processor into a canvas, or by the GPU (`gpu`), which draws the
//! same shapes from the same formulas and takes what is text or a drawing
//! (title bars, cursors, the switcher) from bitmaps drawn here.
//!
//! A window's client area shows its buffer, and its subsurfaces where its
//! frame places them ([`Layer`]): their buffers, opaque, or their
//! backgrounds where they do not reach, and the window's buffer again over
//! them within its overlays, blended. Elsewhere over a subsurface the
//! window's buffer is not read.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use compositor::switcher::Switcher;
use compositor::window::{BUTTON_WIDTH, CORNER_RADIUS, Part, TITLE_HEIGHT};
use compositor::wm::DecorState;
use gfx::color::{over, scale};
use gfx::{Bitmap, Canvas, Color, FillRule, Path, Rect, ShadowTemplate, StrokeStyle, Text};
use num_traits::Float;
use proto::display::{self as dp, Cursor, WindowKind, WindowState};
use runtime::vm::Mapping;

use crate::gpu::{Gpu, Slot, Texture, rgba};
use crate::pixels::{self, Pixels};

/// A window, its client's buffers mapped here.
pub type Window = compositor::window::Window<Mapping>;

/// Decorated windows' shadows: blurred this far around their frame, which
/// they are below by this much.
const SHADOW_BLUR: i32 = 22;
const SHADOW_DROP: i32 = 6;
/// Popups' and notifications' shadows: their corners' radius, the blur,
/// how far below, and their colour.
const POPUP_RADIUS: i32 = 12;
const POPUP_BLUR: i32 = 18;
const POPUP_DROP: i32 = 8;
const POPUP_SHADOW: Color = Color::rgba(0, 0, 0, 120);

/// Visual theme of the window system (dark).
pub mod theme {
    use gfx::Color;
    pub const DESKTOP_TOP: Color = Color::hex(0x0B1022);
    pub const DESKTOP_BOTTOM: Color = Color::hex(0x161C38);
    pub const TITLE_ACTIVE: Color = Color::hex(0x202026);
    pub const TITLE_INACTIVE: Color = Color::hex(0x1A1A1E);
    pub const TITLE_TEXT: Color = Color::hex(0xE8E8EE);
    pub const TITLE_TEXT_INACTIVE: Color = Color::hex(0x80808A);
    pub const ACCENT: Color = Color::hex(0x5B8CFF);
    pub const BORDER: Color = Color::rgba(255, 255, 255, 26);
    pub const BORDER_INACTIVE: Color = Color::rgba(255, 255, 255, 16);
    pub const BUTTON_HOVER: Color = Color::rgba(255, 255, 255, 24);
    pub const BUTTON_PRESSED: Color = Color::rgba(255, 255, 255, 14);
    pub const CLOSE_HOVER: Color = Color::hex(0xC42B1C);
    pub const GLYPH: Color = Color::hex(0xD4D4DA);
    pub const SHADOW: Color = Color::rgba(0, 0, 0, 150);
    pub const SHADOW_INACTIVE: Color = Color::rgba(0, 0, 0, 90);
    /// Placeholder fill while a client has not drawn yet.
    pub const CLIENT_BACKGROUND: Color = Color::hex(0x1C1C21);
}

/// How a window is drawn besides its pixels: whether it has the focus,
/// the caption buttons hovered or pressed, whether its client does not
/// answer, and where it is in an animation (its opacity, and how far below
/// its place it is).
#[derive(Clone, Copy)]
pub struct Look {
    pub focused: bool,
    pub decor: DecorState,
    pub unresponsive: bool,
    pub opacity: f32,
    pub dy: i32,
}

/// A subsurface as a window's frame places it, for the processor: where
/// in the window's client area, what shows where its buffer does not reach
/// (`0xFF_RRGGBB`), and the buffer it shows, if any (none presented yet, or
/// the subsurface gone).
pub struct Layer<'a> {
    pub rect: Rect,
    pub background: u32,
    pub pixels: Option<Pixels<'a>>,
}

/// A subsurface as a window's frame places it, for the GPU: as [`Layer`],
/// with the texture its buffer is in.
pub struct LayerTexture {
    pub rect: Rect,
    pub background: u32,
    pub content: Option<Texture>,
}

/// A layer as it is drawn on the screen: its place (cut to the client
/// area), where its buffer's first pixel is, its background and buffer.
struct OnScreen<'l, 'a> {
    place: Rect,
    origin: (i32, i32),
    background: u32,
    pixels: Option<&'l Pixels<'a>>,
}

/// What windows are drawn with: shadows, the title font, cursors.
pub struct Decor {
    shadow: ShadowTemplate,
    popup_shadow: ShadowTemplate,
    pub text: Text,
    pub title_font: usize,
    cursors: Vec<(Cursor, Bitmap, i32, i32)>,
    /// A row of a client's pixels, copied out to be blended.
    row: Vec<u32>,
    /// A part of a row of a window's pixels, to be blended over its
    /// subsurfaces.
    over: Vec<u32>,
}

/// The title a window's title bar shows.
fn title_text(w: &Window, unresponsive: bool) -> String {
    if unresponsive { format!("{} (not responding)", w.title) } else { w.title.clone() }
}

impl Decor {
    /// Decorations with titles in `title_font` of `text`.
    pub fn new(text: Text, title_font: usize) -> Decor {
        Decor {
            shadow: ShadowTemplate::new(CORNER_RADIUS, SHADOW_BLUR),
            popup_shadow: ShadowTemplate::new(POPUP_RADIUS, POPUP_BLUR),
            text,
            title_font,
            cursors: build_cursors(),
            row: Vec::new(),
            over: Vec::new(),
        }
    }

    /// Cursor `c`'s picture and its hot spot.
    pub fn cursor(&self, c: Cursor) -> Option<(&Bitmap, i32, i32)> {
        let c = if c == Cursor::Busy { Cursor::Arrow } else { c };
        self.cursors.iter().find(|(k, ..)| *k == c).or_else(|| self.cursors.first()).map(|(_, b, hx, hy)| (b, *hx, *hy))
    }

    /// Draws a complete window (shadow, decorations, client content, its
    /// subsurfaces `layers`).
    pub fn draw_window(&mut self, c: &mut Canvas, w: &Window, look: Look, layers: &[Layer]) {
        let opacity = look.opacity;
        let op = (opacity.clamp(0.0, 1.0) * 255.0) as u32;
        if op == 0 {
            return;
        }
        let frame = w.frame().translate(0, look.dy);
        let client = w.client_rect.translate(0, look.dy);
        match w.kind {
            WindowKind::Normal if w.decorated() => {
                let maximized = w.state == WindowState::Maximized;
                let radius = if maximized { 0 } else { CORNER_RADIUS };
                if !maximized {
                    let sc = if look.focused { theme::SHADOW } else { theme::SHADOW_INACTIVE };
                    self.shadow.draw(c, frame.translate(0, SHADOW_DROP), sc.fade(opacity), op < 255);
                }
                let title = Rect::new(frame.x, frame.y, frame.w, TITLE_HEIGHT);
                self.draw_title_bar(c, w, title, look);
                self.draw_client(c, w, client, op, radius, layers);
                if !maximized {
                    let bc = if look.focused { theme::BORDER } else { theme::BORDER_INACTIVE };
                    c.stroke_rounded_rect(frame, radius as f32, 1.0, bc.fade(opacity));
                }
            }
            WindowKind::Popup | WindowKind::Notification => {
                self.popup_shadow.draw(c, frame.translate(0, POPUP_DROP), POPUP_SHADOW.fade(opacity), true);
                self.draw_client(c, w, client, op, 0, layers);
            }
            _ => self.draw_client(c, w, client, op, 0, layers),
        }
    }

    /// A decorated window's title bar in `title`: its background, rounded
    /// at the top unless the window is maximised, its title and buttons.
    fn draw_title_bar(&mut self, c: &mut Canvas, w: &Window, title: Rect, look: Look) {
        let radius = if w.state == WindowState::Maximized { 0 } else { CORNER_RADIUS };
        let tc = if look.focused { theme::TITLE_ACTIVE } else { theme::TITLE_INACTIVE };
        c.save();
        c.clip_to(title);
        c.fill_rounded_rect(
            Rect::new(title.x, title.y, title.w, TITLE_HEIGHT + radius * 2),
            radius as f32,
            tc.fade(look.opacity),
        );
        c.restore();
        self.draw_title_content(c, w, title, look);
    }

    /// A decorated window's title bar alone, as [`Decor::draw_window`]
    /// draws it (for the GPU's texture); `None` if there is not the memory
    /// for it.
    fn title_bitmap(&mut self, w: &Window, look: Look) -> Option<Bitmap> {
        let f = w.frame();
        let mut b = pixels::try_bitmap(f.w.max(1), TITLE_HEIGHT)?;
        let mut c = Canvas::for_bitmap(&mut b);
        c.translate(-f.x, -f.y);
        self.draw_title_bar(&mut c, w, Rect::new(f.x, f.y, f.w, TITLE_HEIGHT), Look { opacity: 1.0, dy: 0, ..look });
        drop(c);
        Some(b)
    }

    /// Draws a complete window with the GPU, as [`Decor::draw_window`]
    /// does: its shadow and border by shaders, its title bar from a texture
    /// drawn here (again when it changes), its client's pixels from
    /// `content`, its subsurfaces from `layers`.
    pub fn draw_window_gpu(
        &mut self,
        g: &mut Gpu,
        w: &Window,
        look: Look,
        content: Option<Texture>,
        layers: &[LayerTexture],
    ) {
        let opacity = look.opacity.clamp(0.0, 1.0);
        if (opacity * 255.0) as u32 == 0 {
            return;
        }
        let frame = w.frame().translate(0, look.dy);
        let client = w.client_rect.translate(0, look.dy);
        match w.kind {
            WindowKind::Normal if w.decorated() => {
                let maximized = w.state == WindowState::Maximized;
                let radius = if maximized { 0 } else { CORNER_RADIUS };
                if !maximized {
                    let sc = if look.focused { theme::SHADOW } else { theme::SHADOW_INACTIVE };
                    let r = frame.translate(0, SHADOW_DROP);
                    let hollow = (opacity * 255.0) as u32 >= 255;
                    g.shadow(r, CORNER_RADIUS as f32, SHADOW_BLUR, rgba(sc.fade(opacity).premul()), hollow);
                }
                let key = title_key(w, look);
                let t = match g.cached(Slot::Title(w.id), key) {
                    Some(t) => Some(t),
                    None => self.title_bitmap(w, look).map(|b| g.upload(Slot::Title(w.id), key, &b)),
                };
                let title = Rect::new(frame.x, frame.y, frame.w, TITLE_HEIGHT);
                match t {
                    Some(t) => g.image(t, Rect::new(0, 0, title.w, title.h), title, opacity, false, None),
                    None => {
                        let tc = if look.focused { theme::TITLE_ACTIVE } else { theme::TITLE_INACTIVE };
                        g.fill(title, 0.0, rgba(tc.fade(opacity).premul()));
                    }
                }
                draw_client_gpu(g, w, client, opacity, radius, content, layers);
                if !maximized {
                    let bc = if look.focused { theme::BORDER } else { theme::BORDER_INACTIVE };
                    g.stroke(frame, radius as f32, 1.0, rgba(bc.fade(opacity).premul()));
                }
            }
            WindowKind::Popup | WindowKind::Notification => {
                let r = frame.translate(0, POPUP_DROP);
                g.shadow(r, POPUP_RADIUS as f32, POPUP_BLUR, rgba(POPUP_SHADOW.fade(opacity).premul()), false);
                draw_client_gpu(g, w, client, opacity, 0, content, layers);
            }
            _ => draw_client_gpu(g, w, client, opacity, 0, content, layers),
        }
    }

    /// The switcher alone, as [`Decor::draw_switcher`] draws it: a bitmap
    /// of `s.bounds()` (for the GPU's texture); `None` if there is not the
    /// memory for it.
    pub fn switcher_bitmap(&mut self, s: &Switcher, titles: &[&str], thumbs: &[Option<Bitmap>]) -> Option<Bitmap> {
        let b = s.bounds();
        let mut bitmap = pixels::try_bitmap(b.w.max(1), b.h.max(1))?;
        let mut c = Canvas::for_bitmap(&mut bitmap);
        c.translate(-b.x, -b.y);
        self.draw_switcher(&mut c, s, titles, thumbs);
        drop(c);
        Some(bitmap)
    }

    /// Draws the Alt+Tab switcher overlay; `titles` and `thumbs` are the
    /// windows' titles and thumbnails in the switcher's order.
    pub fn draw_switcher(&mut self, c: &mut Canvas, s: &Switcher, titles: &[&str], thumbs: &[Option<Bitmap>]) {
        let panel = s.rect;
        self.popup_shadow.draw(c, panel.translate(0, 10), Color::rgba(0, 0, 0, 150), true);
        c.fill_rounded_rect(panel, 16.0, Color::rgba(30, 30, 38, 242));
        c.stroke_rounded_rect(panel, 16.0, 1.0, Color::rgba(255, 255, 255, 34));
        for (i, title) in titles.iter().enumerate() {
            let cell = s.cell(i);
            let selected = i == s.selected;
            if selected {
                c.fill_rounded_rect(cell, 12.0, Color::rgba(255, 255, 255, 20));
                c.stroke_rounded_rect(cell, 12.0, 2.0, theme::ACCENT);
            }
            let bx = s.thumb_box(i);
            match thumbs.get(i).and_then(|t| t.as_ref()) {
                Some(b) => {
                    let r = Rect::new(bx.x + (bx.w - b.width) / 2, bx.y + (bx.h - b.height) / 2, b.width, b.height);
                    c.draw_shadow(r.translate(0, 3), 6, 10, Color::rgba(0, 0, 0, 110));
                    c.draw_bitmap(b, r.x, r.y, 255);
                    c.stroke_rounded_rect(r.inflate(1), 2.0, 1.0, Color::rgba(255, 255, 255, 30));
                }
                None => c.fill_rounded_rect(bx.inset(24, 12, 24, 12), 6.0, theme::CLIENT_BACKGROUND),
            }
            let font = self.title_font;
            let label = self.text.ellipsize(font, 13.0, title, (cell.w - 24) as f32);
            let color = if selected { theme::TITLE_TEXT } else { theme::TITLE_TEXT_INACTIVE };
            let tr = Rect::new(cell.x + 12, bx.bottom() + 10, cell.w - 24, 24);
            self.text.draw_in(c, font, 13.0, tr, &label, color, gfx::Align::Center);
        }
    }

    fn draw_title_content(&mut self, c: &mut Canvas, w: &Window, title: Rect, look: Look) {
        let opacity = look.opacity;
        let text_color = if look.focused { theme::TITLE_TEXT } else { theme::TITLE_TEXT_INACTIVE }.fade(opacity);
        let buttons = if w.resizable { 3 } else { 2 };
        let text_rect = Rect::new(title.x + 16, title.y, title.w - 16 - buttons * BUTTON_WIDTH - 8, TITLE_HEIGHT);
        let font = self.title_font;
        let text = title_text(w, look.unresponsive);
        self.text.draw_in(c, font, 13.0, text_rect, &text, text_color, gfx::Align::Left);
        let dy = title.y - w.title_rect().y;
        for part in [Part::Minimize, Part::Maximize, Part::Close] {
            if part == Part::Maximize && !w.resizable {
                continue;
            }
            let r = w.button_rect(part).translate(0, dy);
            let hovered = look.decor.hover == Some((w.id, part));
            let pressed = look.decor.pressed == Some((w.id, part));
            if hovered || pressed {
                let bg = match (part, pressed) {
                    (Part::Close, _) => theme::CLOSE_HOVER,
                    (_, true) => theme::BUTTON_PRESSED,
                    _ => theme::BUTTON_HOVER,
                };
                // Respect the window's rounded top-right corner.
                let radius = if part == Part::Close && w.state != proto::display::WindowState::Maximized {
                    CORNER_RADIUS
                } else {
                    0
                };
                c.save();
                c.clip_to(r);
                if radius > 0 {
                    c.fill_rounded_rect(
                        Rect::new(r.x - radius, r.y, r.w + radius, r.h + radius),
                        radius as f32,
                        bg.fade(opacity),
                    );
                } else {
                    c.fill_rect(r, bg.fade(opacity));
                }
                c.restore();
            }
            let glyph = if part == Part::Close && hovered { Color::WHITE } else { theme::GLYPH }.fade(opacity);
            draw_button_glyph(c, part, r, glyph, w.state == proto::display::WindowState::Maximized);
        }
    }

    /// Copies the client buffer into `dst`, with its subsurfaces `layers`,
    /// rounding the bottom corners by `radius` and applying `opacity`
    /// (0..=255).
    fn draw_client(&mut self, c: &mut Canvas, w: &Window, dst: Rect, opacity: u32, radius: i32, layers: &[Layer]) {
        let Some(src) = Pixels::shown(&w.surface) else {
            if w.decorated() {
                c.fill_rect(dst, theme::CLIENT_BACKGROUND);
            }
            return;
        };
        let (bw, bh) = (src.width(), src.height());
        let clip = c.clip_rect();
        let vis = dst.intersect(&clip).intersect(&Rect::new(dst.x, dst.y, bw, bh));
        // Areas of the client rect not covered by the buffer (mid-resize).
        if w.decorated() && (bw < dst.w || bh < dst.h) {
            c.fill_rect(Rect::new(dst.x + bw, dst.y, dst.w - bw, dst.h), theme::CLIENT_BACKGROUND);
            c.fill_rect(Rect::new(dst.x, dst.y + bh, bw.min(dst.w), dst.h - bh), theme::CLIENT_BACKGROUND);
        }
        if vis.is_empty() {
            return;
        }
        let n = vis.w as usize;
        if self.row.len() < n {
            self.row.resize(n, 0);
        }
        let b = c.bounds();
        let (pixels, stride) = c.pixels_mut();
        let corner_top = dst.bottom() - radius;
        // Opaque windows (not fading) are copied row by row, except for the
        // anti-aliased rounded corners. Their pixels' alpha is ignored,
        // fading or not, as the GPU's shader ignores it.
        let opaque = w.opaque();
        let copy = opaque && opacity == 255;
        let alpha = if opaque { 0xFF00_0000 } else { 0 };
        // Its subsurfaces and overlays on the screen.
        let placed: Vec<OnScreen> = layers
            .iter()
            .map(|l| {
                let at = l.rect.translate(dst.x, dst.y);
                OnScreen {
                    place: at.intersect(&vis),
                    origin: (at.x, at.y),
                    background: l.background,
                    pixels: l.pixels.as_ref(),
                }
            })
            .filter(|l| !l.place.is_empty())
            .collect();
        let overlays: Vec<Rect> = if placed.is_empty() {
            Vec::new()
        } else {
            w.placements
                .overlays
                .iter()
                .map(|o| o.translate(dst.x, dst.y).intersect(&vis))
                .filter(|o| !o.is_empty())
                .collect()
        };
        let client = Client { src: &src, dst, opaque, layers: &placed, overlays: &overlays };
        for y in vis.y..vis.bottom() {
            let to = ((y - b.y) * stride + vis.x - b.x) as usize;
            let out = &mut pixels[to..to + n];
            let in_corner_rows = radius > 0 && y >= corner_top;
            let layered = placed.iter().any(|l| l.place.y <= y && y < l.place.bottom());
            if copy && !in_corner_rows {
                if layered {
                    client.row(out, (vis.x, y), &mut self.over);
                } else if !src.copy_row(vis.x - dst.x, y - dst.y, out) {
                    out.fill(theme::CLIENT_BACKGROUND.premul());
                }
                continue;
            }
            let line = &mut self.row[..n];
            if layered {
                client.row(line, (vis.x, y), &mut self.over);
            } else if !src.copy_row(vis.x - dst.x, y - dst.y, line) {
                continue;
            }
            if in_corner_rows {
                blend_corner_row(out, line, alpha, opacity, (vis.x, y), (dst, radius));
            } else {
                blend_row(out, line, alpha, opacity);
            }
        }
    }
}

/// A window's client area as it is drawn: its buffer `src` at `dst` on the
/// screen, its subsurfaces `layers` and its `overlays` (on the screen, cut
/// to what is drawn).
struct Client<'c, 'l, 'a> {
    src: &'c Pixels<'c>,
    dst: Rect,
    opaque: bool,
    layers: &'c [OnScreen<'l, 'a>],
    overlays: &'c [Rect],
}

impl Client<'_, '_, '_> {
    /// Row `y` from `x0` on, as the window's frame shows it: its buffer,
    /// its subsurfaces where it places them (their pixels, or their
    /// backgrounds where they do not reach), and its buffer again over them
    /// within its overlays, blended: into `line`. `scratch` holds the
    /// window's pixels on their way to be blended.
    fn row(&self, line: &mut [u32], (x0, y): (i32, i32), scratch: &mut Vec<u32>) {
        let x1 = x0 + line.len() as i32;
        let on_row = |r: &Rect| r.y <= y && y < r.bottom();
        let part = |r: &Rect| (r.x.max(x0), r.right().min(x1));
        // The window's own pixels where no subsurface is: between them
        // (placements do not overlap).
        let mut spans: [(i32, i32); dp::PLACEMENTS_MAX] = [(0, 0); dp::PLACEMENTS_MAX];
        let mut n = 0;
        for l in self.layers.iter().filter(|l| on_row(&l.place)).take(spans.len()) {
            spans[n] = part(&l.place);
            n += 1;
        }
        let spans = &mut spans[..n];
        spans.sort_unstable();
        let mut x = x0;
        for &(a, b) in spans.iter().chain(core::iter::once(&(x1, x1))) {
            if a > x {
                let own = &mut line[(x - x0) as usize..(a - x0) as usize];
                if !self.src.copy_row(x - self.dst.x, y - self.dst.y, own) {
                    own.fill(theme::CLIENT_BACKGROUND.premul());
                }
            }
            x = x.max(b);
        }
        // The subsurfaces.
        for l in self.layers.iter().filter(|l| on_row(&l.place)) {
            let (a, b) = part(&l.place);
            if a >= b {
                continue;
            }
            let (bx, by) = (a - l.origin.0, y - l.origin.1);
            let reach = match l.pixels {
                Some(p) if by < p.height() => (p.width() - bx).clamp(0, b - a),
                _ => 0,
            };
            let (shown, rest) = line[(a - x0) as usize..(b - x0) as usize].split_at_mut(reach as usize);
            if !shown.is_empty() && !l.pixels.is_some_and(|p| p.copy_row(bx, by, shown)) {
                shown.fill(l.background);
            } else if !self.opaque {
                // Opaque, in whatever window: an opaque one's pixels' alpha
                // is ignored already.
                for p in shown.iter_mut() {
                    *p |= 0xFF00_0000;
                }
            }
            rest.fill(l.background);
        }
        // The window over them, in its overlays (which do not overlap).
        for o in self.overlays.iter().filter(|o| on_row(o)) {
            for l in self.layers.iter().filter(|l| on_row(&l.place)) {
                let (a, b) = part(&o.intersect(&l.place));
                if a >= b {
                    continue;
                }
                let k = (b - a) as usize;
                if scratch.len() < k {
                    scratch.resize(k, 0);
                }
                if self.src.copy_row(a - self.dst.x, y - self.dst.y, &mut scratch[..k]) {
                    blend_row(&mut line[(a - x0) as usize..(b - x0) as usize], &scratch[..k], 0, 255);
                }
            }
        }
    }
}

/// Pixel (`x`, `y`) of a window's client area as its frame shows it, from
/// its buffer `src` and its subsurfaces `layers` (see [`Client::row`]).
pub fn composite_pixel(src: &Pixels, layers: &[Layer], overlays: &[Rect], x: i32, y: i32) -> u32 {
    let own = src.get(x, y);
    let Some(l) = layers.iter().find(|l| l.rect.contains(x, y)) else { return own };
    let (bx, by) = (x - l.rect.x, y - l.rect.y);
    let under = match &l.pixels {
        Some(p) if bx < p.width() && by < p.height() => p.get(bx, by) | 0xFF00_0000,
        _ => l.background,
    };
    if overlays.iter().any(|o| o.contains(x, y)) { over(own, under) } else { under }
}

/// Blends `src` over `dst`, each pixel's alpha raised by `alpha` (an opaque
/// window's pixels are opaque) and the whole at `opacity` (of 255).
///
/// Kept scalar: vectorised, its multiplies become SSE instructions that
/// QEMU's TCG emulates with slow helper calls (`core::hint::black_box`
/// keeps LLVM from doing so; see CODING.md's Performance).
fn blend_row(dst: &mut [u32], src: &[u32], alpha: u32, opacity: u32) {
    for (d, &s) in dst.iter_mut().zip(src) {
        let px = core::hint::black_box(s | alpha);
        let px = if opacity < 255 { scale(px, opacity) } else { px };
        *d = if px >> 24 == 255 { px } else { over(px, *d) };
    }
}

/// [`blend_row`] for a row through a window's rounded bottom corners: the
/// row of pixels from (`x`, `y`) on, the window's client area `dst`
/// rounded by `radius`.
fn blend_corner_row(
    dst: &mut [u32],
    src: &[u32],
    alpha: u32,
    opacity: u32,
    (x, y): (i32, i32),
    (rect, radius): (Rect, i32),
) {
    let rad = radius as f32;
    let fy = y as f32 + 0.5 - (rect.bottom() as f32 - rad);
    for (i, (d, &s)) in dst.iter_mut().zip(src).enumerate() {
        let px_x = x + i as i32;
        let mut cov = opacity;
        let cx = if px_x < rect.x + radius {
            Some(rect.x as f32 + rad)
        } else if px_x >= rect.right() - radius {
            Some(rect.right() as f32 - rad)
        } else {
            None
        };
        if let Some(cx) = cx {
            let fx = px_x as f32 + 0.5 - cx;
            let dist = (fx * fx + fy * fy).sqrt();
            cov = (cov as f32 * (rad - dist + 0.5).clamp(0.0, 1.0)) as u32;
        }
        if cov == 0 {
            continue;
        }
        let px = core::hint::black_box(s | alpha);
        let px = if cov < 255 { scale(px, cov) } else { px };
        *d = if px >> 24 == 255 { px } else { over(px, *d) };
    }
}

fn draw_button_glyph(c: &mut Canvas, part: Part, r: Rect, color: Color, maximized: bool) {
    let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
    let s = 5.0;
    let stroke = StrokeStyle::new(1.0);
    let mut p = Path::new();
    match part {
        Part::Minimize => {
            p.move_to(cx - s, cy + 0.5);
            p.line_to(cx + s, cy + 0.5);
        }
        Part::Maximize if maximized => {
            p.rounded_rect(cx - s + 0.5, cy - s + 2.5, 2.0 * s - 2.0, 2.0 * s - 2.0, [1.0; 4]);
            p.move_to(cx - s + 2.5, cy - s + 0.5);
            p.line_to(cx + s + 0.5, cy - s + 0.5);
            p.line_to(cx + s + 0.5, cy + s - 2.5);
        }
        Part::Maximize => p.rounded_rect(cx - s + 0.5, cy - s + 0.5, 2.0 * s, 2.0 * s, [1.5; 4]),
        Part::Close => {
            p.move_to(cx - s, cy - s);
            p.line_to(cx + s, cy + s);
            p.move_to(cx + s, cy - s);
            p.line_to(cx - s, cy + s);
        }
        _ => return,
    }
    c.stroke_path(&p, &stroke, color);
}

/// Draws a window's client area with the GPU, as `Decor::draw_client`
/// does: its pixels from `content`, its subsurfaces from `layers`, the
/// bottom corners rounded by `radius`, at `opacity`; where the client has
/// not drawn (yet, or since it was made larger), the placeholder.
fn draw_client_gpu(
    g: &mut Gpu,
    w: &Window,
    dst: Rect,
    opacity: f32,
    radius: i32,
    content: Option<Texture>,
    layers: &[LayerTexture],
) {
    let background = rgba(theme::CLIENT_BACKGROUND.premul());
    let (Some((bw, bh)), Some(t)) = (w.surface.size(), content) else {
        if w.decorated() {
            g.fill(dst, 0.0, background);
        }
        return;
    };
    if w.decorated() && (bw < dst.w || bh < dst.h) {
        g.fill(Rect::new(dst.x + bw, dst.y, dst.w - bw, dst.h), 0.0, background);
        g.fill(Rect::new(dst.x, dst.y + bh, bw.min(dst.w), dst.h - bh), 0.0, background);
    }
    let vis = dst.intersect(&Rect::new(dst.x, dst.y, bw, bh));
    let round = (radius > 0).then_some((dst, radius as f32));
    // Its own pixels where no subsurface is: under one they would show
    // through it as the window fades.
    let places: Vec<Rect> = layers.iter().map(|l| l.rect.translate(dst.x, dst.y).intersect(&vis)).collect();
    for part in vis.without(&places) {
        g.image(t, Rect::new(part.x - dst.x, part.y - dst.y, part.w, part.h), part, opacity, w.opaque(), round);
    }
    // Its subsurfaces: their buffers where they reach, their backgrounds
    // where they do not.
    for (l, &place) in layers.iter().zip(&places) {
        if place.is_empty() {
            continue;
        }
        let at = l.rect.translate(dst.x, dst.y);
        let reach = l.content.map_or(Rect::default(), |s| Rect::new(at.x, at.y, s.w, s.h).intersect(&place));
        if let Some(s) = l.content
            && !reach.is_empty()
        {
            g.image(s, Rect::new(reach.x - at.x, reach.y - at.y, reach.w, reach.h), reach, opacity, true, round);
        }
        let background = rgba(l.background).map(|c| c * opacity.min(1.0));
        if reach.is_empty() {
            g.fill(place, 0.0, background);
        } else {
            g.fill(Rect::new(reach.right(), place.y, place.right() - reach.right(), place.h), 0.0, background);
            g.fill(Rect::new(place.x, reach.bottom(), reach.w, place.bottom() - reach.bottom()), 0.0, background);
        }
    }
    // The window over them, in its overlays: blended, with its alpha. (As
    // the window fades, a little of a subsurface shows through what is
    // drawn over it there, which the processor's composition, of the two
    // first, does not show.)
    for o in &w.placements.overlays {
        let o = o.translate(dst.x, dst.y);
        for place in &places {
            let r = o.intersect(place);
            if !r.is_empty() {
                g.image(t, Rect::new(r.x - dst.x, r.y - dst.y, r.w, r.h), r, opacity, false, round);
            }
        }
    }
}

/// What a decorated window's title bar is drawn from: its texture is
/// drawn again when this changes (never 0).
fn title_key(w: &Window, look: Look) -> u64 {
    let (focused, st) = (look.focused, look.decor);
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    let mut eat = |v: u64| h = (h ^ v).wrapping_mul(0x0000_0100_0000_01B3);
    for b in w.title.bytes() {
        eat(u64::from(b));
    }
    let part = |p: Option<(u32, Part)>| match p.filter(|(id, _)| *id == w.id).map(|(_, p)| p) {
        None => 0,
        Some(Part::Client) => 1,
        Some(Part::Title) => 2,
        Some(Part::Minimize) => 3,
        Some(Part::Maximize) => 4,
        Some(Part::Close) => 5,
        Some(Part::Edge(..)) => 6,
    };
    eat(w.frame().w as u64);
    let maximized = w.state == WindowState::Maximized;
    eat(u64::from(focused)
        | u64::from(w.resizable) << 1
        | u64::from(maximized) << 2
        | u64::from(look.unresponsive) << 3);
    eat(part(st.hover) | part(st.pressed) << 8);
    h | 1
}

/// The translucent rectangle showing where a dragged window will snap.
pub fn draw_snap_preview(c: &mut Canvas, r: Rect) {
    let r = r.inset(8, 8, 8, 8);
    c.fill_rounded_rect(r, 12.0, Color::rgba(91, 140, 255, 46));
    c.stroke_rounded_rect(r, 12.0, 2.0, Color::rgba(150, 185, 255, 170));
}

/// [`draw_snap_preview`] with the GPU.
pub fn draw_snap_preview_gpu(g: &mut Gpu, r: Rect) {
    let r = r.inset(8, 8, 8, 8);
    g.fill(r, 12.0, rgba(Color::rgba(91, 140, 255, 46).premul()));
    g.stroke(r, 12.0, 2.0, rgba(Color::rgba(150, 185, 255, 170).premul()));
}

/// Renders a cursor shape: white fill with a dark outline and a soft shadow.
fn render_cursor(path: &Path, w: i32, h: i32) -> Bitmap {
    let mut shape = Bitmap::new(w, h);
    {
        let mut c = Canvas::for_bitmap(&mut shape);
        c.fill_path(path, Color::BLACK, FillRule::NonZero);
    }
    let mut out = gfx::shadow::silhouette_shadow(&shape, 3);
    // The shadow bitmap is padded by 6 px; shift it down-right by 1 px.
    let pad = 6;
    let mut final_bm = Bitmap::new(w + pad, h + pad);
    {
        let mut c = Canvas::for_bitmap(&mut final_bm);
        for p in out.pixels.iter_mut() {
            *p = scale(*p, 120);
        }
        c.draw_bitmap(&out, -pad + 1, -pad + 2, 255);
        let t = gfx::Transform::IDENTITY;
        c.stroke_path_transformed(
            path,
            &StrokeStyle::new(2.0).with_join(gfx::LineJoin::Round),
            &t,
            Color::rgba(20, 20, 26, 255),
        );
        c.fill_path(path, Color::WHITE, FillRule::NonZero);
    }
    final_bm
}

fn build_cursors() -> Vec<(Cursor, Bitmap, i32, i32)> {
    let mut v = Vec::new();
    // Arrow (hotspot at the tip).
    let mut arrow = Path::new();
    arrow.move_to(2.0, 2.0);
    arrow.line_to(2.0, 19.0);
    arrow.line_to(6.2, 15.0);
    arrow.line_to(9.0, 21.0);
    arrow.line_to(11.6, 19.9);
    arrow.line_to(8.9, 14.1);
    arrow.line_to(14.6, 14.1);
    arrow.close();
    v.push((Cursor::Arrow, render_cursor(&arrow, 20, 26), 2, 2));
    // Text I-beam.
    let mut beam = Path::new();
    beam.rect(4.0, 3.0, 6.0, 1.6);
    beam.rect(6.2, 3.0, 1.6, 17.0);
    beam.rect(4.0, 18.4, 6.0, 1.6);
    v.push((Cursor::Text, render_cursor(&beam, 16, 24), 7, 11));
    // Pointing hand.
    let mut hand = Path::new();
    hand.rounded_rect(6.0, 2.0, 4.0, 12.0, [2.0; 4]);
    hand.rounded_rect(3.0, 10.0, 14.0, 10.0, [4.0, 4.0, 5.0, 5.0]);
    hand.rounded_rect(10.0, 7.5, 3.6, 6.0, [1.8; 4]);
    hand.rounded_rect(13.4, 8.5, 3.6, 5.0, [1.8; 4]);
    hand.rounded_rect(2.2, 11.5, 3.6, 6.0, [1.8; 4]);
    v.push((Cursor::Hand, render_cursor(&hand, 22, 26), 8, 2));
    // Resize arrows.
    let double_arrow = |horizontal: bool| -> Path {
        let mut p = Path::new();
        let pts: [(f32, f32); 10] = [
            (0.0, 6.0),
            (5.0, 1.0),
            (5.0, 4.5),
            (13.0, 4.5),
            (13.0, 1.0),
            (18.0, 6.0),
            (13.0, 11.0),
            (13.0, 7.5),
            (5.0, 7.5),
            (5.0, 11.0),
        ];
        for (i, &(x, y)) in pts.iter().enumerate() {
            let (x, y) = if horizontal { (x + 2.0, y + 5.0) } else { (y + 5.0, x + 2.0) };
            if i == 0 { p.move_to(x, y) } else { p.line_to(x, y) }
        }
        p.close();
        p
    };
    v.push((Cursor::ResizeHorizontal, render_cursor(&double_arrow(true), 24, 18), 11, 11));
    v.push((Cursor::ResizeVertical, render_cursor(&double_arrow(false), 18, 24), 11, 11));
    let diag = |anti: bool| -> Path {
        let mut p = double_arrow(true);
        let angle = if anti { -core::f32::consts::FRAC_PI_4 } else { core::f32::consts::FRAC_PI_4 };
        p.transform(&gfx::Transform::rotate_about(angle, 11.0, 11.0).then_translate(1.0, 1.0));
        p
    };
    v.push((Cursor::ResizeDiagonal, render_cursor(&diag(false), 24, 24), 12, 12));
    v.push((Cursor::ResizeAntiDiagonal, render_cursor(&diag(true), 24, 24), 12, 12));
    // Move: four-way.
    let mut mv = double_arrow(true);
    mv.append(&double_arrow(false));
    v.push((Cursor::Move, render_cursor(&mv, 24, 24), 11, 11));
    let mut cross = Path::new();
    cross.rect(10.0, 2.0, 2.0, 18.0);
    cross.rect(2.0, 10.0, 18.0, 2.0);
    v.push((Cursor::Crosshair, render_cursor(&cross, 24, 24), 11, 11));
    v.push((Cursor::Hidden, Bitmap::new(1, 1), 0, 0));
    v
}

/// Background shown where no desktop window covers the screen.
pub fn draw_background(c: &mut Canvas, screen: Rect) {
    c.fill_vertical_gradient(screen, theme::DESKTOP_TOP, theme::DESKTOP_BOTTOM);
}

/// [`draw_background`] with the GPU.
pub fn draw_background_gpu(g: &mut Gpu, screen: Rect) {
    g.gradient(screen, rgba(theme::DESKTOP_TOP.premul()), rgba(theme::DESKTOP_BOTTOM.premul()));
}

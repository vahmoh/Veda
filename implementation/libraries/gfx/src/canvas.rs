// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! [`Canvas`]: drawing operations on a borrowed pixel buffer.
//!
//! All coordinates are relative to the current origin ([`Canvas::translate`])
//! and clipped to the current clip rectangle ([`Canvas::clip_to`]); `save` /
//! `restore` bracket temporary changes. Drawing costs what lies inside the
//! clip: paths are rasterized against the clip rectangle alone, so that
//! redrawing a small part of a window pays for that part.
//!
//! What drawing keeps from one call to the next (the rasterizer's buffers,
//! the templates of shadows) is a [`DrawCache`]. A canvas lent one
//! ([`Canvas::with_cache`]) uses it, so that it lasts from frame to frame;
//! any other canvas makes its own when it first needs it.
//!
//! A canvas can note where drawing touches an area ([`Canvas::track`]): a
//! window keeps a picture there from frame to frame, and puts it back
//! ([`Canvas::put_back`]) where something was drawn over it.
//!
//! The loops over pixels stay scalar (see [`crate::color`]).

use alloc::boxed::Box;
use alloc::vec::Vec;

// Float math under `no_std` (std, linked into host tests, has these inherently).
#[allow(unused_imports)]
use num_traits::Float;
use raster::{Coverage, FillRule, Path, Rasterizer, Span, StrokeStyle, Transform};

use crate::bitmap::Bitmap;
use crate::color::{Color, blend_run, keep_scalar, lerp_px, over, scale};
use crate::geom::{Damage, Rect};
use crate::shadow::ShadowTemplate;

/// Image sampling filter for scaled drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Nearest,
    Bilinear,
}

/// How many shadow templates a [`DrawCache`] keeps.
const SHADOWS_KEPT: usize = 8;
/// The size of the largest shadow template kept: its corner is at most this
/// many pixels square (64 KiB).
const LARGEST_SHADOW_KEPT: i32 = 256;

/// What drawing keeps from one call to the next: the rasterizer, whose
/// buffers grow to what paths need, and the templates of the shadows drawn
/// last (making one computes its corner pixel by pixel).
///
/// Lend one to every frame's canvas ([`Canvas::with_cache`]) and it is made
/// once.
#[derive(Default)]
pub struct DrawCache {
    raster: Rasterizer,
    /// The templates of recent shadows, the most recent first.
    shadows: Vec<ShadowTemplate>,
}

impl DrawCache {
    /// An empty cache: what drawing needs is made as it is first needed.
    pub fn new() -> DrawCache {
        DrawCache::default()
    }

    /// The rasterizer, for paths rasterized by hand (into a mask, say).
    pub fn rasterizer(&mut self) -> &mut Rasterizer {
        &mut self.raster
    }

    /// The template for shadows of `radius` and `blur`: taken out of the
    /// cache (to be put back with [`DrawCache::keep_shadow`]), or made.
    fn take_shadow(&mut self, radius: i32, blur: i32) -> ShadowTemplate {
        let (radius, blur) = (radius.max(0), blur.max(0));
        match self.shadows.iter().position(|t| t.radius() == radius && t.blur() == blur) {
            Some(i) => self.shadows.remove(i),
            None => ShadowTemplate::new(radius, blur),
        }
    }

    /// Keeps `template` as the most recent one, unless it is large.
    fn keep_shadow(&mut self, template: ShadowTemplate) {
        if template.radius() + 2 * template.blur() <= LARGEST_SHADOW_KEPT {
            self.shadows.truncate(SHADOWS_KEPT - 1);
            self.shadows.insert(0, template);
        }
    }
}

/// The cache a canvas draws with.
enum Cache<'a> {
    /// Its own, made when first needed.
    Own(Option<Box<DrawCache>>),
    /// One lent to it.
    Lent(&'a mut DrawCache),
}

impl Cache<'_> {
    fn get(&mut self) -> &mut DrawCache {
        match self {
            Cache::Own(cache) => cache.get_or_insert_with(Box::default),
            Cache::Lent(cache) => cache,
        }
    }
}

/// Premultiplied pixels to draw from: `width` x `height` of them, `stride`
/// from one row to the next (a [`Bitmap`]'s, or a window's buffer).
#[derive(Debug, Clone, Copy)]
pub struct Picture<'p> {
    pixels: &'p [u32],
    width: i32,
    height: i32,
    stride: usize,
    opaque: bool,
}

impl<'p> Picture<'p> {
    /// A picture of no pixels.
    pub const EMPTY: Picture<'static> = Picture { pixels: &[], width: 0, height: 0, stride: 0, opaque: false };

    /// The `width` x `height` pixels of `pixels`, rows `stride` apart:
    /// `None` if they do not fit in `pixels`.
    pub fn new(pixels: &'p [u32], width: i32, height: i32, stride: usize) -> Option<Picture<'p>> {
        let (w, h) = (usize::try_from(width).ok()?, usize::try_from(height).ok()?);
        let needed = match h {
            0 => 0,
            _ => stride.checked_mul(h - 1)?.checked_add(w)?,
        };
        (stride >= w && pixels.len() >= needed).then_some(Picture { pixels, width, height, stride, opaque: false })
    }

    /// The same picture, every pixel of which is opaque whatever its alpha
    /// byte says (a window's buffer, a photograph): its alpha is taken as
    /// 255 where it is blended, and its rows are copied as they are where
    /// nothing is.
    pub fn opaque(self) -> Picture<'p> {
        Picture { opaque: true, ..self }
    }

    /// The width in pixels.
    pub fn width(&self) -> i32 {
        self.width
    }

    /// The height in pixels.
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Row `y` (`0 <= y < height`).
    pub(crate) fn row(&self, y: i32) -> &'p [u32] {
        let start = y as usize * self.stride;
        &self.pixels[start..start + self.width as usize]
    }
}

pub struct Canvas<'a> {
    pixels: &'a mut [u32],
    width: i32,
    height: i32,
    stride: i32,
    /// Clip in device coordinates.
    clip: Rect,
    /// Origin in device coordinates.
    ox: i32,
    oy: i32,
    stack: Vec<(Rect, i32, i32)>,
    cache: Cache<'a>,
    /// The area whose drawing is noted, and what drawing touched of it
    /// ([`Canvas::track`]).
    tracked: Option<(Rect, Damage)>,
}

/// Composites `px` (premultiplied) over `dst` at coverage `cov` (0..=255).
#[inline(always)]
pub(crate) fn blend_cov(dst: &mut u32, px: u32, cov: u32) {
    if cov >= 255 {
        *dst = over(px, *dst);
    } else if cov > 0 {
        *dst = over(scale(px, cov), *dst);
    }
}

/// How much (0..=255) of a pixel whose centre is `dist` from the centre of a
/// disc of `radius` the disc covers: across a band one pixel wide at its
/// edge. Circles and the corners of rounded rectangles and pictures are
/// drawn with it.
#[inline]
fn disc_coverage(dist: f32, radius: f32) -> u32 {
    ((radius - dist + 0.5).clamp(0.0, 1.0) * 255.0) as u32
}

/// For output index `i` of `dst_len` scaled from `src_len` (both counted
/// from the destination's origin): the two source indices to blend and the
/// weight (0..=255) of the second, in 16.16 fixed point.
fn taps(i: i32, src_len: i32, dst_len: i32, bilinear: bool) -> (usize, usize, u32) {
    let step = ((src_len as i64) << 16) / dst_len as i64;
    let centre = i as i64 * step + step / 2;
    if !bilinear {
        let s = (centre >> 16).clamp(0, src_len as i64 - 1) as usize;
        return (s, s, 0);
    }
    let p = centre - (1 << 15);
    let s0 = p >> 16;
    let weight = if p < 0 { 0 } else { ((p & 0xFFFF) >> 8) as u32 };
    let clamp = |v: i64| v.clamp(0, src_len as i64 - 1) as usize;
    (clamp(s0), clamp(s0 + 1), weight)
}

/// Composites a span of the rasterizer, in the coordinates of the clip
/// rectangle `clip` it was rasterized against, with the premultiplied colour
/// `px`. Runs of one coverage are blended as runs.
fn blend_span(pixels: &mut [u32], stride: i32, clip: Rect, px: u32, span: Span<'_>) {
    let start = (clip.y + span.y as i32) as usize * stride as usize + (clip.x + span.x as i32) as usize;
    let Some(row) = pixels.get_mut(start..start + span.len as usize) else { return };
    match span.coverage {
        Coverage::Solid(255) => blend_run(row, px),
        Coverage::Solid(cov) => blend_run(row, scale(px, cov as u32)),
        Coverage::Mask(mask) => {
            for (d, &cov) in row.iter_mut().zip(mask) {
                blend_cov(d, px, cov as u32);
                keep_scalar();
            }
        }
    }
}

/// The rows and columns a path's spans cover, gathered while the canvas
/// tracks drawing ([`Canvas::track`]), in the coordinates of the clip
/// rectangle they were rasterized against.
struct Spans {
    gather: bool,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Spans {
    fn new(gather: bool) -> Spans {
        Spans { gather, x0: u32::MAX, y0: u32::MAX, x1: 0, y1: 0 }
    }

    #[inline]
    fn add(&mut self, span: &Span<'_>) {
        if self.gather && span.len > 0 {
            self.x0 = self.x0.min(span.x);
            self.y0 = self.y0.min(span.y);
            self.x1 = self.x1.max(span.x + span.len);
            self.y1 = self.y1.max(span.y + 1);
        }
    }

    /// What they cover, in device coordinates (empty if none came).
    fn bounds(&self, clip: Rect) -> Rect {
        if self.x1 <= self.x0 || self.y1 <= self.y0 {
            return Rect::default();
        }
        let (x, y) = (clip.x + self.x0 as i32, clip.y + self.y0 as i32);
        Rect::new(x, y, (self.x1 - self.x0) as i32, (self.y1 - self.y0) as i32)
    }
}

impl<'a> Canvas<'a> {
    /// Wraps a pixel buffer of `width` x `height` with `stride` pixels per row.
    pub fn new(pixels: &'a mut [u32], width: i32, height: i32, stride: i32) -> Canvas<'a> {
        Canvas::make(pixels, width, height, stride, Cache::Own(None))
    }

    /// Like [`Canvas::new`], drawing with `cache`, which it leaves for the
    /// next canvas.
    pub fn with_cache(
        pixels: &'a mut [u32],
        width: i32,
        height: i32,
        stride: i32,
        cache: &'a mut DrawCache,
    ) -> Canvas<'a> {
        Canvas::make(pixels, width, height, stride, Cache::Lent(cache))
    }

    fn make(pixels: &'a mut [u32], width: i32, height: i32, stride: i32, cache: Cache<'a>) -> Canvas<'a> {
        // In 64 bits, where a large stride times a large height cannot wrap.
        assert!(stride >= width.max(0) && pixels.len() as u64 >= stride as u64 * height.max(0) as u64);
        Canvas {
            pixels,
            width,
            height,
            stride,
            clip: Rect::new(0, 0, width, height),
            ox: 0,
            oy: 0,
            stack: Vec::new(),
            cache,
            tracked: None,
        }
    }

    pub fn for_bitmap(b: &'a mut Bitmap) -> Canvas<'a> {
        let (w, h) = (b.width, b.height);
        Canvas::new(&mut b.pixels, w, h, w)
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    /// The full canvas in local coordinates.
    pub fn bounds(&self) -> Rect {
        Rect::new(-self.ox, -self.oy, self.width, self.height)
    }

    /// The current clip in local coordinates.
    pub fn clip_rect(&self) -> Rect {
        self.clip.translate(-self.ox, -self.oy)
    }

    pub fn save(&mut self) {
        self.stack.push((self.clip, self.ox, self.oy));
    }

    pub fn restore(&mut self) {
        if let Some((c, x, y)) = self.stack.pop() {
            self.clip = c;
            self.ox = x;
            self.oy = y;
        }
    }

    pub fn translate(&mut self, dx: i32, dy: i32) {
        self.ox += dx;
        self.oy += dy;
    }

    /// Restricts drawing to `r` (local coordinates) within the current clip.
    pub fn clip_to(&mut self, r: Rect) {
        self.clip = self.clip.intersect(&r.translate(self.ox, self.oy));
    }

    /// Device-space rectangle clipped to the current clip.
    pub(crate) fn device(&self, r: Rect) -> Rect {
        r.translate(self.ox, self.oy).intersect(&self.clip)
    }

    /// The device position of local `(x, y)`.
    pub(crate) fn to_device(&self, x: i32, y: i32) -> (i32, i32) {
        (x + self.ox, y + self.oy)
    }

    /// Pixels `x0..x1` of device row `y` (inside the canvas).
    #[inline]
    pub(crate) fn device_row(&mut self, y: i32, x0: i32, x1: i32) -> &mut [u32] {
        let start = (y * self.stride) as usize;
        &mut self.pixels[start + x0 as usize..start + x1 as usize]
    }

    /// Raw access to the pixels (premultiplied), e.g. for custom effects.
    /// What is written through it is not known: tracking takes it as all of
    /// the area ([`Canvas::track`]).
    pub fn pixels_mut(&mut self) -> (&mut [u32], i32) {
        self.touch(Rect::new(0, 0, self.width, self.height));
        (self.pixels, self.stride)
    }

    /// From now on, notes where drawing touches `area` (device
    /// coordinates), until [`Canvas::take_touched`]: the rectangles that
    /// each operation drew in, clipped as it was, of the area alone (a
    /// [`Damage`], so near ones merge).
    pub fn track(&mut self, area: Rect) {
        let area = area.intersect(&Rect::new(0, 0, self.width, self.height));
        self.tracked = Some((area, Damage::new()));
    }

    /// Where drawing touched the area [`Canvas::track`] gave, which is no
    /// longer tracked; `None` if none was.
    pub fn take_touched(&mut self) -> Option<Damage> {
        self.tracked.take().map(|(_, drawn)| drawn)
    }

    /// Notes that drawing touched `d` (device coordinates).
    #[inline]
    pub(crate) fn touch(&mut self, d: Rect) {
        if let Some((area, drawn)) = &mut self.tracked {
            drawn.add(d.intersect(area));
        }
    }

    /// Copies `src` of `picture` as it is, without blending, to device
    /// position `(x, y)`, whatever the clip and the origin, and unnoted by
    /// tracking ([`Canvas::track`]): how a window puts back a picture that
    /// it keeps where the picture changed, or where something was drawn
    /// over it.
    pub fn put_back(&mut self, picture: &Picture<'_>, src: Rect, x: i32, y: i32) {
        let part = src.intersect(&Rect::new(0, 0, picture.width, picture.height));
        // Where the part of `src` in the picture goes, cut to the canvas.
        let (x, y) = (x.saturating_add(part.x.saturating_sub(src.x)), y.saturating_add(part.y.saturating_sub(src.y)));
        let to = Rect::new(x, y, part.w, part.h).intersect(&Rect::new(0, 0, self.width, self.height));
        if to.is_empty() {
            return;
        }
        let (sx, sy) = (part.x + to.x - x, part.y + to.y - y);
        for row in 0..to.h {
            let from = &picture.row(sy + row)[sx as usize..(sx + to.w) as usize];
            self.device_row(to.y + row, to.x, to.right()).copy_from_slice(from);
        }
    }

    pub fn clear(&mut self, color: Color) {
        let px = color.premul();
        let c = self.clip;
        self.touch(c);
        for y in c.y..c.bottom() {
            self.device_row(y, c.x, c.right()).fill(px);
        }
    }

    /// Fills a rectangle (alpha-blended).
    pub fn fill_rect(&mut self, r: Rect, color: Color) {
        self.fill_rect_px(r, color.premul());
    }

    pub fn fill_rect_px(&mut self, r: Rect, px: u32) {
        let d = self.device(r);
        self.touch(d);
        for y in d.y..d.bottom() {
            blend_run(self.device_row(y, d.x, d.right()), px);
        }
    }

    pub fn fill_vertical_gradient(&mut self, r: Rect, top: Color, bottom: Color) {
        let d = self.device(r);
        self.touch(d);
        let ry = r.y + self.oy;
        for y in d.y..d.bottom() {
            let t = if r.h > 1 { (y - ry) as f32 / (r.h - 1) as f32 } else { 0.0 };
            let px = top.lerp(bottom, t).premul();
            blend_run(self.device_row(y, d.x, d.right()), px);
        }
    }

    pub fn fill_horizontal_gradient(&mut self, r: Rect, left: Color, right: Color) {
        let d = self.device(r);
        if d.is_empty() {
            return;
        }
        self.touch(d);
        let rx = r.x + self.ox;
        let cols: Vec<u32> = (d.x..d.right())
            .map(|x| {
                let t = if r.w > 1 { (x - rx) as f32 / (r.w - 1) as f32 } else { 0.0 };
                left.lerp(right, t).premul()
            })
            .collect();
        for y in d.y..d.bottom() {
            let row = self.device_row(y, d.x, d.right());
            for (dst, &px) in row.iter_mut().zip(&cols) {
                *dst = over(px, *dst);
                keep_scalar();
            }
        }
    }

    /// Fills a rounded rectangle with exact anti-aliased corners, taking the
    /// color of each row from `row_color` (premultiplied).
    fn rounded_rect_rows(&mut self, r: Rect, radius: f32, mut row_color: impl FnMut(i32) -> u32) {
        let d = self.device(r);
        if d.is_empty() {
            return;
        }
        self.touch(d);
        let rad = radius.min(r.w as f32 / 2.0).min(r.h as f32 / 2.0).max(0.0);
        let band = rad.ceil() as i32;
        let (rx0, ry0) = ((r.x + self.ox) as f32, (r.y + self.oy) as f32);
        let (rx1, ry1) = (rx0 + r.w as f32, ry0 + r.h as f32);
        let ry_i = r.y + self.oy;
        // Where the left corners end and the right ones start.
        let x_left_end = (r.x + self.ox + band).min(d.right());
        let x_right_start = (r.right() + self.ox - band).max(d.x).max(x_left_end);
        for y in d.y..d.bottom() {
            let px = row_color(y - ry_i);
            if px == 0 {
                continue;
            }
            let ly = y - ry_i;
            let in_band = ly < band || ly >= r.h - band;
            let row = self.device_row(y, 0, self.width);
            if !in_band || rad <= 0.0 {
                blend_run(&mut row[d.x as usize..d.right() as usize], px);
                continue;
            }
            let cy = if ly < band { ry0 + rad } else { ry1 - rad };
            let fy = y as f32 + 0.5 - cy;
            let cov_at = |x: i32, cx: f32| {
                let fx = x as f32 + 0.5 - cx;
                disc_coverage((fx * fx + fy * fy).sqrt(), rad)
            };
            for x in d.x..x_left_end {
                blend_cov(&mut row[x as usize], px, cov_at(x, rx0 + rad));
                keep_scalar();
            }
            let mid0 = x_left_end.max(d.x);
            let mid1 = x_right_start.min(d.right());
            if mid1 > mid0 {
                blend_run(&mut row[mid0 as usize..mid1 as usize], px);
            }
            for x in x_right_start..d.right() {
                blend_cov(&mut row[x as usize], px, cov_at(x, rx1 - rad));
                keep_scalar();
            }
        }
    }

    pub fn fill_rounded_rect(&mut self, r: Rect, radius: f32, color: Color) {
        let px = color.premul();
        self.rounded_rect_rows(r, radius, |_| px);
    }

    pub fn fill_rounded_rect_gradient(&mut self, r: Rect, radius: f32, top: Color, bottom: Color) {
        let h = (r.h - 1).max(1) as f32;
        self.rounded_rect_rows(r, radius, |ly| top.lerp(bottom, ly as f32 / h).premul());
    }

    /// Outlines a rounded rectangle with a `width`-pixel line inside `r`.
    pub fn stroke_rounded_rect(&mut self, r: Rect, radius: f32, width: f32, color: Color) {
        let mut path = Path::new();
        let hw = width / 2.0;
        let rad = (radius - hw).max(0.0);
        path.rounded_rect(r.x as f32 + hw, r.y as f32 + hw, r.w as f32 - width, r.h as f32 - width, [rad; 4]);
        self.stroke_path(&path, &StrokeStyle::new(width), color);
    }

    pub fn fill_circle(&mut self, cx: f32, cy: f32, radius: f32, color: Color) {
        let px = color.premul();
        if px == 0 || radius.is_nan() || radius <= 0.0 {
            return;
        }
        // The pixels the disc may touch, kept in a range where the
        // arithmetic of rectangles cannot overflow (the clip trims them).
        let limit = |v: f32| v.clamp(-(1 << 29) as f32, (1 << 29) as f32) as i32;
        let (dcx, dcy) = (cx + self.ox as f32, cy + self.oy as f32);
        let (x0, y0) = (limit((dcx - radius - 1.0).floor()), limit((dcy - radius - 1.0).floor()));
        let (x1, y1) = (limit((dcx + radius + 1.0).ceil()), limit((dcy + radius + 1.0).ceil()));
        let d = Rect::new(x0, y0, x1 - x0, y1 - y0).intersect(&self.clip);
        self.touch(d);
        for y in d.y..d.bottom() {
            let fy = y as f32 + 0.5 - dcy;
            let row = self.device_row(y, d.x, d.right());
            for (x, dst) in (d.x..).zip(row.iter_mut()) {
                let fx = x as f32 + 0.5 - dcx;
                blend_cov(dst, px, disc_coverage((fx * fx + fy * fy).sqrt(), radius));
                keep_scalar();
            }
        }
    }

    /// Fills a vector path (local coordinates) with a solid color.
    pub fn fill_path(&mut self, path: &Path, color: Color, rule: FillRule) {
        self.fill_path_transformed(path, &Transform::IDENTITY, color, rule);
    }

    pub fn fill_path_transformed(&mut self, path: &Path, t: &Transform, color: Color, rule: FillRule) {
        let px = color.premul();
        let clip = self.clip;
        if px == 0 || clip.is_empty() {
            return;
        }
        // Rasterized in the clip's coordinates: what cannot show is not
        // computed.
        let t = t.then_translate((self.ox - clip.x) as f32, (self.oy - clip.y) as f32);
        let Canvas { pixels, stride, cache, tracked, .. } = self;
        let stride = *stride;
        let raster: &mut Rasterizer = &mut cache.get().raster;
        let mut spans = Spans::new(tracked.is_some());
        raster.fill(path, &t, rule, (clip.w as u32, clip.h as u32), |span| {
            spans.add(&span);
            blend_span(pixels, stride, clip, px, span)
        });
        self.touch(spans.bounds(clip));
    }

    pub fn stroke_path(&mut self, path: &Path, style: &StrokeStyle, color: Color) {
        self.stroke_path_transformed(path, style, &Transform::IDENTITY, color);
    }

    pub fn stroke_path_transformed(&mut self, path: &Path, style: &StrokeStyle, t: &Transform, color: Color) {
        let px = color.premul();
        let clip = self.clip;
        if px == 0 || clip.is_empty() {
            return;
        }
        let t = t.then_translate((self.ox - clip.x) as f32, (self.oy - clip.y) as f32);
        let Canvas { pixels, stride, cache, tracked, .. } = self;
        let stride = *stride;
        let raster: &mut Rasterizer = &mut cache.get().raster;
        let mut spans = Spans::new(tracked.is_some());
        raster.stroke(path, style, &t, (clip.w as u32, clip.h as u32), |span| {
            spans.add(&span);
            blend_span(pixels, stride, clip, px, span)
        });
        self.touch(spans.bounds(clip));
    }

    pub fn draw_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Color) {
        let mut p = Path::new();
        p.move_to(x0, y0);
        p.line_to(x1, y1);
        self.stroke_path(&p, &StrokeStyle::new(width), color);
    }

    /// Blends an 8-bit coverage mask (`w` x `h`, row stride `w`) at (x, y)
    /// tinted with `color` (used for glyphs and icons). A mask shorter than
    /// `w` x `h` draws nothing.
    pub fn fill_mask(&mut self, x: i32, y: i32, w: i32, h: i32, mask: &[u8], color: Color) {
        let px = color.premul();
        let size = (w.max(0) as usize).checked_mul(h.max(0) as usize);
        if px == 0 || size.is_none_or(|n| mask.len() < n) {
            return;
        }
        let d = self.device(Rect::new(x, y, w, h));
        self.touch(d);
        let (mx, my) = self.to_device(x, y);
        for dy in d.y..d.bottom() {
            let from = ((dy - my) * w + d.x - mx) as usize;
            let covers = &mask[from..from + d.w as usize];
            for (dst, &cov) in self.device_row(dy, d.x, d.right()).iter_mut().zip(covers) {
                blend_cov(dst, px, cov as u32);
                keep_scalar();
            }
        }
    }

    /// Draws a bitmap at (x, y) with `opacity` (0..=255).
    pub fn draw_bitmap(&mut self, b: &Bitmap, x: i32, y: i32, opacity: u8) {
        self.draw_bitmap_region(b, b.rect(), x, y, opacity);
    }

    /// Draws the `src` part of a bitmap with its top-left at (x, y).
    pub fn draw_bitmap_region(&mut self, b: &Bitmap, src: Rect, x: i32, y: i32, opacity: u8) {
        let src = src.intersect(&b.rect());
        let d = self.device(Rect::new(x, y, src.w, src.h));
        let (dx0, dy0) = self.to_device(x, y);
        let op = opacity as u32;
        if op == 0 {
            return;
        }
        self.touch(d);
        for dy in d.y..d.bottom() {
            let sy = src.y + dy - dy0;
            let srow = (sy * b.width + src.x - dx0) as isize;
            let srcrow = &b.pixels[(srow + d.x as isize) as usize..(srow + d.right() as isize) as usize];
            let dst = self.device_row(dy, d.x, d.right());
            if op == 255 {
                for (o, &s) in dst.iter_mut().zip(srcrow) {
                    *o = over(s, *o);
                    keep_scalar();
                }
            } else {
                for (o, &s) in dst.iter_mut().zip(srcrow) {
                    *o = over(scale(s, op), *o);
                    keep_scalar();
                }
            }
        }
    }

    /// Copies a bitmap region without blending (fast path for opaque images).
    pub fn blit(&mut self, b: &Bitmap, src: Rect, x: i32, y: i32) {
        let src = src.intersect(&b.rect());
        let d = self.device(Rect::new(x, y, src.w, src.h));
        self.touch(d);
        let (dx0, dy0) = self.to_device(x, y);
        for dy in d.y..d.bottom() {
            let sy = src.y + dy - dy0;
            let sx = src.x + d.x - dx0;
            let s = (sy * b.width + sx) as usize;
            self.device_row(dy, d.x, d.right()).copy_from_slice(&b.pixels[s..s + d.w as usize]);
        }
    }

    /// Draws `picture` with its top-left at (x, y), its corners rounded by
    /// `radii` (top-left, top-right, bottom-right, bottom-left, each at most
    /// half a side; anti-aliased as [`Canvas::fill_rounded_rect`]'s are),
    /// faded by `opacity` (0..=255). Where the picture is opaque
    /// ([`Picture::opaque`]) and not faded, the rows between the corners are
    /// copied; only the corners are blended.
    pub fn draw_rounded_picture(&mut self, picture: Picture<'_>, x: i32, y: i32, radii: [f32; 4], opacity: u8) {
        let (w, h) = (picture.width, picture.height);
        let d = self.device(Rect::new(x, y, w, h));
        if d.is_empty() || opacity == 0 {
            return;
        }
        self.touch(d);
        let (px0, py0) = self.to_device(x, y);
        let most = (w as f32 / 2.0).min(h as f32 / 2.0);
        // Not-a-number and negative radii are square corners.
        let [tl, tr, br, bl] = radii.map(|r| if r > 0.0 { r.min(most) } else { 0.0 });
        let op = opacity as u32;
        let copy = picture.opaque && op == 255;
        let alpha = if picture.opaque { 0xFF00_0000 } else { 0 };
        // A pixel of the picture as it is blended, at coverage `cov`.
        let blend = |dst: &mut u32, s: u32, cov: u32| blend_cov(dst, s | alpha, (cov * op + 127) / 255);
        for dy in d.y..d.bottom() {
            let ly = dy - py0;
            let src = picture.row(ly);
            // The corner of one side this row crosses: its radius and its
            // centre's height.
            let corner = |top: f32, bottom: f32| {
                if ly < top.ceil() as i32 {
                    Some((top, (py0 as f32) + top))
                } else if ly >= h - bottom.ceil() as i32 {
                    Some((bottom, (py0 + h) as f32 - bottom))
                } else {
                    None
                }
            };
            let (left, right) = (corner(tl, bl), corner(tr, br));
            let left_end = (px0 + left.map_or(0, |(r, _)| r.ceil() as i32)).min(d.right());
            let right_start = (px0 + w - right.map_or(0, |(r, _)| r.ceil() as i32)).max(left_end).max(d.x);
            let row = self.device_row(dy, 0, self.width);
            let fy = dy as f32 + 0.5;
            if let Some((r, cy)) = left {
                let cx = px0 as f32 + r;
                for x in d.x..left_end {
                    let (fx, fy) = (x as f32 + 0.5 - cx, fy - cy);
                    blend(&mut row[x as usize], src[(x - px0) as usize], disc_coverage((fx * fx + fy * fy).sqrt(), r));
                    keep_scalar();
                }
            }
            let (mid0, mid1) = (left_end.max(d.x), right_start.min(d.right()));
            if mid1 > mid0 {
                let (s0, s1) = ((mid0 - px0) as usize, (mid1 - px0) as usize);
                let dst = &mut row[mid0 as usize..mid1 as usize];
                if copy {
                    dst.copy_from_slice(&src[s0..s1]);
                } else {
                    for (o, &s) in dst.iter_mut().zip(&src[s0..s1]) {
                        blend(o, s, 255);
                        keep_scalar();
                    }
                }
            }
            if let Some((r, cy)) = right {
                let cx = (px0 + w) as f32 - r;
                for x in right_start..d.right() {
                    let (fx, fy) = (x as f32 + 0.5 - cx, fy - cy);
                    blend(&mut row[x as usize], src[(x - px0) as usize], disc_coverage((fx * fx + fy * fy).sqrt(), r));
                    keep_scalar();
                }
            }
        }
    }

    /// Draws a bitmap scaled into `dst`. Sampling positions are computed in
    /// 16.16 fixed point once per column and per row (floating point per
    /// pixel is slow under CPU emulation).
    pub fn draw_bitmap_scaled(&mut self, b: &Bitmap, dst: Rect, filter: Filter, opacity: u8) {
        if b.width == 0 || b.height == 0 || dst.is_empty() {
            return;
        }
        let d = self.device(dst);
        if d.is_empty() {
            return;
        }
        self.touch(d);
        let (dx0, dy0) = self.to_device(dst.x, dst.y);
        let bilinear = filter == Filter::Bilinear;
        let cols: Vec<(usize, usize, u32)> =
            (d.x..d.right()).map(|x| taps(x - dx0, b.width, dst.w, bilinear)).collect();
        let w = b.width as usize;
        let op = opacity as u32;
        for y in d.y..d.bottom() {
            let (r0, r1, wy) = taps(y - dy0, b.height, dst.h, bilinear);
            let (row0, row1) = (&b.pixels[r0 * w..r0 * w + w], &b.pixels[r1 * w..r1 * w + w]);
            let out = self.device_row(y, d.x, d.right());
            for (p, &(c0, c1, wx)) in out.iter_mut().zip(&cols) {
                let s = if bilinear {
                    lerp_px(lerp_px(row0[c0], row0[c1], wx), lerp_px(row1[c0], row1[c1], wx), wy)
                } else {
                    row0[c0]
                };
                let s = if op == 255 { s } else { scale(s, op) };
                *p = over(s, *p);
                keep_scalar();
            }
        }
    }

    /// Copies `w` x `h` opaque pixels from `src` (`stride` pixels from one
    /// row to the next) with their top-left at (x, y), without blending.
    ///
    /// # Safety
    /// `src` points to `h` rows of `w` readable pixels, `stride` apart, for
    /// the whole call. They may be memory another process writes (a video
    /// decoder's pictures): they are only copied, never borrowed.
    pub unsafe fn blit_raw(&mut self, src: *const u32, stride: usize, w: i32, h: i32, x: i32, y: i32) {
        if w <= 0 || h <= 0 {
            return;
        }
        let d = self.device(Rect::new(x, y, w, h));
        self.touch(d);
        let (dx0, dy0) = self.to_device(x, y);
        for dy in d.y..d.bottom() {
            let s = (dy - dy0) as usize * stride + (d.x - dx0) as usize;
            let out = self.device_row(dy, d.x, d.right());
            // SAFETY: the clipped rectangle lies inside the caller's `w` x
            // `h` pixels, so the row is readable; `out` is the canvas's own.
            unsafe { core::ptr::copy_nonoverlapping(src.add(s), out.as_mut_ptr(), out.len()) };
        }
    }

    /// Draws `w` x `h` opaque pixels from `src` scaled into `dst`, without
    /// blending; sampled as [`Canvas::draw_bitmap_scaled`] does.
    ///
    /// # Safety
    /// As for [`Canvas::blit_raw`].
    pub unsafe fn draw_raw_scaled(
        &mut self,
        src: *const u32,
        stride: usize,
        w: i32,
        h: i32,
        dst: Rect,
        filter: Filter,
    ) {
        if w <= 0 || h <= 0 || dst.is_empty() {
            return;
        }
        let d = self.device(dst);
        if d.is_empty() {
            return;
        }
        self.touch(d);
        let (dx0, dy0) = self.to_device(dst.x, dst.y);
        let bilinear = filter == Filter::Bilinear;
        let cols: Vec<(usize, usize, u32)> = (d.x..d.right()).map(|x| taps(x - dx0, w, dst.w, bilinear)).collect();
        for y in d.y..d.bottom() {
            let (r0, r1, wy) = taps(y - dy0, h, dst.h, bilinear);
            // SAFETY: the taps are clamped inside the caller's `w` x `h`
            // pixels; each is read by value.
            let px = |r: usize, c: usize| unsafe { src.add(r * stride + c).read() };
            let out = self.device_row(y, d.x, d.right());
            for (p, &(c0, c1, wx)) in out.iter_mut().zip(&cols) {
                *p = if bilinear {
                    lerp_px(lerp_px(px(r0, c0), px(r0, c1), wx), lerp_px(px(r1, c0), px(r1, c1), wx), wy)
                } else {
                    px(r0, c0)
                };
                keep_scalar();
            }
        }
    }

    /// Draws a soft shadow of the rounded rectangle `r` (see
    /// [`ShadowTemplate`]), with a template the canvas's [`DrawCache`] keeps
    /// for the next shadow of the same radius and blur.
    pub fn draw_shadow(&mut self, r: Rect, radius: i32, blur: i32, color: Color) {
        let template = self.cache.get().take_shadow(radius, blur);
        template.draw(self, r, color, true);
        self.cache.get().keep_shadow(template);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::vec;

    use super::*;

    #[test]
    fn raw_pixels_are_copied_and_clipped() {
        let src: Vec<u32> = (0..6 * 4).map(|i| 0xFF00_0000 | i).collect();
        let mut dst = vec![0u32; 8 * 5];
        let mut c = Canvas::new(&mut dst, 8, 5, 8);
        // 4 x 3 of the 6-wide source, its top-left at (6, 3): clipped to 2 x 2.
        // SAFETY: `src` holds 4 rows of 6 pixels.
        unsafe { c.blit_raw(src.as_ptr(), 6, 4, 3, 6, 3) };
        drop(c);
        assert_eq!(&dst[3 * 8 + 6..3 * 8 + 8], &[0xFF00_0000, 0xFF00_0001]);
        assert_eq!(&dst[4 * 8 + 6..4 * 8 + 8], &[0xFF00_0006, 0xFF00_0007]);
        assert_eq!(dst.iter().filter(|&&p| p != 0).count(), 4);
    }

    #[test]
    fn raw_pixels_scale_like_bitmaps() {
        let mut b = Bitmap::new(5, 3);
        for (i, p) in b.pixels.iter_mut().enumerate() {
            *p = 0xFF00_0000 | (i as u32 * 0x0A0B0C);
        }
        let dst = Rect::new(1, 1, 13, 7);
        for filter in [Filter::Nearest, Filter::Bilinear] {
            let (mut a, mut r) = (vec![0u32; 16 * 10], vec![0u32; 16 * 10]);
            Canvas::new(&mut a, 16, 10, 16).draw_bitmap_scaled(&b, dst, filter, 255);
            // SAFETY: the bitmap holds 3 rows of 5 pixels.
            unsafe { Canvas::new(&mut r, 16, 10, 16).draw_raw_scaled(b.pixels.as_ptr(), 5, 5, 3, dst, filter) };
            assert_eq!(a, r, "{filter:?}");
        }
    }

    /// A shape with curves, overlaps and edges at every angle.
    fn shape() -> Path {
        let mut p = Path::new();
        p.circle(37.3, 29.6, 21.4);
        p.rounded_rect(8.5, 40.25, 70.0, 22.0, [9.0, 3.0, 12.0, 0.0]);
        p.move_to(70.0, 2.0);
        p.cubic_to(95.0, 30.0, 10.0, 90.0, 60.0, 75.0);
        p
    }

    /// The pixels of a 96 x 80 canvas, cleared to a pattern, after `draw`.
    fn drawn(draw: impl FnOnce(&mut Canvas)) -> Vec<u32> {
        let mut pixels: Vec<u32> =
            (0..96 * 80u32).map(|i| 0xFF00_0000 | (i.wrapping_mul(2_654_435_761) >> 8)).collect();
        draw(&mut Canvas::new(&mut pixels, 96, 80, 96));
        pixels
    }

    #[test]
    fn paths_drawn_in_a_clip_match_the_unclipped_drawing_there() {
        let color = Color::rgba(250, 120, 30, 200);
        let style = StrokeStyle::new(3.5);
        let whole_fill = drawn(|c| c.fill_path(&shape(), color, FillRule::NonZero));
        let whole_stroke = drawn(|c| c.stroke_path(&shape(), &style, color));
        let untouched = drawn(|_| {});
        for clip in
            [Rect::new(0, 0, 96, 80), Rect::new(13, 7, 30, 21), Rect::new(50, 33, 46, 47), Rect::new(70, 0, 5, 80)]
        {
            for (whole, stroke) in [(&whole_fill, false), (&whole_stroke, true)] {
                // Through a translated origin too: what is drawn must not move.
                let got = drawn(|c| {
                    c.translate(5, -3);
                    c.clip_to(clip.translate(-5, 3));
                    let t = Transform::translate(-5.0, 3.0);
                    if stroke {
                        c.stroke_path_transformed(&shape(), &style, &t, color);
                    } else {
                        c.fill_path_transformed(&shape(), &t, color, FillRule::NonZero);
                    }
                });
                for (i, (&g, (&w, &u))) in got.iter().zip(whole.iter().zip(&untouched)).enumerate() {
                    let (x, y) = (i as i32 % 96, i as i32 / 96);
                    if !clip.contains(x, y) {
                        assert_eq!(g, u, "clip {clip:?} stroke {stroke}: ({x}, {y}) is outside");
                        continue;
                    }
                    // Where an edge crosses the clip's border, it is cut there
                    // in floating point: the coverage may round differently.
                    for s in [0, 8, 16, 24] {
                        let (a, b) = ((g >> s) & 0xFF, (w >> s) & 0xFF);
                        assert!(a.abs_diff(b) <= 2, "clip {clip:?} stroke {stroke} at ({x}, {y}): {g:08x} {w:08x}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_lent_cache_keeps_the_rasterizer_and_the_shadows() {
        let mut cache = DrawCache::new();
        let mut a = vec![0u32; 64 * 64];
        let mut b = vec![0u32; 64 * 64];
        for pixels in [&mut a, &mut b] {
            let mut c = Canvas::with_cache(pixels, 64, 64, 64, &mut cache);
            c.fill_path(&shape(), Color::WHITE, FillRule::NonZero);
            c.draw_shadow(Rect::new(10, 10, 30, 20), 6, 8, Color::BLACK);
            c.draw_shadow(Rect::new(20, 20, 30, 20), 6, 8, Color::BLACK);
        }
        assert_eq!(a, b);
        // One template, kept from the first shadow to the last.
        assert_eq!(cache.shadows.len(), 1);
        let mut c = Canvas::with_cache(&mut a, 64, 64, 64, &mut cache);
        for blur in 0..12 {
            c.draw_shadow(Rect::new(10, 10, 30, 20), 4, blur, Color::BLACK);
        }
        // Huge ones are not kept.
        c.draw_shadow(Rect::new(10, 10, 30, 20), 100, 100, Color::BLACK);
        drop(c);
        assert_eq!(cache.shadows.len(), SHADOWS_KEPT);
        assert_eq!((cache.shadows[0].radius(), cache.shadows[0].blur()), (4, 11));
    }

    /// How much of pixel (x, y) of a `w` x `h` picture at the origin its
    /// rounded corners leave: each corner's square, as many pixels wide and
    /// high as its radius rounded up, is covered by the corner's circle.
    fn reference_cover(x: i32, y: i32, w: i32, h: i32, [tl, tr, br, bl]: [f32; 4]) -> u32 {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let square = |r: f32, left: bool, top: bool| {
            let n = r.ceil() as i32;
            (if left { x < n } else { x >= w - n }) && (if top { y < n } else { y >= h - n })
        };
        let cover = |r: f32, cx: f32, cy: f32| disc_coverage(((px - cx).powi(2) + (py - cy).powi(2)).sqrt(), r);
        let (w, h) = (w as f32, h as f32);
        if square(tl, true, true) {
            cover(tl, tl, tl)
        } else if square(bl, true, false) {
            cover(bl, bl, h - bl)
        } else if square(tr, false, true) {
            cover(tr, w - tr, tr)
        } else if square(br, false, false) {
            cover(br, w - br, h - br)
        } else {
            255
        }
    }

    #[test]
    fn rounded_pictures_match_rounded_rectangles() {
        // One colour: the same as a rounded rectangle of it, corner for corner.
        let colour = 0xFF33_6699u32;
        let flat = Bitmap::filled(40, 30, colour);
        for clip in [Rect::new(0, 0, 96, 80), Rect::new(12, 9, 9, 40), Rect::new(30, 20, 40, 40)] {
            for radius in [0.0f32, 1.5, 6.0, 15.0, 40.0] {
                let rect = drawn(|c| {
                    c.clip_to(clip);
                    c.fill_rounded_rect(Rect::new(9, 7, 40, 30), radius, Color(colour));
                });
                for picture in [flat.picture(), flat.picture().opaque()] {
                    let pic = drawn(|c| {
                        c.clip_to(clip);
                        c.draw_rounded_picture(picture, 9, 7, [radius; 4], 255);
                    });
                    assert_eq!(pic, rect, "clip {clip:?} radius {radius}");
                }
            }
        }
        // Each corner its own, over a picture of many colours.
        let mut varied = Bitmap::new(23, 17);
        for (i, p) in varied.pixels.iter_mut().enumerate() {
            *p = 0xFF00_0000 | (i as u32 * 0x01_0305);
        }
        let radii = [0.0, 3.5, 8.0, 5.0];
        let under = 0xFF20_2020u32;
        let mut out = vec![under; 23 * 17];
        Canvas::new(&mut out, 23, 17, 23).draw_rounded_picture(varied.picture().opaque(), 0, 0, radii, 255);
        for y in 0..17 {
            for x in 0..23 {
                let s = varied.pixels[(y * 23 + x) as usize];
                let cov = reference_cover(x, y, 23, 17, radii);
                let want = if cov >= 255 { s } else { over(scale(s, cov), under) };
                assert_eq!(out[(y * 23 + x) as usize], want, "({x}, {y})");
            }
        }
        // Faded, and with a picture whose rows are further apart than wide.
        let wide: Vec<u32> = (0..10 * 4).map(|i| if i % 10 < 6 { 0xFFFF_FFFF } else { 0x1234_5678 }).collect();
        let picture = Picture::new(&wide, 6, 4, 10).unwrap().opaque();
        let mut out = vec![0xFF00_0000u32; 6 * 4];
        Canvas::new(&mut out, 6, 4, 6).draw_rounded_picture(picture, 0, 0, [0.0; 4], 128);
        assert!(out.iter().all(|&p| p == 0xFF80_8080), "{out:08x?}");
        assert!(Picture::new(&wide, 6, 5, 10).is_none(), "five rows do not fit");
        assert!(Picture::new(&wide, 11, 1, 10).is_none(), "rows wider than their stride");
    }

    #[test]
    fn shapes_far_out_of_range_draw_nothing_and_do_not_overflow() {
        let untouched = drawn(|_| {});
        let got = drawn(|c| {
            c.fill_circle(1e20, 5.0, 3.0, Color::WHITE);
            c.fill_circle(5.0, 5.0, f32::INFINITY, Color::rgba(0, 0, 0, 0));
            c.fill_circle(f32::NAN, 5.0, 3.0, Color::WHITE);
            c.fill_mask(3, 3, 4, 4, &[255; 15], Color::WHITE);
        });
        assert_eq!(got, untouched);
        let full = drawn(|c| c.fill_circle(5.0, 5.0, f32::INFINITY, Color::WHITE));
        assert!(full.iter().all(|&p| p == 0xFFFF_FFFF));
    }
}

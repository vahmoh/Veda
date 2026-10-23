// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Visual check: renders a few shapes and strokes to a PNG for manual inspection.
//!
//! Run with `cargo test -p raster -- --ignored --nocapture`; the image is written to
//! `target/agents/font/samples/raster_shapes.png` (relative to the workspace root).

use alloc::vec::Vec;
use std::string::String;

use crate::*;

/// The picture (RGB, 8 bits a channel) as a PNG file.
pub fn encode_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(rgb).unwrap();
    writer.finish().unwrap();
    out
}

/// An RGB canvas that blends spans in a solid color.
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

impl Canvas {
    pub fn new(width: u32, height: u32, bg: [u8; 3]) -> Self {
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for _ in 0..width * height {
            rgb.extend_from_slice(&bg);
        }
        Canvas { width, height, rgb }
    }

    pub fn blend(&mut self, x: u32, y: u32, a: u8, color: [u8; 3]) {
        if x >= self.width || y >= self.height || a == 0 {
            return;
        }
        let i = ((y * self.width + x) * 3) as usize;
        for (c, &s) in color.iter().enumerate() {
            let d = self.rgb[i + c] as u32;
            self.rgb[i + c] = ((s as u32 * a as u32 + d * (255 - a as u32) + 127) / 255) as u8;
        }
    }

    pub fn span(&mut self, s: &Span<'_>, color: [u8; 3]) {
        for i in 0..s.len {
            self.blend(s.x + i, s.y, s.coverage_at(i), color);
        }
    }

    pub fn zoomed(&self, x0: u32, y0: u32, w: u32, h: u32, k: u32) -> Canvas {
        let mut out = Canvas::new(w * k, h * k, [0, 0, 0]);
        for y in 0..h * k {
            for x in 0..w * k {
                let (sx, sy) = (x0 + x / k, y0 + y / k);
                let si = ((sy * self.width + sx) * 3) as usize;
                let di = ((y * w * k + x) * 3) as usize;
                out.rgb[di..di + 3].copy_from_slice(&self.rgb[si..si + 3]);
            }
        }
        out
    }

    pub fn save(&self, name: &str) {
        let dir = String::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../target/agents/font/samples"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = std::format!("{dir}/{name}");
        std::fs::write(&path, encode_png(self.width, self.height, &self.rgb)).unwrap();
        std::println!("wrote {path}");
    }
}

#[test]
#[ignore]
fn render_shapes_png() {
    let mut cv = Canvas::new(640, 420, [255, 255, 255]);
    let mut r = Rasterizer::new();
    let id = Transform::IDENTITY;
    let draw = |cv: &mut Canvas, r: &mut Rasterizer, p: &Path, t: &Transform, rule: FillRule, color: [u8; 3]| {
        r.fill(p, t, rule, (cv.width, cv.height), |s| cv.span(&s, color));
    };

    // Rounded rectangles with per-corner radii.
    let mut p = Path::new();
    p.rounded_rect(20.5, 20.5, 160.0, 90.0, [4.0, 16.0, 32.0, 0.0]);
    draw(&mut cv, &mut r, &p, &id, FillRule::NonZero, [40, 110, 220]);
    let mut p = Path::new();
    p.rounded_rect(40.0, 40.0, 120.0, 50.0, [25.0; 4]);
    draw(&mut cv, &mut r, &p, &id, FillRule::NonZero, [250, 250, 250]);

    // Circles and an ellipse.
    for (i, rad) in [3.0f32, 6.5, 12.0, 24.0].iter().enumerate() {
        let mut c = Path::new();
        c.circle(220.0 + i as f32 * 40.0 + rad * 0.5, 60.0, *rad);
        draw(&mut cv, &mut r, &c, &id, FillRule::NonZero, [200, 40, 60]);
    }
    let mut e = Path::new();
    e.ellipse(0.0, 0.0, 50.0, 20.0);
    draw(&mut cv, &mut r, &e, &Transform::rotate(0.5).then_translate(470.0, 70.0), FillRule::NonZero, [30, 150, 80]);

    // Stars: non-zero vs even-odd.
    let star = |cx: f32, cy: f32, rad: f32| {
        let mut s = Path::new();
        let pts: Vec<Point> = (0..5)
            .map(|i| {
                let a = -core::f32::consts::FRAC_PI_2 + i as f32 * 4.0 * core::f32::consts::PI / 5.0;
                let (sn, cs) = libm::sincosf(a);
                Point::new(cx + rad * cs, cy + rad * sn)
            })
            .collect();
        s.polygon(&pts);
        s
    };
    draw(&mut cv, &mut r, &star(80.0, 200.0, 60.0), &id, FillRule::NonZero, [120, 60, 200]);
    draw(&mut cv, &mut r, &star(220.0, 200.0, 60.0), &id, FillRule::EvenOdd, [120, 60, 200]);

    // Pie and arc.
    let mut pie = Path::new();
    pie.pie(350.0, 200.0, 55.0, -0.3, 4.5);
    draw(&mut cv, &mut r, &pie, &id, FillRule::NonZero, [240, 160, 20]);

    // Strokes with joins and caps.
    let zig = [Point::new(430.0, 250.0), Point::new(470.0, 160.0), Point::new(510.0, 250.0), Point::new(560.0, 170.0)];
    for (i, (join, cap)) in
        [(LineJoin::Miter, LineCap::Butt), (LineJoin::Round, LineCap::Round), (LineJoin::Bevel, LineCap::Square)]
            .iter()
            .enumerate()
    {
        let mut z = Path::new();
        let off = i as f32 * 25.0;
        let pts: Vec<Point> =
            zig.iter().map(|p| Point::new(p.x - 20.0 + off * 0.3, p.y + off * 0.0 + i as f32 * 4.0)).collect();
        z.polyline(&pts);
        let style = StrokeStyle::new(12.0 - i as f32 * 4.0).with_join(*join).with_cap(*cap);
        let colors = [[60, 60, 60], [220, 60, 60], [60, 160, 220]];
        r.stroke(&z, &style, &Transform::translate(0.0, i as f32 * 0.0), (cv.width, cv.height), |s| {
            cv.span(&s, colors[i])
        });
    }
    // Thin hairlines at various angles.
    for i in 0..24 {
        let a = i as f32 * core::f32::consts::PI / 24.0;
        let (sn, cs) = libm::sincosf(a);
        let mut l = Path::new();
        l.line(120.0 + 20.0 * cs, 340.0 + 20.0 * sn, 120.0 + 70.0 * cs, 340.0 + 70.0 * sn);
        r.stroke(&l, &StrokeStyle::new(1.0), &id, (cv.width, cv.height), |s| cv.span(&s, [0, 0, 0]));
    }
    // Stroked curves: a rounded rect outline, a circle outline and a wavy cubic.
    let mut rr = Path::new();
    rr.rounded_rect(230.5, 300.5, 120.0, 70.0, [14.0; 4]);
    r.stroke(&rr, &StrokeStyle::new(1.0), &id, (cv.width, cv.height), |s| cv.span(&s, [20, 20, 20]));
    let mut ring = Path::new();
    ring.circle(420.0, 340.0, 40.0);
    r.stroke(&ring, &StrokeStyle::new(6.0), &id, (cv.width, cv.height), |s| cv.span(&s, [30, 150, 80]));
    let mut wave = Path::new();
    wave.move_to(480.0, 380.0);
    wave.cubic_to(510.0, 280.0, 560.0, 420.0, 620.0, 300.0);
    let st = StrokeStyle::new(5.0).with_cap(LineCap::Round);
    r.stroke(&wave, &st, &id, (cv.width, cv.height), |s| cv.span(&s, [200, 40, 160]));
    // Shapes crossing the canvas border must clip cleanly.
    let mut edge = Path::new();
    edge.circle(0.0, 420.0, 50.0);
    edge.circle(640.0, 0.0, 30.0);
    draw(&mut cv, &mut r, &edge, &id, FillRule::NonZero, [100, 100, 100]);

    cv.save("raster_shapes.png");
    cv.zoomed(20, 20, 160, 100, 4).save("raster_shapes_zoom.png");
}

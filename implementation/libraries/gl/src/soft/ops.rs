// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Operations outside the pipeline: copies between images, copies from
//! the framebuffer, blits (OpenGL ES 3.0 section 4.3.3), mipmap generation
//! and transform feedback's writes.

use alloc::vec::Vec;

use super::SoftBackend;
use super::program::SoftProgram;
use super::resource::{Level, Resource};
use crate::backend::{Blit, BufferRange, Filter, Framebuffer, Rect, Region, ResourceId, Surface};
use crate::format::{Class, Format, Geometry, Texel, downsample};
use crate::pixels;

/// Copies a box between images of the same format.
pub fn copy_region(
    b: &mut SoftBackend,
    src: ResourceId,
    src_level: u32,
    region: Region,
    dst: ResourceId,
    dst_level: u32,
    at: [u32; 3],
) {
    let Some(s) = b.res(src) else { return };
    let Some((_, r)) = s.clip(src_level, region) else { return };
    let f = s.desc.format;
    let row = r.w as usize * f.bytes();
    let image = row * r.h as usize;
    let Some(mut tmp) = pixels::try_zeroed(image * r.d as usize) else { return };
    s.read(src_level, r, &mut tmp, row, image);
    if let Some(d) = b.res_mut(dst)
        && d.desc.format == f
    {
        d.write(dst_level, Region::new(at[0], at[1], at[2], r.w, r.h, r.d), &tmp, row, image);
    }
}

/// Reads a rectangle of a framebuffer's color attachment `index`
/// (multisampled pixels resolved) as its format's texels.
fn read_rect(b: &SoftBackend, fb: &Framebuffer, index: u8, rect: Rect) -> Option<(Format, Vec<u8>)> {
    let s = fb.colors.get(index as usize).copied().flatten()?;
    let r = b.res(s.resource)?;
    let f = r.desc.format;
    let row = rect.w as usize * f.bytes();
    let mut out = pixels::try_zeroed(row * rect.h as usize)?;
    r.read(
        s.level,
        Region::new(rect.x as u32, rect.y as u32, s.layer, rect.w as u32, rect.h as u32, 1),
        &mut out,
        row,
        row * rect.h as usize,
    );
    Some((f, out))
}

/// `CopyTexSubImage`: a rectangle of the read buffer to a texture image.
pub fn copy_to_texture(b: &mut SoftBackend, src: &Framebuffer, read: u8, rect: Rect, dst: Surface, x: u32, y: u32) {
    if rect.w <= 0 || rect.h <= 0 {
        return;
    }
    let Some((sf, texels)) = read_rect(b, src, read, rect) else { return };
    let Some(d) = b.res_mut(dst.resource) else { return };
    let df = d.desc.format;
    let (w, h) = (rect.w as u32, rect.h as u32);
    let data = if sf == df {
        texels
    } else {
        // Convert, values moving as they are stored (sRGB stays encoded).
        let Some(mut out) = pixels::try_zeroed(w as usize * h as usize * df.bytes()) else { return };
        for (o, t) in out.chunks_exact_mut(df.bytes()).zip(texels.chunks_exact(sf.bytes())) {
            pixels::raw(df).encode(&pixels::decode_raw(sf, t), o);
        }
        out
    };
    let row = w as usize * df.bytes();
    d.write(dst.level, Region::new(x, y, dst.layer, w, h, 1), &data, row, row * h as usize);
}

/// The texel of a resource level at `(x, y)` of layer `z`, resolved and
/// decoded (sRGB linearized).
fn texel(r: &Resource, level: u32, x: u32, y: u32, z: u32) -> Texel {
    let f = r.desc.format;
    let lv = r.levels[level as usize];
    let o = lv.at(x, y, z);
    let bytes = &r.data[o..o + lv.texel];
    if r.samples > 1 {
        let mut one = [0u8; 16];
        super::resource::resolve(f, bytes, &mut one[..f.bytes()]);
        f.decode(&one[..f.bytes()])
    } else {
        f.decode(&bytes[..f.bytes()])
    }
}

/// `BlitFramebuffer`.
pub fn blit(b: &mut SoftBackend, blit: &Blit) {
    let [sx0, sy0, sx1, sy1] = blit.src_rect.map(|v| v as f32);
    let [dx0, dy0, dx1, dy1] = blit.dst_rect;
    // Destination pixels: the rectangle (either corner first) clipped to
    // the framebuffer and the scissor.
    let (mut x0, mut x1) = (dx0.min(dx1), dx0.max(dx1));
    let (mut y0, mut y1) = (dy0.min(dy1), dy0.max(dy1));
    if x0 == x1 || y0 == y1 || sx0 == sx1 || sy0 == sy1 {
        return;
    }
    let dst = &blit.dst;
    x0 = x0.max(0);
    y0 = y0.max(0);
    x1 = x1.min(dst.width as i32);
    y1 = y1.min(dst.height as i32);
    if let Some(s) = blit.scissor {
        x0 = x0.max(s.x);
        y0 = y0.max(s.y);
        x1 = x1.min(s.x.saturating_add(s.w));
        y1 = y1.min(s.y.saturating_add(s.h));
    }
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    // Source position of a destination pixel centre (mirroring included).
    let (dw, dh) = ((i64::from(dx1) - i64::from(dx0)) as f32, (i64::from(dy1) - i64::from(dy0)) as f32);
    let sx = |x: i32| sx0 + ((x as f32 + 0.5) - dx0 as f32) * (sx1 - sx0) / dw;
    let sy = |y: i32| sy0 + ((y as f32 + 0.5) - dy0 as f32) * (sy1 - sy0) / dh;
    let src = &blit.src;
    // The source's size: the framebuffer's, within each image read.
    let size = |lv: &Level| (src.width.min(lv.width) as i32, src.height.min(lv.height) as i32);
    // Read the whole source first: source and destination may share a
    // resource (other levels or layers).
    let mut writes: Vec<(Surface, u32, u32, Vec<u8>)> = Vec::new();
    if blit.color
        && let Some(ss) = blit.src_color.and_then(|i| src.colors[i as usize])
    {
        let Some(sr) = b.res(ss.resource) else { return };
        let sf = sr.desc.format;
        let integer = sf.is_integer();
        let Some(slv) = sr.level(ss.level).copied() else { return };
        let (sw, sh) = size(&slv);
        for att in dst.draw_buffers.iter().flatten() {
            let Some(ds) = dst.colors[*att as usize] else { continue };
            let Some(dr) = b.res(ds.resource) else { continue };
            let df = dr.desc.format;
            // Start from the destination: pixels whose source is outside
            // the read buffer keep their values.
            let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
            let row = w as usize * df.bytes();
            let Some(mut buf) = pixels::try_zeroed(row * h as usize) else { continue };
            dr.read(ds.level, Region::new(x0 as u32, y0 as u32, ds.layer, w, h, 1), &mut buf, row, row * h as usize);
            // Nearest texels of the same format move as they are (resolved
            // if multisampled): decoding and encoding them would give the
            // same bytes. Unscaled and unmirrored, a row at a time.
            if (blit.filter == Filter::Nearest || integer) && sf == df && copies_exactly(sf) {
                let tb = sf.bytes();
                // Coordinates small enough that `sx` is exact: the source
                // column of `x` is `x - dx0 + sx0` when unscaled.
                let small = blit.src_rect.iter().chain(&blit.dst_rect).all(|v| v.unsigned_abs() <= 1 << 20);
                let [ax0, ay0, ax1, ay1] = blit.src_rect;
                let unscaled = small && ax1 - ax0 == dx1 - dx0 && ay1 - ay0 == dy1 - dy0;
                for y in y0..y1 {
                    let j = libm::floorf(sy(y)) as i32;
                    if j < 0 || j >= sh {
                        continue;
                    }
                    let out = &mut buf[(y - y0) as usize * row..][..row];
                    if unscaled && sr.samples == 1 {
                        // Source columns `x - dx0 + sx0`, those inside it.
                        let shift = blit.src_rect[0] - dx0;
                        let (a, b) = ((x0 + shift).max(0), (x1 + shift).min(sw));
                        if a < b {
                            let from = slv.at(a as u32, j as u32, ss.layer);
                            let n = (b - a) as usize * tb;
                            out[(a - shift - x0) as usize * tb..][..n].copy_from_slice(&sr.data[from..from + n]);
                        }
                        continue;
                    }
                    for x in x0..x1 {
                        let i = libm::floorf(sx(x)) as i32;
                        if i < 0 || i >= sw {
                            continue;
                        }
                        let from = &sr.data[slv.at(i as u32, j as u32, ss.layer)..][..slv.texel];
                        let to = &mut out[(x - x0) as usize * tb..][..tb];
                        if sr.samples > 1 {
                            super::resource::resolve(sf, from, to);
                        } else {
                            to.copy_from_slice(from);
                        }
                    }
                }
                writes.push((ds, w, h, buf));
                continue;
            }
            for y in y0..y1 {
                for x in x0..x1 {
                    let (fx, fy) = (sx(x), sy(y));
                    let t = if blit.filter == Filter::Linear && !integer {
                        // Bilinear, clamped to the source's edges.
                        let (u, v) = (fx - 0.5, fy - 0.5);
                        let (i0, j0) = (libm::floorf(u) as i32, libm::floorf(v) as i32);
                        let (a, c) = (u - i0 as f32, v - j0 as f32);
                        let at = |i: i32, j: i32| {
                            let (i, j) = (i.clamp(0, sw - 1) as u32, j.clamp(0, sh - 1) as u32);
                            texel(sr, ss.level, i, j, ss.layer).as_float()
                        };
                        let l = |p: [f32; 4], q: [f32; 4], t: f32| [0, 1, 2, 3].map(|k| p[k] + (q[k] - p[k]) * t);
                        Texel::Float(l(l(at(i0, j0), at(i0 + 1, j0), a), l(at(i0, j0 + 1), at(i0 + 1, j0 + 1), a), c))
                    } else {
                        let (i, j) = (libm::floorf(fx) as i32, libm::floorf(fy) as i32);
                        if i < 0 || j < 0 || i >= sw || j >= sh {
                            // Outside the source: the destination keeps
                            // its value.
                            continue;
                        }
                        texel(sr, ss.level, i as u32, j as u32, ss.layer)
                    };
                    let o = (((y - y0) * (x1 - x0) + (x - x0)) as usize) * df.bytes();
                    df.encode(&t, &mut buf[o..o + df.bytes()]);
                }
            }
            writes.push((ds, (x1 - x0) as u32, (y1 - y0) as u32, buf));
        }
    }
    for (on, s, d) in [(blit.depth, src.depth, dst.depth), (blit.stencil, src.stencil, dst.stencil)] {
        let (true, Some(ss), Some(ds)) = (on, s, d) else { continue };
        if writes.iter().any(|w| w.0 == ds) {
            continue;
        }
        let Some(sr) = b.res(ss.resource) else { continue };
        let Some(slv) = sr.level(ss.level).copied() else { continue };
        let (sw, sh) = size(&slv);
        let f = sr.desc.format;
        // Depth and stencil copy whole texels (both blits use the same
        // nearest mapping; the formats match).
        let Some(mut buf) = pixels::try_zeroed(((x1 - x0) * (y1 - y0)) as usize * f.bytes()) else { continue };
        let Some(dr) = b.res(ds.resource) else { continue };
        let Some(lv) = dr.level(ds.level).copied().filter(|lv| x1 as u32 <= lv.width && y1 as u32 <= lv.height) else {
            continue;
        };
        for y in y0..y1 {
            for x in x0..x1 {
                let (i, j) = (libm::floorf(sx(x)) as i32, libm::floorf(sy(y)) as i32);
                let o = (((y - y0) * (x1 - x0) + (x - x0)) as usize) * f.bytes();
                let (si, sj) = if i < 0 || j < 0 || i >= sw || j >= sh {
                    // Keep the destination's texel.
                    let p = lv.at(x as u32, y as u32, ds.layer);
                    buf[o..o + f.bytes()].copy_from_slice(&dr.data[p..p + f.bytes()]);
                    continue;
                } else {
                    (i as u32, j as u32)
                };
                let p = slv.at(si, sj, ss.layer);
                buf[o..o + f.bytes()].copy_from_slice(&sr.data[p..p + f.bytes()]);
            }
        }
        // Depth and stencil share a texel: merge what the blit does not
        // copy from the destination.
        let merge = !(blit.depth && blit.stencil) && f.has_depth() && f.has_stencil();
        if merge {
            for y in y0..y1 {
                for x in x0..x1 {
                    let o = (((y - y0) * (x1 - x0) + (x - x0)) as usize) * f.bytes();
                    let p = lv.at(x as u32, y as u32, ds.layer);
                    let old = f.decode(&dr.data[p..p + f.bytes()]);
                    let new = f.decode(&buf[o..o + f.bytes()]);
                    let t = if blit.depth {
                        Texel::Depth(new.depth(), old.stencil())
                    } else {
                        Texel::Depth(old.depth(), new.stencil())
                    };
                    f.encode(&t, &mut buf[o..o + f.bytes()]);
                }
            }
        }
        writes.push((ds, (x1 - x0) as u32, (y1 - y0) as u32, buf));
    }
    for (s, w, h, buf) in writes {
        if let Some(r) = b.res_mut(s.resource) {
            let row = w as usize * r.desc.format.bytes();
            r.write(s.level, Region::new(x0 as u32, y0 as u32, s.layer, w, h, 1), &buf, row, row * h as usize);
        }
    }
}

/// Whether texels of `f` come out of decoding and encoding as they went in,
/// so that a blit may move them as they are: not where encoding rounds
/// (half and packed floats, the clamped -128 of signed normalized formats)
/// or fills an unused component (`X` of RGBX formats).
fn copies_exactly(f: Format) -> bool {
    use Format::*;
    match f.class() {
        Class::Uint | Class::Sint => {
            !matches!(f, Rgbx8Uint | Rgbx8Sint | Rgbx16Uint | Rgbx16Sint | Rgbx32Uint | Rgbx32Sint)
        }
        Class::Unorm => !matches!(f, Rgbx8Unorm | Rgbx8Srgb),
        Class::Float => matches!(f, R32Float | Rg32Float | Rgba32Float),
        _ => false,
    }
}

/// Fills levels `base + 1..=last` of a texture with box-filtered
/// reductions of the level above (color formats; sRGB averaged in linear
/// space).
pub fn generate_mipmap(b: &mut SoftBackend, id: ResourceId, base: u32, last: u32) {
    let Some(r) = b.res_mut(id) else { return };
    let f = r.desc.format;
    if matches!(f.class(), Class::Depth | Class::DepthStencil | Class::Stencil) || f.is_integer() || r.samples > 1 {
        return;
    }
    let volume = r.desc.target == crate::backend::Target::Texture3D;
    let geometry =
        |v: &Level| Geometry { width: v.width, height: v.height, depth: v.layers, row: v.row, image: v.image };
    for l in base + 1..=last.min(r.levels.len() as u32 - 1) {
        let (src, dst) = (r.levels[l as usize - 1], r.levels[l as usize]);
        // Each level follows the one above it in memory.
        let (above, below) = r.data.split_at_mut(dst.offset);
        let to = &mut below[..dst.bytes()];
        downsample(f, &above[src.offset..], geometry(&src), to, geometry(&dst), volume);
    }
}

/// Writes the components transform feedback recorded (`recorded`: each
/// vertex's captured components in the program's order) to the buffers'
/// ranges.
pub fn write_feedback(b: &mut SoftBackend, p: &SoftProgram, ranges: &[BufferRange], recorded: &[u32]) {
    for (bi, range) in ranges.iter().enumerate() {
        let Some(data) = super::vertex::feedback_bytes(p, bi, range.size, recorded) else { continue };
        b.flush_if_uses(range.buffer);
        if let Some(r) = b.res_mut(range.buffer) {
            let n = data.len();
            r.write(0, Region::bytes(range.offset, n), &data, n, n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Backend, ResourceDesc, Target};
    use crate::soft::Serial;
    use alloc::boxed::Box;
    use alloc::vec;

    /// A blit's result, pixel by pixel as OpenGL ES defines nearest
    /// filtering (and as the general path computes it): the source texel
    /// under each destination pixel's centre, if there is one.
    fn reference(src: &[u8], (sw, sh): (i32, i32), dst: &mut [u8], dw: i32, s: [i32; 4], d: [i32; 4]) {
        let [sx0, sy0, sx1, sy1] = s.map(|v| v as f32);
        let [dx0, dy0, dx1, dy1] = d;
        let (w, h) = ((i64::from(dx1) - i64::from(dx0)) as f32, (i64::from(dy1) - i64::from(dy0)) as f32);
        for y in dy0.min(dy1).max(0)..dy0.max(dy1).min(dst.len() as i32 / 4 / dw) {
            for x in dx0.min(dx1).max(0)..dx0.max(dx1).min(dw) {
                let fx = sx0 + ((x as f32 + 0.5) - dx0 as f32) * (sx1 - sx0) / w;
                let fy = sy0 + ((y as f32 + 0.5) - dy0 as f32) * (sy1 - sy0) / h;
                let (i, j) = (libm::floorf(fx) as i32, libm::floorf(fy) as i32);
                if i >= 0 && j >= 0 && i < sw && j < sh {
                    let (o, p) = (((y * dw + x) * 4) as usize, ((j * sw + i) * 4) as usize);
                    dst[o..o + 4].copy_from_slice(&src[p..p + 4]);
                }
            }
        }
    }

    #[test]
    fn texels_copied_as_they_are_land_where_nearest_filtering_puts_them() {
        let mut b = SoftBackend::new(Box::new(Serial));
        let image = |b: &mut SoftBackend, w, h| {
            let desc = ResourceDesc {
                target: Target::Texture2D,
                format: Format::Rgba8Unorm,
                width: w,
                height: h,
                depth: 1,
                levels: 1,
                samples: 0,
            };
            b.create_resource(&desc).unwrap()
        };
        let (sw, sh, dw, dh) = (13, 9, 11, 10);
        let (s, d) = (image(&mut b, sw as u32, sh as u32), image(&mut b, dw as u32, dh as u32));
        let texels: Vec<u8> = (0..sw * sh * 4).map(|k| (k * 37 % 251) as u8).collect();
        b.write(s, 0, Region::new(0, 0, 0, sw as u32, sh as u32, 1), &texels, sw as usize * 4, texels.len());
        let fb = |id, width, height| {
            let mut f = Framebuffer { width, height, ..Framebuffer::default() };
            f.colors[0] = Some(Surface { resource: id, level: 0, layer: 0 });
            f.draw_buffers[0] = Some(0);
            f
        };
        let rects = [
            ([0, 0, 13, 9], [0, 0, 13, 9]),
            ([2, 1, 9, 8], [-3, 2, 4, 9]),
            ([-4, -2, 20, 12], [1, 1, 25, 15]),
            ([9, 8, 2, 1], [0, 0, 7, 7]),
            ([0, 0, 13, 9], [10, 9, 0, 0]),
            ([1, 1, 4, 4], [0, 0, 11, 10]),
            ([0, 0, 12, 8], [3, 2, 6, 4]),
        ];
        for (src_rect, dst_rect) in rects {
            let zero = vec![0u8; (dw * dh * 4) as usize];
            b.write(d, 0, Region::new(0, 0, 0, dw as u32, dh as u32, 1), &zero, dw as usize * 4, zero.len());
            let blit = Blit {
                src: fb(s, sw as u32, sh as u32),
                src_color: Some(0),
                dst: fb(d, dw as u32, dh as u32),
                src_rect,
                dst_rect,
                color: true,
                depth: false,
                stencil: false,
                filter: Filter::Nearest,
                scissor: None,
            };
            super::blit(&mut b, &blit);
            let mut got = vec![0u8; zero.len()];
            b.read(d, 0, Region::new(0, 0, 0, dw as u32, dh as u32, 1), &mut got, dw as usize * 4, zero.len());
            let mut want = zero.clone();
            reference(&texels, (sw, sh), &mut want, dw, src_rect, dst_rect);
            assert_eq!(got, want, "{src_rect:?} to {dst_rect:?}");
        }
    }
}

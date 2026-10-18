// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Operations on a framebuffer's pixels: clearing its buffers, reading its
//! pixels (`ReadPixels`) and blitting between framebuffers
//! (`BlitFramebuffer`).

use super::{ReadColor, natural_read};
use crate::backend::{Blit, Clear, Filter, Rect, Region};
use crate::context::{Context, DRAW_BUFFERS};
use crate::format::{self, Class, Client, Format, Internal, clamp01};
use crate::gl;
use crate::pixels::{self, PixelsMut};

/// The required `ReadPixels` `(format, type)` for a buffer's class.
fn required_read(i: &Internal) -> (u32, u32) {
    match i.format.class() {
        Class::Uint => (gl::RGBA_INTEGER, gl::UNSIGNED_INT),
        Class::Sint => (gl::RGBA_INTEGER, gl::INT),
        Class::Float => (gl::RGBA, gl::FLOAT),
        _ => (gl::RGBA, gl::UNSIGNED_BYTE),
    }
}

/// The value a color attachment is cleared to (32-bit components as the
/// format's class reads them).
fn color_bits(f: Format, c: [f32; 4]) -> [u32; 4] {
    match f.class() {
        Class::Uint => c.map(|x| libm::roundf(x.max(0.0)) as u32),
        Class::Sint => c.map(|x| libm::roundf(x) as i32 as u32),
        _ => c.map(f32::to_bits),
    }
}

/// The largest coordinate the renderers' blits take: their arithmetic on
/// rectangles cannot overflow, and is exact in 32-bit floats.
const BLIT_RANGE: f64 = (1 << 24) as f64;

/// A blit's rectangles (x0, y0, x1, y1, either corner first) as the
/// renderers take them, for framebuffers of sizes `src` and `dst`: as given
/// if every coordinate is within [`BLIT_RANGE`]; otherwise each is clipped
/// to its framebuffer, the other following at the blit's scale (out to
/// whole pixels), as GPU drivers clip. `None` if nothing is left.
fn blit_rects(
    src: [i32; 4],
    dst: [i32; 4],
    src_size: (u32, u32),
    dst_size: (u32, u32),
) -> Option<([i32; 4], [i32; 4])> {
    if src.iter().chain(&dst).all(|&v| f64::from(v).abs() <= BLIT_RANGE) {
        return Some((src, dst));
    }
    let (mut s, mut d) = (src.map(f64::from), dst.map(f64::from));
    // Clips `a`'s ends on an axis to `0..size`, moving `b`'s with them.
    let clip = |a: &mut [f64; 4], b: &mut [f64; 4], axis: usize, size: u32| {
        let (a0, a1, b0, b1) = (a[axis], a[axis + 2], b[axis], b[axis + 2]);
        let (lo, hi) = (a0.min(a1).max(0.0), a0.max(a1).min(f64::from(size)));
        if a0 == a1 || lo >= hi {
            return false;
        }
        let scale = (b1 - b0) / (a1 - a0);
        let (n0, n1) = if a0 < a1 { (lo, hi) } else { (hi, lo) };
        [a[axis], a[axis + 2]] = [n0, n1];
        [b[axis], b[axis + 2]] = [b0 + (n0 - a0) * scale, b0 + (n1 - a0) * scale];
        true
    };
    for (axis, ds, ss) in [(0, dst_size.0, src_size.0), (1, dst_size.1, src_size.1)] {
        if !clip(&mut d, &mut s, axis, ds) || !clip(&mut s, &mut d, axis, ss) {
            return None;
        }
    }
    // Out to whole pixels, keeping each rectangle's direction.
    let whole = |r: [f64; 4]| {
        let mut out = [0; 4];
        for axis in 0..2 {
            let (a, b) = (r[axis], r[axis + 2]);
            let lo = libm::floor(a.min(b).clamp(-BLIT_RANGE, BLIT_RANGE)) as i32;
            let hi = (libm::ceil(a.max(b).clamp(-BLIT_RANGE, BLIT_RANGE)) as i32).max(lo + 1);
            [out[axis], out[axis + 2]] = if a <= b { [lo, hi] } else { [hi, lo] };
        }
        out
    };
    Some((whole(s), whole(d)))
}

impl Context {
    // ---- Clearing ---------------------------------------------------------------------

    fn scissor_rect(&self) -> Option<Rect> {
        let s = self.local.state.scissor;
        self.local.state.scissor_test.then_some(Rect { x: s[0], y: s[1], w: s[2], h: s[3] })
    }

    /// `glClear`.
    pub fn clear(&mut self, mask: u32) {
        if mask & !(gl::COLOR_BUFFER_BIT | gl::DEPTH_BUFFER_BIT | gl::STENCIL_BUFFER_BIT) != 0 {
            return self.err(gl::INVALID_VALUE);
        }
        let fb = match self.draw_state_framebuffer() {
            Ok(fb) => fb,
            Err(_) => return self.err(gl::INVALID_FRAMEBUFFER_OPERATION),
        };
        if self.local.state.rasterizer_discard {
            return;
        }
        let mut c = Clear {
            scissor: self.scissor_rect(),
            color_mask: self.local.state.color_mask,
            stencil_mask: self.local.state.stencil_front.write_mask,
            ..Default::default()
        };
        if mask & gl::COLOR_BUFFER_BIT != 0 {
            for i in 0..DRAW_BUFFERS {
                let Some(att) = fb.draw_buffers[i] else { continue };
                let Some(internal) = self.color_internal(self.local.draw_framebuffer, att as usize) else { continue };
                // Clearing integer buffers with Clear is undefined; leave
                // them alone.
                if !internal.format.is_integer() && fb.colors[att as usize].is_some() {
                    c.colors[att as usize] = Some(color_bits(internal.format, self.local.state.clear_color));
                }
            }
        }
        if mask & gl::DEPTH_BUFFER_BIT != 0 && self.local.state.depth_write && fb.depth.is_some() {
            c.depth = Some(self.local.state.clear_depth);
        }
        if mask & gl::STENCIL_BUFFER_BIT != 0 && fb.stencil.is_some() {
            c.stencil = Some(self.local.state.clear_stencil);
        }
        if c.colors.iter().any(Option::is_some) || c.depth.is_some() || c.stencil.is_some() {
            self.backend.clear(&fb, &c);
        }
    }

    /// `glClearBuffer*`: `kind` is the scalar class the call passes
    /// (`Float`, `Sint`, `Uint`).
    fn clear_buffer(
        &mut self,
        buffer: u32,
        drawbuffer: i32,
        value: [u32; 4],
        kind: Class,
        depth: Option<f32>,
        stencil: Option<i32>,
    ) {
        let fb = match self.draw_state_framebuffer() {
            Ok(fb) => fb,
            Err(_) => return self.err(gl::INVALID_FRAMEBUFFER_OPERATION),
        };
        if self.local.state.rasterizer_discard {
            return;
        }
        let mut c = Clear {
            scissor: self.scissor_rect(),
            color_mask: self.local.state.color_mask,
            stencil_mask: self.local.state.stencil_front.write_mask,
            ..Default::default()
        };
        match buffer {
            gl::COLOR => {
                let Some(att) = fb.draw_buffers[drawbuffer as usize] else { return };
                let Some(internal) = self.color_internal(self.local.draw_framebuffer, att as usize) else { return };
                let class = match internal.format.class() {
                    Class::Uint => Class::Uint,
                    Class::Sint => Class::Sint,
                    _ => Class::Float,
                };
                // A value of another kind than the buffer's is undefined:
                // nothing is written.
                if class != kind || fb.colors[att as usize].is_none() {
                    return;
                }
                c.colors[att as usize] = Some(value);
            }
            _ => {
                c.depth = depth.filter(|_| fb.depth.is_some() && self.local.state.depth_write).map(clamp01);
                c.stencil = stencil.filter(|_| fb.stencil.is_some());
            }
        }
        self.backend.clear(&fb, &c);
    }

    /// `glClearBufferfv`: `COLOR` (four values) or `DEPTH` (one).
    pub fn clear_bufferfv(&mut self, buffer: u32, drawbuffer: i32, value: &[f32]) {
        match buffer {
            gl::COLOR if (0..DRAW_BUFFERS as i32).contains(&drawbuffer) && value.len() >= 4 => {
                let v = [value[0], value[1], value[2], value[3]].map(f32::to_bits);
                self.clear_buffer(buffer, drawbuffer, v, Class::Float, None, None);
            }
            gl::DEPTH if drawbuffer == 0 && !value.is_empty() => {
                self.clear_buffer(buffer, 0, [0; 4], Class::Float, Some(value[0]), None);
            }
            gl::COLOR | gl::DEPTH => self.err(gl::INVALID_VALUE),
            _ => self.err(gl::INVALID_ENUM),
        }
    }

    /// `glClearBufferiv`: `COLOR` (four values) or `STENCIL` (one).
    pub fn clear_bufferiv(&mut self, buffer: u32, drawbuffer: i32, value: &[i32]) {
        match buffer {
            gl::COLOR if (0..DRAW_BUFFERS as i32).contains(&drawbuffer) && value.len() >= 4 => {
                let v = [value[0], value[1], value[2], value[3]].map(|x| x as u32);
                self.clear_buffer(buffer, drawbuffer, v, Class::Sint, None, None);
            }
            gl::STENCIL if drawbuffer == 0 && !value.is_empty() => {
                self.clear_buffer(buffer, 0, [0; 4], Class::Sint, None, Some(value[0]));
            }
            gl::COLOR | gl::STENCIL => self.err(gl::INVALID_VALUE),
            _ => self.err(gl::INVALID_ENUM),
        }
    }

    /// `glClearBufferuiv`: `COLOR` only.
    pub fn clear_bufferuiv(&mut self, buffer: u32, drawbuffer: i32, value: &[u32]) {
        match buffer {
            gl::COLOR if (0..DRAW_BUFFERS as i32).contains(&drawbuffer) && value.len() >= 4 => {
                let v = [value[0], value[1], value[2], value[3]];
                self.clear_buffer(buffer, drawbuffer, v, Class::Uint, None, None);
            }
            gl::COLOR => self.err(gl::INVALID_VALUE),
            _ => self.err(gl::INVALID_ENUM),
        }
    }

    /// `glClearBufferfi`: `DEPTH_STENCIL`.
    pub fn clear_bufferfi(&mut self, buffer: u32, drawbuffer: i32, depth: f32, stencil: i32) {
        if buffer != gl::DEPTH_STENCIL {
            return self.err(gl::INVALID_ENUM);
        }
        if drawbuffer != 0 {
            return self.err(gl::INVALID_VALUE);
        }
        self.clear_buffer(buffer, 0, [0; 4], Class::Float, Some(depth), Some(stencil));
    }

    // ---- Reading pixels ------------------------------------------------------------------

    /// The bytes from where `ReadPixels`' data starts to its last pixel's
    /// end, under the pack storage modes: what a pointer to them must hold.
    /// `None` if they take no layout (the read fails then, as it says).
    pub fn pack_size(&self, width: i32, height: i32, format: u32, ty: u32) -> Option<usize> {
        let (Ok(w), Ok(h)) = (u32::try_from(width), u32::try_from(height)) else { return None };
        if !format::is_format(format) || !format::is_type(ty) {
            return None;
        }
        pixels::layout(Client::new(format, ty), w, h, 1, &self.local.state.pack).map(|l| l.end)
    }

    /// `glReadPixels`.
    pub fn read_pixels<'a>(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        format: u32,
        ty: u32,
        out: impl Into<PixelsMut<'a>>,
    ) {
        let out = out.into();
        if !format::is_format(format) || !format::is_type(ty) {
            return self.err(gl::INVALID_ENUM);
        }
        if width < 0 || height < 0 {
            return self.err(gl::INVALID_VALUE);
        }
        let src = match self.read_color() {
            Ok(s) => s,
            Err(e) => return self.err(e),
        };
        let pair = (format, ty);
        // EXT_read_format_bgra: what reads as RGBA bytes reads as BGRA too.
        let bgra =
            pair == (gl::BGRA_EXT, gl::UNSIGNED_BYTE) && required_read(&src.internal) == (gl::RGBA, gl::UNSIGNED_BYTE);
        if pair != required_read(&src.internal)
            && pair != natural_read(&src.internal)
            && !bgra
            && !(src.internal.format == Format::Rgb10A2Unorm && pair == (gl::RGBA, gl::UNSIGNED_INT_2_10_10_10_REV))
        {
            return self.err(gl::INVALID_OPERATION);
        }
        let client = Client::new(format, ty);
        let pack = self.local.state.pack;
        let Some(layout) = pixels::layout(client, width as u32, height as u32, 1, &pack) else {
            return self.err(gl::INVALID_VALUE);
        };
        // Where the pixels go: the application's memory or the pack buffer.
        let buffer = self.local.bound.pixel_pack;
        let target = match (out, buffer) {
            (PixelsMut::Data(d), None) => {
                if d.len() < layout.end {
                    return self.err(gl::INVALID_OPERATION);
                }
                Ok(d)
            }
            (PixelsMut::Offset(o), Some(k)) => {
                let (_, size) = self.buffer_store(k);
                let mapped = self.buffers.get(k).map.is_some();
                if mapped
                    || o.checked_add(layout.end).is_none_or(|e| e > size)
                    || !o.is_multiple_of(client.type_bytes())
                {
                    return self.err(gl::INVALID_OPERATION);
                }
                Err((k, o))
            }
            _ => return self.err(gl::INVALID_OPERATION),
        };
        // Only pixels inside the read buffer are read: the rectangle
        // `(x0, y0)` to `(x1, y1)`, `(cw, ch)` large, which starts
        // `(x0 - x, y0 - y)` into the pixels asked for.
        let fb = &src.fb;
        let (x, y) = (i64::from(x), i64::from(y));
        let (x0, y0) = (x.max(0), y.max(0));
        let x1 = (x + i64::from(width)).min(i64::from(fb.width));
        let y1 = (y + i64::from(height)).min(i64::from(fb.height));
        let (cw, ch) = ((x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32);
        let mut sub = layout;
        if cw > 0 && ch > 0 {
            // Inside the pixels asked for (`layout.end` bounds them).
            sub.start += (x0 - x) as usize * layout.pixel + (y0 - y) as usize * layout.row;
            sub.row_bytes = cw as usize * layout.pixel;
        }
        let region = Region::new(x0 as u32, y0 as u32, src.surface.layer, cw, ch, 1);
        match target {
            Ok(d) => {
                if cw > 0 && ch > 0 && !self.read_rect(&src, region, client, &sub, d) {
                    self.err(gl::OUT_OF_MEMORY);
                }
            }
            Err((k, offset)) => {
                // Read-modify-write the range, so that bytes between rows
                // keep their values.
                let Some(mut range) = self.read_buffer_range(k, offset, layout.end) else {
                    return self.err(gl::OUT_OF_MEMORY);
                };
                if cw > 0 && ch > 0 && !self.read_rect(&src, region, client, &sub, &mut range) {
                    return self.err(gl::OUT_OF_MEMORY);
                }
                if let (Some(r), false) = (self.buffers.get(k).resource, range.is_empty()) {
                    let n = range.len();
                    self.backend.write(r, 0, Region::bytes(offset, n), &range, n, n);
                }
            }
        }
    }

    /// Reads `region` of the read buffer `src` into `dst`, as pixels of
    /// `client`'s layout where `layout` puts them: straight in if they are
    /// laid out as the buffer's texels are, otherwise through the texels,
    /// converted as they are packed. `false` if there is no memory to
    /// convert them.
    fn read_rect(
        &mut self,
        src: &ReadColor,
        region: Region,
        client: Client,
        layout: &pixels::Layout,
        dst: &mut [u8],
    ) -> bool {
        let (s, f) = (src.surface, src.internal.format);
        if pixels::same_layout(client, f) {
            let Some(dst) = dst.get_mut(layout.start..) else { return true };
            self.backend.read(s.resource, s.level, region, dst, layout.row, layout.image);
            return true;
        }
        let row = region.w as usize * f.bytes();
        let Some(mut texels) = pixels::try_zeroed(row * region.h as usize) else { return false };
        self.backend.read(s.resource, s.level, region, &mut texels, row, row * region.h as usize);
        pixels::pack(f, &texels, region.w, region.h, client, layout, dst);
        true
    }

    // ---- Blits ------------------------------------------------------------------------------

    /// `glBlitFramebuffer`.
    #[allow(clippy::too_many_arguments)]
    pub fn blit_framebuffer(
        &mut self,
        src_x0: i32,
        src_y0: i32,
        src_x1: i32,
        src_y1: i32,
        dst_x0: i32,
        dst_y0: i32,
        dst_x1: i32,
        dst_y1: i32,
        mask: u32,
        filter: u32,
    ) {
        if mask & !(gl::COLOR_BUFFER_BIT | gl::DEPTH_BUFFER_BIT | gl::STENCIL_BUFFER_BIT) != 0 {
            return self.err(gl::INVALID_VALUE);
        }
        let filter = match filter {
            gl::NEAREST => Filter::Nearest,
            gl::LINEAR => Filter::Linear,
            _ => return self.err(gl::INVALID_ENUM),
        };
        if filter == Filter::Linear && mask & (gl::DEPTH_BUFFER_BIT | gl::STENCIL_BUFFER_BIT) != 0 {
            return self.err(gl::INVALID_OPERATION);
        }
        let (Ok(src), Ok(dst)) = (self.read_state_framebuffer(), self.draw_state_framebuffer()) else {
            return self.err(gl::INVALID_FRAMEBUFFER_OPERATION);
        };
        if dst.samples > 0 {
            return self.err(gl::INVALID_OPERATION);
        }
        let src_rect = [src_x0, src_y0, src_x1, src_y1];
        let dst_rect = [dst_x0, dst_y0, dst_x1, dst_y1];
        // A multisampled framebuffer is resolved only in place: into the
        // same rectangle, corner for corner.
        if src.samples > 0 && src_rect != dst_rect {
            return self.err(gl::INVALID_OPERATION);
        }
        let mut color = mask & gl::COLOR_BUFFER_BIT != 0;
        let mut depth = mask & gl::DEPTH_BUFFER_BIT != 0;
        let mut stencil = mask & gl::STENCIL_BUFFER_BIT != 0;
        let read = src.draw_buffers[0];
        let read_internal = read.and_then(|i| self.color_internal(self.local.read_framebuffer, i as usize));
        if color {
            match (read, read_internal) {
                (Some(ri), Some(rf)) if src.colors[ri as usize].is_some() => {
                    let rclass = rf.format.class();
                    for i in 0..DRAW_BUFFERS {
                        let Some(att) = dst.draw_buffers[i] else { continue };
                        let Some(df) = self.color_internal(self.local.draw_framebuffer, att as usize) else { continue };
                        let dclass = df.format.class();
                        let int = |c: Class| matches!(c, Class::Uint | Class::Sint);
                        if int(rclass) != int(dclass) || (int(rclass) && rclass != dclass) {
                            return self.err(gl::INVALID_OPERATION);
                        }
                        if int(rclass) && filter == Filter::Linear {
                            return self.err(gl::INVALID_OPERATION);
                        }
                        if src.samples > 0 && rf.format != df.format {
                            return self.err(gl::INVALID_OPERATION);
                        }
                        if src.colors[ri as usize] == dst.colors[att as usize] {
                            return self.err(gl::INVALID_OPERATION);
                        }
                    }
                }
                // A buffer missing from either framebuffer is ignored.
                _ => color = false,
            }
        }
        let ds_internal = |c: &Context, read: bool, depth: bool| -> Option<Internal> {
            let fbo = if read { c.local.read_framebuffer } else { c.local.draw_framebuffer };
            match fbo {
                None => c.local.default_fb.depth_internal,
                Some(k) => {
                    let fb = c.local.framebuffers.get(k);
                    c.attachment_internal(if depth { fb.depth } else { fb.stencil })
                }
            }
        };
        for (on, is_depth, s, d) in
            [(&mut depth, true, src.depth, dst.depth), (&mut stencil, false, src.stencil, dst.stencil)]
        {
            if !*on {
                continue;
            }
            if s.is_none() || d.is_none() {
                *on = false;
                continue;
            }
            let (sf, df) = (ds_internal(self, true, is_depth), ds_internal(self, false, is_depth));
            if sf.map(|f| f.format) != df.map(|f| f.format) || s == d {
                return self.err(gl::INVALID_OPERATION);
            }
        }
        if !(color || depth || stencil) {
            return;
        }
        let Some((src_rect, dst_rect)) =
            blit_rects(src_rect, dst_rect, (src.width, src.height), (dst.width, dst.height))
        else {
            return;
        };
        let blit = Blit {
            src,
            src_color: if color { read } else { None },
            dst,
            src_rect,
            dst_rect,
            color,
            depth,
            stencil,
            filter,
            scissor: self.scissor_rect(),
        };
        self.backend.blit(&blit);
    }
}

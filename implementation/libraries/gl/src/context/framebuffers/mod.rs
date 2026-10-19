// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Framebuffer objects (OpenGL ES 3.0 chapter 4): their attachments and
//! completeness, the draw and read buffers, and attachment queries.
//!
//! Renderbuffers are in `renderbuffers`; clearing, reading pixels and blits
//! in `operations`; the default framebuffer a window provides in `default`.

mod default;
mod operations;
mod renderbuffers;

use alloc::vec::Vec;

use super::{Attachment, Context, DRAW_BUFFERS, FramebufferObject, Key};
use crate::backend::{Framebuffer, Surface};
use crate::format::{Class, Format, Internal};
use crate::gl;

/// The read buffer as a copy or `ReadPixels` sees it.
pub(crate) struct ReadColor {
    pub fb: Framebuffer,
    /// The color attachment read.
    pub index: u8,
    pub surface: Surface,
    pub internal: Internal,
}

/// An attachment point of a framebuffer object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Point {
    Color(usize),
    Depth,
    Stencil,
    /// Both depth and stencil (`DEPTH_STENCIL_ATTACHMENT`).
    DepthStencil,
}

/// The attachment point of a framebuffer object `attachment` names, or the
/// error for one that names none: `INVALID_OPERATION` for a color
/// attachment past the last, `INVALID_ENUM` otherwise.
fn attachment_point(attachment: u32) -> Result<Point, u32> {
    match attachment {
        gl::DEPTH_ATTACHMENT => Ok(Point::Depth),
        gl::STENCIL_ATTACHMENT => Ok(Point::Stencil),
        gl::DEPTH_STENCIL_ATTACHMENT => Ok(Point::DepthStencil),
        a if (gl::COLOR_ATTACHMENT0..=gl::COLOR_ATTACHMENT15).contains(&a) => {
            let i = (a - gl::COLOR_ATTACHMENT0) as usize;
            if i < DRAW_BUFFERS { Ok(Point::Color(i)) } else { Err(gl::INVALID_OPERATION) }
        }
        _ => Err(gl::INVALID_ENUM),
    }
}

/// What an attachment point holds, resolved: the image and its format.
#[derive(Clone, Copy)]
struct Resolved {
    surface: Surface,
    internal: Internal,
    width: u32,
    height: u32,
    samples: u32,
}

/// The `(format, type)` `ReadPixels` takes for a buffer besides the
/// required one (`IMPLEMENTATION_COLOR_READ_FORMAT`/`_TYPE`): the buffer's
/// own layout.
pub(crate) fn natural_read(i: &Internal) -> (u32, u32) {
    use Format as F;
    match i.format {
        F::R8Unorm => (gl::RED, gl::UNSIGNED_BYTE),
        F::Rg8Unorm => (gl::RG, gl::UNSIGNED_BYTE),
        F::Rgbx8Unorm | F::Rgbx8Srgb => (gl::RGB, gl::UNSIGNED_BYTE),
        F::B5G6R5Unorm => (gl::RGB, gl::UNSIGNED_SHORT_5_6_5),
        F::Rgba4Unorm => (gl::RGBA, gl::UNSIGNED_SHORT_4_4_4_4),
        F::Rgb5A1Unorm => (gl::RGBA, gl::UNSIGNED_SHORT_5_5_5_1),
        F::Rgb10A2Unorm => (gl::RGBA, gl::UNSIGNED_INT_2_10_10_10_REV),
        F::Rgb10A2Uint => (gl::RGBA_INTEGER, gl::UNSIGNED_INT_2_10_10_10_REV),
        F::R8Uint => (gl::RED_INTEGER, gl::UNSIGNED_BYTE),
        F::Rg8Uint => (gl::RG_INTEGER, gl::UNSIGNED_BYTE),
        F::Rgba8Uint => (gl::RGBA_INTEGER, gl::UNSIGNED_BYTE),
        F::R8Sint => (gl::RED_INTEGER, gl::BYTE),
        F::Rg8Sint => (gl::RG_INTEGER, gl::BYTE),
        F::Rgba8Sint => (gl::RGBA_INTEGER, gl::BYTE),
        F::R16Uint => (gl::RED_INTEGER, gl::UNSIGNED_SHORT),
        F::Rg16Uint => (gl::RG_INTEGER, gl::UNSIGNED_SHORT),
        F::Rgba16Uint => (gl::RGBA_INTEGER, gl::UNSIGNED_SHORT),
        F::R16Sint => (gl::RED_INTEGER, gl::SHORT),
        F::Rg16Sint => (gl::RG_INTEGER, gl::SHORT),
        F::Rgba16Sint => (gl::RGBA_INTEGER, gl::SHORT),
        F::R32Uint => (gl::RED_INTEGER, gl::UNSIGNED_INT),
        F::Rg32Uint => (gl::RG_INTEGER, gl::UNSIGNED_INT),
        F::R32Sint => (gl::RED_INTEGER, gl::INT),
        F::Rg32Sint => (gl::RG_INTEGER, gl::INT),
        F::R16Float => (gl::RED, gl::HALF_FLOAT),
        F::Rg16Float => (gl::RG, gl::HALF_FLOAT),
        F::Rgba16Float => (gl::RGBA, gl::HALF_FLOAT),
        F::R32Float => (gl::RED, gl::FLOAT),
        F::Rg32Float => (gl::RG, gl::FLOAT),
        F::R11G11B10Float => (gl::RGB, gl::UNSIGNED_INT_10F_11F_11F_REV),
        F::Rgba32Uint => (gl::RGBA_INTEGER, gl::UNSIGNED_INT),
        F::Rgba32Sint => (gl::RGBA_INTEGER, gl::INT),
        F::Rgba32Float => (gl::RGBA, gl::FLOAT),
        _ => (gl::RGBA, gl::UNSIGNED_BYTE),
    }
}

impl Context {
    // ---- Framebuffer objects -------------------------------------------------------

    /// `glGenFramebuffers`.
    pub fn gen_framebuffers(&mut self, names: &mut [u32]) {
        for n in names {
            *n = self.local.framebuffers.generate();
        }
    }

    /// One framebuffer name.
    pub fn gen_framebuffer(&mut self) -> u32 {
        self.local.framebuffers.generate()
    }

    /// `glIsFramebuffer`.
    pub fn is_framebuffer(&self, name: u32) -> bool {
        self.local.framebuffers.key(name).is_some()
    }

    /// `glBindFramebuffer`.
    pub fn bind_framebuffer(&mut self, target: u32, name: u32) {
        if !matches!(target, gl::FRAMEBUFFER | gl::DRAW_FRAMEBUFFER | gl::READ_FRAMEBUFFER) {
            return self.err(gl::INVALID_ENUM);
        }
        let key = if name == 0 {
            None
        } else {
            Some(self.local.framebuffers.get_or_create(name, FramebufferObject::default))
        };
        if target != gl::READ_FRAMEBUFFER {
            self.local.draw_framebuffer = key;
        }
        if target != gl::DRAW_FRAMEBUFFER {
            self.local.read_framebuffer = key;
        }
    }

    /// `glDeleteFramebuffers`.
    pub fn delete_framebuffers(&mut self, names: &[u32]) {
        for &name in names {
            let key = self.local.framebuffers.key(name);
            if key.is_some() {
                if self.local.draw_framebuffer == key {
                    self.local.draw_framebuffer = None;
                }
                if self.local.read_framebuffer == key {
                    self.local.read_framebuffer = None;
                }
            }
            if let Some((_, Some(fb))) = self.local.framebuffers.delete(name) {
                for a in fb.attachments() {
                    self.release_attachment(a);
                }
            }
        }
    }

    fn release_attachment(&mut self, a: Attachment) {
        match a {
            Attachment::None => {}
            Attachment::Renderbuffer(k) => {
                if let Some(rb) = self.renderbuffers.release(k) {
                    self.destroy_renderbuffer(rb);
                }
            }
            Attachment::Texture { key, .. } => self.release_texture(key),
        }
    }

    fn retain_attachment(&mut self, a: Attachment) {
        match a {
            Attachment::None => {}
            Attachment::Renderbuffer(k) => self.renderbuffers.retain(k),
            Attachment::Texture { key, .. } => self.textures.retain(key),
        }
    }

    /// Detaches what `pred` matches from the bound framebuffers (deleting a
    /// texture or renderbuffer does so, appendix D.1.2).
    pub(crate) fn detach_from_bound_framebuffers(&mut self, pred: impl Fn(Attachment) -> bool) {
        let mut keys: Vec<Key> = self.local.draw_framebuffer.into_iter().collect();
        if self.local.read_framebuffer != self.local.draw_framebuffer {
            keys.extend(self.local.read_framebuffer);
        }
        for k in keys {
            let fb = self.local.framebuffers.get_mut(k);
            let mut released = Vec::new();
            for a in fb.colors.iter_mut().chain([&mut fb.depth, &mut fb.stencil]) {
                if pred(*a) {
                    released.push(core::mem::take(a));
                }
            }
            for a in released {
                self.release_attachment(a);
            }
        }
    }

    /// The framebuffer bound to `target`: `Err` for an unknown target,
    /// `Ok(None)` for the default framebuffer.
    fn framebuffer_target(&self, target: u32) -> Result<Option<Key>, ()> {
        match target {
            gl::FRAMEBUFFER | gl::DRAW_FRAMEBUFFER => Ok(self.local.draw_framebuffer),
            gl::READ_FRAMEBUFFER => Ok(self.local.read_framebuffer),
            _ => Err(()),
        }
    }

    /// Sets an attachment point of the framebuffer bound to `target`.
    fn attach(&mut self, key: Key, point: Point, a: Attachment) {
        let n = if point == Point::DepthStencil { 2 } else { 1 };
        for _ in 0..n {
            self.retain_attachment(a);
        }
        let fb = self.local.framebuffers.get_mut(key);
        let old: Vec<Attachment> = match point {
            Point::Depth => alloc::vec![core::mem::replace(&mut fb.depth, a)],
            Point::Stencil => alloc::vec![core::mem::replace(&mut fb.stencil, a)],
            Point::DepthStencil => {
                alloc::vec![core::mem::replace(&mut fb.depth, a), core::mem::replace(&mut fb.stencil, a)]
            }
            Point::Color(i) => alloc::vec![core::mem::replace(&mut fb.colors[i], a)],
        };
        for o in old {
            self.release_attachment(o);
        }
    }

    /// The framebuffer object an attaching call changes, with its errors.
    fn attaching(&mut self, target: u32, attachment: u32) -> Option<(Key, Point)> {
        let Ok(fb) = self.framebuffer_target(target) else {
            self.err(gl::INVALID_ENUM);
            return None;
        };
        let point = match attachment_point(attachment) {
            Ok(p) => p,
            Err(e) => {
                self.err(e);
                return None;
            }
        };
        let Some(fb) = fb else {
            self.err(gl::INVALID_OPERATION);
            return None;
        };
        Some((fb, point))
    }

    /// `glFramebufferTexture2D`.
    pub fn framebuffer_texture_2d(&mut self, target: u32, attachment: u32, textarget: u32, texture: u32, level: i32) {
        let Some((fb, point)) = self.attaching(target, attachment) else { return };
        if texture == 0 {
            return self.attach(fb, point, Attachment::None);
        }
        let face = match textarget {
            gl::TEXTURE_2D => None,
            gl::TEXTURE_CUBE_MAP_POSITIVE_X..=gl::TEXTURE_CUBE_MAP_NEGATIVE_Z => {
                Some(textarget - gl::TEXTURE_CUBE_MAP_POSITIVE_X)
            }
            _ => return self.err(gl::INVALID_ENUM),
        };
        let Some(key) = self.textures.key(texture) else { return self.err(gl::INVALID_OPERATION) };
        let t = self.textures.get(key).target;
        if (face.is_some() && t != gl::TEXTURE_CUBE_MAP) || (face.is_none() && t != gl::TEXTURE_2D) {
            return self.err(gl::INVALID_OPERATION);
        }
        let caps = self.backend.caps();
        let max = if face.is_some() { caps.max_cube_map_size } else { caps.max_texture_size };
        if level < 0 || level as u32 >= super::textures::chain_levels(max) {
            return self.err(gl::INVALID_VALUE);
        }
        self.attach(fb, point, Attachment::Texture { key, level: level as u32, layer: face.unwrap_or(0) });
    }

    /// `glFramebufferTextureLayer`.
    pub fn framebuffer_texture_layer(&mut self, target: u32, attachment: u32, texture: u32, level: i32, layer: i32) {
        let Some((fb, point)) = self.attaching(target, attachment) else { return };
        if texture == 0 {
            return self.attach(fb, point, Attachment::None);
        }
        let Some(key) = self.textures.key(texture) else { return self.err(gl::INVALID_OPERATION) };
        let t = self.textures.get(key).target;
        let caps = self.backend.caps();
        let (max_level_size, max_layer) = match t {
            gl::TEXTURE_3D => (caps.max_3d_texture_size, caps.max_3d_texture_size),
            gl::TEXTURE_2D_ARRAY => (caps.max_texture_size, caps.max_array_layers),
            _ => return self.err(gl::INVALID_OPERATION),
        };
        if level < 0
            || level as u32 >= super::textures::chain_levels(max_level_size)
            || layer < 0
            || layer as u32 >= max_layer
        {
            return self.err(gl::INVALID_VALUE);
        }
        self.attach(fb, point, Attachment::Texture { key, level: level as u32, layer: layer as u32 });
    }

    /// `glFramebufferRenderbuffer`.
    pub fn framebuffer_renderbuffer(
        &mut self,
        target: u32,
        attachment: u32,
        renderbuffer_target: u32,
        renderbuffer: u32,
    ) {
        if renderbuffer_target != gl::RENDERBUFFER {
            return self.err(gl::INVALID_ENUM);
        }
        let Some((fb, point)) = self.attaching(target, attachment) else { return };
        if renderbuffer == 0 {
            return self.attach(fb, point, Attachment::None);
        }
        let Some(key) = self.renderbuffers.key(renderbuffer) else { return self.err(gl::INVALID_OPERATION) };
        self.attach(fb, point, Attachment::Renderbuffer(key));
    }

    // ---- Completeness ------------------------------------------------------------

    /// An attachment's image, if it is attachment complete for the point
    /// (`Err` if it is not).
    fn resolve(&mut self, a: Attachment, point: Point) -> Result<Option<Resolved>, ()> {
        let caps_float = self.backend.caps().color_buffer_float;
        let r = match a {
            Attachment::None => return Ok(None),
            Attachment::Renderbuffer(k) => {
                let rb = self.renderbuffers.get(k);
                let (Some(internal), Some(resource)) = (rb.internal, rb.resource) else { return Err(()) };
                Resolved {
                    surface: Surface { resource, level: 0, layer: 0 },
                    internal,
                    width: rb.width,
                    height: rb.height,
                    samples: rb.samples,
                }
            }
            Attachment::Texture { key, level, layer } => {
                let tex = self.textures.get(key);
                let cube = tex.target == gl::TEXTURE_CUBE_MAP;
                let face = if cube { layer as usize } else { 0 };
                let Some(img) = tex.image(face, level).copied() else { return Err(()) };
                if !cube && tex.target != gl::TEXTURE_2D && layer >= img.depth {
                    return Err(());
                }
                // A mutable texture's level must be in its range, and other
                // than the base level only if the texture is mipmap (and
                // cube) complete.
                if tex.immutable_levels.is_none() {
                    let (base, q) = Self::level_range(tex);
                    if level < base || level > q {
                        return Err(());
                    }
                    if level != base && !Self::mipmap_complete(tex, base, q) {
                        return Err(());
                    }
                }
                let Some((resource, l, first)) = self.image_location(tex, face, level) else { return Err(()) };
                let layer = if cube { first } else { first + layer };
                Resolved {
                    surface: Surface { resource, level: l, layer },
                    internal: img.internal,
                    width: img.width,
                    height: img.height,
                    samples: 0,
                }
            }
        };
        if r.width == 0 || r.height == 0 {
            return Err(());
        }
        let f = r.internal.format;
        let ok = match point {
            Point::Depth => f.has_depth(),
            Point::Stencil => f.has_stencil(),
            Point::DepthStencil => f.has_depth() && f.has_stencil(),
            Point::Color(_) => r.internal.renderable || (r.internal.float_renderable && caps_float),
        };
        if ok { Ok(Some(r)) } else { Err(()) }
    }

    /// The renderer's view of a framebuffer object, or its completeness
    /// status (`CheckFramebufferStatus`) if it is not complete.
    fn fbo_state(&mut self, key: Key, draw: bool) -> Result<Framebuffer, u32> {
        let fbo = self.local.framebuffers.get(key).clone();
        let mut out = Framebuffer::default();
        let mut size: Option<(u32, u32)> = None;
        let mut samples: Option<u32> = None;
        let mut renderbuffers = false;
        let mut textures = false;
        let mut any = false;
        let mut images = [None; DRAW_BUFFERS + 2];
        for (i, a) in fbo.attachments().enumerate() {
            let point = match i {
                i if i < DRAW_BUFFERS => Point::Color(i),
                i if i == DRAW_BUFFERS => Point::Depth,
                _ => Point::Stencil,
            };
            // Memory from outside that the GPU only samples is drawn into by
            // nothing (`tex_storage_external`).
            let external = matches!(a, Attachment::Texture { key, .. } if self.textures.get(key).external);
            let Ok(r) = self.resolve(a, point) else { return Err(gl::FRAMEBUFFER_INCOMPLETE_ATTACHMENT) };
            let Some(r) = r else { continue };
            if draw && external {
                return Err(gl::FRAMEBUFFER_INCOMPLETE_ATTACHMENT);
            }
            any = true;
            renderbuffers |= matches!(a, Attachment::Renderbuffer(_));
            textures |= matches!(a, Attachment::Texture { .. });
            if samples.is_some_and(|s| s != r.samples) {
                return Err(gl::FRAMEBUFFER_INCOMPLETE_MULTISAMPLE);
            }
            samples = Some(r.samples);
            size = Some(match size {
                Some((w, h)) => (w.min(r.width), h.min(r.height)),
                None => (r.width, r.height),
            });
            images[i] = Some(r);
        }
        if !any {
            return Err(gl::FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT);
        }
        if renderbuffers && textures && samples != Some(0) {
            return Err(gl::FRAMEBUFFER_INCOMPLETE_MULTISAMPLE);
        }
        // Depth and stencil must be one image (both backends keep them in
        // one resource).
        let (d, s) = (images[DRAW_BUFFERS], images[DRAW_BUFFERS + 1]);
        if let (Some(d), Some(s)) = (d, s)
            && d.surface != s.surface
        {
            return Err(gl::FRAMEBUFFER_UNSUPPORTED);
        }
        for (i, img) in images.iter().take(DRAW_BUFFERS).enumerate() {
            out.colors[i] = img.map(|r| r.surface);
        }
        out.depth = d.map(|r| r.surface);
        out.stencil = s.map(|r| r.surface);
        let (w, h) = size.unwrap_or((0, 0));
        out.width = w;
        out.height = h;
        out.samples = samples.unwrap_or(0);
        if draw {
            for (i, &b) in fbo.draw_buffers.iter().enumerate() {
                if b != gl::NONE {
                    out.draw_buffers[i] = Some((b - gl::COLOR_ATTACHMENT0) as u8);
                }
            }
        } else if fbo.read_buffer != gl::NONE {
            out.draw_buffers[0] = Some((fbo.read_buffer - gl::COLOR_ATTACHMENT0) as u8);
        }
        Ok(out)
    }

    /// The draw framebuffer as the renderer sees it, or its status.
    pub(crate) fn draw_state_framebuffer(&mut self) -> Result<Framebuffer, u32> {
        match self.local.draw_framebuffer {
            Some(k) => self.fbo_state(k, true),
            None if self.local.config.surfaceless => Err(gl::FRAMEBUFFER_UNDEFINED),
            None => Ok(self.default_framebuffer(true)),
        }
    }

    /// The read framebuffer (its read buffer as draw buffer 0), or its
    /// status.
    fn read_state_framebuffer(&mut self) -> Result<Framebuffer, u32> {
        match self.local.read_framebuffer {
            Some(k) => self.fbo_state(k, false),
            None if self.local.config.surfaceless => Err(gl::FRAMEBUFFER_UNDEFINED),
            None => Ok(self.default_framebuffer(false)),
        }
    }

    /// `glCheckFramebufferStatus`.
    pub fn check_framebuffer_status(&mut self, target: u32) -> u32 {
        let r = match target {
            gl::FRAMEBUFFER | gl::DRAW_FRAMEBUFFER => self.draw_state_framebuffer(),
            gl::READ_FRAMEBUFFER => self.read_state_framebuffer(),
            _ => {
                self.err(gl::INVALID_ENUM);
                return 0;
            }
        };
        match r {
            Ok(_) => gl::FRAMEBUFFER_COMPLETE,
            Err(status) => status,
        }
    }

    /// The internal format of a draw framebuffer's color attachment.
    fn color_internal(&self, fbo: Option<Key>, index: usize) -> Option<Internal> {
        match fbo {
            None => (index == 0).then_some(self.local.default_fb.color_internal).flatten(),
            Some(k) => self.attachment_internal(self.local.framebuffers.get(k).colors.get(index).copied()?),
        }
    }

    fn attachment_internal(&self, a: Attachment) -> Option<Internal> {
        match a {
            Attachment::None => None,
            Attachment::Renderbuffer(k) => self.renderbuffers.get(k).internal,
            Attachment::Texture { key, level, layer } => {
                let tex = self.textures.get(key);
                let face = if tex.target == gl::TEXTURE_CUBE_MAP { layer as usize } else { 0 };
                tex.image(face, level).map(|i| i.internal)
            }
        }
    }

    /// The read buffer, with the errors of reading it.
    pub(crate) fn read_color(&mut self) -> Result<ReadColor, u32> {
        let fb = self.read_state_framebuffer().map_err(|_| gl::INVALID_FRAMEBUFFER_OPERATION)?;
        let Some(index) = fb.draw_buffers[0] else { return Err(gl::INVALID_OPERATION) };
        let surface = fb.colors[index as usize].ok_or(gl::INVALID_OPERATION)?;
        if self.local.read_framebuffer.is_some() && fb.samples > 0 {
            return Err(gl::INVALID_OPERATION);
        }
        let internal = self.color_internal(self.local.read_framebuffer, index as usize).ok_or(gl::INVALID_OPERATION)?;
        Ok(ReadColor { fb, index, surface, internal })
    }

    // ---- Draw and read buffers -------------------------------------------------------

    /// `glDrawBuffers`.
    pub fn draw_buffers(&mut self, bufs: &[u32]) {
        if bufs.len() > DRAW_BUFFERS {
            return self.err(gl::INVALID_VALUE);
        }
        match self.local.draw_framebuffer {
            None => {
                if bufs.len() != 1 || !matches!(bufs[0], gl::BACK | gl::NONE) {
                    let known = bufs.iter().all(|&b| {
                        matches!(b, gl::BACK | gl::NONE)
                            || (gl::COLOR_ATTACHMENT0..=gl::COLOR_ATTACHMENT15).contains(&b)
                    });
                    return self.err(if known { gl::INVALID_OPERATION } else { gl::INVALID_ENUM });
                }
                self.local.default_fb.draw_buffer = bufs[0];
            }
            Some(k) => {
                for (i, &b) in bufs.iter().enumerate() {
                    if b == gl::NONE || b == gl::COLOR_ATTACHMENT0 + i as u32 {
                        continue;
                    }
                    let known = b == gl::BACK || (gl::COLOR_ATTACHMENT0..=gl::COLOR_ATTACHMENT15).contains(&b);
                    return self.err(if known { gl::INVALID_OPERATION } else { gl::INVALID_ENUM });
                }
                let fb = self.local.framebuffers.get_mut(k);
                fb.draw_buffers = [gl::NONE; DRAW_BUFFERS];
                fb.draw_buffers[..bufs.len()].copy_from_slice(bufs);
            }
        }
    }

    /// `glReadBuffer`.
    pub fn read_buffer(&mut self, src: u32) {
        let color = (gl::COLOR_ATTACHMENT0..=gl::COLOR_ATTACHMENT15).contains(&src);
        if !(color || matches!(src, gl::BACK | gl::NONE)) {
            return self.err(gl::INVALID_ENUM);
        }
        match self.local.read_framebuffer {
            None => {
                if color {
                    return self.err(gl::INVALID_OPERATION);
                }
                self.local.default_fb.read_buffer = src;
            }
            Some(k) => {
                if src == gl::BACK || (color && (src - gl::COLOR_ATTACHMENT0) as usize >= DRAW_BUFFERS) {
                    return self.err(gl::INVALID_OPERATION);
                }
                self.local.framebuffers.get_mut(k).read_buffer = src;
            }
        }
    }

    /// `glInvalidateFramebuffer` (a hint: checked, then ignored).
    pub fn invalidate_framebuffer(&mut self, target: u32, attachments: &[u32]) {
        let Ok(fb) = self.framebuffer_target(target) else { return self.err(gl::INVALID_ENUM) };
        for &a in attachments {
            let checked = match fb {
                None if matches!(a, gl::COLOR | gl::DEPTH | gl::STENCIL) => Ok(()),
                None => Err(gl::INVALID_ENUM),
                Some(_) => attachment_point(a).map(|_| ()),
            };
            if let Err(e) = checked {
                return self.err(e);
            }
        }
    }

    /// `glInvalidateSubFramebuffer` (a hint).
    pub fn invalidate_sub_framebuffer(
        &mut self,
        target: u32,
        attachments: &[u32],
        _x: i32,
        _y: i32,
        width: i32,
        height: i32,
    ) {
        if width < 0 || height < 0 {
            return self.err(gl::INVALID_VALUE);
        }
        self.invalidate_framebuffer(target, attachments);
    }

    // ---- Attachment queries -------------------------------------------------------------

    /// `glGetFramebufferAttachmentParameteriv` (one value).
    pub fn get_framebuffer_attachment_parameteri(&mut self, target: u32, attachment: u32, pname: u32) -> i32 {
        let Ok(fbo) = self.framebuffer_target(target) else {
            self.err(gl::INVALID_ENUM);
            return 0;
        };
        // What is attached, and its format.
        let (kind, name, a, internal) = match fbo {
            None => {
                let fb = self.local.default_fb;
                let i = match attachment {
                    gl::BACK => fb.color_internal,
                    gl::DEPTH => fb.depth_internal.filter(|i| i.format.has_depth()),
                    gl::STENCIL => fb.depth_internal.filter(|i| i.format.has_stencil()),
                    _ => {
                        self.err(gl::INVALID_OPERATION);
                        return 0;
                    }
                };
                let kind = if i.is_some() { gl::FRAMEBUFFER_DEFAULT } else { gl::NONE };
                (kind, 0, Attachment::None, i)
            }
            Some(k) => {
                let fb = self.local.framebuffers.get(k).clone();
                let a = match attachment_point(attachment) {
                    Ok(Point::Depth) => fb.depth,
                    Ok(Point::Stencil) => fb.stencil,
                    Ok(Point::DepthStencil) if fb.depth == fb.stencil => fb.depth,
                    Ok(Point::DepthStencil) => {
                        self.err(gl::INVALID_OPERATION);
                        return 0;
                    }
                    Ok(Point::Color(i)) => fb.colors[i],
                    Err(e) => {
                        self.err(e);
                        return 0;
                    }
                };
                let (kind, name) = match a {
                    Attachment::None => (gl::NONE, 0),
                    Attachment::Renderbuffer(k) => (gl::RENDERBUFFER, self.renderbuffers.name(k)),
                    Attachment::Texture { key, .. } => (gl::TEXTURE, self.textures.name(key)),
                };
                (kind, name, a, self.attachment_internal(a))
            }
        };
        if pname == gl::FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE {
            return kind as i32;
        }
        if kind == gl::NONE {
            if pname != gl::FRAMEBUFFER_ATTACHMENT_OBJECT_NAME {
                self.err(gl::INVALID_OPERATION);
            }
            return 0;
        }
        let bits = internal.map_or([0; 6], |i| i.bits);
        match pname {
            gl::FRAMEBUFFER_ATTACHMENT_OBJECT_NAME if kind != gl::FRAMEBUFFER_DEFAULT => name as i32,
            gl::FRAMEBUFFER_ATTACHMENT_TEXTURE_LEVEL if kind == gl::TEXTURE => match a {
                Attachment::Texture { level, .. } => level as i32,
                _ => 0,
            },
            gl::FRAMEBUFFER_ATTACHMENT_TEXTURE_CUBE_MAP_FACE if kind == gl::TEXTURE => match a {
                Attachment::Texture { key, layer, .. } if self.textures.get(key).target == gl::TEXTURE_CUBE_MAP => {
                    (gl::TEXTURE_CUBE_MAP_POSITIVE_X + layer) as i32
                }
                _ => 0,
            },
            gl::FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER if kind == gl::TEXTURE => match a {
                Attachment::Texture { key, layer, .. } if self.textures.get(key).target != gl::TEXTURE_CUBE_MAP => {
                    layer as i32
                }
                _ => 0,
            },
            gl::FRAMEBUFFER_ATTACHMENT_RED_SIZE => i32::from(bits[0]),
            gl::FRAMEBUFFER_ATTACHMENT_GREEN_SIZE => i32::from(bits[1]),
            gl::FRAMEBUFFER_ATTACHMENT_BLUE_SIZE => i32::from(bits[2]),
            gl::FRAMEBUFFER_ATTACHMENT_ALPHA_SIZE => i32::from(bits[3]),
            gl::FRAMEBUFFER_ATTACHMENT_DEPTH_SIZE => i32::from(bits[4]),
            gl::FRAMEBUFFER_ATTACHMENT_STENCIL_SIZE => i32::from(bits[5]),
            gl::FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE => {
                if attachment == gl::DEPTH_STENCIL_ATTACHMENT {
                    self.err(gl::INVALID_OPERATION);
                    return 0;
                }
                internal.map_or(gl::NONE, |i| match i.format.class() {
                    Class::Stencil => gl::UNSIGNED_INT,
                    _ => i.component.gl(),
                }) as i32
            }
            gl::FRAMEBUFFER_ATTACHMENT_COLOR_ENCODING => {
                (if internal.is_some_and(|i| i.format.is_srgb()) { gl::SRGB } else { gl::LINEAR }) as i32
            }
            _ => {
                self.err(gl::INVALID_ENUM);
                0
            }
        }
    }

    /// The draw framebuffer's color attachment 0 format (state queries:
    /// `RED_BITS`...), and its depth/stencil format.
    pub(crate) fn draw_formats(&self) -> (Option<Internal>, Option<Internal>, Option<Internal>) {
        match self.local.draw_framebuffer {
            None => {
                let d = self.local.default_fb.depth_internal;
                (
                    self.local.default_fb.color_internal,
                    d.filter(|i| i.format.has_depth()),
                    d.filter(|i| i.format.has_stencil()),
                )
            }
            Some(k) => {
                let fb = self.local.framebuffers.get(k);
                let color = fb.draw_buffers[0];
                let c = (color != gl::NONE)
                    .then(|| fb.colors.get((color - gl::COLOR_ATTACHMENT0) as usize).copied())
                    .flatten()
                    .and_then(|a| self.attachment_internal(a));
                (c, self.attachment_internal(fb.depth), self.attachment_internal(fb.stencil))
            }
        }
    }

    /// The read buffer's `IMPLEMENTATION_COLOR_READ_FORMAT`/`_TYPE`.
    pub(crate) fn color_read_pair(&mut self) -> Option<(u32, u32)> {
        let fbo = self.local.read_framebuffer;
        let index = match fbo {
            None => 0,
            Some(k) => {
                let r = self.local.framebuffers.get(k).read_buffer;
                if r == gl::NONE {
                    return None;
                }
                (r - gl::COLOR_ATTACHMENT0) as usize
            }
        };
        self.color_internal(fbo, index).map(|i| natural_read(&i))
    }
}

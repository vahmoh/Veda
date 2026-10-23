// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Textures (OpenGL ES 3.0 section 3.8): names and binding, the commands
//! that specify images (from application memory, compressed, as immutable
//! storage, or copied from the framebuffer) and mipmap generation.
//!
//! How a texture's images are kept in the renderer's resources, and when a
//! texture is complete, is in `storage`; sampling parameters, a texture's
//! own and sampler objects', are in `samplers`.

mod samplers;
mod storage;

use alloc::borrow::Cow;
use alloc::vec::Vec;

use super::{
    Attachment, Context, Image, Key, Location, MAX_LEVELS, PixelStore, Storage, TEXTURE_UNITS, Texture, target_index,
};
use crate::backend::{Region, ResourceDesc, Surface, Target, minify};
use crate::etc;
use crate::format::{self, Class, Client, ComponentType, FormatError, Internal};
use crate::gl;
use crate::pixels::{self, Pixels, Texels};

pub(crate) use samplers::sampler_state;

/// Which faces a texture image target names: the texture target and the
/// face (cube maps) or 0.
fn image_target(target: u32) -> Option<(u32, usize)> {
    Some(match target {
        gl::TEXTURE_2D => (gl::TEXTURE_2D, 0),
        gl::TEXTURE_3D => (gl::TEXTURE_3D, 0),
        gl::TEXTURE_2D_ARRAY => (gl::TEXTURE_2D_ARRAY, 0),
        gl::TEXTURE_CUBE_MAP_POSITIVE_X..=gl::TEXTURE_CUBE_MAP_NEGATIVE_Z => {
            (gl::TEXTURE_CUBE_MAP, (target - gl::TEXTURE_CUBE_MAP_POSITIVE_X) as usize)
        }
        _ => return None,
    })
}

/// Whether a texture target's images are three-dimensional uploads' (of
/// `TexImage3D`): 3D textures' and 2D arrays'.
fn three_d(target: u32) -> bool {
    matches!(target, gl::TEXTURE_3D | gl::TEXTURE_2D_ARRAY)
}

/// The renderer's resource kind for a texture target.
fn resource_target(target: u32) -> Target {
    match target {
        gl::TEXTURE_3D => Target::Texture3D,
        gl::TEXTURE_CUBE_MAP => Target::TextureCube,
        gl::TEXTURE_2D_ARRAY => Target::Texture2DArray,
        _ => Target::Texture2D,
    }
}

/// `floor(log2(x)) + 1`: the levels of a full mipmap chain.
pub(crate) fn chain_levels(max_dim: u32) -> u32 {
    32 - max_dim.max(1).leading_zeros()
}

/// The unsized internal formats (table 3.3, and depth for
/// `OES_depth_texture`).
fn is_unsized(internalformat: u32) -> bool {
    matches!(
        internalformat,
        gl::RGBA | gl::RGB | gl::LUMINANCE_ALPHA | gl::LUMINANCE | gl::ALPHA | gl::DEPTH_COMPONENT | gl::DEPTH_STENCIL
    )
}

/// Components of a base internal format: red, green, blue, alpha,
/// luminance.
fn base_components(base: u32) -> [bool; 5] {
    match base {
        gl::RED => [true, false, false, false, false],
        gl::RG => [true, true, false, false, false],
        gl::RGB => [true, true, true, false, false],
        gl::RGBA => [true, true, true, true, false],
        gl::ALPHA => [false, false, false, true, false],
        gl::LUMINANCE => [false, false, false, false, true],
        gl::LUMINANCE_ALPHA => [false, false, false, true, true],
        _ => [false; 5],
    }
}

/// The internal format `CopyTexImage2D` creates from a source buffer of
/// format `src` (section 3.8.5), or the error.
fn copy_format(src: &Internal, internalformat: u32) -> Result<Internal, u32> {
    let src_class = src.format.class();
    if matches!(src_class, Class::Float | Class::Depth | Class::DepthStencil | Class::Stencil) {
        return Err(gl::INVALID_OPERATION);
    }
    let srgb = src.format.is_srgb();
    let candidates: Vec<Internal> =
        if matches!(internalformat, gl::RGBA | gl::RGB | gl::LUMINANCE_ALPHA | gl::LUMINANCE | gl::ALPHA) {
            // Unsized: the effective formats table 3.12 gives each.
            if src.component != ComponentType::UnsignedNormalized || srgb {
                return Err(gl::INVALID_OPERATION);
            }
            let options: &[(u32, u32)] = match internalformat {
                gl::RGBA => &[
                    (gl::RGBA, gl::UNSIGNED_BYTE),
                    (gl::RGBA, gl::UNSIGNED_SHORT_4_4_4_4),
                    (gl::RGBA, gl::UNSIGNED_SHORT_5_5_5_1),
                ],
                gl::RGB => &[(gl::RGB, gl::UNSIGNED_BYTE), (gl::RGB, gl::UNSIGNED_SHORT_5_6_5)],
                f => &[(f, gl::UNSIGNED_BYTE)],
            };
            options.iter().filter_map(|&(f, t)| format::tex_format(internalformat, f, t).ok()).collect()
        } else {
            let Some(dst) = format::sized(internalformat) else {
                return Err(if format::is_format(internalformat) { gl::INVALID_OPERATION } else { gl::INVALID_ENUM });
            };
            let compatible = matches!(
                (src.component, dst.component),
                (ComponentType::UnsignedNormalized, ComponentType::UnsignedNormalized)
                    | (ComponentType::Int, ComponentType::Int)
                    | (ComponentType::UnsignedInt, ComponentType::UnsignedInt)
            );
            if !compatible || dst.format.is_srgb() != srgb || dst.format.has_depth() || dst.format.has_stencil() {
                return Err(gl::INVALID_OPERATION);
            }
            alloc::vec![*dst]
        };
    // New components cannot be added (table 3.16); luminance takes red.
    let s = base_components(src.base);
    let ok_base = |dst: &Internal| {
        let d = base_components(dst.base);
        (!d[0] || s[0]) && (!d[1] || s[1]) && (!d[2] || s[2]) && (!d[3] || s[3]) && (!d[4] || s[0])
    };
    // Component sizes, red standing in for luminance.
    let sizes = |f: &Internal| {
        let d = base_components(f.base);
        let lum = if d[4] { f.bits[0].max(8) } else { 0 };
        [if d[4] { lum } else { f.bits[0] }, f.bits[1], f.bits[2], f.bits[3]]
    };
    let present = |f: &Internal| {
        let d = base_components(f.base);
        [d[0] || d[4], d[1], d[2], d[3]]
    };
    let src_sizes = sizes(src);
    let cmp = |f: &Internal, rule: u8| {
        let (fs, p) = (sizes(f), present(f));
        (0..4).filter(|&i| p[i]).all(|i| match rule {
            0 => fs[i] == src_sizes[i],
            1 => fs[i] >= src_sizes[i],
            _ => fs[i] <= src_sizes[i],
        })
    };
    let valid: Vec<&Internal> = candidates.iter().filter(|f| ok_base(f)).collect();
    if valid.is_empty() {
        return Err(gl::INVALID_OPERATION);
    }
    if !is_unsized(internalformat) {
        // Sized formats must match exactly.
        return if cmp(valid[0], 0) { Ok(*valid[0]) } else { Err(gl::INVALID_OPERATION) };
    }
    for rule in 0..3 {
        if let Some(f) = valid.iter().find(|f| cmp(f, rule)) {
            return Ok(**f);
        }
    }
    Err(gl::INVALID_OPERATION)
}

impl Context {
    // ---- Names and binding ---------------------------------------------------

    /// `glGenTextures`.
    pub fn gen_textures(&mut self, names: &mut [u32]) {
        for n in names {
            *n = self.textures.generate();
        }
    }

    /// One texture name.
    pub fn gen_texture(&mut self) -> u32 {
        self.textures.generate()
    }

    /// `glIsTexture`.
    pub fn is_texture(&self, name: u32) -> bool {
        name != 0 && self.textures.key(name).is_some()
    }

    /// `glActiveTexture`.
    pub fn active_texture(&mut self, texture: u32) {
        match texture.checked_sub(gl::TEXTURE0) {
            Some(i) if (i as usize) < TEXTURE_UNITS => self.local.active_texture = i as usize,
            _ => self.err(gl::INVALID_ENUM),
        }
    }

    /// `glBindTexture`.
    pub fn bind_texture(&mut self, target: u32, name: u32) {
        let Some(t) = target_index(target) else { return self.err(gl::INVALID_ENUM) };
        let key = if name == 0 {
            self.local.default_textures[t]
        } else {
            match self.textures.key(name) {
                Some(k) if self.textures.get(k).target != target => return self.err(gl::INVALID_OPERATION),
                Some(k) => k,
                None => self.textures.create(name, Texture::new(target)),
            }
        };
        self.local.texture_units[self.local.active_texture][t] = key;
    }

    /// `glDeleteTextures`.
    pub fn delete_textures(&mut self, names: &[u32]) {
        for &name in names {
            if name == 0 {
                continue;
            }
            let Some(key) = self.textures.key(name) else {
                let _ = self.textures.delete(name);
                continue;
            };
            let defaults = self.local.default_textures;
            for unit in &mut self.local.texture_units {
                for (t, bound) in unit.iter_mut().enumerate() {
                    if *bound == key {
                        *bound = defaults[t];
                    }
                }
            }
            self.detach_from_bound_framebuffers(|a| matches!(a, Attachment::Texture { key: k, .. } if k == key));
            if let Some((_, Some(dead))) = self.textures.delete(name) {
                self.destroy_texture(dead);
            }
        }
    }

    pub(crate) fn destroy_texture(&mut self, t: Texture) {
        for face in &t.images {
            for img in face.iter().flatten() {
                if let Location::Own(r) = img.location {
                    self.backend.destroy_resource(r);
                }
            }
        }
        if let Some(s) = t.storage {
            self.backend.destroy_resource(s.resource);
        }
    }

    pub(crate) fn release_texture(&mut self, key: Key) {
        if let Some(t) = self.textures.release(key) {
            self.destroy_texture(t);
        }
    }

    /// The unpack storage modes of an upload: the image height and the
    /// images skipped are three-dimensional uploads' alone (OpenGL ES 3.0,
    /// section 3.7.2).
    pub(crate) fn unpack_store(&self, three_d: bool) -> PixelStore {
        let mut store = self.local.state.unpack;
        if !three_d {
            store.image_height = 0;
            store.skip_images = 0;
        }
        store
    }

    /// The bytes from where an upload's data starts to its last pixel's
    /// end: what `width` x `height` x `depth` pixels of `(format, ty)` take
    /// under the unpack storage modes, of a two-dimensional image unless
    /// `three_d` (`TexImage3D`). `None` if they take no layout (the upload
    /// fails then, as it says).
    pub fn unpack_size(
        &self,
        width: i32,
        height: i32,
        depth: i32,
        format: u32,
        ty: u32,
        three_d: bool,
    ) -> Option<usize> {
        let dims = [width, height, depth].map(|x| u32::try_from(x).ok());
        let [Some(w), Some(h), Some(d)] = dims else { return None };
        if !format::is_format(format) || !format::is_type(ty) {
            return None;
        }
        pixels::layout(Client::new(format, ty), w, h, d, &self.unpack_store(three_d)).map(|l| l.end)
    }

    /// The texture bound to `target` on the active unit.
    pub(crate) fn bound_texture(&self, target: u32) -> Option<Key> {
        target_index(target).map(|t| self.local.texture_units[self.local.active_texture][t])
    }

    /// The texture names bound to a unit (for state queries).
    pub(crate) fn unit_binding(&self, target: u32) -> u32 {
        let t = target_index(target).unwrap_or(0);
        self.textures.name(self.local.texture_units[self.local.active_texture][t])
    }

    // ---- Image specification ------------------------------------------------

    /// Checks a level and size for a texture image target; records the
    /// error and returns `false` if they are out of range.
    fn check_size(&mut self, tex_target: u32, level: i32, w: i32, h: i32, d: i32) -> bool {
        let caps = self.backend.caps();
        let (max_wh, max_d) = match tex_target {
            gl::TEXTURE_3D => (caps.max_3d_texture_size, caps.max_3d_texture_size),
            gl::TEXTURE_2D_ARRAY => (caps.max_texture_size, caps.max_array_layers),
            gl::TEXTURE_CUBE_MAP => (caps.max_cube_map_size, 1),
            _ => (caps.max_texture_size, 1),
        };
        let max_level = chain_levels(max_wh) as i32 - 1;
        if level < 0 || level > max_level || w < 0 || h < 0 || d < 0 {
            self.err(gl::INVALID_VALUE);
            return false;
        }
        let lim = (max_wh >> level).max(1) as i32;
        let dlim = if tex_target == gl::TEXTURE_3D { (max_d >> level).max(1) as i32 } else { max_d as i32 };
        if w > lim || h > lim || d > dlim || (tex_target == gl::TEXTURE_CUBE_MAP && w != h) {
            self.err(gl::INVALID_VALUE);
            return false;
        }
        true
    }

    /// The bytes of a pixel transfer source: the application's slice or
    /// a range of the unpack buffer. Checks the unpack rules (section
    /// 3.7.1) and records errors.
    fn unpack_source<'a>(
        &mut self,
        pixels: Pixels<'a>,
        layout: &pixels::Layout,
        type_bytes: usize,
    ) -> Result<Option<Cow<'a, [u8]>>, ()> {
        let buffer = self.local.bound.pixel_unpack;
        match (pixels, buffer) {
            (Pixels::Data(d), None) => match d.get(..layout.end) {
                Some(d) => Ok(Some(Cow::Borrowed(d))),
                None => {
                    self.err(gl::INVALID_OPERATION);
                    Err(())
                }
            },
            (Pixels::None, None) => Ok(None),
            (Pixels::Offset(_), None) | (Pixels::Data(_), Some(_)) => {
                self.err(gl::INVALID_OPERATION);
                Err(())
            }
            (p, Some(k)) => {
                let offset = if let Pixels::Offset(o) = p { o } else { 0 };
                let (_, size) = self.buffer_store(k);
                let mapped = self.buffers.get(k).map.is_some();
                let end = offset.checked_add(layout.end);
                if mapped || end.is_none_or(|e| e > size) || !offset.is_multiple_of(type_bytes.max(1)) {
                    self.err(gl::INVALID_OPERATION);
                    return Err(());
                }
                match self.read_buffer_range(k, offset, layout.end) {
                    Some(v) => Ok(Some(Cow::Owned(v))),
                    None => {
                        self.err(gl::OUT_OF_MEMORY);
                        Err(())
                    }
                }
            }
        }
    }

    /// Application pixels (`src`) as an image's texels: themselves if they
    /// are laid out as the texels are, otherwise converted. `None` (and
    /// `OUT_OF_MEMORY`) if there is no memory to convert them into.
    fn texels<'a>(
        &mut self,
        client: Client,
        src: Cow<'a, [u8]>,
        layout: &pixels::Layout,
        size: [u32; 3],
        i: &Internal,
    ) -> Option<Texels<'a>> {
        let t = pixels::texels(client, src, layout, size[0], size[1], size[2], i.format);
        if t.is_none() {
            self.err(gl::OUT_OF_MEMORY);
        }
        t
    }

    /// `glTexImage2D`.
    pub fn tex_image_2d<'a>(
        &mut self,
        target: u32,
        level: i32,
        internalformat: u32,
        width: i32,
        height: i32,
        border: i32,
        format: u32,
        ty: u32,
        pixels: impl Into<Pixels<'a>>,
    ) {
        let pixels = pixels.into();
        let Some((tex_target, face)) =
            image_target(target).filter(|t| matches!(t.0, gl::TEXTURE_2D | gl::TEXTURE_CUBE_MAP))
        else {
            return self.err(gl::INVALID_ENUM);
        };
        self.tex_image(tex_target, face, level, internalformat, width, height, 1, border, format, ty, pixels);
    }

    /// `glTexImage3D`.
    pub fn tex_image_3d<'a>(
        &mut self,
        target: u32,
        level: i32,
        internalformat: u32,
        width: i32,
        height: i32,
        depth: i32,
        border: i32,
        format: u32,
        ty: u32,
        pixels: impl Into<Pixels<'a>>,
    ) {
        let pixels = pixels.into();
        if !matches!(target, gl::TEXTURE_3D | gl::TEXTURE_2D_ARRAY) {
            return self.err(gl::INVALID_ENUM);
        }
        self.tex_image(target, 0, level, internalformat, width, height, depth, border, format, ty, pixels);
    }

    fn tex_image(
        &mut self,
        tex_target: u32,
        face: usize,
        level: i32,
        internalformat: u32,
        w: i32,
        h: i32,
        d: i32,
        border: i32,
        format: u32,
        ty: u32,
        pixels: Pixels<'_>,
    ) {
        if !format::is_type(ty) || !format::is_format(format) {
            return self.err(gl::INVALID_ENUM);
        }
        if !self.check_size(tex_target, level, w, h, d) {
            return;
        }
        if border != 0 {
            return self.err(gl::INVALID_VALUE);
        }
        let internal = match format::tex_format(internalformat, format, ty) {
            Ok(i) => i,
            Err(FormatError::Enum) => return self.err(gl::INVALID_ENUM),
            Err(FormatError::Value) => return self.err(gl::INVALID_VALUE),
            Err(FormatError::Operation) => return self.err(gl::INVALID_OPERATION),
        };
        if tex_target == gl::TEXTURE_3D && (internal.format.has_depth() || internal.format.has_stencil()) {
            return self.err(gl::INVALID_OPERATION);
        }
        let key = self.bound_texture(tex_target).unwrap();
        if self.textures.get(key).immutable_levels.is_some() {
            return self.err(gl::INVALID_OPERATION);
        }
        let client = Client::new(format, ty);
        let unpack = self.unpack_store(three_d(tex_target));
        let Some(layout) = pixels::layout(client, w as u32, h as u32, d as u32, &unpack) else {
            return self.err(gl::INVALID_VALUE);
        };
        let Ok(src) = self.unpack_source(pixels, &layout, client.type_bytes()) else { return };
        let (w, h, d) = (w as u32, h as u32, d as u32);
        let texels = match src {
            Some(s) => match self.texels(client, s, &layout, [w, h, d], &internal) {
                Some(t) => Some(t),
                None => return,
            },
            None => None,
        };
        let img = Image {
            internal,
            compressed: None,
            unsized_format: is_unsized(internalformat),
            width: w,
            height: h,
            depth: d,
            location: Location::Empty,
        };
        if !self.define_image(key, face, level as u32, img, texels.as_ref()) {
            self.err(gl::OUT_OF_MEMORY);
        }
    }

    /// `glTexSubImage2D`.
    pub fn tex_sub_image_2d<'a>(
        &mut self,
        target: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        width: i32,
        height: i32,
        format: u32,
        ty: u32,
        pixels: impl Into<Pixels<'a>>,
    ) {
        let pixels = pixels.into();
        let Some((tex_target, face)) =
            image_target(target).filter(|t| matches!(t.0, gl::TEXTURE_2D | gl::TEXTURE_CUBE_MAP))
        else {
            return self.err(gl::INVALID_ENUM);
        };
        self.tex_sub_image(tex_target, face, level, [xoffset, yoffset, 0], [width, height, 1], format, ty, pixels);
    }

    /// `glTexSubImage3D`.
    pub fn tex_sub_image_3d<'a>(
        &mut self,
        target: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        zoffset: i32,
        width: i32,
        height: i32,
        depth: i32,
        format: u32,
        ty: u32,
        pixels: impl Into<Pixels<'a>>,
    ) {
        let pixels = pixels.into();
        if !matches!(target, gl::TEXTURE_3D | gl::TEXTURE_2D_ARRAY) {
            return self.err(gl::INVALID_ENUM);
        }
        self.tex_sub_image(target, 0, level, [xoffset, yoffset, zoffset], [width, height, depth], format, ty, pixels);
    }

    /// The image a `*SubImage*` call updates, with the checks of its level
    /// and box; `None` after an error.
    fn sub_image(
        &mut self,
        tex_target: u32,
        face: usize,
        level: i32,
        off: [i32; 3],
        size: [i32; 3],
    ) -> Option<(Key, Image)> {
        if !self.check_size(tex_target, level, 0, 0, 0) {
            return None;
        }
        if size.iter().any(|&s| s < 0) || off.iter().any(|&o| o < 0) {
            self.err(gl::INVALID_VALUE);
            return None;
        }
        let key = self.bound_texture(tex_target).unwrap();
        // Memory from outside is the GPU's to sample alone.
        let tex = self.textures.get(key);
        let Some(img) = tex.image(face, level as u32).copied().filter(|_| !tex.external) else {
            self.err(gl::INVALID_OPERATION);
            return None;
        };
        let dims = [img.width, img.height, img.depth];
        if (0..3).any(|i| off[i] as i64 + size[i] as i64 > dims[i] as i64) {
            self.err(gl::INVALID_VALUE);
            return None;
        }
        Some((key, img))
    }

    #[allow(clippy::too_many_arguments)]
    fn tex_sub_image(
        &mut self,
        tex_target: u32,
        face: usize,
        level: i32,
        off: [i32; 3],
        size: [i32; 3],
        format: u32,
        ty: u32,
        pixels: Pixels<'_>,
    ) {
        if !format::is_type(ty) || !format::is_format(format) {
            return self.err(gl::INVALID_ENUM);
        }
        let Some((key, img)) = self.sub_image(tex_target, face, level, off, size) else { return };
        if img.compressed.is_some() {
            return self.err(gl::INVALID_OPERATION);
        }
        // The data must be valid for the image's internal format.
        let valid = format::tex_format(img.internal.gl, format, ty).is_ok_and(|i| i.format == img.internal.format)
            || (img.unsized_format && format::tex_format(img.internal.base, format, ty).is_ok());
        if !valid {
            return self.err(gl::INVALID_OPERATION);
        }
        let client = Client::new(format, ty);
        let unpack = self.unpack_store(three_d(tex_target));
        let [w, h, d] = size.map(|x| x as u32);
        let Some(layout) = pixels::layout(client, w, h, d, &unpack) else { return self.err(gl::INVALID_VALUE) };
        let Ok(src) = self.unpack_source(pixels, &layout, client.type_bytes()) else { return };
        let Some(src) = src else { return };
        if w == 0 || h == 0 || d == 0 {
            return;
        }
        let Some(texels) = self.texels(client, src, &layout, [w, h, d], &img.internal) else { return };
        let tex = self.textures.get(key);
        let Some((r, l, layer)) = self.image_location(tex, face, level as u32) else { return };
        let region = Region::new(off[0] as u32, off[1] as u32, layer + off[2] as u32, w, h, d);
        self.backend.write(r, l, region, texels.data(), texels.row, texels.image);
    }

    // ---- Compressed images ----------------------------------------------------

    /// `glCompressedTexImage2D` (`image_size` is the data's size, which an
    /// offset into the unpack buffer needs).
    pub fn compressed_tex_image_2d<'a>(
        &mut self,
        target: u32,
        level: i32,
        internalformat: u32,
        width: i32,
        height: i32,
        border: i32,
        image_size: usize,
        data: impl Into<Pixels<'a>>,
    ) {
        let data = data.into();
        let Some((tex_target, face)) =
            image_target(target).filter(|t| matches!(t.0, gl::TEXTURE_2D | gl::TEXTURE_CUBE_MAP))
        else {
            return self.err(gl::INVALID_ENUM);
        };
        self.compressed_image(tex_target, face, level, internalformat, [width, height, 1], border, image_size, data);
    }

    /// `glCompressedTexImage3D`.
    pub fn compressed_tex_image_3d<'a>(
        &mut self,
        target: u32,
        level: i32,
        internalformat: u32,
        width: i32,
        height: i32,
        depth: i32,
        border: i32,
        image_size: usize,
        data: impl Into<Pixels<'a>>,
    ) {
        let data = data.into();
        if !matches!(target, gl::TEXTURE_3D | gl::TEXTURE_2D_ARRAY) {
            return self.err(gl::INVALID_ENUM);
        }
        // ETC2/EAC images are two-dimensional: arrays of them, not volumes.
        if target == gl::TEXTURE_3D && etc::compressed(internalformat).is_some() {
            return self.err(gl::INVALID_OPERATION);
        }
        self.compressed_image(target, 0, level, internalformat, [width, height, depth], border, image_size, data);
    }

    /// The compressed bytes a call passes, checked against `image_size`.
    fn compressed_source<'a>(&mut self, data: Pixels<'a>, image_size: usize, expected: usize) -> Option<Cow<'a, [u8]>> {
        if image_size != expected {
            self.err(gl::INVALID_VALUE);
            return None;
        }
        let layout = pixels::Layout { start: 0, row: 0, image: 0, pixel: 1, row_bytes: expected, end: expected };
        let data = match data {
            Pixels::Data(d) if d.len() != image_size => {
                self.err(gl::INVALID_VALUE);
                return None;
            }
            Pixels::None if self.local.bound.pixel_unpack.is_none() => {
                self.err(gl::INVALID_VALUE);
                return None;
            }
            d => d,
        };
        self.unpack_source(data, &layout, 1).ok().flatten()
    }

    #[allow(clippy::too_many_arguments)]
    fn compressed_image(
        &mut self,
        tex_target: u32,
        face: usize,
        level: i32,
        internalformat: u32,
        size: [i32; 3],
        border: i32,
        image_size: usize,
        data: Pixels<'_>,
    ) {
        let Some(c) = etc::compressed(internalformat) else { return self.err(gl::INVALID_ENUM) };
        let [w, h, d] = size;
        if !self.check_size(tex_target, level, w, h, d) {
            return;
        }
        if border != 0 {
            return self.err(gl::INVALID_VALUE);
        }
        let key = self.bound_texture(tex_target).unwrap();
        if self.textures.get(key).immutable_levels.is_some() {
            return self.err(gl::INVALID_OPERATION);
        }
        let (w, h, d) = (w as u32, h as u32, d as u32);
        let Some(expected) = c.image_size(w, h, d) else { return self.err(gl::INVALID_VALUE) };
        let Some(src) = self.compressed_source(data, image_size, expected) else { return };
        let Some(texels) = c.decode(&src, w, h, d) else { return self.err(gl::OUT_OF_MEMORY) };
        let img = Image {
            internal: c.internal(),
            compressed: Some(internalformat),
            unsized_format: false,
            width: w,
            height: h,
            depth: d,
            location: Location::Empty,
        };
        let texels = Texels::packed(texels, w, h, c.format.bytes());
        if !self.define_image(key, face, level as u32, img, Some(&texels)) {
            self.err(gl::OUT_OF_MEMORY);
        }
    }

    /// `glCompressedTexSubImage2D`.
    pub fn compressed_tex_sub_image_2d<'a>(
        &mut self,
        target: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        width: i32,
        height: i32,
        format: u32,
        image_size: usize,
        data: impl Into<Pixels<'a>>,
    ) {
        let data = data.into();
        let Some((tex_target, face)) =
            image_target(target).filter(|t| matches!(t.0, gl::TEXTURE_2D | gl::TEXTURE_CUBE_MAP))
        else {
            return self.err(gl::INVALID_ENUM);
        };
        self.compressed_sub_image(
            tex_target,
            face,
            level,
            [xoffset, yoffset, 0],
            [width, height, 1],
            format,
            image_size,
            data,
        );
    }

    /// `glCompressedTexSubImage3D`.
    pub fn compressed_tex_sub_image_3d<'a>(
        &mut self,
        target: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        zoffset: i32,
        width: i32,
        height: i32,
        depth: i32,
        format: u32,
        image_size: usize,
        data: impl Into<Pixels<'a>>,
    ) {
        let data = data.into();
        if !matches!(target, gl::TEXTURE_3D | gl::TEXTURE_2D_ARRAY) {
            return self.err(gl::INVALID_ENUM);
        }
        self.compressed_sub_image(
            target,
            0,
            level,
            [xoffset, yoffset, zoffset],
            [width, height, depth],
            format,
            image_size,
            data,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn compressed_sub_image(
        &mut self,
        tex_target: u32,
        face: usize,
        level: i32,
        off: [i32; 3],
        size: [i32; 3],
        format: u32,
        image_size: usize,
        data: Pixels<'_>,
    ) {
        let Some(c) = etc::compressed(format) else { return self.err(gl::INVALID_ENUM) };
        let Some((key, img)) = self.sub_image(tex_target, face, level, off, size) else { return };
        if img.compressed != Some(format) {
            return self.err(gl::INVALID_OPERATION);
        }
        // Whole blocks, except at the image's right and bottom edges.
        let [w, h, d] = size.map(|x| x as u32);
        let [x, y, z] = off.map(|x| x as u32);
        let aligned = |o: u32, s: u32, full: u32| o.is_multiple_of(4) && (s.is_multiple_of(4) || o + s == full);
        if !aligned(x, w, img.width) || !aligned(y, h, img.height) {
            return self.err(gl::INVALID_OPERATION);
        }
        let Some(expected) = c.image_size(w, h, d) else { return self.err(gl::INVALID_VALUE) };
        let Some(src) = self.compressed_source(data, image_size, expected) else { return };
        if w == 0 || h == 0 || d == 0 {
            return;
        }
        let Some(texels) = c.decode(&src, w, h, d) else { return self.err(gl::OUT_OF_MEMORY) };
        let tex = self.textures.get(key);
        let Some((r, l, layer)) = self.image_location(tex, face, level as u32) else { return };
        let t = Texels::packed(texels, w, h, c.format.bytes());
        self.backend.write(r, l, Region::new(x, y, layer + z, w, h, d), t.data(), t.row, t.image);
    }

    // ---- Immutable storage -------------------------------------------------------

    /// `glTexStorage2D`.
    pub fn tex_storage_2d(&mut self, target: u32, levels: i32, internalformat: u32, width: i32, height: i32) {
        if !matches!(target, gl::TEXTURE_2D | gl::TEXTURE_CUBE_MAP) {
            return self.err(gl::INVALID_ENUM);
        }
        self.tex_storage(target, levels, internalformat, width, height, 1);
    }

    /// `glTexStorage3D`.
    pub fn tex_storage_3d(
        &mut self,
        target: u32,
        levels: i32,
        internalformat: u32,
        width: i32,
        height: i32,
        depth: i32,
    ) {
        if !matches!(target, gl::TEXTURE_3D | gl::TEXTURE_2D_ARRAY) {
            return self.err(gl::INVALID_ENUM);
        }
        self.tex_storage(target, levels, internalformat, width, height, depth);
    }

    /// Makes the bound 2D texture's storage memory from outside, which the
    /// GPU then samples in place: one level of `width` x `height` pixels of
    /// `internalformat` (`GL_RGBA8`, or `GL_RGB8`, whose pixels' fourth
    /// byte is never read), immutable, which nothing draws into or writes
    /// (`*TexSubImage*` and `GenerateMipmap` fail with
    /// `GL_INVALID_OPERATION`, and a draw framebuffer it is attached to is
    /// incomplete). As a texture of an EGL image of a window's buffer, read
    /// by a compositor. `GL_INVALID_OPERATION` if the renderer cannot take
    /// such memory, `GL_OUT_OF_MEMORY` if not now.
    pub fn tex_storage_external(
        &mut self,
        target: u32,
        internalformat: u32,
        width: i32,
        height: i32,
        memory: &crate::backend::External,
    ) {
        if target != gl::TEXTURE_2D {
            return self.err(gl::INVALID_ENUM);
        }
        let Some(internal) = format::sized(internalformat).copied().filter(|i| matches!(i.gl, gl::RGBA8 | gl::RGB8))
        else {
            return self.err(gl::INVALID_ENUM);
        };
        if width < 1 || height < 1 {
            return self.err(gl::INVALID_VALUE);
        }
        if !self.check_size(target, 0, width, height, 1) {
            return;
        }
        let rows = (memory.stride() as usize).checked_mul(height as usize);
        if (memory.stride() as usize) < width as usize * 4 || rows.is_none_or(|bytes| bytes > memory.size()) {
            return self.err(gl::INVALID_VALUE);
        }
        let key = self.bound_texture(target).unwrap();
        if self.textures.get(key).immutable_levels.is_some() {
            return self.err(gl::INVALID_OPERATION);
        }
        let desc = ResourceDesc {
            target: resource_target(target),
            format: internal.format,
            width: width as u32,
            height: height as u32,
            depth: 1,
            levels: 1,
            samples: 0,
        };
        let resource = match self.backend.import_resource(&desc, memory) {
            Ok(r) => r,
            Err(crate::backend::ImportError::Unsupported) => return self.err(gl::INVALID_OPERATION),
            Err(crate::backend::ImportError::NoMemory) => return self.err(gl::OUT_OF_MEMORY),
        };
        let old = core::mem::replace(self.textures.get_mut(key), Texture::new(target));
        let mut tex = Texture {
            sampler: old.sampler,
            base_level: old.base_level,
            max_level: old.max_level,
            swizzle: old.swizzle,
            ..Texture::new(target)
        };
        self.destroy_texture(old);
        tex.immutable_levels = Some(1);
        tex.storage = Some(Storage { resource, desc, base: 0 });
        tex.external = true;
        tex.images[0][0] = Some(Image {
            internal,
            compressed: None,
            unsized_format: false,
            width: width as u32,
            height: height as u32,
            depth: 1,
            location: Location::Storage,
        });
        *self.textures.get_mut(key) = tex;
    }

    fn tex_storage(&mut self, target: u32, levels: i32, internalformat: u32, w: i32, h: i32, d: i32) {
        let (internal, compressed) = match (format::sized(internalformat), etc::compressed(internalformat)) {
            (Some(i), _) => (*i, None),
            (None, Some(c)) => (c.internal(), Some(internalformat)),
            _ => return self.err(gl::INVALID_ENUM),
        };
        if target == gl::TEXTURE_3D
            && (compressed.is_some() || internal.format.has_depth() || internal.format.has_stencil())
        {
            return self.err(gl::INVALID_OPERATION);
        }
        if w < 1 || h < 1 || d < 1 || levels < 1 {
            return self.err(gl::INVALID_VALUE);
        }
        if !self.check_size(target, 0, w, h, d) {
            return;
        }
        let key = self.bound_texture(target).unwrap();
        if self.textures.get(key).immutable_levels.is_some() {
            return self.err(gl::INVALID_OPERATION);
        }
        let (w, h, d) = (w as u32, h as u32, d as u32);
        let max_dim = if target == gl::TEXTURE_3D { w.max(h).max(d) } else { w.max(h) };
        if levels as u32 > chain_levels(max_dim) {
            return self.err(gl::INVALID_OPERATION);
        }
        let levels = levels as u32;
        let desc = ResourceDesc {
            target: resource_target(target),
            format: internal.format,
            width: w,
            height: h,
            depth: if target == gl::TEXTURE_CUBE_MAP { 6 } else { d },
            levels,
            samples: 0,
        };
        // The contents are undefined: zero, as a new resource reads.
        let Ok(resource) = self.backend.create_resource(&desc) else { return self.err(gl::OUT_OF_MEMORY) };
        let old = core::mem::replace(self.textures.get_mut(key), Texture::new(target));
        let mut tex = Texture {
            sampler: old.sampler,
            base_level: old.base_level,
            max_level: old.max_level,
            swizzle: old.swizzle,
            ..Texture::new(target)
        };
        self.destroy_texture(old);
        tex.immutable_levels = Some(levels);
        tex.storage = Some(Storage { resource, desc, base: 0 });
        for f in 0..tex.images.len() {
            for l in 0..levels {
                let (width, height, layers) = desc.level_size(l);
                tex.images[f][l as usize] = Some(Image {
                    internal,
                    compressed,
                    unsized_format: false,
                    width,
                    height,
                    // A cube map's faces are its storage's layers.
                    depth: if target == gl::TEXTURE_CUBE_MAP { 1 } else { layers },
                    location: Location::Storage,
                });
            }
        }
        *self.textures.get_mut(key) = tex;
    }

    // ---- Copies from the framebuffer ---------------------------------------------

    /// `glCopyTexImage2D`.
    pub fn copy_tex_image_2d(
        &mut self,
        target: u32,
        level: i32,
        internalformat: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        border: i32,
    ) {
        let Some((tex_target, face)) =
            image_target(target).filter(|t| matches!(t.0, gl::TEXTURE_2D | gl::TEXTURE_CUBE_MAP))
        else {
            return self.err(gl::INVALID_ENUM);
        };
        if !self.check_size(tex_target, level, width, height, 1) {
            return;
        }
        if border != 0 {
            return self.err(gl::INVALID_VALUE);
        }
        let src = match self.read_color() {
            Ok(s) => s,
            Err(e) => return self.err(e),
        };
        let internal = match copy_format(&src.internal, internalformat) {
            Ok(i) => i,
            Err(e) => return self.err(e),
        };
        let key = self.bound_texture(tex_target).unwrap();
        if self.textures.get(key).immutable_levels.is_some() {
            return self.err(gl::INVALID_OPERATION);
        }
        let img = Image {
            internal,
            compressed: None,
            unsized_format: is_unsized(internalformat),
            width: width as u32,
            height: height as u32,
            depth: 1,
            location: Location::Empty,
        };
        if !self.define_image(key, face, level as u32, img, None) {
            return self.err(gl::OUT_OF_MEMORY);
        }
        self.copy_from_read_buffer(&src, key, face, level as u32, [0, 0, 0], x, y, width, height);
    }

    /// `glCopyTexSubImage2D`.
    pub fn copy_tex_sub_image_2d(
        &mut self,
        target: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) {
        let Some((tex_target, face)) =
            image_target(target).filter(|t| matches!(t.0, gl::TEXTURE_2D | gl::TEXTURE_CUBE_MAP))
        else {
            return self.err(gl::INVALID_ENUM);
        };
        self.copy_tex_sub_image(tex_target, face, level, [xoffset, yoffset, 0], x, y, width, height);
    }

    /// `glCopyTexSubImage3D`.
    pub fn copy_tex_sub_image_3d(
        &mut self,
        target: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        zoffset: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) {
        if !matches!(target, gl::TEXTURE_3D | gl::TEXTURE_2D_ARRAY) {
            return self.err(gl::INVALID_ENUM);
        }
        self.copy_tex_sub_image(target, 0, level, [xoffset, yoffset, zoffset], x, y, width, height);
    }

    #[allow(clippy::too_many_arguments)]
    fn copy_tex_sub_image(
        &mut self,
        tex_target: u32,
        face: usize,
        level: i32,
        off: [i32; 3],
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    ) {
        let Some((key, img)) = self.sub_image(tex_target, face, level, off, [w, h, 1]) else { return };
        let src = match self.read_color() {
            Ok(s) => s,
            Err(e) => return self.err(e),
        };
        // The image's format must be one the read buffer can be copied to.
        let base = if img.unsized_format { img.internal.base } else { img.internal.gl };
        if img.compressed.is_some() || copy_format(&src.internal, base).is_err() {
            return self.err(gl::INVALID_OPERATION);
        }
        self.copy_from_read_buffer(&src, key, face, level as u32, off.map(|v| v as u32), x, y, w, h);
    }

    /// Copies the read buffer's rectangle `(x, y, w, h)` to an image at
    /// `off`; pixels outside the read buffer are left alone.
    #[allow(clippy::too_many_arguments)]
    fn copy_from_read_buffer(
        &mut self,
        src: &super::framebuffers::ReadColor,
        key: Key,
        face: usize,
        level: u32,
        off: [u32; 3],
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    ) {
        let (x0, y0) = (x.max(0), y.max(0));
        let x1 = (x as i64 + w as i64).min(src.fb.width as i64) as i32;
        let y1 = (y as i64 + h as i64).min(src.fb.height as i64) as i32;
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let tex = self.textures.get(key);
        let Some((r, l, layer)) = self.image_location(tex, face, level) else { return };
        let dst = Surface { resource: r, level: l, layer: layer + off[2] };
        let rect = crate::backend::Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
        let (dx, dy) = (off[0] + (x0 - x) as u32, off[1] + (y0 - y) as u32);
        self.backend.copy_to_texture(&src.fb, src.index, rect, dst, dx, dy);
    }

    // ---- Mipmaps ------------------------------------------------------------------

    /// `glGenerateMipmap`.
    pub fn generate_mipmap(&mut self, target: u32) {
        if target_index(target).is_none() {
            return self.err(gl::INVALID_ENUM);
        }
        let key = self.bound_texture(target).unwrap();
        let tex = self.textures.get(key);
        let (base, _) = Self::level_range(tex);
        let Some(b) = tex.image(0, base).copied().filter(|_| !tex.external) else {
            return self.err(gl::INVALID_OPERATION);
        };
        let i = b.internal;
        let renderable = i.renderable || (i.float_renderable && self.backend.caps().color_buffer_float);
        let filterable = i.filterable || (i.format.class() == Class::Float && self.backend.caps().float_linear);
        if !(b.unsized_format || (renderable && filterable)) || b.compressed.is_some() || i.format.has_depth() {
            return self.err(gl::INVALID_OPERATION);
        }
        if target == gl::TEXTURE_CUBE_MAP && !Self::cube_complete(tex, base) {
            return self.err(gl::INVALID_OPERATION);
        }
        if b.width == 0 || b.height == 0 || b.depth == 0 {
            return;
        }
        // Levels base + 1 to q are replaced.
        let max_dim = if target == gl::TEXTURE_3D { b.width.max(b.height).max(b.depth) } else { b.width.max(b.height) };
        let p = base + chain_levels(max_dim) - 1;
        let q = match tex.immutable_levels {
            Some(n) => p.min(n - 1).min(tex.max_level.max(base)),
            None => p.min(tex.max_level).min(MAX_LEVELS as u32 - 1),
        };
        if q <= base {
            return;
        }
        let faces = tex.images.len();
        let immutable = tex.immutable_levels.is_some();
        if !immutable {
            for f in 0..faces {
                for l in base + 1..=q {
                    let k = l - base;
                    let depth = if target == gl::TEXTURE_3D { minify(b.depth, k) } else { b.depth };
                    let width = minify(b.width, k);
                    let img = Image { width, height: minify(b.height, k), depth, location: Location::Storage, ..b };
                    // Replace the level: in the storage if it fits there
                    // (the renderer fills it below), else as a new image.
                    let tex = self.textures.get(key);
                    if tex.storage.and_then(|s| Self::storage_level(&s, &img, l)).is_some() {
                        let old = self.textures.get_mut(key).images[f][l as usize].replace(img);
                        if let Some(Location::Own(r)) = old.map(|i| i.location) {
                            self.backend.destroy_resource(r);
                        }
                    } else if !self.define_image(key, f, l, img, None) {
                        return self.err(gl::OUT_OF_MEMORY);
                    }
                }
            }
        }
        let Some(s) = self.finalize(key, base, q) else { return self.err(gl::OUT_OF_MEMORY) };
        if let (Some(from), Some(to)) = (base.checked_sub(s.base), q.checked_sub(s.base)) {
            self.backend.generate_mipmap(s.resource, from, to);
        }
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Where a texture's images are kept, and texture completeness.
//!
//! Each image of a mutable texture can be specified separately with any
//! size and format, while a renderer stores a texture as one resource
//! holding a mipmap chain. Images that fit the texture's resource (its
//! *storage*) live there; images that do not get a resource of their own,
//! and when the texture is used with levels that are not all in its
//! storage, the storage is rebuilt from the base level and every image that
//! fits is copied in (as Mesa's state tracker does). Immutable textures
//! (`TexStorage*`) are one resource from the start.

use alloc::vec::Vec;

use super::samplers::mipmapped;
use super::{chain_levels, resource_target};
use crate::backend::{Region, ResourceDesc, ResourceId, Target, View, minify};
use crate::context::{Context, Image, Key, Location, MAX_LEVELS, SamplerParams, Storage, Texture};
use crate::format::Class;
use crate::gl;
use crate::pixels::{self, Texels};

/// What texture completeness found: the levels to sample and where they
/// are.
pub(crate) struct Complete {
    pub base: u32,
    pub last: u32,
}

impl Context {
    // ---- Storage ----------------------------------------------------------------

    /// The level of storage `s` that would hold `img` as level `level` of
    /// the texture, if it fits there (the same format and size).
    pub(super) fn storage_level(s: &Storage, img: &Image, level: u32) -> Option<u32> {
        let l = level.checked_sub(s.base).filter(|&l| l < s.desc.levels)?;
        let (w, h, d) = s.desc.level_size(l);
        // A cube map's six faces are its storage's layers, one each.
        let d = if s.desc.target == Target::TextureCube { 1 } else { d };
        (s.desc.format == img.internal.format && (w, h, d) == (img.width, img.height, img.depth)).then_some(l)
    }

    /// The layer of a texture's storage a face's images are in.
    fn face_layer(target: u32, face: usize) -> u32 {
        if target == gl::TEXTURE_CUBE_MAP { face as u32 } else { 0 }
    }

    /// Where the texels of an image are: resource, level and first layer
    /// (`None` for an empty image).
    pub(crate) fn image_location(&self, tex: &Texture, face: usize, level: u32) -> Option<(ResourceId, u32, u32)> {
        match tex.image(face, level)?.location {
            Location::Own(r) => Some((r, 0, 0)),
            Location::Empty => None,
            Location::Storage => {
                let s = tex.storage?;
                Some((s.resource, level.checked_sub(s.base)?, Self::face_layer(tex.target, face)))
            }
        }
    }

    /// A resource for an image of its own.
    fn create_image_resource(&mut self, target: u32, img: &Image) -> Option<ResourceId> {
        let target = match target {
            gl::TEXTURE_CUBE_MAP => Target::Texture2D,
            t => resource_target(t),
        };
        let desc = ResourceDesc {
            target,
            format: img.internal.format,
            width: img.width,
            height: img.height,
            depth: img.depth,
            levels: 1,
            samples: 0,
        };
        self.backend.create_resource(&desc).ok()
    }

    /// Creates storage for a texture from an image at `level`, with the
    /// whole mipmap chain below it if `chain`.
    fn create_storage(&mut self, target: u32, img: &Image, level: u32, chain: bool) -> Option<Storage> {
        let depth = match target {
            gl::TEXTURE_CUBE_MAP => 6,
            _ => img.depth,
        };
        let max_dim =
            if target == gl::TEXTURE_3D { img.width.max(img.height).max(img.depth) } else { img.width.max(img.height) };
        let desc = ResourceDesc {
            target: resource_target(target),
            format: img.internal.format,
            width: img.width,
            height: img.height,
            depth,
            levels: if chain { chain_levels(max_dim).min((MAX_LEVELS as u32).saturating_sub(level)).max(1) } else { 1 },
            samples: 0,
        };
        let resource = self.backend.create_resource(&desc).ok()?;
        Some(Storage { resource, desc, base: level })
    }

    /// Defines image `(face, level)` of a texture, with its texels if
    /// given, reading as zero otherwise; returns `false` (and changes
    /// nothing) if memory ran out.
    pub(crate) fn define_image(
        &mut self,
        key: Key,
        face: usize,
        level: u32,
        mut img: Image,
        data: Option<&Texels<'_>>,
    ) -> bool {
        let tex = self.textures.get(key);
        let target = tex.target;
        let old = tex.image(face, level).map(|i| i.location);
        let chain = mipmapped(tex.sampler.min_filter);
        let mut storage = tex.storage;
        // A resource made here reads as zero already; a level of the
        // storage holds the image this one replaces.
        let mut fresh = false;
        if storage.is_none() && !img.is_empty() {
            let Some(s) = self.create_storage(target, &img, level, chain) else { return false };
            self.textures.get_mut(key).storage = Some(s);
            storage = Some(s);
            fresh = true;
        }
        let in_storage = storage.and_then(|s| Some((s, Self::storage_level(&s, &img, level)?)));
        let (resource, l, layer) = match in_storage {
            _ if img.is_empty() => {
                img.location = Location::Empty;
                (None, 0, 0)
            }
            Some((s, l)) => {
                img.location = Location::Storage;
                (Some(s.resource), l, Self::face_layer(target, face))
            }
            None => {
                let Some(r) = self.create_image_resource(target, &img) else { return false };
                img.location = Location::Own(r);
                fresh = true;
                (Some(r), 0, 0)
            }
        };
        // Undefined contents are zero, never an earlier image's data.
        let tb = img.internal.format.bytes();
        let zeros = match (resource, data, fresh) {
            (Some(_), None, false) => {
                match pixels::try_zeroed(tb * img.width as usize * img.height as usize * img.depth as usize) {
                    Some(z) => Some(Texels::packed(z, img.width, img.height, tb)),
                    None => return false,
                }
            }
            _ => None,
        };
        if let Some(Location::Own(r)) = old {
            self.backend.destroy_resource(r);
        }
        if let (Some(r), Some(t)) = (resource, data.or(zeros.as_ref())) {
            let region = Region::new(0, 0, layer, img.width, img.height, img.depth);
            self.backend.write(r, l, region, t.data(), t.row, t.image);
        }
        self.textures.get_mut(key).images[face][level as usize] = Some(img);
        true
    }

    /// Makes the storage of a texture hold levels `base..=last` (of every
    /// face), rebuilding it if needed; returns the storage, or `None` if
    /// memory ran out (the texture is then as it was).
    pub(crate) fn finalize(&mut self, key: Key, base: u32, last: u32) -> Option<Storage> {
        let tex = self.textures.get(key);
        let faces = tex.images.len();
        let target = tex.target;
        let in_storage = |l: u32| (0..faces).all(|f| tex.image(f, l).is_some_and(|i| i.location == Location::Storage));
        if tex.storage.is_some() && (base..=last).all(in_storage) {
            return tex.storage;
        }
        let b = *tex.image(0, base)?;
        let old = tex.storage;
        let new = self.create_storage(target, &b, base, true)?;
        // Every image moves into the new storage if it fits there, and out
        // of the old one into a resource of its own if it does not. The
        // resources are made first: running out of memory half way leaves
        // the texture as it was.
        let tex = self.textures.get(key);
        let mut planned: Vec<(usize, u32, Image, Option<u32>)> = Vec::new();
        for f in 0..faces {
            for l in 0..MAX_LEVELS as u32 {
                let Some(&img) = tex.image(f, l) else { continue };
                match (img.location, Self::storage_level(&new, &img, l)) {
                    (Location::Empty, _) | (Location::Own(_), None) => {}
                    (_, at) => planned.push((f, l, img, at)),
                }
            }
        }
        // Each image's new place: resource, level and layer, and location.
        struct Move {
            face: usize,
            level: u32,
            image: Image,
            to: (ResourceId, u32, u32),
            location: Location,
        }
        let mut moves: Vec<Move> = Vec::new();
        for (face, level, image, at) in planned {
            let (to, location) = match at {
                Some(at) => ((new.resource, at, Self::face_layer(target, face)), Location::Storage),
                None => match self.create_image_resource(target, &image) {
                    Some(r) => ((r, 0, 0), Location::Own(r)),
                    None => {
                        for m in moves {
                            if let Location::Own(r) = m.location {
                                self.backend.destroy_resource(r);
                            }
                        }
                        self.backend.destroy_resource(new.resource);
                        return None;
                    }
                },
            };
            moves.push(Move { face, level, image, to, location });
        }
        for Move { face: f, level: l, image: mut img, to: (dst, dst_level, dst_layer), location } in moves {
            let from = match img.location {
                Location::Own(r) => Some((r, 0, 0)),
                _ => old.and_then(|o| Some((o.resource, l.checked_sub(o.base)?, Self::face_layer(target, f)))),
            };
            if let Some((src, src_level, src_layer)) = from {
                let region = Region::new(0, 0, src_layer, img.width, img.height, img.depth);
                self.backend.copy_region(src, src_level, region, dst, dst_level, 0, 0, dst_layer);
            }
            if let Location::Own(r) = img.location {
                self.backend.destroy_resource(r);
            }
            img.location = location;
            self.textures.get_mut(key).images[f][l as usize] = Some(img);
        }
        if let Some(o) = old {
            self.backend.destroy_resource(o.resource);
        }
        self.textures.get_mut(key).storage = Some(new);
        Some(new)
    }

    // ---- Completeness --------------------------------------------------------

    /// The effective base and maximum levels of a texture
    /// (section 3.8.10.4): `(base, q)` for mipmapped filtering.
    pub(crate) fn level_range(tex: &Texture) -> (u32, u32) {
        match tex.immutable_levels {
            Some(n) => {
                let base = tex.base_level.min(n - 1);
                let max = tex.max_level.clamp(base, n - 1);
                (base, max)
            }
            None => {
                let base = tex.base_level.min(MAX_LEVELS as u32 - 1);
                let Some(b) = tex.image(0, base) else { return (base, base) };
                let dim = if tex.target == gl::TEXTURE_3D {
                    b.width.max(b.height).max(b.depth)
                } else {
                    b.width.max(b.height)
                };
                let p = base + chain_levels(dim) - 1;
                (base, p.min(tex.max_level).min(MAX_LEVELS as u32 - 1).max(base))
            }
        }
    }

    /// Whether levels `base..=q` form a mipmap chain (and, for cube maps,
    /// whether the faces agree).
    pub(crate) fn mipmap_complete(tex: &Texture, base: u32, q: u32) -> bool {
        let Some(b) = tex.image(0, base) else { return false };
        for f in 0..tex.images.len() {
            for l in base..=q {
                let Some(i) = tex.image(f, l) else { return false };
                let k = l - base;
                let depth = if tex.target == gl::TEXTURE_3D { minify(b.depth, k) } else { b.depth };
                if i.internal != b.internal
                    || (i.width, i.height, i.depth) != (minify(b.width, k), minify(b.height, k), depth)
                {
                    return false;
                }
            }
        }
        true
    }

    /// Cube completeness: the faces' base images are square and agree.
    pub(super) fn cube_complete(tex: &Texture, base: u32) -> bool {
        let Some(b) = tex.image(0, base) else { return false };
        b.width == b.height
            && (0..6).all(|f| {
                tex.image(f, base)
                    .is_some_and(|i| i.internal == b.internal && i.width == b.width && i.height == b.height)
            })
    }

    /// Whether a texture is complete when sampled with `p` (section
    /// 3.8.13), and the levels it samples.
    pub(crate) fn completeness(&self, key: Key, p: &SamplerParams) -> Option<Complete> {
        let tex = self.textures.get(key);
        let (base, q) = Self::level_range(tex);
        if tex.immutable_levels.is_none() && tex.base_level > tex.max_level {
            return None;
        }
        let b = tex.image(0, base)?;
        if b.width == 0 || b.height == 0 || b.depth == 0 {
            return None;
        }
        let mip = mipmapped(p.min_filter);
        let last = if mip { q } else { base };
        if tex.target == gl::TEXTURE_CUBE_MAP && !Self::cube_complete(tex, base) {
            return None;
        }
        if mip && !Self::mipmap_complete(tex, base, last) {
            return None;
        }
        if tex.target != gl::TEXTURE_CUBE_MAP && !Self::mipmap_complete(tex, base, base) {
            return None;
        }
        let linear = p.mag_filter != gl::NEAREST || !matches!(p.min_filter, gl::NEAREST | gl::NEAREST_MIPMAP_NEAREST);
        let i = b.internal;
        let class = i.format.class();
        if linear {
            // Integer textures, depth textures without comparison, and
            // float textures the renderer cannot filter need NEAREST.
            if i.format.is_integer() {
                return None;
            }
            // Depth textures filter only their comparisons (percentage
            // closer filtering).
            if matches!(class, Class::Depth | Class::DepthStencil) {
                if p.compare_mode == gl::NONE {
                    return None;
                }
            } else if !i.filterable && !(class == Class::Float && self.backend.caps().float_linear) {
                return None;
            }
        }
        Some(Complete { base, last })
    }

    /// The view a sampler uses for the texture `key` with parameters `p`,
    /// or `None` if the texture is incomplete.
    pub(crate) fn texture_view(&mut self, key: Key, p: &SamplerParams) -> Option<View> {
        let c = self.completeness(key, p)?;
        let tex = self.textures.get(key);
        let target = resource_target(tex.target);
        let swizzle = tex.swizzle.map(|s| match s {
            gl::RED => 0,
            gl::GREEN => 1,
            gl::BLUE => 2,
            gl::ALPHA => 3,
            gl::ZERO => 4,
            _ => 5,
        });
        // A single level in a resource of its own is used as it is.
        if c.base == c.last
            && tex.target != gl::TEXTURE_CUBE_MAP
            && let Some(Location::Own(r)) = tex.image(0, c.base).map(|i| i.location)
        {
            return Some(View { resource: r, target, base_level: 0, max_level: 0, swizzle });
        }
        let s = self.finalize(key, c.base, c.last)?;
        Some(View { resource: s.resource, target, base_level: c.base - s.base, max_level: c.last - s.base, swizzle })
    }
}

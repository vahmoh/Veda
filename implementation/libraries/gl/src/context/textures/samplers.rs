// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Sampling parameters: a texture's own, and sampler objects', which take
//! the place of a texture's on the units they are bound to; and the
//! renderer's sampler state they make.

use crate::backend::{Filter, Func, SamplerState, Wrap};
use crate::context::{Context, Sampler, SamplerParams, TEXTURE_UNITS, compare_func};
use crate::gl;

fn filter_valid(f: u32, min: bool) -> bool {
    match f {
        gl::NEAREST | gl::LINEAR => true,
        gl::NEAREST_MIPMAP_NEAREST
        | gl::LINEAR_MIPMAP_NEAREST
        | gl::NEAREST_MIPMAP_LINEAR
        | gl::LINEAR_MIPMAP_LINEAR => min,
        _ => false,
    }
}

/// Whether a minification filter samples mipmaps.
pub(super) fn mipmapped(min_filter: u32) -> bool {
    !matches!(min_filter, gl::NEAREST | gl::LINEAR)
}

fn wrap(w: u32) -> Wrap {
    match w {
        gl::CLAMP_TO_EDGE => Wrap::ClampToEdge,
        gl::MIRRORED_REPEAT => Wrap::MirroredRepeat,
        _ => Wrap::Repeat,
    }
}

/// The backend's sampler state for GL sampler parameters.
pub(crate) fn sampler_state(p: &SamplerParams) -> SamplerState {
    let (min, mip) = match p.min_filter {
        gl::NEAREST => (Filter::Nearest, None),
        gl::LINEAR => (Filter::Linear, None),
        gl::NEAREST_MIPMAP_NEAREST => (Filter::Nearest, Some(Filter::Nearest)),
        gl::LINEAR_MIPMAP_NEAREST => (Filter::Linear, Some(Filter::Nearest)),
        gl::NEAREST_MIPMAP_LINEAR => (Filter::Nearest, Some(Filter::Linear)),
        _ => (Filter::Linear, Some(Filter::Linear)),
    };
    SamplerState {
        min,
        mag: if p.mag_filter == gl::NEAREST { Filter::Nearest } else { Filter::Linear },
        mip,
        wrap: p.wrap.map(wrap),
        min_lod: p.min_lod,
        max_lod: p.max_lod,
        compare: (p.compare_mode == gl::COMPARE_REF_TO_TEXTURE)
            .then(|| compare_func(p.compare_func).unwrap_or(Func::LessEqual)),
        max_anisotropy: p.max_anisotropy,
    }
}

/// How a parameter value was given.
#[derive(Clone, Copy)]
enum Param {
    I(i32),
    F(f32),
}

impl Param {
    fn int(self) -> i32 {
        match self {
            Param::I(i) => i,
            // Enums and integers given as floats.
            Param::F(f) => libm::roundf(f) as i32,
        }
    }

    fn float(self) -> f32 {
        match self {
            Param::I(i) => i as f32,
            Param::F(f) => f,
        }
    }

    fn enum_(self) -> u32 {
        match self {
            Param::I(i) => i as u32,
            Param::F(f) => {
                if f >= 0.0 && f == libm::roundf(f) && f < 4_294_967_296.0 {
                    f as u32
                } else {
                    u32::MAX
                }
            }
        }
    }
}

impl Context {
    // ---- Parameters ------------------------------------------------------------

    /// Sets a sampler parameter shared by textures and sampler objects;
    /// `None` if `pname` is not one.
    fn set_sampler_param(p: &mut SamplerParams, pname: u32, v: Param) -> Option<Result<(), u32>> {
        let e = v.enum_();
        Some(match pname {
            gl::TEXTURE_MIN_FILTER if filter_valid(e, true) => {
                p.min_filter = e;
                Ok(())
            }
            gl::TEXTURE_MAG_FILTER if filter_valid(e, false) => {
                p.mag_filter = e;
                Ok(())
            }
            gl::TEXTURE_WRAP_S | gl::TEXTURE_WRAP_T | gl::TEXTURE_WRAP_R
                if matches!(e, gl::REPEAT | gl::CLAMP_TO_EDGE | gl::MIRRORED_REPEAT) =>
            {
                let i = match pname {
                    gl::TEXTURE_WRAP_S => 0,
                    gl::TEXTURE_WRAP_T => 1,
                    _ => 2,
                };
                p.wrap[i] = e;
                Ok(())
            }
            gl::TEXTURE_MIN_LOD => {
                p.min_lod = v.float();
                Ok(())
            }
            gl::TEXTURE_MAX_LOD => {
                p.max_lod = v.float();
                Ok(())
            }
            gl::TEXTURE_COMPARE_MODE if matches!(e, gl::NONE | gl::COMPARE_REF_TO_TEXTURE) => {
                p.compare_mode = e;
                Ok(())
            }
            gl::TEXTURE_COMPARE_FUNC if compare_func(e).is_some() => {
                p.compare_func = e;
                Ok(())
            }
            gl::TEXTURE_MAX_ANISOTROPY_EXT => {
                let f = v.float();
                if f.is_nan() || f < 1.0 {
                    Err(gl::INVALID_VALUE)
                } else {
                    p.max_anisotropy = f;
                    Ok(())
                }
            }
            gl::TEXTURE_MIN_FILTER
            | gl::TEXTURE_MAG_FILTER
            | gl::TEXTURE_WRAP_S
            | gl::TEXTURE_WRAP_T
            | gl::TEXTURE_WRAP_R
            | gl::TEXTURE_COMPARE_MODE
            | gl::TEXTURE_COMPARE_FUNC => Err(gl::INVALID_ENUM),
            _ => return None,
        })
    }

    fn tex_parameter(&mut self, target: u32, pname: u32, v: Param) {
        let Some(key) = self.bound_texture(target) else { return self.err(gl::INVALID_ENUM) };
        let max_aniso = self.backend.caps().max_anisotropy;
        let t = self.textures.get_mut(key);
        if let Some(r) = Self::set_sampler_param(&mut t.sampler, pname, v) {
            t.sampler.max_anisotropy = t.sampler.max_anisotropy.min(max_aniso);
            if let Err(e) = r {
                self.err(e);
            }
            return;
        }
        let e = v.enum_();
        match pname {
            gl::TEXTURE_BASE_LEVEL | gl::TEXTURE_MAX_LEVEL => {
                let i = v.int();
                if i < 0 {
                    return self.err(gl::INVALID_VALUE);
                }
                if pname == gl::TEXTURE_BASE_LEVEL {
                    t.base_level = i as u32;
                } else {
                    t.max_level = i as u32;
                }
            }
            gl::TEXTURE_SWIZZLE_R | gl::TEXTURE_SWIZZLE_G | gl::TEXTURE_SWIZZLE_B | gl::TEXTURE_SWIZZLE_A => {
                if !matches!(e, gl::RED | gl::GREEN | gl::BLUE | gl::ALPHA | gl::ZERO | gl::ONE) {
                    return self.err(gl::INVALID_ENUM);
                }
                t.swizzle[(pname - gl::TEXTURE_SWIZZLE_R) as usize] = e;
            }
            _ => self.err(gl::INVALID_ENUM),
        }
    }

    /// `glTexParameteri`.
    pub fn tex_parameteri(&mut self, target: u32, pname: u32, param: i32) {
        self.tex_parameter(target, pname, Param::I(param));
    }

    /// `glTexParameterf`.
    pub fn tex_parameterf(&mut self, target: u32, pname: u32, param: f32) {
        self.tex_parameter(target, pname, Param::F(param));
    }

    /// The value of a texture or sampler parameter.
    fn sampler_param(p: &SamplerParams, pname: u32) -> Option<Param> {
        Some(match pname {
            gl::TEXTURE_MIN_FILTER => Param::I(p.min_filter as i32),
            gl::TEXTURE_MAG_FILTER => Param::I(p.mag_filter as i32),
            gl::TEXTURE_WRAP_S => Param::I(p.wrap[0] as i32),
            gl::TEXTURE_WRAP_T => Param::I(p.wrap[1] as i32),
            gl::TEXTURE_WRAP_R => Param::I(p.wrap[2] as i32),
            gl::TEXTURE_MIN_LOD => Param::F(p.min_lod),
            gl::TEXTURE_MAX_LOD => Param::F(p.max_lod),
            gl::TEXTURE_COMPARE_MODE => Param::I(p.compare_mode as i32),
            gl::TEXTURE_COMPARE_FUNC => Param::I(p.compare_func as i32),
            gl::TEXTURE_MAX_ANISOTROPY_EXT => Param::F(p.max_anisotropy),
            _ => return None,
        })
    }

    fn get_tex_parameter(&mut self, target: u32, pname: u32) -> Option<Param> {
        let Some(key) = self.bound_texture(target) else {
            self.err(gl::INVALID_ENUM);
            return None;
        };
        let t = self.textures.get(key);
        if let Some(p) = Self::sampler_param(&t.sampler, pname) {
            return Some(p);
        }
        Some(match pname {
            gl::TEXTURE_BASE_LEVEL => Param::I(t.base_level.min(i32::MAX as u32) as i32),
            gl::TEXTURE_MAX_LEVEL => Param::I(t.max_level.min(i32::MAX as u32) as i32),
            gl::TEXTURE_SWIZZLE_R | gl::TEXTURE_SWIZZLE_G | gl::TEXTURE_SWIZZLE_B | gl::TEXTURE_SWIZZLE_A => {
                Param::I(t.swizzle[(pname - gl::TEXTURE_SWIZZLE_R) as usize] as i32)
            }
            gl::TEXTURE_IMMUTABLE_FORMAT => Param::I(i32::from(t.immutable_levels.is_some())),
            gl::TEXTURE_IMMUTABLE_LEVELS => Param::I(t.immutable_levels.unwrap_or(0) as i32),
            _ => {
                self.err(gl::INVALID_ENUM);
                return None;
            }
        })
    }

    /// `glGetTexParameteriv` (one value).
    pub fn get_tex_parameteri(&mut self, target: u32, pname: u32) -> i32 {
        self.get_tex_parameter(target, pname).map_or(0, |p| match p {
            Param::I(i) => i,
            Param::F(f) => libm::roundf(f) as i32,
        })
    }

    /// `glGetTexParameterfv` (one value).
    pub fn get_tex_parameterf(&mut self, target: u32, pname: u32) -> f32 {
        self.get_tex_parameter(target, pname).map_or(0.0, Param::float)
    }

    // ---- Sampler objects ----------------------------------------------------------

    /// `glGenSamplers`.
    pub fn gen_samplers(&mut self, names: &mut [u32]) {
        for n in names {
            let name = self.samplers.generate();
            self.samplers.create(name, Sampler::default());
            *n = name;
        }
    }

    /// One sampler (created at once, as `glGenSamplers` does).
    pub fn gen_sampler(&mut self) -> u32 {
        let mut n = [0];
        self.gen_samplers(&mut n);
        n[0]
    }

    /// `glIsSampler`.
    pub fn is_sampler(&self, name: u32) -> bool {
        self.samplers.key(name).is_some()
    }

    /// `glDeleteSamplers`.
    pub fn delete_samplers(&mut self, names: &[u32]) {
        for &name in names {
            if let Some(key) = self.samplers.key(name) {
                for unit in &mut self.sampler_units {
                    if *unit == Some(key) {
                        *unit = None;
                    }
                }
            }
            let _ = self.samplers.delete(name);
        }
    }

    /// `glBindSampler`.
    pub fn bind_sampler(&mut self, unit: u32, sampler: u32) {
        if unit as usize >= TEXTURE_UNITS {
            return self.err(gl::INVALID_VALUE);
        }
        let key = if sampler == 0 {
            None
        } else {
            match self.samplers.key(sampler) {
                Some(k) => Some(k),
                None => return self.err(gl::INVALID_OPERATION),
            }
        };
        self.sampler_units[unit as usize] = key;
    }

    fn sampler_parameter(&mut self, sampler: u32, pname: u32, v: Param) {
        let Some(key) = self.samplers.key(sampler) else { return self.err(gl::INVALID_OPERATION) };
        let max_aniso = self.backend.caps().max_anisotropy;
        let s = self.samplers.get_mut(key);
        match Self::set_sampler_param(&mut s.params, pname, v) {
            Some(Ok(())) => s.params.max_anisotropy = s.params.max_anisotropy.min(max_aniso),
            Some(Err(e)) => self.err(e),
            None => self.err(gl::INVALID_ENUM),
        }
    }

    /// `glSamplerParameteri`.
    pub fn sampler_parameteri(&mut self, sampler: u32, pname: u32, param: i32) {
        self.sampler_parameter(sampler, pname, Param::I(param));
    }

    /// `glSamplerParameterf`.
    pub fn sampler_parameterf(&mut self, sampler: u32, pname: u32, param: f32) {
        self.sampler_parameter(sampler, pname, Param::F(param));
    }

    fn get_sampler_param(&mut self, sampler: u32, pname: u32) -> Option<Param> {
        let Some(key) = self.samplers.key(sampler) else {
            self.err(gl::INVALID_OPERATION);
            return None;
        };
        let p = Self::sampler_param(&self.samplers.get(key).params, pname);
        if p.is_none() {
            self.err(gl::INVALID_ENUM);
        }
        p
    }

    /// `glGetSamplerParameteriv` (one value).
    pub fn get_sampler_parameteri(&mut self, sampler: u32, pname: u32) -> i32 {
        self.get_sampler_param(sampler, pname).map_or(0, |p| match p {
            Param::I(i) => i,
            Param::F(f) => libm::roundf(f) as i32,
        })
    }

    /// `glGetSamplerParameterfv` (one value).
    pub fn get_sampler_parameterf(&mut self, sampler: u32, pname: u32) -> f32 {
        self.get_sampler_param(sampler, pname).map_or(0.0, Param::float)
    }
}

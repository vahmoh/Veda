//! Post-processing on the GPU: the scene is drawn in high dynamic range
//! (half floats) with 4x multisampling, resolved, bloomed and tone mapped
//! into the window's framebuffer.
//!
//! * **Bloom.** The bright parts glow: the resolved frame is filtered
//!   down a chain of half-size images (13 taps each, the first keeping
//!   only what is brighter than a soft threshold), then back up (a tent
//!   filter, each level added to the next larger one).
//! * **The look.** Exposure, a filmic curve (ACES), the grade of the
//!   time of day (lift, gamma, gain, saturation), a vignette, and
//!   dithering so that dark gradients (a night sky) do not band.

use alloc::string::String;
use alloc::vec::Vec;

use vgl::{Context, gl};

use super::gpu::Program;
use super::shaders::source;

/// How many bloom levels (each half the size of the one before).
const LEVELS: usize = 6;

/// The grade of a moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grade {
    pub exposure: f32,
    pub bloom: f32,
    pub saturation: f32,
    pub contrast: f32,
    /// Added to the shadows, multiplying the highlights.
    pub lift: [f32; 3],
    pub gain: [f32; 3],
    pub vignette: f32,
}

const FULLSCREEN: &str = r#"
in vec2 a_pos;
out vec2 v_uv;
void main() {
    v_uv = a_pos * 0.5 + 0.5;
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
"#;

/// A 13-tap downsample (two boxes of four, one of nine), weighted to
/// keep fireflies from flickering; the first level keeps only what is
/// brighter than the threshold, with a soft knee.
const DOWN_FS: &str = r#"
uniform sampler2D u_src;
uniform vec2 u_texel;
uniform float u_threshold;
in vec2 v_uv;
out vec4 o_color;

vec3 tap(vec2 o) {
    return texture(u_src, v_uv + o * u_texel).rgb;
}

void main() {
    vec3 a = tap(vec2(-2.0, -2.0));
    vec3 b = tap(vec2(0.0, -2.0));
    vec3 c = tap(vec2(2.0, -2.0));
    vec3 d = tap(vec2(-1.0, -1.0));
    vec3 e = tap(vec2(1.0, -1.0));
    vec3 f = tap(vec2(-2.0, 0.0));
    vec3 g = tap(vec2(0.0, 0.0));
    vec3 h = tap(vec2(2.0, 0.0));
    vec3 i = tap(vec2(-1.0, 1.0));
    vec3 j = tap(vec2(1.0, 1.0));
    vec3 k = tap(vec2(-2.0, 2.0));
    vec3 l = tap(vec2(0.0, 2.0));
    vec3 m = tap(vec2(2.0, 2.0));
    vec3 c0 = (d + e + i + j) * 0.125;
    vec3 c1 = (a + b + f + g) * 0.03125;
    vec3 c2 = (b + c + g + h) * 0.03125;
    vec3 c3 = (f + g + k + l) * 0.03125;
    vec3 c4 = (g + h + l + m) * 0.03125;
    vec3 col = c0 + c1 + c2 + c3 + c4;
    if (u_threshold > 0.0) {
        float bright = max(col.r, max(col.g, col.b));
        float knee = u_threshold * 0.5;
        float soft = clamp(bright - u_threshold + knee, 0.0, 2.0 * knee);
        soft = soft * soft / (4.0 * knee + 1e-4);
        float keep = max(soft, bright - u_threshold) / max(bright, 1e-4);
        col *= keep;
        // Too bright to be anything but a highlight: tamed.
        col = min(col, vec3(64.0));
    }
    o_color = vec4(col, 1.0);
}
"#;

/// A 3x3 tent upsample, added onto the larger level.
const UP_FS: &str = r#"
uniform sampler2D u_src;
uniform vec2 u_texel;
uniform float u_weight;
in vec2 v_uv;
out vec4 o_color;
void main() {
    vec3 s = texture(u_src, v_uv + vec2(-1.0, -1.0) * u_texel).rgb;
    s += texture(u_src, v_uv + vec2(0.0, -1.0) * u_texel).rgb * 2.0;
    s += texture(u_src, v_uv + vec2(1.0, -1.0) * u_texel).rgb;
    s += texture(u_src, v_uv + vec2(-1.0, 0.0) * u_texel).rgb * 2.0;
    s += texture(u_src, v_uv).rgb * 4.0;
    s += texture(u_src, v_uv + vec2(1.0, 0.0) * u_texel).rgb * 2.0;
    s += texture(u_src, v_uv + vec2(-1.0, 1.0) * u_texel).rgb;
    s += texture(u_src, v_uv + vec2(0.0, 1.0) * u_texel).rgb * 2.0;
    s += texture(u_src, v_uv + vec2(1.0, 1.0) * u_texel).rgb;
    o_color = vec4(s / 16.0 * u_weight, 1.0);
}
"#;

/// The frame and its bloom, exposed, tone mapped, graded, vignetted and
/// dithered.
const COMPOSITE_FS: &str = r#"
uniform sampler2D u_scene;
uniform sampler2D u_bloom;
uniform float u_exposure;
uniform float u_bloomStrength;
uniform float u_saturation;
uniform float u_contrast;
uniform vec3 u_lift;
uniform vec3 u_gain;
uniform float u_vignette;
uniform vec2 u_size;
in vec2 v_uv;
out vec4 o_color;

vec3 aces(vec3 x) {
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), 0.0, 1.0);
}

void main() {
    vec3 c = texture(u_scene, v_uv).rgb;
    vec3 bloom = texture(u_bloom, v_uv).rgb;
    c = (c + bloom * u_bloomStrength) * u_exposure;
    // Grade in linear light: lift the shadows, scale the highlights.
    c = c * u_gain + u_lift * (1.0 - c / (1.0 + c));
    vec3 t = aces(c);
    float luma = dot(t, vec3(0.2126, 0.7152, 0.0722));
    t = mix(vec3(luma), t, u_saturation);
    t = clamp((t - 0.5) * u_contrast + 0.5, 0.0, 1.0);
    // The vignette.
    vec2 d = v_uv - 0.5;
    d.x *= u_size.x / u_size.y;
    t *= 1.0 - u_vignette * smoothstep(0.35, 1.0, length(d) * 1.3);
    // To sRGB, dithered by a pixel's worth of noise.
    vec3 srgb = pow(t, vec3(1.0 / 2.2));
    vec2 p = gl_FragCoord.xy;
    float n = fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453) + fract(sin(dot(p, vec2(39.346, 11.135))) * 24634.6345) - 1.0;
    o_color = vec4(srgb + n / 255.0, 1.0);
}
"#;

/// An image the chain draws into.
struct Target {
    fbo: u32,
    tex: u32,
    w: i32,
    h: i32,
}

/// The targets and programs of post-processing.
pub struct Post {
    down: Program,
    up: Program,
    composite: Program,
    /// The multisampled scene (colour and depth renderbuffers) and its
    /// resolved colour.
    msaa: Option<(u32, u32, u32)>,
    scene: Option<Target>,
    bloom: Vec<Target>,
    size: (i32, i32),
    samples: i32,
    fullscreen: u32,
}

fn target(gl: &mut Context, w: i32, h: i32, format: u32) -> Target {
    let tex = gl.gen_texture();
    gl.bind_texture(gl::TEXTURE_2D, tex);
    gl.tex_storage_2d(gl::TEXTURE_2D, 1, format, w, h);
    for (p, v) in [
        (gl::TEXTURE_MIN_FILTER, gl::LINEAR),
        (gl::TEXTURE_MAG_FILTER, gl::LINEAR),
        (gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE),
        (gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE),
    ] {
        gl.tex_parameteri(gl::TEXTURE_2D, p, v as i32);
    }
    let fbo = gl.gen_framebuffer();
    gl.bind_framebuffer(gl::FRAMEBUFFER, fbo);
    gl.framebuffer_texture_2d(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, tex, 0);
    gl.bind_framebuffer(gl::FRAMEBUFFER, 0);
    Target { fbo, tex, w, h }
}

impl Post {
    /// The programs; the targets come at the first frame's size.
    pub fn new(gl: &mut Context, fullscreen: u32) -> Result<Post, String> {
        let prog =
            |gl: &mut Context, name: &str, fs: &str, uniforms: &[&'static str], samplers: &[(&'static str, u32)]| {
                Program::new(gl, name, &source("", false, FULLSCREEN), &source("", false, fs), uniforms, samplers)
            };
        let down = prog(gl, "bloom down", DOWN_FS, &["u_texel", "u_threshold"], &[("u_src", 0)])?;
        let up = prog(gl, "bloom up", UP_FS, &["u_texel", "u_weight"], &[("u_src", 0)])?;
        let composite = prog(
            gl,
            "composite",
            COMPOSITE_FS,
            &[
                "u_exposure",
                "u_bloomStrength",
                "u_saturation",
                "u_contrast",
                "u_lift",
                "u_gain",
                "u_vignette",
                "u_size",
            ],
            &[("u_scene", 0), ("u_bloom", 1)],
        )?;
        let samples = if gl.backend().caps().max_samples >= 4 { 4 } else { 0 };
        Ok(Post { down, up, composite, msaa: None, scene: None, bloom: Vec::new(), size: (0, 0), samples, fullscreen })
    }

    fn release(&mut self, gl: &mut Context) {
        if let Some((fbo, color, depth)) = self.msaa.take() {
            gl.delete_framebuffers(&[fbo]);
            gl.delete_renderbuffers(&[color, depth]);
        }
        for t in self.scene.take().into_iter().chain(self.bloom.drain(..)) {
            gl.delete_framebuffers(&[t.fbo]);
            gl.delete_textures(&[t.tex]);
        }
    }

    /// Binds the framebuffer the scene is drawn into (made for `w` x `h`
    /// if that changed).
    pub fn begin(&mut self, gl: &mut Context, w: i32, h: i32) {
        if self.size != (w, h) {
            self.release(gl);
            self.size = (w, h);
            let scene = target(gl, w, h, gl::RGBA16F);
            let color = gl.gen_renderbuffer();
            gl.bind_renderbuffer(gl::RENDERBUFFER, color);
            gl.renderbuffer_storage_multisample(gl::RENDERBUFFER, self.samples, gl::RGBA16F, w, h);
            let depth = gl.gen_renderbuffer();
            gl.bind_renderbuffer(gl::RENDERBUFFER, depth);
            gl.renderbuffer_storage_multisample(gl::RENDERBUFFER, self.samples, gl::DEPTH24_STENCIL8, w, h);
            let fbo = gl.gen_framebuffer();
            gl.bind_framebuffer(gl::FRAMEBUFFER, fbo);
            gl.framebuffer_renderbuffer(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::RENDERBUFFER, color);
            gl.framebuffer_renderbuffer(gl::FRAMEBUFFER, gl::DEPTH_STENCIL_ATTACHMENT, gl::RENDERBUFFER, depth);
            self.msaa = Some((fbo, color, depth));
            self.scene = Some(scene);
            let (mut bw, mut bh) = (w / 2, h / 2);
            for _ in 0..LEVELS {
                let t = target(gl, bw.max(1), bh.max(1), gl::RGBA16F);
                self.bloom.push(t);
                bw /= 2;
                bh /= 2;
            }
        }
        let fbo = self.msaa.as_ref().map_or(0, |m| m.0);
        gl.bind_framebuffer(gl::FRAMEBUFFER, fbo);
        gl.viewport(0, 0, w, h);
    }

    /// Resolves the scene, blooms it and draws the result, graded, into
    /// the window's framebuffer.
    pub fn finish(&mut self, gl: &mut Context, grade: &Grade) {
        let (w, h) = self.size;
        let (Some(msaa), Some(scene)) = (self.msaa.as_ref(), self.scene.as_ref()) else { return };
        gl.bind_framebuffer(gl::READ_FRAMEBUFFER, msaa.0);
        gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, scene.fbo);
        gl.blit_framebuffer(0, 0, w, h, 0, 0, w, h, gl::COLOR_BUFFER_BIT, gl::NEAREST);
        gl.disable(gl::DEPTH_TEST);
        gl.disable(gl::CULL_FACE);
        gl.disable(gl::BLEND);
        gl.depth_mask(false);
        gl.bind_vertex_array(self.fullscreen);
        gl.active_texture(gl::TEXTURE0);

        // Down the chain.
        gl.use_program(self.down.id);
        let mut src = (scene.tex, scene.w, scene.h);
        for (i, t) in self.bloom.iter().enumerate() {
            gl.bind_framebuffer(gl::FRAMEBUFFER, t.fbo);
            gl.viewport(0, 0, t.w, t.h);
            gl.bind_texture(gl::TEXTURE_2D, src.0);
            gl.uniform2f(self.down.at("u_texel"), 1.0 / src.1 as f32, 1.0 / src.2 as f32);
            // What is brighter than white once exposed blooms.
            let threshold = 0.85 / grade.exposure.max(1e-3);
            gl.uniform1f(self.down.at("u_threshold"), if i == 0 { threshold } else { 0.0 });
            gl.draw_arrays(gl::TRIANGLES, 0, 3);
            src = (t.tex, t.w, t.h);
        }
        // And back up, each level added to the next larger.
        gl.use_program(self.up.id);
        gl.enable(gl::BLEND);
        gl.blend_func(gl::ONE, gl::ONE);
        for i in (1..self.bloom.len()).rev() {
            let (from, to) = (&self.bloom[i], &self.bloom[i - 1]);
            gl.bind_framebuffer(gl::FRAMEBUFFER, to.fbo);
            gl.viewport(0, 0, to.w, to.h);
            gl.bind_texture(gl::TEXTURE_2D, from.tex);
            gl.uniform2f(self.up.at("u_texel"), 1.0 / from.w as f32, 1.0 / from.h as f32);
            gl.uniform1f(self.up.at("u_weight"), 1.0);
            gl.draw_arrays(gl::TRIANGLES, 0, 3);
        }
        gl.disable(gl::BLEND);

        // The picture.
        gl.bind_framebuffer(gl::FRAMEBUFFER, 0);
        gl.viewport(0, 0, w, h);
        gl.use_program(self.composite.id);
        let p = &self.composite;
        gl.active_texture(gl::TEXTURE0);
        gl.bind_texture(gl::TEXTURE_2D, scene.tex);
        gl.active_texture(gl::TEXTURE1);
        gl.bind_texture(gl::TEXTURE_2D, self.bloom[0].tex);
        gl.active_texture(gl::TEXTURE0);
        gl.uniform1f(p.at("u_exposure"), grade.exposure);
        // The bloom's levels add up to about their count: normalised.
        gl.uniform1f(p.at("u_bloomStrength"), grade.bloom / LEVELS as f32);
        gl.uniform1f(p.at("u_saturation"), grade.saturation);
        gl.uniform1f(p.at("u_contrast"), grade.contrast);
        gl.uniform3f(p.at("u_lift"), grade.lift[0], grade.lift[1], grade.lift[2]);
        gl.uniform3f(p.at("u_gain"), grade.gain[0], grade.gain[1], grade.gain[2]);
        gl.uniform1f(p.at("u_vignette"), grade.vignette);
        gl.uniform2f(p.at("u_size"), w as f32, h as f32);
        gl.draw_arrays(gl::TRIANGLES, 0, 3);
        gl.depth_mask(true);
        gl.bind_vertex_array(0);
    }
}

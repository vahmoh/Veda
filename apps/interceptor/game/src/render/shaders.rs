//! The shaders (GLSL ES 3.00).
//!
//! Every lit program starts with [`PRELUDE`]: the frame's uniform block
//! (camera, sun, sky, fog, weather and the dynamic lights) and the
//! lighting every surface shares, physically based: a GGX highlight and a
//! Lambert body for the sun and each light, the sky's ambient light from
//! above and below, reflections of the sky weighted by Fresnel, and fog
//! that glows towards the sun. Programs output linear light, which the
//! post-processing tone maps; built with `TONEMAP` (no post-processing)
//! they tone map themselves.
//!
//! [`source`] assembles a stage: the version line, the build's defines,
//! the prelude, the program's own code.

use alloc::string::String;

/// The most dynamic lights a frame passes to the shaders.
pub const MAX_LIGHTS: usize = 16;

/// A complete shader: `#version`, `defines` (e.g. `"#define TONEMAP\n"`),
/// the prelude if `lit`, then `body`.
pub fn source(defines: &str, lit: bool, body: &str) -> String {
    let mut s = String::from("#version 300 es\nprecision highp float;\nprecision highp int;\n");
    s.push_str(defines);
    if lit {
        s.push_str(PRELUDE);
    }
    s.push_str(body);
    s
}

/// The frame's uniforms and the lighting.
pub const PRELUDE: &str = r#"
#define MAX_LIGHTS 16
layout(std140) uniform Frame {
    mat4 u_viewProj;
    mat4 u_shadowMatrix;
    vec4 u_eye;          // camera; w: seconds
    vec4 u_sunDir;       // towards the sun (or moon); w: its shadows' strength
    vec4 u_sunColor;     // what a white surface facing it reflects
    vec4 u_skyColor;     // ambient light from above
    vec4 u_groundColor;  // ambient light from below
    vec4 u_fogColor;     // w: density per metre
    vec4 u_fogParams;    // x: height falloff, y: glow towards the sun, z: most fog
    vec4 u_weather;      // x: wetness, y: rain, z: street lights, w: exposure
    vec4 u_lightRect;    // the light map's world rectangle: x0, z0, 1/width, 1/depth
    vec4 u_misc;         // x: dynamic lights, y: sky reflections' strength
    vec4 u_lights[MAX_LIGHTS * 3]; // (position, radius), (colour, 0), (direction, cos of the cone)
};

const float PI = 3.14159265;

float saturate(float x) { return clamp(x, 0.0, 1.0); }

// The sky's light from a direction: from the ground's colour below the
// horizon to the sky's above.
vec3 ambientLight(vec3 n) {
    return mix(u_groundColor.rgb, u_skyColor.rgb, n.y * 0.5 + 0.5);
}

// Reflectance of the environment, integrated over the highlight's lobe
// (an analytic fit), so that rough surfaces reflect less at grazing angles.
vec3 envBRDF(vec3 f0, float rough, float nv) {
    const vec4 c0 = vec4(-1.0, -0.0275, -0.572, 0.022);
    const vec4 c1 = vec4(1.0, 0.0425, 1.04, -0.04);
    vec4 r = rough * c0 + c1;
    float a004 = min(r.x * r.x, exp2(-9.28 * nv)) * r.x + r.y;
    vec2 ab = vec2(-1.04, 1.04) * a004 + r.zw;
    return f0 * ab.x + ab.y;
}

// GGX distribution and the height-correlated Smith visibility.
float ggx(float nh, float a) {
    float a2 = a * a;
    float d = nh * nh * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d + 1e-6);
}

float smith(float nv, float nl, float a) {
    float a2 = a * a;
    float v = nl * sqrt(nv * nv * (1.0 - a2) + a2);
    float l = nv * sqrt(nl * nl * (1.0 - a2) + a2);
    return 0.5 / (v + l + 1e-5);
}

// One light's contribution: radiance `c` arriving from direction `l`.
// Lights are not points: a highlight is never sharper than the light's
// size makes it (which also keeps a mirror's highlight finite).
vec3 brdf(vec3 diff, vec3 f0, float a, vec3 n, vec3 v, vec3 l, vec3 c) {
    a = max(a, 0.012);
    float nl = max(dot(n, l), 0.0);
    if (nl <= 0.0) {
        return vec3(0.0);
    }
    vec3 h = normalize(l + v);
    float nh = max(dot(n, h), 0.0);
    float vh = max(dot(v, h), 0.0);
    float nv = max(dot(n, v), 1e-3);
    vec3 f = f0 + (1.0 - f0) * pow(1.0 - vh, 5.0);
    vec3 spec = f * (ggx(nh, a) * smith(nv, nl, a) * PI);
    return (diff * (1.0 - f) + spec) * c * nl;
}

struct Surface {
    vec3 albedo;
    float rough;
    float metal;
    vec3 n;
    // How open to the sky (ambient, reflections, sunlight), and occlusion.
    float sky;
    float ao;
};

uniform samplerCube u_env;
uniform sampler2D u_lightMap;
uniform highp sampler2DShadow u_shadowMap;

// The street lights' light falling on a surface at `p` facing `n`: their
// pools, painted from above into the light map (its alpha: how high the
// lamps hang there, above which their light does not reach).
vec3 lampLight(vec3 p, vec3 n) {
    if (u_weather.z <= 0.0) {
        return vec3(0.0);
    }
    vec4 l = texture(u_lightMap, (p.xz - u_lightRect.xy) * u_lightRect.zw);
    float below = smoothstep(l.a + 3.0, l.a - 3.0, p.y);
    float facing = 0.45 + 0.55 * max(n.y, 0.0);
    return l.rgb * (u_weather.z * below * facing);
}

// The light a surface at `p` sends towards the camera.
vec3 shade(Surface s, vec3 p, float sunShadow) {
    vec3 v = normalize(u_eye.xyz - p);
    float nv = max(dot(s.n, v), 1e-3);
    float a = max(s.rough * s.rough, 0.003);
    vec3 f0 = mix(vec3(0.04), s.albedo, s.metal);
    vec3 diff = s.albedo * (1.0 - s.metal);
    vec3 c = brdf(diff, f0, a, s.n, v, u_sunDir.xyz, u_sunColor.rgb) * sunShadow * s.sky;
    // The street lights: their light from above, and its sheen.
    vec3 lamps = lampLight(p, s.n);
    if (lamps.r + lamps.g + lamps.b > 0.0) {
        c += diff * lamps;
        float up = max(dot(reflect(-v, s.n), vec3(0.0, 1.0, 0.0)), 0.0);
        float sheen = pow(up, mix(64.0, 4.0, s.rough)) * mix(1.0, 0.15, s.rough);
        c += lamps * envBRDF(f0, s.rough, nv) * sheen * 2.0;
    }
    // Ambient light and the sky's reflection.
    c += diff * ambientLight(s.n) * s.ao * (0.2 + 0.8 * s.sky);
    vec3 r = reflect(-v, s.n);
    float lod = s.rough * 7.0;
    vec3 env = textureLod(u_env, r, lod).rgb * u_misc.y;
    c += env * envBRDF(f0, s.rough, nv) * s.ao * s.sky;
    // The dynamic lights.
    int count = int(u_misc.x);
    for (int i = 0; i < MAX_LIGHTS; i++) {
        if (i >= count) {
            break;
        }
        vec4 pr = u_lights[i * 3];
        vec3 d = pr.xyz - p;
        float dist2 = dot(d, d);
        float r2 = pr.w * pr.w;
        if (dist2 >= r2) {
            continue;
        }
        vec3 l = d * inversesqrt(dist2);
        vec4 sp = u_lights[i * 3 + 2];
        float cone = 1.0;
        if (sp.w > -1.0) {
            cone = smoothstep(sp.w, sp.w + (1.0 - sp.w) * 0.6, dot(-l, sp.xyz));
            if (cone <= 0.0) {
                continue;
            }
        }
        float fall = saturate(1.0 - dist2 * dist2 / (r2 * r2));
        float atten = fall * fall / (dist2 + 1.0) * cone;
        c += brdf(diff, f0, a, s.n, v, l, u_lights[i * 3 + 1].rgb * atten);
    }
    return c;
}

// Exponential fog, thinner with height, glowing towards the sun.
vec3 fog(vec3 c, vec3 p) {
    vec3 d = p - u_eye.xyz;
    float dist = length(d);
    float k = u_fogParams.x;
    float h0 = u_eye.y;
    float h1 = p.y;
    // The fog's density integrated along the ray, falling off with height.
    float dh = h1 - h0;
    float column = abs(dh) > 0.01 ? (exp(-k * max(h0, 0.0)) - exp(-k * max(h1, 0.0))) / (k * dh) : exp(-k * max(h0, 0.0));
    float amount = 1.0 - exp(-u_fogColor.w * dist * max(column, 0.02));
    amount = min(amount, u_fogParams.z);
    vec3 dir = d / max(dist, 1e-3);
    float sun = pow(max(dot(dir, u_sunDir.xyz), 0.0), 8.0) * u_fogParams.y;
    vec3 fc = u_fogColor.rgb + u_sunColor.rgb * sun;
    return mix(c, fc, amount);
}

#ifdef TONEMAP
// Without post-processing: exposure, a filmic curve and sRGB here.
vec3 finish(vec3 c) {
    c = max(c, vec3(0.0)) * u_weather.w;
    c = (c * (2.51 * c + 0.03)) / (c * (2.43 * c + 0.59) + 0.14);
    return pow(clamp(c, 0.0, 1.0), vec3(1.0 / 2.2));
}
#else
// In high dynamic range, kept finite and positive: half floats end at
// 65504, and a single infinity would spread through the bloom.
vec3 finish(vec3 c) {
    if (any(isnan(c))) {
        return vec3(0.0);
    }
    return clamp(c, vec3(0.0), vec3(512.0));
}
#endif
"#;

/// A full-screen triangle (no transform).
pub const FULLSCREEN_VS: &str = r#"
in vec2 a_pos;
out vec2 v_pos;
void main() {
    v_pos = a_pos;
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
"#;

/// The sky for one face of its cube map: the direction through each pixel
/// from the face's basis. Day, dusk and night come from the uniforms: the
/// colours at the zenith and the horizon, the sun's (or moon's) direction
/// and glow, clouds, and stars.
pub const SKY_BAKE_FS: &str = r#"
uniform vec3 u_forward;
uniform vec3 u_right;
uniform vec3 u_up;
uniform vec3 u_sun;
uniform vec3 u_zenith;
uniform vec3 u_horizon;
uniform vec3 u_glow;      // around the sun
uniform vec3 u_disc;      // the sun's (moon's) disc
uniform vec4 u_clouds;    // rgb colour, a cover
uniform float u_stars;
uniform vec3 u_city;      // light pollution low over the city
in vec2 v_pos;
out vec4 o_color;

float hash(vec3 p) {
    p = fract(p * 0.3183099 + 0.1);
    p *= 17.0;
    return fract(p.x * p.y * p.z * (p.x + p.y + p.z));
}

float noise(vec3 x) {
    vec3 i = floor(x);
    vec3 f = fract(x);
    f = f * f * (3.0 - 2.0 * f);
    return mix(mix(mix(hash(i), hash(i + vec3(1, 0, 0)), f.x), mix(hash(i + vec3(0, 1, 0)), hash(i + vec3(1, 1, 0)), f.x), f.y),
               mix(mix(hash(i + vec3(0, 0, 1)), hash(i + vec3(1, 0, 1)), f.x), mix(hash(i + vec3(0, 1, 1)), hash(i + vec3(1, 1, 1)), f.x), f.y), f.z);
}

float fbm(vec3 p) {
    float s = 0.0;
    float a = 0.5;
    for (int i = 0; i < 5; i++) {
        s += a * noise(p);
        p = p * 2.03 + vec3(1.7, 9.2, 3.1);
        a *= 0.5;
    }
    return s;
}

void main() {
    vec3 d = normalize(u_forward + v_pos.x * u_right + v_pos.y * u_up);
    float h = d.y;
    // The gradient: the horizon's colour near it, the zenith's above.
    vec3 c = mix(u_horizon, u_zenith, pow(clamp(h, 0.0, 1.0), 0.5));
    // Below the horizon, a darker haze.
    if (h < 0.0) {
        c = mix(u_horizon, u_horizon * 0.35, clamp(-h * 3.0, 0.0, 1.0));
    }
    // Light pollution over the city (north), low in the sky.
    float north = clamp(-d.z * 0.7 + 0.3, 0.0, 1.0);
    c += u_city * north * exp(-max(h, 0.0) * 9.0);
    // The glow around the sun, and its disc.
    float s = max(dot(d, u_sun), 0.0);
    c += u_glow * (pow(s, 6.0) * 0.5 + pow(s, 64.0) * 0.8) * clamp(h * 4.0 + 1.0, 0.0, 1.0);
    c += u_disc * smoothstep(0.9993, 0.9997, s);
    // Clouds on a plane high above, thinning towards the horizon.
    if (u_clouds.a > 0.0 && h > 0.01) {
        vec2 q = d.xz / (h + 0.08) * 1.4;
        float n = fbm(vec3(q, 0.5));
        float cover = smoothstep(1.0 - u_clouds.a, 1.0 - u_clouds.a + 0.35, n);
        float lit = 0.75 + 0.5 * pow(s, 3.0);
        vec3 cc = u_clouds.rgb * lit + u_glow * pow(s, 4.0) * 0.6;
        c = mix(c, cc, cover * smoothstep(0.01, 0.2, h) * 0.9);
    }
    o_color = vec4(c, 1.0);
}
"#;

/// The sky behind everything, drawn last at the far plane.
pub const SKY_VS: &str = r#"
in vec2 a_pos;
uniform mat4 u_invViewProj;
out vec3 v_dir;
void main() {
    vec4 far = u_invViewProj * vec4(a_pos, 1.0, 1.0);
    vec4 near = u_invViewProj * vec4(a_pos, -1.0, 1.0);
    v_dir = far.xyz / far.w - near.xyz / near.w;
    gl_Position = vec4(a_pos, 1.0, 1.0);
}
"#;

pub const SKY_FS: &str = r#"
uniform float u_stars;
in vec3 v_dir;
out vec4 o_color;

float hash3(vec3 p) {
    p = fract(p * vec3(0.1031, 0.1030, 0.0973));
    p += dot(p, p.yxz + 33.33);
    return fract((p.x + p.y) * p.z);
}

void main() {
    vec3 d = normalize(v_dir);
    vec3 c = textureLod(u_env, d, 0.0).rgb;
    // Stars: points on a fine grid of the directions, one in a few hundred
    // cells lit, at their own brightness, round and as sharp as a pixel.
    if (u_stars > 0.0 && d.y > 0.0) {
        float scale = 900.0;
        vec3 q = d * scale;
        vec3 cell = floor(q);
        float h = hash3(cell);
        if (h > 0.996) {
            vec3 center = cell + 0.5 + (vec3(hash3(cell + 1.0), hash3(cell + 2.0), hash3(cell + 3.0)) - 0.5) * 0.6;
            float dist = length(q - center);
            float px = fwidth(q.x) + fwidth(q.y) + fwidth(q.z);
            float point = smoothstep(px * 0.9 + 0.05, 0.0, dist);
            float bright = pow(hash3(cell + 7.0), 4.0) * 3.0 + 0.3;
            vec3 tint = mix(vec3(1.0, 0.85, 0.7), vec3(0.75, 0.85, 1.0), hash3(cell + 9.0));
            c += tint * point * bright * u_stars * smoothstep(0.02, 0.3, d.y) * 0.15;
        }
    }
    o_color = vec4(finish(c), 1.0);
}
"#;

/// Static geometry in world space: the shared vertex layout.
pub const STATIC_VS: &str = r#"
in vec3 a_pos;
in vec3 a_normal;
in vec2 a_uv;
in vec4 a_params;
out vec3 v_pos;
out vec3 v_normal;
out vec2 v_uv;
out vec4 v_params;
void main() {
    v_pos = a_pos;
    v_normal = a_normal;
    v_uv = a_uv;
    v_params = a_params;
    gl_Position = u_viewProj * vec4(a_pos, 1.0);
}
"#;

/// The ground: grass, rock on the steep slopes, sand by the sea, patches
/// of dirt, varied over large distances.
pub const TERRAIN_FS: &str = r#"
uniform highp sampler2DArray u_ground;
in vec3 v_pos;
in vec3 v_normal;
in vec2 v_uv;
in vec4 v_params;
out vec4 o_color;
void main() {
    vec3 n = normalize(v_normal);
    vec2 uv = v_pos.xz / 8.0;
    vec4 grass = texture(u_ground, vec3(uv, 0.0));
    vec4 dirt = texture(u_ground, vec3(uv * 0.7, 3.0));
    vec4 sand = texture(u_ground, vec3(uv, 2.0));
    // Rock is projected from the side, along the slope.
    vec2 side = abs(n.x) > abs(n.z) ? v_pos.zy : v_pos.xy;
    vec4 rock = texture(u_ground, vec3(side / 10.0, 1.0));
    // Large-scale variation from the dirt layer's height, sampled wide.
    float macro = texture(u_ground, vec3(v_pos.xz / 230.0, 3.0)).a;
    float macro2 = texture(u_ground, vec3(v_pos.xz / 61.0, 0.0)).a;
    vec3 c = grass.rgb * (0.75 + 0.5 * macro2);
    c = mix(c, dirt.rgb, smoothstep(0.62, 0.8, macro + dirt.a * 0.2) * 0.7);
    float steep = smoothstep(0.82, 0.68, n.y + (rock.a - 0.5) * 0.1);
    c = mix(c, rock.rgb, steep);
    float beach = smoothstep(4.0, 1.5, v_pos.y + (sand.a - 0.5) * 1.5);
    c = mix(c, sand.rgb, beach * (1.0 - steep));
    float rough = mix(mix(0.9, 0.95, beach), 0.75, steep);
    // Wet ground darkens.
    c *= 1.0 - u_weather.x * 0.35;
    Surface s = Surface(c, rough, 0.0, n, 1.0, 1.0);
    vec3 col = shade(s, v_pos, 1.0);
    o_color = vec4(finish(fog(col, v_pos)), 1.0);
}
"#;

/// The tarmac: asphalt with its markings painted from road coordinates
/// (`v_uv` is lateral offset, distance along), lanes polished by tyres,
/// darker and glossy when wet, with puddles.
pub const TARMAC_FS: &str = r#"
uniform sampler2D u_asphalt;
uniform float u_dash;     // the dashes' period along the road (m)
in vec3 v_pos;
in vec3 v_normal;
in vec2 v_uv;
in vec4 v_params;
out vec4 o_color;

// A line `width` wide at `center` across `x`, anti-aliased.
float line(float x, float center, float width) {
    float w = max(fwidth(x), 1e-4);
    return 1.0 - smoothstep(width * 0.5 - w, width * 0.5 + w, abs(x - center));
}

void main() {
    float lat = v_uv.x;
    float along = v_uv.y;
    float a = abs(lat);
    vec4 tex = texture(u_asphalt, vec2(lat, along) / 4.0);
    float macro = texture(u_asphalt, vec2(lat, along) / 53.0).a;
    vec3 c = tex.rgb * (0.85 + 0.3 * macro);
    float rough = tex.a;
    // Tyres polish the lanes' wheel paths.
    float lane = mod(a - 0.3, 3.6);
    float tracks = smoothstep(0.5, 0.0, abs(lane - 0.9)) + smoothstep(0.5, 0.0, abs(lane - 2.7));
    tracks *= step(0.3, a) * step(a, 7.5);
    c *= 1.0 - 0.18 * tracks;
    rough -= 0.12 * tracks;
    // The markings.
    float divided = v_params.z;
    float dash = step(fract(along / u_dash), 0.38);
    float white = line(a, 3.9, 0.13) * dash + line(a, 7.42, 0.16);
    // A double yellow line down the middle, or yellow edge lines along
    // the median barrier.
    float yellow = (1.0 - divided) * line(a, 0.12, 0.1) + divided * line(a, 0.55, 0.12);
    float worn = smoothstep(0.15, 0.6, texture(u_asphalt, vec2(lat * 3.0, along) / 9.0).a);
    float paint = clamp(white + yellow, 0.0, 1.0) * worn;
    vec3 paintColor = mix(vec3(0.75, 0.75, 0.72), vec3(0.75, 0.5, 0.06), clamp(yellow, 0.0, 1.0));
    c = mix(c, paintColor, paint);
    rough = mix(rough, 0.55, paint);
    // Wet: darker, glossier, with puddles in the low spots.
    float wet = u_weather.x;
    float puddle = smoothstep(0.62, 0.7, texture(u_asphalt, vec2(lat, along) / 23.0).a + tracks * 0.15) * wet;
    c *= 1.0 - 0.45 * wet * (1.0 - paint * 0.5);
    rough = mix(rough, 0.18, wet);
    rough = mix(rough, 0.03, puddle);
    vec3 n = normalize(v_normal);
    float sky = v_params.y;
    Surface s = Surface(c, rough, 0.0, n, 1.0, sky);
    vec3 col = shade(s, v_pos, 1.0);
    o_color = vec4(finish(fog(col, v_pos)), 1.0);
}
"#;

/// What lines the road: kerbs, pavements, barriers, verges, rails,
/// tunnel walls (a texture array by material; metal is metallic).
pub const SURFACE_FS: &str = r#"
uniform highp sampler2DArray u_surfaces;
in vec3 v_pos;
in vec3 v_normal;
in vec2 v_uv;
in vec4 v_params;
out vec4 o_color;
void main() {
    float layer = floor(v_params.x * 255.0 + 0.5);
    vec4 t = texture(u_surfaces, vec3(v_uv * 0.5, layer));
    vec3 n = normalize(v_normal);
    if (!gl_FrontFacing) {
        n = -n;
    }
    float metal = layer == 3.0 ? 0.9 : 0.0;
    vec3 c = t.rgb * (1.0 - u_weather.x * 0.3);
    float rough = mix(t.a, t.a * 0.4, u_weather.x);
    Surface s = Surface(c, rough, metal, n, 1.0, v_params.y);
    vec3 col = shade(s, v_pos, 1.0);
    o_color = vec4(finish(fog(col, v_pos)), 1.0);
}
"#;

/// The sea: ripples at two scales drifting, the sky reflected by Fresnel,
/// a deep colour below, lighter and foamy in the shallows (`v_uv.x` is the
/// depth).
pub const WATER_FS: &str = r#"
uniform sampler2D u_ripples;
in vec3 v_pos;
in vec3 v_normal;
in vec2 v_uv;
in vec4 v_params;
out vec4 o_color;
void main() {
    float t = u_eye.w;
    vec2 p = v_pos.xz;
    vec3 n1 = texture(u_ripples, p / 37.0 + vec2(t * 0.011, t * 0.007)).xyz * 2.0 - 1.0;
    vec3 n2 = texture(u_ripples, p / 11.0 - vec2(t * 0.019, -t * 0.013)).xyz * 2.0 - 1.0;
    vec3 n3 = texture(u_ripples, p / 173.0 + vec2(-t * 0.004, t * 0.006)).xyz * 2.0 - 1.0;
    vec2 slope = n1.xy * 0.6 + n2.xy * 0.35 + n3.xy * 0.8;
    float dist = length(u_eye.xyz - v_pos);
    // Calmer far away, where ripples would only shimmer.
    slope *= mix(1.0, 0.25, smoothstep(150.0, 1500.0, dist));
    vec3 n = normalize(vec3(slope.x * 0.18, 1.0, slope.y * 0.18));
    vec3 v = normalize(u_eye.xyz - v_pos);
    float nv = max(dot(n, v), 0.0);
    float fresnel = 0.02 + 0.98 * pow(1.0 - nv, 5.0);
    vec3 r = reflect(-v, n);
    r.y = abs(r.y);
    vec3 sky = textureLod(u_env, r, 0.0).rgb * u_misc.y;
    float depth = v_uv.x;
    vec3 deep = vec3(0.004, 0.018, 0.028);
    vec3 shallow = vec3(0.02, 0.09, 0.085);
    vec3 body = mix(shallow, deep, smoothstep(0.5, 9.0, depth)) * (ambientLight(vec3(0.0, 1.0, 0.0)) + u_sunColor.rgb * max(u_sunDir.y, 0.0) * 0.6);
    vec3 c = mix(body, sky, fresnel);
    // The sun's (or moon's) glitter.
    vec3 h = normalize(u_sunDir.xyz + v);
    float spec = pow(max(dot(n, h), 0.0), 600.0) * 60.0 + pow(max(dot(n, h), 0.0), 60.0) * 0.6;
    c += u_sunColor.rgb * spec * fresnel * 4.0 * step(0.0, u_sunDir.y);
    // Foam where it is shallow.
    float foam = smoothstep(1.2, 0.2, depth) * smoothstep(0.35, 0.65, texture(u_ripples, p / 5.0 + vec2(t * 0.05, 0.0)).z * 0.5 + slope.x * 0.3 + 0.3);
    c = mix(c, vec3(0.6, 0.65, 0.65) * (ambientLight(vec3(0.0, 1.0, 0.0)) + u_sunColor.rgb * 0.5), foam * 0.6);
    o_color = vec4(finish(fog(c, v_pos)), 1.0);
}
"#;

/// Buildings: a unit box (x and z from -0.5 to 0.5, y from 0 to 1) per
/// block, placed, turned and sized by the instance: `a_inst0` the foot's
/// middle and the turn, `a_inst1` the size and the style, `a_inst2` the
/// walls' colour and the seed.
pub const BUILDING_VS: &str = r#"
in vec3 a_pos;
in vec3 a_normal;
in vec4 a_inst0;
in vec4 a_inst1;
in vec4 a_inst2;
out vec3 v_pos;
out vec3 v_normal;
// Facade coordinates in metres: along the face, and up from the foot.
out vec2 v_facade;
// The block's height, and whether this is its roof.
out vec2 v_block;
flat out vec3 v_tint;
flat out vec2 v_style;
void main() {
    float c = cos(a_inst0.w);
    float s = sin(a_inst0.w);
    vec3 size = a_inst1.xyz;
    vec3 l = a_pos * size;
    vec3 w = vec3(c * l.x + s * l.z, l.y, -s * l.x + c * l.z) + a_inst0.xyz;
    v_pos = w;
    v_normal = vec3(c * a_normal.x + s * a_normal.z, a_normal.y, -s * a_normal.x + c * a_normal.z);
    float across = abs(a_normal.x) > 0.5 ? l.z + size.z * 0.5 : l.x + size.x * 0.5;
    v_facade = a_normal.y > 0.5 ? l.xz : vec2(across, l.y);
    v_block = vec2(size.y, a_normal.y > 0.5 ? 1.0 : 0.0);
    v_tint = a_inst2.rgb;
    v_style = vec2(a_inst1.w, a_inst2.w);
    gl_Position = u_viewProj * vec4(w, 1.0);
}
"#;

/// A facade: walls of the style's material in the block's colour, a grid
/// of windows (glass reflecting the sky; lit rooms at night, some of them),
/// a shop front along the street, a roof. Windows too small to see fade to
/// their average.
pub const BUILDING_FS: &str = r#"
uniform highp sampler2DArray u_walls;
in vec3 v_pos;
in vec3 v_normal;
in vec2 v_facade;
in vec2 v_block;
flat in vec3 v_tint;
flat in vec2 v_style;
out vec4 o_color;

float hash2(vec2 p) {
    p = fract(p * vec2(0.1031, 0.1030));
    p += dot(p, p.yx + 33.33);
    return fract((p.x + p.y) * p.x);
}

// The fraction of a box from lo to hi (in a cell's 0..1) covered, filtered
// over the footprint `fw` of a pixel.
float box1(float x, float lo, float hi, float fw) {
    float w = max(fw, 1e-4);
    return clamp((min(x + w * 0.5, hi) - max(x - w * 0.5, lo)) / w, 0.0, 1.0);
}

void main() {
    int style = int(v_style.x + 0.5);
    float seed = v_style.y;
    vec3 n = normalize(v_normal);
    // The style: floor height, bay width, the window's share of each, the
    // wall's material, how glossy its glass, how many rooms are lit.
    float floorH = 3.6;
    float bay = 2.4;
    vec2 win = vec2(0.5, 0.55);
    float layer = 0.0;
    float lit = 0.35;
    if (style == 0) { floorH = 3.9; bay = 1.6; win = vec2(0.92, 0.82); layer = 0.0; lit = 0.3; }
    else if (style == 1) { floorH = 3.8; bay = 3.0; win = vec2(0.96, 0.46); layer = 0.0; lit = 0.32; }
    else if (style == 2) { floorH = 3.2; bay = 2.2; win = vec2(0.42, 0.55); layer = 1.0; lit = 0.45; }
    else if (style == 3) { floorH = 3.0; bay = 3.2; win = vec2(0.55, 0.6); layer = 0.0; lit = 0.5; }
    else if (style == 4) { floorH = 5.0; bay = 6.0; win = vec2(0.3, 0.12); layer = 2.0; lit = 0.15; }
    else { floorH = 4.2; bay = 2.6; win = vec2(0.34, 0.68); layer = 3.0; lit = 0.3; }

    vec3 wall = texture(u_walls, vec3(v_facade * 0.5, layer)).rgb * v_tint;
    float rough = 0.8;
    float roof = v_block.y;
    float height = v_facade.y;

    // The window grid, above the ground floor.
    vec2 cellSize = vec2(bay, floorH);
    vec2 q = v_facade / cellSize;
    vec2 cell = floor(q);
    vec2 f = fract(q);
    vec2 fw = fwidth(q);
    float ground = 1.0 - step(4.6, height);
    vec2 lo = (1.0 - win) * 0.5;
    float glass = box1(f.x, lo.x, 1.0 - lo.x, fw.x) * box1(f.y, lo.y + 0.06, 1.0 - lo.y + 0.06, fw.y);
    // Too small to tell apart: their average.
    float far = smoothstep(0.25, 0.6, max(fw.x, fw.y));
    glass = mix(glass, win.x * win.y, far);
    // The shop fronts along the street: tall glass, a band above.
    if (ground > 0.5) {
        float shopBay = floor(v_facade.x / 6.0);
        float sf = fract(v_facade.x / 6.0);
        glass = box1(sf, 0.08, 0.92, fwidth(v_facade.x / 6.0)) * box1(height, 0.4, 3.4, fwidth(height));
        cell = vec2(shopBay, -1.0);
    }
    glass *= 1.0 - roof;
    // Light inside: some rooms, warm or cool, brighter or dimmer.
    float h = hash2(cell + seed * 173.0);
    float on = step(h, ground > 0.5 ? 0.75 : lit);
    float tone = hash2(cell * 1.7 + seed * 31.0);
    vec3 room = mix(vec3(1.0, 0.72, 0.42), vec3(0.78, 0.88, 1.0), step(0.65, tone)) * (0.6 + 0.8 * fract(tone * 7.0));
    float glow = mix(on, lit, far) * u_weather.z;
    // The roof: gravel, darker.
    if (roof > 0.5) {
        wall = texture(u_walls, vec3(v_facade * 0.25, 0.0)).rgb * 0.35;
        rough = 0.95;
    }
    // Darker at the foot (the street's shade).
    float ao = mix(0.55, 1.0, smoothstep(0.0, 8.0, height + roof * 100.0));
    vec3 albedo = mix(wall, vec3(0.02, 0.025, 0.03), glass);
    float r = mix(rough, 0.06, glass);
    Surface s = Surface(albedo, r, 0.0, n, ao, 1.0);
    vec3 c = shade(s, v_pos, 1.0);
    // Glass tinted like the walls, for the curtain walls.
    if (style == 0) {
        c = mix(c, c * (0.6 + v_tint * 1.2), glass);
    }
    c += room * glow * glass * (ground > 0.5 ? 0.9 : 0.45);
    o_color = vec4(finish(fog(c, v_pos)), 1.0);
}
"#;

/// A car's body or wheel: car space to the world by `u_model`.
pub const CAR_VS: &str = r#"
in vec3 a_pos;
in vec3 a_normal;
in vec4 a_params;
uniform mat4 u_model;
out vec3 v_pos;
out vec3 v_normal;
out vec3 v_local;
flat out float v_part;
void main() {
    vec4 w = u_model * vec4(a_pos, 1.0);
    v_pos = w.xyz;
    v_normal = mat3(u_model) * a_normal;
    v_local = a_pos;
    v_part = floor(a_params.x * 255.0 + 0.5);
    gl_Position = u_viewProj * w;
}
"#;

/// A car: clear-coated paint (in its livery), glass, trim, chrome,
/// rubber, and lights that shine when lit: headlights, tail lights
/// (brighter when braking), and the police light bar's red and blue,
/// flashing in turn.
pub const CAR_FS: &str = r#"
uniform vec3 u_paint;
uniform int u_livery;      // 0 plain, 1 police, 2 taxi
uniform vec4 u_lamps;      // headlights, brake, the light bar's red and blue
uniform vec4 u_shape;      // the windscreen's foot (cowl) and the roof's ends (a, c), the shoulder
in vec3 v_pos;
in vec3 v_normal;
in vec3 v_local;
flat in float v_part;
out vec4 o_color;

vec3 clearcoat(vec3 n, vec3 p) {
    vec3 v = normalize(u_eye.xyz - p);
    float nv = max(dot(n, v), 0.0);
    float f = 0.04 + 0.96 * pow(1.0 - nv, 5.0);
    vec3 env = textureLod(u_env, reflect(-v, n), 0.6).rgb * u_misc.y;
    vec3 h = normalize(u_sunDir.xyz + v);
    float spec = ggx(max(dot(n, h), 0.0), 0.02) * max(dot(n, u_sunDir.xyz), 0.0) * 0.25;
    return (env + u_sunColor.rgb * spec) * f;
}

void main() {
    vec3 n = normalize(v_normal);
    if (!gl_FrontFacing) {
        n = -n;
    }
    int part = int(v_part);
    vec3 albedo = vec3(0.03);
    float rough = 0.5;
    float metal = 0.0;
    vec3 emit = vec3(0.0);
    bool coat = false;
    vec3 l = v_local;
    if (part == 0) {
        albedo = u_paint;
        if (u_livery == 1) {
            // Black and white: white doors and roof, black wings, bonnet
            // and boot.
            bool doors = abs(l.x) > 0.55 && l.z < u_shape.x - 0.1 && l.z > u_shape.z - 0.15 && l.y < u_shape.w;
            bool roof = l.y > u_shape.w + 0.3;
            albedo = (doors || roof) ? vec3(0.78, 0.79, 0.8) : vec3(0.012, 0.013, 0.016);
            // A blue stripe along the doors.
            if (doors && abs(l.y - (u_shape.w - 0.28)) < 0.03) {
                albedo = vec3(0.02, 0.07, 0.35);
            }
        } else if (u_livery == 2) {
            // A checker band along the doors of a taxi.
            if (abs(l.x) > 0.6 && abs(l.y - (u_shape.w - 0.22)) < 0.05) {
                float ch = mod(floor(l.z / 0.05) + floor((l.y - u_shape.w) / 0.05), 2.0);
                albedo = mix(vec3(0.02), vec3(0.8), ch);
            }
        }
        rough = 0.32;
        metal = 0.15;
        coat = true;
    } else if (part == 1) {
        albedo = vec3(0.005, 0.006, 0.008);
        rough = 0.02;
    } else if (part == 2) {
        albedo = vec3(0.025);
        rough = 0.55;
    } else if (part == 3) {
        albedo = vec3(0.85);
        metal = 1.0;
        rough = 0.12;
    } else if (part == 4) {
        // Headlights: a reflector behind clear glass.
        albedo = vec3(0.6);
        metal = 0.8;
        rough = 0.15;
        emit = vec3(1.0, 0.95, 0.85) * 6.0 * u_lamps.x;
    } else if (part == 5) {
        albedo = vec3(0.25, 0.01, 0.01);
        rough = 0.15;
        emit = vec3(1.0, 0.03, 0.02) * (1.2 * u_lamps.x + 5.0 * u_lamps.y);
    } else if (part == 6 || part == 7) {
        vec3 tint = part == 6 ? vec3(1.0, 0.04, 0.02) : vec3(0.05, 0.2, 1.0);
        albedo = tint * 0.25;
        rough = 0.1;
        emit = tint * 14.0 * (part == 6 ? u_lamps.z : u_lamps.w);
    } else if (part == 8) {
        albedo = vec3(0.02);
        rough = 0.85;
    } else if (part == 9) {
        albedo = vec3(0.7);
        metal = 1.0;
        rough = 0.25;
    } else if (part == 10) {
        albedo = vec3(0.75, 0.75, 0.7);
        rough = 0.4;
    } else if (part == 12) {
        albedo = vec3(0.4, 0.2, 0.02);
        rough = 0.15;
    } else {
        albedo = vec3(0.012);
        rough = 0.9;
    }
    Surface s = Surface(albedo, rough, metal, n, 1.0, 1.0);
    vec3 c = shade(s, v_pos, 1.0);
    if (coat) {
        c += clearcoat(n, v_pos);
    }
    c += emit;
    o_color = vec4(finish(fog(c, v_pos)), 1.0);
}
"#;

/// The street lights' map: each lamp's pool, as seen from above, added
/// into an image of the world (`a_inst0`: the lamp's x, z, the pool's
/// radius and the lamp's height; `a_inst1`: its colour).
pub const LIGHT_MAP_VS: &str = r#"
in vec2 a_pos;
in vec4 a_inst0;
in vec4 a_inst1;
uniform vec4 u_rect;
out vec2 v_q;
flat out vec4 v_color;
void main() {
    v_q = a_pos;
    vec2 w = a_inst0.xy + a_pos * a_inst0.z;
    v_color = vec4(a_inst1.rgb, a_inst0.w);
    gl_Position = vec4((w - u_rect.xy) * u_rect.zw * 2.0 - 1.0, 0.0, 1.0);
}
"#;

pub const LIGHT_MAP_FS: &str = r#"
in vec2 v_q;
flat in vec4 v_color;
out vec4 o_color;
void main() {
    float d2 = dot(v_q, v_q);
    if (d2 >= 1.0) {
        discard;
    }
    // Bright under the lamp, falling away smoothly to the pool's edge.
    float window = (1.0 - d2) * (1.0 - d2);
    float fall = window / (1.0 + d2 * 9.0);
    o_color = vec4(v_color.rgb * fall, v_color.a);
}
"#;

/// Props drawn many times over (street lights, for a start): placed and
/// turned by the instance (`a_inst0`: position and heading; `a_inst1`:
/// the lamp's colour and the prop's scale). Materials: 0 painted metal,
/// 1 a lamp's glass (lit at night), 2 concrete.
pub const PROP_VS: &str = r#"
in vec3 a_pos;
in vec3 a_normal;
in vec4 a_params;
in vec4 a_inst0;
in vec4 a_inst1;
out vec3 v_pos;
out vec3 v_normal;
flat out vec4 v_color;
flat out float v_part;
void main() {
    float c = cos(a_inst0.w);
    float s = sin(a_inst0.w);
    vec3 l = a_pos * a_inst1.w;
    vec3 w = vec3(c * l.x + s * l.z, l.y, -s * l.x + c * l.z) + a_inst0.xyz;
    v_pos = w;
    v_normal = vec3(c * a_normal.x + s * a_normal.z, a_normal.y, -s * a_normal.x + c * a_normal.z);
    v_color = vec4(a_inst1.rgb, 1.0);
    v_part = floor(a_params.x * 255.0 + 0.5);
    gl_Position = u_viewProj * vec4(w, 1.0);
}
"#;

pub const PROP_FS: &str = r#"
in vec3 v_pos;
in vec3 v_normal;
flat in vec4 v_color;
flat in float v_part;
out vec4 o_color;
void main() {
    vec3 n = normalize(v_normal);
    int part = int(v_part);
    vec3 albedo = vec3(0.09, 0.10, 0.10);
    float rough = 0.45;
    float metal = 0.6;
    vec3 emit = vec3(0.0);
    if (part == 1) {
        albedo = vec3(0.8);
        rough = 0.2;
        metal = 0.0;
        emit = v_color.rgb * 6.0 * u_weather.z;
    } else if (part == 2) {
        albedo = vec3(0.3);
        rough = 0.85;
        metal = 0.0;
    }
    Surface s = Surface(albedo, rough, metal, n, 1.0, 1.0);
    vec3 c = shade(s, v_pos, 1.0) + emit;
    o_color = vec4(finish(fog(c, v_pos)), 1.0);
}
"#;

/// Glows: soft discs facing the camera, added to the picture (lamps,
/// headlights, light bars). `a_inst0`: the place and the size; `a_inst1`:
/// the colour (its brightness included).
pub const GLOW_VS: &str = r#"
in vec2 a_pos;
in vec4 a_inst0;
in vec4 a_inst1;
uniform vec3 u_right;
uniform vec3 u_up;
out vec2 v_q;
flat out vec3 v_color;
out float v_fog;
void main() {
    vec3 c = a_inst0.xyz;
    float dist = length(c - u_eye.xyz);
    // A little larger far away, so that distant lights still show; faded
    // when the camera is in them.
    float size = a_inst0.w * (1.0 + dist * 0.004);
    vec3 w = c + (u_right * a_pos.x + u_up * a_pos.y) * size;
    v_q = a_pos;
    v_color = a_inst1.rgb * smoothstep(1.0, 4.0, dist);
    // Pulled towards the camera a little, so that the lamp's own fitting
    // does not hide its glow.
    vec4 clip = u_viewProj * vec4(w + normalize(u_eye.xyz - c) * min(1.0, dist * 0.5), 1.0);
    v_fog = 1.0 - min(1.0 - exp(-u_fogColor.w * dist), u_fogParams.z);
    gl_Position = clip;
}
"#;

pub const GLOW_FS: &str = r#"
in vec2 v_q;
flat in vec3 v_color;
in float v_fog;
out vec4 o_color;
void main() {
    float d2 = dot(v_q, v_q);
    if (d2 >= 1.0) {
        discard;
    }
    float g = exp(-d2 * 6.0) * 0.8 + exp(-d2 * 40.0) * 2.0;
    o_color = vec4(finish(v_color * g * v_fog), 1.0);
}
"#;

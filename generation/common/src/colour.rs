// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Colours with channels in floating point, as the generators paint:
//! nominally in `[0, 1]` (display space), light allowed to add up beyond 1
//! before a picture's tone curve.

/// A colour: red, green and blue.
pub type Rgb = [f32; 3];

/// `0xRRGGBB` as a colour.
pub const fn hex(c: u32) -> Rgb {
    [((c >> 16) & 0xFF) as f32 / 255.0, ((c >> 8) & 0xFF) as f32 / 255.0, (c & 0xFF) as f32 / 255.0]
}

/// `a` mixed with `b`: `t` of the way from one to the other.
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// The light of `a` and `b` together.
pub fn add(a: Rgb, b: Rgb) -> Rgb {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// `a`, `k` times as bright.
pub fn scale(a: Rgb, k: f32) -> Rgb {
    [a[0] * k, a[1] * k, a[2] * k]
}

/// 0 below `e0`, 1 above `e1`, and a smooth step between.
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The colour at `t` of a gradient through `stops` (positions and colours,
/// in order), each step between two a smooth one.
pub fn gradient(stops: &[(f32, Rgb)], t: f32) -> Rgb {
    if t <= stops[0].0 {
        return stops[0].1;
    }
    for w in stops.windows(2) {
        let ((p0, c0), (p1, c1)) = (w[0], w[1]);
        if t <= p1 {
            return mix(c0, c1, smoothstep(0.0, 1.0, (t - p0) / (p1 - p0).max(1e-6)));
        }
    }
    stops[stops.len() - 1].1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_mix_and_step() {
        assert_eq!(hex(0xFF8000), [1.0, 128.0 / 255.0, 0.0]);
        assert_eq!(mix([0.0; 3], [1.0, 0.5, 0.0], 0.5), [0.5, 0.25, 0.0]);
        assert_eq!(add(scale([1.0, 0.5, 0.25], 2.0), [0.0, 0.0, 0.5]), [2.0, 1.0, 1.0]);
        assert_eq!((smoothstep(0.0, 1.0, -1.0), smoothstep(0.0, 1.0, 0.5), smoothstep(0.0, 1.0, 2.0)), (0.0, 0.5, 1.0));
        let stops = [(0.0, [0.0; 3]), (0.5, [1.0; 3]), (1.0, [0.0; 3])];
        assert_eq!(gradient(&stops, -1.0), [0.0; 3]);
        assert_eq!(gradient(&stops, 0.25), [0.5; 3]);
        assert_eq!(gradient(&stops, 0.5), [1.0; 3]);
        assert_eq!(gradient(&stops, 2.0), [0.0; 3]);
    }
}

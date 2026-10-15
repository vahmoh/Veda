// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Integer hashes of lattice points, which noise and scattered things are
//! made from: well mixed, and the same on every machine.

/// The hash of the lattice point (`x`, `y`) for `seed`.
#[inline]
pub fn hash(x: i32, y: i32, seed: u32) -> u32 {
    let mut h =
        seed.wrapping_mul(0x9E37_79B9) ^ (x as u32).wrapping_mul(0x27D4_EB2D) ^ (y as u32).wrapping_mul(0x1656_67B1);
    h = (h ^ (h >> 15)).wrapping_mul(0x85EB_CA6B);
    h = (h ^ (h >> 13)).wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 16)
}

/// A hash mapped to `[0, 1)`.
#[inline]
pub fn hash01(x: i32, y: i32, seed: u32) -> f32 {
    (hash(x, y, seed) >> 8) as f32 / (1u32 << 24) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_the_same_everywhere_and_spread() {
        // The pictures depend on these values staying what they are.
        assert_eq!(hash(0, 0, 0), 0);
        assert_eq!(hash(1, 2, 3), hash(1, 2, 3));
        let values: Vec<f32> = (0..1000).map(|i| hash01(i, -i, 7)).collect();
        assert!(values.iter().all(|v| (0.0..1.0).contains(v)));
        let mean = values.iter().sum::<f32>() / values.len() as f32;
        assert!((mean - 0.5).abs() < 0.05, "{mean}");
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Pictures turned upright on their way into a slot: a stream's rotation
//! (clockwise, in right angles) and mirroring, applied to opaque pixels the
//! scaler made at the turned size.

/// How a picture is turned to show it upright: rotated clockwise by
/// `rotation` degrees (0, 90, 180 or 270), then mirrored left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Orientation {
    pub rotation: u32,
    pub mirrored: bool,
}

impl Orientation {
    /// `rotation` must be a right angle; others count as none.
    pub fn new(rotation: u32, mirrored: bool) -> Orientation {
        let rotation = if matches!(rotation, 90 | 180 | 270) { rotation } else { 0 };
        Orientation { rotation, mirrored }
    }

    /// Nothing to do.
    pub fn is_upright(&self) -> bool {
        self.rotation == 0 && !self.mirrored
    }

    /// Width and height trade places.
    pub fn swaps(&self) -> bool {
        self.rotation == 90 || self.rotation == 270
    }

    /// The size of the picture before turning, for `width` x `height`
    /// upright.
    pub fn source_size(&self, width: u32, height: u32) -> (u32, u32) {
        if self.swaps() { (height, width) } else { (width, height) }
    }

    /// For the upright picture's pixel (0, 0): the source pixel it comes
    /// from, and how far the source moves (in pixels: x, y) for a step
    /// right and a step down in the upright picture. `sw` x `sh` is the
    /// source's size.
    fn walk(&self, sw: isize, sh: isize) -> ((isize, isize), (isize, isize), (isize, isize)) {
        match (self.rotation, self.mirrored) {
            (90, false) => ((0, sh - 1), (0, -1), (1, 0)),
            (90, true) => ((0, 0), (0, 1), (1, 0)),
            (180, false) => ((sw - 1, sh - 1), (-1, 0), (0, -1)),
            (180, true) => ((0, sh - 1), (1, 0), (0, -1)),
            (270, false) => ((sw - 1, 0), (0, 1), (-1, 0)),
            (270, true) => ((sw - 1, sh - 1), (0, -1), (-1, 0)),
            (_, true) => ((sw - 1, 0), (-1, 0), (0, 1)),
            _ => ((0, 0), (1, 0), (0, 1)),
        }
    }
}

/// Square blocks the copy walks in, so that turning reads the source a few
/// cache lines at a time rather than a column at a time.
const TILE: usize = 32;

/// Copies the `sw` x `sh` pixels of `src` (`src_stride` from one row to
/// the next) turned by `o` into `dst` (the upright size: `sh` x `sw` when
/// it swaps, `dst_stride` from one row to the next).
///
/// # Safety
/// `dst` can be written for the upright size's rows, `dst_stride` apart. It
/// may be memory another process reads: it is only ever written through
/// the pointer.
pub unsafe fn turn(
    src: &[u32],
    sw: usize,
    sh: usize,
    src_stride: usize,
    dst: *mut u32,
    dst_stride: usize,
    o: Orientation,
) {
    if sw == 0 || sh == 0 || src.len() < (sh - 1) * src_stride + sw {
        return;
    }
    let (dw, dh) = if o.swaps() { (sh, sw) } else { (sw, sh) };
    let ((bx, by), (rx, ry), (ux, uy)) = o.walk(sw as isize, sh as isize);
    let stride = src_stride as isize;
    let base = by * stride + bx;
    let (right, down) = (ry * stride + rx, uy * stride + ux);
    for ty in (0..dh).step_by(TILE) {
        for tx in (0..dw).step_by(TILE) {
            for y in ty..(ty + TILE).min(dh) {
                let mut i = base + y as isize * down + tx as isize * right;
                for x in tx..(tx + TILE).min(dw) {
                    // `i` stays inside the source: every upright pixel comes
                    // from one of its `sw` x `sh` pixels.
                    let px = src[i as usize];
                    // SAFETY: inside the caller's rows.
                    unsafe { dst.add(y * dst_stride + x).write(px) };
                    i += right;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// The picture rotated a quarter turn clockwise, written the plain way.
    fn quarter(p: &[u32], w: usize, h: usize) -> Vec<u32> {
        let mut out = vec![0; w * h];
        for y in 0..h {
            for x in 0..w {
                // Source (x, y) lands at (h - 1 - y, x) in an h-wide picture.
                out[x * h + (h - 1 - y)] = p[y * w + x];
            }
        }
        out
    }

    fn mirror(p: &[u32], w: usize, h: usize) -> Vec<u32> {
        (0..h).flat_map(|y| (0..w).rev().map(move |x| (y, x))).map(|(y, x)| p[y * w + x]).collect()
    }

    #[test]
    fn every_orientation_matches_turning_by_hand() {
        for (w, h) in [(3usize, 2usize), (1, 5), (37, 70), (64, 33)] {
            let src: Vec<u32> = (0..(w * h) as u32).collect();
            for rotation in [0, 90, 180, 270] {
                for mirrored in [false, true] {
                    let o = Orientation::new(rotation, mirrored);
                    let (mut want, mut ww, mut wh) = (src.clone(), w, h);
                    for _ in 0..rotation / 90 {
                        want = quarter(&want, ww, wh);
                        (ww, wh) = (wh, ww);
                    }
                    if mirrored {
                        want = mirror(&want, ww, wh);
                    }
                    // A wider destination: rows `ww + 3` apart.
                    let mut got = vec![u32::MAX; (ww + 3) * wh];
                    // SAFETY: `got` holds `wh` rows of `ww + 3`.
                    unsafe { turn(&src, w, h, w, got.as_mut_ptr(), ww + 3, o) };
                    let got: Vec<u32> = (0..wh).flat_map(|y| got[y * (ww + 3)..y * (ww + 3) + ww].to_vec()).collect();
                    assert_eq!(got, want, "{w}x{h} turned {rotation}, mirrored {mirrored}");
                }
            }
        }
    }

    #[test]
    fn sizes_and_odd_angles() {
        let o = Orientation::new(90, false);
        assert!(o.swaps() && !o.is_upright());
        assert_eq!(o.source_size(1080, 1920), (1920, 1080));
        assert_eq!(Orientation::new(45, false), Orientation::default());
        // A source too short for its size is left alone.
        let mut dst = [7u32; 4];
        // SAFETY: `dst` holds the 2 x 2 upright picture.
        unsafe { turn(&[1, 2, 3], 2, 2, 2, dst.as_mut_ptr(), 2, o) };
        assert_eq!(dst, [7; 4]);
    }
}

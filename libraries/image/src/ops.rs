// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Image operations: scaling (by [`fast_image_resize`]), thumbnails, cropping, and the flips
//! and quarter turns of EXIF orientation.
//!
//! # Scaling
//!
//! [`resize`] is a separable convolution: the filter is widened by the scale factor when
//! shrinking, so every source pixel contributes and nothing aliases, and pixels are filtered
//! with their alpha premultiplied, so that transparent pixels lend no colour to visible ones.
//! `fast_image_resize` picks SSE4.1 or AVX2 code for the processor it runs on.

use alloc::vec::Vec;

use fast_image_resize::images::{Image as Target, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

use crate::{Image, ImageError, Orientation};

/// A resampling filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Filter {
    /// Nearest neighbor: fastest, blocky. Exact for integer upscaling of pixel art.
    Nearest,
    /// Triangle filter (bilinear interpolation when enlarging; widened when shrinking).
    Bilinear,
    /// Area averaging: each output pixel is the average of the source area it covers.
    Box,
    /// Lanczos windowed sinc with 3 lobes: the sharpest high-quality filter.
    Lanczos3,
}

impl Filter {
    fn algorithm(self) -> ResizeAlg {
        match self {
            Filter::Nearest => ResizeAlg::Nearest,
            Filter::Bilinear => ResizeAlg::Convolution(FilterType::Bilinear),
            Filter::Box => ResizeAlg::Convolution(FilterType::Box),
            Filter::Lanczos3 => ResizeAlg::Convolution(FilterType::Lanczos3),
        }
    }
}

/// Resizes `image` to `width x height` with `filter`. Works for any combination of up- and
/// downscaling; a zero target dimension yields an empty image.
pub fn resize(image: &Image, width: u32, height: u32, filter: Filter) -> Image {
    if width == 0 || height == 0 {
        return Image { width, height, pixels: Vec::new() };
    }
    if image.is_empty() {
        return Image::new(width, height);
    }
    if (width, height) == (image.width, image.height) {
        return image.clone();
    }
    let mut out = Image::new(width, height);
    let options = ResizeOptions::new().resize_alg(filter.algorithm()).use_alpha(image.has_alpha());
    let source = ImageRef::new(image.width, image.height, bytemuck::cast_slice(&image.pixels), PixelType::U8x4);
    let target = Target::from_slice_u8(width, height, bytemuck::cast_slice_mut(&mut out.pixels), PixelType::U8x4);
    // Both buffers are sized for their images, and of one pixel type: nothing can fail.
    if let (Ok(source), Ok(mut target)) = (source, target) {
        let _ = Resizer::new().resize(&source, &mut target, &options);
    }
    out
}

/// Computes the size of a thumbnail that fits in `max_width x max_height` while preserving the
/// aspect ratio of a `width x height` image (never larger than the image itself).
pub fn fit_size(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    if width <= max_width && height <= max_height {
        return (width, height);
    }
    let (w, h, mw, mh) = (width as u64, height as u64, max_width as u64, max_height as u64);
    if w * mh > h * mw {
        (max_width, ((h * mw + w / 2) / w).max(1) as u32)
    } else {
        (((w * mh + h / 2) / h).max(1) as u32, max_height)
    }
}

/// Scales `image` down (never up) to fit in `max_width x max_height`, preserving the aspect
/// ratio.
pub fn thumbnail(image: &Image, max_width: u32, max_height: u32, filter: Filter) -> Image {
    if max_width == 0 || max_height == 0 {
        return Image { width: 0, height: 0, pixels: Vec::new() };
    }
    let (tw, th) = fit_size(image.width, image.height, max_width, max_height);
    resize(image, tw, th, filter)
}

/// Returns the part of `image` inside the rectangle at `(x, y)` of size `width x height`,
/// clipped to the image (the result may be smaller, or empty).
pub fn crop(image: &Image, x: u32, y: u32, width: u32, height: u32) -> Image {
    let x0 = x.min(image.width);
    let y0 = y.min(image.height);
    let x1 = x.saturating_add(width).min(image.width);
    let y1 = y.saturating_add(height).min(image.height);
    let (w, h) = (x1 - x0, y1 - y0);
    let mut pixels = Vec::with_capacity(w as usize * h as usize);
    if w > 0 {
        for row in image.pixels.chunks_exact(image.width as usize).skip(y0 as usize).take(h as usize) {
            pixels.extend_from_slice(&row[x0 as usize..x1 as usize]);
        }
    }
    Image { width: w, height: h, pixels }
}

/// Mirrors `image` left to right.
pub fn flip_horizontal(image: &Image) -> Image {
    oriented(image, Orientation::FlipHorizontal, Vec::with_capacity(image.pixels.len()))
}

/// Mirrors `image` top to bottom.
pub fn flip_vertical(image: &Image) -> Image {
    oriented(image, Orientation::FlipVertical, Vec::with_capacity(image.pixels.len()))
}

/// Rotates `image` by 180 degrees.
pub fn rotate180(image: &Image) -> Image {
    oriented(image, Orientation::Rotate180, Vec::with_capacity(image.pixels.len()))
}

/// Rotates `image` by 90 degrees clockwise.
pub fn rotate90(image: &Image) -> Image {
    oriented(image, Orientation::Rotate90, Vec::with_capacity(image.pixels.len()))
}

/// Rotates `image` by 270 degrees clockwise (90 degrees counter-clockwise).
pub fn rotate270(image: &Image) -> Image {
    oriented(image, Orientation::Rotate270, Vec::with_capacity(image.pixels.len()))
}

/// Transforms stored pixels to the upright orientation described by an EXIF tag.
pub fn apply_orientation(image: &Image, orientation: Orientation) -> Image {
    oriented(image, orientation, Vec::with_capacity(image.pixels.len()))
}

/// [`apply_orientation`], reporting a failed allocation instead of aborting.
pub(crate) fn try_apply_orientation(image: &Image, orientation: Orientation) -> Result<Image, ImageError> {
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(image.pixels.len())?;
    Ok(oriented(image, orientation, pixels))
}

/// `image` turned and mirrored as `orientation` says, in `pixels` (empty, with the capacity).
fn oriented(image: &Image, orientation: Orientation, mut pixels: Vec<u32>) -> Image {
    let (w, h) = (image.width as usize, image.height as usize);
    let rows = || image.pixels.chunks_exact(w.max(1));
    match orientation {
        Orientation::Normal => pixels.extend_from_slice(&image.pixels),
        Orientation::FlipHorizontal => rows().for_each(|row| pixels.extend(row.iter().rev())),
        Orientation::FlipVertical => rows().rev().for_each(|row| pixels.extend_from_slice(row)),
        Orientation::Rotate180 => pixels.extend(image.pixels.iter().rev()),
        // Transposed (output (x, y) is input (y, x)), then mirrored; in tiles, to stay in cache.
        Orientation::Transpose | Orientation::Rotate90 | Orientation::Transverse | Orientation::Rotate270 => {
            let flip_h = matches!(orientation, Orientation::Rotate90 | Orientation::Transverse);
            let flip_v = matches!(orientation, Orientation::Transverse | Orientation::Rotate270);
            let (ow, oh) = (h, w);
            pixels.resize(w * h, 0);
            const TILE: usize = 32;
            for ty in (0..oh).step_by(TILE) {
                for tx in (0..ow).step_by(TILE) {
                    for y in ty..(ty + TILE).min(oh) {
                        let sx = if flip_v { w - 1 - y } else { y };
                        let out_row = &mut pixels[y * ow..(y + 1) * ow];
                        for (x, o) in out_row.iter_mut().enumerate().take((tx + TILE).min(ow)).skip(tx) {
                            let sy = if flip_h { h - 1 - x } else { x };
                            *o = image.pixels[sy * w + sx];
                        }
                    }
                }
            }
            return Image { width: image.height, height: image.width, pixels };
        }
    }
    Image { width: image.width, height: image.height, pixels }
}

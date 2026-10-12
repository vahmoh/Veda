//! Encoding (the `encode` feature), for the tools that make pictures on the build machine:
//! PNG with the `png` crate, JPEG with `jpeg-encoder`, QOI with the `qoi` crate.

use alloc::vec::Vec;

use crate::{Image, ImageError};

/// JPEG chroma subsampling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Subsampling {
    /// Full-resolution chroma.
    Yuv444,
    /// Chroma halved horizontally.
    Yuv422,
    /// Chroma halved in both directions (the usual choice for photographs).
    Yuv420,
}

/// How [`encode_jpeg`] encodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegOptions {
    /// Quality, 1 (smallest) to 100 (best).
    pub quality: u8,
    /// Chroma subsampling.
    pub subsampling: Subsampling,
    /// Progressive scans instead of one baseline scan.
    pub progressive: bool,
    /// Huffman tables optimized for the image (smaller files, a little slower).
    pub optimize_huffman: bool,
    /// EXIF data (a TIFF structure) for an APP1 segment, such as an orientation tag.
    pub exif: Option<Vec<u8>>,
}

impl Default for JpegOptions {
    /// Quality 90, 4:2:0, baseline, standard Huffman tables, no EXIF data.
    fn default() -> Self {
        JpegOptions {
            quality: 90,
            subsampling: Subsampling::Yuv420,
            progressive: false,
            optimize_huffman: false,
            exif: None,
        }
    }
}

/// `image`'s pixels as packed 8-bit samples, RGB if it is opaque (the flag) and RGBA otherwise.
fn samples(image: &Image) -> (Vec<u8>, bool) {
    if image.has_alpha() {
        return (image.to_rgba8(), false);
    }
    let rgb = image.pixels.iter().flat_map(|p| {
        let [b, g, r, _] = p.to_le_bytes();
        [r, g, b]
    });
    (rgb.collect(), true)
}

/// Encodes `image` as an 8-bit PNG: RGB if it is opaque, RGBA otherwise.
pub fn encode_png(image: &Image) -> Result<Vec<u8>, ImageError> {
    if image.is_empty() {
        return Err(ImageError::InvalidArgument("cannot encode an empty image"));
    }
    let (samples, opaque) = samples(image);
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
    encoder.set_color(if opaque { png::ColorType::Rgb } else { png::ColorType::Rgba });
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Balanced);
    let failed = |_| ImageError::InvalidArgument("PNG encoding failed");
    let mut writer = encoder.write_header().map_err(failed)?;
    writer.write_image_data(&samples).map_err(failed)?;
    writer.finish().map_err(failed)?;
    Ok(out)
}

/// Encodes `image` as a JPEG (alpha is ignored).
pub fn encode_jpeg(image: &Image, options: &JpegOptions) -> Result<Vec<u8>, ImageError> {
    let (Ok(width), Ok(height)) = (u16::try_from(image.width), u16::try_from(image.height)) else {
        return Err(ImageError::InvalidArgument("a JPEG is at most 65535 pixels wide and high"));
    };
    if image.is_empty() {
        return Err(ImageError::InvalidArgument("cannot encode an empty image"));
    }
    let failed = |_| ImageError::InvalidArgument("JPEG encoding failed");
    let mut out = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut out, options.quality.clamp(1, 100));
    encoder.set_sampling_factor(match options.subsampling {
        Subsampling::Yuv444 => jpeg_encoder::SamplingFactor::R_4_4_4,
        Subsampling::Yuv422 => jpeg_encoder::SamplingFactor::R_4_2_2,
        Subsampling::Yuv420 => jpeg_encoder::SamplingFactor::R_4_2_0,
    });
    encoder.set_progressive(options.progressive);
    encoder.set_optimized_huffman_tables(options.optimize_huffman);
    if let Some(tiff) = &options.exif {
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(tiff);
        encoder.add_app_segment(1, app1).map_err(failed)?;
    }
    // Veda's pixels are B, G, R, A in memory.
    encoder
        .encode(bytemuck::cast_slice(&image.pixels), width, height, jpeg_encoder::ColorType::Bgra)
        .map_err(failed)?;
    Ok(out)
}

/// Encodes `image` as QOI: RGB if it is opaque, RGBA otherwise.
pub fn encode_qoi(image: &Image) -> Result<Vec<u8>, ImageError> {
    qoi::encode_to_vec(samples(image).0, image.width, image.height)
        .map_err(|_| ImageError::InvalidArgument("QOI encoding failed"))
}

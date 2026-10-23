// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Encoding (the `encode` feature), without `std`: in Veda, for programs that save pictures
//! (Paint), and on the build machine, for the tools that make them.
//!
//! | Format | Encoder |
//! |--------|---------|
//! | PNG  | the format's chunks around a zlib stream that [`miniz_oxide`] deflates (the `flate2` crate's deflate) |
//! | JPEG | [`jpeg_encoder`]: baseline or progressive, any subsampling, optimised Huffman tables, EXIF |
//! | QOI  | the [`qoi`] crate |
//! | BMP  | a header before the rows: 24 bits a pixel, or 32 with an alpha mask where there is transparency |
//!
//! Opaque pictures are written without their alpha channel (RGB) in every format that has one.
//!
//! # PNG
//!
//! Each row is filtered with the filter whose output has the smallest sum of absolute values
//! (the heuristic the PNG specification recommends, section 12.8), then deflated as it is
//! filtered: what an encoding holds at once is the compressed stream and two rows, never a
//! filtered copy of the picture. The stream is split into `IDAT` chunks of at most
//! [`IDAT_MAX`] bytes, each with its CRC-32 ([`crc32fast`]).

use alloc::vec;
use alloc::vec::Vec;

use miniz_oxide::deflate::core::{
    CompressorOxide, TDEFLFlush, TDEFLStatus, compress, create_comp_flags_from_zip_params,
};

use crate::{Format, Image, ImageError};

/// The largest `IDAT` chunk written (bytes).
pub const IDAT_MAX: usize = 1 << 18;

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

/// How [`encode_png_with`] encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PngOptions {
    /// Deflate's effort, 1 (fastest) to 9 (smallest); 0 stores the rows uncompressed.
    pub level: u8,
}

impl Default for PngOptions {
    /// Level 6, zlib's default: within a few percent of the smallest, several times faster.
    fn default() -> Self {
        PngOptions { level: 6 }
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

/// Refuses an image no encoder can write.
fn check(image: &Image) -> Result<(), ImageError> {
    if image.is_empty() {
        return Err(ImageError::InvalidArgument("cannot encode an empty image"));
    }
    if image.pixels.len() as u64 != image.width as u64 * image.height as u64 {
        return Err(ImageError::InvalidArgument("pixel buffer length does not match the dimensions"));
    }
    Ok(())
}

/// Encodes `image` in `format`, with each encoder's default options.
pub fn encode(image: &Image, format: Format) -> Result<Vec<u8>, ImageError> {
    match format {
        Format::Png => encode_png(image),
        Format::Jpeg => encode_jpeg(image, &JpegOptions::default()),
        Format::Bmp => encode_bmp(image),
        Format::Qoi => encode_qoi(image),
    }
}

/// Encodes `image` as an 8-bit PNG: RGB if it is opaque, RGBA otherwise.
pub fn encode_png(image: &Image) -> Result<Vec<u8>, ImageError> {
    encode_png_with(image, &PngOptions::default())
}

/// The most a PNG's width or height may be (the specification's 2^31 - 1).
const PNG_MAX_DIMENSION: u32 = (1 << 31) - 1;

/// Encodes `image` as an 8-bit PNG (RGB if it is opaque, RGBA otherwise) with `options`.
pub fn encode_png_with(image: &Image, options: &PngOptions) -> Result<Vec<u8>, ImageError> {
    check(image)?;
    if image.width > PNG_MAX_DIMENSION || image.height > PNG_MAX_DIMENSION {
        return Err(ImageError::InvalidArgument("a PNG is at most 2^31 - 1 pixels wide and high"));
    }
    let opaque = !image.has_alpha();
    let bpp = if opaque { 3 } else { 4 };
    let row_len = image.width as usize * bpp;

    let mut out = Vec::new();
    out.try_reserve(row_len.saturating_mul(image.height as usize) / 2 + 1024)?;
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n']);
    let mut ihdr = [0u8; 13];
    ihdr[..4].copy_from_slice(&image.width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&image.height.to_be_bytes());
    // 8 bits a sample; colour type 2 (RGB) or 6 (RGBA); deflate, adaptive filtering, no interlace.
    ihdr[8] = 8;
    ihdr[9] = if opaque { 2 } else { 6 };
    write_chunk(&mut out, b"IHDR", &ihdr);

    let stream = deflate_rows(image, opaque, options.level.min(10))?;
    for part in stream.chunks(IDAT_MAX) {
        write_chunk(&mut out, b"IDAT", part);
    }
    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

/// Appends a chunk: its length, type, data and the CRC-32 of the type and data.
fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = crc32fast::Hasher::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finalize().to_be_bytes());
}

/// The zlib stream of `image`'s rows, each filtered and preceded by its filter's number.
fn deflate_rows(image: &Image, opaque: bool, level: u8) -> Result<Vec<u8>, ImageError> {
    let bpp = if opaque { 3 } else { 4 };
    let width = image.width as usize;
    let row_len = width * bpp;
    let flags = create_comp_flags_from_zip_params(level.into(), 1, 0);
    let mut compressor = CompressorOxide::new(flags);
    let mut stream: Vec<u8> = Vec::new();
    stream.try_reserve(row_len + 64)?;
    stream.resize(row_len.max(64), 0);
    let mut written = 0;

    // The row being filtered, the row above it, and the best filtered row with a scratch one.
    let (mut prev, mut cur) = (vec![0u8; row_len], vec![0u8; row_len]);
    let (mut best, mut trial) = (vec![0u8; row_len + 1], vec![0u8; row_len + 1]);
    for y in 0..image.height as usize {
        let row = &image.pixels[y * width..(y + 1) * width];
        if opaque {
            for (o, p) in cur.as_chunks_mut::<3>().0.iter_mut().zip(row) {
                let [b, g, r, _] = p.to_le_bytes();
                *o = [r, g, b];
            }
        } else {
            for (o, p) in cur.as_chunks_mut::<4>().0.iter_mut().zip(row) {
                let [b, g, r, a] = p.to_le_bytes();
                *o = [r, g, b, a];
            }
        }
        let mut best_cost = u64::MAX;
        for filter in 0..5u8 {
            trial[0] = filter;
            let cost = filter_row(filter, &cur, &prev, bpp, &mut trial[1..]);
            if cost < best_cost {
                best_cost = cost;
                core::mem::swap(&mut best, &mut trial);
            }
        }
        deflate(&mut compressor, &best, &mut stream, &mut written, TDEFLFlush::None)?;
        core::mem::swap(&mut prev, &mut cur);
    }
    deflate(&mut compressor, &[], &mut stream, &mut written, TDEFLFlush::Finish)?;
    stream.truncate(written);
    Ok(stream)
}

/// Feeds `input` to the compressor, growing `stream` (of which `written` bytes are used) as its
/// output needs; with [`TDEFLFlush::Finish`], until the stream is complete.
fn deflate(
    compressor: &mut CompressorOxide,
    mut input: &[u8],
    stream: &mut Vec<u8>,
    written: &mut usize,
    flush: TDEFLFlush,
) -> Result<(), ImageError> {
    let mut grow = false;
    loop {
        if grow || stream.len() - *written < 64 {
            let more = stream.len().max(4096);
            stream.try_reserve(more)?;
            stream.resize(stream.len() + more, 0);
        }
        let (status, consumed, produced) = compress(compressor, input, &mut stream[*written..], flush);
        *written += produced;
        input = input.get(consumed..).unwrap_or_default();
        match status {
            TDEFLStatus::Done => return Ok(()),
            TDEFLStatus::Okay if input.is_empty() && flush == TDEFLFlush::None => return Ok(()),
            // More input to take, or more output to give: around again, with more room if the
            // compressor could do nothing with what it had.
            TDEFLStatus::Okay => grow = consumed == 0 && produced == 0,
            TDEFLStatus::BadParam | TDEFLStatus::PutBufFailed => {
                return Err(ImageError::InvalidArgument("PNG compression failed"));
            }
        }
    }
}

/// Paeth's predictor: whichever of left, above and upper left is closest to `a + b - c`.
#[inline]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Filters `row` (with `prev` above it, `bpp` bytes a pixel) with PNG filter `filter` into `out`;
/// returns the sum of the output bytes' magnitudes as signed bytes, which the encoder minimises.
fn filter_row(filter: u8, row: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) -> u64 {
    let mut cost = 0u64;
    for i in 0..row.len() {
        let left = if i >= bpp { row[i - bpp] } else { 0 };
        let up = prev[i];
        let up_left = if i >= bpp { prev[i - bpp] } else { 0 };
        let predicted = match filter {
            0 => 0,
            1 => left,
            2 => up,
            3 => ((left as u16 + up as u16) / 2) as u8,
            _ => paeth(left, up, up_left),
        };
        let v = row[i].wrapping_sub(predicted);
        out[i] = v;
        cost += (v as i8).unsigned_abs() as u64;
    }
    cost
}

/// Encodes `image` as a JPEG (alpha is ignored).
pub fn encode_jpeg(image: &Image, options: &JpegOptions) -> Result<Vec<u8>, ImageError> {
    let (Ok(width), Ok(height)) = (u16::try_from(image.width), u16::try_from(image.height)) else {
        return Err(ImageError::InvalidArgument("a JPEG is at most 65535 pixels wide and high"));
    };
    check(image)?;
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
    check(image)?;
    qoi::encode_to_vec(samples(image).0, image.width, image.height)
        .map_err(|_| ImageError::InvalidArgument("QOI encoding failed"))
}

/// The file header's size, and the two information headers' (`BITMAPINFOHEADER`'s and
/// `BITMAPV4HEADER`'s).
const BMP_FILE_HEADER: usize = 14;
const BMP_INFO_HEADER: usize = 40;
const BMP_V4_HEADER: usize = 108;
/// 72 pixels an inch, in pixels a metre.
const BMP_RESOLUTION: u32 = 2835;

/// Encodes `image` as a BMP, its rows bottom up: 24 bits a pixel (`BITMAPINFOHEADER`) if it is
/// opaque; otherwise 32, with the channels' masks and the sRGB colour space in a
/// `BITMAPV4HEADER`, which readers that know alpha read it from.
pub fn encode_bmp(image: &Image) -> Result<Vec<u8>, ImageError> {
    check(image)?;
    let (Ok(width), Ok(height)) = (i32::try_from(image.width), i32::try_from(image.height)) else {
        return Err(ImageError::InvalidArgument("a BMP is at most 2^31 - 1 pixels wide and high"));
    };
    let alpha = image.has_alpha();
    let header = if alpha { BMP_V4_HEADER } else { BMP_INFO_HEADER };
    let w = image.width as usize;
    let stride = if alpha { w * 4 } else { (w * 3).div_ceil(4) * 4 };
    let offset = BMP_FILE_HEADER + header;
    let size = (stride as u64)
        .checked_mul(image.height as u64)
        .and_then(|data| data.checked_add(offset as u64))
        .and_then(|total| u32::try_from(total).ok())
        .ok_or(ImageError::InvalidArgument("a BMP holds at most 4 GiB"))?;

    let mut out = Vec::new();
    out.try_reserve_exact(size as usize)?;
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(offset as u32).to_le_bytes());

    out.extend_from_slice(&(header as u32).to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    // Positive: the rows go bottom up, which every reader takes.
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(if alpha { 32u16 } else { 24 }).to_le_bytes());
    // BI_BITFIELDS (3) with the masks below, or BI_RGB (0).
    out.extend_from_slice(&(if alpha { 3u32 } else { 0 }).to_le_bytes());
    out.extend_from_slice(&((stride * image.height as usize) as u32).to_le_bytes());
    out.extend_from_slice(&BMP_RESOLUTION.to_le_bytes());
    out.extend_from_slice(&BMP_RESOLUTION.to_le_bytes());
    // No palette; every colour is important.
    out.extend_from_slice(&[0; 8]);
    if alpha {
        for mask in [0x00FF_0000u32, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000] {
            out.extend_from_slice(&mask.to_le_bytes());
        }
        // LCS_sRGB, then the unused endpoints (36 bytes) and gamma (12 bytes).
        out.extend_from_slice(b"BGRs");
        out.extend_from_slice(&[0; 48]);
    }
    for y in (0..image.height as usize).rev() {
        let row = &image.pixels[y * w..(y + 1) * w];
        if alpha {
            // `0xAARRGGBB` is B, G, R, A in memory, as the masks say.
            for p in row {
                out.extend_from_slice(&p.to_le_bytes());
            }
        } else {
            for p in row {
                out.extend_from_slice(&p.to_le_bytes()[..3]);
            }
            out.resize(out.len() + stride - w * 3, 0);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_filters_predict_from_their_neighbours() {
        let prev = [10u8, 20, 30, 40, 50, 60];
        let row = [12u8, 25, 31, 49, 47, 70];
        let mut out = [0u8; 6];
        filter_row(0, &row, &prev, 3, &mut out);
        assert_eq!(out, row, "none");
        filter_row(1, &row, &prev, 3, &mut out);
        assert_eq!(out, [12, 25, 31, 37, 22, 39], "sub: minus the pixel on the left");
        filter_row(2, &row, &prev, 3, &mut out);
        assert_eq!(out, [2, 5, 1, 9, 253, 10], "up: minus the pixel above");
        filter_row(3, &row, &prev, 3, &mut out);
        assert_eq!(out, [7, 15, 16, 23, 10, 25], "average of left and above");
        assert_eq!(paeth(10, 20, 30), 10);
        assert_eq!(paeth(30, 20, 10), 30);
        assert_eq!(paeth(20, 30, 25), 25);
        // A flat row costs nothing with the filter that predicts it.
        let flat = [7u8; 9];
        assert_eq!(filter_row(1, &flat, &[0; 9], 3, &mut [0; 9]), 21);
        assert_eq!(filter_row(2, &flat, &flat, 3, &mut [0; 9]), 0);
    }

    #[test]
    fn png_chunks_carry_their_crc() {
        let mut out = Vec::new();
        write_chunk(&mut out, b"IEND", &[]);
        // The CRC every PNG ends with.
        assert_eq!(out, [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]);
    }
}

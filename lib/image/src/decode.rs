// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Decoding with the format libraries, into [`Image`]s.
//!
//! Every format goes the same way: the library reads the header; the size is checked against
//! the limits and against the file's length; the picture is allocated (fallibly) and the
//! library decodes straight into it; then its samples are put in Veda's order (`B, G, R, A`
//! bytes, one `u32` per pixel).

use zune_bmp::BmpDecoder;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions as ZuneOptions;
use zune_jpeg::JpegDecoder;
use zune_png::{InterlaceMethod, PngDecoder};

use crate::{DecodeOptions, Format, Image, ImageError, ImageInfo, Orientation, exif};

/// How many pixels a byte of a file can describe at most: a JPEG block of 64 pixels takes at
/// least a few bits, deflate expands data at most 1032 times and a PNG pixel takes at least a
/// bit, and a QOI byte repeats at most 62 pixels. A file too short for its size is refused
/// before the picture is allocated.
const JPEG_PIXELS_PER_BYTE: u64 = 2048;
const PNG_PIXELS_PER_BYTE: u64 = 8 * 1032;
const QOI_PIXELS_PER_BYTE: u64 = 62;

/// The decoders' own limit on a side: far beyond this crate's limits (which are checked once
/// the header is read, so that a picture too large is reported with its size), and small
/// enough that no size computed from it can overflow.
const LIBRARY_MAX_SIDE: usize = 1 << 20;

/// The options of the zune decoders.
fn zune_options(options: &DecodeOptions) -> ZuneOptions {
    ZuneOptions::default()
        .set_max_width(LIBRARY_MAX_SIDE)
        .set_max_height(LIBRARY_MAX_SIDE)
        .png_set_confirm_crc(options.verify_checksums)
        .inflate_set_confirm_adler(options.verify_checksums)
        .png_set_strip_to_8bit(true)
        .png_set_decode_animated(false)
}

/// What a header says.
struct Header {
    width: u32,
    height: u32,
    has_alpha: bool,
    orientation: Orientation,
    progressive: bool,
}

/// Reads the header of a file in `format`.
pub(crate) fn read_info(data: &[u8], format: Format) -> Result<ImageInfo, ImageError> {
    let h = match format {
        Format::Jpeg => {
            let mut d = JpegDecoder::new_with_options(ZCursor::new(data), zune_options(&DecodeOptions::DEFAULT));
            d.decode_headers()?;
            jpeg_header(&d)?
        }
        Format::Png => {
            let mut d = PngDecoder::new_with_options(ZCursor::new(data), zune_options(&DecodeOptions::DEFAULT));
            d.decode_headers()?;
            png_header(&d)?
        }
        Format::Bmp => {
            let mut d = BmpDecoder::new_with_options(ZCursor::new(data), zune_options(&DecodeOptions::DEFAULT));
            d.decode_headers()?;
            bmp_header(&d)?
        }
        Format::Qoi => qoi_header(&qoi::decode_header(data)?),
    };
    let (width, height) = if h.orientation.swaps_dimensions() { (h.height, h.width) } else { (h.width, h.height) };
    Ok(ImageInfo {
        format,
        width,
        height,
        has_alpha: h.has_alpha,
        orientation: h.orientation,
        progressive: h.progressive,
    })
}

/// Decodes a file in `format`, returning its pixels as stored and its EXIF orientation.
pub(crate) fn decode(data: &[u8], format: Format, options: &DecodeOptions) -> Result<(Image, Orientation), ImageError> {
    match format {
        Format::Jpeg => jpeg(data, options),
        Format::Png => png(data, options).map(|i| (i, Orientation::Normal)),
        Format::Bmp => bmp(data, options).map(|i| (i, Orientation::Normal)),
        Format::Qoi => qoi(data, options).map(|i| (i, Orientation::Normal)),
    }
}

fn jpeg_header<T: zune_core::bytestream::ZByteReaderTrait>(d: &JpegDecoder<T>) -> Result<Header, ImageError> {
    let info = d.info().ok_or(ImageError::Invalid("JPEG header is missing"))?;
    Ok(Header {
        width: info.width.into(),
        height: info.height.into(),
        has_alpha: false,
        orientation: d.exif().and_then(|e| exif::orientation(e)).unwrap_or_default(),
        progressive: info.sof.is_progressive(),
    })
}

fn jpeg(data: &[u8], options: &DecodeOptions) -> Result<(Image, Orientation), ImageError> {
    // YCbCr, nearly every JPEG, converts straight to Veda's order. The converter is chosen with
    // the header, so another colour model takes a second decoder.
    let zune = zune_options(options);
    let mut d = JpegDecoder::new_with_options(ZCursor::new(data), zune.jpeg_set_out_colorspace(ColorSpace::BGRA));
    d.decode_headers()?;
    let h = jpeg_header(&d)?;
    let mut image = allocate(&h, data.len(), JPEG_PIXELS_PER_BYTE, options)?;
    let layout = match d.input_colorspace() {
        Some(ColorSpace::YCbCr) => {
            expect(d.output_buffer_size(), &image, ColorSpace::BGRA)?;
            d.decode_into(bytemuck::cast_slice_mut(&mut image.pixels))?;
            return Ok((image, h.orientation));
        }
        Some(ColorSpace::Luma) => ColorSpace::RGBA,
        // zune-jpeg 0.5's CMYK and YCCK conversions write three bytes a pixel even into RGBA, so
        // these take RGB, which is right; RGB pictures it copies.
        Some(ColorSpace::CMYK | ColorSpace::YCCK | ColorSpace::RGB) => ColorSpace::RGB,
        _ => return Err(ImageError::Unsupported("this JPEG colour model")),
    };
    let mut d = JpegDecoder::new_with_options(ZCursor::new(data), zune.jpeg_set_out_colorspace(layout));
    d.decode_headers()?;
    expect(d.output_buffer_size(), &image, layout)?;
    d.decode_into(bytemuck::cast_slice_mut(&mut image.pixels))?;
    to_bgra(&mut image.pixels, layout);
    Ok((image, h.orientation))
}

fn png_header<T: zune_core::bytestream::ZByteReaderTrait>(d: &PngDecoder<T>) -> Result<Header, ImageError> {
    let ((width, height), info) = d.dimensions().zip(d.info()).ok_or(ImageError::Invalid("PNG header is missing"))?;
    Ok(Header {
        width: width as u32,
        height: height as u32,
        has_alpha: d.colorspace().is_some_and(|c| c.has_alpha()),
        orientation: Orientation::Normal,
        progressive: info.interlace_method == InterlaceMethod::Adam7,
    })
}

fn png(data: &[u8], options: &DecodeOptions) -> Result<Image, ImageError> {
    let mut d = PngDecoder::new_with_options(ZCursor::new(data), zune_options(options));
    d.decode_headers()?;
    let h = png_header(&d)?;
    let mut image = allocate(&h, data.len(), PNG_PIXELS_PER_BYTE, options)?;
    // 8-bit gray or RGB, with alpha if the file has an alpha channel or `tRNS`. (zune-png's own
    // added alpha channel would undo `tRNS` transparency of gray and RGB pictures.)
    let layout = d.colorspace().ok_or(ImageError::Invalid("PNG header is missing"))?;
    expect(d.output_buffer_size(), &image, layout)?;
    d.decode_into(bytemuck::cast_slice_mut(&mut image.pixels))?;
    to_bgra(&mut image.pixels, layout);
    Ok(image)
}

fn bmp_header<T: zune_core::bytestream::ZByteReaderTrait>(d: &BmpDecoder<T>) -> Result<Header, ImageError> {
    let (width, height) = d.dimensions().ok_or(ImageError::Invalid("BMP header is missing"))?;
    Ok(Header {
        width: width as u32,
        height: height as u32,
        has_alpha: d.colorspace().is_some_and(|c| c.has_alpha()),
        orientation: Orientation::Normal,
        progressive: false,
    })
}

fn bmp(data: &[u8], options: &DecodeOptions) -> Result<Image, ImageError> {
    let mut d = BmpDecoder::new_with_options(ZCursor::new(data), zune_options(options));
    d.decode_headers()?;
    let h = bmp_header(&d)?;
    // Run-length encoded bitmaps can describe a large picture in a few bytes; uncompressed
    // ones need their rows.
    let per_byte = if bmp_is_compressed(data) { u64::MAX } else { 8 };
    let mut image = allocate(&h, data.len(), per_byte, options)?;
    let layout = d.colorspace().ok_or(ImageError::Invalid("BMP header is missing"))?;
    expect(d.output_buf_size(), &image, layout)?;
    d.decode_into(bytemuck::cast_slice_mut(&mut image.pixels))?;
    to_bgra(&mut image.pixels, layout);
    Ok(image)
}

/// Whether a BMP's pixels are run-length encoded (Windows headers of 40 bytes and more give
/// the compression at offset 30: 1 is RLE8, 2 RLE4).
fn bmp_is_compressed(data: &[u8]) -> bool {
    let le32 = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    le32(14).is_some_and(|size| size >= 40) && matches!(le32(30), Some(1 | 2))
}

fn qoi_header(h: &qoi::Header) -> Header {
    Header {
        width: h.width,
        height: h.height,
        has_alpha: h.channels == qoi::Channels::Rgba,
        orientation: Orientation::Normal,
        progressive: false,
    }
}

fn qoi(data: &[u8], options: &DecodeOptions) -> Result<Image, ImageError> {
    let mut d = qoi::Decoder::new(data)?.with_channels(qoi::Channels::Rgba);
    let h = qoi_header(d.header());
    let mut image = allocate(&h, data.len(), QOI_PIXELS_PER_BYTE, options)?;
    expect(Some(d.required_buf_len()), &image, ColorSpace::RGBA)?;
    d.decode_to_buf(bytemuck::cast_slice_mut::<u32, u8>(&mut image.pixels))?;
    to_bgra(&mut image.pixels, ColorSpace::RGBA);
    Ok(image)
}

/// Checks a header's size against the limits and the file's length, and allocates its picture.
fn allocate(h: &Header, file_len: usize, pixels_per_byte: u64, options: &DecodeOptions) -> Result<Image, ImageError> {
    options.check_dimensions(h.width, h.height)?;
    if h.width as u64 * h.height as u64 > (file_len as u64).saturating_mul(pixels_per_byte) {
        return Err(ImageError::Truncated);
    }
    Image::try_new(h.width, h.height)
}

/// Makes sure that a decoder writes `image`'s pixels in `layout`, which [`to_bgra`] rearranges,
/// and that they fit.
fn expect(output_len: Option<usize>, image: &Image, layout: ColorSpace) -> Result<(), ImageError> {
    let channels = match layout {
        ColorSpace::BGRA | ColorSpace::RGBA => 4,
        ColorSpace::RGB => 3,
        ColorSpace::LumaA => 2,
        ColorSpace::Luma => 1,
        _ => return Err(ImageError::Unsupported("this pixel format")),
    };
    if output_len != Some(image.pixels.len() * channels) {
        return Err(ImageError::Unsupported("this pixel format"));
    }
    Ok(())
}

/// Rearranges pixels a decoder wrote in `layout` (8-bit samples, packed from the start of the
/// buffer) into Veda's `0xAARRGGBB` words, in place.
fn to_bgra(pixels: &mut [u32], layout: ColorSpace) {
    let bytes: &mut [u8] = bytemuck::cast_slice_mut(pixels);
    let n = bytes.len() / 4;
    match layout {
        ColorSpace::BGRA => {}
        ColorSpace::RGBA => {
            for p in bytes.as_chunks_mut::<4>().0 {
                p.swap(0, 2);
            }
        }
        // Narrower pixels spread out from the end, so none is overwritten before it is read.
        ColorSpace::RGB => {
            for i in (0..n).rev() {
                let (r, g, b) = (bytes[3 * i], bytes[3 * i + 1], bytes[3 * i + 2]);
                bytes[4 * i..4 * i + 4].copy_from_slice(&[b, g, r, 0xFF]);
            }
        }
        ColorSpace::LumaA => {
            for i in (0..n).rev() {
                let (l, a) = (bytes[2 * i], bytes[2 * i + 1]);
                bytes[4 * i..4 * i + 4].copy_from_slice(&[l, l, l, a]);
            }
        }
        ColorSpace::Luma => {
            for i in (0..n).rev() {
                let l = bytes[i];
                bytes[4 * i..4 * i + 4].copy_from_slice(&[l, l, l, 0xFF]);
            }
        }
        // [`expect`] lets no other layout through.
        _ => {}
    }
}

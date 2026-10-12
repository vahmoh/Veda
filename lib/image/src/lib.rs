//! Images for Veda: decoding, scaling and (on the build machine) encoding,
//! on proven libraries.
//!
//! `vimage` opens the pictures of Photos, the wallpapers and the file
//! manager's thumbnails, and scales them. The codecs are not Veda's own:
//!
//! | Format | Decoder |
//! |--------|---------|
//! | JPEG | [`zune_jpeg`] (the `image` crate's): baseline, extended and progressive, any sampling, restarts, grayscale, CMYK and YCCK |
//! | PNG  | [`zune_png`]: every colour type and bit depth, Adam7, `tRNS` (an APNG shows its default image) |
//! | BMP  | [`zune_bmp`]: 1 to 32 bits, bit fields, RLE4 and RLE8, OS/2 and Windows V1-V5 headers |
//! | QOI  | [`qoi`] |
//!
//! Scaling is [`fast_image_resize`]'s convolution, alpha-correct and using
//! the processor's SIMD where it has it. With the `encode` feature (the
//! build machine's tools), [`encode_png`] (the `png` crate), [`encode_jpeg`]
//! (`jpeg-encoder`) and [`encode_qoi`] write pictures.
//!
//! What this crate adds is Veda's side: one [`Image`] type in the
//! framebuffer's pixel layout, limits on what a file can make it allocate,
//! EXIF orientation, and errors an application can explain to its user.
//!
//! # Pixels
//!
//! Every decoder produces an [`Image`]: `width * height` pixels in row-major order without
//! padding, each a `u32` in `0xAARRGGBB` order with straight (non-premultiplied) alpha. That is
//! the byte order `B, G, R, A` in memory, which matches the Veda framebuffer.
//!
//! # Entry points
//!
//! * [`detect`] identifies a format from its magic bytes.
//! * [`decode`] / [`decode_with`] decode any supported format; [`decode_lenient`] also accepts
//!   files whose only fault is a checksum, and says so.
//! * [`read_info`] returns dimensions and alpha information without decoding pixels.
//! * [`resize`], [`thumbnail`] and [`crop`] scale and cut images; [`apply_orientation`] and the
//!   rotations turn them.
//!
//! # Robustness
//!
//! Malformed input is reported as an [`ImageError`] (the tests feed the decoders thousands of
//! damaged files). Dimensions are limited by [`DecodeOptions`] (16384 x 16384 and 64 Mi pixels
//! by default); the picture is only allocated once the file is long enough to describe it, and
//! fallibly ([`ImageError::OutOfMemory`]). The decoders' own working memory (a progressive
//! JPEG's coefficients, a PNG's inflated rows) is in proportion to the picture.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod decode;
#[cfg(feature = "encode")]
mod encode;
mod error;
mod exif;
mod image;
mod ops;

#[cfg(test)]
mod tests;

#[cfg(feature = "encode")]
pub use encode::{JpegOptions, Subsampling, encode_jpeg, encode_png, encode_qoi};
pub use error::ImageError;
pub use image::{Image, Orientation};
pub use ops::{
    Filter, apply_orientation, crop, fit_size, flip_horizontal, flip_vertical, resize, rotate90, rotate180, rotate270,
    thumbnail,
};

/// Default maximum width or height accepted by the decoders.
pub const MAX_DIMENSION: u32 = 16384;
/// Default maximum number of pixels (`width * height`) accepted by the decoders.
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// An image file format supported by this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// Portable Network Graphics.
    Png,
    /// JPEG (JFIF / EXIF).
    Jpeg,
    /// Windows bitmap.
    Bmp,
    /// Quite OK Image format.
    Qoi,
}

impl Format {
    /// A short human-readable name, e.g. `"PNG"`.
    pub fn name(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpeg => "JPEG",
            Format::Bmp => "BMP",
            Format::Qoi => "QOI",
        }
    }

    /// The usual file extension without the dot, e.g. `"png"`.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpg",
            Format::Bmp => "bmp",
            Format::Qoi => "qoi",
        }
    }

    /// The MIME type, e.g. `"image/png"`.
    pub fn mime_type(self) -> &'static str {
        match self {
            Format::Png => "image/png",
            Format::Jpeg => "image/jpeg",
            Format::Bmp => "image/bmp",
            Format::Qoi => "image/qoi",
        }
    }
}

/// Basic facts about an image file, obtained without decoding its pixels (see [`read_info`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    /// The file format.
    pub format: Format,
    /// Width of the image [`decode`] returns, i.e. after applying [`ImageInfo::orientation`].
    pub width: u32,
    /// Height of the image [`decode`] returns, i.e. after applying [`ImageInfo::orientation`].
    pub height: u32,
    /// Whether the image may contain non-opaque pixels (alpha channel, PNG `tRNS`, BMP alpha mask).
    pub has_alpha: bool,
    /// EXIF orientation stored in the file ([`Orientation::Normal`] for formats without EXIF).
    pub orientation: Orientation,
    /// The file is a progressive JPEG or an Adam7-interlaced PNG.
    pub progressive: bool,
}

/// Limits and switches for decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeOptions {
    /// Maximum accepted width and height.
    pub max_dimension: u32,
    /// Maximum accepted `width * height`.
    pub max_pixels: u64,
    /// Verify PNG chunk CRCs and the zlib Adler-32 checksum.
    pub verify_checksums: bool,
    /// Rotate/flip JPEG images according to their EXIF orientation tag.
    pub apply_orientation: bool,
}

impl DecodeOptions {
    /// The defaults: [`MAX_DIMENSION`], [`MAX_PIXELS`], checksums verified, orientation applied.
    pub const DEFAULT: DecodeOptions = DecodeOptions {
        max_dimension: MAX_DIMENSION,
        max_pixels: MAX_PIXELS,
        verify_checksums: true,
        apply_orientation: true,
    };

    /// Checks `width x height` against the limits.
    pub fn check_dimensions(&self, width: u32, height: u32) -> Result<(), ImageError> {
        if width == 0 || height == 0 {
            return Err(ImageError::Invalid("image has a zero dimension"));
        }
        if width > self.max_dimension || height > self.max_dimension || width as u64 * height as u64 > self.max_pixels {
            return Err(ImageError::TooLarge { width, height });
        }
        Ok(())
    }
}

impl Default for DecodeOptions {
    fn default() -> Self {
        DecodeOptions::DEFAULT
    }
}

/// The PNG file signature.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];

/// Identifies the format of `data` from its magic bytes.
pub fn detect(data: &[u8]) -> Option<Format> {
    if data.starts_with(&PNG_SIGNATURE) {
        Some(Format::Png)
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if data.starts_with(b"qoif") {
        Some(Format::Qoi)
    } else if zune_bmp::probe_bmp(data) {
        Some(Format::Bmp)
    } else {
        None
    }
}

/// Decodes an image in any supported format with the default [`DecodeOptions`].
pub fn decode(data: &[u8]) -> Result<Image, ImageError> {
    decode_with(data, &DecodeOptions::DEFAULT)
}

/// Decodes an image in any supported format.
pub fn decode_with(data: &[u8], options: &DecodeOptions) -> Result<Image, ImageError> {
    let format = detect(data).ok_or(ImageError::UnknownFormat)?;
    let (image, orientation) = decode::decode(data, format, options)?;
    if options.apply_orientation && orientation != Orientation::Normal {
        return ops::try_apply_orientation(&image, orientation);
    }
    Ok(image)
}

/// Decodes like [`decode`], but when the only problem is a checksum (a PNG chunk CRC or the zlib
/// Adler-32, which some tools do not update) decodes again without verifying checksums. The
/// flag is `true` in that case, so that viewers can say that the file is damaged.
pub fn decode_lenient(data: &[u8]) -> Result<(Image, bool), ImageError> {
    match decode(data) {
        Err(ImageError::ChecksumMismatch(_)) => {
            let lax = DecodeOptions { verify_checksums: false, ..DecodeOptions::DEFAULT };
            decode_with(data, &lax).map(|img| (img, true))
        }
        other => other.map(|img| (img, false)),
    }
}

/// Reads the format, dimensions and alpha information of an image without decoding its pixels.
///
/// This only parses headers, so it is cheap enough for file listings.
pub fn read_info(data: &[u8]) -> Result<ImageInfo, ImageError> {
    decode::read_info(data, detect(data).ok_or(ImageError::UnknownFormat)?)
}

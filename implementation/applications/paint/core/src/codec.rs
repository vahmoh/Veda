// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Pictures from and to files: PNG, JPEG, BMP and QOI, by `vimage`'s decoders and encoders.
//!
//! Paint works on premultiplied pixels and files hold straight ones, so pictures are converted
//! on the way in and out. A JPEG has no transparency: what is transparent is saved over white,
//! as it would print.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use vgfx::Bitmap;
use vimage::{DecodeOptions, Format, Image, ImageError, JpegOptions, Subsampling};

use crate::color::unpremultiply;

/// The formats Paint saves in, by their files' extensions.
pub const SAVE_FORMATS: [(Format, &str, &str); 4] = [
    (Format::Png, "png", "PNG picture"),
    (Format::Jpeg, "jpg", "JPEG picture"),
    (Format::Bmp, "bmp", "BMP picture"),
    (Format::Qoi, "qoi", "QOI picture"),
];

/// The format a file's name says (by its extension), if Paint knows it.
pub fn format_of(path: &str) -> Option<Format> {
    let ext = vfiles::path::extension(path);
    Some(match ext.as_str() {
        "png" => Format::Png,
        "jpg" | "jpeg" | "jfif" | "jpe" => Format::Jpeg,
        "bmp" | "dib" => Format::Bmp,
        "qoi" => Format::Qoi,
        _ => return None,
    })
}

/// Why a picture could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    Image(ImageError),
    /// The picture has more pixels than Paint may take (the limit, in pixels).
    TooLarge(u64),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodecError::Image(ImageError::UnknownFormat) => {
                f.write_str("it is not a picture Paint can open (PNG, JPEG, BMP or QOI)")
            }
            CodecError::Image(ImageError::OutOfMemory) => f.write_str("there is not enough memory for it"),
            CodecError::Image(e) => write!(f, "{e}"),
            CodecError::TooLarge(most) => {
                write!(f, "it is too large to edit here (Paint takes up to {} megapixels)", most / 1_000_000)
            }
        }
    }
}

impl From<ImageError> for CodecError {
    fn from(e: ImageError) -> Self {
        CodecError::Image(e)
    }
}

/// A picture read from a file.
pub struct Opened {
    pub picture: Bitmap,
    pub format: Format,
    /// Its checksums were wrong (it was read all the same).
    pub damaged: bool,
}

/// Reads a picture of at most `max_pixels` pixels.
pub fn decode(data: &[u8], max_pixels: u64) -> Result<Opened, CodecError> {
    let format = vimage::detect(data).ok_or(ImageError::UnknownFormat)?;
    let options = DecodeOptions { max_pixels, ..DecodeOptions::DEFAULT };
    let lenient = DecodeOptions { verify_checksums: false, ..options };
    let (image, damaged) = match vimage::decode_with(data, &options) {
        Ok(i) => (i, false),
        Err(ImageError::ChecksumMismatch(_)) => (vimage::decode_with(data, &lenient)?, true),
        Err(ImageError::TooLarge { .. }) => return Err(CodecError::TooLarge(max_pixels)),
        Err(e) => return Err(e.into()),
    };
    let picture = Bitmap::from_straight(image.width as i32, image.height as i32, image.pixels);
    Ok(Opened { picture, format, damaged })
}

/// The picture with straight colours, over white where `flatten` (for formats without
/// transparency).
fn straight(picture: &Bitmap, flatten: bool) -> Image {
    let pixels: Vec<u32> = picture
        .pixels
        .iter()
        .map(|&p| if flatten { unpremultiply(vgfx::color::over(p, 0xFFFF_FFFF)) } else { unpremultiply(p) })
        .collect();
    Image { width: picture.width as u32, height: picture.height as u32, pixels }
}

/// The file of `picture` in `format`; `quality` (1 to 100) is a JPEG's.
pub fn encode(picture: &Bitmap, format: Format, quality: u8) -> Result<Vec<u8>, CodecError> {
    let image = straight(picture, format == Format::Jpeg);
    Ok(match format {
        Format::Jpeg => {
            let options = JpegOptions {
                quality: quality.clamp(1, 100),
                // Full colour resolution for the sharp edges drawings have.
                subsampling: Subsampling::Yuv444,
                optimize_huffman: true,
                ..JpegOptions::default()
            };
            vimage::encode_jpeg(&image, &options)?
        }
        other => vimage::encode(&image, other)?,
    })
}

/// A name for a new picture's file: `Untitled.png`, or the picture's own name with the
/// extension of `format`.
pub fn file_name(current: Option<&str>, format: Format) -> String {
    let base = current.map(vfiles::path::file_name).unwrap_or("Untitled");
    let stem = match base.rfind('.') {
        Some(i) if i > 0 => &base[..i],
        _ => base,
    };
    let ext = SAVE_FORMATS.iter().find(|f| f.0 == format).map_or("png", |f| f.1);
    alloc::format!("{stem}.{ext}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture() -> Bitmap {
        let mut b = Bitmap::new(16, 8);
        for (i, p) in b.pixels.iter_mut().enumerate() {
            let a = if i % 3 == 0 { 0x80 } else { 0xFF };
            *p = vgfx::color::premultiply(a << 24 | (i as u32 * 16 % 256) << 16 | 0x4080);
        }
        b
    }

    #[test]
    fn pictures_round_trip_through_files() {
        let b = picture();
        for format in [Format::Png, Format::Bmp, Format::Qoi] {
            let file = encode(&b, format, 90).unwrap();
            let back = decode(&file, 1 << 20).unwrap();
            assert_eq!(back.format, format);
            assert!(!back.damaged);
            // Premultiplying straight colours again loses nothing of what was premultiplied.
            assert_eq!(back.picture.pixels, b.pixels, "{format:?}");
        }
        // A JPEG is opaque: transparency goes over white.
        let jpeg = decode(&encode(&b, Format::Jpeg, 95).unwrap(), 1 << 20).unwrap();
        assert!(jpeg.picture.pixels.iter().all(|p| p >> 24 == 0xFF));
    }

    #[test]
    fn limits_and_names() {
        let file = encode(&picture(), Format::Png, 90).unwrap();
        assert_eq!(decode(&file, 100).err(), Some(CodecError::TooLarge(100)));
        assert!(matches!(decode(b"not a picture", 1 << 20), Err(CodecError::Image(ImageError::UnknownFormat))));
        assert_eq!(format_of("/home/user/Pictures/Cat.JPEG"), Some(Format::Jpeg));
        assert_eq!(format_of("notes.txt"), None);
        assert_eq!(file_name(Some("/home/user/Pictures/Cat.jpg"), Format::Png), "Cat.png");
        assert_eq!(file_name(None, Format::Bmp), "Untitled.bmp");
        assert_eq!(file_name(Some(".hidden"), Format::Png), ".hidden.png");
    }
}

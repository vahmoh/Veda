//! The error type, and how the format libraries' errors map onto it.

use core::fmt;

use zune_bmp::BmpDecoderErrors;
use zune_inflate::errors::DecodeErrorStatus as InflateError;
use zune_jpeg::errors::{DecodeErrors as JpegError, UnsupportedSchemes};
use zune_png::error::PngDecodeErrors;

/// Errors returned by `vimage` decoders, encoders and image operations.
///
/// Malformed input never panics; every problem is reported as one of these variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageError {
    /// The data does not start with the signature of a supported format.
    UnknownFormat,
    /// The data ended before the image or compressed stream was complete.
    Truncated,
    /// The data is malformed; the message describes the problem.
    Invalid(&'static str),
    /// The data is valid but uses a feature this crate does not implement.
    Unsupported(&'static str),
    /// The image dimensions exceed the limits in [`DecodeOptions`](crate::DecodeOptions).
    TooLarge {
        /// Width declared by the file.
        width: u32,
        /// Height declared by the file.
        height: u32,
    },
    /// A checksum (PNG chunk CRC-32 or zlib Adler-32) does not match the data; the message names it.
    ChecksumMismatch(&'static str),
    /// A memory allocation failed.
    OutOfMemory,
    /// An argument passed by the caller is invalid (for example an empty image given to an encoder).
    InvalidArgument(&'static str),
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImageError::UnknownFormat => f.write_str("unknown image format"),
            ImageError::Truncated => f.write_str("image data is truncated"),
            ImageError::Invalid(msg) => write!(f, "invalid image data: {msg}"),
            ImageError::Unsupported(msg) => write!(f, "unsupported image feature: {msg}"),
            ImageError::TooLarge { width, height } => write!(f, "image size {width}x{height} exceeds the limits"),
            ImageError::ChecksumMismatch(what) => write!(f, "{what} checksum mismatch"),
            ImageError::OutOfMemory => f.write_str("out of memory"),
            ImageError::InvalidArgument(msg) => write!(f, "invalid argument: {msg}"),
        }
    }
}

impl From<alloc::collections::TryReserveError> for ImageError {
    fn from(_: alloc::collections::TryReserveError) -> Self {
        ImageError::OutOfMemory
    }
}

impl From<JpegError> for ImageError {
    fn from(e: JpegError) -> Self {
        match e {
            // The input ran out (reads past the end are I/O errors of the cursor).
            JpegError::ExhaustedData | JpegError::IoErrors(_) => ImageError::Truncated,
            JpegError::Unsupported(scheme) => ImageError::Unsupported(match scheme {
                UnsupportedSchemes::LosslessHuffman | UnsupportedSchemes::LosslessArithmetic => "lossless JPEG",
                UnsupportedSchemes::ExtendedSequentialDctArithmetic | UnsupportedSchemes::ProgressiveDctArithmetic => {
                    "arithmetic-coded JPEG"
                }
                UnsupportedSchemes::ExtendedSequentialHuffman => "this kind of JPEG",
            }),
            JpegError::IllegalMagicBytes(_) => ImageError::Invalid("not a JPEG file"),
            JpegError::HuffmanDecode(_) => ImageError::Invalid("bad JPEG Huffman data"),
            JpegError::DqtError(_) => ImageError::Invalid("bad JPEG quantization table"),
            JpegError::SosError(_) => ImageError::Invalid("bad JPEG scan header"),
            // zune-jpeg tells 12- and 16-bit samples, which it does not decode, only in words.
            JpegError::SofError(why) if why.contains("bits of precision") => {
                ImageError::Unsupported("JPEG sample precision other than 8 bits")
            }
            JpegError::SofError(_) => ImageError::Invalid("bad JPEG frame header"),
            JpegError::MCUError(_) => ImageError::Invalid("bad JPEG block data"),
            JpegError::LargeDimensions(_) => ImageError::Invalid("JPEG dimensions out of range"),
            _ => ImageError::Invalid("malformed JPEG data"),
        }
    }
}

impl From<PngDecodeErrors> for ImageError {
    fn from(e: PngDecodeErrors) -> Self {
        match e {
            PngDecodeErrors::BadCrc(..) => ImageError::ChecksumMismatch("PNG chunk CRC"),
            PngDecodeErrors::ZlibDecodeErrors(z) => match z.error {
                InflateError::InsufficientData => ImageError::Truncated,
                InflateError::MismatchedAdler(..) => ImageError::ChecksumMismatch("zlib Adler-32"),
                _ => ImageError::Invalid("bad PNG compressed data"),
            },
            PngDecodeErrors::IoErrors(_) => ImageError::Truncated,
            PngDecodeErrors::BadSignature => ImageError::Invalid("not a PNG file"),
            PngDecodeErrors::EmptyPalette => ImageError::Invalid("PNG palette is missing"),
            _ => ImageError::Invalid("malformed PNG data"),
        }
    }
}

impl From<BmpDecoderErrors> for ImageError {
    fn from(e: BmpDecoderErrors) -> Self {
        match e {
            BmpDecoderErrors::IoErrors(_) => ImageError::Truncated,
            BmpDecoderErrors::InvalidMagicBytes => ImageError::Invalid("not a BMP file"),
            BmpDecoderErrors::TooLargeDimensions(..) => ImageError::Invalid("BMP dimensions out of range"),
            _ => ImageError::Invalid("malformed BMP data"),
        }
    }
}

impl From<qoi::Error> for ImageError {
    fn from(e: qoi::Error) -> Self {
        match e {
            qoi::Error::UnexpectedBufferEnd => ImageError::Truncated,
            qoi::Error::InvalidMagic { .. } => ImageError::Invalid("not a QOI file"),
            qoi::Error::InvalidPadding => ImageError::Invalid("QOI end marker is missing"),
            _ => ImageError::Invalid("malformed QOI data"),
        }
    }
}

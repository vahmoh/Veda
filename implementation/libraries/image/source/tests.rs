// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Tests of this crate's side of decoding: every format and colour model reaches Veda's pixel
//! order exactly, the limits hold, checksums can be skipped, scaling and orientation are
//! right, and damaged files (mutation fuzzing) give a picture or an error, never a panic.

use std::println;
use std::vec;
use std::vec::Vec;

use crate::{
    DecodeOptions, Filter, Format, Image, ImageError, Orientation, decode, decode_lenient, decode_with, detect,
    read_info,
};

/// xorshift64* pseudo-random numbers (deterministic, so failures are reproducible).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

const fn argb(a: u32, r: u32, g: u32, b: u32) -> u32 {
    a << 24 | r << 16 | g << 8 | b
}

/// A PNG written chunk by chunk: `samples` are a picture's samples (palette indices, gray
/// levels or colour channels) at `depth` bits, row by row, optionally Adam7-interlaced.
struct TestPng<'a> {
    width: u32,
    height: u32,
    /// PNG colour type: 0 gray, 2 RGB, 3 palette, 4 gray and alpha, 6 RGBA.
    color: u8,
    depth: u8,
    interlaced: bool,
    samples: &'a [u16],
    palette: &'a [u8],
    trns: Option<&'a [u8]>,
}

impl TestPng<'_> {
    fn channels(&self) -> usize {
        match self.color {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            _ => 4,
        }
    }

    /// The filtered (filter type 0) rows of the pixels at `xs` x `ys`.
    fn rows(&self, xs: &[usize], ys: &[usize], raw: &mut Vec<u8>) {
        let c = self.channels();
        for &y in ys {
            raw.push(0);
            let (mut acc, mut bits) = (0u32, 0u32);
            for &x in xs {
                for k in 0..c {
                    let v = self.samples[(y * self.width as usize + x) * c + k] as u32;
                    match self.depth {
                        16 => raw.extend_from_slice(&(v as u16).to_be_bytes()),
                        8 => raw.push(v as u8),
                        d => {
                            acc = acc << d | v;
                            bits += d as u32;
                            if bits == 8 {
                                raw.push(acc as u8);
                                (acc, bits) = (0, 0);
                            }
                        }
                    }
                }
            }
            if bits > 0 {
                raw.push((acc << (8 - bits)) as u8);
            }
        }
    }

    fn build(&self) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut raw = Vec::new();
        if self.interlaced {
            for (x0, y0, dx, dy) in
                [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]
            {
                let xs: Vec<usize> = (x0..w).step_by(dx).collect();
                let ys: Vec<usize> = (y0..h).step_by(dy).collect();
                if !xs.is_empty() && !ys.is_empty() {
                    self.rows(&xs, &ys, &mut raw);
                }
            }
        } else {
            self.rows(&(0..w).collect::<Vec<_>>(), &(0..h).collect::<Vec<_>>(), &mut raw);
        }
        let mut out = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];
        let mut chunk = |kind: &[u8; 4], data: &[u8]| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut crc = crc32fast::Hasher::new();
            crc.update(kind);
            crc.update(data);
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            out.extend_from_slice(&crc.finalize().to_be_bytes());
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&self.width.to_be_bytes());
        ihdr.extend_from_slice(&self.height.to_be_bytes());
        ihdr.extend_from_slice(&[self.depth, self.color, 0, 0, self.interlaced as u8]);
        chunk(b"IHDR", &ihdr);
        if !self.palette.is_empty() {
            chunk(b"PLTE", self.palette);
        }
        if let Some(t) = self.trns {
            chunk(b"tRNS", t);
        }
        chunk(b"IDAT", &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6));
        chunk(b"IEND", &[]);
        out
    }
}

/// Builds a BMP with a `BITMAPINFOHEADER` (or a core header, `hsize` 12) around raw pixel rows.
fn bmp(
    hsize: u32,
    width: i32,
    height: i32,
    bpp: u16,
    compression: u32,
    palette: &[[u8; 4]],
    masks: &[u32],
    pixels: &[u8],
) -> Vec<u8> {
    let mut info = Vec::new();
    info.extend_from_slice(&hsize.to_le_bytes());
    if hsize == 12 {
        info.extend_from_slice(&(width as u16).to_le_bytes());
        info.extend_from_slice(&(height as u16).to_le_bytes());
        info.extend_from_slice(&1u16.to_le_bytes());
        info.extend_from_slice(&bpp.to_le_bytes());
    } else {
        info.extend_from_slice(&width.to_le_bytes());
        info.extend_from_slice(&height.to_le_bytes());
        info.extend_from_slice(&1u16.to_le_bytes());
        info.extend_from_slice(&bpp.to_le_bytes());
        info.extend_from_slice(&compression.to_le_bytes());
        info.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
        info.extend_from_slice(&[0; 8]);
        info.extend_from_slice(&(palette.len() as u32).to_le_bytes());
        info.extend_from_slice(&[0; 4]);
        if hsize > 40 {
            for m in masks {
                info.extend_from_slice(&m.to_le_bytes());
            }
        }
        info.resize(hsize as usize, 0);
        if hsize == 40 {
            for m in masks {
                info.extend_from_slice(&m.to_le_bytes());
            }
        }
    }
    let mut pal = Vec::new();
    for p in palette {
        pal.extend_from_slice(if hsize == 12 { &p[..3] } else { &p[..] });
    }
    let offset = 14 + info.len() + pal.len();
    let mut out = b"BM".to_vec();
    out.extend_from_slice(&((offset + pixels.len()) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(offset as u32).to_le_bytes());
    out.extend_from_slice(&info);
    out.extend_from_slice(&pal);
    out.extend_from_slice(pixels);
    out
}

/// A JPEG of `image` by `jpeg-encoder`, with `color` its samples' model.
fn jpeg(image: &Image, setup: impl FnOnce(&mut jpeg_encoder::Encoder<&mut Vec<u8>>)) -> Vec<u8> {
    let mut out = Vec::new();
    let mut e = jpeg_encoder::Encoder::new(&mut out, 90);
    setup(&mut e);
    let bytes: &[u8] = bytemuck::cast_slice(&image.pixels);
    e.encode(bytes, image.width as u16, image.height as u16, jpeg_encoder::ColorType::Bgra).unwrap();
    out
}

/// A smooth opaque picture (JPEG-friendly).
fn photo(w: u32, h: u32) -> Image {
    Image::from_fn(w, h, |x, y| argb(255, x * 255 / w, y * 255 / h, (x + y) * 127 / (w + h)))
}

/// The mean absolute difference of two pictures' colour channels.
fn mean_error(a: &Image, b: &Image) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let mut sum = 0u64;
    for (&p, &q) in a.pixels.iter().zip(&b.pixels) {
        for s in [0, 8, 16] {
            sum += ((p >> s) & 255).abs_diff((q >> s) & 255) as u64;
        }
    }
    sum as f64 / (3 * a.pixels.len()) as f64
}

#[test]
fn formats_are_told_by_their_magic() {
    let png =
        TestPng { width: 1, height: 1, color: 0, depth: 8, interlaced: false, samples: &[7], palette: &[], trns: None };
    assert_eq!(detect(&png.build()), Some(Format::Png));
    assert_eq!(detect(&jpeg(&photo(8, 8), |_| {})), Some(Format::Jpeg));
    assert_eq!(detect(&qoi::encode_to_vec([1u8, 2, 3], 1, 1).unwrap()), Some(Format::Qoi));
    assert_eq!(detect(&bmp(40, 1, 1, 24, 0, &[], &[], &[1, 2, 3, 0])), Some(Format::Bmp));
    assert_eq!(detect(b"BM is not enough"), None);
    assert_eq!(detect(b"GIF89a"), None);
    assert_eq!(decode(b"GIF89a....").unwrap_err(), ImageError::UnknownFormat);
}

#[test]
fn png_colour_types_and_depths() {
    // 8-bit RGBA and RGB.
    let rgba: Vec<u16> = (0..3 * 2 * 4).map(|i| (i * 21 % 256) as u16).collect();
    let file = TestPng {
        width: 3,
        height: 2,
        color: 6,
        depth: 8,
        interlaced: false,
        samples: &rgba,
        palette: &[],
        trns: None,
    };
    let want: Vec<u32> = rgba.chunks(4).map(|p| argb(p[3] as u32, p[0] as u32, p[1] as u32, p[2] as u32)).collect();
    assert_eq!(decode(&file.build()).unwrap().pixels, want);
    let rgb: Vec<u16> = (0..3 * 2 * 3).map(|i| (i * 37 % 256) as u16).collect();
    let file =
        TestPng { width: 3, height: 2, color: 2, depth: 8, interlaced: false, samples: &rgb, palette: &[], trns: None };
    let want: Vec<u32> = rgb.chunks(3).map(|p| argb(255, p[0] as u32, p[1] as u32, p[2] as u32)).collect();
    assert_eq!(decode(&file.build()).unwrap().pixels, want);
    // Gray at 1, 2, 4 and 8 bits, scaled to 8 bits; one level transparent through tRNS.
    for depth in [1u8, 2, 4, 8] {
        let max = (1u16 << depth) - 1;
        let gray: Vec<u16> = (0..7 * 3).map(|i| i as u16 % (max + 1)).collect();
        let trns = [0, 1];
        let file = TestPng {
            width: 7,
            height: 3,
            color: 0,
            depth,
            interlaced: false,
            samples: &gray,
            palette: &[],
            trns: Some(&trns),
        };
        let want: Vec<u32> = gray
            .iter()
            .map(|&v| {
                let l = v as u32 * 255 / max as u32;
                argb(if v == 1 { 0 } else { 255 }, l, l, l)
            })
            .collect();
        let decoded = decode(&file.build()).unwrap_or_else(|e| panic!("{depth}-bit gray: {e}"));
        assert_eq!(decoded.pixels, want, "{depth}-bit gray");
        assert!(read_info(&file.build()).unwrap().has_alpha);
    }
    // Gray and alpha, and 16-bit RGB (whose low bytes are dropped).
    let ga: Vec<u16> = (0..4 * 2 * 2).map(|i| (i * 31 % 256) as u16).collect();
    let file =
        TestPng { width: 4, height: 2, color: 4, depth: 8, interlaced: false, samples: &ga, palette: &[], trns: None };
    let want: Vec<u32> = ga.chunks(2).map(|p| argb(p[1] as u32, p[0] as u32, p[0] as u32, p[0] as u32)).collect();
    assert_eq!(decode(&file.build()).unwrap().pixels, want);
    let rgb16: Vec<u16> = (0..2 * 2 * 3).map(|i| (i * 9973 % 65536) as u16).collect();
    let file = TestPng {
        width: 2,
        height: 2,
        color: 2,
        depth: 16,
        interlaced: false,
        samples: &rgb16,
        palette: &[],
        trns: None,
    };
    let want: Vec<u32> =
        rgb16.chunks(3).map(|p| argb(255, p[0] as u32 >> 8, p[1] as u32 >> 8, p[2] as u32 >> 8)).collect();
    assert_eq!(decode(&file.build()).unwrap().pixels, want);
    // A 4-bit palette, partly transparent.
    let palette: Vec<u8> = (0..13 * 3).map(|i| (i * 19) as u8).collect();
    let trns = [0u8, 50, 100];
    let idx: Vec<u16> = (0..12 * 10).map(|i| (i * 7 % 13) as u16).collect();
    let file = TestPng {
        width: 12,
        height: 10,
        color: 3,
        depth: 4,
        interlaced: false,
        samples: &idx,
        palette: &palette,
        trns: Some(&trns),
    };
    let want: Vec<u32> = idx
        .iter()
        .map(|&i| {
            let i = i as usize;
            let a = trns.get(i).copied().unwrap_or(255) as u32;
            argb(a, palette[3 * i] as u32, palette[3 * i + 1] as u32, palette[3 * i + 2] as u32)
        })
        .collect();
    assert_eq!(decode(&file.build()).unwrap().pixels, want);
}

#[test]
fn interlaced_pngs_decode_like_plain_ones() {
    let rgba: Vec<u16> = (0..13 * 11 * 4).map(|i| (i * 29 % 256) as u16).collect();
    let plain = TestPng {
        width: 13,
        height: 11,
        color: 6,
        depth: 8,
        interlaced: false,
        samples: &rgba,
        palette: &[],
        trns: None,
    };
    let adam7 = TestPng { interlaced: true, ..plain };
    assert_eq!(decode(&adam7.build()).unwrap(), decode(&plain.build()).unwrap());
    let info = read_info(&adam7.build()).unwrap();
    assert!(info.progressive && info.has_alpha && (info.width, info.height) == (13, 11));
    assert!(!read_info(&plain.build()).unwrap().progressive);
}

#[test]
fn jpegs_of_every_kind() {
    let img = photo(48, 32);
    let cases: [(&str, Vec<u8>); 4] = [
        ("4:2:0", jpeg(&img, |e| e.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_2_0))),
        ("4:4:4", jpeg(&img, |e| e.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_4_4))),
        ("progressive", jpeg(&img, |e| e.set_progressive(true))),
        ("restarts", jpeg(&img, |e| e.set_restart_interval(2))),
    ];
    for (name, file) in &cases {
        let decoded = decode(file).unwrap_or_else(|e| panic!("{name}: {e}"));
        let err = mean_error(&decoded, &img);
        assert!(err < 3.0, "{name}: mean error {err}");
        assert!(decoded.pixels.iter().all(|p| p >> 24 == 255), "{name}");
    }
    assert!(read_info(&cases[2].1).unwrap().progressive);
    // Grayscale (one component), which the library converts through RGBA.
    let mut gray_file = Vec::new();
    let gray: Vec<u8> = (0..32 * 16).map(|i| (i % 32 * 8) as u8).collect();
    jpeg_encoder::Encoder::new(&mut gray_file, 95).encode(&gray, 32, 16, jpeg_encoder::ColorType::Luma).unwrap();
    let decoded = decode(&gray_file).unwrap();
    for (p, &g) in decoded.pixels.iter().zip(&gray) {
        let [b, gg, r, a] = p.to_le_bytes();
        assert!(r == gg && gg == b && a == 255 && r.abs_diff(g) <= 6, "{p:08x} for {g}");
    }
    // CMYK (Adobe, four components).
    let mut cmyk_file = Vec::new();
    let cmyk: Vec<u8> = (0..16 * 8).flat_map(|i| [0, (i * 2) as u8, 255 - (i * 2) as u8, 0]).collect();
    jpeg_encoder::Encoder::new(&mut cmyk_file, 95).encode(&cmyk, 16, 8, jpeg_encoder::ColorType::Cmyk).unwrap();
    let decoded = decode(&cmyk_file).unwrap();
    assert_eq!((decoded.width, decoded.height), (16, 8));
    for (i, p) in decoded.pixels.iter().enumerate() {
        // No cyan or black: red full, green 255 - magenta, blue 255 - yellow.
        let want = argb(255, 255, 255 - (i as u32 * 2), i as u32 * 2);
        assert!(mean_error(&Image::filled(1, 1, *p), &Image::filled(1, 1, want)) <= 4.0, "{i}: {p:08x} for {want:08x}");
    }
}

#[test]
fn jpegs_with_a_scan_per_component() {
    // jpeg-encoder writes a baseline JPEG with optimized Huffman tables as one scan per
    // component, whose data units are in each component's own raster order (T.81 A.2.2):
    // zune-jpeg 0.5.15 misplaced the luma rows of subsampled ones (see
    // implementation/ports/zune-jpeg).
    for (w, h) in [(48u32, 32u32), (100, 60), (333, 211)] {
        let img = photo(w, h);
        for sampling in [jpeg_encoder::SamplingFactor::R_4_2_0, jpeg_encoder::SamplingFactor::R_4_2_2] {
            let file = jpeg(&img, |e| {
                e.set_sampling_factor(sampling);
                e.set_optimized_huffman_tables(true);
            });
            let scans = file.windows(2).filter(|m| m == &[0xFF, 0xDA]).count();
            assert_eq!(scans, 3, "{w}x{h}");
            let err = mean_error(&decode(&file).unwrap(), &img);
            assert!(err < 3.0, "{w}x{h} {sampling:?}: mean error {err}");
        }
    }
}

#[test]
fn exif_orientation_turns_the_picture() {
    let stored = photo(40, 24);
    // EXIF orientation 6: the stored picture is shown turned 90 degrees clockwise.
    let mut tiff = b"II*\0\x08\0\0\0\x01\0".to_vec();
    tiff.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
    tiff.extend_from_slice(&[0, 0, 0, 0]);
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);
    let file = jpeg(&stored, |e| e.add_app_segment(1, app1).unwrap());
    let info = read_info(&file).unwrap();
    assert_eq!((info.orientation, info.width, info.height), (Orientation::Rotate90, 24, 40));
    let shown = decode(&file).unwrap();
    assert_eq!((shown.width, shown.height), (24, 40));
    assert!(mean_error(&shown, &crate::rotate90(&stored)) < 3.0);
    let as_stored = decode_with(&file, &DecodeOptions { apply_orientation: false, ..DecodeOptions::DEFAULT }).unwrap();
    assert_eq!((as_stored.width, as_stored.height), (40, 24));
}

#[test]
fn bmp_bit_depths_and_compressions() {
    let pal = [[0u8, 0, 255, 0], [0, 255, 0, 0], [255, 0, 0, 0], [9, 9, 9, 0]];
    let colour = |i: usize| argb(255, pal[i][2] as u32, pal[i][1] as u32, pal[i][0] as u32);
    // 24-bit, bottom-up (rows padded to 4 bytes).
    let px24 = [1, 2, 3, 4, 5, 6, 0, 0, 7, 8, 9, 10, 11, 12, 0, 0];
    let img = decode(&bmp(40, 2, 2, 24, 0, &[], &[], &px24)).unwrap();
    assert_eq!(img.pixels, [argb(255, 9, 8, 7), argb(255, 12, 11, 10), argb(255, 3, 2, 1), argb(255, 6, 5, 4)]);
    // 32-bit with an alpha mask, top-down.
    let px32 = [10, 20, 30, 40, 50, 60, 70, 80];
    let img = decode(&bmp(108, 2, -1, 32, 3, &[], &[0xFF0000, 0xFF00, 0xFF, 0xFF00_0000], &px32)).unwrap();
    assert_eq!(img.pixels, [argb(40, 30, 20, 10), argb(80, 70, 60, 50)]);
    // 8-bit palette.
    let px8 = [0, 1, 2, 3, 3, 2, 1, 0];
    let img = decode(&bmp(40, 4, -2, 8, 0, &pal, &[], &px8)).unwrap();
    assert_eq!(img.pixels, px8.map(|i| colour(i as usize)));
    // 4-bit palette with a core header.
    let px4 = [0x01, 0x23, 0, 0];
    let img = decode(&bmp(12, 4, 1, 4, 0, &pal, &[], &px4)).unwrap();
    assert_eq!(img.pixels, [0, 1, 2, 3].map(colour));
    // RLE8: a run of three 1s, three literal pixels (2, 3, 1, padded to a word), end of line;
    // end of bitmap.
    let rle8 = [3, 1, 0, 3, 2, 3, 1, 0, 0, 0, 0, 1];
    let img = decode(&bmp(40, 6, 1, 8, 1, &pal, &[], &rle8)).unwrap();
    assert_eq!(img.pixels, [1, 1, 1, 2, 3, 1].map(colour));
    // RLE4: four pixels alternating 1 and 2; end of bitmap.
    let rle4 = [4, 0x12, 0, 1];
    let img = decode(&bmp(40, 4, 1, 4, 2, &pal, &[], &rle4)).unwrap();
    assert_eq!(img.pixels, [1, 2, 1, 2].map(colour));
    // 16-bit 5-6-5: the extremes of each channel.
    let px16 = [0x00, 0xF8, 0xE0, 0x07, 0x1F, 0x00, 0x00, 0x00];
    let img = decode(&bmp(40, 4, 1, 16, 3, &[], &[0xF800, 0x07E0, 0x001F], &px16)).unwrap();
    assert_eq!(img.pixels, [argb(255, 255, 0, 0), argb(255, 0, 255, 0), argb(255, 0, 0, 255), argb(255, 0, 0, 0)]);
}

#[test]
fn qoi_round_trips() {
    let rgba = Image::from_fn(9, 7, |x, y| argb((x * 28) % 256, x * 20, y * 30, (x * y) % 256));
    let file = qoi::encode_to_vec(rgba.to_rgba8(), 9, 7).unwrap();
    assert_eq!(decode(&file).unwrap(), rgba);
    assert!(read_info(&file).unwrap().has_alpha);
    let rgb: Vec<u8> = (0..5 * 3 * 3).map(|i| (i * 17) as u8).collect();
    let file = qoi::encode_to_vec(&rgb, 5, 3).unwrap();
    let want: Vec<u32> = rgb.chunks(3).map(|p| argb(255, p[0] as u32, p[1] as u32, p[2] as u32)).collect();
    assert_eq!(decode(&file).unwrap().pixels, want);
    assert!(!read_info(&file).unwrap().has_alpha);
}

#[test]
fn limits_and_short_files() {
    let png = |w: u32, h: u32| {
        let samples = vec![0u16; (w * h) as usize];
        TestPng {
            width: w,
            height: h,
            color: 0,
            depth: 8,
            interlaced: false,
            samples: &samples,
            palette: &[],
            trns: None,
        }
        .build()
    };
    let small = DecodeOptions { max_dimension: 64, max_pixels: 1000, ..DecodeOptions::DEFAULT };
    assert_eq!(decode_with(&png(65, 2), &small), Err(ImageError::TooLarge { width: 65, height: 2 }));
    assert_eq!(decode_with(&png(40, 40), &small), Err(ImageError::TooLarge { width: 40, height: 40 }));
    assert!(decode_with(&png(30, 30), &small).is_ok());
    // A header claiming a picture far larger than the rest of the file could describe is refused
    // before anything is allocated for it.
    let mut huge = png(1, 1);
    huge[16..24].copy_from_slice(&[0, 0, 0x20, 0, 0, 0, 0x20, 0]);
    let fixed = crc32fast::hash(&huge[12..29]).to_be_bytes();
    huge[29..33].copy_from_slice(&fixed);
    assert_eq!(read_info(&huge).map(|i| (i.width, i.height)), Ok((8192, 8192)));
    assert_eq!(decode(&huge), Err(ImageError::Truncated));
    let mut qoi = qoi::encode_to_vec([1u8, 2, 3, 4], 1, 1).unwrap();
    qoi[4..12].copy_from_slice(&[0, 0, 0x10, 0, 0, 0, 0x10, 0]);
    assert_eq!(decode(&qoi), Err(ImageError::Truncated));
}

#[test]
fn lenient_decoding_skips_only_checksums() {
    let samples: Vec<u16> = (0..16 * 12).map(|i| (i % 256) as u16).collect();
    let file = TestPng {
        width: 16,
        height: 12,
        color: 0,
        depth: 8,
        interlaced: false,
        samples: &samples,
        palette: &[],
        trns: None,
    }
    .build();
    let img = decode(&file).unwrap();
    assert_eq!(decode_lenient(&file).unwrap(), (img.clone(), false));
    // A damaged IHDR CRC fails normal decoding but not lenient decoding, which reports it.
    let mut bad_crc = file.clone();
    bad_crc[29] ^= 0x5A;
    assert_eq!(decode(&bad_crc), Err(ImageError::ChecksumMismatch("PNG chunk CRC")));
    assert_eq!(decode_lenient(&bad_crc).unwrap(), (img, true));
    // Other errors are passed through.
    assert_eq!(decode_lenient(&file[..file.len() / 2]).unwrap_err(), ImageError::Truncated);
    assert_eq!(decode_lenient(b"not a picture").unwrap_err(), ImageError::UnknownFormat);
}

#[test]
fn scaling_keeps_colour_and_alpha_apart() {
    // Shrinking a uniform picture keeps its colour exactly.
    let flat = Image::filled(97, 61, argb(255, 200, 100, 50));
    for filter in [Filter::Nearest, Filter::Bilinear, Filter::Box, Filter::Lanczos3] {
        let small = crate::resize(&flat, 13, 9, filter);
        assert!(small.pixels.iter().all(|&p| p == argb(255, 200, 100, 50)), "{filter:?}");
    }
    // Transparent pixels lend no colour: half opaque red, half transparent green.
    let half = Image::from_fn(16, 2, |x, _| if x < 8 { argb(255, 255, 0, 0) } else { argb(0, 0, 255, 0) });
    let shrunk = crate::resize(&half, 2, 1, Filter::Box);
    assert_eq!(shrunk.pixels[0], argb(255, 255, 0, 0));
    assert_eq!(shrunk.pixels[1] >> 24, 0);
    let middle = crate::resize(&half, 1, 1, Filter::Box).pixels[0];
    let [_, g, r, a] = middle.to_le_bytes();
    assert!(a.abs_diff(128) <= 1 && r == 255 && g == 0, "{middle:08x}");
    // Thumbnails keep the aspect ratio and never enlarge.
    assert_eq!(crate::fit_size(4000, 3000, 256, 256), (256, 192));
    assert_eq!(crate::fit_size(100, 50, 256, 256), (100, 50));
    let t = crate::thumbnail(&photo(400, 100), 64, 64, Filter::Bilinear);
    assert_eq!((t.width, t.height), (64, 16));
    let c = crate::crop(&photo(10, 10), 8, 8, 5, 5);
    assert_eq!((c.width, c.height), (2, 2));
}

#[test]
fn covering_crops_the_centre() {
    // Left and right thirds red, the middle green: a square cover of the
    // wide picture is the middle, all green.
    let wide =
        Image::from_fn(90, 30, |x, _| if (30..60).contains(&x) { argb(255, 0, 255, 0) } else { argb(255, 255, 0, 0) });
    let square = crate::cover(&wide, 10, 10, Filter::Bilinear);
    assert_eq!((square.width, square.height), (10, 10));
    assert!(square.pixels.iter().all(|&p| p == argb(255, 0, 255, 0)));
    // A tall one is a narrower middle; a banner, the full width with the red.
    let tall = crate::cover(&wide, 10, 40, Filter::Bilinear);
    assert_eq!((tall.width, tall.height), (10, 40));
    assert!(tall.pixels.iter().all(|&p| p == argb(255, 0, 255, 0)));
    let banner = crate::cover(&wide, 120, 10, Filter::Bilinear);
    assert_eq!((banner.width, banner.height), (120, 10));
    assert!(banner.pixels.iter().any(|&p| p == argb(255, 255, 0, 0)));
    // Degenerate sizes give what resize gives, without dividing by zero.
    assert!(crate::cover(&wide, 0, 10, Filter::Bilinear).pixels.is_empty());
    assert_eq!(crate::cover(&Image::new(0, 0), 4, 4, Filter::Bilinear).pixels.len(), 16);
    let line = crate::cover(&photo(1000, 1), 3, 3, Filter::Bilinear);
    assert_eq!((line.width, line.height), (3, 3));
}

#[test]
fn turns_and_mirrors() {
    let img = Image::from_fn(3, 2, |x, y| y * 3 + x);
    assert_eq!(crate::rotate90(&img).pixels, [3, 0, 4, 1, 5, 2]);
    assert_eq!(crate::rotate270(&img).pixels, [2, 5, 1, 4, 0, 3]);
    assert_eq!(crate::rotate180(&img).pixels, [5, 4, 3, 2, 1, 0]);
    assert_eq!(crate::flip_horizontal(&img).pixels, [2, 1, 0, 5, 4, 3]);
    assert_eq!(crate::flip_vertical(&img).pixels, [3, 4, 5, 0, 1, 2]);
    for v in 1..=8 {
        let o = Orientation::from_exif(v).unwrap();
        let shown = crate::apply_orientation(&img, o);
        let swapped = (shown.width, shown.height) == (2, 3);
        assert_eq!(swapped, o.swaps_dimensions(), "{o:?}");
    }
    assert_eq!(crate::apply_orientation(&img, Orientation::Transpose).pixels, [0, 3, 1, 4, 2, 5]);
    assert_eq!(crate::apply_orientation(&img, Orientation::Transverse).pixels, [5, 2, 4, 1, 3, 0]);
}

/// Applies 1..=4 random mutations to `data`.
fn mutate(rng: &mut Rng, data: &[u8]) -> Vec<u8> {
    let mut d = data.to_vec();
    for _ in 0..1 + rng.below(4) {
        if d.is_empty() {
            d.push(rng.next() as u8);
            continue;
        }
        let pos = rng.below(d.len());
        match rng.below(9) {
            0 | 1 => d[pos] ^= 1 << rng.below(8),
            2 => d[pos] = rng.next() as u8,
            3 => d[pos] = [0x00, 0xFF, 0x7F, 0x80, 0x01, 0xFE, 0x10][rng.below(7)],
            4 => d.truncate(pos),
            5 => {
                let n = 1 + rng.below(16);
                let bytes: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                d.splice(pos..pos, bytes);
            }
            6 => {
                let end = (pos + 1 + rng.below(32)).min(d.len());
                d.drain(pos..end);
            }
            7 => {
                let end = (pos + 1 + rng.below(64)).min(d.len());
                let chunk = d[pos..end].to_vec();
                let at = rng.below(d.len());
                d.splice(at..at, chunk);
            }
            _ => {
                // Large integers where headers keep sizes and lengths.
                let v = [0xFFFF_FFFFu32, 0x7FFF_FFFF, 0x8000_0000, 0x0001_0000, 0xFFFF][rng.below(5)].to_be_bytes();
                for (i, b) in v.iter().enumerate() {
                    if let Some(x) = d.get_mut(pos + i) {
                        *x = *b;
                    }
                }
            }
        }
    }
    d
}

/// Small valid files covering the decoders' code paths.
fn corpus() -> Vec<(&'static str, Vec<u8>)> {
    let mut files = Vec::new();
    let rgba: Vec<u16> = (0..23 * 17 * 4).map(|i| (i * 11 % 256) as u16).collect();
    files.push((
        "png rgba",
        TestPng {
            width: 23,
            height: 17,
            color: 6,
            depth: 8,
            interlaced: false,
            samples: &rgba,
            palette: &[],
            trns: None,
        }
        .build(),
    ));
    let samples: Vec<u16> = (0..12 * 10).map(|i| (i * 7 % 13) as u16).collect();
    let palette: Vec<u8> = (0..13 * 3).map(|i| (i * 19) as u8).collect();
    files.push((
        "png palette interlaced",
        TestPng {
            width: 12,
            height: 10,
            color: 3,
            depth: 4,
            interlaced: true,
            samples: &samples,
            palette: &palette,
            trns: Some(&[0, 50, 100]),
        }
        .build(),
    ));
    let gray16: Vec<u16> = (0..11 * 9).map(|i| (i * 977 % 65536) as u16).collect();
    files.push((
        "png gray16 interlaced",
        TestPng {
            width: 11,
            height: 9,
            color: 0,
            depth: 16,
            interlaced: true,
            samples: &gray16,
            palette: &[],
            trns: Some(&[0, 0]),
        }
        .build(),
    ));
    let bits: Vec<u16> = (0..20 * 6).map(|i| (i % 3 == 0) as u16).collect();
    files.push((
        "png gray1",
        TestPng {
            width: 20,
            height: 6,
            color: 0,
            depth: 1,
            interlaced: false,
            samples: &bits,
            palette: &[],
            trns: None,
        }
        .build(),
    ));

    let img = photo(32, 24);
    files.push(("jpeg 420", jpeg(&img, |_| {})));
    files.push((
        "jpeg 444 restart",
        jpeg(&img, |e| {
            e.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_4_4);
            e.set_restart_interval(2);
        }),
    ));
    files.push((
        "jpeg 422 optimized",
        jpeg(&img, |e| {
            e.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_2_2);
            e.set_optimized_huffman_tables(true);
        }),
    ));
    files.push(("jpeg 420 scan per component", jpeg(&img, |e| e.set_optimized_huffman_tables(true))));
    files.push(("jpeg progressive", jpeg(&img, |e| e.set_progressive(true))));
    files.push((
        "jpeg progressive restart",
        jpeg(&img, |e| {
            e.set_progressive(true);
            e.set_restart_interval(3);
        }),
    ));
    let mut gray = Vec::new();
    let luma: Vec<u8> = (0..24 * 16).map(|i| (i * 7 % 256) as u8).collect();
    jpeg_encoder::Encoder::new(&mut gray, 80).encode(&luma, 24, 16, jpeg_encoder::ColorType::Luma).unwrap();
    files.push(("jpeg gray", gray));
    let mut cmyk = Vec::new();
    let planes: Vec<u8> = (0..16 * 8 * 4).map(|i| (i * 13 % 256) as u8).collect();
    jpeg_encoder::Encoder::new(&mut cmyk, 80).encode(&planes, 16, 8, jpeg_encoder::ColorType::Cmyk).unwrap();
    files.push(("jpeg cmyk", cmyk));
    let mut ycck = Vec::new();
    let mut e = jpeg_encoder::Encoder::new(&mut ycck, 80);
    e.set_progressive(true);
    e.encode(&planes, 16, 8, jpeg_encoder::ColorType::CmykAsYcck).unwrap();
    files.push(("jpeg ycck progressive", ycck));

    let pal = [[0u8, 0, 255, 0], [0, 255, 0, 0], [255, 0, 0, 0], [9, 9, 9, 0]];
    let px32: Vec<u8> = (0..23 * 17 * 4).map(|i| (i * 3) as u8).collect();
    files.push(("bmp 32 v4", bmp(108, 23, 17, 32, 3, &[], &[0xFF0000, 0xFF00, 0xFF, 0xFF00_0000], &px32)));
    let px8: Vec<u8> = (0..8 * 6).map(|i| (i % 4) as u8).collect();
    files.push(("bmp 8", bmp(40, 8, 6, 8, 0, &pal, &[], &px8)));
    let px4: Vec<u8> = (0..4 * 5).map(|i| (i * 0x13) as u8).collect();
    files.push(("bmp 4 core", bmp(12, 7, 5, 4, 0, &pal, &[], &px4)));
    files.push((
        "bmp rle8",
        bmp(40, 4, 3, 8, 1, &pal, &[], &[3, 1, 1, 2, 0, 0, 0, 2, 1, 0, 1, 3, 0, 0, 0, 4, 1, 2, 3, 1, 0, 1]),
    ));
    files.push(("bmp rle4", bmp(40, 5, 2, 4, 2, &pal, &[], &[5, 0x12, 0, 0, 0, 5, 0x12, 0x30, 0x20, 0, 0, 1])));
    let px16: Vec<u8> = (0..6 * 4 * 2).map(|i| (i * 37) as u8).collect();
    files.push(("bmp 565", bmp(40, 6, -4, 16, 3, &[], &[0xF800, 0x07E0, 0x001F], &px16)));
    let px24: Vec<u8> = (0..4 * 12).map(|i| (i * 5) as u8).collect();
    files.push(("bmp 24", bmp(40, 4, 4, 24, 0, &[], &[], &px24)));

    let qoi_rgba: Vec<u8> = (0..23 * 17 * 4).map(|i| (i * 11 / 7) as u8).collect();
    files.push(("qoi rgba", qoi::encode_to_vec(&qoi_rgba, 23, 17).unwrap()));
    let qoi_rgb: Vec<u8> = (0..20 * 12 * 3).map(|i| (i / 5) as u8).collect();
    files.push(("qoi rgb", qoi::encode_to_vec(&qoi_rgb, 20, 12).unwrap()));
    files
}

const LIMITS: DecodeOptions =
    DecodeOptions { max_dimension: 4096, max_pixels: 1 << 18, verify_checksums: true, apply_orientation: true };

/// Decodes `data` with every entry point; all must return without panicking. Returns whether
/// the strict and the checksum-ignoring decodes succeeded.
fn exercise(data: &[u8]) -> (bool, bool) {
    let _ = read_info(data);
    let lax = DecodeOptions { verify_checksums: false, ..LIMITS };
    let a = decode_with(data, &LIMITS);
    let b = decode_with(data, &lax);
    for img in [&a, &b].into_iter().flatten() {
        assert_eq!(img.pixels.len(), img.width as usize * img.height as usize);
        assert!(img.width as u64 * img.height as u64 <= LIMITS.max_pixels);
    }
    (a.is_ok(), b.is_ok())
}

#[test]
fn corpus_decodes() {
    for (name, data) in corpus() {
        let img = decode_with(&data, &LIMITS).unwrap_or_else(|e| panic!("{name}: {e}"));
        let info = read_info(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((img.width, img.height), (info.width, info.height), "{name}");
    }
}

fn run_mutations(seed: u64, iterations: usize) {
    let mut rng = Rng(seed);
    for (name, data) in corpus() {
        let (mut strict, mut lax) = (0, 0);
        for _ in 0..iterations {
            let m = mutate(&mut rng, &data);
            let (a, b) = exercise(&m);
            strict += a as usize;
            lax += b as usize;
        }
        println!("{name:<32} {iterations} mutations: {strict} decoded, {lax} without checksums");
    }
}

#[test]
fn mutations_never_panic() {
    run_mutations(0x9E37_79B9_7F4A_7C15, 4000);
}

/// A much longer run (about a million decodes); run manually.
#[test]
#[ignore]
fn mutations_never_panic_long() {
    for seed in 1..=10 {
        run_mutations(seed * 0x1234_5678_9ABC_DEF1, 5000);
    }
}

#[test]
fn every_truncation() {
    for (_, data) in corpus() {
        for len in 0..data.len() {
            exercise(&data[..len]);
        }
    }
}

#[test]
fn garbage_after_magic() {
    let mut rng = Rng(42);
    let magics: [&[u8]; 6] = [
        &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'],
        &[0xFF, 0xD8, 0xFF],
        b"BM\x00\x00\x00\x00\x00\x00\x00\x00\x36\x00\x00\x00\x28\x00\x00\x00",
        b"qoif",
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR",
        &[0xFF, 0xD8, 0xFF, 0xC2],
    ];
    for magic in magics {
        for _ in 0..2000 {
            let mut d = magic.to_vec();
            let n = rng.below(300);
            d.extend((0..n).map(|_| rng.next() as u8));
            exercise(&d);
        }
    }
}

#[cfg(feature = "encode")]
#[test]
fn encoders_round_trip() {
    use crate::{JpegOptions, Subsampling, encode_jpeg, encode_png, encode_qoi};
    let rgba = Image::from_fn(21, 13, |x, y| argb((x * 12) % 256, x * 12, y * 19, 77));
    assert_eq!(decode(&encode_png(&rgba).unwrap()).unwrap(), rgba);
    assert_eq!(decode(&encode_qoi(&rgba).unwrap()).unwrap(), rgba);
    let opaque = photo(40, 30);
    let png = encode_png(&opaque).unwrap();
    assert_eq!(decode(&png).unwrap(), opaque);
    assert!(!read_info(&png).unwrap().has_alpha);
    for options in [
        JpegOptions::default(),
        JpegOptions { subsampling: Subsampling::Yuv444, optimize_huffman: true, ..JpegOptions::default() },
        JpegOptions { progressive: true, quality: 75, ..JpegOptions::default() },
    ] {
        let file = encode_jpeg(&opaque, &options).unwrap();
        assert!(mean_error(&decode(&file).unwrap(), &opaque) < 4.0, "{options:?}");
        assert_eq!(read_info(&file).unwrap().progressive, options.progressive);
    }
    let mut tiff = b"MM\0*\0\0\0\x08\0\x01".to_vec();
    tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 8, 0, 0, 0, 0, 0, 0]);
    let file = encode_jpeg(&opaque, &JpegOptions { exif: Some(tiff), ..JpegOptions::default() }).unwrap();
    assert_eq!(read_info(&file).unwrap().orientation, Orientation::Rotate270);
}

/// Reads a PNG with the `png` crate, a decoder of its own, into Veda's pixels.
#[cfg(feature = "encode")]
fn png_crate_decode(file: &[u8]) -> Image {
    let mut reader = png::Decoder::new(std::io::Cursor::new(file)).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let frame = reader.next_frame(&mut buf).unwrap();
    assert_eq!(frame.bit_depth, png::BitDepth::Eight);
    let pixels = buf[..frame.buffer_size()]
        .chunks_exact(frame.color_type.samples())
        .map(|s| match *s {
            [r, g, b] => argb(255, r as u32, g as u32, b as u32),
            [r, g, b, a] => argb(a as u32, r as u32, g as u32, b as u32),
            _ => unreachable!("RGB or RGBA"),
        })
        .collect();
    Image::from_pixels(frame.width, frame.height, pixels).unwrap()
}

#[cfg(feature = "encode")]
#[test]
fn pngs_read_back_with_both_decoders_at_every_level() {
    use crate::{PngOptions, encode_png_with};
    let rgba = Image::from_fn(37, 23, |x, y| argb((x * 7 + y * 3) % 256, x * 6, y * 11, (x ^ y) * 9 % 256));
    let opaque = photo(64, 48);
    for level in 0..=10 {
        for image in [&rgba, &opaque] {
            let file = encode_png_with(image, &PngOptions { level }).unwrap();
            assert_eq!(&decode(&file).unwrap(), image, "level {level}");
            assert_eq!(&png_crate_decode(&file), image, "level {level}");
        }
    }
    // Compressed: a smooth opaque picture takes far less than its samples.
    let smooth = Image::from_fn(256, 128, |x, y| argb(255, x, y * 2, 128));
    let file = crate::encode_png(&smooth).unwrap();
    assert!(file.len() < 256 * 128 * 3 / 10, "{} bytes", file.len());
    // A stream longer than one chunk is split into several IDATs, each read.
    let noisy = noise(400, 300, 0xC0FFEE);
    let file = encode_png_with(&noisy, &PngOptions { level: 0 }).unwrap();
    let idats = file.windows(4).filter(|w| *w == b"IDAT").count();
    assert!(idats >= (400 * 300 * 4) / crate::IDAT_MAX, "{idats} IDAT chunks");
    assert_eq!(decode(&file).unwrap(), noisy);
    assert_eq!(png_crate_decode(&file), noisy);
}

/// An RGBA picture of pseudo-random pixels (incompressible).
#[cfg(feature = "encode")]
fn noise(width: u32, height: u32, seed: u64) -> Image {
    let mut rng = Rng(seed);
    Image::from_fn(width, height, |_, _| rng.next() as u32)
}

#[cfg(feature = "encode")]
#[test]
fn bmps_round_trip_with_and_without_alpha() {
    use crate::{encode, encode_bmp};
    // Odd widths pad 24-bit rows to four bytes.
    for width in [1, 2, 3, 5, 31] {
        let opaque = Image::from_fn(width, 7, |x, y| argb(255, x * 8, y * 30, 200));
        let file = encode_bmp(&opaque).unwrap();
        assert_eq!(file[28], 24, "24 bits a pixel");
        assert_eq!(file.len(), 54 + (width as usize * 3).div_ceil(4) * 4 * 7);
        assert_eq!(decode(&file).unwrap(), opaque, "width {width}");
        let rgba = Image::from_fn(width, 4, |x, y| argb(x * 40 % 256, 255, y * 60, x));
        let file = encode_bmp(&rgba).unwrap();
        assert_eq!(file[28], 32, "32 bits a pixel");
        assert_eq!(decode(&file).unwrap(), rgba, "width {width}");
        assert!(read_info(&file).unwrap().has_alpha);
    }
    // Every format through the one entry point, and nothing from nothing.
    let picture = photo(24, 16);
    for format in [Format::Png, Format::Bmp, Format::Qoi] {
        assert_eq!(decode(&encode(&picture, format).unwrap()).unwrap(), picture, "{format:?}");
        assert!(encode(&Image::new(0, 3), format).is_err());
    }
    assert!(mean_error(&decode(&encode(&picture, Format::Jpeg).unwrap()).unwrap(), &picture) < 6.0);
    let short = Image { width: 4, height: 4, pixels: vec![0; 3] };
    assert!(encode(&short, Format::Png).is_err() && encode(&short, Format::Bmp).is_err());
}

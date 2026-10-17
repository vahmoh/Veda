// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `assetgen` — generates the Veda wallpapers and sample pictures at build time.
//!
//! ```text
//! assetgen OUT_DIR [FILTER]
//! ```
//!
//! Writes `OUT_DIR/wallpapers/*` (installed as `/system/wallpapers`) and
//! `OUT_DIR/samples/Pictures/*` (copied to `~/Pictures` at boot), and lists them in
//! `OUT_DIR`'s manifest (see `gencommon::output`), removing what else is there. Everything is
//! procedural and deterministic. The optional `FILTER` renders only files whose name contains it
//! (handy while tuning), and leaves the manifest and the other files as they are.

mod noise;
mod paint;
mod photos;
mod wallpapers;

use std::path::{Path, PathBuf};
use std::time::Instant;

use gencommon::output::Output;
use image::{JpegOptions, Subsampling};
use paint::Buf;

/// How a generated image is stored.
#[derive(Clone, Copy)]
enum Store {
    /// Baseline JPEG at the given quality.
    Jpeg(u8, Subsampling),
    /// Progressive JPEG.
    ProgressiveJpeg(u8),
    /// JPEG stored rotated by 90 degrees counter-clockwise with EXIF orientation 6, so that
    /// viewers that honour EXIF show it upright.
    RotatedJpeg(u8),
    Png,
    Bmp,
    Qoi,
}

struct Item {
    dir: &'static str,
    name: &'static str,
    w: usize,
    h: usize,
    render: fn(usize, usize) -> Buf,
    store: Store,
}

const WALLPAPERS: &str = "wallpapers";
const PICTURES: &str = "samples/Pictures";

fn items() -> Vec<Item> {
    let wp = |name, render: fn(usize, usize) -> Buf| Item {
        dir: WALLPAPERS,
        name,
        w: 1920,
        h: 1200,
        render,
        store: Store::Jpeg(90, Subsampling::Yuv420),
    };
    let pic = |name, w, h, render, store| Item { dir: PICTURES, name, w, h, render, store };
    vec![
        // The default wallpaper is the first file name in byte order.
        wp("Aurora.jpg", wallpapers::aurora),
        wp("Dunes.jpg", wallpapers::dunes),
        wp("Nebula.jpg", wallpapers::nebula),
        wp("Pastel.jpg", wallpapers::pastel),
        wp("Ridges.jpg", wallpapers::ridges),
        pic("Alpine Lake.jpg", 2400, 1600, photos::alpine_lake, Store::Jpeg(90, Subsampling::Yuv420)),
        pic("City Lights.jpg", 1920, 1280, photos::city_lights, Store::ProgressiveJpeg(90)),
        pic("Lighthouse.jpg", 1200, 1600, photos::lighthouse, Store::RotatedJpeg(90)),
        pic("Soap Film.jpg", 1024, 1024, photos::soap_film, Store::Jpeg(92, Subsampling::Yuv444)),
        pic("Still Life.qoi", 960, 720, photos::still_life, Store::Qoi),
        pic("Glass Orb.png", 900, 900, photos::glass_orb, Store::Png),
        pic("Retro Sunset.bmp", 720, 450, photos::retro_sunset, Store::Bmp),
        pic("Misty Forest.jpg", 640, 480, photos::misty_forest, Store::Jpeg(90, Subsampling::Yuv420)),
    ]
}

fn encode(img: &image::Image, store: Store) -> Vec<u8> {
    let jpeg = |img: &image::Image, quality, subsampling, progressive, exif| {
        let opts = JpegOptions { quality, subsampling, optimize_huffman: true, progressive, exif };
        image::encode_jpeg(img, &opts).expect("JPEG encoding")
    };
    match store {
        Store::Jpeg(q, s) => jpeg(img, q, s, false, None),
        Store::ProgressiveJpeg(q) => jpeg(img, q, Subsampling::Yuv420, true, None),
        Store::RotatedJpeg(q) => {
            let stored = image::rotate270(img);
            jpeg(&stored, q, Subsampling::Yuv420, false, Some(paint::exif_orientation(6)))
        }
        Store::Png => image::encode_png(img).expect("PNG encoding"),
        Store::Bmp => paint::bmp24(img),
        Store::Qoi => image::encode_qoi(img).expect("QOI encoding"),
    }
}

/// The peak signal-to-noise ratio of `b` against `a` (same size), over the colour channels.
fn psnr(a: &image::Image, b: &image::Image) -> f64 {
    let mut sum = 0u64;
    for (&p, &q) in a.pixels.iter().zip(&b.pixels) {
        for shift in [0, 8, 16] {
            let d = ((p >> shift) & 255).abs_diff((q >> shift) & 255) as u64;
            sum += d * d;
        }
    }
    let mse = sum as f64 / (3 * a.pixels.len()) as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0 * 255.0 / mse).log10() }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(out) = args.next().map(PathBuf::from) else {
        eprintln!("usage: assetgen OUT_DIR [FILTER]");
        std::process::exit(2);
    };
    let filter = args.next();
    if let Err(e) = run(&out, filter.as_deref()) {
        eprintln!("assetgen: {e}");
        std::process::exit(1);
    }
}

/// Renders every item (or those `filter` names) into `out`.
fn run(out: &Path, filter: Option<&str>) -> Result<(), String> {
    let mut out = Output::new(out)?;
    let started = Instant::now();
    let mut total = 0usize;
    for (i, item) in items().into_iter().enumerate() {
        if filter.is_some_and(|f| !item.name.contains(f)) {
            continue;
        }
        let t = Instant::now();
        let img = (item.render)(item.w, item.h).to_image(0x5EED + i as u32);
        let bytes = encode(&img, item.store);
        // Every file must decode with the system's own decoder to what was drawn: exactly when
        // the format is lossless, closely when it is JPEG (quality 90 is well above 35 dB).
        let decoded = image::decode(&bytes).expect("generated image decodes");
        assert_eq!((decoded.width as usize, decoded.height as usize), (item.w, item.h), "{}", item.name);
        let quality = psnr(&img, &decoded);
        match item.store {
            Store::Png | Store::Qoi => assert!(decoded == img, "{} decodes differently", item.name),
            Store::Bmp => assert!(quality.is_infinite(), "{} decodes differently", item.name),
            _ => assert!(quality > 35.0, "{} decodes at {quality:.1} dB", item.name),
        }
        out.write(&format!("{}/{}", item.dir, item.name), &bytes)?;
        // While tuning, `ASSETGEN_PREVIEW=DIR` also writes every image as PNG (as displayed).
        if let Some(preview) = std::env::var_os("ASSETGEN_PREVIEW") {
            let preview = PathBuf::from(preview);
            std::fs::create_dir_all(&preview).expect("creating the preview directory");
            let png = image::encode_png(&decoded).expect("PNG encoding");
            std::fs::write(preview.join(format!("{}.png", item.name)), png).expect("writing the preview");
        }
        total += bytes.len();
        println!(
            "  {:<34} {:>4}x{:<4} {:>8} KiB  {:>5.0} ms",
            format!("{}/{}", item.dir, item.name),
            item.w,
            item.h,
            bytes.len() / 1024,
            t.elapsed().as_secs_f64() * 1000.0
        );
    }
    if filter.is_none() {
        out.finish()?;
    }
    println!("assetgen: {:.1} MiB in {:.1} s", total as f64 / (1024.0 * 1024.0), started.elapsed().as_secs_f64());
    Ok(())
}

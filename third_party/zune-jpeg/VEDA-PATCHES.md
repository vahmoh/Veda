# Veda changes to zune-jpeg 0.5.15

This directory holds zune-jpeg 0.5.15 as published on crates.io (MIT, Apache-2.0 or
Zlib license, see the `LICENSE-*` files), with the changes below. Everything else is
unmodified. Every changed spot is marked with a `VEDA PATCH` comment.

## Sequential JPEGs with a scan per component (`src/mcu.rs`)

A baseline or extended JPEG may code its components in separate scans, one each, rather
than interleaved in one (T.81, A.2.2). Such a scan holds its component's data units in
raster order: for a picture whose chroma is subsampled, the luma scan has twice the rows
of MCUs. Upstream decoded every scan for as many rows as the picture has MCU rows, and
placed each row of luma blocks two block rows apart, so these pictures came out as
stripes (libjpeg decodes them correctly). jpeg-encoder, for one, writes every baseline
JPEG with optimized Huffman tables this way.

* `single_component_scan_rows` gives the rows of data units of a scan's only component;
  a scan of one component is decoded for that many rows.
* In such a scan, a row of data units is 8 lines of the component's plane.

Pictures without subsampling, and scans of several components, decode as before.
`vimage`'s `jpegs_with_a_scan_per_component` test checks the fix.

## A crate-level `#![macro_use]` removed (`src/lib.rs`)

On a crate the attribute does nothing, and rustc warns that it will become an error.

## Known upstream bugs worked around elsewhere

These are left as they are: `vimage` does not use the affected paths.

* CMYK and YCCK pictures converted to RGBA (or BGRA) are written three bytes a pixel
  (`color_convert_cymk_to_rgb` and `color_convert_ycck_to_rgb` in `src/worker.rs` step
  by 3 whatever the output's components). `vimage` converts them to RGB, which is right.

## Manifest

The packaged `Cargo.toml` is kept as published.

## Running zune-jpeg's own tests

```text
cd third_party/zune-jpeg
cargo test --lib --target-dir ../../target/zune-jpeg
```

(The crate is excluded from the Veda workspace, so this resolves its dependencies
separately; the target directory keeps the build under the ignored `target/`.)

## Updating

Copy the new release's `src/`, `Cargo.toml`, `README.md`, `Changelog.md` and licenses over
this directory, re-apply the changes above where upstream has not fixed them (search for
`VEDA PATCH` in this version), and run `cargo test -p vimage`.

# Third-party code

Veda uses mature external crates where an established, widely used
implementation exists: protocols, cryptography, image codecs, math (see
"Dependencies" in `docs/CODING.md`). Most come from crates.io and are
pinned by `Cargo.lock`. This directory holds the ones that Veda has to
modify; each has a `VEDA-PATCHES.md` describing every change.

| Directory | Crate | License | Why it is vendored |
|-----------|-------|---------|--------------------|
| `smoltcp/` | smoltcp 0.14.0, the TCP/IP stack | 0BSD | cryptographically random TCP sequence numbers and protocol ids |
| `zune-jpeg/` | zune-jpeg 0.5.15, the JPEG decoder | MIT, Apache-2.0 or Zlib | sequential JPEGs with a scan per component and subsampled chroma decode correctly |

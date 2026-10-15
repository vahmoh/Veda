// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Images as their pieces: what goes where in an image (a boot sector, a
//! file's data, borrowed from where it is), the rest zeros. An image is
//! made without a copy of it in memory, and written with its zeros left
//! out (a sparse file, where the file system has them: a disk image's
//! free space takes no room). And reading images a piece at a time
//! ([`ReadAt`]): a file on a stick image is read without the image.

use std::borrow::Cow;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::util::{self, Result};

/// The pieces of an image of `len` bytes.
#[derive(Default)]
pub struct Pieces<'a> {
    len: u64,
    pieces: Vec<(u64, Cow<'a, [u8]>)>,
}

/// How much of a piece is looked at for zeros at a time when it is
/// written: a block of zeros is left out of the file.
const BLOCK: usize = 64 << 10;

impl<'a> Pieces<'a> {
    /// An image of `len` zeros.
    pub fn new(len: u64) -> Pieces<'a> {
        Pieces { len, pieces: Vec::new() }
    }

    /// The image's length.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Puts `data` at byte `at` (over what is there).
    pub fn put(&mut self, at: u64, data: impl Into<Cow<'a, [u8]>>) {
        let data = data.into();
        debug_assert!(at + data.len() as u64 <= self.len, "a piece beyond the image");
        if !data.is_empty() {
            self.pieces.push((at, data));
        }
    }

    /// Puts the image `other` at byte `at`.
    pub fn place(&mut self, at: u64, other: Pieces<'a>) {
        debug_assert!(at + other.len <= self.len, "an image beyond the image");
        self.pieces.extend(other.pieces.into_iter().map(|(start, data)| (at + start, data)));
    }

    /// The image's bytes, for tests.
    #[cfg(test)]
    pub fn to_vec(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.len as usize];
        for (at, data) in &self.pieces {
            out[*at as usize..*at as usize + data.len()].copy_from_slice(data);
        }
        out
    }

    /// Writes the image to `path`, whole or not at all (a stopped run
    /// leaves the previous one), its blocks of zeros left out; returns its
    /// length.
    pub fn write(&self, path: &Path) -> Result<u64> {
        let partial = path.with_extension(format!("part{}", std::process::id()));
        let written = self.write_to(&partial).and_then(|()| util::rename(&partial, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&partial);
        }
        written.map(|()| self.len)
    }

    fn write_to(&self, path: &Path) -> Result {
        let fail = |e: std::io::Error| format!("writing {}: {e}", path.display());
        let file = File::create(path).map_err(fail)?;
        file.set_len(self.len).map_err(fail)?;
        for (at, data) in &self.pieces {
            for (i, block) in data.chunks(BLOCK).enumerate() {
                if block.iter().any(|&b| b != 0) {
                    file.write_all_at(block, at + (i * BLOCK) as u64).map_err(fail)?;
                }
            }
        }
        Ok(())
    }
}

/// What an image can be read from, a piece at a time: a file, or bytes.
pub trait ReadAt {
    /// Fills `buf` with the bytes from `at` on.
    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result;
    /// The image's length.
    fn length(&self) -> Result<u64>;

    /// `len` bytes from `at` on.
    fn bytes(&self, at: u64, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        self.read_at(at, &mut buf)?;
        Ok(buf)
    }
}

impl ReadAt for [u8] {
    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result {
        let start = usize::try_from(at).map_err(|_| "beyond the image")?;
        let bytes = self.get(start..start.saturating_add(buf.len())).ok_or("beyond the image")?;
        buf.copy_from_slice(bytes);
        Ok(())
    }

    fn length(&self) -> Result<u64> {
        Ok(self.len() as u64)
    }
}

impl ReadAt for Vec<u8> {
    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result {
        self.as_slice().read_at(at, buf)
    }

    fn length(&self) -> Result<u64> {
        Ok(self.len() as u64)
    }
}

impl ReadAt for File {
    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result {
        self.read_exact_at(buf, at).map_err(|e| format!("reading {} bytes at {at}: {e}", buf.len()))
    }

    fn length(&self) -> Result<u64> {
        self.metadata().map(|m| m.len()).map_err(|e| e.to_string())
    }
}

/// A part of an image (a partition of a disk's).
pub struct Part<'r, R: ReadAt + ?Sized> {
    pub image: &'r R,
    pub start: u64,
    pub len: u64,
}

impl<R: ReadAt + ?Sized> ReadAt for Part<'_, R> {
    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result {
        if at.saturating_add(buf.len() as u64) > self.len {
            return Err("beyond the partition".into());
        }
        self.image.read_at(self.start + at, buf)
    }

    fn length(&self) -> Result<u64> {
        Ok(self.len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_their_pieces_and_zeros() {
        let file = [7u8; 3];
        let mut inner = Pieces::new(8);
        inner.put(1, &file[..]);
        let mut image = Pieces::new(16);
        image.put(0, vec![1u8, 2]);
        image.place(8, inner);
        image.put(15, &[9u8][..]);
        assert_eq!(image.to_vec(), [1, 2, 0, 0, 0, 0, 0, 0, 0, 7, 7, 7, 0, 0, 0, 9]);
        let path = std::env::temp_dir().join(format!("orchestration-pieces-{}.img", std::process::id()));
        assert_eq!(image.write(&path), Ok(16));
        assert_eq!(std::fs::read(&path).unwrap(), image.to_vec());
        let opened = File::open(&path).unwrap();
        assert_eq!(opened.bytes(9, 3), Ok(vec![7, 7, 7]));
        assert!(opened.bytes(15, 2).is_err());
        let part = Part { image: &opened, start: 8, len: 8 };
        assert_eq!(part.bytes(1, 2), Ok(vec![7, 7]));
        assert!(part.bytes(7, 2).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn zeros_take_no_room() {
        // 64 MiB, of which one block is written.
        let mut image = Pieces::new(64 << 20);
        let data = vec![0u8; 32 << 20];
        image.put(0, &data[..]);
        image.put(40 << 20, vec![1u8; 100]);
        let path = std::env::temp_dir().join(format!("orchestration-sparse-{}.img", std::process::id()));
        image.write(&path).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), 64 << 20);
        // Allocated blocks: the one written, whatever the file system rounds
        // it to (where the file system has holes at all).
        use std::os::unix::fs::MetadataExt;
        assert!(meta.blocks() * 512 < 8 << 20, "{} bytes allocated", meta.blocks() * 512);
        std::fs::remove_file(&path).unwrap();
    }
}

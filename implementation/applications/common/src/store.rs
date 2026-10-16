// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! An application's own small files (its settings, its state), for programs
//! on the standard library: read whole, up to a limit, and replaced in one
//! step, so that a save cut short leaves the last one whole.
//!
//! Pictures, music and other large files go through `vfiles`, which moves a
//! file in one piece of shared memory where the standard library's reads and
//! writes take a message for every few kilobytes.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::vec::Vec;

/// The file at `path`, if it holds at most `limit` bytes: a larger one is
/// none of the application's.
pub fn read(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::other(std::format!("{} holds more than {limit} bytes", path.display())));
    }
    Ok(bytes)
}

/// Replaces the file at `path` with `data` in one step: the data goes into
/// a file beside it, which is then renamed over it (its directory is made
/// first if need be).
pub fn write(path: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("partial");
    fs::write(&partial, data)?;
    fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(std::format!("apps-common-store-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_file_is_replaced_whole_and_read_within_its_limit() {
        let dir = scratch("replace");
        let path = dir.join("config").join("state");
        write(&path, b"first").unwrap();
        write(&path, b"second").unwrap();
        assert_eq!(read(&path, 6).unwrap(), b"second");
        // Nothing is left beside it.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        assert!(read(&path, 5).is_err(), "longer than its limit");
        assert!(read(&dir.join("none"), 5).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}

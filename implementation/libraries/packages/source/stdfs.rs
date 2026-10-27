// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The file system of the standard library behind transactions: the
//! package manager's in Veda, whose standard library reaches the VFS
//! (moving a file is the VFS's `replace`, in one step; syncing is the VFS's
//! `sync`, which saves `/home` to the home disk), and the build machine's,
//! for the tests and the tools.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;

use crate::transaction::{Failure, Files, FilesError, Kind, Sink};

/// The standard library's file system. `sync` brings the changes under
/// `anchor` (a directory that stays) to persistent storage, which on Veda
/// is every change.
#[derive(Debug, Clone)]
pub struct StdFiles {
    pub anchor: PathBuf,
}

fn error(e: std::io::Error) -> FilesError {
    let failure = match e.kind() {
        ErrorKind::NotFound => Failure::NotFound,
        ErrorKind::StorageFull | ErrorKind::QuotaExceeded | ErrorKind::FileTooLarge => Failure::NoSpace,
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => Failure::Denied,
        _ => Failure::Other,
    };
    FilesError { failure, detail: e.to_string() }
}

/// A file being written.
struct StdSink(std::io::BufWriter<fs::File>);

impl Sink for StdSink {
    fn write(&mut self, data: &[u8]) -> Result<(), FilesError> {
        self.0.write_all(data).map_err(error)
    }

    fn finish(self: Box<Self>) -> Result<(), FilesError> {
        let file = self.0.into_inner().map_err(|e| error(e.into_error()))?;
        drop(file);
        Ok(())
    }
}

impl Files for StdFiles {
    fn kind(&mut self, path: &str) -> Result<Option<Kind>, FilesError> {
        match fs::symlink_metadata(path) {
            Ok(m) if m.is_dir() => Ok(Some(Kind::Dir)),
            Ok(_) => Ok(Some(Kind::File)),
            // Nothing is at a path through a file, either.
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => Ok(None),
            Err(e) => Err(error(e)),
        }
    }

    fn read(&mut self, path: &str, limit: u64) -> Result<Vec<u8>, FilesError> {
        let file = fs::File::open(path).map_err(error)?;
        let mut data = Vec::new();
        file.take(limit.saturating_add(1)).read_to_end(&mut data).map_err(error)?;
        if data.len() as u64 > limit {
            return Err(FilesError { failure: Failure::Other, detail: String::from("larger than allowed") });
        }
        Ok(data)
    }

    fn create(&mut self, path: &str) -> Result<Box<dyn Sink>, FilesError> {
        let file = fs::File::create(path).map_err(error)?;
        Ok(Box::new(StdSink(std::io::BufWriter::with_capacity(64 * 1024, file))))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), FilesError> {
        fs::rename(from, to).map_err(error)
    }

    fn remove_file(&mut self, path: &str) -> Result<(), FilesError> {
        fs::remove_file(path).map_err(error)
    }

    fn make_dir(&mut self, path: &str) -> Result<(), FilesError> {
        fs::create_dir(path).map_err(error)
    }

    fn remove_dir(&mut self, path: &str) -> Result<(), FilesError> {
        fs::remove_dir(path).map_err(error)
    }

    fn list(&mut self, path: &str) -> Result<Vec<String>, FilesError> {
        let mut names = Vec::new();
        for entry in fs::read_dir(path).map_err(error)? {
            let entry = entry.map_err(error)?;
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    }

    fn sync(&mut self) -> Result<(), FilesError> {
        fs::File::open(&self.anchor).and_then(|f| f.sync_all()).map_err(error)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::Digest;
    use crate::archive::{Builder, Compression};
    use crate::database::Database;
    use crate::index::Offer;
    use crate::name::Name;
    use crate::requirement::Requirement;
    use crate::tests::manifest;
    use crate::transaction::{self, Change, Incoming, Layout, Recovery};

    #[test]
    fn changes_are_made_on_a_real_file_system() {
        let root = std::env::temp_dir().join(alloc::format!("packages-stdfs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (live, state) = (root.join("packages"), root.join("private"));
        fs::create_dir_all(&live).unwrap();
        fs::create_dir_all(&state).unwrap();
        let layout = Layout { live: live.display().to_string(), state: state.display().to_string() };
        let mut files = StdFiles { anchor: root.clone() };
        let offer = |m: &str, contents: &[(&str, &[u8])]| {
            let mut b = Builder::new(manifest(m));
            for (path, data) in contents {
                b.add(path, data.to_vec()).unwrap();
            }
            let bytes = b.build(Compression::Deflate).unwrap();
            let offer = Offer {
                manifest: manifest(m),
                file: String::from("x.vpkg"),
                size: bytes.len() as u64,
                sha256: Digest::of(&bytes),
                unpacked: contents.iter().map(|(_, d)| d.len() as u64).sum(),
            };
            (bytes, offer)
        };
        let (one, one_offer) = offer("hello 1.0", &[("bin/hello", b"one"), ("share/hello/x", b"x")]);
        let (two, two_offer) = offer("hello 2.0", &[("bin/hello", b"two"), ("share/hello/x/y", b"y")]);
        let world = vec![Requirement::parse("hello").unwrap()];
        let db = Database::new();
        let (one_path, two_path) =
            (root.join("one.vpkg").display().to_string(), root.join("two.vpkg").display().to_string());
        fs::write(&one_path, &one).unwrap();
        fs::write(&two_path, &two).unwrap();
        let install = |path, offer| Incoming { path, offer, repository: "main" };
        let change =
            Change { remove: vec![], install: vec![install(&one_path, &one_offer)], world: world.clone(), now: 0 };
        let db = transaction::apply(&mut files, &layout, &db, &change, &mut |_| {}).unwrap();
        assert_eq!(fs::read(live.join("bin/hello")).unwrap(), b"one");
        let change = Change {
            remove: vec![Name::parse("hello").unwrap()],
            install: vec![install(&two_path, &two_offer)],
            world,
            now: 0,
        };
        let db = transaction::apply(&mut files, &layout, &db, &change, &mut |_| {}).unwrap();
        assert_eq!(fs::read(live.join("bin/hello")).unwrap(), b"two");
        assert_eq!(fs::read(live.join("share/hello/x/y")).unwrap(), b"y");
        assert_eq!(transaction::load(&mut files, &layout).unwrap(), db);
        assert_eq!(transaction::verify(&mut files, &layout, &db, None).unwrap(), []);
        assert_eq!(transaction::recover(&mut files, &layout).unwrap(), Recovery::Nothing);
        let left: Vec<String> =
            fs::read_dir(&state).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, ["installed.json"]);
        assert_eq!(files.read(&live.join("bin/hello").display().to_string(), 2).unwrap_err().failure, Failure::Other);
        assert_eq!(files.kind(&root.join("nothing").display().to_string()).unwrap(), None);
        fs::remove_dir_all(&root).unwrap();
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What whoever publishes packages does, without the packages service:
//! makes a key pair (`pkg key`), a package of a directory (`pkg build`),
//! and a repository of a directory of packages (`pkg index`), its index
//! signed by their key.
//!
//! A package's directory holds its manifest (`manifest`, `KEY=VALUE`
//! lines, `packages::manifest`) and, in `files/`, what it installs, at the
//! paths it installs them at under `/home/packages`:
//!
//! ```text
//! hello/manifest
//! hello/files/bin/hello
//! hello/files/share/hello/README
//! ```

use std::path::{Path, PathBuf};

use packages::archive::{Archive, Builder, Compression};
use packages::index::{self, Contents, Offer};
use packages::keys::SecretKey;
use packages::manifest::Manifest;
use packages::{Digest, limits};
use zeroize::Zeroizing;

use crate::{bytes, unix_now};

/// Reads every file under `dir`, by path relative to `base`.
fn files_under(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| format!("{}: {e}", path.display()))?;
        if kind.is_dir() {
            files_under(base, &path, out)?;
        } else if kind.is_file() {
            let relative = path.strip_prefix(base).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            out.push((relative, path));
        } else {
            return Err(format!("{}: packages hold files and directories alone, not links", path.display()));
        }
        if out.len() > limits::FILES {
            return Err(format!("{}: a package has at most {} files", base.display(), limits::FILES));
        }
    }
    Ok(())
}

/// `pkg build DIR [-o FILE]`: makes a package of `dir`; returns where.
pub fn build(dir: &Path, out: Option<&Path>) -> Result<PathBuf, String> {
    let manifest_path = dir.join("manifest");
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let manifest = Manifest::from_text(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let base = dir.join("files");
    let mut files = Vec::new();
    if base.is_dir() {
        files_under(&base, &base, &mut files)?;
    }
    let mut builder = Builder::new(manifest.clone());
    for (relative, path) in &files {
        let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        builder.add(relative, data).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let package = builder.build(Compression::Deflate).map_err(|e| e.to_string())?;
    let name = format!("{}-{}.{}.vpkg", manifest.name, manifest.version, manifest.architecture).replace(':', "_");
    let out = out.map_or_else(|| PathBuf::from(&name), Path::to_path_buf);
    std::fs::write(&out, &package).map_err(|e| format!("{}: {e}", out.display()))?;
    let unpacked = Archive::open(&package).map(|a| a.unpacked()).unwrap_or(0);
    println!(
        "{}: {}, {} files, {} ({} unpacked)",
        out.display(),
        manifest.label(),
        files.len(),
        bytes(package.len() as u64),
        bytes(unpacked)
    );
    Ok(out)
}

/// The package files in `dir` (and the directories in it), by path
/// relative to it.
fn package_files(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            package_files(base, &path, out)?;
        } else if path.extension().is_some_and(|e| e == "vpkg") {
            let relative = path.strip_prefix(base).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            out.push((relative, path));
        }
    }
    Ok(())
}

/// What `pkg index` is told.
pub struct IndexOptions<'a> {
    pub dir: &'a Path,
    pub repository: &'a str,
    pub key: &'a Path,
    /// The index's serial (else the one before's, plus one).
    pub serial: Option<u64>,
    /// Days until it expires (none: never).
    pub expires: Option<u64>,
}

/// `pkg index DIR --name NAME --key SECRET`: signs an index of the packages
/// in `dir`.
pub fn index(o: &IndexOptions) -> Result<(), String> {
    let text = Zeroizing::new(std::fs::read_to_string(o.key).map_err(|e| format!("{}: {e}", o.key.display()))?);
    let key = SecretKey::parse(&text).map_err(|e| format!("{}: {e}", o.key.display()))?;
    let mut files = Vec::new();
    package_files(o.dir, o.dir, &mut files)?;
    files.sort();
    let mut packages = Vec::new();
    for (relative, path) in files {
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let archive = Archive::open(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        packages.push(Offer {
            manifest: archive.manifest.clone(),
            file: relative,
            size: bytes.len() as u64,
            sha256: Digest::of(&bytes),
            unpacked: archive.unpacked(),
        });
    }
    // The serial of the index there, if it is this key's, plus one.
    let path = o.dir.join(index::FILE);
    let previous = std::fs::read(&path).ok().and_then(|bytes| {
        let keys = [key.public()];
        let policy =
            index::Policy { keys: &keys, repository: o.repository, now: 0, must_expire: false, previous: None };
        index::verify(&bytes, &policy).ok().map(|v| v.contents.serial)
    });
    let serial = o.serial.unwrap_or_else(|| previous.map_or(1, |s| s + 1));
    let generated = unix_now();
    let count = packages.len();
    let contents = Contents {
        repository: o.repository.to_string(),
        serial,
        generated,
        expires: o.expires.map(|days| generated + days * 86_400),
        packages,
    };
    let expires = contents.expires;
    let signed = index::sign(contents, &[&key], true).map_err(|e| e.to_string())?;
    let new = o.dir.join(format!("{}.new", index::FILE));
    std::fs::write(&new, &signed)
        .and_then(|()| std::fs::rename(&new, &path))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    println!(
        "{}: {}, serial {serial}, signed by {}, {}",
        path.display(),
        packages::count(count as u64, "package", "packages"),
        key.public().fingerprint(),
        expires.map_or(String::from("never expires"), |at| format!("expires on {}", packages::date(at)))
    );
    Ok(())
}

/// `pkg key FILE`: makes a key pair, the secret key in `file` and the
/// public one in `FILE.pub`.
pub fn key(file: &Path) -> Result<(), String> {
    let public_path = PathBuf::from(format!("{}.pub", file.display()));
    for path in [file, public_path.as_path()] {
        if path.exists() {
            return Err(format!("{} is there already: no key is written over", path.display()));
        }
    }
    let mut seed = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *seed).map_err(|e| format!("no random numbers for a key: {e}"))?;
    let key = SecretKey::from_seed(&seed);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    use std::io::Write;
    options
        .open(file)
        .and_then(|mut f| f.write_all(key.to_text().as_bytes()))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    std::fs::write(&public_path, format!("{}\n", key.public()))
        .map_err(|e| format!("{}: {e}", public_path.display()))?;
    println!("{}: the secret key (keep it so: whoever has it signs what systems trusting it install)", file.display());
    println!("{}: the public key, {}", public_path.display(), key.public());
    Ok(())
}

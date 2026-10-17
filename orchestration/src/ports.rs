// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The ports (`implementation/ports/`): the releases of other projects that
//! Veda builds with changes of its own, each pinned by its `port.toml` (see
//! `implementation/ports/README.md`).
//!
//! What builds them fetches their releases here, each build into a
//! `downloads/` of its own: the toolchain, with the media libraries and
//! Rust (`toolchain`), the driver VM's Linux (`linux`), and the crates
//! (`crates`).

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::util::{self, Result};
use crate::{arch, crates, linux, toolchain};

/// The ports' directory.
pub fn dir() -> PathBuf {
    util::workspace_root().join("implementation").join("ports")
}

/// The ports' settings for the architecture built for.
pub fn settings() -> PathBuf {
    dir().join("architectures").join(arch::NAME).join("settings.sh")
}

/// The SHA-256 of what builds the ports `names` (every file of their
/// directories: the release each pins, Veda's patches, their scripts and
/// settings) and the files `extra`: a build made from the ports as they are
/// noted it, so that a later change of theirs is seen.
pub fn fingerprint(names: &[&str], extra: &[PathBuf]) -> Result<String> {
    let root = dir();
    let mut files = extra.to_vec();
    for name in names {
        files.extend(util::files_under(&root.join(name))?);
    }
    files.sort();
    let mut hash = Sha256::new();
    for file in &files {
        let data = util::read(file)?;
        let name = file.strip_prefix(&root).unwrap_or(file).to_string_lossy();
        hash.update(format!("{name}\0{}\0", data.len()));
        hash.update(&data);
    }
    Ok(util::hex(&hash.finalize()))
}

/// A source package of `implementation/ports/` (its `port.toml`).
pub struct Port {
    pub name: String,
    pub version: String,
    url: String,
    sha256: String,
}

impl Port {
    /// The port in `implementation/ports/NAME`.
    pub fn named(name: &str) -> Result<Port> {
        Port::load(&dir().join(name))
    }

    fn load(dir: &Path) -> Result<Port> {
        let file = dir.join("port.toml");
        let text = std::fs::read_to_string(&file).map_err(|e| format!("reading {}: {e}", file.display()))?;
        let value = |key: &str| -> Result<String> {
            text.lines()
                .filter_map(|l| l.split_once('='))
                .find(|(k, _)| k.trim() == key)
                .map(|(_, v)| v.trim().trim_matches('"').to_string())
                .ok_or_else(|| format!("{}: no {key}", file.display()))
        };
        Ok(Port { name: value("name")?, version: value("version")?, url: value("url")?, sha256: value("sha256")? })
    }

    /// Where its archive is downloaded to, in `downloads`.
    pub fn archive(&self, downloads: &Path) -> PathBuf {
        downloads.join(file_name(&self.url))
    }
}

/// The name of the file at `url`.
fn file_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

/// The SHA-256 of a file, in hexadecimal: of a downloaded archive, as its
/// note beside it (`NAME.sha256`: its size, its modification time and its
/// SHA-256) remembers it while the file is as it was then, which spares
/// reading the toolchain's half a gigabyte of archives at every build.
pub fn sha256_of(path: &Path) -> Result<String> {
    let note = path.with_file_name(format!("{}.sha256", path.file_name().unwrap_or_default().to_string_lossy()));
    let stamp = |m: std::fs::Metadata| -> Option<String> {
        let since = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(format!("{} {}", m.len(), since.as_nanos()))
    };
    let now = std::fs::metadata(path).ok().and_then(stamp);
    if let (Some(now), Ok(kept)) = (&now, std::fs::read_to_string(&note))
        && let Some((was, sha256)) = kept.trim().rsplit_once(' ')
        && was == now
    {
        return Ok(sha256.to_string());
    }
    let sha256 = util::sha256_file(path)?;
    if let Some(now) = now {
        // Only a cache: a note that cannot be written is read again.
        let _ = util::write_atomically(&note, format!("{now} {sha256}\n"));
    }
    Ok(sha256)
}

/// curl, fetching `url` into `to`: failing on HTTP errors, trying three
/// times, and giving up on a connection that does not open in 30 s or
/// moves less than 1 KiB a second for a minute.
pub fn curl(url: &str, to: &Path) -> Command {
    let mut cmd = Command::new("curl");
    cmd.args(["-fL", "--retry", "3", "--silent", "--show-error"])
        .args(["--connect-timeout", "30", "--speed-limit", "1024", "--speed-time", "60", "-o"])
        .arg(to)
        .arg(url);
    cmd
}

/// Downloads a port's source archive into `downloads` unless it is there,
/// and checks it against the SHA-256 the port pins. An archive another
/// build has (the toolchain's, the driver VM's Linux's or the crates') is
/// copied.
pub fn fetch(port: &Port, downloads: &Path) -> Result {
    download(&port.url, &port.sha256, &format!("implementation/ports/{}/port.toml", port.name), downloads).map(|_| ())
}

/// Downloads the archive at `url` into `downloads` unless it is there, and
/// checks it against `sha256`, which `pinned_by` pins; returns its path. An
/// archive another build has (the toolchain's, the driver VM's Linux's or the
/// crates') is copied.
pub fn download(url: &str, sha256: &str, pinned_by: &str, downloads: &Path) -> Result<PathBuf> {
    let archive = downloads.join(file_name(url));
    if archive.is_file() && sha256_of(&archive)? == sha256 {
        return Ok(archive);
    }
    for other in [toolchain::root(), linux::root(), crates::root()] {
        let other = other.join("downloads");
        let copy = other.join(file_name(url));
        if other != downloads && copy.is_file() && sha256_of(&copy)? == sha256 {
            util::create_dir_all(downloads)?;
            let partial = archive.with_extension("part");
            util::copy(&copy, &partial)?;
            util::rename(&partial, &archive)?;
            return Ok(archive);
        }
    }
    util::status("Downloading", url);
    util::create_dir_all(downloads)?;
    let partial = archive.with_extension("part");
    util::run(&mut curl(url, &partial))?;
    let got = util::sha256_file(&partial)?;
    if got != sha256 {
        util::remove_file(&partial)?;
        return Err(format!("{url}: SHA-256 {got}, expected {sha256} ({pinned_by})"));
    }
    util::rename(&partial, &archive)?;
    Ok(archive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_archives_are_not_read_again() {
        let dir = std::env::temp_dir().join(format!("orchestration-ports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        util::create_dir_all(&dir).unwrap();
        let archive = dir.join("a-1.0.tar.xz");
        util::write(&archive, b"first").unwrap();
        let first = sha256_of(&archive).unwrap();
        assert_eq!(first, util::hex(&Sha256::digest(b"first")));
        assert!(dir.join("a-1.0.tar.xz.sha256").is_file());
        // The note answers while the file is as it was: a note that lies is
        // believed (that is what it saves) ...
        let note = dir.join("a-1.0.tar.xz.sha256");
        let kept = std::fs::read_to_string(&note).unwrap();
        util::write(&note, kept.replace(&first, "cafe")).unwrap();
        assert_eq!(sha256_of(&archive).unwrap(), "cafe");
        // ... until the file changes.
        std::thread::sleep(std::time::Duration::from_millis(10));
        util::write(&archive, b"second").unwrap();
        assert_eq!(sha256_of(&archive).unwrap(), util::hex(&Sha256::digest(b"second")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn downloads_give_up_on_a_stalled_connection() {
        let cmd = curl("https://example.org/a.tar.xz", Path::new("/tmp/a.part"));
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        for (flag, value) in [("--connect-timeout", "30"), ("--speed-limit", "1024"), ("--speed-time", "60")] {
            assert!(args.windows(2).any(|w| w == [flag, value]), "{flag}: {args:?}");
        }
        assert_eq!(args.last().map(String::as_str), Some("https://example.org/a.tar.xz"));
    }

    #[test]
    fn fingerprints_change_with_the_ports() {
        let print = fingerprint(&["zlib"], &[dir().join("common.sh")]).unwrap();
        assert_eq!(print.len(), 64);
        assert_eq!(fingerprint(&["zlib"], &[dir().join("common.sh")]).unwrap(), print);
        assert_ne!(fingerprint(&["zlib", "musl"], &[dir().join("common.sh")]).unwrap(), print);
        assert!(fingerprint(&["zlib"], &[dir().join("no-such-file")]).is_err());
    }
}

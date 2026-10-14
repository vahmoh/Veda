// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The ports (`ports/`): the releases of other projects that Veda builds
//! with changes of its own, each pinned by its `port.toml` (see
//! `ports/README.md`).
//!
//! What builds them fetches their releases here, each build into a
//! `downloads/` of its own: the toolchain, with the media libraries and
//! Rust (`toolchain`), the driver VM's Linux (`linux`), and the crates
//! (`crates`).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::util::{self, Result};
use crate::{crates, linux, toolchain};

/// A source package of `ports/` (its `port.toml`).
pub struct Port {
    pub name: String,
    pub version: String,
    url: String,
    sha256: String,
}

impl Port {
    /// The port in `ports/NAME`.
    pub fn named(name: &str) -> Result<Port> {
        Port::load(&util::workspace_root().join("ports").join(name))
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

/// The SHA-256 of a file, in hexadecimal.
pub fn sha256_of(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let data = util::read(path)?;
    Ok(Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect())
}

/// Downloads a port's source archive into `downloads` unless it is there,
/// and checks it against the SHA-256 the port pins. An archive another
/// build has (the toolchain's, the driver VM's Linux's or the crates') is
/// copied.
pub fn fetch(port: &Port, downloads: &Path) -> Result {
    download(&port.url, &port.sha256, &format!("ports/{}/port.toml", port.name), downloads).map(|_| ())
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
            std::fs::create_dir_all(downloads).map_err(|e| format!("{e}"))?;
            std::fs::copy(&copy, &archive).map_err(|e| format!("copying {}: {e}", copy.display()))?;
            return Ok(archive);
        }
    }
    util::status("Downloading", url);
    std::fs::create_dir_all(downloads).map_err(|e| format!("{e}"))?;
    let partial = archive.with_extension("part");
    util::run(
        Command::new("curl").args(["-fL", "--retry", "3", "--silent", "--show-error", "-o"]).arg(&partial).arg(url),
    )?;
    let got = sha256_of(&partial)?;
    if got != sha256 {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("{url}: SHA-256 {got}, expected {sha256} ({pinned_by})"));
    }
    std::fs::rename(&partial, &archive).map_err(|e| format!("{e}"))?;
    Ok(archive)
}

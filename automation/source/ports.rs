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
use crate::{architecture, crates, linux, python, toolchain, web};

/// The ports' directory.
pub fn dir() -> PathBuf {
    util::workspace_root().join("implementation").join("ports")
}

/// The ports' settings for the architecture built for.
pub fn settings() -> PathBuf {
    dir().join("architectures").join(architecture::NAME).join("settings.sh")
}

/// What every build of libraries for Veda (the web engine's, Python's)
/// runs besides its own script: the helpers the build scripts share
/// (`common.sh`), how libraries are built for Veda (`libraries.sh`) and
/// CMake's description of Veda (`cmake/`).
pub fn library_build_files() -> Result<Vec<PathBuf>> {
    let dir = dir();
    let mut files = vec![dir.join("common.sh"), dir.join("libraries.sh")];
    files.extend(util::files_under(&dir.join("cmake"))?);
    Ok(files)
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

/// A source package of `implementation/ports/` (its `port.toml`): one
/// archive, or a set of files (fonts released one at a time).
pub struct Port {
    pub name: String,
    pub version: String,
    /// Its licence, as SPDX writes it (`license` in `port.toml`).
    pub licence: String,
    /// The archive's address; a set's, where its files' paths start.
    url: String,
    sha256: String,
    /// The name its archive is kept under (`archive` in `port.toml`), where
    /// the address's own does not tell it from others' (a tag's archive on
    /// GitHub, `v1.2.0.tar.gz`).
    file: String,
    /// A set's files: each one's path under `url`, and its SHA-256 (the
    /// list `files` in `port.toml` names, beside it, as `sha256sum` writes
    /// one: `HASH  PATH` a line).
    set: Vec<(String, String)>,
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
        let url = value("url")?;
        let file = value("archive").unwrap_or_else(|_| file_name(&url).to_string());
        let (set, sha256) = match value("files") {
            Ok(list) => (parse_set(&dir.join(list))?, String::new()),
            Err(_) => (Vec::new(), value("sha256")?),
        };
        Ok(Port {
            name: value("name")?,
            version: value("version")?,
            licence: value("license")?,
            url,
            sha256,
            file,
            set,
        })
    }

    /// What the image says of where the port's source is (`SOURCE`, beside
    /// its licence texts in `/system/licences`): its release, and Veda's
    /// patches of it, which Veda's source has with the scripts that build
    /// it. Of what a port's licence asks of whoever passes its binaries on
    /// (the GPL's and the LGPL's offer of the source), this is the part the
    /// image can carry.
    pub fn source_note(&self) -> String {
        let release = if self.set.is_empty() {
            format!("  {}\n  SHA-256 {}", self.url, self.sha256)
        } else {
            format!("  {} files under {},\n  each pinned by its SHA-256", self.set.len(), self.url)
        };
        let dir = dir().join(&self.name);
        let patches: Vec<String> =
            ["veda.patch".to_string(), format!("architectures/{}/veda.patch", architecture::NAME)]
                .into_iter()
                .filter(|patch| dir.join(patch).is_file())
                .map(|patch| format!("implementation/ports/{}/{patch}", self.name))
                .collect();
        let changes = if patches.is_empty() {
            "as released".to_string()
        } else {
            format!("with Veda's changes ({})", patches.join(", "))
        };
        format!(
            "{} {} ({}) is its release\n\n{release}\n\n{changes}, built by the scripts and settings in \
             implementation/ports/ of Veda's source: {}\n",
            self.name,
            self.version,
            self.licence,
            env!("CARGO_PKG_REPOSITORY")
        )
    }

    /// Where its archive is downloaded to, in `downloads`.
    pub fn archive(&self, downloads: &Path) -> PathBuf {
        downloads.join(&self.file)
    }

    /// Where its files are downloaded to, in `downloads`: its archive, or
    /// a set's files (in a directory of the port's name).
    pub fn files(&self, downloads: &Path) -> Vec<PathBuf> {
        if self.set.is_empty() {
            return vec![self.archive(downloads)];
        }
        self.set.iter().map(|(path, _)| downloads.join(self.set_file(path))).collect()
    }

    /// Where a set's file at `path` is kept, under the downloads.
    fn set_file(&self, path: &str) -> String {
        format!("{}/{}", self.name, file_name(path))
    }
}

/// A set's list of files (`HASH  PATH` a line, as `sha256sum` writes it;
/// blank lines and `#` comments aside).
fn parse_set(list: &Path) -> Result<Vec<(String, String)>> {
    let text = std::fs::read_to_string(list).map_err(|e| format!("reading {}: {e}", list.display()))?;
    let mut set = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.split_once(char::is_whitespace) {
            Some((hash, path)) if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
                set.push((path.trim().to_string(), hash.to_ascii_lowercase()));
            }
            _ => return Err(format!("{} line {}: not `HASH  PATH`", list.display(), n + 1)),
        }
    }
    if set.is_empty() {
        return Err(format!("{}: no files", list.display()));
    }
    Ok(set)
}

/// A path as it goes into an address: the characters curl would take for
/// patterns, or that an address cannot hold, escaped.
fn url_path(path: &str) -> String {
    path.replace('%', "%25").replace(' ', "%20").replace('[', "%5B").replace(']', "%5D")
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
    let pinned_by = format!("implementation/ports/{}/port.toml", port.name);
    if port.set.is_empty() {
        return download_as(&port.url, &port.file, &port.sha256, &pinned_by, downloads).map(|_| ());
    }
    for (path, sha256) in &port.set {
        let url = format!("{}{}", port.url, url_path(path));
        download_as(&url, &port.set_file(path), sha256, &pinned_by, downloads)?;
    }
    Ok(())
}

/// Downloads the archive at `url` into `downloads` unless it is there, and
/// checks it against `sha256`, which `pinned_by` pins; returns its path. An
/// archive another build has (the toolchain's, the driver VM's Linux's, the
/// crates', the web engine's or Python's) is copied.
pub fn download(url: &str, sha256: &str, pinned_by: &str, downloads: &Path) -> Result<PathBuf> {
    download_as(url, file_name(url), sha256, pinned_by, downloads)
}

/// [`download`], keeping the archive as `file` in `downloads`.
fn download_as(url: &str, file: &str, sha256: &str, pinned_by: &str, downloads: &Path) -> Result<PathBuf> {
    let archive = downloads.join(file);
    if archive.is_file() && sha256_of(&archive)? == sha256 {
        return Ok(archive);
    }
    for other in [toolchain::root(), linux::root(), crates::root(), web::root(), python::root()] {
        let other = other.join("downloads");
        let copy = other.join(file);
        if other != downloads && copy.is_file() && sha256_of(&copy)? == sha256 {
            util::create_dir_all(archive.parent().unwrap_or(downloads))?;
            let partial = archive.with_extension("part");
            util::copy(&copy, &partial)?;
            util::rename(&partial, &archive)?;
            return Ok(archive);
        }
    }
    util::status("Downloading", url);
    util::create_dir_all(archive.parent().unwrap_or(downloads))?;
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
        let dir = std::env::temp_dir().join(format!("automation-ports-{}", std::process::id()));
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
    fn sets_list_their_files_by_hash_and_path() {
        let dir = std::env::temp_dir().join(format!("automation-sets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        util::create_dir_all(&dir).unwrap();
        let hash = "ab".repeat(32);
        let list = format!("# The fonts\n{hash}  fonts/A/A[wdth,wght].ttf\n\n{}  B.ttf\n", "CD".repeat(32));
        util::write(&dir.join("files.sha256"), list).unwrap();
        util::write(
            &dir.join("port.toml"),
            "name = \"set\"\nversion = \"1\"\nurl = \"https://example.org/r/\"\nfiles = \"files.sha256\"\nlicense = \"OFL-1.1\"\n",
        )
        .unwrap();
        let port = Port::load(&dir).unwrap();
        assert_eq!(port.set, [("fonts/A/A[wdth,wght].ttf".to_string(), hash), ("B.ttf".into(), "cd".repeat(32))]);
        let downloads = Path::new("/d");
        assert_eq!(port.files(downloads), [Path::new("/d/set/A[wdth,wght].ttf"), Path::new("/d/set/B.ttf")]);
        assert_eq!(url_path("fonts/A/A[wdth,wght] 1%.ttf"), "fonts/A/A%5Bwdth,wght%5D%201%25.ttf");
        // A list that is not one, or empty.
        util::write(&dir.join("files.sha256"), "cafe  B.ttf\n").unwrap();
        assert!(Port::load(&dir).is_err());
        util::write(&dir.join("files.sha256"), "# nothing\n").unwrap();
        assert!(Port::load(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn source_notes_name_the_release_and_the_patches() {
        let musl = Port::named("musl").unwrap();
        let note = musl.source_note();
        assert!(note.starts_with(&format!("musl {} (MIT) is its release", musl.version)), "{note}");
        assert!(note.contains(&musl.url) && note.contains(&musl.sha256), "{note}");
        assert!(note.contains("implementation/ports/musl/veda.patch"), "{note}");
        assert!(note.contains(env!("CARGO_PKG_REPOSITORY")), "{note}");
        // A port without patches is its release as it is.
        assert!(Port::named("dav1d").unwrap().source_note().contains("\nas released, built by"));
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
    fn stages_are_fresh_from_another_checkout() {
        // Two checkouts of the same port, at different paths, building into
        // one directory (a worktree's build with the main checkout's
        // VEDA_TOOLCHAIN, say): the second finds the first's stages fresh,
        // until what one of the files holds changes.
        let dir = std::env::temp_dir().join(format!("automation-stages-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let checkouts = [dir.join("one"), dir.join("elsewhere").join("two")];
        let architecture_patch =
            |ports: &Path| ports.join("p").join("architectures").join(architecture::NAME).join("veda.patch");
        for ports in &checkouts {
            util::create_dir_all(architecture_patch(ports).parent().unwrap()).unwrap();
            util::copy(&super::dir().join("common.sh"), &ports.join("common.sh")).unwrap();
            util::write(&ports.join("p").join("port.toml"), "name = \"p\"\nversion = \"1\"\n").unwrap();
            util::write(&ports.join("p").join("veda.patch"), "Veda's patch\n").unwrap();
            util::write(&architecture_patch(ports), "the architecture's patch\n").unwrap();
            util::write(&ports.join("p").join("settings"), "a setting\n").unwrap();
        }
        let stamps = dir.join("stamps");
        util::create_dir_all(&stamps).unwrap();
        // The port's source as Veda patches it, then its build, which
        // depends on a file and a variable too: the stages that run.
        let ran = dir.join("ran");
        let build = |ports: &Path| {
            let script = r#"source "$PORTS/common.sh"
                stage_src_p() { echo src_p >> "$RAN"; }
                stage src_p +p
                stage_p() { echo p >> "$RAN"; }
                stage p src_p "$PORTS/p/settings" @ROOT"#;
            let out = Command::new("bash")
                .args(["-euo", "pipefail", "-c", script])
                .env("PORTS", ports)
                .env("VEDA_ARCH", architecture::NAME)
                .env("STAMPS", &stamps)
                .env("ROOT", &dir)
                .env("RAN", &ran)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let stages = std::fs::read_to_string(&ran).unwrap_or_default();
            let _ = std::fs::remove_file(&ran);
            stages.split_whitespace().collect::<Vec<_>>().join(" ")
        };
        assert_eq!(build(&checkouts[0]), "src_p p");
        assert_eq!(build(&checkouts[1]), "");
        util::write(&architecture_patch(&checkouts[1]), "another patch\n").unwrap();
        assert_eq!(build(&checkouts[1]), "src_p p");
        util::write(&checkouts[1].join("p").join("settings"), "another setting\n").unwrap();
        assert_eq!(build(&checkouts[1]), "p");
        std::fs::remove_dir_all(&dir).unwrap();
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

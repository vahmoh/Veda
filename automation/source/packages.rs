// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The system image's packages ([Packages](../../documentation/PACKAGES.md)):
//!
//! * **The system's repository**, `system`, in `/system/packages`: a valid
//!   repository, its index signed, which every Veda has, the live system's
//!   on a USB stick too, and finds at every start without a network. It
//!   offers no packages yet; what a release puts there later installs from
//!   the image itself.
//! * **Its configuration**, `/system/etc/packages/repositories/system.repo`
//!   (its address and the public key its index is checked against), and
//!   what the system provides (`/system/etc/packages/provides`: `veda` at
//!   the image's version), which packages may depend on.
//! * **The tests' repositories**, in `/system/tests/packages`: `one` and
//!   `two` (newer versions), both called `test`, signed by a key whose
//!   secret is no secret (it is made from a fixed seed, so that test images
//!   are the same everywhere): no system trusts it unless a test adds the
//!   repository, giving its public key (`test.pub`). They need the C test
//!   programs (`cargo automation toolchain`), whose `hello` is their
//!   packages' program. The host's web server serves `one`'s packages too,
//!   as repositories on the network are ([`web_files`]).
//!
//! **The key** that signs the system's repository is a release's, whose
//! secret key file `VEDA_REPOSITORY_KEY` names (kept off the build machine
//! but for a release's build), or else the build machine's own, made once
//! in the target directory (`packages/repository.key`) and kept there, so
//! that its images are the same from one build to the next. Ed25519 signs
//! the same index the same every time, and the indexes say they were made
//! on 2026-01-01, as the image's other dates do.

use std::path::{Path, PathBuf};

use packages::archive::{Builder, Compression};
use packages::index::{self, Contents, Offer};
use packages::keys::SecretKey;
use packages::manifest::Manifest;
use packages::{Digest, archive};

use crate::util::{self, Result};

/// The system's repository's name.
const SYSTEM: &str = "system";
/// Where its files are in the image, and the address of that directory.
const SYSTEM_DIR: &str = "packages";
const SYSTEM_URL: &str = "file:///system/packages";
/// The tests' repositories' name, and where they are.
const TEST: &str = "test";
const TEST_DIR: &str = "tests/packages";
/// When the image's indexes were made (2026-01-01 12:00:00 UTC), and the
/// system's index's serial: an image's own repository is read from the
/// image every time, so it needs neither to grow.
const GENERATED: u64 = 1_767_268_800;
const SERIAL: u64 = 1;
/// The environment variable naming a release's secret key file.
const KEY_VARIABLE: &str = "VEDA_REPOSITORY_KEY";

/// The secret key file that signs the system's repository, and whether it
/// is a release's (else the build machine's own).
fn key_file() -> (PathBuf, bool) {
    match std::env::var_os(KEY_VARIABLE) {
        Some(path) => (PathBuf::from(path), true),
        None => (util::target_dir().join("packages").join("repository.key"), false),
    }
}

/// The key in `path`, made there first if it is not there and `make`.
fn load_or_make(path: &Path, make: bool) -> Result<SecretKey> {
    if path.is_file() || !make {
        let text = zeroize::Zeroizing::new(util::read_to_string(path)?);
        return SecretKey::parse(&text).map_err(|e| format!("{}: {e}", path.display()));
    }
    let mut seed = zeroize::Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *seed).map_err(|e| format!("no random numbers for the repository's key: {e}"))?;
    let key = SecretKey::from_seed(&seed);
    if let Some(dir) = path.parent() {
        util::create_dir_all(dir)?;
    }
    util::write_atomically(path, key.to_text().as_bytes())?;
    util::status(
        "Made",
        format!(
            "the build machine's key for the system's repository, {} ({KEY_VARIABLE} names a release's)",
            path.display()
        ),
    );
    Ok(key)
}

/// The key that signs the system's repository: a release's, or the build
/// machine's own (made the first time).
fn signing_key() -> Result<SecretKey> {
    let (path, release) = key_file();
    load_or_make(&path, !release)
}

/// Which key signs the system's repository, for `doctor`.
pub fn key_status() -> Result<String> {
    let (path, release) = key_file();
    if !release && !path.is_file() {
        return Err(format!("this machine's own, made at the first build in {}", path.display()));
    }
    let key = load_or_make(&path, false)?;
    let whose = if release { "a release's" } else { "this machine's own" };
    Ok(format!("{whose}, {} (public key {})", path.display(), key.public().fingerprint()))
}

/// The tests' key: made from a fixed seed, its secret no secret.
fn test_key() -> SecretKey {
    SecretKey::from_seed(&Digest::of(b"Veda's test repositories, whose key is no secret").0)
}

/// A package of `manifest` (written as its `KEY=VALUE` lines) holding
/// `files`, as its repository offers it at `file`.
fn package(manifest: &str, files: &[(&str, &[u8])]) -> Result<(Offer, Vec<u8>)> {
    let manifest = Manifest::from_text(manifest).map_err(|e| e.to_string())?;
    let mut builder = Builder::new(manifest.clone());
    for (path, data) in files {
        builder.add(path, data.to_vec()).map_err(|e| e.to_string())?;
    }
    let bytes = builder.build(Compression::Deflate).map_err(|e| e.to_string())?;
    let unpacked = archive::Archive::open(&bytes).map_err(|e| e.to_string())?.unpacked();
    let file = format!("{}-{}.{}.vpkg", manifest.name, manifest.version, manifest.architecture);
    let offer = Offer { manifest, file, size: bytes.len() as u64, sha256: Digest::of(&bytes), unpacked };
    Ok((offer, bytes))
}

/// When a repository's index was made, and when it expires (Unix time,
/// s).
#[derive(Clone, Copy)]
struct Dates {
    generated: u64,
    expires: Option<u64>,
}

/// The image's indexes': made on 2026-01-01, never expiring.
const IMAGE: Dates = Dates { generated: GENERATED, expires: None };

/// A repository's files, under `dir`: its packages and its index of them,
/// signed by `key`.
fn repository(
    dir: &str,
    name: &str,
    serial: u64,
    packages: Vec<(Offer, Vec<u8>)>,
    key: &SecretKey,
    dates: Dates,
) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    let mut offers = Vec::new();
    for (offer, bytes) in packages {
        files.push((format!("{dir}/{}", offer.file), bytes));
        offers.push(offer);
    }
    let Dates { generated, expires } = dates;
    let contents = Contents { repository: name.to_string(), serial, generated, expires, packages: offers };
    let index = index::sign(contents, &[key], true).map_err(|e| format!("the {name} repository's index: {e}"))?;
    files.push((format!("{dir}/{}", index::FILE), index));
    Ok(files)
}

/// What the tests' packages' manifests share.
const COMMON: &str = "licence=GPL-3.0-only\nhomepage=https://www.vedaos.org\n";

/// The packages of the tests' first repository: `hello` (the C test program
/// `hello`, which says hello and ends with 42) and what it depends on, a
/// package whose file another's is, and one for a newer system than any.
fn first_packages(hello: &[u8]) -> Result<Vec<(Offer, Vec<u8>)>> {
    let common = COMMON;
    Ok(vec![
        package(
            &format!(
                "name=hello\nversion=1.0\narchitecture=amd64\nsummary=Says hello, for the tests\n{common}\
                 depends=libgreet (>= 1.0)\ndepends=veda (>= 0.1)\n"
            ),
            &[("bin/hello", hello), ("share/hello/README", b"hello 1.0, a package of Veda's tests.\n")],
        )?,
        package(
            &format!("name=libgreet\nversion=1.0\narchitecture=any\nsummary=Greetings, for the tests\n{common}"),
            &[("share/libgreet/greeting", b"Hello from libgreet 1.0\n")],
        )?,
        package(
            &format!(
                "name=clash\nversion=1.0\narchitecture=any\nsummary=A file of libgreet's, for the tests\n{common}"
            ),
            &[("share/libgreet/greeting", b"Not libgreet's\n")],
        )?,
        package(
            &format!(
                "name=toonew\nversion=1.0\narchitecture=any\nsummary=For a newer system, for the tests\n{common}\
                 depends=veda (>= 99)\n"
            ),
            &[("share/toonew/README", b"For Veda 99.\n")],
        )?,
    ])
}

/// The tests' repositories: `one`, and `two`, newer versions of its first
/// two.
fn test_repositories(hello: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let key = test_key();
    let common = COMMON;
    let two = vec![
        package(
            &format!(
                "name=hello\nversion=2.0\narchitecture=amd64\nsummary=Says hello, for the tests\n{common}\
                 depends=libgreet (>= 2.0)\ndepends=veda (>= 0.1)\n"
            ),
            &[
                ("bin/hello", hello),
                ("share/hello/NEWS", b"2.0: a NEWS file.\n"),
                ("share/hello/README", b"hello 2.0, a package of Veda's tests.\n"),
            ],
        )?,
        package(
            &format!("name=libgreet\nversion=2.0\narchitecture=any\nsummary=Greetings, for the tests\n{common}"),
            &[("share/libgreet/greeting", b"Hello from libgreet 2.0\n")],
        )?,
    ];
    let mut files = repository(&format!("{TEST_DIR}/one"), TEST, 1, first_packages(hello)?, &key, IMAGE)?;
    files.extend(repository(&format!("{TEST_DIR}/two"), TEST, 2, two, &key, IMAGE)?);
    files.push((format!("{TEST_DIR}/test.pub"), format!("{}\n", key.public()).into_bytes()));
    Ok(files)
}

/// What the host's web server serves the scripts as repositories on the
/// network, by path: `packages/web`, whose index (`web`'s) expires a day
/// after `now`, as one fetched over the network must; `packages/forever`,
/// whose index (`forever`'s) never does, which is refused; both with the
/// packages of the image's `one`, signed by the tests' key.
pub fn web_files(hello: &[u8], now: u64) -> Result<Vec<(String, Vec<u8>)>> {
    let key = test_key();
    let day = Dates { generated: now, expires: Some(now + 86_400) };
    let mut files = repository("packages/web", "web", now, first_packages(hello)?, &key, day)?;
    let forever = Dates { generated: now, expires: None };
    files.extend(repository("packages/forever", "forever", now, first_packages(hello)?, &key, forever)?);
    Ok(files)
}

/// The image's packages, repositories and their configuration;
/// `hello`, the C test program, if the toolchain has made it.
pub fn image_files(hello: Option<&[u8]>) -> Result<Vec<(String, Vec<u8>)>> {
    signed_files(&signing_key()?, hello)
}

/// [`image_files`], the system's repository signed by `key`.
fn signed_files(key: &SecretKey, hello: Option<&[u8]>) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = repository(SYSTEM_DIR, SYSTEM, SERIAL, Vec::new(), key, IMAGE)?;
    let config = format!(
        "# The repository of the system image: every Veda has it, and finds it at\n\
         # every start, from the image itself.\n\
         title=Veda system image\n\
         url={SYSTEM_URL}\n\
         key={}\n",
        key.public()
    );
    files.push((format!("etc/packages/repositories/{SYSTEM}.repo"), config.into_bytes()));
    files.push(("etc/packages/provides".into(), format!("veda (= {})\n", env!("CARGO_PKG_VERSION")).into_bytes()));
    match hello {
        Some(hello) => files.extend(test_repositories(hello)?),
        None => util::status("Note", "no test repositories in the image: they need the C test programs"),
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use packages::index::Policy;
    use packages::keys::PublicKey;

    #[test]
    fn the_build_machine_keeps_its_key() {
        let path = std::env::temp_dir().join(format!("automation-repository-{}", std::process::id())).join("key");
        assert!(load_or_make(&path, false).is_err());
        let made = load_or_make(&path, true).unwrap();
        assert_eq!(load_or_make(&path, true).unwrap().public(), made.public());
        assert_eq!(load_or_make(&path, false).unwrap().public(), made.public());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn the_image_has_a_valid_empty_repository_and_its_configuration() {
        let key = SecretKey::from_seed(&[5; 32]);
        let files = signed_files(&key, Some(b"\x7fELF hello")).unwrap();
        let file = |path: &str| &files.iter().find(|(p, _)| p == path).unwrap().1;
        // The configuration names the key the index checks against.
        let config = String::from_utf8(file("etc/packages/repositories/system.repo").clone()).unwrap();
        let repository = packages::config::system_repository(SYSTEM, &config).unwrap();
        assert_eq!(repository.url.to_string(), SYSTEM_URL);
        assert!(repository.url.in_system_image());
        let policy = Policy {
            keys: &repository.keys,
            repository: SYSTEM,
            now: GENERATED + 1,
            must_expire: false,
            previous: None,
        };
        let index = index::verify(file("packages/index.vidx"), &policy).unwrap();
        assert!(index.contents.packages.is_empty() && index.contents.expires.is_none());
        let provides = String::from_utf8(file("etc/packages/provides").clone()).unwrap();
        assert!(packages::requirement::Provision::parse(provides.trim()).is_ok(), "{provides}");
        assert_eq!(repository.keys, [key.public()]);
        // The same key makes the same image.
        assert_eq!(signed_files(&key, Some(b"\x7fELF hello")).unwrap(), files);
        // The tests' repositories check against the public key they come
        // with, and offer what the scripts install.
        let test_key = PublicKey::parse(std::str::from_utf8(file("tests/packages/test.pub")).unwrap()).unwrap();
        for (dir, serial, count) in [("one", 1, 4), ("two", 2, 2)] {
            let keys = [test_key.clone()];
            let policy =
                Policy { keys: &keys, repository: TEST, now: GENERATED + 1, must_expire: false, previous: None };
            let v = index::verify(file(&format!("tests/packages/{dir}/index.vidx")), &policy).unwrap();
            assert_eq!((v.contents.serial, v.contents.packages.len()), (serial, count));
            for offer in &v.contents.packages {
                let bytes = file(&format!("tests/packages/{dir}/{}", offer.file));
                assert_eq!(Digest::of(bytes), offer.sha256);
            }
        }
        // The network's: one expires, one does not.
        let web = web_files(b"\x7fELF hello", GENERATED).unwrap();
        let web_file = |path: &str| &web.iter().find(|(p, _)| p == path).unwrap().1;
        let keys = [test_key.clone()];
        for (name, expires) in [("web", Some(GENERATED + 86_400)), ("forever", None)] {
            let policy =
                Policy { keys: &keys, repository: name, now: GENERATED + 1, must_expire: false, previous: None };
            let v = index::verify(web_file(&format!("packages/{name}/index.vidx")), &policy).unwrap();
            assert_eq!((v.contents.expires, v.contents.packages.len()), (expires, 4));
        }
        // Without the C test programs, the system's repository alone.
        let alone = signed_files(&key, None).unwrap();
        assert!(alone.iter().all(|(p, _)| !p.starts_with(TEST_DIR)) && alone.len() == 3);
    }
}

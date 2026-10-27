// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The crates among the ports: Rust crates that Veda builds with changes
//! of its own (see "Crates" in `implementation/ports/README.md`).
//!
//! The workspace depends on each one's release by its version, as on any
//! crate. [`prepare`], which every `cargo automation` command runs before
//! anything else, has Cargo build the port in its place: it downloads and
//! checks the release, has `implementation/ports/crates.sh` unpack it and
//! apply the port's patches in `implementation/ports/.crates/src/<crate>`,
//! and writes `implementation/ports/.crates/config.toml`, a
//! `[patch.crates-io]` of them, which `.cargo/config.toml` includes. They
//! are kept outside `target/` (and `cargo clean`), as Cargo reads them
//! before it builds anything; until a checkout has them, Cargo takes the
//! releases as published.
//!
//! Resolving the workspace without them, Cargo drops their entries from
//! Cargo.lock and takes each crate's newest release that the requirements
//! on it allow, as a new checkout's first run of Cargo (the `cargo run`
//! behind `cargo automation`) does. Cargo.toml holds the crates the
//! workspace depends on directly at their ports' versions (`=`);
//! [`prepare`] moves the others back to theirs.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::architecture;
use crate::ports::{self, Port};
use crate::util::{self, Result};

/// The crates Veda builds as ports, by the names of their ports: a crate's
/// own, or, for an older release of a crate the workspace has two releases
/// of, its name and that release's (`getrandom-0.3`).
pub const CRATES: [&str; 11] = [
    "smoltcp",
    "zune-jpeg",
    "libc",
    "getrandom",
    "getrandom-0.3",
    "rustix",
    "errno",
    "socket2",
    "mio",
    "tokio",
    "libloading",
];

/// The programs that preparing them takes: for the downloads, and for
/// `implementation/ports/crates.sh`.
const TOOLS: [&str; 6] = ["curl", "bash", "tar", "gzip", "patch", "sha256sum"];

/// Their directory in `implementation/ports/`.
const DIRECTORY: &str = ".crates";

/// The source of their releases, crates.io, as Cargo.lock names it.
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// Where they are prepared: `downloads/` (the verified releases), `src/`
/// (each crate, unpacked and patched), `stamps/` (the fingerprints of
/// `implementation/ports/crates.sh`), `prepared` (the fingerprint of the
/// ports they were prepared from) and `config.toml` (Cargo's patch).
pub fn root() -> PathBuf {
    ports::dir().join(DIRECTORY)
}

/// Makes Cargo build every crate of [`CRATES`] as its port has it: prepares
/// them again when a port has changed (or they are not there), points
/// Cargo at them and checks that it builds them.
pub fn prepare() -> Result {
    let root = root();
    let (config, stamp) = (root.join("config.toml"), root.join("prepared"));
    let print = fingerprint()?;
    let mut changed = false;
    // One preparation at a time (another `cargo automation` may be at
    // it): the lock is held until Cargo has the crates.
    let _lock = if prepared(&root, &stamp, &print) {
        None
    } else {
        std::fs::create_dir_all(&root).map_err(|e| format!("creating {}: {e}", root.display()))?;
        let lock_path = root.join(".lock");
        let lock = std::fs::File::create(&lock_path).map_err(|e| format!("{}: {e}", lock_path.display()))?;
        lock.lock().map_err(|e| format!("locking {}: {e}", lock_path.display()))?;
        if !prepared(&root, &stamp, &print) {
            // Until the crates are ready, nothing says they are: Cargo takes
            // the releases (a crate that a failed preparation has left out
            // would stop it from building anything, `automation`
            // included), and the next command prepares them again (one it
            // has left half done too, whichever ports it is prepared from).
            for file in [&config, &stamp] {
                let _ = std::fs::remove_file(file);
            }
            unpack(&root)?;
            std::fs::write(&stamp, &print).map_err(|e| format!("writing {}: {e}", stamp.display()))?;
        }
        changed = true;
        Some(lock)
    };
    let text = config_text()?;
    if std::fs::read_to_string(&config).ok().as_deref() != Some(text.as_str()) {
        // Whole, for a Cargo that reads it meanwhile.
        let partial = config.with_extension("part");
        std::fs::write(&partial, &text).map_err(|e| format!("writing {}: {e}", partial.display()))?;
        std::fs::rename(&partial, &config).map_err(|e| format!("writing {}: {e}", config.display()))?;
        changed = true;
    }
    if changed {
        // Cargo resolved the workspace for this command without the crates
        // as they are now: it resolves it again (downloading nothing), and
        // takes the port of each crate that Cargo.lock has at its version.
        util::run(util::cargo().args(["update", "--workspace", "--quiet"]))?;
    }
    move_back()?;
    check_lock()
}

/// Moves back to its port's version each crate that Cargo.lock has from
/// crates.io at another version: one that Cargo resolved without the patch,
/// to the newest release the workspace allowed, and that
/// `cargo update --workspace`, which unlocks only the workspace's own
/// packages, leaves there.
fn move_back() -> Result {
    let path = util::workspace_root().join("Cargo.lock");
    for name in CRATES {
        let port = Port::named(name)?;
        let (package, version) = (&port.name, &port.version);
        // Read for each crate, as moving one changes it.
        let lock = util::read_to_string(&path)?;
        if let Some(release) = release_in_place_of(&lock, package, version) {
            util::status("Moving", format!("{package} {release} back to {version} (implementation/ports/{name})"));
            // With the patch, that version is the port's. Should something
            // in the workspace exclude it, Cargo says what, and check_lock()
            // fails.
            let spec = format!("{package}@{release}");
            let _ = util::run(util::cargo().args(["update", "--quiet", "--package", &spec, "--precise", version]));
        }
    }
    Ok(())
}

/// For `cargo automation doctor`: the crates, as Cargo builds them
/// (prepared now if they were not).
pub fn status() -> std::result::Result<String, String> {
    prepare()?;
    let crates: Vec<String> =
        CRATES.iter().map(|c| Port::named(c).map(|p| format!("{c} {}", p.version))).collect::<Result<_>>()?;
    Ok(format!("{} ({})", crates.join(", "), root().display()))
}

/// The SHA-256 of what the crates are prepared from:
/// `implementation/ports/crates.sh`, and each crate's `port.toml` and
/// patches (its own, and the architecture's).
fn fingerprint() -> Result<String> {
    use sha2::{Digest, Sha256};
    let ports = ports::dir();
    let mut files = vec![ports.join("crates.sh")];
    for name in CRATES {
        let dir = ports.join(name);
        files.push(dir.join("port.toml"));
        let patches = [dir.join("veda.patch"), dir.join("architectures").join(architecture::NAME).join("veda.patch")];
        files.extend(patches.into_iter().filter(|p| p.is_file()));
    }
    let mut hash = Sha256::new();
    for file in &files {
        let data = util::read(file)?;
        let name = file.strip_prefix(&ports).unwrap_or(file).to_string_lossy();
        hash.update(format!("{name}\0{}\0", data.len()));
        hash.update(&data);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Whether the crates are there, prepared from the ports as they are:
/// `stamp` has their fingerprint, `print`.
fn prepared(root: &Path, stamp: &Path, print: &str) -> bool {
    std::fs::read_to_string(stamp).is_ok_and(|p| p == print)
        && CRATES.iter().all(|c| root.join("src").join(c).join("Cargo.toml").is_file())
}

/// Downloads the crates' releases and has `implementation/ports/crates.sh`
/// unpack and patch those whose ports have changed.
fn unpack(root: &Path) -> Result {
    let missing: Vec<&str> = TOOLS.into_iter().filter(|t| util::find_on_path(t).is_none()).collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} missing: install from the distribution's packages (the crates in implementation/ports/ are \
             prepared with them)",
            missing.join(", ")
        ));
    }
    for name in CRATES {
        ports::fetch(&Port::named(name)?, &root.join("downloads"))?;
        // A crate that is not there is unpacked again, whatever its
        // fingerprint says.
        if !root.join("src").join(name).join("Cargo.toml").is_file() {
            let _ = std::fs::remove_file(root.join("stamps").join(format!("src_{name}")));
        }
    }
    let ports = ports::dir();
    util::run(
        Command::new("bash")
            .arg(ports.join("crates.sh"))
            .args(CRATES)
            .env("VEDA_PORTS", &ports)
            .env("VEDA_ARCH", architecture::NAME)
            .env("VEDA_CRATES", root),
    )
}

/// `config.toml`: the crates, for Cargo, in place of their releases. A
/// port named otherwise than its crate is a patch of that crate (its
/// `package`), of the release the port has.
fn config_text() -> Result<String> {
    let mut text = String::from(
        "# Written by `cargo automation` (automation/source/crates.rs): Cargo\n\
         # builds the crates in implementation/ports/ as they are prepared here,\n\
         # in place of their releases. It takes these paths from\n\
         # implementation/ports/, the directory above this file's. Should one of\n\
         # them be missing, remove implementation/ports/.crates: the next\n\
         # `cargo automation` command prepares them anew.\n\
         [patch.crates-io]\n",
    );
    for name in CRATES {
        let package = Port::named(name)?.name;
        let rename = if package == name { String::new() } else { format!(", package = \"{package}\"") };
        // Quoted: a port's name may have a dot (`getrandom-0.3`), which TOML
        // takes for a table's.
        text.push_str(&format!("\"{name}\" = {{ path = \"{DIRECTORY}/src/{name}\"{rename} }}\n"));
    }
    Ok(text)
}

/// Fails unless Cargo.lock has every crate as prepared: at its port's
/// version, from no registry. (Cargo takes a crate's release when the
/// workspace asks for a version other than its port's.)
fn check_lock() -> Result {
    let path = util::workspace_root().join("Cargo.lock");
    let lock = std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    for name in CRATES {
        let port = Port::named(name)?;
        if !locked_from_path(&lock, &port.name, &port.version) {
            return Err(format!(
                "Cargo does not build {} {} from implementation/ports/{name}: the workspace must depend on that \
                 version (`{} = {{ version = \"={}\", ... }}` in Cargo.toml)",
                port.name, port.version, port.name, port.version
            ));
        }
    }
    Ok(())
}

/// Whether `lock`, a Cargo.lock, has the package `name` at `version` from
/// no registry or repository: from a path, as a patch of ours is.
fn locked_from_path(lock: &str, name: &str, version: &str) -> bool {
    locked(lock, name).contains(&(version, None))
}

/// The release of the crate `name` that `lock`, a Cargo.lock, has from
/// crates.io in place of its port at `version`: another version
/// semver-compatible with the port's (Cargo takes one release of those).
fn release_in_place_of<'a>(lock: &'a str, name: &str, version: &str) -> Option<&'a str> {
    let range = semver_range(version);
    locked(lock, name)
        .into_iter()
        .find(|&(v, source)| source == Some(CRATES_IO) && v != version && semver_range(v) == range)
        .map(|(v, _)| v)
}

/// The versions of the package `name` that `lock`, a Cargo.lock, has, each
/// with its source: none for a path's. (A patch that Cargo leaves unused is
/// no package: it is listed apart, under `[[patch.unused]]`.)
fn locked<'a>(lock: &'a str, name: &str) -> Vec<(&'a str, Option<&'a str>)> {
    let name = format!("name = \"{name}\"");
    lock.split("\n\n")
        .filter_map(|entry| {
            let lines: Vec<&str> = entry.lines().collect();
            if lines.first() != Some(&"[[package]]") || !lines.contains(&name.as_str()) {
                return None;
            }
            let value =
                |key: &str| lines.iter().find_map(|l| l.strip_prefix(key)?.strip_prefix(" = \"")?.strip_suffix('"'));
            Some((value("version")?, value("source")))
        })
        .collect()
}

/// What the versions semver-compatible with `version` share: its numbers
/// up to the first that is not 0 (`0.2` of `0.2.190`, `1` of `1.53.3`).
fn semver_range(version: &str) -> String {
    let mut range = Vec::new();
    for number in version.split(['-', '+']).next().unwrap_or(version).split('.') {
        range.push(number);
        if number != "0" {
            break;
        }
    }
    range.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_lock_tells_a_patched_crate_from_its_release() {
        let patched =
            "[[package]]\nname = \"zune-jpeg\"\nversion = \"0.5.15\"\ndependencies = [\n \"zune-core\",\n]\n\n";
        let release = "[[package]]\nname = \"zune-jpeg\"\nversion = \"0.5.15\"\n\
                       source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n";
        assert!(locked_from_path(patched, "zune-jpeg", "0.5.15"));
        assert!(!locked_from_path(release, "zune-jpeg", "0.5.15"));
        assert!(!locked_from_path(patched, "zune-jpeg", "0.5.16"));
        assert!(!locked_from_path(patched, "zune-core", "0.5.15"));
        // The workspace asks for another version: Cargo takes that one's
        // release, and leaves the patch unused.
        let unused = "[[package]]\nname = \"zune-jpeg\"\nversion = \"0.5.16\"\n\
                      source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n\
                      [[patch.unused]]\nname = \"zune-jpeg\"\nversion = \"0.5.15\"\n";
        assert!(!locked_from_path(unused, "zune-jpeg", "0.5.15"));
    }

    #[test]
    fn cargo_lock_tells_the_release_in_place_of_a_port() {
        let lock = "[[package]]\nname = \"getrandom\"\nversion = \"0.2.17\"\n\
                    source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n\
                    [[package]]\nname = \"getrandom\"\nversion = \"0.4.4\"\n\
                    source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n\
                    [[package]]\nname = \"libc\"\nversion = \"0.2.190\"\n\n\
                    [[package]]\nname = \"mio\"\nversion = \"1.2.5\"\n\
                    source = \"git+https://github.com/tokio-rs/mio#0123abcd\"\n\n";
        assert_eq!(release_in_place_of(lock, "getrandom", "0.4.3"), Some("0.4.4"));
        // 0.2.17 and 0.4.4 are of other ranges than 0.3.4's.
        assert_eq!(release_in_place_of(lock, "getrandom", "0.3.4"), None);
        // The port itself.
        assert_eq!(release_in_place_of(lock, "libc", "0.2.190"), None);
        // No patch of crates.io takes the place of a repository's crate.
        assert_eq!(release_in_place_of(lock, "mio", "1.2.4"), None);
        assert_eq!(semver_range("0.2.190"), "0.2");
        assert_eq!(semver_range("1.53.3"), "1");
        assert_eq!(semver_range("0.0.3"), "0.0.3");
        assert_eq!(semver_range("1.0.0-alpha.5"), "1");
    }

    #[test]
    fn cargo_finds_each_crate_where_it_is_prepared() {
        // Cargo takes the paths of a configuration file from the directory
        // above the file's own.
        let text = config_text().unwrap();
        for name in CRATES {
            let line = text.lines().find(|l| l.starts_with(&format!("\"{name}\" = "))).expect("a patch for each crate");
            let path = line.split('"').nth(3).expect("a path");
            assert_eq!(root().parent().unwrap().join(path), root().join("src").join(name));
        }
    }
}

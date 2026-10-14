// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The crates among the ports: Rust crates that Veda builds with changes
//! of its own (see "Crates" in `implementation/ports/README.md`).
//!
//! The workspace depends on each one's release by its version, as on any
//! crate. [`prepare`], which every `cargo xtask` command runs before
//! anything else, has Cargo build the port in its place: it downloads and
//! checks the release, has `implementation/ports/crates.sh` unpack it and
//! apply the port's patches in `implementation/ports/.crates/src/<crate>`,
//! and writes `implementation/ports/.crates/config.toml`, a
//! `[patch.crates-io]` of them, which `.cargo/config.toml` includes. They
//! are kept outside `target/` (and `cargo clean`), as Cargo reads them
//! before it builds anything; until a checkout has them, Cargo takes the
//! releases as published.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch;
use crate::ports::{self, Port};
use crate::util::{self, Result};

/// The crates Veda builds as ports, by the names of their ports (which are
/// the crates').
pub const CRATES: [&str; 2] = ["smoltcp", "zune-jpeg"];

/// The programs that preparing them takes: for the downloads, and for
/// `implementation/ports/crates.sh`.
const TOOLS: [&str; 6] = ["curl", "bash", "tar", "gzip", "patch", "sha256sum"];

/// Their directory in `implementation/ports/`.
const DIRECTORY: &str = ".crates";

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
    // One preparation at a time (another `cargo xtask` may be at it): the
    // lock is held until Cargo has the crates.
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
            // would stop it from building anything, xtask included), and the
            // next command prepares them again (one it has left half done
            // too, whichever ports it is prepared from).
            for file in [&config, &stamp] {
                let _ = std::fs::remove_file(file);
            }
            unpack(&root)?;
            std::fs::write(&stamp, &print).map_err(|e| format!("writing {}: {e}", stamp.display()))?;
        }
        changed = true;
        Some(lock)
    };
    let text = config_text();
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
        // Cargo.lock has them.
        util::run(util::cargo().args(["update", "--workspace", "--quiet"]))?;
    }
    check_lock()
}

/// For `cargo xtask doctor`: the crates, as Cargo builds them (prepared
/// now if they were not).
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
        let patches = [dir.join("veda.patch"), dir.join("arch").join(arch::NAME).join("veda.patch")];
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
            .env("VEDA_ARCH", arch::NAME)
            .env("VEDA_CRATES", root),
    )
}

/// `config.toml`: the crates, for Cargo, in place of their releases.
fn config_text() -> String {
    let mut text = String::from(
        "# Written by `cargo xtask` (xtask/src/crates.rs): Cargo builds the\n\
         # crates in implementation/ports/ as they are prepared here, in place\n\
         # of their releases. It takes these paths from implementation/ports/,\n\
         # the directory above this file's. Should one of them be missing,\n\
         # remove implementation/ports/.crates: the next `cargo xtask` command\n\
         # prepares them anew.\n\
         [patch.crates-io]\n",
    );
    for name in CRATES {
        text.push_str(&format!("{name} = {{ path = \"{DIRECTORY}/src/{name}\" }}\n"));
    }
    text
}

/// Fails unless Cargo.lock has every crate as prepared: at its port's
/// version, from no registry. (Cargo takes a crate's release when the
/// workspace asks for a version other than its port's.)
fn check_lock() -> Result {
    let path = util::workspace_root().join("Cargo.lock");
    let lock = std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    for name in CRATES {
        let port = Port::named(name)?;
        if !locked_from_path(&lock, name, &port.version) {
            return Err(format!(
                "Cargo does not build {name} {} from implementation/ports/{name}: the workspace must depend \
                 on that version (`{name} = {{ version = \"={}\", ... }}` in Cargo.toml)",
                port.version, port.version
            ));
        }
    }
    Ok(())
}

/// Whether `lock`, a Cargo.lock, has the package `name` at `version` from
/// no registry or repository: from a path, as a patch of ours is. (A
/// patch that Cargo leaves unused is no package: it is listed apart, under
/// `[[patch.unused]]`.)
fn locked_from_path(lock: &str, name: &str, version: &str) -> bool {
    let (name, version) = (format!("name = \"{name}\""), format!("version = \"{version}\""));
    lock.split("\n\n").any(|entry| {
        let lines: Vec<&str> = entry.lines().collect();
        lines.first() == Some(&"[[package]]")
            && lines.contains(&name.as_str())
            && lines.contains(&version.as_str())
            && !lines.iter().any(|l| l.starts_with("source = "))
    })
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
    fn cargo_finds_each_crate_where_it_is_prepared() {
        // Cargo takes the paths of a configuration file from the directory
        // above the file's own.
        let text = config_text();
        for name in CRATES {
            let line = text.lines().find(|l| l.starts_with(&format!("{name} = "))).expect("a patch for each crate");
            let path = line.split('"').nth(1).expect("a path");
            assert_eq!(root().parent().unwrap().join(path), root().join("src").join(name));
        }
    }
}

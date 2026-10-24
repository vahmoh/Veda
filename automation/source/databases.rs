// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The databases the C library reads, in `/system/etc` (where Veda's patch
//! of musl looks for what Linux has in `/etc`): `services`, the services
//! of the Internet's protocols and their ports, IANA's registry from the
//! `iana-etc` port (the service names of `getaddrinfo`, `getservbyname` and
//! `getnameinfo`), and `passwd` and `group`, Veda's one user, as which
//! every program runs (`getpwuid`, `getgrgid`...). The image has them with
//! the C toolchain, whose downloads have the port.

use std::path::Path;
use std::process::Command;

use crate::ports::{self, Port};
use crate::util::{self, Result};

/// The port of the services database.
pub const PORT: &str = "iana-etc";

/// Veda's one user: its name (the Terminal's `USER`), its user and group id
/// (`rt::process::UID`, which the POSIX layer's `getuid` and `getgid` say)
/// and its home (`files::path::HOME`).
const USER: &str = "user";
const ID: u32 = 1000;
const HOME: &str = "/home/user";

/// The databases for the image, with where the services' source is
/// (`licences/iana-etc/SOURCE`); the port is fetched into `downloads` if
/// it is not there.
pub fn image_files(downloads: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let port = Port::named(PORT)?;
    // No shell: the Terminal's is its own.
    let passwd = format!("{USER}:x:{ID}:{ID}:{USER}:{HOME}:\n");
    let group = format!("{USER}:x:{ID}:\n");
    let note = port.source_note()
        + "\nIANA's registry of service names and port numbers, which IANA and the IETF dedicate to the public \
           domain (CC0 1.0: https://creativecommons.org/publicdomain/zero/1.0/legalcode).\n";
    Ok(vec![
        ("etc/services".into(), services(&port, downloads)?),
        ("etc/passwd".into(), passwd.into_bytes()),
        ("etc/group".into(), group.into_bytes()),
        (format!("licences/{PORT}/SOURCE"), note.into_bytes()),
    ])
}

/// The services database of the port's release, taken out of its archive
/// once and kept beside it.
fn services(port: &Port, downloads: &Path) -> Result<Vec<u8>> {
    let kept = downloads.join(format!("{}-{}.services", port.name, port.version));
    if !kept.is_file() {
        ports::fetch(port, downloads)?;
        let archive = port.archive(downloads);
        let member = format!("{}-{}/services", port.name, port.version);
        let out = Command::new("tar")
            .arg("-xzOf")
            .arg(&archive)
            .arg(&member)
            .output()
            .map_err(|e| format!("running tar: {e}"))?;
        if !out.status.success() || out.stdout.is_empty() {
            return Err(format!("{} has no {member}", archive.display()));
        }
        util::write(&kept, &out.stdout)?;
    }
    util::read(&kept)
}

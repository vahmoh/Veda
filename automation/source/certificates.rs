// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The certificate authorities that OpenSSL trusts in the system image,
//! `/system/etc/ssl/cert.pem` (its `--openssldir` is `/system/etc/ssl`):
//! Mozilla's list, as curl extracts it, from the `ca-certificates` port.
//! OpenSSL's TLS is the web engine's (under glib-networking) and Python's
//! (`ssl`); Veda's own programs trust webpki-roots', built in. The image
//! has the list once either is built.

use std::path::Path;

use crate::ports::{self, Port};
use crate::util::Result;

/// The port of the certificate authorities.
pub const PORT: &str = "ca-certificates";

/// The list for the image, fetched into `downloads` if it is not there:
/// `etc/ssl/cert.pem`, and where its source is
/// (`licences/ca-certificates/SOURCE`), as the Mozilla Public License asks.
pub fn image_files(downloads: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let port = Port::named(PORT)?;
    ports::fetch(&port, downloads)?;
    let mut files = Vec::new();
    for path in port.files(downloads) {
        files.push(("etc/ssl/cert.pem".to_string(), crate::util::read(&path)?));
    }
    let note = port.source_note()
        + "\nThe list is Mozilla's (its first lines say where its source is), under the Mozilla Public License 2.0, \
           whose text is at https://mozilla.org/MPL/2.0/.\n";
    files.push((format!("licences/{PORT}/SOURCE"), note.into_bytes()));
    Ok(files)
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Fetching a repository's files: from a directory of this system's
//! (`file://`, the system image's own repository among them), or from a
//! web server (`https://`, `http://`), each within a limit of bytes, which
//! the repository's signed index sets for its package files, and a web
//! server's within a time limit too. What is fetched is checked by the
//! caller (an index's signatures, a package's SHA-256), never trusted for
//! where it came from: a web server, or what stands between it and Veda,
//! can send anything.

use std::io::Read;

use packages::config::Url;
use packages::index::Offer;
use packages::{Digest, index, limits};
use runtime::time::Duration;

/// The time a web server has to send an index.
const INDEX_TIME: Duration = Duration::from_secs(60);
/// The time a web server has to send a package file: half a minute, and a
/// second for each 32 KiB of it.
fn package_time(size: u64) -> Duration {
    Duration::from_secs(30 + size / (32 * 1024))
}

/// `file` of the repository at `url`, at most `limit` bytes.
fn get(url: &Url, file: &str, limit: u64, time: Duration) -> Result<Vec<u8>, String> {
    let address = url.join(file);
    match url {
        Url::File(_) => {
            let f = std::fs::File::open(&address).map_err(|e| format!("{address}: {e}"))?;
            let mut data = Vec::new();
            f.take(limit.saturating_add(1)).read_to_end(&mut data).map_err(|e| format!("{address}: {e}"))?;
            if data.len() as u64 > limit {
                return Err(format!("{address} is larger than it may be"));
            }
            Ok(data)
        }
        Url::Web { .. } => web::http::get(&address, time, limit as usize)
            .and_then(web::http::Response::ok)
            .map(|r| r.body)
            .map_err(|e| format!("{address}: {e}")),
    }
}

/// A repository's index file, unchecked.
pub fn index(url: &Url) -> Result<Vec<u8>, String> {
    get(url, index::FILE, limits::INDEX_FILE as u64, INDEX_TIME)
}

/// The package file `offer` names, checked against its size and SHA-256.
pub fn package(url: &Url, offer: &Offer) -> Result<Vec<u8>, String> {
    let bytes = get(url, &offer.file, offer.size, package_time(offer.size))?;
    if bytes.len() as u64 != offer.size || Digest::of(&bytes) != offer.sha256 {
        return Err(format!(
            "{} is not the package file the repository's index names (its size or SHA-256 differs)",
            url.join(&offer.file)
        ));
    }
    Ok(bytes)
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Saved networks on the home disk, in the Wi-Fi service's private
//! directory (`/home/.private/wlan`), which the file system opens to no
//! other program.
//!
//! The file holds passphrases, which the service needs to join networks
//! again. The `wlan` API never returns them and no application reads the
//! file, but they are not encrypted on the disk.
//!
//! Earlier systems kept the file in the user's configuration directory,
//! where every program reads and writes it; it moves from there as the
//! service starts ([`move_old`]).

use alloc::vec::Vec;

use files::fs::{Error, Fs, FsError};
use rt::println;
use wlan::profile::{self, Profile};

const DIR: &str = "/home/.private/wlan";
const PATH: &str = "/home/.private/wlan/networks";
const TEMP: &str = "/home/.private/wlan/networks.new";
/// Where the file was before, and the new file a save there wrote first.
const OLD_DIR: &str = "/home/user/.config/wlan";
const OLD_PATH: &str = "/home/user/.config/wlan/networks";
const OLD_TEMP: &str = "/home/user/.config/wlan/networks.new";

/// Reads the saved networks (none if the file is missing or unreadable).
pub fn load(fs: &Fs) -> Vec<Profile> {
    match fs.read(PATH) {
        Ok(data) => profile::parse(&data),
        Err(_) => Vec::new(),
    }
}

/// Writes the saved networks.
pub fn save(fs: &Fs, profiles: &[Profile]) -> bool {
    write(fs, profile::serialize(profiles).as_bytes()).is_ok()
}

/// Writes `data` as the file: to a new file first, then renamed over the
/// old one, so a failure part-way leaves the previous list intact.
fn write(fs: &Fs, data: &[u8]) -> Result<(), Error> {
    fs.mkdir_all(DIR)?;
    fs.write(TEMP, data)?;
    if fs.rename(TEMP, PATH).is_ok() {
        return Ok(());
    }
    // Renaming over an existing file is not supported everywhere: replace
    // it instead.
    let _ = fs.remove(PATH);
    fs.rename(TEMP, PATH)
}

/// Moves the file from where it was before to the private directory, if
/// that has none yet: one there is newer, and the old one then stays as it
/// is. The old file goes, and its directory if nothing else is in it, only
/// once the new one reads back whole, so a step that fails loses nothing;
/// the next start goes on from there. With nothing to move, an empty list
/// takes the new place, so that a file a program puts in the old one later
/// is never taken for saved networks.
pub fn move_old(fs: &Fs) {
    let new = match fs.read(PATH) {
        Ok(new) => Some(new),
        Err(Error::Fs(FsError::NotFound)) => None,
        // Whether there is one is not known: both stay as they are.
        Err(_) => return,
    };
    // The service never wrote a larger file (any program may have).
    let old = match fs.read_limited(OLD_PATH, profile::MAX_FILE) {
        Ok(old) => old,
        // The new file wins, whatever the old place holds.
        Err(_) if new.is_some() => return,
        Err(Error::Fs(FsError::NotFound)) => {
            let _ = save(fs, &[]);
            return;
        }
        Err(e) => {
            println!("cannot move the saved networks from {}: {}", OLD_PATH, e);
            return;
        }
    };
    match new {
        // An earlier start moved it, but could not remove the old file.
        Some(new) if new == old => {}
        Some(_) => {
            println!("the saved networks in {} stay there: those in {} are newer", OLD_PATH, PATH);
            return;
        }
        None => {
            if let Err(e) = write(fs, &old) {
                println!("cannot move the saved networks from {} to {}: {}", OLD_PATH, PATH, e);
                return;
            }
            if !fs.read(PATH).is_ok_and(|back| back == old) {
                println!("cannot move the saved networks from {} to {}: they do not read back whole", OLD_PATH, PATH);
                return;
            }
            println!("moved the saved networks from {} to {}", OLD_PATH, PATH);
        }
    }
    if let Err(e) = fs.remove(OLD_PATH) {
        println!("cannot remove the saved networks left in {}: {}", OLD_PATH, e);
        return;
    }
    // A save that did not finish there left passphrases too; the directory
    // is removed only if empty.
    let _ = fs.remove(OLD_TEMP);
    let _ = fs.remove(OLD_DIR);
}

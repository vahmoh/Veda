// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `efivartest` — checks, inside the driver VM's Linux, the PC's firmware
//! variables Veda gave it, as a test's firmware has them
//! (`verification/ui/drivervm-variables.vts`): two variables of a test
//! vendor's, `VedaTest`, which an operating system may read, and
//! `VedaBootOnly`, which it may not. The guest reads the first through
//! efivarfs as the firmware keeps it, does not have the second, and cannot
//! change them. It says `efivartest: PASS` when all is well.

use std::fs;

use guest_sys::checks::Checks;
use rustix::mount::{MountFlags, mount};

/// The test vendor's GUID.
const VENDOR: &str = "4c9c67e7-ba9b-4464-baa5-3e0538e7a82d";
/// `VedaTest`'s attributes (non-volatile, boot services' and the
/// runtime's) and data, as the test's firmware has them.
const ATTRIBUTES: u32 = 7;
const DATA: &[u8] = b"as the PC keeps it";
/// Where efivarfs goes.
const MOUNTED: &str = "/efivars";

fn main() {
    let mut c = Checks::new("efivartest");
    let mounted = fs::create_dir_all(MOUNTED)
        .and_then(|()| Ok(mount("efivarfs", MOUNTED, "efivarfs", MountFlags::empty(), None)?));
    if let Err(e) = &mounted {
        println!("efivartest: cannot mount efivarfs: {e}");
    }
    c.check("efivarfs mounts", mounted.is_ok());

    let names: Vec<String> = fs::read_dir(MOUNTED)
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    println!("efivartest: {} variables", names.len());
    let test = format!("VedaTest-{VENDOR}");
    c.check("the variable an operating system may read is listed", names.contains(&test));
    c.check("the one it may not read is not", !names.iter().any(|n| n.starts_with("VedaBootOnly-")));

    // efivarfs gives a variable as its attributes, then its data.
    let read = fs::read(format!("{MOUNTED}/{test}")).unwrap_or_default();
    let (attributes, data) = read.split_at_checked(4).unwrap_or_default();
    c.check("it has its attributes", attributes == ATTRIBUTES.to_le_bytes());
    c.check("and its data", data == DATA);

    let written = fs::write(format!("{MOUNTED}/VedaNew-{VENDOR}"), [7, 0, 0, 0, 1]);
    c.check("variables cannot be made", written.is_err());

    c.finish();
}

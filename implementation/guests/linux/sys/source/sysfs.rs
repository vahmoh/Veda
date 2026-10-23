// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What Linux's sysfs (and procfs) say: a file's text, and what a link
//! names (a device's driver, its bus, the device itself).

use std::path::Path;

/// The text of the file at `path`, trimmed (empty if it cannot be read).
pub fn read(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// The name of what the link at `path` points to (its last component).
pub fn link_name(path: impl AsRef<Path>) -> Option<String> {
    Some(std::fs::read_link(path).ok()?.file_name()?.to_string_lossy().into_owned())
}

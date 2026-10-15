// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The guest's processes, as `/proc` lists them.

use rustix::process::Pid;

/// The processes called `name` (their command's name, as
/// `/proc/<pid>/comm` has it).
pub fn named(name: &str) -> Vec<Pid> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.flatten()
        .filter_map(|e| {
            let pid = Pid::from_raw(e.file_name().to_str()?.parse().ok()?)?;
            let comm = std::fs::read_to_string(e.path().join("comm")).ok()?;
            (comm.trim() == name).then_some(pid)
        })
        .collect()
}

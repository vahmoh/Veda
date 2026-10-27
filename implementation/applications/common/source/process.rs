// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The running processes, and which of them are the system's.
//!
//! An application is a process the launcher started as one
//! (`TaskInfo::is_app`), or one an application started (a program run in
//! the Terminal); the system's are its services, what they started (the
//! drivers), and a process whose ancestry is lost (its parent ended). This
//! is how `init` decides who may end what, so the Terminal's `kill` and
//! Task Manager tell the user the same. When the launcher cannot be asked,
//! every process counts as the system's: ending one then takes the user's
//! explicit word.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec;
use alloc::vec::Vec;

use abi::ProcessInfo;
use proto::init::TaskInfo;

/// How far up its parents a process's ancestry is followed.
const MAX_DEPTH: usize = 64;

/// The processes the kernel lists (running or not), however many there
/// are.
pub fn list() -> Vec<ProcessInfo> {
    let mut infos = vec![ProcessInfo::default(); 256];
    loop {
        let n = rt::object::process_list(&mut infos).unwrap_or(0);
        if n <= infos.len() {
            infos.truncate(n);
            return infos;
        }
        // More than there was room for: again, with room for those and a
        // few started meanwhile.
        infos = vec![ProcessInfo::default(); n + 32];
    }
}

/// The running processes, by process id.
pub fn running() -> Vec<ProcessInfo> {
    let mut infos = list();
    infos.retain(|p| p.state == abi::process_state::RUNNING);
    infos.sort_by_key(|p| p.koid);
    infos
}

/// Tells the system's processes from applications'.
#[derive(Debug, Clone, Default)]
pub struct Classifier {
    /// What the launcher started as applications.
    apps: BTreeSet<u64>,
    /// Each process's parent.
    parents: BTreeMap<u64, u64>,
}

impl Classifier {
    /// From the processes (with their parents) and the processes the
    /// launcher started as applications.
    pub fn new(processes: &[ProcessInfo], apps: impl IntoIterator<Item = u64>) -> Classifier {
        Classifier {
            apps: apps.into_iter().collect(),
            parents: processes.iter().map(|p| (p.koid, p.parent_koid)).collect(),
        }
    }

    /// From the processes and the launcher's tasks (`None` when the
    /// launcher could not be asked: everything is the system's).
    pub fn from_tasks(processes: &[ProcessInfo], tasks: Option<&[TaskInfo]>) -> Classifier {
        let apps = tasks.unwrap_or_default().iter().filter(|t| t.is_app).map(|t| t.koid);
        Classifier::new(processes, apps)
    }

    /// Whether process `koid` is the system's.
    pub fn is_system(&self, koid: u64) -> bool {
        let mut k = koid;
        for _ in 0..MAX_DEPTH {
            if self.apps.contains(&k) {
                return false;
            }
            match self.parents.get(&k) {
                Some(&parent) if parent != 0 && parent != k => k = parent,
                _ => return true,
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(koid: u64, parent: u64) -> ProcessInfo {
        ProcessInfo { koid, parent_koid: parent, ..Default::default() }
    }

    #[test]
    fn applications_and_what_they_started() {
        // init (1) started filesystem (2), device-manager (3), the Terminal
        // (10) as an application; device-manager started a driver (4); the
        // Terminal a program (11), which started another (12). 20's parent
        // has ended.
        let procs = [p(1, 0), p(2, 1), p(3, 1), p(4, 3), p(10, 1), p(11, 10), p(12, 11), p(20, 99)];
        let c = Classifier::new(&procs, [10]);
        for system in [1, 2, 3, 4, 20, 404] {
            assert!(c.is_system(system), "{system}");
        }
        for app in [10, 11, 12] {
            assert!(!c.is_system(app), "{app}");
        }
        // Without the launcher, nothing is known to be an application.
        let c = Classifier::from_tasks(&procs, None);
        assert!(c.is_system(10) && c.is_system(11));
        // A loop in the ancestry (which the kernel never gives) ends.
        assert!(Classifier::new(&[p(5, 6), p(6, 5)], []).is_system(5));
    }
}

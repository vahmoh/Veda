// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What a change does: from what is installed and what the resolver chose,
//! the packages to install, upgrade, downgrade and remove, what is to be
//! downloaded, and how the installed size changes.
//!
//! A plan has a *digest*: the SHA-256 of what is installed (the database's
//! generation), of every step with the SHA-256 of the package file it
//! installs, and of the world after. Whoever shows a plan to the user hands
//! its digest back when they agree; the package manager carries the change
//! out only if the plan it works out then has the same digest, so that
//! what is done is what was agreed to, even if a repository was updated or
//! another change was made in between.

use alloc::vec::Vec;

use crate::database::Database;
use crate::name::Name;
use crate::requirement::Requirement;
use crate::resolve::{Choice, Problem, Solution};
use crate::version::Version;
use crate::{Digest, Hasher};

/// What a step does to a package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Action {
    Remove,
    Downgrade,
    Upgrade,
    Install,
}

/// One package's step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub action: Action,
    pub name: Name,
    /// The version installed now, and the one after.
    pub from: Option<Version>,
    pub to: Option<Version>,
    /// The offer installed ([`Problem::available`]'s), for every action but
    /// `Remove`.
    pub offer: Option<usize>,
}

/// A change, worked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// By name.
    pub steps: Vec<Step>,
    /// The world after.
    pub world: Vec<Requirement>,
    /// The package files to install, in bytes together.
    pub download: u64,
    /// The installed packages' size after, less before.
    pub growth: i64,
    pub digest: Digest,
}

impl Plan {
    /// Whether it changes nothing installed (the world may still change: a
    /// package installed for another is now asked for by name).
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// Works out the plan of `solution`, a solution of `problem`, from `db`,
/// leaving `world` as the world.
pub fn plan(db: &Database, problem: &Problem, solution: &Solution, world: Vec<Requirement>) -> Plan {
    let mut steps = Vec::new();
    let (mut download, mut growth) = (0u64, 0i64);
    for installed in &db.packages {
        let name = &installed.manifest.name;
        if !solution.chosen.contains_key(name) {
            growth -= installed.size() as i64;
            steps.push(Step {
                action: Action::Remove,
                name: name.clone(),
                from: Some(installed.manifest.version.clone()),
                to: None,
                offer: None,
            });
        }
    }
    for (name, choice) in &solution.chosen {
        let Choice::Available(j) = *choice else { continue };
        let offer = problem.available[j].offer;
        let from = db.get(name.as_str());
        let action = match from {
            None => Action::Install,
            Some(old) if old.manifest.version < offer.manifest.version => Action::Upgrade,
            Some(_) => Action::Downgrade,
        };
        download += offer.size;
        growth += offer.unpacked as i64 - from.map_or(0, |p| p.size() as i64);
        steps.push(Step {
            action,
            name: name.clone(),
            from: from.map(|p| p.manifest.version.clone()),
            to: Some(offer.manifest.version.clone()),
            offer: Some(j),
        });
    }
    steps.sort_by(|a, b| a.name.cmp(&b.name));
    let world = Database::tidy_world(world);
    let digest = digest(db.generation, problem, &steps, &world);
    Plan { steps, world, download, growth, digest }
}

/// The plan's digest (see the module's documentation): each field
/// preceded by its length, so that no two plans write the same bytes.
fn digest(generation: u64, problem: &Problem, steps: &[Step], world: &[Requirement]) -> Digest {
    fn field(h: &mut Hasher, bytes: &[u8]) {
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    }
    let mut h = Hasher::new();
    field(&mut h, b"Veda package plan 1");
    field(&mut h, &generation.to_le_bytes());
    for s in steps {
        field(&mut h, &[s.action as u8]);
        field(&mut h, s.name.as_str().as_bytes());
        field(&mut h, s.from.as_ref().map_or("", Version::as_str).as_bytes());
        field(&mut h, s.to.as_ref().map_or("", Version::as_str).as_bytes());
        field(&mut h, &s.offer.map_or(Digest::default(), |j| problem.available[j].offer.sha256).0);
    }
    for w in world {
        field(&mut h, alloc::format!("{w}").as_bytes());
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};
    use alloc::vec;

    use super::*;
    use crate::archive::Entry;
    use crate::database::Installed;
    use crate::index::Offer;
    use crate::manifest::Architecture;
    use crate::resolve::{self, Available, Upgrade};
    use crate::tests::manifest;

    fn offer(label: &str) -> Offer {
        let m = manifest(label);
        Offer {
            file: alloc::format!("{}.vpkg", m.name),
            manifest: m,
            size: 100,
            sha256: Digest::of(label.as_bytes()),
            unpacked: 1000,
        }
    }

    fn installed(label: &str, size: u64) -> Installed {
        Installed {
            manifest: manifest(label),
            repository: String::from("main"),
            sha256: Digest::default(),
            installed: 0,
            files: vec![Entry { path: String::from("bin/x"), size, sha256: Digest::default() }],
        }
    }

    #[test]
    fn plans_say_what_changes() {
        let mut db = Database::new();
        db.generation = 7;
        db.packages = vec![installed("gone 1.0", 50), installed("hello 1.0", 400), installed("lib 3.0", 300)];
        let offers = [offer("hello 2.0"), offer("lib 2.0"), offer("new 1.0")];
        let world = vec![Requirement::parse("new").unwrap(), Requirement::parse("hello").unwrap()];
        let lib = Requirement::parse("lib (< 3)").unwrap();
        let mut asked = world.clone();
        asked.push(lib);
        let problem = Problem {
            system: &[],
            installed: db.packages.iter().map(|package| resolve::Installed { package, repository: Some(0) }).collect(),
            available: offers.iter().map(|offer| Available { offer, repository: 0 }).collect(),
            world: &asked,
            removing: &[],
            upgrade: Upgrade::Everything,
            architecture: Architecture::Amd64,
        };
        let solution = resolve::resolve(&problem).unwrap();
        let p = plan(&db, &problem, &solution, asked.clone());
        let steps: Vec<String> = p
            .steps
            .iter()
            .map(|s| {
                alloc::format!(
                    "{:?} {} {:?} -> {:?}",
                    s.action,
                    s.name,
                    s.from.as_ref().map(Version::to_string),
                    s.to.as_ref().map(Version::to_string)
                )
            })
            .collect();
        assert_eq!(
            steps,
            [
                "Remove gone Some(\"1.0\") -> None",
                "Upgrade hello Some(\"1.0\") -> Some(\"2.0\")",
                "Downgrade lib Some(\"3.0\") -> Some(\"2.0\")",
                "Install new None -> Some(\"1.0\")",
            ]
        );
        assert_eq!(p.download, 300);
        assert_eq!(p.growth, -50 + (1000 - 400) + (1000 - 300) + 1000);
        let written: Vec<String> = p.world.iter().map(|w| w.to_string()).collect();
        assert_eq!(written, ["hello", "lib (< 3)", "new"]);
        // The same plan, the same digest; anything changed, another.
        assert_eq!(plan(&db, &problem, &solution, asked.clone()).digest, p.digest);
        db.generation = 8;
        assert_ne!(plan(&db, &problem, &solution, asked.clone()).digest, p.digest);
        db.generation = 7;
        assert_ne!(plan(&db, &problem, &solution, world).digest, p.digest);
        assert!(!p.is_empty());
    }
}

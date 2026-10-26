// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What is installed: each package's manifest, the repository it came from,
//! its package file's SHA-256, and every file it installed with its size
//! and SHA-256; and the *world*, the packages the user asked for by name
//! (with the constraints they asked with: `hello (< 2)` keeps hello
//! below 2). Every change works out what to install anew from the world
//! ([`crate::resolve`]), so a package installed only for another goes once
//! nothing needs it any more.
//!
//! The database is one JSON file, which the packages service replaces whole
//! with each change: its *generation* goes up by one each time, which is
//! how a journal tells whether the change it records was made
//! ([`crate::transaction`]). It is checked as it is read back: names once
//! each and in order, each path owned by one package alone, each a path a
//! package may install.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use serde::{Deserialize, Serialize};

use crate::archive::{Entry, check_path};
use crate::manifest::Manifest;
use crate::name::{Name, is_repository_name};
use crate::requirement::Requirement;
use crate::{Digest, limits};

/// The format this code reads and writes.
pub const FORMAT: u32 = 1;

/// An installed package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installed {
    pub manifest: Manifest,
    /// The repository it was installed from.
    pub repository: String,
    /// Its package file's SHA-256.
    pub sha256: Digest,
    /// When it was installed (Unix time, s).
    pub installed: u64,
    /// What it installed, sorted by path.
    pub files: Vec<Entry>,
}

impl Installed {
    /// Its files' sizes together.
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// The installed packages, and the world.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Database {
    pub format: u32,
    pub generation: u64,
    /// The packages asked for by name, each name once, in order.
    pub world: Vec<Requirement>,
    /// Sorted by name.
    pub packages: Vec<Installed>,
}

/// Why a database is not taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadDatabase(pub String);

impl fmt::Display for BadDatabase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the database of installed packages is damaged: {}", self.0)
    }
}

impl Default for Database {
    fn default() -> Database {
        Database { format: FORMAT, generation: 0, world: Vec::new(), packages: Vec::new() }
    }
}

impl Database {
    /// Nothing installed.
    pub fn new() -> Database {
        Database::default()
    }

    /// Reads and checks a database.
    pub fn parse(bytes: &[u8]) -> Result<Database, BadDatabase> {
        if bytes.len() > limits::DATABASE {
            return Err(BadDatabase(String::from("it is larger than a database may be")));
        }
        let db: Database = serde_json::from_slice(bytes).map_err(|e| BadDatabase(alloc::format!("{e}")))?;
        db.validate()?;
        Ok(db)
    }

    /// Checks what the types alone do not.
    pub fn validate(&self) -> Result<(), BadDatabase> {
        let bad = |why: String| Err(BadDatabase(why));
        if self.format != FORMAT {
            return bad(alloc::format!("it is of format {}, which this system does not read", self.format));
        }
        if self.world.len() > limits::WORLD {
            return bad(String::from("its world names more packages than it may"));
        }
        for (i, w) in self.world.iter().enumerate() {
            if i > 0 && self.world[i - 1].name >= w.name {
                return bad(alloc::format!("its world names {} out of order, or twice", w.name));
            }
        }
        let mut owners: BTreeMap<&str, &Name> = BTreeMap::new();
        for (i, p) in self.packages.iter().enumerate() {
            let name = &p.manifest.name;
            if i > 0 && self.packages[i - 1].manifest.name >= *name {
                return bad(alloc::format!("{name} is out of order, or there twice"));
            }
            p.manifest.validate().map_err(|e| BadDatabase(e.to_string()))?;
            if !is_repository_name(&p.repository) {
                return bad(alloc::format!("{name} names no repository"));
            }
            if p.files.len() > limits::FILES {
                return bad(alloc::format!("{name} has more files than a package may"));
            }
            for (j, f) in p.files.iter().enumerate() {
                check_path(&f.path).map_err(|why| BadDatabase(alloc::format!("{name}'s file {} {why}", f.path)))?;
                if j > 0 && p.files[j - 1].path >= f.path {
                    return bad(alloc::format!("{name}'s file {} is out of order, or there twice", f.path));
                }
                if let Some(other) = owners.insert(&f.path, name) {
                    return bad(alloc::format!("{} belongs to both {other} and {name}", f.path));
                }
            }
        }
        Ok(())
    }

    /// The database as the service keeps it.
    pub fn to_bytes(&self) -> Vec<u8> {
        crate::to_json(self)
    }

    /// The installed package `name`.
    pub fn get(&self, name: &str) -> Option<&Installed> {
        self.packages.binary_search_by(|p| p.manifest.name.as_str().cmp(name)).ok().map(|i| &self.packages[i])
    }

    /// The world's requirement of `name`, if the user asked for it.
    pub fn wanted(&self, name: &str) -> Option<&Requirement> {
        self.world.iter().find(|w| w.name.as_str() == name)
    }

    /// Which package each installed path belongs to.
    pub fn owners(&self) -> BTreeMap<&str, &Installed> {
        let mut owners = BTreeMap::new();
        for p in &self.packages {
            for f in &p.files {
                owners.insert(f.path.as_str(), p);
            }
        }
        owners
    }

    /// The package `path` belongs to.
    pub fn owner(&self, path: &str) -> Option<&Installed> {
        self.packages.iter().find(|p| p.files.binary_search_by(|f| f.path.as_str().cmp(path)).is_ok())
    }

    /// Puts `world` in the order the database keeps it, each name once (the
    /// last requirement of a name standing).
    pub fn tidy_world(world: Vec<Requirement>) -> Vec<Requirement> {
        let mut by_name: BTreeMap<Name, Requirement> = BTreeMap::new();
        for w in world {
            by_name.insert(w.name.clone(), w);
        }
        by_name.into_values().collect()
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::tests::manifest;

    fn installed(label: &str, files: &[&str]) -> Installed {
        Installed {
            manifest: manifest(label),
            repository: String::from("main"),
            sha256: Digest::of(label.as_bytes()),
            installed: 1_700_000_000,
            files: files
                .iter()
                .map(|p| Entry { path: p.to_string(), size: 3, sha256: Digest::of(p.as_bytes()) })
                .collect(),
        }
    }

    fn database() -> Database {
        Database {
            format: FORMAT,
            generation: 4,
            world: vec![Requirement::parse("hello").unwrap()],
            packages: vec![
                installed("hello 1.0", &["bin/hello", "share/hello/a"]),
                installed("libgreet 2.0", &["lib/g"]),
            ],
        }
    }

    #[test]
    fn the_database_reads_back_as_written() {
        let db = database();
        let back = Database::parse(&db.to_bytes()).unwrap();
        assert_eq!(back, db);
        assert_eq!(back.get("libgreet").unwrap().manifest.label(), "libgreet 2.0");
        assert!(back.get("nothing").is_none());
        assert_eq!(back.owner("share/hello/a").unwrap().manifest.name.as_str(), "hello");
        assert!(back.owner("share/hello").is_none());
        assert_eq!(back.owners().len(), 3);
        assert!(back.wanted("hello").is_some() && back.wanted("libgreet").is_none());
        assert_eq!(back.get("hello").unwrap().size(), 6);
        assert_eq!(Database::parse(&Database::new().to_bytes()).unwrap().generation, 0);
    }

    #[test]
    fn a_damaged_database_is_refused() {
        let mut twice = database();
        twice.packages[1].files[0].path = String::from("bin/hello");
        let mut unordered = database();
        unordered.packages.swap(0, 1);
        let mut bad_path = database();
        bad_path.packages[0].files[0].path = String::from("../etc/x");
        let mut world = database();
        world.world.push(Requirement::parse("hello (< 2)").unwrap());
        let mut newer = database();
        newer.format = 2;
        for (db, why) in [
            (twice, "belongs to both"),
            (unordered, "out of order"),
            (bad_path, "../etc/x"),
            (world, "out of order, or twice"),
            (newer, "format 2"),
        ] {
            let e = Database::parse(&db.to_bytes()).unwrap_err();
            assert!(e.to_string().contains(why), "{why}: {e}");
        }
        assert!(Database::parse(b"{}").is_err());
    }

    #[test]
    fn the_world_keeps_one_requirement_a_name() {
        let world = ["zed", "hello", "hello (< 2)"].iter().map(|r| Requirement::parse(r).unwrap()).collect();
        let tidy: Vec<String> = Database::tidy_world(world).iter().map(|r| r.to_string()).collect();
        assert_eq!(tidy, ["hello (< 2)", "zed"]);
    }
}

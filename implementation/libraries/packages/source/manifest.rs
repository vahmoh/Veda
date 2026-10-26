// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What a package says of itself: its name and version, the architecture
//! it is for, what it is (summary, description, licence, homepage), and
//! how it stands to other packages (what it depends on, conflicts with and
//! provides).
//!
//! A package carries its manifest as JSON ([`crate::archive`]), and a
//! repository's index repeats it for each package it offers. Whoever makes
//! a package writes it as `KEY=VALUE` lines ([`Manifest::from_text`]),
//! as an application's `.app` manifest is written:
//!
//! ```text
//! name=hello
//! version=1.0-1
//! architecture=amd64
//! summary=Says hello
//! description=A program that says hello, for trying packages out.
//! licence=GPL-3.0-only
//! homepage=https://www.vedaos.org
//! depends=libgreet (>= 1.2)
//! depends=veda (>= 0.1)
//! ```
//!
//! `depends`, `conflicts` and `provides` may be given any number of times,
//! one requirement each; `description` too, each line a line of it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use serde::{Deserialize, Serialize};

use crate::limits;
use crate::name::Name;
use crate::printable;
use crate::requirement::{Provision, Requirement};
use crate::version::Version;

/// The processor architecture a package's programs are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Architecture {
    /// AMD64 (x86-64).
    Amd64,
    /// None: the package holds no programs (data, fonts, documents,
    /// scripts), and installs on every architecture.
    Any,
}

impl Architecture {
    /// The architecture this code is compiled for, which a system installs
    /// packages of (and of [`Architecture::Any`]).
    #[cfg(target_arch = "x86_64")]
    pub const NATIVE: Architecture = Architecture::Amd64;

    pub fn as_str(self) -> &'static str {
        match self {
            Architecture::Amd64 => "amd64",
            Architecture::Any => "any",
        }
    }

    /// Reads an architecture's name.
    pub fn parse(text: &str) -> Option<Architecture> {
        match text {
            "amd64" => Some(Architecture::Amd64),
            "any" => Some(Architecture::Any),
            _ => None,
        }
    }

    /// Whether a system of architecture `system` installs packages of this
    /// one.
    pub fn installs_on(self, system: Architecture) -> bool {
        self == Architecture::Any || self == system
    }
}

impl fmt::Display for Architecture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A package's manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: Name,
    pub version: Version,
    pub architecture: Architecture,
    /// What it is, in one line.
    pub summary: String,
    /// What it is, at more length (may be empty).
    pub description: String,
    /// Its licence, as an SPDX expression (`GPL-3.0-only`, `MIT OR
    /// Apache-2.0`).
    pub licence: String,
    /// Where to read more of it (may be empty).
    pub homepage: String,
    pub depends: Vec<Requirement>,
    pub conflicts: Vec<Requirement>,
    pub provides: Vec<Provision>,
}

/// Why a manifest is not a valid one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadManifest(pub String);

impl fmt::Display for BadManifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `text` is a short text of at most `max` bytes, on one line.
fn short(text: &str, max: usize) -> bool {
    text.len() <= max && printable(text)
}

impl Manifest {
    /// Checks what the types alone do not: the texts' lengths and
    /// characters, the number of relations, and that the package names
    /// itself in none of them (it would depend on, or conflict with,
    /// itself).
    pub fn validate(&self) -> Result<(), BadManifest> {
        let bad = |what: String| Err(BadManifest(alloc::format!("the manifest of {}: {what}", self.name)));
        if self.summary.is_empty() || !short(&self.summary, limits::SUMMARY) {
            return bad(alloc::format!("its summary is one line of 1 to {} bytes", limits::SUMMARY));
        }
        if self.description.len() > limits::DESCRIPTION || self.description.chars().any(|c| c.is_control() && c != '\n')
        {
            return bad(alloc::format!("its description is text of at most {} bytes", limits::DESCRIPTION));
        }
        if self.licence.is_empty() || !short(&self.licence, limits::SHORT_TEXT) {
            return bad(String::from("it names its licence (an SPDX expression, such as GPL-3.0-only)"));
        }
        if !short(&self.homepage, limits::SHORT_TEXT)
            || !(self.homepage.is_empty()
                || self.homepage.starts_with("https://")
                || self.homepage.starts_with("http://"))
        {
            return bad(String::from("its homepage is a web address (http:// or https://), or nothing"));
        }
        for (what, count) in
            [("depends", self.depends.len()), ("conflicts", self.conflicts.len()), ("provides", self.provides.len())]
        {
            if count > limits::RELATIONS {
                return bad(alloc::format!("it has more than {} entries of {what}", limits::RELATIONS));
            }
        }
        let names = self.depends.iter().chain(&self.conflicts).map(|r| &r.name);
        if names.chain(self.provides.iter().map(|p| &p.name)).any(|n| *n == self.name) {
            return bad(String::from("it names itself among what it depends on, conflicts with or provides"));
        }
        Ok(())
    }

    /// Reads a manifest written as `KEY=VALUE` lines (see the module's
    /// documentation), and checks it.
    pub fn from_text(text: &str) -> Result<Manifest, BadManifest> {
        let bad = |line: usize, what: String| Err(BadManifest(alloc::format!("line {line}: {what}")));
        let mut fields: [Option<String>; 6] = Default::default();
        const SINGLE: [&str; 6] = ["name", "version", "architecture", "summary", "licence", "homepage"];
        let mut description: Vec<String> = Vec::new();
        let (mut depends, mut conflicts, mut provides) = (Vec::new(), Vec::new(), Vec::new());
        for (i, line) in text.lines().enumerate() {
            let number = i + 1;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return bad(number, String::from("each line is KEY=VALUE"));
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "description" => description.push(value.to_string()),
                "depends" | "conflicts" => {
                    let req = match Requirement::parse(value) {
                        Ok(r) => r,
                        Err(e) => return bad(number, e.to_string()),
                    };
                    if key == "depends" { depends.push(req) } else { conflicts.push(req) }
                }
                "provides" => match Provision::parse(value) {
                    Ok(p) => provides.push(p),
                    Err(e) => return bad(number, e.to_string()),
                },
                _ => match SINGLE.iter().position(|k| *k == key) {
                    Some(at) if fields[at].is_some() => return bad(number, alloc::format!("{key} is given twice")),
                    Some(at) => fields[at] = Some(value.to_string()),
                    None => return bad(number, alloc::format!("no manifest has {key}")),
                },
            }
        }
        let [name, version, architecture, summary, licence, homepage] = fields;
        let missing = |key: &str| BadManifest(alloc::format!("the manifest gives no {key}"));
        let name = Name::parse(&name.ok_or_else(|| missing("name"))?).map_err(|e| BadManifest(e.to_string()))?;
        let version =
            Version::parse(&version.ok_or_else(|| missing("version"))?).map_err(|e| BadManifest(e.to_string()))?;
        let architecture = architecture.ok_or_else(|| missing("architecture"))?;
        let architecture = Architecture::parse(&architecture)
            .ok_or_else(|| BadManifest(alloc::format!("'{architecture}' is not an architecture: amd64 or any")))?;
        let manifest = Manifest {
            name,
            version,
            architecture,
            summary: summary.ok_or_else(|| missing("summary"))?,
            description: description.join("\n"),
            licence: licence.ok_or_else(|| missing("licence"))?,
            homepage: homepage.unwrap_or_default(),
            depends,
            conflicts,
            provides,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// The package's name and version, as people read them:
    /// `hello 1.0-1`.
    pub fn label(&self) -> String {
        alloc::format!("{} {}", self.name, self.version)
    }

    /// Whether `requirement` is met by this package, by its name or by a
    /// name it provides.
    pub fn meets(&self, requirement: &Requirement) -> bool {
        requirement.met_by(&self.name, &self.version) || self.provides.iter().any(|p| requirement.met_by_provision(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO: &str = "\
# A test package.
name=hello
version=1.0-1
architecture=amd64
summary=Says hello
description=A program that says hello,
description=for trying packages out.
licence=GPL-3.0-only
homepage=https://www.vedaos.org
depends=libgreet (>= 1.2)
depends = veda (>= 0.1)
provides=greeter (= 1.0)
";

    #[test]
    fn manifests_are_written_as_lines() {
        let m = Manifest::from_text(HELLO).unwrap();
        assert_eq!(m.label(), "hello 1.0-1");
        assert_eq!(m.architecture, Architecture::Amd64);
        assert_eq!(m.description, "A program that says hello,\nfor trying packages out.");
        assert_eq!(m.depends.len(), 2);
        assert_eq!(m.depends[1].to_string(), "veda (>= 0.1)");
        assert_eq!(m.provides[0].to_string(), "greeter (= 1.0)");
        assert!(m.meets(&Requirement::parse("hello (>= 1)").unwrap()));
        assert!(m.meets(&Requirement::parse("greeter (= 1.0)").unwrap()));
        assert!(!m.meets(&Requirement::parse("greeter (>= 2)").unwrap()));
    }

    #[test]
    fn manifests_travel_as_json() {
        let m = Manifest::from_text(HELLO).unwrap();
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.starts_with("{\"name\":\"hello\",\"version\":\"1.0-1\",\"architecture\":\"amd64\""), "{json}");
        assert_eq!(serde_json::from_str::<Manifest>(&json).unwrap(), m);
        // Nothing unknown is passed over: a field that a newer format
        // added would change what the package means.
        let extra = json.replacen('{', "{\"scripts\":[],", 1);
        assert!(serde_json::from_str::<Manifest>(&extra).is_err());
        let missing = json.replace(",\"homepage\":\"https://www.vedaos.org\"", "");
        assert!(serde_json::from_str::<Manifest>(&missing).is_err());
    }

    #[test]
    fn manifests_say_what_is_wrong() {
        for (change, why) in [
            ("name=hello\n", "is given twice"),
            ("colour=red\n", "no manifest has colour"),
            ("just words\n", "KEY=VALUE"),
            ("depends=libgreet >= 1\n", "constraints in parentheses"),
            ("provides=x (>= 1)\n", "names a version with ="),
            ("depends=hello\n", "names itself"),
        ] {
            let e = Manifest::from_text(&alloc::format!("{HELLO}{change}")).unwrap_err();
            assert!(e.to_string().contains(why), "{change}: {e}");
        }
        for (drop, why) in [
            ("name=hello\n", "gives no name"),
            ("version=1.0-1\n", "gives no version"),
            ("summary=Says hello\n", "gives no summary"),
            ("licence=GPL-3.0-only\n", "gives no licence"),
        ] {
            let e = Manifest::from_text(&HELLO.replace(drop, "")).unwrap_err();
            assert!(e.to_string().contains(why), "{drop}: {e}");
        }
        for (from, to, why) in [
            ("architecture=amd64", "architecture=arm", "not an architecture"),
            ("homepage=https://www.vedaos.org", "homepage=ftp://x", "web address"),
            ("summary=Says hello", "summary=", "summary is one line"),
            ("version=1.0-1", "version=one", "not a version"),
        ] {
            let e = Manifest::from_text(&HELLO.replace(from, to)).unwrap_err();
            assert!(e.to_string().contains(why), "{to}: {e}");
        }
        let mut m = Manifest::from_text(HELLO).unwrap();
        m.summary = String::from("two\nlines");
        assert!(m.validate().is_err());
        m.summary = "x".repeat(limits::SUMMARY + 1);
        assert!(m.validate().is_err());
    }

    #[test]
    fn architectures_install_where_they_run() {
        assert!(Architecture::Amd64.installs_on(Architecture::Amd64));
        assert!(Architecture::Any.installs_on(Architecture::Amd64));
        assert!(!Architecture::Amd64.installs_on(Architecture::Any));
        assert_eq!(Architecture::parse("any"), Some(Architecture::Any));
        assert_eq!(Architecture::NATIVE.as_str(), "amd64");
    }
}

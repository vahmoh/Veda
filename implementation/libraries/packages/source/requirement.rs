// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What packages ask of one another.
//!
//! * A [`Requirement`] names a package and, in parentheses, constraints
//!   on its version, all of which must hold: `libgreet`,
//!   `libgreet (>= 1.2)`, `libgreet (>= 1.2, < 2)`. Packages depend on
//!   requirements, conflict with them, and the world (the packages the
//!   user asked for) is a list of them.
//! * A [`Provision`] is a name a package answers to besides its own, with
//!   a version or without: `greeter`, `greeter (= 1.0)`.
//!
//! A requirement is met by a package of its name whose version meets every
//! constraint, or by a package that provides its name: with a version that
//! meets them, or, for a requirement without constraints, with none (a
//! provision without a version tells nothing a constraint could check, as
//! in Debian).

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use crate::limits;
use crate::name::Name;
use crate::version::Version;

/// How a version compares with the one a constraint names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Op {
    Less,
    LessOrEqual,
    Equal,
    GreaterOrEqual,
    Greater,
}

impl Op {
    fn symbol(self) -> &'static str {
        match self {
            Op::Less => "<",
            Op::LessOrEqual => "<=",
            Op::Equal => "=",
            Op::GreaterOrEqual => ">=",
            Op::Greater => ">",
        }
    }
}

/// One condition on a version: `>= 1.2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraint {
    pub op: Op,
    pub version: Version,
}

impl Constraint {
    /// Whether `version` meets it.
    pub fn allows(&self, version: &Version) -> bool {
        let order = version.cmp(&self.version);
        match self.op {
            Op::Less => order.is_lt(),
            Op::LessOrEqual => order.is_le(),
            Op::Equal => order.is_eq(),
            Op::GreaterOrEqual => order.is_ge(),
            Op::Greater => order.is_gt(),
        }
    }
}

impl fmt::Display for Constraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.op.symbol(), self.version)
    }
}

/// A package, and what its version must be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub name: Name,
    /// All must hold; none: any version.
    pub constraints: Vec<Constraint>,
}

/// A name a package answers to besides its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provision {
    pub name: Name,
    pub version: Option<Version>,
}

/// Why a requirement or provision is not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadRequirement {
    pub text: String,
    pub why: String,
}

impl fmt::Display for BadRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "'{}' is not a requirement: {}", self.text.chars().take(120).collect::<String>(), self.why)
    }
}

/// Splits `NAME [(...)]` into the name and what the parentheses hold.
fn split(text: &str) -> Result<(Name, Option<&str>), BadRequirement> {
    let bad = |why: &str| BadRequirement { text: text.to_string(), why: why.to_string() };
    let text = text.trim();
    let (name, rest) = match text.find(|c: char| c == '(' || c.is_whitespace()) {
        Some(i) => (&text[..i], text[i..].trim_start()),
        None => (text, ""),
    };
    let name = Name::parse(name).map_err(|e| bad(&e.to_string()))?;
    if rest.is_empty() {
        return Ok((name, None));
    }
    let inner = rest
        .strip_prefix('(')
        .and_then(|r| r.strip_suffix(')'))
        .ok_or_else(|| bad("what follows the name must be constraints in parentheses, such as (>= 1.2)"))?;
    if inner.contains(['(', ')']) {
        return Err(bad("parentheses do not nest"));
    }
    Ok((name, Some(inner)))
}

/// Reads one constraint: an operator, then a version.
fn constraint(text: &str) -> Result<Constraint, String> {
    let text = text.trim();
    let (op, rest) =
        [("<=", Op::LessOrEqual), (">=", Op::GreaterOrEqual), ("<", Op::Less), (">", Op::Greater), ("=", Op::Equal)]
            .iter()
            .find_map(|(symbol, op)| text.strip_prefix(symbol).map(|rest| (*op, rest)))
            .ok_or_else(|| String::from("each constraint begins with <, <=, =, >= or >"))?;
    let version = Version::parse(rest.trim()).map_err(|e| e.to_string())?;
    Ok(Constraint { op, version })
}

impl Requirement {
    /// Reads `NAME`, or `NAME (OP VERSION[, OP VERSION]...)`.
    pub fn parse(text: &str) -> Result<Requirement, BadRequirement> {
        let (name, inner) = split(text)?;
        let mut constraints = Vec::new();
        if let Some(inner) = inner {
            for part in inner.split(',') {
                let c = constraint(part).map_err(|why| BadRequirement { text: text.to_string(), why })?;
                constraints.push(c);
            }
            if constraints.len() > limits::CONSTRAINTS {
                return Err(BadRequirement {
                    text: text.to_string(),
                    why: alloc::format!("a requirement has at most {} constraints", limits::CONSTRAINTS),
                });
            }
        }
        Ok(Requirement { name, constraints })
    }

    /// A requirement of `name`, of any version.
    pub fn any(name: Name) -> Requirement {
        Requirement { name, constraints: Vec::new() }
    }

    /// Whether `version` meets every constraint.
    pub fn allows(&self, version: &Version) -> bool {
        self.constraints.iter().all(|c| c.allows(version))
    }

    /// Whether the package `name` at `version` meets the requirement.
    pub fn met_by(&self, name: &Name, version: &Version) -> bool {
        *name == self.name && self.allows(version)
    }

    /// Whether `provision` meets the requirement.
    pub fn met_by_provision(&self, provision: &Provision) -> bool {
        provision.name == self.name
            && match &provision.version {
                Some(v) => self.allows(v),
                None => self.constraints.is_empty(),
            }
    }
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name.as_str())?;
        for (i, c) in self.constraints.iter().enumerate() {
            f.write_str(if i == 0 { " (" } else { ", " })?;
            write!(f, "{c}")?;
        }
        if !self.constraints.is_empty() {
            f.write_str(")")?;
        }
        Ok(())
    }
}

impl Provision {
    /// Reads `NAME` or `NAME (= VERSION)`.
    pub fn parse(text: &str) -> Result<Provision, BadRequirement> {
        let (name, inner) = split(text)?;
        let version = match inner {
            None => None,
            Some(inner) => match constraint(inner) {
                Ok(Constraint { op: Op::Equal, version }) => Some(version),
                _ => {
                    return Err(BadRequirement {
                        text: text.to_string(),
                        why: String::from("a provision names a version with = alone, such as (= 1.0)"),
                    });
                }
            },
        };
        Ok(Provision { name, version })
    }
}

impl fmt::Display for Provision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.version {
            Some(v) => write!(f, "{} (= {v})", self.name),
            None => f.write_str(self.name.as_str()),
        }
    }
}

macro_rules! as_string {
    ($t:ty) => {
        impl serde::Serialize for $t {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(self)
            }
        }

        impl<'de> serde::Deserialize<'de> for $t {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<$t, D::Error> {
                let text: &str = serde::Deserialize::deserialize(d)?;
                <$t>::parse(text).map_err(serde::de::Error::custom)
            }
        }
    };
}

as_string!(Requirement);
as_string!(Provision);

#[cfg(test)]
mod tests {
    use super::*;

    fn r(text: &str) -> Requirement {
        Requirement::parse(text).unwrap()
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn n(text: &str) -> Name {
        Name::parse(text).unwrap()
    }

    #[test]
    fn requirements_read_and_write_alike() {
        for (text, written) in [
            ("libgreet", "libgreet"),
            ("  libgreet  ", "libgreet"),
            ("libgreet (>= 1.2)", "libgreet (>= 1.2)"),
            ("libgreet(>=1.2,<2)", "libgreet (>= 1.2, < 2)"),
            ("a (= 1:1.0-1)", "a (= 1:1.0-1)"),
            ("a (<= 3, > 1, != 2)", ""),
        ] {
            match Requirement::parse(text) {
                Ok(req) => assert_eq!(req.to_string(), written, "{text}"),
                Err(e) => assert!(written.is_empty(), "{text}: {e}"),
            }
        }
    }

    #[test]
    fn malformed_requirements_say_why() {
        for (text, why) in [
            ("", "not a package's name"),
            ("Libgreet", "not a package's name"),
            ("a >= 1", "constraints in parentheses"),
            ("a (>= 1", "constraints in parentheses"),
            ("a ((>= 1))", "do not nest"),
            ("a (1.0)", "each constraint begins with"),
            ("a (>= x)", "not a version"),
            ("a ()", "each constraint begins with"),
            ("a (>1, >2, >3, >4, >5)", "at most 4 constraints"),
        ] {
            let e = Requirement::parse(text).unwrap_err();
            assert!(e.to_string().contains(why), "{text}: {e}");
        }
    }

    #[test]
    fn constraints_bound_versions() {
        let req = r("libgreet (>= 1.2, < 2)");
        for (version, allowed) in [("1.1", false), ("1.2", true), ("1.10", true), ("2~rc1", true), ("2", false)] {
            assert_eq!(req.allows(&v(version)), allowed, "{version}");
        }
        assert!(req.met_by(&n("libgreet"), &v("1.5")));
        assert!(!req.met_by(&n("libgreeter"), &v("1.5")));
        assert!(r("a").allows(&v("0")));
        for (op, version, allowed) in [("<", "1", false), ("<=", "1", true), ("=", "1.0", false), (">", "0.9", true)] {
            assert_eq!(r(&alloc::format!("a ({op} {version})")).allows(&v("1")), allowed, "{op} {version}");
        }
    }

    #[test]
    fn provisions_meet_requirements_by_their_versions() {
        let versioned = Provision::parse("greeter (= 1.5)").unwrap();
        let bare = Provision::parse("greeter").unwrap();
        assert_eq!(versioned.to_string(), "greeter (= 1.5)");
        assert!(r("greeter").met_by_provision(&versioned) && r("greeter").met_by_provision(&bare));
        assert!(r("greeter (>= 1)").met_by_provision(&versioned));
        assert!(!r("greeter (>= 2)").met_by_provision(&versioned));
        // A provision without a version tells nothing a constraint checks.
        assert!(!r("greeter (>= 1)").met_by_provision(&bare));
        assert!(!r("other").met_by_provision(&versioned));
        for bad in ["greeter (>= 1)", "greeter (= 1, = 2)", "Greeter"] {
            assert!(Provision::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn requirements_travel_as_strings() {
        let req = r("libgreet (>= 1.2, < 2)");
        let text = serde_json::to_string(&req).unwrap();
        assert_eq!(text, "\"libgreet (>= 1.2, < 2)\"");
        assert_eq!(serde_json::from_str::<Requirement>(&text).unwrap(), req);
        assert!(serde_json::from_str::<Requirement>("\"a (\"").is_err());
    }
}

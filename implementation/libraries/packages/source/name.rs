// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What packages and repositories are called.
//!
//! A package's name is 1 to 64 bytes of lower-case ASCII letters, digits
//! and `+`, `-`, `.`, beginning with a letter or digit (`libgreet`,
//! `gtk+`, `python3.15`): no two names differ only in case, and every name
//! can stand in a file's name. A repository's name is the same, without
//! `+` and `.`, and at most 32 bytes (`system`, `veda-testing`).

use alloc::string::{String, ToString};
use core::fmt;

use crate::limits;

/// A package's name.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(String);

/// Why a name is not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadName(pub String);

impl fmt::Display for BadName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "'{}' is not a package's name: it takes 1 to {} lower-case letters, digits, '+', '-' and '.', \
             beginning with a letter or digit",
            self.0.chars().take(80).collect::<String>(),
            limits::NAME
        )
    }
}

impl Name {
    /// Checks `text` as a package's name.
    pub fn parse(text: &str) -> Result<Name, BadName> {
        let bytes = text.as_bytes();
        let valid = !bytes.is_empty()
            && bytes.len() <= limits::NAME
            && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
            && bytes.iter().all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.'));
        if valid { Ok(Name(text.to_string())) } else { Err(BadName(text.to_string())) }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl core::borrow::Borrow<str> for Name {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl serde::Serialize for Name {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Name {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Name, D::Error> {
        let text: &str = serde::Deserialize::deserialize(d)?;
        Name::parse(text).map_err(serde::de::Error::custom)
    }
}

/// The longest name of a repository.
pub const REPOSITORY_NAME: usize = 32;

/// Whether `text` is a repository's name: 1 to [`REPOSITORY_NAME`] bytes of
/// lower-case letters, digits and `-`, beginning with a letter or digit.
pub fn is_repository_name(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= REPOSITORY_NAME
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_lower_case_and_short() {
        for good in ["a", "libgreet", "gtk+", "python3.15", "0ad", "x-y.z+1"] {
            assert_eq!(Name::parse(good).unwrap().as_str(), good);
        }
        let long = "a".repeat(limits::NAME + 1);
        for bad in ["", "Hello", "-x", ".x", "+x", "a b", "a/b", "a_b", "../x", "é", long.as_str()] {
            assert!(Name::parse(bad).is_err(), "{bad:?}");
        }
        assert!(Name::parse(&"a".repeat(limits::NAME)).is_ok());
        assert!(BadName("Hello".into()).to_string().contains("'Hello' is not a package's name"));
    }

    #[test]
    fn repositories_have_plainer_names() {
        for good in ["system", "veda-testing", "r2"] {
            assert!(is_repository_name(good), "{good}");
        }
        for bad in ["", "System", "-x", "a.b", "a+b", "a b", &"r".repeat(REPOSITORY_NAME + 1)] {
            assert!(!is_repository_name(bad), "{bad:?}");
        }
    }
}

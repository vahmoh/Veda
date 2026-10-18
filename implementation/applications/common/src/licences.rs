// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The licences of the system's parts, as `/system/licences/README` lists
//! them (the image build writes it): each part's name, version, licence,
//! what it is in the system and where its texts are. About Veda shows them,
//! and Settings lists them.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

/// The list.
pub const README: &str = "/system/licences/README";

/// What the system says before it lists the parts: Veda's copyright and
/// licence, that it comes with no warranty, and where the texts are.
pub const NOTICE: &str = "Veda © 2026 Vahid Mohammadi is free software: you can redistribute it and/or modify it \
                          under the terms of the GNU General Public License, version 3 (in /system/LICENSE.txt). It \
                          comes with ABSOLUTELY NO WARRANTY. Its other parts keep their own licences, whose texts \
                          are in /system/licences:";

/// A part of the system and its licence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    /// Its version; empty if it has none (a part of many).
    pub version: String,
    /// Its licence, as SPDX writes it (in words, for a part of many).
    pub licence: String,
    /// What it is in the system.
    pub what: String,
    /// Where its texts are: a file, or a directory of them.
    pub texts: String,
}

impl Part {
    /// Its name with its version.
    pub fn title(&self) -> String {
        if self.version.is_empty() { self.name.clone() } else { format!("{} {}", self.name, self.version) }
    }
}

/// The parts `text` lists: each a `[name]` line, then `key = value` lines
/// (`version`, `licence`, `what`, `texts`); `#` lines are comments, and
/// other keys are left for later lists. A part without texts is left out.
pub fn parse(text: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut part: Option<Part> = None;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            parts.extend(part.take().filter(|p| !p.texts.is_empty()));
            part = Some(Part { name: name.trim().to_string(), ..Part::default() });
            continue;
        }
        let (Some(p), Some((key, value))) = (part.as_mut(), line.split_once('=')) else { continue };
        let value = value.trim().to_string();
        match key.trim() {
            "version" => p.version = value,
            "licence" => p.licence = value,
            "what" => p.what = value,
            "texts" => p.texts = value,
            _ => {}
        }
    }
    parts.extend(part.filter(|p| !p.texts.is_empty()));
    parts
}

/// What a system without the list has: Veda's licence, and its fonts'.
pub fn fallback() -> Vec<Part> {
    vec![
        Part {
            name: "Veda".into(),
            version: String::new(),
            licence: "GPL-3.0-only".into(),
            what: "The operating system".into(),
            texts: "/system/LICENSE.txt".into(),
        },
        Part {
            name: "Fonts".into(),
            version: String::new(),
            licence: "OFL-1.1".into(),
            what: "Inter (© The Inter Project Authors), Lato (© tyPoland Łukasz Dziedzic) and JetBrains Mono (© The \
                   JetBrains Mono Project Authors)"
                .into(),
            texts: "/system/fonts/OFL.txt".into(),
        },
    ]
}

/// `files`, the paths of a part's texts in its directory, in the order to
/// show them: its `README` first (a list of what the others are), then by
/// path.
pub fn ordered(mut files: Vec<String>) -> Vec<String> {
    files.sort_by(|a, b| (a != "README", a).cmp(&(b != "README", b)));
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_gives_each_part() {
        let text = "# The licences.\n\n[Veda]\nversion = 0.1.0\nlicence = GPL-3.0-only\nwhat = The system\n\
                    texts = /system/LICENSE.txt\n\n[Rust crates]\nlicence = each crate's: MIT, Zlib\n\
                    what = The crates = many\nmaintainer = someone\ntexts = /system/licences/crates\n\n\
                    [Nothing]\nlicence = MIT\n";
        let parts = parse(text);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert_eq!(parts[0].title(), "Veda 0.1.0");
        assert_eq!(parts[0].texts, "/system/LICENSE.txt");
        // A part of many has no version; a value may hold `=`.
        assert_eq!(parts[1].title(), "Rust crates");
        assert_eq!(parts[1].what, "The crates = many");
        assert_eq!(parts[1].licence, "each crate's: MIT, Zlib");
        assert!(parse("").is_empty());
        assert_eq!(fallback()[0].texts, "/system/LICENSE.txt");
    }

    #[test]
    fn a_parts_list_comes_first() {
        let files = ["SOURCE", "b/LICENSE", "README", "COPYING"].map(String::from).to_vec();
        assert_eq!(ordered(files), ["README", "COPYING", "SOURCE", "b/LICENSE"]);
    }
}

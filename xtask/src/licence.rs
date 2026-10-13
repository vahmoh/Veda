// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The licence header every file of Veda's own starts with
//! (documentation/CODING.md): its copyright holders, then its licence, as
//! SPDX tags in the file's comment syntax. Its test holds the repository to
//! it.

use std::process::Command;

use crate::util;

/// The licences of Veda's files: its own, and the MIT licence of the code
/// that Mesa's build compiles with Mesa (the driver VM's renderer).
const LICENCES: &[&str] = &["GPL-3.0-only", "MIT"];

/// Whether the file at `path` (from the repository's root) goes without a
/// header: one in a format without comments (images, fonts, videos, test
/// vectors, JSON, the website's domain), Cargo's `Cargo.lock`, a licence's
/// text, or the ports' patches, which change other projects' code.
fn exempt(path: &str) -> bool {
    const BINARY: &[&str] = &["png", "jpg", "ico", "ttf", "otf", "woff2", "mkv", "mp4", "webm", "der", "sig", "bin"];
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(_, e)| BINARY.contains(&e) || e == "webmanifest" || e == "patch")
        || ["Cargo.lock", "CNAME", "LICENSE", "OFL.txt"].contains(&name)
}

/// Checks the header at the top of `text`: after what has to come first (a
/// script's `#!` line, an HTML page's doctype, an XML declaration, a `/*`
/// alone on its line), one or more copyright lines, then the licence's.
fn check(text: &str) -> Result<(), String> {
    let first = |l: &&str| {
        l.starts_with("#!")
            || l.get(..9).is_some_and(|d| d.eq_ignore_ascii_case("<!doctype"))
            || l.starts_with("<?xml")
            || *l == "/*"
    };
    let mut holders = 0;
    for line in text.lines().skip_while(first) {
        if tag(line, "SPDX-FileCopyrightText:").is_some_and(|holder| !holder.is_empty()) {
            holders += 1;
            continue;
        }
        if holders == 0 {
            break;
        }
        return match tag(line, "SPDX-License-Identifier:") {
            Some(licence) if LICENCES.contains(&licence) => Ok(()),
            Some(licence) => Err(format!("is under {licence}, none of Veda's licences ({})", LICENCES.join(", "))),
            None => Err("has no SPDX-License-Identifier line after its copyright".into()),
        };
    }
    Err(if holders == 0 { "does not start with an SPDX-FileCopyrightText line" } else { "ends in its header" }.into())
}

/// The value of the SPDX tag `name` on `line`, if the line is a comment
/// that holds it: nothing but comment marks and spaces before it, and the
/// end of a comment after it left out.
fn tag<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let (before, after) = line.split_once(name)?;
    let value = after.trim().trim_end_matches("-->").trim_end_matches("*/").trim_end();
    before.chars().all(|c| " /*#<!-".contains(c)).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_read_in_every_comment_syntax() {
        let ok = [
            "// SPDX-FileCopyrightText: © 2026 A\n// SPDX-License-Identifier: GPL-3.0-only\n\n//! A module.\n",
            "#!/bin/sh\n# SPDX-FileCopyrightText: © 2026 A\n# SPDX-License-Identifier: GPL-3.0-only\n",
            "/*\n * SPDX-FileCopyrightText: © 2026 A\n * SPDX-FileCopyrightText: © 2027 B\n * SPDX-License-Identifier: MIT\n */",
            "/* SPDX-FileCopyrightText: © 2026 A\n   SPDX-License-Identifier: GPL-3.0-only */\n",
            "<!doctype html>\n<!-- SPDX-FileCopyrightText: © 2026 A -->\n<!-- SPDX-License-Identifier: GPL-3.0-only -->\n",
        ];
        for text in ok {
            assert_eq!(check(text), Ok(()), "{text}");
        }
        let bad = [
            ("", "does not start"),
            ("fn main() {}\n// SPDX-FileCopyrightText: © 2026 A\n", "does not start"),
            ("// SPDX-FileCopyrightText:\n// SPDX-License-Identifier: MIT\n", "does not start"),
            ("// SPDX-FileCopyrightText: © 2026 A\n\n// SPDX-License-Identifier: MIT\n", "no SPDX-License-Identifier"),
            ("// SPDX-FileCopyrightText: © 2026 A\n// SPDX-License-Identifier: GPL-3.0\n", "none of Veda's"),
            ("// SPDX-FileCopyrightText: © 2026 A\n", "ends in its header"),
            ("let a = \"SPDX-FileCopyrightText: © 2026 A\";\n", "does not start"),
        ];
        for (text, why) in bad {
            assert!(check(text).is_err_and(|e| e.contains(why)), "{text}: {:?}", check(text));
        }
    }

    #[test]
    fn what_cannot_carry_a_header_goes_without() {
        for path in ["documentation/images/a.png", "Cargo.lock", "LICENSE", "ports/gcc/veda.patch"] {
            assert!(exempt(path), "{path}");
        }
        for path in
            ["kernel/src/main.rs", "ports/crates.sh", "ports/linux/veda.config", "documentation/website/index.html"]
        {
            assert!(!exempt(path), "{path}");
        }
    }

    /// Every file in the repository, and every one about to be added to it
    /// (not ignored by Git), has its header.
    #[test]
    fn every_file_starts_with_its_licence() {
        let root = util::workspace_root();
        let listed = Command::new("git")
            .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
            .current_dir(&root)
            .output();
        // Outside a Git checkout (a source archive) there is no list.
        let Ok(listed) = listed else { return };
        if !listed.status.success() {
            return;
        }
        let files = String::from_utf8(listed.stdout).expect("paths in UTF-8");
        let mut wrong = Vec::new();
        for path in files.split('\0').filter(|p| !p.is_empty() && !exempt(p)) {
            // Deleted, but not yet in Git's index.
            let Ok(data) = std::fs::read(root.join(path)) else { continue };
            match String::from_utf8(data) {
                Ok(text) => {
                    if let Err(why) = check(&text) {
                        wrong.push(format!("{path} {why}"));
                    }
                }
                Err(_) => wrong.push(format!("{path} is not text: add its format to those without comments")),
            }
        }
        assert!(
            wrong.is_empty(),
            "files without their licence header (documentation/CODING.md):\n{}",
            wrong.join("\n")
        );
    }
}

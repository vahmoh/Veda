// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The applications the launcher lists and starts, each described by a
//! manifest, `<id>.app`:
//!
//! ```text
//! name=Text Editor
//! exe=/system/bin/editor
//! icon=document
//! category=Productivity
//! description=Write and edit text documents
//! pinned=true
//! taskbar=false
//! ```
//!
//! `pinned` puts the app in the start menu's pinned grid, `taskbar` on the
//! taskbar (both off unless given); a line that begins with `#` is a
//! comment.
//!
//! The system image's applications are in its `apps/` ([`system`]). Those
//! of the installed packages are in `/home/packages/apps` ([`packaged`]):
//! the launcher lists them beside the system's, but never in their place,
//! since services decide what a program may do by its identity, whose
//! name is its application's id.
//!
//! * **Their ids are their own.** An id is 1 to 64 bytes of lower-case
//!   letters, digits and `+`, `-`, `.`, beginning with a letter or digit,
//!   as a package's name is, and no name of the system's: neither a system
//!   application's id nor the name of a program in the system image (a
//!   service's, a command's), which the system's own programs run under.
//!   A package's `terminal.app` is no Terminal.
//! * **Their programs are installed ones.** A program (`exe`, by default
//!   `/home/packages/bin/<id>`) is a plain path in `/home/packages` or in
//!   `/system` ([`program`]): nothing that another program could have put
//!   anywhere else.
//! * **The start menu's pinned grid and the taskbar stay as the system
//!   image has them.** A package's application is in the start menu's
//!   list and its search; its `pinned` and `taskbar` are left out.
//! * **Bounded.** At most [`MAX_PACKAGED`] are listed, each from a
//!   manifest of [`MAX_MANIFEST`] bytes at most, its values rid of control
//!   characters and cut short ([`MAX_NAME`], [`MAX_DESCRIPTION`],
//!   [`MAX_EXE`]), so that the list of every application fits one message
//!   (`launcher::apps`) and shows as plain text wherever it is shown.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use proto::init::AppInfo;

/// Where the installed packages' files are, and their applications'
/// manifests.
pub const PACKAGES: &str = "/home/packages";
pub const PACKAGES_APPS: &str = "/home/packages/apps";

/// The most applications of installed packages the launcher lists.
pub const MAX_PACKAGED: usize = 64;
/// The largest manifest of an installed package's application read, in
/// bytes.
pub const MAX_MANIFEST: usize = 4096;
/// The longest id of an installed package's application, in bytes: a
/// package's name's longest.
pub const MAX_ID: usize = 64;
/// The longest program path of an installed package's application, in
/// bytes.
pub const MAX_EXE: usize = 256;
/// What the other values of an installed package's application are cut
/// to, in bytes: its name, icon and category, and its description.
pub const MAX_NAME: usize = 64;
pub const MAX_DESCRIPTION: usize = 256;

/// The application `id` of manifest `text`, whose program is `exe` unless
/// the manifest names another.
fn parse(id: &str, text: &str, exe: String) -> AppInfo {
    let mut app = AppInfo {
        id: id.to_string(),
        name: id.to_string(),
        exe,
        icon: id.to_string(),
        category: "Applications".to_string(),
        description: String::new(),
        pinned: false,
        taskbar: false,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim().to_string();
        match k.trim() {
            "name" => app.name = v,
            "exe" => app.exe = v,
            "icon" => app.icon = v,
            "category" => app.category = v,
            "description" => app.description = v,
            "pinned" => app.pinned = v == "true",
            "taskbar" => app.taskbar = v == "true",
            _ => {}
        }
    }
    app
}

/// The system image's application `id`, of manifest `text`: its program
/// is `/system/bin/<id>` unless the manifest names another.
pub fn system(id: &str, text: &str) -> AppInfo {
    parse(id, text, format!("/system/bin/{id}"))
}

/// Why an installed package's application is left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Its id is not one an application of a package may have.
    Id,
    /// Its id is a name of the system's.
    Taken,
    /// Its program is not a plain path in `/home/packages` or `/system`.
    Exe,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refusal::Id => {
                "its id is not 1 to 64 lower-case letters, digits, '+', '-' and '.' that begin with a letter or digit"
            }
            Refusal::Taken => "its id is the system's own",
            Refusal::Exe => "its program is not a plain path in /home/packages or /system",
        })
    }
}

/// Whether `id` may be the id of an installed package's application.
fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.first().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && b.len() <= MAX_ID
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"+-.".contains(c))
}

/// `v` without its control characters, cut short at `max` bytes where a
/// character begins.
fn text(v: &str, max: usize) -> String {
    let mut out = String::new();
    for c in v.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

/// The installed package's application `id` (its manifest's file name),
/// of manifest `manifest`, if the launcher may list and start it; `taken`
/// says whether a name is the system's (an application's id, the name of
/// a program of the system image).
pub fn packaged(id: &str, manifest: &str, taken: impl Fn(&str) -> bool) -> Result<AppInfo, Refusal> {
    if !valid_id(id) {
        return Err(Refusal::Id);
    }
    if taken(id) {
        return Err(Refusal::Taken);
    }
    let app = parse(id, manifest, format!("{PACKAGES}/bin/{id}"));
    if app.exe.len() > MAX_EXE || program(&app.exe).is_none() {
        return Err(Refusal::Exe);
    }
    let name = text(&app.name, MAX_NAME);
    Ok(AppInfo {
        name: if name.is_empty() { app.id.clone() } else { name },
        icon: text(&app.icon, MAX_NAME),
        category: text(&app.category, MAX_NAME),
        description: text(&app.description, MAX_DESCRIPTION),
        pinned: false,
        taskbar: false,
        ..app
    })
}

/// Where an application's program is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program<'a> {
    /// In the system image: its path there (`bin/editor`).
    System(&'a str),
    /// Among the installed packages' files: its path
    /// (`/home/packages/bin/abacus`), which the launcher reads through the
    /// VFS.
    Packages(&'a str),
}

/// Where the program `exe` is, if it is a plain path (absolute, no `.`,
/// `..` or empty part, no control character) to a file in `/system` or in
/// `/home/packages`.
pub fn program(exe: &str) -> Option<Program<'_>> {
    let parts = exe.strip_prefix('/')?.split('/');
    if !parts.clone().all(|part| !matches!(part, "" | "." | "..") && !part.chars().any(char::is_control)) {
        return None;
    }
    if let Some(path) = exe.strip_prefix("/system/") {
        return Some(Program::System(path));
    }
    exe.strip_prefix(PACKAGES).filter(|rest| rest.starts_with('/')).map(|_| Program::Packages(exe))
}

/// The id of the application whose manifest is the file `name`
/// (`<id>.app`).
pub fn manifest_id(name: &str) -> Option<&str> {
    name.strip_suffix(".app").filter(|id| !id.is_empty())
}

/// Every application, by name: the system image's, and the installed
/// packages' ([`MAX_PACKAGED`] at most, none with a system application's
/// id), the system's first of two of the same name.
pub fn merge(system: &[AppInfo], packaged: Vec<AppInfo>) -> Vec<AppInfo> {
    let mut all = system.to_vec();
    let own = packaged.into_iter().filter(|p| !system.iter().any(|s| s.id == p.id));
    all.extend(own.take(MAX_PACKAGED));
    all.sort_by(|a, b| a.name.cmp(&b.name));
    all
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use ipc::Encode;

    use super::*;

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn a_system_manifest_says_what_it_gives() {
        let manifest = "# The editor\nname=Text Editor\nexe=/system/bin/editor\nicon=document\n\
                        category=Productivity\ndescription=Write and edit text documents\npinned=true\n";
        let app = system("editor", manifest);
        let expected = AppInfo {
            id: "editor".into(),
            name: "Text Editor".into(),
            exe: "/system/bin/editor".into(),
            icon: "document".into(),
            category: "Productivity".into(),
            description: "Write and edit text documents".into(),
            pinned: true,
            taskbar: false,
        };
        assert_eq!(app, expected);
        // What it leaves out is the id's.
        let bare = system("music", "");
        assert_eq!(
            (bare.name.as_str(), bare.exe.as_str(), bare.icon.as_str(), bare.category.as_str()),
            ("music", "/system/bin/music", "music", "Applications")
        );
    }

    #[test]
    fn a_package_s_application_runs_an_installed_program() {
        let app = packaged("abacus", "name=Abacus\nicon=calculator\npinned=true\ntaskbar=true\n", none).unwrap();
        assert_eq!((app.name.as_str(), app.exe.as_str()), ("Abacus", "/home/packages/bin/abacus"));
        assert!(!app.pinned && !app.taskbar, "a package puts nothing in the pinned grid or on the taskbar");
        // Or the system's, or another of the packages' files.
        for exe in ["/system/bin/editor", "/home/packages/libexec/abacus/main"] {
            assert_eq!(packaged("abacus", &format!("exe={exe}"), none).unwrap().exe, exe);
        }
    }

    #[test]
    fn a_package_s_application_takes_no_name_of_the_system() {
        let names = ["terminal", "settings", "shell", "pkg"];
        let taken = |id: &str| names.contains(&id);
        for id in names {
            assert_eq!(packaged(id, "name=Terminal", taken), Err(Refusal::Taken), "{id}");
        }
        let long = "x".repeat(MAX_ID + 1);
        for id in ["", "Terminal", "-x", ".x", "a b", "a/b", "a_b", "é", long.as_str()] {
            assert_eq!(packaged(id, "", none), Err(Refusal::Id), "{id}");
        }
        let longest = "x".repeat(MAX_ID);
        for id in ["x", "abacus", "gtk+", "python3.15", "a-b", "0ad", longest.as_str()] {
            assert!(packaged(id, "", none).is_ok(), "{id}");
        }
    }

    #[test]
    fn a_package_s_application_runs_nothing_from_elsewhere() {
        let long = format!("/home/packages/{}", "x".repeat(MAX_EXE));
        for exe in [
            "bin/abacus",
            "/home/user/abacus",
            "/tmp/abacus",
            "/home/packages",
            "/home/packages/",
            "/home/packagesx/abacus",
            "/home/packages/../user/abacus",
            "/home/packages/./bin/abacus",
            "/home/packages//bin/abacus",
            "/home/packages/bin/abacus/",
            "/system",
            "/system/../home/user/abacus",
            "/home/packages/bin/a\u{7}b",
            long.as_str(),
        ] {
            assert_eq!(packaged("abacus", &format!("exe={exe}"), none), Err(Refusal::Exe), "{exe}");
        }
    }

    #[test]
    fn a_package_s_application_shows_as_plain_text_and_briefly() {
        let manifest = format!(
            "name=Ab\u{1b}]0;acus\u{7}\ndescription={}\ncategory={}\nicon=\u{7}\n",
            "é".repeat(MAX_DESCRIPTION),
            "c".repeat(MAX_NAME + 1)
        );
        let app = packaged("abacus", &manifest, none).unwrap();
        assert_eq!(app.name, "Ab]0;acus");
        assert_eq!(app.description, "é".repeat(MAX_DESCRIPTION / 2));
        assert_eq!(app.category, "c".repeat(MAX_NAME));
        assert_eq!(app.icon, "");
        // A name that says nothing is the id.
        assert_eq!(packaged("abacus", "name=\u{7}", none).unwrap().name, "abacus");
    }

    #[test]
    fn programs_are_found_where_they_are() {
        assert_eq!(program("/system/bin/editor"), Some(Program::System("bin/editor")));
        assert_eq!(program("/home/packages/bin/abacus"), Some(Program::Packages("/home/packages/bin/abacus")));
        for elsewhere in ["", "editor", "/home/user/editor", "/system", "/system/", "/system/bin/../editor"] {
            assert_eq!(program(elsewhere), None, "{elsewhere}");
        }
    }

    #[test]
    fn manifests_are_named_for_their_application() {
        assert_eq!(manifest_id("abacus.app"), Some("abacus"));
        for other in [".app", "abacus", "abacus.app.txt", "abacus.APP"] {
            assert_eq!(manifest_id(other), None, "{other}");
        }
    }

    #[test]
    fn the_system_s_applications_come_first() {
        let app = |id: &str, name: &str| AppInfo { name: name.into(), ..system(id, "") };
        let system_apps = vec![app("files", "Files"), app("terminal", "Terminal")];
        let packaged_apps =
            vec![app("zz", "Files"), app("abacus", "Abacus"), app("terminal", "Terminal"), app("term", "Terminal")];
        let all = merge(&system_apps, packaged_apps);
        let ids: Vec<&str> = all.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["abacus", "files", "zz", "terminal", "term"]);
        let many = (0..MAX_PACKAGED + 5).map(|i| app(&format!("app{i}"), "App")).collect();
        assert_eq!(merge(&system_apps, many).len(), system_apps.len() + MAX_PACKAGED);
    }

    #[test]
    fn every_application_fits_one_message() {
        // More of the system's than any image has, and as many of the
        // packages' as are listed, each with every value at its longest.
        let system_apps: Vec<AppInfo> = (0..32)
            .map(|i| AppInfo {
                name: format!("Application {i:>20}"),
                category: "Accessories".into(),
                description: "d".repeat(128),
                ..system(&format!("application{i:>8}"), "")
            })
            .collect();
        let exe = format!("/home/packages/bin/{}", "x".repeat(MAX_EXE - "/home/packages/bin/".len()));
        let long = "w".repeat(2 * MAX_DESCRIPTION);
        let manifest = format!("name={long}\nexe={exe}\nicon={long}\ncategory={long}\ndescription={long}\n");
        let packaged_apps =
            (0..MAX_PACKAGED).map(|i| packaged(&format!("{i:0>64}"), &manifest, none).unwrap()).collect();
        let all = merge(&system_apps, packaged_apps);
        assert_eq!(all.len(), 32 + MAX_PACKAGED);
        let mut e = ipc::Encoder::new();
        all.encode(&mut e);
        assert!(e.bytes.len() + ipc::HEADER_LEN < abi::CHANNEL_MAX_BYTES, "{} bytes", e.bytes.len());
    }
}

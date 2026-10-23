// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The licences of the parts of the system image that are not Veda's own,
//! whose texts the image carries in `/system/licences` (About Veda shows
//! them).
//!
//! Each part's texts are as its release has them, in a directory of its
//! own (`/system/licences/<part>`; a port's, with `SOURCE`, which says
//! where its source is: [`port_texts`]). What builds a part gathers them,
//! and an image build fails where it would have a part without them: the
//! ports' build scripts gather theirs (their `licences` stages), the
//! firmware lists name each file's (`linux::firmware`), and [`crates`]
//! finds those of the crates the programs are built with. [`readme`] lists the parts, with their
//! versions, licences, what each is and where its texts are, in
//! `/system/licences/README`.
//!
//! The crates are what Cargo links into each program of the image, for the
//! program's target ([`Group`]), as `cargo tree` gives them: their normal
//! dependencies, without procedural macros (which run on the build machine)
//! and without Veda's own crates. Their texts are their packages' files
//! (`LICENSE*`, `COPYING*` and the like, and what the manifest's
//! `license-file` names); [`UNPACKAGED`] says where the terms are of the
//! few whose packages have none.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::ports::Port;
use crate::util::{self, Result};

/// What `/system/licences/README` says first.
const README_HEADER: &str = "\
# The licences of the parts of this system, and where their texts are,
# which About Veda shows. Veda itself is free software under the GNU
# General Public License, version 3; its other parts keep their own
# licences. Those under the GNU GPL or LGPL ask whoever passes them on to
# offer their source too: SOURCE, beside such a part's texts, says what it
# is.
#
# A part a paragraph: its name, then its version, its licence (as SPDX
# writes it), what it is in the system, and where its texts are.
";

/// The parts whose texts are in `/system/licences/<part>`, in the order
/// [`readme`] lists them: the directory, the name About Veda gives the
/// part, and what it is. A port's directory is named after it, and its
/// version and licence are its `port.toml`'s.
const PARTS: &[(&str, &str, &str)] = &[
    (
        "musl",
        "musl",
        "The C library, in every C program, every program on Rust's standard library and the driver VM's programs",
    ),
    (
        "gcc",
        "GCC",
        "The C compiler of the system's C toolchain, and GCC's runtime libraries (libgcc, libstdc++, libatomic) in \
         every C program and every program on Rust's standard library, which its Runtime Library Exception leaves \
         under their own licences",
    ),
    ("binutils", "GNU Binutils", "The C toolchain's assembler, linker and tools for object files"),
    ("gmp", "GMP", "Arithmetic in any precision, in the C compiler"),
    ("mpfr", "MPFR", "Floating-point arithmetic in any precision, in the C compiler"),
    ("mpc", "MPC", "Complex arithmetic in any precision, in the C compiler"),
    ("ffmpeg", "FFmpeg", "The demuxers, decoders, scaler and resampler of the video decoder, mediadec"),
    ("dav1d", "dav1d", "The AV1 decoder, in the video decoder"),
    (
        "rust",
        "Rust",
        "Rust's standard library, in Veda's programs and the driver VM's: Rust's own code, LLVM's unwinder and the \
         crates the library is built from",
    ),
    ("crates", "Rust crates", "The crates Veda's programs are built with, each under its own licence"),
    ("linux", "Linux", "The kernel of the driver VM, which drives Veda's devices"),
    ("mesa", "Mesa", "The driver VM's renderer: OpenGL ES on Gallium's drivers"),
    (
        "firmware",
        "Device firmware",
        "The firmware the driver VM's Linux loads into devices (Wi-Fi, Bluetooth, the GPU, sound), each file under \
         its maker's terms",
    ),
    ("web", "WPE WebKit", "The browser's web engine and the libraries it stands on, each under its own licence"),
];

/// The texts of `port`, for `/system/licences/<port>`, as the build that
/// has it gathered them into `gathered` (`<port>/...`, as its release has
/// them), with where its source is (`SOURCE`). None there is an error,
/// which says that `how` gathers them.
pub fn port_texts(gathered: &Path, port: &str, how: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let dir = gathered.join(port);
    let texts = util::files_under(&dir)?;
    if texts.is_empty() {
        return Err(format!(
            "{port} is in the image without its licence texts ({}): {how} gathers them",
            dir.display()
        ));
    }
    let mut files = Vec::new();
    for path in texts {
        let rel = path.strip_prefix(&dir).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        files.push((format!("licences/{port}/{rel}"), util::read(&path)?));
    }
    files.push((format!("licences/{port}/SOURCE"), Port::named(port)?.source_note().into_bytes()));
    Ok(files)
}

/// Programs of the image built for one target: their packages, and how
/// Cargo is run for them, as their build runs it (with the compiler that
/// knows their target, in the directory whose configuration they are
/// built with).
pub struct Group {
    pub target: &'static str,
    pub packages: Vec<String>,
    /// The compiler, if not the build machine's.
    pub rustc: Option<PathBuf>,
    /// The directory, if not the workspace's.
    pub dir: Option<PathBuf>,
}

impl Group {
    pub fn new(target: &'static str, packages: &[&str]) -> Group {
        Group { target, packages: packages.iter().map(|p| p.to_string()).collect(), rustc: None, dir: None }
    }
}

/// A crate the image's programs are built with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crate {
    pub name: String,
    pub version: String,
    /// Its licence, as its manifest writes it.
    pub licence: String,
    /// Its home (the manifest's `repository`), if it gives one.
    pub repository: Option<String>,
    /// Its package's directory: Cargo's copy of it, or a port's as prepared.
    dir: PathBuf,
    /// The file its manifest names as its licence (`license-file`), if any.
    licence_file: Option<String>,
}

impl Crate {
    /// Its directory in `/system/licences/crates`.
    fn dir_name(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }
}

/// The crates of the programs of `groups`: what `cargo tree` gives for
/// each group, but Veda's own crates, each once, in the order of their
/// names and versions.
pub fn crates(groups: &[Group]) -> Result<Vec<Crate>> {
    let known = packages()?;
    let mut listed = BTreeSet::new();
    for group in groups.iter().filter(|g| !g.packages.is_empty()) {
        let mut tree = util::cargo();
        if let Some(dir) = &group.dir {
            tree.current_dir(dir);
        }
        if let Some(rustc) = &group.rustc {
            tree.env("RUSTC", rustc);
        }
        tree.args(["tree", "--edges", "normal,no-proc-macro", "--target", group.target]);
        tree.args(["--prefix", "none", "--format", "{p}"]);
        for package in &group.packages {
            tree.args(["--package", package]);
        }
        let out = tree.output().map_err(|e| format!("running cargo tree: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "cargo tree for {} ({}) failed: {}",
                group.packages.join(", "),
                group.target,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(entry) = tree_entry(line) {
                listed.insert(entry);
            }
        }
    }
    let own = util::workspace_root();
    let ports = crate::crates::root();
    let mut crates = Vec::new();
    for (name, version, path) in listed {
        // Veda's own crates are in its checkout, the ports' prepared
        // crates aside.
        if path.as_ref().is_some_and(|p| p.starts_with(&own) && !p.starts_with(&ports)) {
            continue;
        }
        // One from a registry is Cargo's copy, never one of the checkout's.
        let found = known.iter().find(|c| {
            c.name == name
                && c.version == version
                && match &path {
                    Some(p) => c.dir == *p,
                    None => !c.dir.starts_with(&own),
                }
        });
        match found {
            Some(c) => crates.push(c.clone()),
            None => return Err(format!("cargo tree lists {name} {version}, which cargo metadata does not know")),
        }
    }
    Ok(crates)
}

/// A line of `cargo tree --prefix none --format {p}`: the crate's name, its
/// version and, for one built from a directory, the directory
/// (`smoltcp v0.14.0 (/veda/implementation/ports/.crates/src/smoltcp)`);
/// `(*)` marks one shown before.
fn tree_entry(line: &str) -> Option<(String, String, Option<PathBuf>)> {
    let line = line.trim().trim_end_matches("(*)").trim_end();
    let mut words = line.splitn(3, ' ');
    let name = words.next().filter(|n| !n.is_empty())?;
    let version = words.next()?.strip_prefix('v')?;
    let path = words
        .next()
        .and_then(|rest| rest.strip_prefix('(')?.strip_suffix(')'))
        .filter(|rest| rest.starts_with('/'))
        .map(PathBuf::from);
    Some((name.to_string(), version.to_string(), path))
}

/// Every package Cargo knows of in the workspace (`cargo metadata`), with
/// what [`Crate`] keeps of it.
fn packages() -> Result<Vec<Crate>> {
    let out = util::cargo()
        .args(["metadata", "--format-version", "1"])
        .output()
        .map_err(|e| format!("running cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let metadata: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata's output: {e}"))?;
    let text = |p: &serde_json::Value, key: &str| p.get(key).and_then(serde_json::Value::as_str).map(String::from);
    let mut packages = Vec::new();
    for p in metadata.get("packages").and_then(serde_json::Value::as_array).into_iter().flatten() {
        let (Some(name), Some(version), Some(manifest)) =
            (text(p, "name"), text(p, "version"), text(p, "manifest_path"))
        else {
            return Err("cargo metadata lists a package without its name, version or manifest".into());
        };
        let dir = Path::new(&manifest).parent().map(Path::to_path_buf).unwrap_or_default();
        packages.push(Crate {
            name,
            version,
            licence: text(p, "license").unwrap_or_default(),
            repository: text(p, "repository"),
            dir,
            licence_file: text(p, "license_file"),
        });
    }
    Ok(packages)
}

/// Whether the file `name` of a package's top directory holds its licence
/// or a notice of it, as packages name them (`LICENSE-MIT`,
/// `COPYING.LESSER`, `UNLICENSE`, `NOTICE`).
fn is_licence_file(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["licence", "license", "copying", "copyright", "notice", "unlicense", "patents"].iter().any(|p| name.starts_with(p))
}

/// A crate whose package has no licence file, and where its terms are
/// instead: the first `lines` lines of its `file` (kept as `as_name`),
/// and, for one whose header names its licences without their texts, the
/// texts in the package of another crate of the same project's
/// (`texts_of`, in the image too).
struct Unpackaged {
    name: &'static str,
    version: &'static str,
    file: &'static str,
    lines: usize,
    as_name: &'static str,
    texts_of: Option<&'static str>,
}

/// The crates of the image's whose packages have no licence file. A
/// crate's new version, which may have them, or other terms, is looked at
/// again.
const UNPACKAGED: &[Unpackaged] = &[
    // FastNoise Lite's MIT licence, at the top of its one source file.
    Unpackaged {
        name: "fastnoise-lite",
        version: "1.1.1",
        file: "src/lib.rs",
        lines: 22,
        as_name: "LICENSE",
        texts_of: None,
    },
    // zune-image's DEFLATE decoder, whose header names the MIT, Apache 2.0
    // and zlib licences of its project, which zune-core's package has.
    Unpackaged {
        name: "zune-inflate",
        version: "0.2.54",
        file: "src/lib.rs",
        lines: 5,
        as_name: "NOTICE",
        texts_of: Some("zune-core"),
    },
];

/// The licence texts of `krate`'s package: its files of [`is_licence_file`]
/// (its `LICENSES` directory's too) and the one its manifest names, by
/// their paths in it.
fn package_texts(krate: &Crate) -> Result<Vec<(String, PathBuf)>> {
    let mut texts = BTreeMap::new();
    let entries = std::fs::read_dir(&krate.dir).map_err(|e| format!("reading {}: {e}", krate.dir.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if path.is_file() && is_licence_file(&name) {
            texts.insert(name, path);
        } else if path.is_dir() && name.eq_ignore_ascii_case("licenses") {
            for file in util::files_under(&path)? {
                let rel = file.strip_prefix(&krate.dir).unwrap_or(&file).to_string_lossy().replace('\\', "/");
                texts.insert(rel, file);
            }
        }
    }
    if let Some(file) = &krate.licence_file
        && !file.split(['/', '\\']).any(|part| part == "..")
        && krate.dir.join(file).is_file()
    {
        texts.insert(file.replace('\\', "/"), krate.dir.join(file));
    }
    Ok(texts.into_iter().collect())
}

/// The licence texts of `crates`, for `/system/licences/crates`: each
/// crate's in `<name>-<version>/`, and `README`, the list of them. A crate
/// whose package has no licence file, and is not in [`UNPACKAGED`], is an
/// error.
pub fn crate_files(crates: &[Crate]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    let mut readme = String::from(
        "The Rust crates Veda's programs are built with (the kernel, the loader, the services and applications, the C \
         library's POSIX layer, the video decoder and the driver VM's programs), as Cargo links them into the \
         system: each one's version, licence (as its manifest writes it) and home. Each one's licence texts, as its \
         package has them, are in the directory of its name and version.\n\n",
    );
    for krate in crates {
        let dir = format!("licences/crates/{}", krate.dir_name());
        let mut texts = package_texts(krate)?;
        if texts.is_empty() {
            let Some(terms) = UNPACKAGED.iter().find(|u| u.name == krate.name && u.version == krate.version) else {
                return Err(format!(
                    "the crate {} {} has no licence file in its package ({}): say where its terms are in \
                     automation/source/notices.rs (UNPACKAGED)",
                    krate.name,
                    krate.version,
                    krate.dir.display()
                ));
            };
            let source = util::read_to_string(&krate.dir.join(terms.file))?;
            let header: String = source.lines().take(terms.lines).map(|l| format!("{l}\n")).collect();
            files.push((format!("{dir}/{}", terms.as_name), header.into_bytes()));
            if let Some(other) = terms.texts_of {
                let Some(sibling) = crates.iter().find(|c| c.name == other) else {
                    return Err(format!("{}'s licence texts are {other}'s, which is not in the image", krate.name));
                };
                texts = package_texts(sibling)?;
            }
        }
        for (rel, path) in texts {
            files.push((format!("{dir}/{rel}"), util::read(&path)?));
        }
        let home = krate.repository.as_deref().map(|r| format!(" ({r})")).unwrap_or_default();
        let licence = if krate.licence.is_empty() { "see its texts" } else { &krate.licence };
        readme.push_str(&format!("{} {}: {licence}{home}\n", krate.name, krate.version));
    }
    if !crates.is_empty() {
        files.push(("licences/crates/README".into(), readme.into_bytes()));
    }
    Ok(files)
}

/// The licences `crates` are under, each once (the identifiers of their
/// SPDX expressions), for the list's summary.
fn crate_licences(crates: &[Crate]) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for krate in crates {
        let mut words = krate.licence.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == '/');
        while let Some(word) = words.next() {
            match word {
                "" | "OR" | "AND" => {}
                // An exception to the licence before it is not one.
                "WITH" => {
                    words.next();
                }
                id => {
                    ids.insert(id.to_string());
                }
            }
        }
    }
    ids.into_iter().collect()
}

/// `/system/licences/README`: Veda's licence, the fonts' (and Noto's, with
/// `noto`, the browser's fonts), then each part in `parts` (the
/// directories of `/system/licences` that have texts), as [`PARTS`]
/// describes it. A part [`PARTS`] does not describe is an error.
pub fn readme(parts: &BTreeSet<String>, crates: &[Crate], noto: bool) -> Result<String> {
    let mut text = String::from(README_HEADER);
    let mut part = |name: &str, version: &str, licence: &str, what: &str, texts: &str| {
        text.push_str(&format!("\n[{name}]\n"));
        if !version.is_empty() {
            text.push_str(&format!("version = {version}\n"));
        }
        text.push_str(&format!("licence = {licence}\nwhat = {what}\ntexts = {texts}\n"));
    };
    part(
        "Veda",
        env!("CARGO_PKG_VERSION"),
        "GPL-3.0-only",
        "The operating system: its kernel, services, applications and the rest of its own",
        "/system/LICENSE.txt",
    );
    let noto = if noto { "; and Noto's (© The Noto Project Authors), the browser's" } else { "" };
    part(
        "Fonts",
        "",
        "OFL-1.1",
        &format!(
            "The system's fonts: Inter (© 2020 The Inter Project Authors), Lato (© 2011-2015 tyPoland Łukasz \
             Dziedzic) and JetBrains Mono (© 2020 The JetBrains Mono Project Authors){noto}"
        ),
        "/system/fonts/OFL.txt",
    );
    if let Some(unknown) = parts.iter().find(|p| !PARTS.iter().any(|(dir, ..)| *dir == p.as_str())) {
        return Err(format!(
            "/system/licences/{unknown} has no description: add one to automation/source/notices.rs (PARTS), which \
             About Veda shows"
        ));
    }
    for (dir, name, what) in PARTS.iter().filter(|(dir, ..)| parts.contains(*dir)) {
        let texts = format!("/system/licences/{dir}");
        match *dir {
            "crates" => {
                let licences = crate_licences(crates).join(", ");
                let what = format!("{what} ({} of them)", crates.len());
                part(name, "", &format!("each crate's: {licences}"), &what, &texts);
            }
            "firmware" => part(name, "", "redistributable: each maker's terms", what, &texts),
            "web" => {
                let webkit = Port::named("wpewebkit")?;
                part(name, &webkit.version, &format!("{}, and each library's own", webkit.licence), what, &texts);
            }
            port => {
                let port = Port::named(port)?;
                part(name, &port.version, &port.licence, what, &texts);
            }
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_tree_lines_name_crates_and_their_directories() {
        assert_eq!(tree_entry("serde v1.0.229"), Some(("serde".into(), "1.0.229".into(), None)));
        assert_eq!(tree_entry("block-buffer v0.12.1 (*)"), Some(("block-buffer".into(), "0.12.1".into(), None)));
        assert_eq!(
            tree_entry("smoltcp v0.14.0 (/veda/implementation/ports/.crates/src/smoltcp)"),
            Some((
                "smoltcp".into(),
                "0.14.0".into(),
                Some(PathBuf::from("/veda/implementation/ports/.crates/src/smoltcp"))
            ))
        );
        assert_eq!(
            tree_entry("rt v0.1.0 (/veda/implementation/libraries/rt) (*)"),
            Some(("rt".into(), "0.1.0".into(), Some(PathBuf::from("/veda/implementation/libraries/rt"))))
        );
        assert_eq!(tree_entry(""), None);
        assert_eq!(tree_entry("warning: something"), None);
    }

    #[test]
    fn licence_files_are_known_by_their_names() {
        for name in ["LICENSE", "LICENSE-MIT", "license-apache-2.0", "LICENCE.md", "COPYING", "UNLICENSE", "NOTICE"] {
            assert!(is_licence_file(name), "{name}");
        }
        for name in ["README.md", "Cargo.toml", "src", "CHANGELOG.md"] {
            assert!(!is_licence_file(name), "{name}");
        }
    }

    /// A crate in `dir`, with `files` in its package.
    fn package(dir: &Path, name: &str, version: &str, files: &[(&str, &str)]) -> Crate {
        let at = dir.join(format!("{name}-{version}"));
        for (file, text) in files {
            util::create_dir_all(at.join(file).parent().unwrap()).unwrap();
            util::write(&at.join(file), text).unwrap();
        }
        Crate {
            name: name.into(),
            version: version.into(),
            licence: "MIT OR Apache-2.0".into(),
            repository: Some(format!("https://example.org/{name}")),
            dir: at,
            licence_file: None,
        }
    }

    #[test]
    fn crates_carry_their_packages_texts_or_say_where_they_are() {
        let dir = std::env::temp_dir().join(format!("automation-notices-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = package(
            &dir,
            "a",
            "1.0.0",
            &[("LICENSE-MIT", "MIT"), ("LICENSES/Apache-2.0.txt", "A"), ("src/lib.rs", "")],
        );
        let core = package(&dir, "zune-core", "0.5.3", &[("LICENSE-ZLIB", "zlib")]);
        let header: String = (1..=7).map(|n| format!("// line {n}\n")).collect();
        let inflate = package(&dir, "zune-inflate", "0.2.54", &[("src/lib.rs", &header)]);
        let files = crate_files(&[a.clone(), core.clone(), inflate.clone()]).unwrap();
        let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            names,
            [
                "licences/crates/a-1.0.0/LICENSE-MIT",
                "licences/crates/a-1.0.0/LICENSES/Apache-2.0.txt",
                "licences/crates/zune-core-0.5.3/LICENSE-ZLIB",
                "licences/crates/zune-inflate-0.2.54/NOTICE",
                "licences/crates/zune-inflate-0.2.54/LICENSE-ZLIB",
                "licences/crates/README",
            ]
        );
        // The header's first lines, and the list of crates.
        assert_eq!(files[3].1, (1..=5).map(|n| format!("// line {n}\n")).collect::<String>().into_bytes());
        let readme = String::from_utf8(files[5].1.clone()).unwrap();
        assert!(readme.contains("\na 1.0.0: MIT OR Apache-2.0 (https://example.org/a)\n"), "{readme}");
        // Another crate without texts, or the same one in a version not
        // looked at, is an error.
        let bare = package(&dir, "bare", "1.0.0", &[("src/lib.rs", "")]);
        assert!(crate_files(&[bare]).unwrap_err().contains("UNPACKAGED"));
        let newer = package(&dir, "zune-inflate", "0.2.55", &[("src/lib.rs", &header)]);
        assert!(crate_files(&[core, newer]).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The crates of the kernel's, of the programs on `rt` and of the driver
    /// VM's (whose targets the build machine's Rust knows) have their texts,
    /// and Veda's own crates are not among them.
    #[test]
    fn the_programs_crates_have_their_texts() {
        let programs: Vec<&str> = crate::components::PROGRAMS.iter().filter(|p| !p.std).map(|p| p.package).collect();
        let mut guest = Group::new(crate::architecture::GUEST_TARGET, &["guest-init"]);
        guest.dir = Some(util::workspace_root().join("implementation").join("guests").join("linux"));
        let groups = [
            Group::new(crate::architecture::KERNEL_TARGET, &["kernel"]),
            Group::new(crate::architecture::USER_TARGET, &programs),
            guest,
        ];
        let crates = crates(&groups).unwrap();
        // netd's TCP/IP stack is a port's; rt is Veda's.
        assert!(crates.iter().any(|c| c.name == "smoltcp"), "{crates:?}");
        assert!(!crates.iter().any(|c| c.name == "rt" || c.name == "kernel"), "{crates:?}");
        let files = crate_files(&crates).unwrap();
        assert!(files.iter().any(|(path, _)| path.starts_with("licences/crates/smoltcp-")));
    }

    #[test]
    fn the_readme_describes_every_part() {
        let parts: BTreeSet<String> = ["musl", "crates", "firmware"].iter().map(|p| p.to_string()).collect();
        let crates = vec![Crate {
            name: "a".into(),
            version: "1.0.0".into(),
            licence: "(MIT OR Apache-2.0) AND Unicode-3.0 AND Apache-2.0 WITH LLVM-exception".into(),
            repository: None,
            dir: PathBuf::new(),
            licence_file: None,
        }];
        let text = readme(&parts, &crates, false).unwrap();
        let musl = Port::named("musl").unwrap();
        assert!(text.contains("\n[Veda]\nversion = 0.1.0\nlicence = GPL-3.0-only\n"), "{text}");
        assert!(text.contains("texts = /system/fonts/OFL.txt\n") && !text.contains("Noto"), "{text}");
        assert!(text.contains(&format!("\n[musl]\nversion = {}\nlicence = MIT\n", musl.version)), "{text}");
        assert!(text.contains("licence = each crate's: Apache-2.0, MIT, Unicode-3.0\n"), "{text}");
        // In PARTS' order: the C library before the crates and the firmware.
        let at = |part: &str| text.find(&format!("[{part}]")).unwrap();
        assert!(at("musl") < at("Rust crates") && at("Rust crates") < at("Device firmware"));
        assert!(readme(&parts, &crates, true).unwrap().contains("Noto"));
        let unknown: BTreeSet<String> = ["something"].iter().map(|p| p.to_string()).collect();
        assert!(readme(&unknown, &[], false).unwrap_err().contains("PARTS"));
    }
}

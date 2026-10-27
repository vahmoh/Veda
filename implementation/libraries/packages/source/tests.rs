// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the modules' tests share, and the tests of changes as a whole:
//! packages built, offered, resolved, planned and installed, upgraded and
//! removed on a file system in memory, and every change and every recovery
//! cut short at each of its steps.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::archive::{Builder, Compression};
use crate::database::Database;
use crate::index::Offer;
use crate::manifest::{Architecture, Manifest};
use crate::memfs::{MemFiles, Node};
use crate::name::Name;
use crate::requirement::{Provision, Requirement};
use crate::transaction::{self, Change, Error, Files, Incoming, Layout, Problem, Recovery};
use crate::version::Version;
use crate::{Digest, limits};

/// A manifest of `label`, `NAME VERSION`, for amd64.
pub fn manifest(label: &str) -> Manifest {
    let (name, version) = label.split_once(' ').expect("NAME VERSION");
    Manifest {
        name: Name::parse(name).unwrap(),
        version: Version::parse(version).unwrap(),
        architecture: Architecture::Amd64,
        summary: alloc::format!("{name}, for the tests"),
        description: String::new(),
        licence: String::from("GPL-3.0-only"),
        homepage: String::new(),
        depends: Vec::new(),
        conflicts: Vec::new(),
        provides: Vec::new(),
    }
}

/// Adds relations to a manifest, or to an offer's.
pub trait With: Sized {
    fn manifest_mut(&mut self) -> &mut Manifest;

    fn depends(mut self, requirement: &str) -> Self {
        self.manifest_mut().depends.push(Requirement::parse(requirement).unwrap());
        self
    }

    fn conflicts(mut self, requirement: &str) -> Self {
        self.manifest_mut().conflicts.push(Requirement::parse(requirement).unwrap());
        self
    }

    fn provides(mut self, provision: &str) -> Self {
        self.manifest_mut().provides.push(Provision::parse(provision).unwrap());
        self
    }

    /// For every architecture.
    fn any(mut self) -> Self {
        self.manifest_mut().architecture = Architecture::Any;
        self
    }
}

impl With for Manifest {
    fn manifest_mut(&mut self) -> &mut Manifest {
        self
    }
}

impl With for Offer {
    fn manifest_mut(&mut self) -> &mut Manifest {
        &mut self.manifest
    }
}

/// Where the tests' system keeps the installed files and its own.
fn layout() -> Layout {
    Layout { live: String::from("/home/packages"), state: String::from("/home/.private/package-manager") }
}

/// A system with nothing installed.
fn system() -> MemFiles {
    let fs = MemFiles::new();
    fs.put_dir("/home/packages");
    fs.put_dir("/home/.private/package-manager");
    fs
}

/// A package file of `label` holding `files`, and its offer.
fn package(m: Manifest, files: &[(&str, &[u8])]) -> (Vec<u8>, Offer) {
    let mut b = Builder::new(m.clone());
    for (path, data) in files {
        b.add(path, data.to_vec()).unwrap();
    }
    let bytes = b.build(Compression::Deflate).unwrap();
    let unpacked = files.iter().map(|(_, d)| d.len() as u64).sum();
    let offer = Offer {
        file: alloc::format!("{}-{}.vpkg", m.name, m.version),
        manifest: m,
        size: bytes.len() as u64,
        sha256: Digest::of(&bytes),
        unpacked,
    };
    (bytes, offer)
}

fn names(list: &[&str]) -> Vec<Name> {
    list.iter().map(|n| Name::parse(n).unwrap()).collect()
}

fn world(list: &[&str]) -> Vec<Requirement> {
    list.iter().map(|r| Requirement::parse(r).unwrap()).collect()
}

/// Carries out a change on `fs`: removing `remove`, installing `install`
/// (whose files are put in `/archives` first, as the service's cache has
/// them).
fn change(
    fs: &mut MemFiles,
    remove: &[&str],
    install: &[&(Vec<u8>, Offer)],
    wanted: &[&str],
) -> Result<Database, Error> {
    let db = transaction::load(fs, &layout())?;
    let paths: Vec<String> =
        install.iter().map(|(_, offer)| alloc::format!("/archives/{}.vpkg", offer.sha256)).collect();
    for ((bytes, _), path) in install.iter().zip(&paths) {
        fs.put(path, bytes);
    }
    let change = Change {
        remove: names(remove),
        install: install
            .iter()
            .zip(&paths)
            .map(|((_, offer), path)| Incoming { path, offer, repository: "main" })
            .collect(),
        world: world(wanted),
        now: 1_800_000_000,
    };
    transaction::apply(fs, &layout(), &db, &change, &mut |_| {})
}

/// What `fs` holds in `/home`.
fn home(fs: &MemFiles) -> BTreeMap<String, Node> {
    fs.tree("/home")
}

/// The live directory's files, by relative path, and its directories
/// (as `DIR/`).
fn live(fs: &MemFiles) -> Vec<String> {
    fs.tree("/home/packages")
        .into_iter()
        .map(|(path, node)| match node {
            Node::File(data) => alloc::format!("{path}={}", String::from_utf8_lossy(&data)),
            Node::Dir => alloc::format!("{path}/"),
        })
        .collect()
}

/// Nothing of a change is left in the state directory but the database.
fn tidy(fs: &MemFiles) -> bool {
    let state = fs.tree("/home/.private/package-manager");
    state.keys().all(|k| k == "installed.json")
}

#[test]
fn packages_install_upgrade_and_go() {
    let mut fs = system();
    let hello1 = package(manifest("hello 1.0"), &[("bin/hello", b"hello one"), ("share/hello/a", b"a")]);
    let lib = package(manifest("lib 1.0"), &[("lib/libgreet", b"greet")]);
    let db = change(&mut fs, &[], &[&hello1, &lib], &["hello"]).unwrap();
    assert_eq!(db.generation, 1);
    assert_eq!(db.world, world(&["hello"]));
    assert_eq!(
        live(&fs),
        ["bin/", "bin/hello=hello one", "lib/", "lib/libgreet=greet", "share/", "share/hello/", "share/hello/a=a"]
    );
    assert!(tidy(&fs));
    assert_eq!(transaction::verify(&mut fs, &layout(), &db, None).unwrap(), []);

    // Upgraded, lib removed, another installed.
    let hello2 = package(manifest("hello 2.0"), &[("bin/hello", b"hello two"), ("share/hello/b", b"b")]);
    let other = package(manifest("other 1.0"), &[("bin/other", b"other")]);
    let db = change(&mut fs, &["hello", "lib"], &[&hello2, &other], &["hello", "other"]).unwrap();
    assert_eq!(db.generation, 2);
    assert_eq!(
        live(&fs),
        ["bin/", "bin/hello=hello two", "bin/other=other", "share/", "share/hello/", "share/hello/b=b"]
    );
    let labels: Vec<String> = db.packages.iter().map(|p| p.manifest.label()).collect();
    assert_eq!(labels, ["hello 2.0", "other 1.0"]);
    assert_eq!(db.get("hello").unwrap().files.len(), 2);
    assert_eq!(db.get("hello").unwrap().installed, 1_800_000_000);
    assert!(tidy(&fs));

    // All of it removed: nothing left.
    let db = change(&mut fs, &["hello", "other"], &[], &[]).unwrap();
    assert!(db.packages.is_empty() && db.world.is_empty());
    assert_eq!(live(&fs), Vec::<String>::new());
    assert_eq!(transaction::load(&mut fs, &layout()).unwrap(), db);
}

#[test]
fn packages_do_not_take_one_anothers_places() {
    let mut fs = system();
    let a = package(manifest("a 1.0"), &[("bin/x", b"a")]);
    change(&mut fs, &[], &[&a], &["a"]).unwrap();
    let before = home(&fs);
    for (install, why) in [
        (package(manifest("b 1.0"), &[("bin/x", b"b")]), "/home/packages/bin/x, which belongs to a"),
        (package(manifest("b 1.0"), &[("bin/x/y", b"b")]), "/home/packages/bin/x, which belongs to a"),
    ] {
        let e = change(&mut fs, &[], &[&install], &["a", "b"]).unwrap_err();
        assert_eq!(e.to_string(), alloc::format!("b 1.0 has a file at {why}"));
        assert_eq!(home(&fs), before);
    }
    // Two packages arriving with one file.
    let (b, c) = (package(manifest("b 1.0"), &[("bin/y", b"b")]), package(manifest("c 1.0"), &[("bin/y", b"c")]));
    let e = change(&mut fs, &[], &[&b, &c], &["a", "b", "c"]).unwrap_err();
    assert!(e.to_string().contains("another package being installed"), "{e}");
    // A file that no package installed is in the way.
    fs.put("/home/packages/share/stray", b"?");
    let s = package(manifest("s 1.0"), &[("share/stray", b"s")]);
    let e = change(&mut fs, &[], &[&s], &["a", "s"]).unwrap_err();
    assert_eq!(e.to_string(), "s 1.0 has a file at /home/packages/share/stray, where something of no package's is");
    // But a package's own file moves on to its next version.
    let a2 = package(manifest("a 2.0"), &[("bin/x", b"a2")]);
    change(&mut fs, &["a"], &[&a2], &["a"]).unwrap();
    assert!(live(&fs).contains(&String::from("bin/x=a2")));
}

#[test]
fn damaged_or_false_packages_change_nothing() {
    let mut fs = system();
    let (mut bytes, offer) = package(manifest("a 1.0"), &[("bin/a", b"aaaa")]);
    let before = home(&fs);
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let e = change(&mut fs, &[], &[&(bytes, offer.clone())], &["a"]).unwrap_err();
    assert_eq!(e, Error::Mismatch(String::from("a 1.0")));
    // The index offers another manifest than the file has.
    let (bytes, mut lying) = package(manifest("a 1.0"), &[("bin/a", b"aaaa")]);
    lying.manifest.summary = String::from("Something else");
    assert_eq!(change(&mut fs, &[], &[&(bytes, lying)], &["a"]).unwrap_err(), Error::Mismatch(String::from("a 1.0")));
    assert_eq!(home(&fs), before);
    assert_eq!(change(&mut fs, &["nothing"], &[], &[]).unwrap_err(), Error::NotInstalled(String::from("nothing")));
}

#[test]
fn no_room_changes_nothing() {
    let mut fs = system();
    let small = package(manifest("small 1.0"), &[("bin/s", b"small")]);
    change(&mut fs, &[], &[&small], &["small"]).unwrap();
    let before = home(&fs);
    fs.set_room(4000);
    let big: Vec<u8> = (0..20_000u32).map(|i| (i % 7) as u8).collect();
    let large = package(manifest("large 1.0"), &[("share/large", &big)]);
    let e = change(&mut fs, &["small"], &[&large], &["large"]).unwrap_err();
    assert!(e.to_string().contains("there is no room left"), "{e}");
    assert_eq!(home(&fs), before);
}

#[test]
fn installed_files_are_verified() {
    let mut fs = system();
    let a = package(manifest("a 1.0"), &[("bin/a", b"aaaa"), ("share/a/b", b"b"), ("share/a/c", b"c")]);
    let db = change(&mut fs, &[], &[&a], &["a"]).unwrap();
    fs.put("/home/packages/bin/a", b"changed");
    fs.remove_file("/home/packages/share/a/c").unwrap();
    fs.put("/home/packages/lib/extra", b"?");
    let problems: Vec<String> =
        transaction::verify(&mut fs, &layout(), &db, None).unwrap().iter().map(ToString::to_string).collect();
    assert_eq!(
        problems,
        [
            "/home/packages/bin/a of a is not what was installed",
            "/home/packages/share/a/c of a is missing",
            "/home/packages/lib/extra belongs to no package",
        ]
    );
    let one = transaction::verify(&mut fs, &layout(), &db, Some("a")).unwrap();
    assert!(one.iter().all(|p| !matches!(p, Problem::Stray(_))) && one.len() == 2);
}

/// A system with `hello 1.0` and `lib 1.0` installed, the change that the
/// crash tests cut short (hello upgraded, a directory of hello's becoming
/// a file and a file a directory, lib removed, `new` installed), and the
/// packages of that change.
fn before_the_change() -> (MemFiles, [(Vec<u8>, Offer); 2]) {
    let mut fs = system();
    let hello1 = package(
        manifest("hello 1.0"),
        &[("bin/hello", b"one"), ("share/hello/doc/a", b"a"), ("share/hello/doc/b", b"b"), ("share/hello/x", b"x")],
    );
    let lib = package(manifest("lib 1.0"), &[("lib/greet/libgreet", b"greet"), ("share/lib", b"lib")]);
    change(&mut fs, &[], &[&hello1, &lib], &["hello", "lib"]).unwrap();
    let hello2 = package(
        manifest("hello 2.0"),
        &[("bin/hello", b"two"), ("share/hello/doc", b"doc"), ("share/hello/x/y", b"y"), ("share/lib", b"now hello's")],
    );
    let new = package(manifest("new 1.0"), &[("lib/greet/libnew", b"new")]);
    (fs, [hello2, new])
}

/// Carries out the crash tests' change.
fn the_change(fs: &mut MemFiles, packages: &[(Vec<u8>, Offer); 2]) -> Result<Database, Error> {
    change(fs, &["hello", "lib"], &[&packages[0], &packages[1]], &["hello", "new"])
}

#[test]
fn every_change_cut_short_is_rolled_back_or_finished() {
    let (fs, packages) = before_the_change();
    let start = fs.nodes();
    let (before_live, before_db) = (live(&fs), transaction::load(&mut fs.clone(), &layout()).unwrap());
    // The change whole: how many changes of the file system it makes, and
    // what it leaves.
    let mut whole = MemFiles::from(start.clone());
    let base = whole.changes();
    let after_db = the_change(&mut whole, &packages).unwrap();
    let steps = whole.changes() - base;
    let after_live = live(&whole);
    assert_eq!(
        after_live,
        [
            "bin/",
            "bin/hello=two",
            "lib/",
            "lib/greet/",
            "lib/greet/libnew=new",
            "share/",
            "share/hello/",
            "share/hello/doc=doc",
            "share/hello/x/",
            "share/hello/x/y=y",
            "share/lib=now hello's",
        ]
    );
    assert!(steps > 20, "{steps} steps");
    let (mut rolled_back, mut finished) = (0, 0);
    for n in 0..steps {
        let mut fs = MemFiles::from(start.clone());
        fs.lose_power_after(n);
        // An error, unless the power went once the change was committed,
        // as it was tidying up.
        let _ = the_change(&mut fs, &packages);
        let off = fs.nodes();
        // The next start recovers; and so does every start after a
        // recovery cut short at any step of its own.
        let mut again = MemFiles::from(off.clone());
        let recovered = transaction::recover(&mut again, &layout()).unwrap();
        let recovery_steps = again.changes();
        for m in 0..recovery_steps {
            let mut cut = MemFiles::from(off.clone());
            cut.lose_power_after(m);
            assert!(transaction::recover(&mut cut, &layout()).is_err());
            let mut next = MemFiles::from(cut.nodes());
            transaction::recover(&mut next, &layout()).unwrap();
            assert_eq!(live(&next), live(&again), "change cut after {n}, recovery after {m}");
            assert_eq!(next.nodes(), again.nodes(), "change cut after {n}, recovery after {m}");
        }
        let db = transaction::load(&mut again, &layout()).unwrap();
        if db == before_db {
            assert_eq!(live(&again), before_live, "cut short after {n}: {recovered:?}");
            rolled_back += 1;
        } else {
            assert_eq!(db, after_db, "cut short after {n}");
            assert_eq!(live(&again), after_live, "cut short after {n}: {recovered:?}");
            finished += 1;
        }
        assert!(tidy(&again), "cut short after {n}: {:?}", again.tree("/home/.private/package-manager"));
        assert_eq!(transaction::verify(&mut again, &layout(), &db, None).unwrap(), []);
        // Recovering once more finds nothing to do, and the next change
        // goes ahead.
        assert_eq!(transaction::recover(&mut again, &layout()).unwrap(), Recovery::Nothing);
    }
    assert!(rolled_back > 0 && finished > 0, "{rolled_back} rolled back, {finished} finished");
}

#[test]
fn a_change_waits_for_recovery() {
    let (mut fs, packages) = before_the_change();
    let start = fs.nodes();
    let base = fs.changes();
    the_change(&mut fs, &packages).unwrap();
    let steps = fs.changes() - base;
    // Cut short in the middle: a journal is left.
    let mut cut = MemFiles::from(start);
    cut.lose_power_after(steps / 2);
    assert!(the_change(&mut cut, &packages).is_err());
    let mut next = MemFiles::from(cut.nodes());
    assert!(next.kind("/home/.private/package-manager/journal.json").unwrap().is_some());
    assert_eq!(the_change(&mut next, &packages).unwrap_err(), Error::Interrupted);
    assert!(matches!(transaction::recover(&mut next, &layout()).unwrap(), Recovery::RolledBack | Recovery::Finished));
    the_change(&mut next, &packages).ok();
    assert_eq!(transaction::recover(&mut next, &layout()).unwrap(), Recovery::Nothing);
}

#[test]
fn a_damaged_journal_is_not_acted_on() {
    let mut fs = system();
    fs.put(
        "/home/.private/package-manager/journal.json",
        b"{\"format\":1,\"from\":0,\"to\":1,\"out\":[\"../../etc\"],\"cleared\":[],\"made\":[],\"into\":[]}",
    );
    let e = transaction::recover(&mut fs, &layout()).unwrap_err();
    assert!(e.to_string().contains("the record of a change cut short is damaged"), "{e}");
    fs.put(
        "/home/.private/package-manager/journal.json",
        b"{\"format\":1,\"from\":5,\"to\":6,\"out\":[],\"cleared\":[],\"made\":[],\"into\":[]}",
    );
    let e = transaction::recover(&mut fs, &layout()).unwrap_err();
    assert!(e.to_string().contains("generation 0"), "{e}");
    fs.put("/home/.private/package-manager/journal.json", b"not json");
    assert!(matches!(transaction::recover(&mut fs, &layout()), Err(Error::Journal(_))));
}

#[test]
fn staging_cut_short_is_discarded() {
    let mut fs = system();
    fs.put("/home/.private/package-manager/transaction/new/bin/half", b"ha");
    assert_eq!(transaction::recover(&mut fs, &layout()).unwrap(), Recovery::Discarded);
    assert!(tidy(&fs));
    assert_eq!(transaction::recover(&mut fs, &layout()).unwrap(), Recovery::Nothing);
}

#[test]
fn the_limits_are_consistent() {
    // A package's file and its unpacked files fit what an index offers.
    let m = manifest("x 1.0");
    let offer = Offer {
        manifest: m,
        file: String::from("x.vpkg"),
        size: limits::ARCHIVE,
        sha256: Digest::default(),
        unpacked: limits::UNPACKED,
    };
    let contents = crate::index::Contents {
        repository: String::from("r"),
        serial: 1,
        generated: 0,
        expires: None,
        packages: vec![offer],
    };
    assert!(contents.validate().is_ok());
    let mut nodes = BTreeMap::new();
    nodes.insert(String::from("/x"), Node::Dir);
    assert_eq!(MemFiles::from(nodes).tree("/x"), BTreeMap::new());
}

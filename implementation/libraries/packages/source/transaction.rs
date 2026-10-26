// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Carrying a change out, so that one cut short is never left half done.
//!
//! The installed packages' files are in the *live* directory
//! (`/home/packages`); the packages service keeps its database, a journal
//! and the change under way in its *state* directory
//! (`/home/.private/packages`), on the same file system, where moving a
//! file is one step ([`Layout`]). A change ([`apply`]):
//!
//! 1. **Stages** every package it installs: unpacks it into
//!    `transaction/new`, each file checked against its SHA-256 as it ends,
//!    after checking that the package is what its repository's index says
//!    and that none of its files would take another package's place.
//!    Nothing live has changed yet: anything that fails here (a damaged
//!    package, no room) leaves the system as it was.
//! 2. **Journals** what it is about to do: the files it moves out of the
//!    live directory and into it, the directories it removes and makes,
//!    and the database's generation before and after. The journal is
//!    written whole under another name, then put in place.
//! 3. **Moves** the files of the packages that go into `transaction/old`,
//!    makes and removes directories, and moves the new files into place.
//! 4. **Commits**: puts the new database in place, in one step. The
//!    change is made, as a whole, at that moment.
//! 5. **Finishes**: removes `transaction`, the directories emptied, and
//!    the journal.
//!
//! No package runs anything as it installs, so there is nothing else a
//! change does. Should it be cut short anywhere (power lost, the service
//! ended), the next start [`recover`]s: with no journal, nothing live had
//! changed, and what was staged goes; with one, the database's generation
//! says whether the change was committed: if it was, recovery finishes it;
//! if not, it rolls it back, moving every file back to where it was. Each
//! step of recovery looks before it acts, so recovery cut short is
//! recovered from too. Either way the system is as it was before the
//! change or as it is after, never between, which the tests check by
//! cutting changes and recoveries short at every step.

use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use serde::{Deserialize, Serialize};

use crate::archive::{self, Archive, Entry, Piece, UnpackError};
use crate::database::{BadDatabase, Database, Installed};
use crate::index::Offer;
use crate::name::Name;
use crate::requirement::Requirement;
use crate::{Digest, Hasher, limits};

/// What is at a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

/// How a file system operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    NotFound,
    NoSpace,
    Denied,
    Other,
}

/// A file system operation that failed: how, and in the file system's
/// words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesError {
    pub failure: Failure,
    pub detail: String,
}

impl fmt::Display for FilesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

/// A file being written.
pub trait Sink {
    fn write(&mut self, data: &[u8]) -> Result<(), FilesError>;
    /// Ends the file: it is whole once this returns.
    fn finish(self: Box<Self>) -> Result<(), FilesError>;
}

/// The file system a change is carried out on. Paths are absolute.
pub trait Files {
    /// What is at `path`, if anything.
    fn kind(&mut self, path: &str) -> Result<Option<Kind>, FilesError>;
    /// The file at `path`, if it is at most `limit` bytes.
    fn read(&mut self, path: &str, limit: u64) -> Result<Vec<u8>, FilesError>;
    /// Creates the file `path` (replacing one there), to be written in
    /// pieces. Its directory exists.
    fn create(&mut self, path: &str) -> Result<Box<dyn Sink>, FilesError>;
    /// Moves the file `from` to `to`, replacing a file there, in one step.
    fn rename(&mut self, from: &str, to: &str) -> Result<(), FilesError>;
    fn remove_file(&mut self, path: &str) -> Result<(), FilesError>;
    /// Makes the directory `path`, whose parent exists.
    fn make_dir(&mut self, path: &str) -> Result<(), FilesError>;
    /// Removes the empty directory `path`.
    fn remove_dir(&mut self, path: &str) -> Result<(), FilesError>;
    /// The names in the directory `path`.
    fn list(&mut self, path: &str) -> Result<Vec<String>, FilesError>;
    /// Brings every change to persistent storage.
    fn sync(&mut self) -> Result<(), FilesError>;
}

/// Where the installed files and the service's own are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The installed packages' files (`/home/packages`).
    pub live: String,
    /// The service's directory (`/home/.private/packages`).
    pub state: String,
}

impl Layout {
    pub fn database(&self) -> String {
        alloc::format!("{}/installed.json", self.state)
    }

    pub fn journal(&self) -> String {
        alloc::format!("{}/journal.json", self.state)
    }

    fn staging(&self) -> String {
        alloc::format!("{}/transaction", self.state)
    }

    fn staged(&self) -> String {
        alloc::format!("{}/transaction/new", self.state)
    }

    fn kept(&self) -> String {
        alloc::format!("{}/transaction/old", self.state)
    }

    fn live(&self, path: &str) -> String {
        alloc::format!("{}/{path}", self.live)
    }
}

/// What went wrong with a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The file system failed: what was being done, where, and how.
    Files {
        doing: &'static str,
        path: String,
        error: FilesError,
    },
    /// A package is damaged (which, and how).
    Archive {
        package: String,
        error: archive::Error,
    },
    /// A package is not what its repository's index says.
    Mismatch(String),
    /// A file of `package` would take the place of `path`, which belongs
    /// to `owner` (none: no package's).
    Conflict {
        package: String,
        path: String,
        owner: Option<String>,
    },
    /// The package is not installed.
    NotInstalled(String),
    /// A change was cut short: [`recover`] first.
    Interrupted,
    Database(BadDatabase),
    /// The journal of a change cut short does not read, or does not fit
    /// the database.
    Journal(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Files { doing, path, error } => {
                write!(f, "could not {doing} {path}: {error}")?;
                if error.failure == Failure::NoSpace {
                    f.write_str(" (there is no room left: remove something, then try again)")?;
                }
                Ok(())
            }
            Error::Archive { package, error } => write!(f, "{package}: {error}"),
            Error::Mismatch(package) => {
                write!(f, "{package} is not the package its repository's index says it is: it was not installed")
            }
            Error::Conflict { package, path, owner: Some(owner) } => {
                write!(f, "{package} has a file at {path}, which belongs to {owner}")
            }
            Error::Conflict { package, path, owner: None } => {
                write!(f, "{package} has a file at {path}, where something of no package's is")
            }
            Error::NotInstalled(name) => write!(f, "{name} is not installed"),
            Error::Interrupted => f.write_str("a change was cut short and is to be recovered from first"),
            Error::Database(e) => e.fmt(f),
            Error::Journal(why) => write!(f, "the record of a change cut short is damaged: {why}"),
        }
    }
}

/// A package to install: where its file is (on the file system the change
/// is made on), what its repository's index offers of it (its file's size
/// and SHA-256, its manifest, which the file must match), and the
/// repository's name.
#[derive(Debug, Clone, Copy)]
pub struct Incoming<'a> {
    pub path: &'a str,
    pub offer: &'a Offer,
    pub repository: &'a str,
}

/// Reads `incoming`'s package file, which must be what its offer says.
fn read_package(fs: &mut dyn Files, incoming: &Incoming) -> Result<Vec<u8>, Error> {
    let label = incoming.offer.manifest.label();
    let bytes = fs.read(incoming.path, incoming.offer.size).map_err(|error| match error.failure {
        Failure::NotFound => Error::Files { doing: "read", path: incoming.path.to_string(), error },
        _ => Error::Mismatch(label.clone()),
    })?;
    if bytes.len() as u64 != incoming.offer.size || Digest::of(&bytes) != incoming.offer.sha256 {
        return Err(Error::Mismatch(label));
    }
    Ok(bytes)
}

/// A change: packages that go (removed, or replaced by another version of
/// theirs), packages installed, and the world after.
#[derive(Debug, Clone)]
pub struct Change<'a> {
    pub remove: Vec<Name>,
    pub install: Vec<Incoming<'a>>,
    pub world: Vec<Requirement>,
    /// The time now (Unix time, s), recorded as when packages were
    /// installed.
    pub now: u64,
}

/// How a change goes, as [`apply`] tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step<'a> {
    /// Unpacking `package`: `done` bytes of its files, of `total`.
    Unpacking { package: &'a str, done: u64, total: u64 },
    /// Everything unpacked and checked: the files go into place, and the
    /// change is committed.
    Committing,
}

/// What [`recover`] found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// No change was cut short.
    Nothing,
    /// One was cut short before anything live changed: what it staged is
    /// gone.
    Discarded,
    /// One was cut short before it committed: it was rolled back.
    RolledBack,
    /// One was cut short after it committed: it was finished.
    Finished,
}

/// The journal of a change under way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    format: u32,
    /// The database's generation before the change, and after.
    from: u64,
    to: u64,
    /// Files moved out of the live directory (relative paths).
    out: Vec<String>,
    /// Directories removed from it to make room for files, deepest first.
    cleared: Vec<String>,
    /// Directories made in it, parents first.
    made: Vec<String>,
    /// Files moved into it.
    into: Vec<String>,
}

const JOURNAL_FORMAT: u32 = 1;

/// `error` from the file system, while `doing` something to `path`.
fn files<'p>(doing: &'static str, path: &'p str) -> impl FnOnce(FilesError) -> Error + 'p {
    move |error| Error::Files { doing, path: path.to_string(), error }
}

/// The directories `path` is in, from the outermost (`a`, `a/b` for
/// `a/b/c`).
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(move |(i, _)| &path[..i])
}

/// Writes `data` as `path`, whole: under another name first, then put in
/// place.
fn write_whole(fs: &mut dyn Files, path: &str, data: &[u8]) -> Result<(), Error> {
    let new = alloc::format!("{path}.new");
    let mut sink = fs.create(&new).map_err(files("write", &new))?;
    sink.write(data).map_err(files("write", &new))?;
    sink.finish().map_err(files("write", &new))?;
    fs.rename(&new, path).map_err(files("put in place", path))
}

/// Removes `path` and everything in it, if it is there.
fn remove_all(fs: &mut dyn Files, path: &str) -> Result<(), Error> {
    match fs.kind(path).map_err(files("look at", path))? {
        None => Ok(()),
        Some(Kind::File) => fs.remove_file(path).map_err(files("remove", path)),
        Some(Kind::Dir) => {
            for name in fs.list(path).map_err(files("list", path))? {
                remove_all(fs, &alloc::format!("{path}/{name}"))?;
            }
            fs.remove_dir(path).map_err(files("remove", path))
        }
    }
}

/// Makes the directories `path` is in (absolute), as far as they are not
/// there, from `base` (which is).
fn make_parents(fs: &mut dyn Files, base: &str, relative: &str) -> Result<(), Error> {
    for dir in ancestors(relative) {
        let full = alloc::format!("{base}/{dir}");
        if fs.kind(&full).map_err(files("look at", &full))?.is_none() {
            fs.make_dir(&full).map_err(files("make", &full))?;
        }
    }
    Ok(())
}

/// Reads the database (none yet: nothing installed).
pub fn load(fs: &mut dyn Files, layout: &Layout) -> Result<Database, Error> {
    let path = layout.database();
    match fs.kind(&path).map_err(files("look at", &path))? {
        None => Ok(Database::new()),
        Some(_) => {
            let bytes = fs.read(&path, limits::DATABASE as u64).map_err(files("read", &path))?;
            Database::parse(&bytes).map_err(Error::Database)
        }
    }
}

/// Recovers from a change cut short, as the module's documentation says.
pub fn recover(fs: &mut dyn Files, layout: &Layout) -> Result<Recovery, Error> {
    let journal = layout.journal();
    if fs.kind(&journal).map_err(files("look at", &journal))?.is_none() {
        let staged = fs.kind(&layout.staging()).map_err(files("look at", &layout.staging()))?.is_some();
        remove_all(fs, &alloc::format!("{journal}.new"))?;
        remove_all(fs, &alloc::format!("{}.new", layout.database()))?;
        remove_all(fs, &layout.staging())?;
        return Ok(if staged { Recovery::Discarded } else { Recovery::Nothing });
    }
    let bytes = fs.read(&journal, limits::JOURNAL as u64).map_err(files("read", &journal))?;
    let j: Journal = serde_json::from_slice(&bytes).map_err(|e| Error::Journal(alloc::format!("{e}")))?;
    if j.format != JOURNAL_FORMAT || [&j.out, &j.cleared, &j.made, &j.into].iter().any(|l| l.iter().any(|p| !plain(p)))
    {
        return Err(Error::Journal(String::from("it names paths a change cannot have made")));
    }
    let db = load(fs, layout)?;
    if db.generation == j.to {
        finish(fs, layout, &j)?;
        Ok(Recovery::Finished)
    } else if db.generation == j.from {
        roll_back(fs, layout, &j)?;
        Ok(Recovery::RolledBack)
    } else {
        Err(Error::Journal(alloc::format!(
            "it is of a change from generation {} to {}, and the database is of generation {}",
            j.from,
            j.to,
            db.generation
        )))
    }
}

/// Whether `path` is one a journal may name: relative, of components that
/// are neither empty, `.` nor `..`.
fn plain(path: &str) -> bool {
    !path.is_empty() && path.len() <= limits::PATH && path.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

/// Undoes what a change not committed did: the files it moved in go, the
/// directories it made go and those it removed come back, and the files it
/// moved out go back where they were.
fn roll_back(fs: &mut dyn Files, layout: &Layout, j: &Journal) -> Result<(), Error> {
    let (new, old) = (layout.staged(), layout.kept());
    let out: BTreeSet<&str> = j.out.iter().map(String::as_str).collect();
    for p in j.into.iter().rev() {
        let (staged, kept, live) = (alloc::format!("{new}/{p}"), alloc::format!("{old}/{p}"), layout.live(p));
        // Moved in, as it is no longer staged: take it out again; unless
        // the file there is the one it replaced, which a recovery cut
        // short put back already (or never moved out).
        let moved_in = fs.kind(&staged).map_err(files("look at", &staged))?.is_none();
        let replaced_and_kept = out.contains(p.as_str()) && fs.kind(&kept).map_err(files("look at", &kept))?.is_none();
        if moved_in && !replaced_and_kept && fs.kind(&live).map_err(files("look at", &live))? == Some(Kind::File) {
            fs.remove_file(&live).map_err(files("remove", &live))?;
        }
    }
    for d in j.made.iter().rev() {
        let live = layout.live(d);
        if fs.kind(&live).map_err(files("look at", &live))? == Some(Kind::Dir) {
            fs.remove_dir(&live).map_err(files("remove", &live))?;
        }
    }
    for d in j.cleared.iter().rev() {
        let live = layout.live(d);
        if fs.kind(&live).map_err(files("look at", &live))?.is_none() {
            fs.make_dir(&live).map_err(files("make", &live))?;
        }
    }
    for p in &j.out {
        let kept = alloc::format!("{old}/{p}");
        if fs.kind(&kept).map_err(files("look at", &kept))? == Some(Kind::File) {
            let live = layout.live(p);
            fs.rename(&kept, &live).map_err(files("put back", &live))?;
        }
    }
    remove_all(fs, &alloc::format!("{}.new", layout.database()))?;
    remove_all(fs, &layout.staging())?;
    fs.sync().map_err(files("save", &layout.live))?;
    let journal = layout.journal();
    remove_all(fs, &journal)?;
    fs.sync().map_err(files("save", &layout.live))
}

/// Finishes a change committed: what it moved out goes, and so do the
/// directories that held only that.
fn finish(fs: &mut dyn Files, layout: &Layout, j: &Journal) -> Result<(), Error> {
    remove_all(fs, &layout.staging())?;
    let mut emptied: BTreeSet<&str> = BTreeSet::new();
    for p in &j.out {
        emptied.extend(ancestors(p));
    }
    // Deepest first, so that a directory's own directories go before it.
    for d in emptied.iter().rev() {
        let live = layout.live(d);
        if fs.kind(&live).map_err(files("look at", &live))? == Some(Kind::Dir)
            && fs.list(&live).map_err(files("list", &live))?.is_empty()
        {
            fs.remove_dir(&live).map_err(files("remove", &live))?;
        }
    }
    let journal = layout.journal();
    remove_all(fs, &journal)
}

/// A package being installed, as its file lists it.
struct Arriving {
    manifest: crate::manifest::Manifest,
    files: Vec<Entry>,
}

/// The files of a package being unpacked into staging, one at a time.
struct Unpacking<'f> {
    fs: &'f mut dyn Files,
    base: String,
    /// Directories made in staging.
    made: BTreeSet<String>,
    file: Option<(String, Box<dyn Sink>)>,
}

impl Unpacking<'_> {
    fn piece(&mut self, piece: Piece<'_>) -> Result<(), Error> {
        match piece {
            Piece::Begin(entry) => {
                for dir in ancestors(&entry.path) {
                    if self.made.insert(dir.to_string()) {
                        let full = alloc::format!("{}/{dir}", self.base);
                        if self.fs.kind(&full).map_err(files("look at", &full))?.is_none() {
                            self.fs.make_dir(&full).map_err(files("make", &full))?;
                        }
                    }
                }
                let full = alloc::format!("{}/{}", self.base, entry.path);
                let sink = self.fs.create(&full).map_err(files("write", &full))?;
                self.file = Some((full, sink));
            }
            Piece::Data(data) => {
                if let Some((path, sink)) = &mut self.file {
                    sink.write(data).map_err(files("write", path))?;
                }
            }
            Piece::End(_) => {
                if let Some((path, sink)) = self.file.take() {
                    sink.finish().map_err(files("write", &path))?;
                }
            }
        }
        Ok(())
    }
}

/// Carries `change` out on what `db` records (the database as it is), as
/// the module's documentation says; returns the database after.
/// `progress` hears how it goes.
pub fn apply(
    fs: &mut dyn Files,
    layout: &Layout,
    db: &Database,
    change: &Change,
    progress: &mut dyn FnMut(Step),
) -> Result<Database, Error> {
    let journal_path = layout.journal();
    if fs.kind(&journal_path).map_err(files("look at", &journal_path))?.is_some() {
        return Err(Error::Interrupted);
    }
    remove_all(fs, &layout.staging())?;

    // What goes (the files of the packages going that are there: the
    // journal names only what is moved, so that what a rollback finds
    // tells it what was), and what comes, each checked.
    let mut out: BTreeSet<String> = BTreeSet::new();
    for name in &change.remove {
        let p = db.get(name.as_str()).ok_or_else(|| Error::NotInstalled(name.to_string()))?;
        for f in &p.files {
            let live = layout.live(&f.path);
            if fs.kind(&live).map_err(files("look at", &live))? == Some(Kind::File) {
                out.insert(f.path.clone());
            }
        }
    }
    // Each package read and checked, and its list of files kept; its file
    // is read again to be unpacked, so that no more than one package is in
    // memory at a time.
    let mut archives: Vec<(Arriving, &Incoming)> = Vec::new();
    for incoming in &change.install {
        let label = incoming.offer.manifest.label();
        let bytes = read_package(fs, incoming)?;
        let archive = Archive::open(&bytes).map_err(|error| Error::Archive { package: label.clone(), error })?;
        if archive.manifest != incoming.offer.manifest || archive.unpacked() != incoming.offer.unpacked {
            return Err(Error::Mismatch(label));
        }
        archives.push((Arriving { manifest: archive.manifest, files: archive.files }, incoming));
    }
    let staying = |path: &str| db.owner(path).filter(|p| !change.remove.contains(&p.manifest.name));
    let mut arriving: BTreeSet<&str> = BTreeSet::new();
    for (archive, _) in &archives {
        let label = archive.manifest.label();
        for e in &archive.files {
            let conflict = |path: &str, owner: Option<String>| Error::Conflict {
                package: label.clone(),
                path: alloc::format!("{}/{path}", layout.live),
                owner,
            };
            if !arriving.insert(&e.path) {
                return Err(conflict(&e.path, Some(String::from("another package being installed"))));
            }
            if let Some(owner) = staying(&e.path) {
                return Err(conflict(&e.path, Some(owner.manifest.name.to_string())));
            }
            for dir in ancestors(&e.path) {
                if let Some(owner) = staying(dir) {
                    return Err(conflict(dir, Some(owner.manifest.name.to_string())));
                }
            }
        }
    }
    // A file arriving inside another arriving one (`x` and `x/y`, from two
    // packages).
    for p in &arriving {
        if let Some(dir) = ancestors(p).find(|d| arriving.contains(d)) {
            return Err(Error::Conflict {
                package: String::from("a package being installed"),
                path: layout.live(dir),
                owner: Some(String::from("another package being installed")),
            });
        }
    }

    // Where the live directory stands in the way: a file there that is not
    // going, or a directory of files that are not all going.
    let mut cleared: Vec<String> = Vec::new();
    let mut made: Vec<String> = Vec::new();
    let mut made_set: BTreeSet<String> = BTreeSet::new();
    for (archive, _) in &archives {
        for e in &archive.files {
            let in_the_way = |path: &str| Error::Conflict {
                package: archive.manifest.label(),
                path: layout.live(path),
                owner: db.owner(path).map(|p| p.manifest.name.to_string()),
            };
            let live = layout.live(&e.path);
            match fs.kind(&live).map_err(files("look at", &live))? {
                None => {}
                Some(Kind::File) if out.contains(&e.path) => {}
                Some(Kind::File) => return Err(in_the_way(&e.path)),
                Some(Kind::Dir) => clear(fs, layout, &e.path, &out, &mut cleared).map_err(|_| in_the_way(&e.path))?,
            }
            for dir in ancestors(&e.path) {
                if made_set.contains(dir) {
                    continue;
                }
                let live = layout.live(dir);
                match fs.kind(&live).map_err(files("look at", &live))? {
                    Some(Kind::Dir) => {}
                    Some(Kind::File) if !out.contains(dir) => return Err(in_the_way(dir)),
                    _ => {
                        made_set.insert(dir.to_string());
                        made.push(dir.to_string());
                    }
                }
            }
        }
    }

    // Staged: every file unpacked and checked.
    let new = layout.staged();
    let staging = layout.staging();
    fs.make_dir(&staging).map_err(files("make", &staging))?;
    fs.make_dir(&new).map_err(files("make", &new))?;
    let mut installed: Vec<Installed> = Vec::new();
    for (arriving, incoming) in &archives {
        let label = arriving.manifest.label();
        let bytes = match read_package(fs, incoming) {
            Ok(bytes) => bytes,
            Err(e) => {
                let _ = remove_all(fs, &staging);
                return Err(e);
            }
        };
        let archive = match Archive::open(&bytes) {
            Ok(a) if a.manifest == arriving.manifest && a.files == arriving.files => a,
            _ => {
                let _ = remove_all(fs, &staging);
                return Err(Error::Mismatch(label));
            }
        };
        let (total, mut done) = (archive.unpacked(), 0u64);
        let package = archive.manifest.name.as_str();
        progress(Step::Unpacking { package, done: 0, total });
        let unpacked = {
            let mut unpacking = Unpacking { fs: &mut *fs, base: new.clone(), made: BTreeSet::new(), file: None };
            archive.unpack(|piece| {
                if let Piece::Data(d) = &piece {
                    done += d.len() as u64;
                }
                let ended = matches!(piece, Piece::End(_));
                unpacking.piece(piece)?;
                if ended {
                    progress(Step::Unpacking { package, done, total });
                }
                Ok::<(), Error>(())
            })
        };
        if let Err(e) = unpacked {
            let _ = remove_all(fs, &staging);
            return Err(match e {
                UnpackError::Archive(error) => Error::Archive { package: label, error },
                UnpackError::Sink(e) => e,
            });
        }
        installed.push(Installed {
            manifest: archive.manifest.clone(),
            repository: incoming.repository.to_string(),
            sha256: incoming.offer.sha256,
            installed: change.now,
            files: archive.files.clone(),
        });
    }

    // The database after.
    let mut after = db.clone();
    after.generation += 1;
    after.packages.retain(|p| !change.remove.contains(&p.manifest.name));
    after.packages.extend(installed);
    after.packages.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    after.world = Database::tidy_world(change.world.clone());
    after.validate().map_err(Error::Database)?;

    // Journalled, then carried out.
    progress(Step::Committing);
    let journal = Journal {
        format: JOURNAL_FORMAT,
        from: db.generation,
        to: after.generation,
        out: out.iter().cloned().collect(),
        cleared,
        made,
        into: arriving.iter().map(|p| p.to_string()).collect(),
    };
    if let Err(e) = write_whole(fs, &journal_path, &crate::to_json(&journal)) {
        let _ = remove_all(fs, &alloc::format!("{journal_path}.new"));
        let _ = remove_all(fs, &staging);
        return Err(e);
    }
    match carry_out(fs, layout, &journal, &after) {
        Ok(()) => {
            // Committed: what is left to do is tidying, which recovery
            // does as well should it not get done now.
            let _ = finish(fs, layout, &journal);
            Ok(after)
        }
        Err(e) => {
            // Not committed (the database is put in place last): rolled
            // back now, or at the next start if that fails too.
            let _ = roll_back(fs, layout, &journal);
            Err(e)
        }
    }
}

/// Removes from the live directory the directory `relative`, whose every
/// file is going (`out`), noting the directories in `cleared`, deepest
/// first; fails if something there is staying.
fn clear(
    fs: &mut dyn Files,
    layout: &Layout,
    relative: &str,
    out: &BTreeSet<String>,
    cleared: &mut Vec<String>,
) -> Result<(), ()> {
    let live = layout.live(relative);
    for name in fs.list(&live).map_err(|_| ())? {
        let inner = alloc::format!("{relative}/{name}");
        match fs.kind(&layout.live(&inner)).map_err(|_| ())? {
            Some(Kind::Dir) => clear(fs, layout, &inner, out, cleared)?,
            Some(Kind::File) if out.contains(&inner) => {}
            _ => return Err(()),
        }
    }
    cleared.push(relative.to_string());
    Ok(())
}

/// Steps 3 and 4 of a change: files out, directories cleared and made,
/// files in, the database in place.
fn carry_out(fs: &mut dyn Files, layout: &Layout, j: &Journal, after: &Database) -> Result<(), Error> {
    let (new, old) = (layout.staged(), layout.kept());
    fs.make_dir(&old).map_err(files("make", &old))?;
    for p in &j.out {
        let live = layout.live(p);
        if fs.kind(&live).map_err(files("look at", &live))? == Some(Kind::File) {
            make_parents(fs, &old, p)?;
            let kept = alloc::format!("{old}/{p}");
            fs.rename(&live, &kept).map_err(files("move", &live))?;
        }
    }
    for d in &j.cleared {
        let live = layout.live(d);
        fs.remove_dir(&live).map_err(files("remove", &live))?;
    }
    for d in &j.made {
        let live = layout.live(d);
        fs.make_dir(&live).map_err(files("make", &live))?;
    }
    for p in &j.into {
        let (staged, live) = (alloc::format!("{new}/{p}"), layout.live(p));
        fs.rename(&staged, &live).map_err(files("put in place", &live))?;
    }
    write_whole(fs, &layout.database(), &after.to_bytes())?;
    fs.sync().map_err(files("save", &layout.live))
}

/// Something wrong with what is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// A package's file is not there (the package, the path).
    Missing { package: String, path: String },
    /// A package's file is not what it installed.
    Changed { package: String, path: String },
    /// A file is in the live directory that no package installed.
    Stray(String),
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::Missing { package, path } => write!(f, "{path} of {package} is missing"),
            Problem::Changed { package, path } => write!(f, "{path} of {package} is not what was installed"),
            Problem::Stray(path) => write!(f, "{path} belongs to no package"),
        }
    }
}

/// Checks the installed files of `name` (every package's, if none) against
/// their sizes and SHA-256s, and, for every package, that the live
/// directory holds nothing that none installed.
pub fn verify(fs: &mut dyn Files, layout: &Layout, db: &Database, name: Option<&str>) -> Result<Vec<Problem>, Error> {
    let mut problems = Vec::new();
    for p in db.packages.iter().filter(|p| name.is_none_or(|n| p.manifest.name.as_str() == n)) {
        for f in &p.files {
            let live = layout.live(&f.path);
            let problem = |package: &Installed, path: &str, missing: bool| {
                let (package, path) = (package.manifest.name.to_string(), path.to_string());
                if missing { Problem::Missing { package, path } } else { Problem::Changed { package, path } }
            };
            match fs.kind(&live).map_err(files("look at", &live))? {
                None => problems.push(problem(p, &live, true)),
                Some(Kind::Dir) => problems.push(problem(p, &live, false)),
                Some(Kind::File) => match fs.read(&live, f.size) {
                    Ok(bytes) if same(&bytes, f) => {}
                    Ok(_) => problems.push(problem(p, &live, false)),
                    Err(e) if e.failure == Failure::NotFound => problems.push(problem(p, &live, true)),
                    // Larger than it was: changed.
                    Err(_) => problems.push(problem(p, &live, false)),
                },
            }
        }
    }
    if name.is_none() && fs.kind(&layout.live).map_err(files("look at", &layout.live))? == Some(Kind::Dir) {
        let owners = db.owners();
        strays(fs, layout, "", &owners, &mut problems)?;
    }
    Ok(problems)
}

/// Whether `bytes` are the file `entry` records.
fn same(bytes: &[u8], entry: &Entry) -> bool {
    let mut h = Hasher::new();
    h.update(bytes);
    bytes.len() as u64 == entry.size && h.finish() == entry.sha256
}

/// The files under `relative` (of the live directory) that no package
/// owns.
fn strays(
    fs: &mut dyn Files,
    layout: &Layout,
    relative: &str,
    owners: &alloc::collections::BTreeMap<&str, &Installed>,
    problems: &mut Vec<Problem>,
) -> Result<(), Error> {
    let dir = if relative.is_empty() { layout.live.clone() } else { layout.live(relative) };
    for name in fs.list(&dir).map_err(files("list", &dir))? {
        let inner = if relative.is_empty() { name } else { alloc::format!("{relative}/{name}") };
        let full = layout.live(&inner);
        match fs.kind(&full).map_err(files("look at", &full))? {
            Some(Kind::Dir) => strays(fs, layout, &inner, owners, problems)?,
            Some(Kind::File) if !owners.contains_key(inner.as_str()) => problems.push(Problem::Stray(full)),
            _ => {}
        }
    }
    Ok(())
}

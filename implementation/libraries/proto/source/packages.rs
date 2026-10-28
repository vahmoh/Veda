// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The package manager (`package-manager`): what is installed, what the
//! repositories offer, and changes to both.
//!
//! The service alone changes the installed packages (`/home/packages`,
//! which every program reads and none but the service writes) and the
//! repositories a system uses. Anyone may ask what is installed and what is
//! offered; only the Terminal (and so the programs it runs, such as `pkg`),
//! Settings and the programs the kernel command line asks for may change
//! them ([`PackageError::Denied`] for the rest), as installing software
//! changes what the computer runs.
//!
//! A change goes in two calls: [`packages::Client::plan`] says what a
//! request would do, which the user is shown; [`packages::Client::apply`]
//! carries it out, given the plan's digest, and only if the plan it works
//! out then is the same one (else [`PackageError::PlanChanged`]). Updating
//! the repositories and carrying a change out take time (downloads), so the
//! service answers them once done, answering other calls meanwhile, and
//! tells how they go on the channel the caller gives ([`PROGRESS`]
//! events, which a caller that falls behind misses some of). One change or
//! update runs at a time ([`PackageError::Busy`]).
//!
//! Lists come in pages, by name after the last of the page before, as
//! directories' listings do.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use ipc::{Bytes, enumeration, message, protocol, union};
use runtime::object::Channel;

/// The most entries a page of [`packages::Client::find`] or
/// [`packages::Client::files`] holds.
pub const PAGE: u32 = 100;
/// The most steps a [`Plan`] lists (it counts the rest).
pub const PLAN_STEPS: usize = 200;
/// The most problems a [`Report`] lists (it counts the rest).
pub const REPORT_PROBLEMS: usize = 200;

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PackageError {
        /// The caller may not change packages or repositories.
        Denied = 1,
        /// Another change or update is under way.
        Busy = 2,
        /// No package or repository has the name given.
        NotFound = 3,
        /// No packages satisfy the request (the detail says why).
        Unsatisfiable = 4,
        /// What the request would do is no longer what its plan said: show
        /// the new plan.
        PlanChanged = 5,
        /// A repository's index could not be had, or was refused.
        Repository = 6,
        /// A package could not be downloaded, or is not what its index
        /// says.
        Download = 7,
        /// A package, or what is installed, is damaged.
        Damaged = 8,
        /// A package would take another's place.
        Conflict = 9,
        /// There is no room for what the request installs.
        NoSpace = 10,
        /// A name, version, address or key in the request is not valid.
        Invalid = 11,
        /// The service could not do it (it could not keep what it keeps).
        Unavailable = 12,
    }
}

message! {
    /// Why a call failed: the kind, and in words, for people.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Failure {
        pub error: PackageError,
        pub detail: String,
    }
}

impl Failure {
    pub fn new(error: PackageError, detail: impl Into<String>) -> Failure {
        Failure { error, detail: detail.into() }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.detail.is_empty() {
            return f.write_str(&self.detail);
        }
        f.write_str(match self.error {
            PackageError::Denied => "Only the Terminal and Settings change the installed software.",
            PackageError::Busy => "Another change is under way: wait for it to end.",
            PackageError::NotFound => "There is no such package or repository.",
            PackageError::Unsatisfiable => "No packages satisfy that.",
            PackageError::PlanChanged => "What that would do has changed since it was shown.",
            PackageError::Repository => "A repository could not be read.",
            PackageError::Download => "A package could not be downloaded.",
            PackageError::Damaged => "A package, or what is installed, is damaged.",
            PackageError::Conflict => "A package would take another's place.",
            PackageError::NoSpace => "There is no room left.",
            PackageError::Invalid => "That is not a valid name, version, address or key.",
            PackageError::Unavailable => "The package manager could not do it.",
        })
    }
}

message! {
    /// The service's state.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct Status {
        /// A change or update is under way.
        pub busy: bool,
        pub installed: u32,
        pub repositories: u32,
        /// Why changes are refused, if they are (what is installed cannot
        /// be read); empty when they are not.
        pub problem: String,
    }
}

message! {
    /// A repository in use.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RepositoryInfo {
        pub name: String,
        pub title: String,
        pub url: String,
        /// The fingerprints of the keys it is trusted with.
        pub keys: Vec<String>,
        pub enabled: bool,
        /// It is the system image's (disabled, never removed).
        pub system: bool,
        /// What its index offers, as of the index taken last: the number
        /// of packages, its serial, when it was taken and when it expires
        /// (Unix time, s; 0: never).
        pub packages: u32,
        pub serial: u64,
        pub updated: u64,
        pub expires: u64,
        /// Why it cannot be used now, if it cannot (no index taken yet, the
        /// index expired); empty when it can.
        pub problem: String,
    }
}

union! {
    /// A change to the repositories.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum RepositoryChange {
        /// Adds a repository: its name (the one its index gives), what
        /// people call it, its address, and the public keys it is trusted
        /// with (`ed25519:...`).
        1 => Add { name: String, title: String, url: String, keys: Vec<String> },
        /// Removes a repository the user added.
        2 => Remove { name: String },
        /// Enables or disables a repository.
        3 => Enable { name: String, enabled: bool },
    }
}

message! {
    /// What updating a repository came to.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Refreshed {
        pub repository: String,
        pub packages: u32,
        pub serial: u64,
        /// Its index was the one taken already.
        pub unchanged: bool,
        /// Why it could not be updated, if it could not.
        pub problem: String,
    }
}

enumeration! {
    /// Which packages [`packages::Client::find`] looks through.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Scope {
        /// What the enabled repositories offer, the newest version of each.
        Available = 1,
        /// What is installed.
        Installed = 2,
        /// What is installed and has a newer version where it came from.
        Upgradable = 3,
    }
}

message! {
    /// A package, in a list.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PackageSummary {
        pub name: String,
        /// The version listed: the newest offered, or the one installed.
        pub version: String,
        pub summary: String,
        /// The version installed, if one is.
        pub installed: String,
        /// The repository it is offered by, or was installed from.
        pub repository: String,
        /// The user asked for it by name (not only for another package).
        pub explicit: bool,
    }
}

message! {
    /// A version of a package, as people read it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Details {
        pub version: String,
        pub architecture: String,
        pub summary: String,
        pub description: String,
        pub licence: String,
        pub homepage: String,
        pub depends: Vec<String>,
        pub conflicts: Vec<String>,
        pub provides: Vec<String>,
        pub repository: String,
        /// Its package file's size, and its files' together.
        pub size: u64,
        pub unpacked: u64,
    }
}

message! {
    /// What is known of a package.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PackageInfo {
        pub name: String,
        /// The version installed, and when, how many files it has, and
        /// whether the user asked for it by name.
        pub installed: Option<Details>,
        pub installed_at: u64,
        pub files: u32,
        pub explicit: bool,
        /// The version installing it would install, if one is offered.
        pub candidate: Option<Details>,
        /// Every version offered, `VERSION (REPOSITORY)`, newest first.
        pub versions: Vec<String>,
    }
}

union! {
    /// A change to the installed packages. Packages are named with the
    /// version wanted, if one is (`hello`, `hello (>= 2)`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Request {
        /// Installs the packages, which the user then asks for by name.
        1 => Install { packages: Vec<String> },
        /// Removes the packages, and those installed only for them.
        2 => Remove { packages: Vec<String> },
        /// Upgrades the packages (every one, for none) to the newest
        /// versions their repositories offer.
        3 => Upgrade { packages: Vec<String> },
    }
}

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Action {
        Install = 1,
        Upgrade = 2,
        Downgrade = 3,
        Remove = 4,
    }
}

message! {
    /// A package's step of a plan.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PlanStep {
        pub action: Action,
        pub name: String,
        /// The version installed now, and the one after (empty: none).
        pub from: String,
        pub to: String,
        /// Where the version after comes from, and its package file's
        /// size.
        pub repository: String,
        pub size: u64,
    }
}

message! {
    /// What a request would do.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Plan {
        /// The steps, by name; at most [`PLAN_STEPS`] of them.
        pub steps: Vec<PlanStep>,
        /// The steps beyond those listed.
        pub more: u32,
        /// The bytes to download, and how the installed size changes.
        pub download: u64,
        pub growth: i64,
        /// It changes what the user asked for by name (the world), even if
        /// it installs and removes nothing.
        pub world: bool,
        /// What to hand [`packages::Client::apply`] to carry it out.
        pub digest: Bytes,
        /// What the user should know of it (a package asked for that is
        /// installed already, ...).
        pub notes: Vec<String>,
    }
}

message! {
    /// What a change did.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct Outcome {
        pub installed: u32,
        pub upgraded: u32,
        pub downgraded: u32,
        pub removed: u32,
        /// Bytes downloaded.
        pub downloaded: u64,
    }
}

message! {
    /// Something wrong with what is installed.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Problem {
        /// The package (empty for a file of none).
        pub package: String,
        pub path: String,
        /// What is wrong, in words.
        pub what: String,
    }
}

message! {
    /// What checking the installed files found.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct Report {
        pub packages: u32,
        pub files: u32,
        /// At most [`REPORT_PROBLEMS`] of them.
        pub problems: Vec<Problem>,
        /// The problems beyond those listed.
        pub more: u32,
    }
}

enumeration! {
    /// What an update or change is doing.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Stage {
        /// Fetching a repository's index (`name` is the repository's).
        Index = 1,
        /// Downloading a package file.
        Download = 2,
        /// Unpacking a package.
        Unpack = 3,
        /// Every package unpacked and checked: putting their files in
        /// place.
        Commit = 4,
    }
}

message! {
    /// How an update or change goes, as [`PROGRESS`] events tell it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Progress {
        pub stage: Stage,
        pub name: String,
        /// Bytes done so far, of how many (0: not known).
        pub done: u64,
        pub total: u64,
    }
}

/// The ordinal of the events that tell how an update or change goes.
pub const PROGRESS: u32 = 1;

protocol! {
    /// The package manager.
    pub mod packages = "packages" {
        1 => fn status() -> Status;
        /// The repositories, in the order they are preferred.
        2 => fn repositories() -> Vec<RepositoryInfo>;
        /// The Terminal and Settings: adds, removes, enables or disables a
        /// repository.
        3 => fn configure(change: RepositoryChange) -> Result<(), Failure>;
        /// The Terminal and Settings: fetches every enabled repository's
        /// index anew; answered once done, with progress on `progress`.
        4 => fn update(progress: Option<Channel>) -> Result<Vec<Refreshed>, Failure>;
        /// A page of the packages in `scope` whose name or summary holds
        /// `query` (every one, for ""), by name, after `after`; at most
        /// `max` ([`PAGE`] at most).
        5 => fn find(query: String, scope: Scope, after: String, max: u32) -> Result<Vec<PackageSummary>, Failure>;
        6 => fn describe(name: String) -> Result<PackageInfo, Failure>;
        /// What `request` would do.
        7 => fn plan(request: Request) -> Result<Plan, Failure>;
        /// The Terminal and Settings: carries `request` out, if what it
        /// does is the plan of `digest`; answered once done, with progress
        /// on `progress`.
        8 => fn apply(request: Request, digest: Bytes, progress: Option<Channel>) -> Result<Outcome, Failure>;
        /// A page of the files of the installed package `name`, by path,
        /// after `after`.
        9 => fn files(name: String, after: String, max: u32) -> Result<Vec<String>, Failure>;
        /// The Terminal and Settings (it reads every file installed, which
        /// takes a while): checks the files of the installed package
        /// `name` (every one's, and that nothing else is there, for "")
        /// against what was installed; answered once done.
        10 => fn verify(name: String) -> Result<Report, Failure>;
        /// The Terminal and Settings: removes the downloaded package files
        /// kept; answers how many bytes they took.
        11 => fn clean() -> Result<u64, Failure>;
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec;

    use ipc::{Decode, Decoder, Encode, Encoder};

    use super::*;

    fn roundtrip<T: Encode + Decode>(v: T) -> T {
        let mut e = Encoder::new();
        v.encode(&mut e);
        let mut d = Decoder::new(&e.bytes, Vec::new());
        let out = T::decode(&mut d).unwrap();
        d.finish().unwrap();
        out
    }

    #[test]
    fn packages_travel_whole() {
        let plan = Plan {
            steps: vec![PlanStep {
                action: Action::Upgrade,
                name: "hello".into(),
                from: "1.0".into(),
                to: "2.0".into(),
                repository: "system".into(),
                size: 4096,
            }],
            more: 0,
            download: 4096,
            growth: -12,
            world: true,
            digest: Bytes(vec![7; 32]),
            notes: vec!["hello is asked for by name now".into()],
        };
        assert_eq!(roundtrip(plan.clone()), plan);
        for request in [
            Request::Install { packages: vec!["hello (>= 2)".into()] },
            Request::Remove { packages: vec!["hello".into()] },
            Request::Upgrade { packages: vec![] },
        ] {
            assert_eq!(roundtrip(request.clone()), request);
        }
        let change = RepositoryChange::Add {
            name: "extra".into(),
            title: "Extra".into(),
            url: "https://example.org/r".into(),
            keys: vec!["ed25519:x".into()],
        };
        assert_eq!(roundtrip(change.clone()), change);
        let failed: Result<Outcome, Failure> = Err(Failure::new(PackageError::Conflict, "hello has a file at ..."));
        assert_eq!(roundtrip(failed.clone()), failed);
        let progress = Progress { stage: Stage::Download, name: "hello".into(), done: 10, total: 20 };
        assert_eq!(roundtrip(progress.clone()), progress);
    }

    #[test]
    fn failures_say_what_to_do() {
        assert_eq!(
            Failure::new(PackageError::Busy, "").to_string(),
            "Another change is under way: wait for it to end."
        );
        assert_eq!(Failure::new(PackageError::Busy, "Updating.").to_string(), "Updating.");
    }

    #[test]
    fn the_largest_answers_fit_a_message() {
        // A full plan of the longest names and versions.
        let step = PlanStep {
            action: Action::Install,
            name: "n".repeat(64),
            from: "1".repeat(64),
            to: "2".repeat(64),
            repository: "r".repeat(32),
            size: u64::MAX,
        };
        let plan = Plan {
            steps: vec![step; PLAN_STEPS],
            more: 1,
            download: 0,
            growth: 0,
            world: false,
            digest: Bytes(vec![0; 32]),
            notes: Vec::new(),
        };
        let mut e = Encoder::new();
        plan.encode(&mut e);
        assert!(e.bytes.len() + ipc::HEADER_LEN < abi::CHANNEL_MAX_BYTES, "{} bytes", e.bytes.len());
        // A page of the longest summaries.
        let summary = PackageSummary {
            name: "n".repeat(64),
            version: "1".repeat(64),
            summary: "s".repeat(200),
            installed: "1".repeat(64),
            repository: "r".repeat(32),
            explicit: true,
        };
        let mut e = Encoder::new();
        vec![summary; PAGE as usize].encode(&mut e);
        assert!(e.bytes.len() + ipc::HEADER_LEN < abi::CHANNEL_MAX_BYTES, "{} bytes", e.bytes.len());
    }
}

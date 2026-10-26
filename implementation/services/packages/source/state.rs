// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the service knows: the repositories in use (the system image's and
//! the user's, [`packages::config`]), the index taken of each, what is
//! installed, and what the system image provides; and what it answers from
//! them: lists, details, and the plan of a request.
//!
//! The service keeps, in its private directory (`/home/.private/packages`):
//!
//! | Path | What |
//! |------|------|
//! | `installed.json` | the database of installed packages |
//! | `repositories.json` | the repositories the user added, and which they enabled or disabled |
//! | `indexes/NAME.vidx` | the index taken last of each repository not in the system image, as it was signed |
//! | `cache/SHA256.vpkg` | package files downloaded for a change, until it is made |
//! | `journal.json`, `transaction/` | a change under way ([`packages::transaction`]) |
//!
//! What is read back is checked again: an index's signatures against its
//! repository's keys (as they are now), the database and the configuration
//! as they are parsed. The system image's repositories are read from the
//! image at every start, and nothing of them is kept: an image started from
//! later, newer or older, is the one whose repositories are used.

use std::collections::BTreeMap;
use std::sync::Arc;

use packages::config::{self, Origin, Repository, UserConfig};
use packages::database::Database;
use packages::date;
use packages::index::{self, Offer, Verified};
use packages::manifest::{Architecture, Manifest};
use packages::name::Name;
use packages::plan::{self, Action as PlanAction};
use packages::requirement::{Provision, Requirement};
use packages::resolve::{self, Available, Installed, Problem, Upgrade};
use packages::stdfs::StdFiles;
use packages::transaction::{self, Layout, Recovery};
use proto::packages::{
    Action, Details, Failure, PLAN_STEPS, PackageError, PackageInfo, PackageSummary, Plan, PlanStep, RepositoryChange,
    RepositoryInfo, Request, Scope, Status,
};
use rt::println;

/// The installed packages' files.
pub const LIVE: &str = "/home/packages";
/// The service's private directory.
pub const STATE: &str = "/home/.private/packages";
const USER_CONFIG: &str = "/home/.private/packages/repositories.json";
const INDEXES: &str = "/home/.private/packages/indexes";
const CACHE: &str = "/home/.private/packages/cache";
/// The system image's repositories, and what it provides.
const SYSTEM_REPOSITORIES: &str = "/system/etc/packages/repositories";
const SYSTEM_PROVIDES: &str = "/system/etc/packages/provides";
/// The most versions [`State::describe`] lists, and the most relations of
/// each kind its details give.
const VERSIONS: usize = 32;
const RELATIONS: usize = 32;

/// The time now, as indexes give it (Unix time, s, UTC).
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// An index taken, and when.
#[derive(Debug)]
pub struct Taken {
    pub verified: Verified,
    pub at: u64,
}

impl Taken {
    pub fn offers(&self) -> &[Offer] {
        &self.verified.contents.packages
    }

    /// Whether it has expired by `now`.
    pub fn expired(&self, now: u64) -> bool {
        self.verified.contents.expires.is_some_and(|at| now >= at)
    }
}

/// A package to install, as a plan has it: what its repository offers,
/// and where the repository is.
#[derive(Debug, Clone)]
pub struct Install {
    pub offer: Offer,
    pub repository: String,
    pub url: config::Url,
}

/// A request worked out: its plan, and what carrying it out takes.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub plan: Plan,
    pub installs: Vec<Install>,
    /// The installed packages that go (removed, or replaced).
    pub removes: Vec<Name>,
    pub world: Vec<Requirement>,
    /// The steps, for the log.
    pub steps: Vec<PlanStep>,
}

/// What the service knows.
pub struct State {
    pub layout: Layout,
    system: Vec<Repository>,
    user: UserConfig,
    /// The user's configuration is there but could not be read: it is left
    /// as it is, and the repositories cannot be changed.
    user_damaged: bool,
    pub repositories: Vec<Repository>,
    pub indexes: BTreeMap<String, Arc<Taken>>,
    pub db: Arc<Database>,
    provides: Vec<Provision>,
    /// Why changes are refused, if they are.
    pub problem: Option<String>,
}

/// A failure of `error`, in words.
pub fn failure(error: PackageError, detail: impl Into<String>) -> Failure {
    Failure::new(error, detail)
}

/// Where `offer`'s package file is kept once downloaded.
pub fn cached(offer: &Offer) -> String {
    format!("{CACHE}/{}.vpkg", offer.sha256.hex())
}

/// Where the index of the repository `name` is kept.
fn stored_index(name: &str) -> String {
    format!("{INDEXES}/{name}.vidx")
}

/// The system image's repositories, each file of its directory read.
fn system_repositories() -> Vec<Repository> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(SYSTEM_REPOSITORIES) else {
        println!("the system image has no repositories ({SYSTEM_REPOSITORIES})");
        return found;
    };
    let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    for file in names {
        let Some(name) = file.strip_suffix(".repo") else { continue };
        let path = format!("{SYSTEM_REPOSITORIES}/{file}");
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| config::system_repository(name, &t).map_err(|e| e.to_string()))
        {
            Ok(r) => found.push(r),
            Err(e) => println!("the system image's repository {name} is not used: {e}"),
        }
    }
    found
}

/// What the system image provides, one provision a line (`veda (= 0.1.0)`).
fn system_provides() -> Vec<Provision> {
    let text = std::fs::read_to_string(SYSTEM_PROVIDES).unwrap_or_default();
    let mut provides = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        match Provision::parse(line) {
            Ok(p) => provides.push(p),
            Err(e) => println!("{SYSTEM_PROVIDES}: {e}"),
        }
    }
    provides
}

/// Writes `data` as `path`, whole: under another name, then put in place.
pub fn write_whole(path: &str, data: &[u8]) -> std::io::Result<()> {
    let new = format!("{path}.new");
    std::fs::write(&new, data)?;
    std::fs::rename(&new, path)
}

/// Details of a version, as people read them.
fn details(m: &Manifest, repository: &str, size: u64, unpacked: u64) -> Details {
    let texts = |list: Vec<String>| list.into_iter().take(RELATIONS).collect();
    Details {
        version: m.version.to_string(),
        architecture: m.architecture.to_string(),
        summary: m.summary.clone(),
        description: m.description.clone(),
        licence: m.licence.clone(),
        homepage: m.homepage.clone(),
        depends: texts(m.depends.iter().map(ToString::to_string).collect()),
        conflicts: texts(m.conflicts.iter().map(ToString::to_string).collect()),
        provides: texts(m.provides.iter().map(ToString::to_string).collect()),
        repository: repository.to_string(),
        size,
        unpacked,
    }
}

/// Whether `m` matches `query` (in its name or summary, in any case).
fn matches(m: &Manifest, query: &str) -> bool {
    let query = query.to_lowercase();
    m.name.as_str().contains(&query) || m.summary.to_lowercase().contains(&query)
}

/// Reads requirements as a request names them: `hello`, `hello (>= 2)`.
fn requirements(packages: &[String]) -> Result<Vec<Requirement>, Failure> {
    packages.iter().map(|p| Requirement::parse(p).map_err(|e| failure(PackageError::Invalid, e.to_string()))).collect()
}

impl State {
    /// What the service knows at its start: a change cut short recovered
    /// from, the database read, the repositories and their indexes.
    pub fn load() -> State {
        let layout = Layout { live: LIVE.into(), state: STATE.into() };
        for dir in [STATE, INDEXES, CACHE] {
            if let Err(e) = std::fs::create_dir_all(dir) {
                println!("cannot make {dir}: {e}");
            }
        }
        // The VFS makes it at every start; a home disk saved before it did
        // may lack it until then.
        let _ = std::fs::create_dir(LIVE);
        let mut problem = None;
        let mut files = StdFiles { anchor: STATE.into() };
        match transaction::recover(&mut files, &layout) {
            Ok(Recovery::Nothing) => {}
            Ok(Recovery::Discarded) => println!("a change cut short before it began was discarded"),
            Ok(Recovery::RolledBack) => {
                println!("a change cut short was rolled back: the system is as it was before it")
            }
            Ok(Recovery::Finished) => println!("a change cut short after it was made was finished"),
            Err(e) => {
                println!("cannot recover from a change cut short: {e}");
                problem = Some(e.to_string());
            }
        }
        let db = match transaction::load(&mut files, &layout) {
            Ok(db) => db,
            Err(e) => {
                println!("{e}: nothing can be installed or removed");
                problem.get_or_insert(e.to_string());
                Database::new()
            }
        };
        let system = system_repositories();
        let (user, user_damaged) = match std::fs::read(USER_CONFIG) {
            Err(_) => (UserConfig::new(), false),
            Ok(bytes) => match UserConfig::parse(&bytes) {
                Ok(c) => (c, false),
                Err(e) => {
                    println!("the repositories' configuration is damaged ({e}): the system image's alone are used");
                    (UserConfig::new(), true)
                }
            },
        };
        let mut state = State {
            layout,
            system,
            user,
            user_damaged,
            repositories: Vec::new(),
            indexes: BTreeMap::new(),
            db: Arc::new(db),
            provides: system_provides(),
            problem,
        };
        state.merge();
        for r in state.repositories.clone() {
            state.take_stored(&r);
        }
        state
    }

    /// Puts the system's and the user's repositories together.
    fn merge(&mut self) {
        let (repositories, shadowed) = config::merge(self.system.clone(), &self.user);
        for name in shadowed {
            println!("the repository {name} that was added is not used: the system image has one of that name");
        }
        self.repositories = repositories;
        // Indexes of repositories no longer in use are forgotten.
        let names: Vec<&String> = self.repositories.iter().map(|r| &r.name).collect();
        self.indexes.retain(|name, _| names.contains(&name));
    }

    /// Takes the index kept of `r` (the system image's own, for one in
    /// the image), checking it again.
    fn take_stored(&mut self, r: &Repository) {
        let path = if r.url.in_system_image() { r.url.join(index::FILE) } else { stored_index(&r.name) };
        let Ok(bytes) = std::fs::read(&path) else {
            if r.url.in_system_image() {
                println!("{}: the system image has no index at {path}", r.name);
            }
            return;
        };
        // Expiry is weighed as the index is used, not here: an index that
        // expired is still listed, and replaced by an update.
        let policy = index::Policy { keys: &r.keys, repository: &r.name, now: 0, must_expire: false, previous: None };
        match index::verify(&bytes, &policy) {
            Ok(verified) => {
                let at = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                let c = &verified.contents;
                println!(
                    "{}: {} (serial {})",
                    r.name,
                    packages::count(c.packages.len() as u64, "package", "packages"),
                    c.serial
                );
                self.indexes.insert(r.name.clone(), Arc::new(Taken { verified, at }));
            }
            Err(e) => println!("{}: the index kept is not taken: {e}", r.name),
        }
    }

    /// The repositories that can be used now, in order: enabled, with an
    /// index that has not expired.
    pub fn usable(&self, now: u64) -> Vec<(&Repository, &Arc<Taken>)> {
        self.repositories
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| self.indexes.get(&r.name).map(|t| (r, t)))
            .filter(|(_, t)| !t.expired(now))
            .collect()
    }

    /// Why `r` cannot be used now, if it cannot.
    fn unusable(&self, r: &Repository, now: u64) -> Option<String> {
        if !r.enabled {
            return Some(String::from("disabled"));
        }
        match self.indexes.get(&r.name) {
            None if r.url.in_system_image() => Some(String::from("its index in the system image is not valid")),
            None => Some(String::from("no index taken yet: update the repositories")),
            Some(t) if t.expired(now) => Some(format!(
                "its index expired on {}: update the repositories",
                date(t.verified.contents.expires.unwrap_or(0))
            )),
            Some(_) => None,
        }
    }

    pub fn status(&self, busy: bool) -> Status {
        Status {
            busy,
            installed: self.db.packages.len() as u32,
            repositories: self.repositories.len() as u32,
            problem: self.problem.clone().unwrap_or_default(),
        }
    }

    pub fn repository_infos(&self) -> Vec<RepositoryInfo> {
        let now = now();
        self.repositories
            .iter()
            .map(|r| {
                let taken = self.indexes.get(&r.name);
                let contents = taken.map(|t| &t.verified.contents);
                RepositoryInfo {
                    name: r.name.clone(),
                    title: r.title.clone(),
                    url: r.url.to_string(),
                    keys: r.keys.iter().map(|k| k.fingerprint()).collect(),
                    enabled: r.enabled,
                    system: r.origin == Origin::System,
                    packages: contents.map_or(0, |c| c.packages.len() as u32),
                    serial: contents.map_or(0, |c| c.serial),
                    updated: taken.map_or(0, |t| t.at),
                    expires: contents.and_then(|c| c.expires).unwrap_or(0),
                    problem: self.unusable(r, now).unwrap_or_default(),
                }
            })
            .collect()
    }

    /// Adds, removes, enables or disables a repository, keeping the user's
    /// configuration.
    pub fn configure(&mut self, change: RepositoryChange) -> Result<(), Failure> {
        if self.user_damaged {
            return Err(failure(
                PackageError::Unavailable,
                format!("the repositories' configuration ({USER_CONFIG}) is damaged, and is left as it is"),
            ));
        }
        let mut user = self.user.clone();
        let invalid = |e: config::BadConfig| failure(PackageError::Invalid, e.to_string());
        let refused = |e: config::ConfigError| match e {
            config::ConfigError::NotFound(_) => failure(PackageError::NotFound, e.to_string()),
            config::ConfigError::Bad(_) => failure(PackageError::Invalid, e.to_string()),
            _ => failure(PackageError::Invalid, e.to_string()),
        };
        let removed = match change {
            RepositoryChange::Add { name, title, url, keys } => {
                let url = config::Url::parse(&url).map_err(invalid)?;
                let keys = keys
                    .iter()
                    .map(|k| {
                        packages::keys::PublicKey::parse(k).map_err(|e| failure(PackageError::Invalid, e.to_string()))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let title = if title.is_empty() { name.clone() } else { title };
                let repository = config::UserRepository { name, title, url, keys, enabled: true };
                user.add(repository, &self.system).map_err(refused)?;
                None
            }
            RepositoryChange::Remove { name } => {
                user.remove(&name, &self.system).map_err(refused)?;
                Some(name)
            }
            RepositoryChange::Enable { name, enabled } => {
                user.set_enabled(&name, enabled, &self.system).map_err(refused)?;
                None
            }
        };
        write_whole(USER_CONFIG, &user.to_bytes())
            .map_err(|e| failure(PackageError::Unavailable, format!("cannot keep the repositories: {e}")))?;
        self.user = user;
        if let Some(name) = removed {
            let _ = std::fs::remove_file(stored_index(&name));
        }
        self.merge();
        Ok(())
    }

    /// Takes the indexes an update fetched.
    pub fn take_updates(&mut self, taken: Vec<(String, Arc<Taken>)>) {
        for (name, t) in taken {
            if self.repositories.iter().any(|r| r.name == name) {
                self.indexes.insert(name, t);
            }
        }
    }

    /// The installed package `name`'s repository's place among those usable
    /// now, if it is usable.
    fn origin(usable: &[(&Repository, &Arc<Taken>)], repository: &str) -> Option<usize> {
        usable.iter().position(|(r, _)| r.name == repository)
    }

    /// A page of the packages in `scope` matching `query`.
    pub fn find(&self, query: &str, scope: Scope, after: &str, max: u32) -> Vec<PackageSummary> {
        let now = now();
        let usable = self.usable(now);
        let max = max.clamp(1, proto::packages::PAGE) as usize;
        // The newest version of each package offered (the first
        // repository's, of versions alike), of this architecture.
        let mut newest: BTreeMap<&str, (&Offer, &str)> = BTreeMap::new();
        for (r, t) in &usable {
            for o in t.offers() {
                if !o.manifest.architecture.installs_on(Architecture::NATIVE) {
                    continue;
                }
                let e = newest.entry(o.manifest.name.as_str()).or_insert((o, r.name.as_str()));
                if o.manifest.version > e.0.manifest.version {
                    *e = (o, r.name.as_str());
                }
            }
        }
        let installed = |name: &str| self.db.get(name);
        let summary = |m: &Manifest, repository: &str| PackageSummary {
            name: m.name.to_string(),
            version: m.version.to_string(),
            summary: m.summary.clone(),
            installed: installed(m.name.as_str()).map(|p| p.manifest.version.to_string()).unwrap_or_default(),
            repository: repository.to_string(),
            explicit: self.db.wanted(m.name.as_str()).is_some(),
        };
        let mut found = Vec::new();
        match scope {
            Scope::Available => {
                let later = newest.range::<str, _>((core::ops::Bound::Excluded(after), core::ops::Bound::Unbounded));
                for (o, r) in later.map(|(_, offer)| offer).filter(|(o, _)| matches(&o.manifest, query)).take(max) {
                    found.push(summary(&o.manifest, r));
                }
            }
            Scope::Installed | Scope::Upgradable => {
                for p in self.db.packages.iter().filter(|p| p.manifest.name.as_str() > after) {
                    if found.len() == max {
                        break;
                    }
                    if !matches(&p.manifest, query) {
                        continue;
                    }
                    if scope == Scope::Upgradable {
                        // Only its own repository moves it on.
                        let newer = Self::origin(&usable, &p.repository).and_then(|i| {
                            usable[i]
                                .1
                                .offers()
                                .iter()
                                .filter(|o| o.manifest.name == p.manifest.name)
                                .max_by(|a, b| a.manifest.version.cmp(&b.manifest.version))
                        });
                        match newer {
                            Some(o) if o.manifest.version > p.manifest.version => {
                                let mut s = summary(&o.manifest, &p.repository);
                                s.installed = p.manifest.version.to_string();
                                found.push(s);
                            }
                            _ => {}
                        }
                    } else {
                        let mut s = summary(&p.manifest, &p.repository);
                        s.installed = p.manifest.version.to_string();
                        found.push(s);
                    }
                }
            }
        }
        found
    }

    /// What is known of the package `name`.
    pub fn describe(&self, name: &str) -> Result<PackageInfo, Failure> {
        let usable = self.usable(now());
        let installed = self.db.get(name);
        let mut offered: Vec<(&Offer, &str, usize)> = Vec::new();
        for (i, (r, t)) in usable.iter().enumerate() {
            for o in t.offers().iter().filter(|o| o.manifest.name.as_str() == name) {
                if o.manifest.architecture.installs_on(Architecture::NATIVE) {
                    offered.push((o, r.name.as_str(), i));
                }
            }
        }
        if installed.is_none() && offered.is_empty() {
            return Err(failure(PackageError::NotFound, format!("no package called {name} is installed or offered")));
        }
        offered.sort_by(|a, b| b.0.manifest.version.cmp(&a.0.manifest.version).then(a.2.cmp(&b.2)));
        // What would be installed: the newest offered; for one installed,
        // the newest its own repository offers.
        let candidate = offered
            .iter()
            .find(|(_, r, _)| installed.is_none_or(|p| p.repository == *r))
            .map(|(o, r, _)| details(&o.manifest, r, o.size, o.unpacked));
        Ok(PackageInfo {
            name: name.to_string(),
            installed: installed.map(|p| details(&p.manifest, &p.repository, 0, p.size())),
            installed_at: installed.map_or(0, |p| p.installed),
            files: installed.map_or(0, |p| p.files.len() as u32),
            explicit: self.db.wanted(name).is_some(),
            candidate,
            versions: offered.iter().take(VERSIONS).map(|(o, r, _)| format!("{} ({r})", o.manifest.version)).collect(),
        })
    }

    /// A page of the installed package `name`'s files.
    pub fn files(&self, name: &str, after: &str, max: u32) -> Result<Vec<String>, Failure> {
        let p = self.db.get(name).ok_or_else(|| failure(PackageError::NotFound, format!("{name} is not installed")))?;
        let after = after.strip_prefix(LIVE).map_or(after, |a| a.trim_start_matches('/'));
        let max = max.clamp(1, proto::packages::PAGE) as usize;
        Ok(p.files.iter().filter(|f| f.path.as_str() > after).take(max).map(|f| format!("{LIVE}/{}", f.path)).collect())
    }

    /// Works out what `request` would do.
    pub fn resolve(&self, request: &Request) -> Result<Resolved, Failure> {
        if let Some(problem) = &self.problem {
            return Err(failure(PackageError::Unavailable, format!("{problem}: nothing can be installed or removed")));
        }
        let now = now();
        let usable = self.usable(now);
        let db = &*self.db;
        let mut notes = Vec::new();
        let installed_name = |n: &Name| {
            db.get(n.as_str())
                .map(|_| ())
                .ok_or_else(|| failure(PackageError::NotFound, format!("{n} is not installed")))
        };
        let (world, removing, upgrade_names, upgrade_all) = match request {
            Request::Install { packages } => {
                if packages.is_empty() {
                    return Err(failure(PackageError::Invalid, "name the packages to install"));
                }
                let asked = requirements(packages)?;
                let mut world = db.world.clone();
                for r in &asked {
                    world.retain(|w| w.name != r.name);
                    world.push(r.clone());
                    if let Some(p) = db.get(r.name.as_str())
                        && r.allows(&p.manifest.version)
                    {
                        let by_name =
                            if db.wanted(r.name.as_str()).is_none() { ", and is asked for by name now" } else { "" };
                        notes.push(format!("{} is installed already{by_name}", p.manifest.label()));
                    }
                }
                (world, Vec::new(), Vec::new(), false)
            }
            Request::Remove { packages } => {
                if packages.is_empty() {
                    return Err(failure(PackageError::Invalid, "name the packages to remove"));
                }
                let names = requirements(packages)?.into_iter().map(|r| r.name).collect::<Vec<_>>();
                for n in &names {
                    installed_name(n)?;
                }
                let world = db.world.iter().filter(|w| !names.contains(&w.name)).cloned().collect();
                (world, names, Vec::new(), false)
            }
            Request::Upgrade { packages } => {
                let names = requirements(packages)?.into_iter().map(|r| r.name).collect::<Vec<_>>();
                for n in &names {
                    installed_name(n)?;
                }
                let all = names.is_empty();
                (db.world.clone(), Vec::new(), names, all)
            }
        };
        let upgrade = if upgrade_all {
            Upgrade::Everything
        } else if upgrade_names.is_empty() {
            Upgrade::Nothing
        } else {
            Upgrade::Only(&upgrade_names)
        };
        for r in &self.repositories {
            if r.enabled
                && let Some(why) = self.unusable(r, now)
            {
                notes.push(format!("the repository {} is not used: {why}", r.name));
            }
        }
        let problem = Problem {
            system: &self.provides,
            installed: db
                .packages
                .iter()
                .map(|package| Installed { package, repository: Self::origin(&usable, &package.repository) })
                .collect(),
            available: usable
                .iter()
                .enumerate()
                .flat_map(|(repository, (_, t))| t.offers().iter().map(move |offer| Available { offer, repository }))
                .collect(),
            world: &world,
            removing: &removing,
            upgrade,
            architecture: Architecture::NATIVE,
        };
        let solution = resolve::resolve(&problem).map_err(|e| failure(PackageError::Unsatisfiable, e.to_string()))?;
        let worked = plan::plan(db, &problem, &solution, world.clone());
        if worked.steps.is_empty() && matches!(request, Request::Upgrade { .. }) {
            notes.push(String::from("everything asked for is up to date"));
        }
        let mut installs = Vec::new();
        let mut removes = Vec::new();
        let mut steps = Vec::new();
        let mut download = 0;
        for s in &worked.steps {
            let offer = s.offer.map(|j| problem.available[j]);
            let repository = offer.map(|a| usable[a.repository].0.name.clone()).unwrap_or_default();
            if s.action != PlanAction::Install {
                removes.push(s.name.clone());
            }
            if let Some(a) = offer {
                let r = usable[a.repository].0;
                if std::fs::metadata(cached(a.offer)).map_or(true, |m| m.len() != a.offer.size) {
                    download += a.offer.size;
                }
                installs.push(Install { offer: a.offer.clone(), repository: r.name.clone(), url: r.url.clone() });
            }
            steps.push(PlanStep {
                action: match s.action {
                    PlanAction::Install => Action::Install,
                    PlanAction::Upgrade => Action::Upgrade,
                    PlanAction::Downgrade => Action::Downgrade,
                    PlanAction::Remove => Action::Remove,
                },
                name: s.name.to_string(),
                from: s.from.as_ref().map(ToString::to_string).unwrap_or_default(),
                to: s.to.as_ref().map(ToString::to_string).unwrap_or_default(),
                repository,
                size: offer.map_or(0, |a| a.offer.size),
            });
        }
        let more = steps.len().saturating_sub(PLAN_STEPS) as u32;
        let plan = Plan {
            steps: steps.iter().take(PLAN_STEPS).cloned().collect(),
            more,
            download,
            growth: worked.growth,
            world: worked.world != db.world,
            digest: ipc::Bytes(worked.digest.0.to_vec()),
            notes,
        };
        Ok(Resolved { plan, installs, removes, world: worked.world, steps })
    }

    /// Whether `digest` is the plan's of `resolved`: what the user agreed
    /// to is what would be done.
    pub fn agreed(resolved: &Resolved, digest: &[u8]) -> bool {
        resolved.plan.digest.0 == digest
    }
}

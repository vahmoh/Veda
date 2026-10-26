// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Which versions of which packages satisfy the world.
//!
//! A [`Problem`] gives what is installed, what the repositories offer, the
//! world (what the user asked for), what is to be removed and which
//! packages may move to newer versions; [`resolve`] chooses at most one
//! version of each package so that every requirement of the world, and
//! every dependency of every package chosen, is met (by the system image,
//! by a package of its name, or by one that provides it), and no package
//! chosen conflicts with another. What is installed and not chosen is to be
//! removed: nothing that nothing needs stays.
//!
//! The search goes depth first through the requirements in the order they
//! arise, trying for each the candidates in order of preference, and goes
//! back to the last choice that has others left when one cannot be met.
//! The preference makes the first solution it finds the one wanted:
//!
//! * an installed package keeps its version unless it may be upgraded or
//!   its version no longer meets what is asked of it; else the newest
//!   version first;
//! * an installed package moves only to versions the repository it came
//!   from offers, so that a repository added later cannot take over a
//!   package of another by offering a higher version; one whose repository
//!   is gone keeps its version;
//! * between repositories offering one version, the first in the order the
//!   system uses them;
//! * a package of the name asked for before one that provides it; among
//!   providers, installed ones first, then by name.
//!
//! The result depends on nothing but the problem: the same problem resolves
//! the same way every time. Choosing among versions of interdependent
//! packages is NP-complete in general, so the search is bounded
//! ([`MAX_STEPS`]): a problem that needs more steps (no real repository's
//! does) fails rather than keeps the system busy. When no solution exists,
//! the error says why the most preferred attempt failed, in words: what
//! needed what, and what stood in its way.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use crate::database;
use crate::index::Offer;
use crate::manifest::{Architecture, Manifest};
use crate::name::Name;
use crate::requirement::{Provision, Requirement};

/// How many choices a resolution may make.
pub const MAX_STEPS: usize = 200_000;

/// An installed package, and the place among the problem's repositories of
/// the one it came from (none, if that repository is not in use now).
#[derive(Debug, Clone, Copy)]
pub struct Installed<'a> {
    pub package: &'a database::Installed,
    pub repository: Option<usize>,
}

/// A package a repository offers, and the repository's place.
#[derive(Debug, Clone, Copy)]
pub struct Available<'a> {
    pub offer: &'a Offer,
    pub repository: usize,
}

/// Which installed packages may move to newer versions.
#[derive(Debug, Clone, Copy)]
pub enum Upgrade<'a> {
    Nothing,
    Everything,
    Only(&'a [Name]),
}

/// What to resolve.
#[derive(Debug, Clone)]
pub struct Problem<'a> {
    /// What the system image provides (`veda (= 0.1.0)`), which is always
    /// there.
    pub system: &'a [Provision],
    pub installed: Vec<Installed<'a>>,
    /// In the order the repositories are preferred.
    pub available: Vec<Available<'a>>,
    pub world: &'a [Requirement],
    /// Packages to remove: none of them is chosen, whatever needs them.
    pub removing: &'a [Name],
    pub upgrade: Upgrade<'a>,
    /// The system's architecture.
    pub architecture: Architecture,
}

/// A version chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// The installed one ([`Problem::installed`]'s).
    Installed(usize),
    /// One a repository offers ([`Problem::available`]'s).
    Available(usize),
}

/// The packages chosen, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Solution {
    pub chosen: BTreeMap<Name, Choice>,
}

/// Why no packages were chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No set of packages satisfies the world (why).
    Unsatisfiable(String),
    /// The search went on longer than a resolution may.
    TooHard,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsatisfiable(why) => f.write_str(why),
            Error::TooHard => f.write_str(
                "the packages' dependencies are too intricate to work out: ask for fewer packages at once, or name \
                 the versions wanted",
            ),
        }
    }
}

/// A version the search may choose.
#[derive(Debug, Clone, Copy)]
struct Candidate<'a> {
    manifest: &'a Manifest,
    choice: Choice,
}

impl Candidate<'_> {
    fn installed(&self) -> bool {
        matches!(self.choice, Choice::Installed(_))
    }
}

/// A requirement to meet, and the package that has it (none: the world).
#[derive(Debug, Clone, Copy)]
struct Need<'a> {
    requirement: &'a Requirement,
    by: Option<&'a Manifest>,
}

/// A choice made: for which need, among which options, how many tried, and
/// how many needs there were before it.
#[derive(Debug)]
struct Frame<'a> {
    need: usize,
    options: Vec<(&'a Name, usize)>,
    tried: usize,
    needs: usize,
}

struct Search<'p, 'a> {
    problem: &'p Problem<'a>,
    /// Each package's candidates, best first.
    candidates: BTreeMap<&'a Name, Vec<Candidate<'a>>>,
    /// Which candidates provide each name.
    providers: BTreeMap<&'a Name, Vec<(&'a Name, usize)>>,
    /// Names offered only for other architectures.
    foreign: BTreeSet<&'a Name>,
    chosen: BTreeMap<&'a Name, usize>,
    /// The need each chosen package was chosen for.
    chosen_for: BTreeMap<&'a Name, usize>,
    needs: Vec<Need<'a>>,
    frames: Vec<Frame<'a>>,
    steps: usize,
    /// Why the first attempt that failed did.
    failure: Option<String>,
}

/// Chooses the packages that satisfy `problem`'s world, as the module's
/// documentation says.
pub fn resolve(problem: &Problem) -> Result<Solution, Error> {
    let mut s = Search::new(problem);
    s.needs.extend(problem.world.iter().map(|requirement| Need { requirement, by: None }));
    let mut next = 0;
    loop {
        while next < s.needs.len() && s.met(s.needs[next].requirement) {
            next += 1;
        }
        if next == s.needs.len() {
            return Ok(s.solution());
        }
        let options = s.options(&s.needs[next]);
        next = if options.is_empty() {
            if s.failure.is_none() {
                s.failure = Some(s.explain(next));
            }
            s.back()?
        } else {
            let needs = s.needs.len();
            s.frames.push(Frame { need: next, options, tried: 0, needs });
            s.forward()?
        };
    }
}

impl<'p, 'a> Search<'p, 'a> {
    fn new(problem: &'p Problem<'a>) -> Search<'p, 'a> {
        let mut candidates: BTreeMap<&'a Name, Vec<Candidate<'a>>> = BTreeMap::new();
        let mut installed: BTreeMap<&'a Name, (usize, Option<usize>)> = BTreeMap::new();
        for (i, p) in problem.installed.iter().enumerate() {
            let manifest = &p.package.manifest;
            installed.insert(&manifest.name, (i, p.repository));
            candidates.entry(&manifest.name).or_default().push(Candidate { manifest, choice: Choice::Installed(i) });
        }
        let mut foreign = BTreeSet::new();
        for (j, a) in problem.available.iter().enumerate() {
            let manifest = &a.offer.manifest;
            if !manifest.architecture.installs_on(problem.architecture) {
                foreign.insert(&manifest.name);
                continue;
            }
            if let Some(&(i, from)) = installed.get(&manifest.name) {
                // Only the repository it came from moves an installed
                // package on; the installed version stands for the same
                // version offered again.
                if from != Some(a.repository) || problem.installed[i].package.manifest.version == manifest.version {
                    continue;
                }
            }
            candidates.entry(&manifest.name).or_default().push(Candidate { manifest, choice: Choice::Available(j) });
        }
        for (name, list) in candidates.iter_mut() {
            let upgrading = match problem.upgrade {
                Upgrade::Nothing => false,
                Upgrade::Everything => true,
                Upgrade::Only(names) => names.contains(name),
            };
            // Of versions alike, the installed one, then the first
            // repository's.
            let rank = |c: &Candidate| match c.choice {
                Choice::Installed(_) => (0, 0, 0),
                Choice::Available(j) => (1, problem.available[j].repository, j),
            };
            list.sort_by(|a, b| {
                let kept = if upgrading { core::cmp::Ordering::Equal } else { b.installed().cmp(&a.installed()) };
                kept.then(b.manifest.version.cmp(&a.manifest.version)).then(rank(a).cmp(&rank(b)))
            });
        }
        let mut providers: BTreeMap<&'a Name, Vec<(&'a Name, usize)>> = BTreeMap::new();
        for (name, list) in &candidates {
            for (k, c) in list.iter().enumerate() {
                for p in &c.manifest.provides {
                    providers.entry(&p.name).or_default().push((*name, k));
                }
            }
        }
        Search {
            problem,
            candidates,
            providers,
            foreign,
            chosen: BTreeMap::new(),
            chosen_for: BTreeMap::new(),
            needs: Vec::new(),
            frames: Vec::new(),
            steps: 0,
            failure: None,
        }
    }

    fn candidate(&self, name: &Name, k: usize) -> &Candidate<'a> {
        &self.candidates[name][k]
    }

    /// Whether the system or a package chosen meets `requirement`.
    fn met(&self, requirement: &Requirement) -> bool {
        if self.problem.system.iter().any(|p| requirement.met_by_provision(p)) {
            return true;
        }
        if let Some(&k) = self.chosen.get(&requirement.name)
            && requirement.allows(&self.candidate(&requirement.name, k).manifest.version)
        {
            return true;
        }
        self.providers.get(&requirement.name).is_some_and(|list| {
            list.iter().any(|&(name, k)| self.chosen.get(name) == Some(&k) && self.provides(name, k, requirement))
        })
    }

    /// Whether candidate `k` of `name` provides what meets `requirement`.
    fn provides(&self, name: &Name, k: usize, requirement: &Requirement) -> bool {
        self.candidate(name, k).manifest.provides.iter().any(|p| requirement.met_by_provision(p))
    }

    fn removing(&self, name: &Name) -> bool {
        self.problem.removing.contains(name)
    }

    /// The package (chosen, or the system) that `manifest` would conflict
    /// with, or that would conflict with it.
    fn conflict(&self, manifest: &Manifest) -> Option<String> {
        for r in &manifest.conflicts {
            if let Some(p) = self.problem.system.iter().find(|p| r.met_by_provision(p)) {
                return Some(alloc::format!("{} conflicts with {p}, which the system has", manifest.label()));
            }
        }
        for (name, &k) in &self.chosen {
            if **name == manifest.name {
                continue;
            }
            let other = self.candidate(name, k).manifest;
            if manifest.conflicts.iter().any(|r| other.meets(r)) {
                return Some(alloc::format!("{} conflicts with {}", manifest.label(), other.label()));
            }
            if other.conflicts.iter().any(|r| manifest.meets(r)) {
                return Some(alloc::format!("{} conflicts with {}", other.label(), manifest.label()));
            }
        }
        None
    }

    /// The candidates that could meet `need` now, in order of preference.
    fn options(&self, need: &Need<'a>) -> Vec<(&'a Name, usize)> {
        let r = need.requirement;
        let mut options = Vec::new();
        if let Some((name, list)) = self.candidates.get_key_value(&r.name)
            && !self.chosen.contains_key(&r.name)
            && !self.removing(&r.name)
        {
            for (k, c) in list.iter().enumerate() {
                if r.allows(&c.manifest.version) && self.conflict(c.manifest).is_none() {
                    options.push((*name, k));
                }
            }
        }
        let mut providing: Vec<(&'a Name, usize)> = self
            .providers
            .get(&r.name)
            .map(|list| {
                list.iter()
                    .copied()
                    .filter(|&(name, k)| {
                        !self.chosen.contains_key(name)
                            && !self.removing(name)
                            && self.provides(name, k, r)
                            && self.conflict(self.candidate(name, k).manifest).is_none()
                    })
                    .collect()
            })
            .unwrap_or_default();
        providing.sort_by_key(|&(name, k)| (!self.candidate(name, k).installed(), name, k));
        options.extend(providing);
        options
    }

    /// Makes the next choice of the last frame.
    fn forward(&mut self) -> Result<usize, Error> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return Err(Error::TooHard);
        }
        let Some(frame) = self.frames.last_mut() else { return Err(Error::TooHard) };
        let (name, k) = frame.options[frame.tried];
        frame.tried += 1;
        let need = frame.need;
        self.chosen.insert(name, k);
        self.chosen_for.insert(name, need);
        let manifest = self.candidate(name, k).manifest;
        self.needs.extend(manifest.depends.iter().map(|requirement| Need { requirement, by: Some(manifest) }));
        Ok(need + 1)
    }

    /// Undoes choices back to the last that has others left, and makes the
    /// next; fails once none has.
    fn back(&mut self) -> Result<usize, Error> {
        loop {
            let Some(frame) = self.frames.last() else {
                return Err(Error::Unsatisfiable(self.failure.take().unwrap_or_default()));
            };
            let (name, _) = frame.options[frame.tried - 1];
            let (more, needs) = (frame.tried < frame.options.len(), frame.needs);
            self.chosen.remove(name);
            self.chosen_for.remove(name);
            self.needs.truncate(needs);
            if more {
                return self.forward();
            }
            self.frames.pop();
        }
    }

    fn solution(&self) -> Solution {
        let chosen = self.chosen.iter().map(|(name, &k)| ((*name).clone(), self.candidate(name, k).choice)).collect();
        Solution { chosen }
    }

    /// Who asked for need `i`: "hello 1.0 needs" or "you asked for".
    fn asker(&self, i: usize) -> String {
        match self.needs[i].by {
            Some(m) => alloc::format!("{} needs {}", m.label(), self.needs[i].requirement),
            None => alloc::format!("{} is asked for", self.needs[i].requirement),
        }
    }

    /// Why need `i` cannot be met now.
    fn explain(&self, i: usize) -> String {
        let r = self.needs[i].requirement;
        let asked = self.asker(i);
        let list: &[Candidate] = self.candidates.get(&r.name).map_or(&[], Vec::as_slice);
        let providers = self.providers.get(&r.name).map_or(0, Vec::len);
        if self.removing(&r.name) {
            return alloc::format!("{asked}, but {} is being removed", r.name);
        }
        if let Some(&k) = self.chosen.get(&r.name) {
            let because = match self.chosen_for.get(&r.name) {
                Some(&j) => alloc::format!(" (as {})", self.asker(j)),
                None => String::new(),
            };
            let version = &self.candidate(&r.name, k).manifest.version;
            return alloc::format!("{asked}, but {} {version} is to be installed{because}", r.name);
        }
        if list.is_empty() && providers == 0 {
            if let Some(p) = self.problem.system.iter().find(|p| p.name == r.name) {
                return alloc::format!("{asked}, but the system has {p}");
            }
            if self.foreign.contains(&r.name) {
                return alloc::format!("{asked}, but the repositories have it for other architectures alone");
            }
            return alloc::format!("{asked}, but no repository has it");
        }
        let allowed: Vec<&Candidate> = list.iter().filter(|c| r.allows(&c.manifest.version)).collect();
        if allowed.is_empty()
            && !self.providers.get(&r.name).is_some_and(|l| l.iter().any(|&(n, k)| self.provides(n, k, r)))
        {
            let mut versions: Vec<String> = list.iter().take(5).map(|c| c.manifest.version.to_string()).collect();
            if list.len() > 5 {
                versions.push(String::from("..."));
            }
            return match versions.len() {
                0 => alloc::format!("{asked}, but no version of it meets that"),
                _ => alloc::format!("{asked}, but there is {} {} alone", r.name, versions.join(", ")),
            };
        }
        for c in allowed {
            if let Some(why) = self.conflict(c.manifest) {
                return alloc::format!("{asked}, but {why}");
            }
        }
        alloc::format!("{asked}, but nothing that provides it can be installed beside the rest")
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::Digest;
    use crate::tests::{With, manifest};

    /// A repository's offers.
    fn offers(labels: &[Manifest]) -> Vec<Offer> {
        labels
            .iter()
            .map(|m| Offer {
                manifest: m.clone(),
                file: alloc::format!("{}-{}.vpkg", m.name, m.version),
                size: 10,
                sha256: Digest::of(m.label().as_bytes()),
                unpacked: 20,
            })
            .collect()
    }

    fn installed(m: Manifest, repository: &str) -> database::Installed {
        database::Installed {
            manifest: m,
            repository: repository.to_string(),
            sha256: Digest::default(),
            installed: 0,
            files: Vec::new(),
        }
    }

    fn reqs(texts: &[&str]) -> Vec<Requirement> {
        texts.iter().map(|t| Requirement::parse(t).unwrap()).collect()
    }

    fn names(texts: &[&str]) -> Vec<Name> {
        texts.iter().map(|t| Name::parse(t).unwrap()).collect()
    }

    /// A world to resolve against repositories (each a list of offers) and
    /// what is installed (each from a repository by its place, or none).
    struct Case {
        repositories: Vec<Vec<Offer>>,
        installed: Vec<(database::Installed, Option<usize>)>,
        system: Vec<Provision>,
    }

    impl Case {
        fn new(repositories: Vec<Vec<Offer>>) -> Case {
            Case { repositories, installed: Vec::new(), system: vec![Provision::parse("veda (= 0.1.0)").unwrap()] }
        }

        fn with_installed(mut self, m: Manifest, from: Option<usize>) -> Case {
            self.installed.push((installed(m, "main"), from));
            self
        }

        fn problem<'a>(&'a self, world: &'a [Requirement], removing: &'a [Name], upgrade: Upgrade<'a>) -> Problem<'a> {
            let available = self
                .repositories
                .iter()
                .enumerate()
                .flat_map(|(repository, list)| list.iter().map(move |offer| Available { offer, repository }))
                .collect();
            let installed = self
                .installed
                .iter()
                .map(|(package, repository)| Installed { package, repository: *repository })
                .collect();
            Problem {
                system: &self.system,
                installed,
                available,
                world,
                removing,
                upgrade,
                architecture: Architecture::Amd64,
            }
        }

        /// The solution, as labels with where each came from: `hello 1.0
        /// (installed)` or `hello 2.0 (1)`, the repository's place.
        fn solve(&self, world: &[&str], removing: &[&str], upgrade: Upgrade) -> Result<Vec<String>, Error> {
            let (world, removing) = (reqs(world), names(removing));
            let problem = self.problem(&world, &removing, upgrade);
            let solution = resolve(&problem)?;
            Ok(solution
                .chosen
                .values()
                .map(|c| match *c {
                    Choice::Installed(i) => {
                        alloc::format!("{} (installed)", problem.installed[i].package.manifest.label())
                    }
                    Choice::Available(j) => {
                        let a = problem.available[j];
                        alloc::format!("{} ({})", a.offer.manifest.label(), a.repository)
                    }
                })
                .collect())
        }
    }

    #[test]
    fn the_newest_versions_and_their_dependencies_are_chosen() {
        let case = Case::new(vec![offers(&[
            manifest("hello 1.0"),
            manifest("hello 2.0").depends("libgreet (>= 2)").depends("veda (>= 0.1)"),
            manifest("libgreet 1.5"),
            manifest("libgreet 2.1"),
            manifest("libgreet 3.0~rc1"),
            manifest("unrelated 1.0"),
        ])]);
        assert_eq!(case.solve(&["hello"], &[], Upgrade::Nothing).unwrap(), ["hello 2.0 (0)", "libgreet 3.0~rc1 (0)"]);
        assert_eq!(
            case.solve(&["hello", "libgreet (< 3~)"], &[], Upgrade::Nothing).unwrap(),
            ["hello 2.0 (0)", "libgreet 2.1 (0)"]
        );
        // Going back: the newest hello needs a libgreet that cannot be had,
        // so the one before is chosen.
        assert_eq!(
            case.solve(&["hello", "libgreet (< 2)"], &[], Upgrade::Nothing).unwrap(),
            ["hello 1.0 (0)", "libgreet 1.5 (0)"]
        );
        assert_eq!(case.solve(&[], &[], Upgrade::Nothing).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn installed_versions_stay_until_upgraded() {
        let case = Case::new(vec![offers(&[
            manifest("hello 1.0"),
            manifest("hello 2.0").depends("lib"),
            manifest("lib 2.0"),
        ])])
        .with_installed(manifest("hello 1.0").depends("lib"), Some(0))
        .with_installed(manifest("lib 1.0"), Some(0));
        assert_eq!(
            case.solve(&["hello"], &[], Upgrade::Nothing).unwrap(),
            ["hello 1.0 (installed)", "lib 1.0 (installed)"]
        );
        assert_eq!(case.solve(&["hello"], &[], Upgrade::Everything).unwrap(), ["hello 2.0 (0)", "lib 2.0 (0)"]);
        let hello = names(&["hello"]);
        assert_eq!(
            case.solve(&["hello"], &[], Upgrade::Only(&hello)).unwrap(),
            ["hello 2.0 (0)", "lib 1.0 (installed)"]
        );
        // Asking for a version the installed one does not meet moves it,
        // down as readily as up.
        assert_eq!(
            case.solve(&["hello", "lib (>= 2)"], &[], Upgrade::Nothing).unwrap(),
            ["hello 1.0 (installed)", "lib 2.0 (0)"]
        );
        // What nothing needs goes.
        assert_eq!(case.solve(&["lib"], &[], Upgrade::Nothing).unwrap(), ["lib 1.0 (installed)"]);
    }

    #[test]
    fn packages_move_only_within_their_repository() {
        // "other" offers a higher hello than main, which hello came from:
        // it is not taken over, unless it is installed anew.
        let case = Case::new(vec![offers(&[manifest("hello 2.0")]), offers(&[manifest("hello 9.0")])])
            .with_installed(manifest("hello 1.0"), Some(0));
        assert_eq!(case.solve(&["hello"], &[], Upgrade::Everything).unwrap(), ["hello 2.0 (0)"]);
        let fresh = Case::new(vec![offers(&[manifest("hello 2.0")]), offers(&[manifest("hello 9.0")])]);
        assert_eq!(fresh.solve(&["hello"], &[], Upgrade::Nothing).unwrap(), ["hello 9.0 (1)"]);
        // The same version in two repositories: the first one's.
        let same = Case::new(vec![offers(&[manifest("hello 2.0")]), offers(&[manifest("hello 2.0")])]);
        assert_eq!(same.solve(&["hello"], &[], Upgrade::Nothing).unwrap(), ["hello 2.0 (0)"]);
        // One whose repository is gone keeps its version.
        let held = Case::new(vec![offers(&[manifest("hello 2.0")])]).with_installed(manifest("hello 1.0"), None);
        assert_eq!(held.solve(&["hello"], &[], Upgrade::Everything).unwrap(), ["hello 1.0 (installed)"]);
    }

    #[test]
    fn providers_and_conflicts_are_weighed() {
        let case = Case::new(vec![offers(&[
            manifest("app 1.0").depends("editor (>= 1)"),
            manifest("vim 9.0").provides("editor (= 1.0)").conflicts("nano"),
            manifest("nano 7.0").provides("editor (= 1.0)"),
            manifest("bare 1.0").provides("editor"),
        ])]);
        // Providers by name, nano before vim; "bare" provides no version.
        assert_eq!(case.solve(&["app"], &[], Upgrade::Nothing).unwrap(), ["app 1.0 (0)", "nano 7.0 (0)"]);
        assert_eq!(case.solve(&["app", "vim"], &[], Upgrade::Nothing).unwrap(), ["app 1.0 (0)", "vim 9.0 (0)"]);
        let e = case.solve(&["vim", "nano"], &[], Upgrade::Nothing).unwrap_err();
        assert_eq!(e.to_string(), "nano is asked for, but vim 9.0 conflicts with nano 7.0");
        // An installed provider is preferred.
        let installed =
            Case::new(vec![offers(&[manifest("app 1.0").depends("editor"), manifest("nano 7.0").provides("editor")])])
                .with_installed(manifest("vim 9.0").provides("editor"), None);
        assert_eq!(installed.solve(&["app"], &[], Upgrade::Nothing).unwrap(), ["app 1.0 (0)", "vim 9.0 (installed)"]);
    }

    #[test]
    fn failures_say_why() {
        let case = Case::new(vec![offers(&[
            manifest("hello 1.0").depends("libgreet (>= 2)"),
            manifest("libgreet 1.4"),
            manifest("clash 1.0").conflicts("veda (< 1)"),
            manifest("app 1.0").depends("hello"),
            manifest("big 1.0").any().provides("data"),
            manifest("a 1.0"),
            manifest("b 1.0").depends("a (>= 2)"),
            manifest("newer 1.0").depends("veda (>= 2)"),
        ])])
        .with_installed(manifest("old 1.0"), None);
        for (world, removing, why) in [
            (&["nothing"][..], &[][..], "nothing is asked for, but no repository has it"),
            (&["hello"], &[], "hello 1.0 needs libgreet (>= 2), but there is libgreet 1.4 alone"),
            (&["libgreet (>= 3)"], &[], "libgreet (>= 3) is asked for, but there is libgreet 1.4 alone"),
            (&["clash"], &[], "clash is asked for, but clash 1.0 conflicts with veda (= 0.1.0), which the system has"),
            (&["app"], &["libgreet"], "hello 1.0 needs libgreet (>= 2), but libgreet is being removed"),
            (&["a", "b"], &[], "b 1.0 needs a (>= 2), but a 1.0 is to be installed (as a is asked for)"),
            (&["newer"], &[], "newer 1.0 needs veda (>= 2), but the system has veda (= 0.1.0)"),
        ] {
            let e = case.solve(world, removing, Upgrade::Nothing).unwrap_err();
            assert_eq!(e.to_string(), why, "{world:?}");
        }
    }

    #[test]
    fn other_architectures_are_not_installed() {
        let mut arm = manifest("tool 1.0");
        arm.architecture = Architecture::Any;
        let case = Case::new(vec![offers(&[arm])]);
        assert_eq!(case.solve(&["tool"], &[], Upgrade::Nothing).unwrap(), ["tool 1.0 (0)"]);
        let problem_offers = offers(&[manifest("tool 1.0")]);
        let world = reqs(&["tool"]);
        let problem = Problem {
            system: &[],
            installed: Vec::new(),
            available: problem_offers.iter().map(|offer| Available { offer, repository: 0 }).collect(),
            world: &world,
            removing: &[],
            upgrade: Upgrade::Nothing,
            architecture: Architecture::Any,
        };
        let e = resolve(&problem).unwrap_err();
        assert_eq!(e.to_string(), "tool is asked for, but the repositories have it for other architectures alone");
    }

    #[test]
    fn hard_problems_end() {
        // A chain of choices that only the last can settle: each package
        // comes in two versions, the newer of which needs the newer next
        // one; the last's newer version cannot be had. Backtracking is
        // exponential here, and bounded.
        let mut list = Vec::new();
        const N: usize = 40;
        for i in 0..N {
            let next = alloc::format!("p{} (>= 2)", i + 1);
            let mut newer = manifest(&alloc::format!("p{i} 2.0"));
            let older = manifest(&alloc::format!("p{i} 1.0")).depends(&alloc::format!("p{}", i + 1));
            if i + 1 < N {
                newer = newer.depends(&next);
            } else {
                newer = newer.depends("missing");
            }
            list.push(newer);
            list.push(older);
        }
        list.push(manifest(&alloc::format!("p{N} 1.0")).depends("missing"));
        let world: Vec<String> = (0..N).map(|i| alloc::format!("p{i}")).collect();
        let world: Vec<&str> = world.iter().map(String::as_str).collect();
        let case = Case::new(vec![offers(&list)]);
        let e = case.solve(&world, &[], Upgrade::Nothing).unwrap_err();
        assert!(matches!(e, Error::TooHard | Error::Unsatisfiable(_)), "{e}");
    }

    #[test]
    fn ties_go_to_the_first_repository_whatever_the_order_given() {
        let (later, first) = (offers(&[manifest("hello 2.0")]), offers(&[manifest("hello 2.0")]));
        let world = reqs(&["hello"]);
        let problem = Problem {
            system: &[],
            installed: Vec::new(),
            available: vec![
                Available { offer: &later[0], repository: 1 },
                Available { offer: &first[0], repository: 0 },
            ],
            world: &world,
            removing: &[],
            upgrade: Upgrade::Nothing,
            architecture: Architecture::Amd64,
        };
        assert_eq!(resolve(&problem).unwrap().chosen.values().collect::<Vec<_>>(), [&Choice::Available(1)]);
    }

    #[test]
    fn resolving_is_deterministic() {
        let case = Case::new(vec![offers(&[
            manifest("a 1.0").depends("x"),
            manifest("b 1.0").depends("x"),
            manifest("x1 1.0").provides("x"),
            manifest("x2 1.0").provides("x"),
        ])]);
        let first = case.solve(&["b", "a"], &[], Upgrade::Nothing).unwrap();
        for _ in 0..5 {
            assert_eq!(case.solve(&["b", "a"], &[], Upgrade::Nothing).unwrap(), first);
        }
        assert_eq!(first, ["a 1.0 (0)", "b 1.0 (0)", "x1 1.0 (0)"]);
    }
}

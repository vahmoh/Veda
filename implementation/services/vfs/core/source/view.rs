// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What a confined connection reaches (`vfs::confine`): the places of the
//! namespace it was confined to, each to read or to change too, and the
//! directories on the way to them, which it can look at and list but which
//! show it only what leads on to a place. Nothing else is there for it. As
//! OpenBSD's `unveil` does for a process, a program gives up the rest of
//! the file system for good before it handles what it cannot trust (the
//! browser's page processes). A loan another connection made it
//! (`vfs::borrow`, [`crate::loans`]) adds a place to read, and nothing more,
//! until its lender takes it back (`vfs::take_back`): the place leaves the
//! view, and nothing else does. The view's own places are the connection's
//! for good; a borrowed one stays the lender's, which confining again
//! neither keeps nor drops.
//!
//! Paths are their components, as `tree::components` makes them: absolute
//! and without `.` or `..`, so that no path names a place it is not in.
//!
//! `/dev/shm`, where programs keep the memory they share
//! ([`crate::shm`]), is reached by places at or inside it alone: one that
//! holds it (`/dev`, as a program is given the devices to read, or `/`)
//! reaches the rest, so that no confined program reaches the memory of
//! others by being given what holds it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// How a view reaches a path, from not at all to wholly: each reaches what
/// the ones before it do, and more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reach {
    /// Not at all: there is nothing there for the connection.
    None,
    /// A directory on the way to places of the view: it can be looked at
    /// (`stat`) and listed, which shows only the entries that lead on to a
    /// place, and nothing in it can be read or changed.
    Through,
    /// Inside a place to read.
    Read,
    /// Inside a place to change too.
    Write,
}

/// A place of a view: its path's components, whether it may be changed,
/// and the loan that added it, if it was borrowed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    comps: Vec<String>,
    write: bool,
    /// The number of the loan that added it ([`View::borrow`]), which takes
    /// it back; none for the view's own places.
    loan: Option<u64>,
}

impl Place {
    /// Whether the place holds `comps`: it is the place, or inside it.
    fn holds(&self, comps: &[&str]) -> bool {
        comps.len() >= self.comps.len() && self.comps.iter().zip(comps).all(|(a, b)| a == b)
    }

    /// Whether `comps` is on the way to the place: a directory it is in.
    fn beyond(&self, comps: &[&str]) -> bool {
        comps.len() < self.comps.len() && self.comps.iter().zip(comps).all(|(a, b)| a == b)
    }
}

/// The places a confined connection reaches.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct View {
    places: Vec<Place>,
}

impl View {
    /// A view of `places`: the components of each one's path, and whether
    /// it may be changed. Where one place holds another, the inner one says
    /// how what is inside it is reached (`/dev` to read, `/dev/null` to
    /// change).
    pub fn new<'a>(places: impl IntoIterator<Item = (Vec<&'a str>, bool)>) -> View {
        let places = places.into_iter().map(|(comps, write)| Place {
            comps: comps.iter().map(|c| c.to_string()).collect(),
            write,
            loan: None,
        });
        View { places: places.collect() }
    }

    /// How the view reaches the path of `comps`.
    pub fn reach(&self, comps: &[&str]) -> Reach {
        reach(self.places.iter(), comps)
    }

    /// How many places the view has, borrowed or its own.
    pub fn len(&self) -> usize {
        self.places.len()
    }

    /// Whether the view has no places, and so reaches nothing.
    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
    }

    /// How many of its places the view borrowed.
    pub fn borrowed(&self) -> usize {
        self.places.iter().filter(|p| p.loan.is_some()).count()
    }

    /// The view's own places, without what it borrowed.
    fn own(&self) -> impl Iterator<Item = &Place> + Clone {
        self.places.iter().filter(|p| p.loan.is_none())
    }

    /// Whether a loan of `comps` would add it to the view ([`View::borrow`]):
    /// none of the view's own places holds it.
    pub fn would_borrow(&self, comps: &[&str]) -> bool {
        reach(self.own(), comps) < Reach::Read
    }

    /// Adds `comps` as a place to read, as taking loan number `loan` does
    /// (`vfs::borrow`), unless one of the view's own places holds it already
    /// (it reaches it for good): whether it was added. A place borrowed
    /// before that holds it does not keep it out: each loan adds a place of
    /// its own, which it alone takes back ([`View::take_back`]). So no own
    /// place holds a borrowed one, and the view reaches nothing less after:
    /// what is inside the new place is reached as the innermost place there
    /// says (one of the view's inside it, or the new one, read as a
    /// borrowed place around it was), and what is not, as before.
    pub fn borrow(&mut self, comps: &[&str], loan: u64) -> bool {
        if !self.would_borrow(comps) {
            return false;
        }
        let comps = comps.iter().map(|c| c.to_string()).collect();
        self.places.push(Place { comps, write: false, loan: Some(loan) });
        true
    }

    /// Takes back the place that loan number `loan` added, as its lender
    /// does (`vfs::take_back`): whether there was one. Nothing else leaves
    /// the view, and it reaches nothing more after: what was inside the
    /// place is reached as the innermost place left there says (one inside
    /// it, as before, or a borrowed one around it, read as it was: no own
    /// place holds a borrowed one), or not at all.
    pub fn take_back(&mut self, loan: u64) -> bool {
        let found = self.places.iter().position(|p| p.loan == Some(loan));
        found.map(|i| self.places.remove(i)).is_some()
    }

    /// Confines the view again, to `narrower`'s places, as confining a
    /// connection again does (`vfs::confine`): whether it may, as its own
    /// places admit `narrower` ([`View::admits`]). The places it borrowed
    /// stay, borrowed: they are their lenders' to take back, and a view
    /// makes no borrowed place its own by naming it. (Nor do the new own
    /// places hold a borrowed one, which none of the old ones did.)
    pub fn confine(&mut self, narrower: View) -> bool {
        let own = View { places: self.own().cloned().collect() };
        if !own.admits(&narrower) {
            return false;
        }
        let borrowed = self.places.drain(..).filter(|p| p.loan.is_some());
        let places = narrower.places.into_iter().chain(borrowed).collect();
        self.places = places;
        true
    }

    /// Whether a listing of directory `dir` shows the view its entry
    /// `name`: anything inside a place, and on the way to places, what
    /// leads on to them.
    pub fn shows(&self, dir: &[&str], name: &str) -> bool {
        let mut entry = dir.to_vec();
        entry.push(name);
        self.reach(&entry) != Reach::None
    }

    /// Whether `narrower` reaches nothing this view does not, and nothing
    /// more wholly: what confining a connection again may make of its view.
    pub fn admits(&self, narrower: &View) -> bool {
        // How a view reaches a path changes only at its places: comparing
        // the two at each of `narrower`'s, and at each of this view's that
        // one of `narrower`'s holds, compares them everywhere.
        let mine = self.places.iter().filter(|q| narrower.places.iter().any(|p| p.holds(&strs(&q.comps))));
        narrower.places.iter().chain(mine).all(|p| {
            let comps = strs(&p.comps);
            narrower.reach(&comps) <= self.reach(&comps)
        })
    }
}

/// The components of `/dev/shm`, which only places at or inside it reach.
const SHM: [&str; 2] = ["dev", "shm"];

/// Whether `comps` is `/dev/shm` or inside it.
fn in_shm<S: AsRef<str>>(comps: &[S]) -> bool {
    comps.len() >= SHM.len() && comps.iter().zip(SHM).all(|(c, s)| c.as_ref() == s)
}

/// How `places` reach the path of `comps`.
fn reach<'a>(places: impl Iterator<Item = &'a Place> + Clone, comps: &[&str]) -> Reach {
    // Inside `/dev/shm`, the places there alone count.
    let shm = in_shm(comps);
    let places = places.filter(move |p| !shm || in_shm(&p.comps));
    // The innermost place that holds it says how.
    let holder = places.clone().filter(|p| p.holds(comps)).max_by_key(|p| p.comps.len());
    match holder {
        Some(p) if p.write => Reach::Write,
        Some(_) => Reach::Read,
        None if places.into_iter().any(|p| p.beyond(comps)) => Reach::Through,
        None => Reach::None,
    }
}

/// Components as the `&str`s paths are looked up by.
fn strs(comps: &[String]) -> Vec<&str> {
    comps.iter().map(String::as_str).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree;

    /// A view of `places` written as paths, `+` before the ones that may
    /// be changed.
    fn view(places: &[&str]) -> View {
        View::new(places.iter().map(|p| {
            let (path, write) = match p.strip_prefix('+') {
                Some(path) => (path, true),
                None => (*p, false),
            };
            (tree::components(path).unwrap(), write)
        }))
    }

    fn reach(v: &View, path: &str) -> Reach {
        v.reach(&tree::components(path).unwrap())
    }

    /// What a page's process reaches (the browser's web processes).
    fn page() -> View {
        view(&["/system", "/tmp/fontconfig", "/dev", "+/dev/null"])
    }

    #[test]
    fn shared_memory_is_reached_by_its_own_places() {
        // The devices to read, or everything, do not reach the memory of
        // others.
        let v = page();
        assert_eq!(reach(&v, "/dev/shm"), Reach::None);
        assert_eq!(reach(&v, "/dev/shm/psm_1234"), Reach::None);
        assert!(!v.shows(&["dev"], "shm") && v.shows(&["dev"], "zero"));
        assert_eq!(reach(&view(&["+/"]), "/dev/shm/psm_1234"), Reach::None);
        assert_eq!(reach(&view(&["+/"]), "/dev/null"), Reach::Write);
        // A place inside it does.
        let v = view(&["/dev", "+/dev/shm/mine"]);
        assert_eq!(reach(&v, "/dev/shm/mine"), Reach::Write);
        assert_eq!(reach(&v, "/dev/shm"), Reach::Through);
        assert_eq!(reach(&v, "/dev/shm/other"), Reach::None);
        assert_eq!(reach(&v, "/dev/zero"), Reach::Read);
        assert_eq!(reach(&view(&["/dev/shm"]), "/dev/shm/other"), Reach::Read);
        // `/dev/shmem` is no part of it.
        assert_eq!(reach(&page(), "/dev/shmem"), Reach::Read);
    }

    #[test]
    fn places_and_what_is_inside_them() {
        let v = page();
        assert_eq!(reach(&v, "/system"), Reach::Read);
        assert_eq!(reach(&v, "/system/fonts/Inter.ttf"), Reach::Read);
        assert_eq!(reach(&v, "/tmp/fontconfig/cache-1"), Reach::Read);
        // The innermost place says how.
        assert_eq!(reach(&v, "/dev/null"), Reach::Write);
        assert_eq!(reach(&v, "/dev/urandom"), Reach::Read);
        assert_eq!(reach(&v, "/dev"), Reach::Read);
    }

    #[test]
    fn the_way_to_places_and_nothing_else() {
        let v = page();
        assert_eq!(reach(&v, "/"), Reach::Through);
        assert_eq!(reach(&v, "/tmp"), Reach::Through);
        for outside in ["/home", "/home/user", "/home/user/Documents/notes.txt", "/tmp/other", "/tmp/fontconfigs", "/x"]
        {
            assert_eq!(reach(&v, outside), Reach::None, "{outside}");
        }
        // Listings show what leads on to places, and inside them all.
        assert!(v.shows(&[], "system") && v.shows(&[], "tmp") && v.shows(&[], "dev"));
        assert!(!v.shows(&[], "home") && !v.shows(&["tmp"], "other"));
        assert!(v.shows(&["tmp"], "fontconfig") && v.shows(&["system", "fonts"], "Inter.ttf"));
        // A path that climbs out of a place is where it ends up.
        assert_eq!(reach(&v, "/system/../home/user"), Reach::None);
        assert_eq!(reach(&v, "/tmp/fontconfig/../../system/x"), Reach::Read);
    }

    #[test]
    fn a_view_of_nothing_and_of_everything() {
        let nothing = view(&[]);
        assert_eq!(reach(&nothing, "/"), Reach::None);
        assert_eq!(reach(&nothing, "/system"), Reach::None);
        let everything = view(&["+/"]);
        assert_eq!(reach(&everything, "/"), Reach::Write);
        assert_eq!(reach(&everything, "/home/user/x"), Reach::Write);
        let read = view(&["/"]);
        assert_eq!(reach(&read, "/home/user/x"), Reach::Read);
    }

    #[test]
    fn a_read_only_place_inside_a_writable_one() {
        let v = view(&["+/tmp", "/tmp/fontconfig"]);
        assert_eq!(reach(&v, "/tmp/x"), Reach::Write);
        assert_eq!(reach(&v, "/tmp/fontconfig/cache"), Reach::Read);
        assert_eq!(reach(&v, "/"), Reach::Through);
    }

    #[test]
    fn confining_again_only_narrows() {
        let v = page();
        // Less of it, or the same, or less wholly.
        for narrower in [
            &[][..],
            &["/system"],
            &["/system/fonts", "/dev/null"],
            &["/dev"],
            &["/system", "/tmp/fontconfig", "/dev", "+/dev/null"],
        ] {
            assert!(v.admits(&view(narrower)), "{narrower:?}");
        }
        // More, or more wholly.
        for wider in [&["/home"][..], &["+/system"], &["+/dev"], &["/"], &["/tmp"], &["/system", "/home/user"]] {
            assert!(!v.admits(&view(wider)), "{wider:?}");
        }
        // A writable place that would take back what an inner place of the
        // view only reads.
        let tmp = view(&["+/tmp", "/tmp/fontconfig"]);
        assert!(!tmp.admits(&view(&["+/tmp"])));
        assert!(tmp.admits(&view(&["+/tmp/a", "/tmp/fontconfig/x"])));
        assert!(!tmp.admits(&view(&["+/tmp/fontconfig/x"])));
        // Every view admits itself.
        assert!(tmp.admits(&tmp) && v.admits(&v));
    }

    /// Paths around the places of the views below.
    const AROUND: [&str; 14] = [
        "/",
        "/home",
        "/home/user",
        "/home/user/Documents",
        "/home/user/Documents/page.html",
        "/home/user/Documents/Pictures/a.png",
        "/home/user/Documents2",
        "/home/user/Pictures/a.png",
        "/home/user/.cache",
        "/home/user/.cache/browser/x",
        "/home/user/.cache/other",
        "/tmp/browser/x",
        "/system/fonts",
        "/dev/null",
    ];

    /// The browser's network process, as it confines itself.
    fn network() -> View {
        view(&["/system", "/dev", "+/dev/null", "+/home/user/.cache/browser", "+/tmp/browser"])
    }

    #[test]
    fn a_borrowed_place_is_read() {
        let mut v = network();
        let places = v.len();
        assert!(v.borrow(&tree::components("/home/user/Documents").unwrap(), 1));
        assert_eq!((v.len(), v.borrowed()), (places + 1, 1));
        assert_eq!(reach(&v, "/home/user/Documents"), Reach::Read);
        assert_eq!(reach(&v, "/home/user/Documents/Pictures/a.png"), Reach::Read);
        // Nothing beside it.
        assert_eq!(reach(&v, "/home/user/Documents2"), Reach::None);
        assert_eq!(reach(&v, "/home/user/Pictures/a.png"), Reach::None);
        assert_eq!(reach(&v, "/home/user"), Reach::Through);
        assert!(v.shows(&["home", "user"], "Documents") && !v.shows(&["home", "user"], "Pictures"));
        // A file alone.
        assert!(v.borrow(&tree::components("/home/user/page.html").unwrap(), 2));
        assert_eq!(reach(&v, "/home/user/page.html"), Reach::Read);
        assert_eq!(reach(&v, "/home/user/page.htm"), Reach::None);
    }

    #[test]
    fn what_the_views_own_places_reach_is_not_borrowed() {
        let mut v = network();
        for path in
            ["/system/share", "/home/user/.cache/browser", "/home/user/.cache/browser/x", "/dev/null", "/tmp/browser"]
        {
            let comps = tree::components(path).unwrap();
            assert!(!v.would_borrow(&comps) && !v.borrow(&comps, 1), "{path}");
        }
        assert_eq!(v, network());
        // Nor taken back: it is the view's for good.
        assert!(!v.take_back(1));
        assert_eq!(reach(&v, "/home/user/.cache/browser/x"), Reach::Write);
    }

    #[test]
    fn each_loan_adds_a_place_of_its_own() {
        // One inside a borrowed place, and the same place again, are
        // borrowed too, each loan's own.
        let mut v = network();
        let documents = tree::components("/home/user/Documents").unwrap();
        let pictures = tree::components("/home/user/Documents/Pictures").unwrap();
        assert!(v.borrow(&documents, 1) && v.would_borrow(&pictures) && v.borrow(&pictures, 2));
        assert!(v.borrow(&documents, 3));
        assert_eq!(v.borrowed(), 3);
        // Each takes back its own, and what another lent stays.
        assert!(v.take_back(1));
        assert_eq!(reach(&v, "/home/user/Documents/page.html"), Reach::Read);
        assert!(v.take_back(3));
        assert_eq!(reach(&v, "/home/user/Documents/page.html"), Reach::None);
        assert_eq!(reach(&v, "/home/user/Documents"), Reach::Through);
        assert_eq!(reach(&v, "/home/user/Documents/Pictures/a.png"), Reach::Read);
        assert!(v.take_back(2));
        assert_eq!(v, network());
    }

    #[test]
    fn a_borrowed_place_is_taken_back() {
        let mut v = network();
        assert!(v.borrow(&tree::components("/home/user/Documents").unwrap(), 7));
        assert!(v.borrow(&tree::components("/home/user/page.html").unwrap(), 8));
        // Another loan's number takes back nothing.
        assert!(!v.take_back(9));
        assert!(v.take_back(7));
        assert_eq!(reach(&v, "/home/user/Documents/Pictures/a.png"), Reach::None);
        assert!(!v.shows(&["home", "user"], "Documents"));
        // The other stays; and once taken back, a place is gone.
        assert_eq!(reach(&v, "/home/user/page.html"), Reach::Read);
        assert!(!v.take_back(7));
        assert!(v.take_back(8));
        assert_eq!(v, network());
        assert_eq!(reach(&v, "/home/user/page.html"), Reach::None);
    }

    #[test]
    fn borrowing_reaches_nothing_less_and_taking_back_nothing_more() {
        // Around each place the view has, borrowed places holding it,
        // inside it, or beside it: nothing is reached less wholly after
        // each is borrowed, nor more wholly after each is taken back, in
        // either order; at last the view is as it was.
        let places = ["/home/user/.cache", "/home/user/Documents", "/home", "/tmp", "/home/user/.cache/other"];
        for (i, first) in places.iter().enumerate() {
            for second in places {
                let mut v = network();
                let mut steps = Vec::from([v.clone()]);
                assert!(v.borrow(&tree::components(first).unwrap(), 1), "{first}");
                steps.push(v.clone());
                assert!(v.borrow(&tree::components(second).unwrap(), 2), "{second}");
                steps.push(v.clone());
                assert!(v.take_back(if i % 2 == 0 { 1 } else { 2 }));
                steps.push(v.clone());
                assert!(v.take_back(if i % 2 == 0 { 2 } else { 1 }));
                steps.push(v.clone());
                for path in AROUND {
                    let reaches: Vec<Reach> = steps.iter().map(|s| reach(s, path)).collect();
                    let grew = reaches[0] <= reaches[1] && reaches[1] <= reaches[2];
                    let shrank = reaches[2] >= reaches[3] && reaches[3] >= reaches[4];
                    assert!(grew && shrank, "{first}, {second}: {path}: {reaches:?}");
                }
                // What the view could change it still changes.
                for s in &steps {
                    assert_eq!(reach(s, "/home/user/.cache/browser/x"), Reach::Write, "{first}, {second}");
                    assert_eq!(reach(s, "/dev/null"), Reach::Write, "{first}, {second}");
                }
                assert_eq!(v, network(), "{first}, {second}");
            }
        }
    }

    #[test]
    fn confining_again_keeps_what_was_borrowed() {
        let mut v = network();
        assert!(v.borrow(&tree::components("/home/user/Documents").unwrap(), 1));
        // Narrower than its own places: what it borrowed stays, borrowed.
        assert!(v.confine(view(&["/system", "/dev", "+/home/user/.cache/browser"])));
        assert_eq!(reach(&v, "/home/user/Documents/page.html"), Reach::Read);
        assert_eq!(reach(&v, "/tmp/browser/x"), Reach::None);
        assert_eq!(reach(&v, "/dev/null"), Reach::Read);
        assert_eq!(v.borrowed(), 1);
        // A borrowed place named is not made its own: such a view is wider
        // than its own places, and refused, as is one that reaches into it.
        for wider in [&["/system", "/home/user/Documents"][..], &["/home/user/Documents/Pictures"], &["/home/user"]] {
            let before = v.clone();
            assert!(!v.confine(view(wider)), "{wider:?}");
            assert_eq!(v, before, "{wider:?}");
        }
        // And taken back, it is gone, the view as it was confined.
        assert!(v.take_back(1));
        assert_eq!(v, view(&["/system", "/dev", "+/home/user/.cache/browser"]));
    }
}

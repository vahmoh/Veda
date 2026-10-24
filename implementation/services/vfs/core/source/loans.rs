// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Loans: a place to read, which a connection that is not confined lends
//! and a confined one takes (`vfs::lend`, `vfs::borrow`), as the browser
//! lends its network process the folder of a page the user opens, until
//! the lender takes it back (`vfs::take_back`), as the browser does once no
//! page it has open needs the folder. As a sandbox extension of Apple's
//! systems does, it lets a sandboxed process reach one more place without
//! its sandbox being opened wider.
//!
//! A loan is a secret of [`LOAN_SIZE`] bytes from the kernel's random
//! generator, which only its lender has, and those it hands it to: whoever
//! has it may take the loan, once, naming the place it was made for. A
//! loan not taken lapses after [`LAPSE_NS`], or as its lender's connection
//! closes, and a lender holds at most [`MAX_UNTAKEN`] at once. Secrets are
//! compared in constant time: no answer tells how near a guess came.
//!
//! A loan taken whose place joined the taker's view is held
//! ([`Loans::hold`]) until its lender takes it back with its secret, which
//! no one else can ([`Loans::take_back`]), or the lender's connection
//! closes: either way the place leaves the view ([`TakenBack`]). It is
//! forgotten as the taker's connection closes.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use proto::fs::{FsError, LOAN_SIZE};

use crate::same;

/// How long a loan waits to be taken: in nanoseconds, a minute.
pub const LAPSE_NS: u64 = 60_000_000_000;

/// The untaken loans one connection holds at most.
pub const MAX_UNTAKEN: usize = 16;

/// A loan not taken yet.
struct Loan {
    secret: [u8; LOAN_SIZE],
    /// The place's path, as its components.
    comps: Vec<String>,
    /// The connection that lent it.
    lender: u64,
    /// When it lapses.
    until: u64,
}

/// A loan taken ([`Loans::take`]), which its lender may take back once it
/// is held ([`Loans::hold`]).
pub struct Taken {
    secret: [u8; LOAN_SIZE],
    comps: Vec<String>,
    lender: u64,
    /// Its number, which the place it adds to a view is known by.
    id: u64,
}

impl Taken {
    /// The loan's number, which no other loan has: the place it adds to
    /// the taker's view is known by it (`View::borrow`).
    pub fn id(&self) -> u64 {
        self.id
    }
}

/// A loan held: taken, and its place in the view of `borrower`.
struct Held {
    taken: Taken,
    borrower: u64,
}

/// A place taken back: the connection whose view it leaves, and the
/// number of the loan that added it there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakenBack {
    pub borrower: u64,
    pub id: u64,
    /// The place's path.
    pub path: String,
}

impl From<Held> for TakenBack {
    fn from(held: Held) -> TakenBack {
        let mut path = String::new();
        for c in &held.taken.comps {
            path.push('/');
            path.push_str(c);
        }
        if path.is_empty() {
            path.push('/');
        }
        TakenBack { borrower: held.borrower, id: held.taken.id, path }
    }
}

/// The loans the VFS has made: those not taken yet, and those held.
#[derive(Default)]
pub struct Loans {
    loans: Vec<Loan>,
    held: Vec<Held>,
    /// The number of the next loan taken.
    next: u64,
}

impl Loans {
    pub const fn new() -> Loans {
        Loans { loans: Vec::new(), held: Vec::new(), next: 0 }
    }

    /// Makes a loan of the place `comps` by connection `lender` at `now`,
    /// whose secret is `secret` (random bytes the caller drew): `TooMany`
    /// if the lender holds as many untaken loans as it may.
    pub fn lend(&mut self, lender: u64, comps: &[&str], secret: [u8; LOAN_SIZE], now: u64) -> Result<(), FsError> {
        self.loans.retain(|l| l.until > now);
        if self.loans.iter().filter(|l| l.lender == lender).count() >= MAX_UNTAKEN {
            return Err(FsError::TooMany);
        }
        let comps = comps.iter().map(|c| c.to_string()).collect();
        self.loans.push(Loan { secret, comps, lender, until: now.saturating_add(LAPSE_NS) });
        Ok(())
    }

    /// Takes the loan whose secret is `secret` at `now`, if it is a loan of
    /// the place `comps` that has not lapsed. Taken, it is gone, and its
    /// lender can take it back only once it is held.
    pub fn take(&mut self, secret: &[u8], comps: &[&str], now: u64) -> Option<Taken> {
        let found = self.loans.iter().position(|l| {
            let place = l.comps.len() == comps.len() && l.comps.iter().zip(comps).all(|(a, b)| a == b);
            same(&l.secret, secret) && l.until > now && place
        })?;
        let Loan { secret, comps, lender, .. } = self.loans.swap_remove(found);
        let id = self.next;
        self.next += 1;
        Some(Taken { secret, comps, lender, id })
    }

    /// Holds `taken`, whose place joined the view of connection `borrower`:
    /// its lender may take it back from now on.
    pub fn hold(&mut self, taken: Taken, borrower: u64) {
        self.held.push(Held { taken, borrower });
    }

    /// Connection `lender` takes back its loan whose secret is `secret`: one
    /// not taken yet can be taken no more, and one held is the place to
    /// take out of its borrower's view. Nothing, for another's loan, or one
    /// lapsed, or taken back already, or taken without being held.
    pub fn take_back(&mut self, lender: u64, secret: &[u8]) -> Option<TakenBack> {
        self.loans.retain(|l| !(l.lender == lender && same(&l.secret, secret)));
        let found = self.held.iter().position(|h| h.taken.lender == lender && same(&h.taken.secret, secret))?;
        Some(self.held.swap_remove(found).into())
    }

    /// Connection `conn` closed: the loans it made and that were not taken
    /// lapse, and the places of those held are taken back (returned), as
    /// no one can take them back after it; the loans it held are forgotten.
    pub fn forget(&mut self, conn: u64) -> Vec<TakenBack> {
        self.loans.retain(|l| l.lender != conn);
        self.held.retain(|h| h.borrower != conn);
        let (back, kept) = core::mem::take(&mut self.held).into_iter().partition(|h| h.taken.lender == conn);
        self.held = kept;
        back.into_iter().map(TakenBack::from).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree;

    fn comps(path: &str) -> Vec<&str> {
        tree::components(path).unwrap()
    }

    const SECRET: [u8; LOAN_SIZE] = [7; LOAN_SIZE];

    #[test]
    fn a_loan_is_taken_once_for_its_place() {
        let mut loans = Loans::new();
        loans.lend(1, &comps("/home/user/Documents"), SECRET, 0).unwrap();
        // Another place, or another secret: no loan, and it stays.
        assert!(loans.take(&SECRET, &comps("/home/user/Pictures"), 1).is_none());
        assert!(loans.take(&SECRET, &comps("/home/user"), 1).is_none());
        assert!(loans.take(&SECRET, &comps("/home/user/Documents/a"), 1).is_none());
        let mut other = SECRET;
        other[LOAN_SIZE - 1] ^= 1;
        assert!(loans.take(&other, &comps("/home/user/Documents"), 1).is_none());
        assert!(loans.take(&SECRET[..LOAN_SIZE - 1], &comps("/home/user/Documents"), 1).is_none());
        assert!(loans.take(&[], &comps("/home/user/Documents"), 1).is_none());
        // Its own: once.
        assert!(loans.take(&SECRET, &comps("/home/user/Documents"), 1).is_some());
        assert!(loans.take(&SECRET, &comps("/home/user/Documents"), 2).is_none());
    }

    #[test]
    fn a_loan_lapses() {
        let mut loans = Loans::new();
        loans.lend(1, &comps("/a"), SECRET, 100).unwrap();
        assert!(loans.take(&SECRET, &comps("/a"), 100 + LAPSE_NS - 1).is_some());
        loans.lend(1, &comps("/a"), [8; LOAN_SIZE], 100).unwrap();
        assert!(loans.take(&[8; LOAN_SIZE], &comps("/a"), 100 + LAPSE_NS).is_none());
        // And as its lender's connection closes.
        loans.lend(1, &comps("/a"), SECRET, 0).unwrap();
        loans.lend(2, &comps("/b"), [9; LOAN_SIZE], 0).unwrap();
        assert!(loans.forget(1).is_empty());
        assert!(loans.take(&SECRET, &comps("/a"), 1).is_none());
        assert!(loans.take(&[9; LOAN_SIZE], &comps("/b"), 1).is_some());
    }

    #[test]
    fn a_lender_holds_a_few_untaken_loans() {
        let mut loans = Loans::new();
        for i in 0..MAX_UNTAKEN {
            loans.lend(1, &comps("/a"), [i as u8; LOAN_SIZE], 0).unwrap();
        }
        assert_eq!(loans.lend(1, &comps("/a"), [99; LOAN_SIZE], 0), Err(FsError::TooMany));
        // Others lend as before; and one taken, lapsed or taken back makes
        // room.
        loans.lend(2, &comps("/a"), [99; LOAN_SIZE], 0).unwrap();
        assert!(loans.take(&[0; LOAN_SIZE], &comps("/a"), 1).is_some());
        loans.lend(1, &comps("/a"), [100; LOAN_SIZE], 1).unwrap();
        assert_eq!(loans.lend(1, &comps("/a"), [101; LOAN_SIZE], 1), Err(FsError::TooMany));
        assert_eq!(loans.take_back(1, &[1; LOAN_SIZE]), None);
        loans.lend(1, &comps("/a"), [101; LOAN_SIZE], 1).unwrap();
        assert_eq!(loans.lend(1, &comps("/a"), [102; LOAN_SIZE], 1), Err(FsError::TooMany));
        loans.lend(1, &comps("/a"), [102; LOAN_SIZE], LAPSE_NS).unwrap();
    }

    /// A loan of `path` from connection 1, taken and held by connection 2.
    fn held(loans: &mut Loans, path: &str, secret: [u8; LOAN_SIZE]) -> u64 {
        loans.lend(1, &comps(path), secret, 0).unwrap();
        let taken = loans.take(&secret, &comps(path), 1).unwrap();
        let id = taken.id();
        loans.hold(taken, 2);
        id
    }

    #[test]
    fn a_held_loan_is_taken_back_by_its_lender() {
        let mut loans = Loans::new();
        let documents = held(&mut loans, "/home/user/Documents", SECRET);
        let pictures = held(&mut loans, "/home/user/Pictures", [8; LOAN_SIZE]);
        assert_ne!(documents, pictures);
        // Another secret, or another connection with the secret, takes
        // back nothing.
        let mut other = SECRET;
        other[0] ^= 1;
        assert_eq!(loans.take_back(1, &other), None);
        assert_eq!(loans.take_back(1, &SECRET[1..]), None);
        assert_eq!(loans.take_back(2, &SECRET), None);
        assert_eq!(loans.take_back(3, &SECRET), None);
        // Its lender, with its secret: the place, out of the borrower's view.
        let back = TakenBack { borrower: 2, id: documents, path: "/home/user/Documents".into() };
        assert_eq!(loans.take_back(1, &SECRET), Some(back));
        // Once; the other stays.
        assert_eq!(loans.take_back(1, &SECRET), None);
        assert_eq!(loans.take_back(1, &[8; LOAN_SIZE]).map(|b| b.id), Some(pictures));
    }

    #[test]
    fn a_loan_not_taken_yet_is_taken_back() {
        let mut loans = Loans::new();
        loans.lend(1, &comps("/a"), SECRET, 0).unwrap();
        loans.lend(1, &comps("/b"), [8; LOAN_SIZE], 0).unwrap();
        // By another connection: it stays.
        assert_eq!(loans.take_back(2, &SECRET), None);
        assert!(loans.take(&[8; LOAN_SIZE], &comps("/b"), 1).is_some());
        // By its lender: there is no place to take back yet, and it can be
        // taken no more.
        assert_eq!(loans.take_back(1, &SECRET), None);
        assert!(loans.take(&SECRET, &comps("/a"), 1).is_none());
    }

    #[test]
    fn a_taken_loan_not_held_has_nothing_to_take_back() {
        // Its place did not join a view (the taker reached it already).
        let mut loans = Loans::new();
        loans.lend(1, &comps("/a"), SECRET, 0).unwrap();
        assert!(loans.take(&SECRET, &comps("/a"), 1).is_some());
        assert_eq!(loans.take_back(1, &SECRET), None);
        assert!(loans.forget(1).is_empty());
    }

    #[test]
    fn held_loans_end_with_their_connections() {
        let mut loans = Loans::new();
        let documents = held(&mut loans, "/home/user/Documents", SECRET);
        let page = held(&mut loans, "/home/user/page.html", [8; LOAN_SIZE]);
        // Another lender's, held by another borrower.
        loans.lend(3, &comps("/tmp/x"), [9; LOAN_SIZE], 0).unwrap();
        let other = loans.take(&[9; LOAN_SIZE], &comps("/tmp/x"), 1).unwrap();
        let x = other.id();
        loans.hold(other, 4);
        loans.lend(1, &comps("/tmp/untaken"), [10; LOAN_SIZE], 0).unwrap();
        // As the lender's connection closes, its places are taken back, and
        // what it did not lend stays.
        let mut back = loans.forget(1);
        back.sort_by_key(|b| b.id);
        let paths: Vec<(u64, u64, &str)> = back.iter().map(|b| (b.borrower, b.id, b.path.as_str())).collect();
        assert_eq!(paths, [(2, documents, "/home/user/Documents"), (2, page, "/home/user/page.html")]);
        assert!(loans.take(&[10; LOAN_SIZE], &comps("/tmp/untaken"), 1).is_none());
        // As a borrower's closes, what it held is forgotten: nothing to take
        // back, then or after.
        assert!(loans.forget(4).is_empty());
        assert_eq!(loans.take_back(3, &[9; LOAN_SIZE]), None);
        assert!(loans.forget(3).is_empty());
        assert_ne!(x, documents);
    }
}

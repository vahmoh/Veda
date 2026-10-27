// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The guest's waits that go on: those the bridge could not finish at once
//! (`WAIT`), which the monitor's waiter thread waits on, all of them at
//! once and its own event besides (the one that says they changed), in one
//! wait of the kernel's, round after round. What becomes of each at the
//! end of a round is [`Wait::outcome`]'s.
//!
//! The kernel waits on at most `abi::WAIT_MANY_MAX` items at once, so the
//! items of the waits that go on are at most [`MAX_ITEMS`]: a wait beyond
//! that is refused (`LimitReached`), as is one beyond what the notification
//! ring holds, counting the waits that have finished but are not in it yet
//! (a guest that takes nothing from the ring cannot make the monitor keep
//! more).

use alloc::vec::Vec;

use abi::{Error, WaitItem};
use hv::bridge::{self as b, Completion};

/// The most items of the waits that go on: the kernel's most, but the
/// waiter's own.
pub const MAX_ITEMS: usize = abi::WAIT_MANY_MAX - 1;

/// The most waits that go on or have finished and are not in the ring yet:
/// what the ring holds.
pub const MAX_WAITS: usize = b::ring::CAPACITY as usize;

/// Where item `index` of a wait whose items are at `items` (guest-physical)
/// has the signals it observed: `None` if that is past the address space.
pub fn observed_at(items: u64, index: usize) -> Option<u64> {
    let item = (index as u64).checked_mul(core::mem::size_of::<WaitItem>() as u64)?;
    items.checked_add(item)?.checked_add(core::mem::offset_of!(WaitItem, observed) as u64)
}

/// A wait that goes on: its items, with the handles as the guest names
/// them, and where they are in the guest.
#[derive(Debug, Clone)]
pub struct Wait {
    /// The monitor's name for it, its own ([`Pending::admit`] gives it).
    id: u64,
    /// The guest's name for it, which its completion carries.
    pub key: u64,
    pub items_gpa: u64,
    pub items: Vec<WaitItem>,
    pub deadline: u64,
}

/// How a round of the waiter's went for a wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Round {
    /// It came while the round was on: it was not waited on yet.
    Missed,
    /// Its items were waited on: the signals each had at the end of the
    /// round are its items' `observed`.
    Waited,
    /// It cannot be waited on: a handle it names is gone, or names what
    /// cannot be waited on.
    Failed(Error),
}

impl Wait {
    /// The wait `key` on `items`, which are at `items_gpa` in the guest,
    /// until `deadline`.
    pub fn new(key: u64, items_gpa: u64, items: Vec<WaitItem>, deadline: u64) -> Wait {
        Wait { id: 0, key, items_gpa, items, deadline }
    }

    /// The monitor's name for it, its own among the waits that go on.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// What becomes of it after a round that went as `round`, at `now`
    /// (with what its items observed then in `observed`, the same length
    /// as its items, for [`Round::Waited`]): `None` while it goes on; its
    /// completion once it is finished (the signals its items observed are
    /// then in its items).
    pub fn outcome(&mut self, round: Round, observed: &[WaitItem], now: u64) -> Option<Completion> {
        match round {
            Round::Missed => None,
            Round::Failed(e) => Some(Completion { key: self.key, status: e as u32, satisfied: 0 }),
            Round::Waited => {
                let seen = observed.iter().any(|o| o.observed & o.signals != 0);
                if !seen && now < self.deadline {
                    return None;
                }
                for (mine, o) in self.items.iter_mut().zip(observed) {
                    mine.observed = o.observed;
                }
                let satisfied = self.items.iter().filter(|i| i.observed & i.signals != 0).count() as u32;
                let status = if satisfied > 0 { b::OK } else { Error::TimedOut as u32 };
                Some(Completion { key: self.key, status, satisfied })
            }
        }
    }
}

/// The waits that go on.
#[derive(Debug, Default)]
pub struct Pending {
    waits: Vec<Wait>,
    /// Their items, together.
    items: usize,
    /// The next wait's id.
    next: u64,
}

impl Pending {
    /// Takes `wait` on, when `finished` waits are not in the ring yet
    /// (`LimitReached` if it is beyond the waits or the items that may go
    /// on).
    pub fn admit(&mut self, wait: Wait, finished: usize) -> Result<(), Error> {
        if self.waits.len() + finished >= MAX_WAITS || self.items + wait.items.len() > MAX_ITEMS {
            return Err(Error::LimitReached);
        }
        self.items += wait.items.len();
        self.next += 1;
        self.waits.push(Wait { id: self.next, ..wait });
        Ok(())
    }

    /// Gives up wait `key`: whether it went on.
    pub fn cancel(&mut self, key: u64) -> bool {
        let Some(i) = self.waits.iter().position(|w| w.key == key) else { return false };
        let w = self.waits.swap_remove(i);
        self.items -= w.items.len();
        true
    }

    /// Whether a wait that goes on names the guest's handle `handle`.
    pub fn names(&self, handle: u32) -> bool {
        self.names_any(&[handle])
    }

    /// Whether a wait that goes on names one of the guest's `handles`.
    pub fn names_any(&self, handles: &[u32]) -> bool {
        !handles.is_empty() && self.waits.iter().any(|w| w.items.iter().any(|i| handles.contains(&i.handle)))
    }

    pub fn len(&self) -> usize {
        self.waits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.waits.is_empty()
    }

    /// The waits that go on, as they are now.
    pub fn waits(&self) -> &[Wait] {
        &self.waits
    }

    /// Takes every wait out, to settle them (those that go on come back
    /// with [`Pending::restore`]).
    pub fn take(&mut self) -> Vec<Wait> {
        self.items = 0;
        core::mem::take(&mut self.waits)
    }

    /// Puts back a wait that goes on.
    pub fn restore(&mut self, wait: Wait) {
        self.items += wait.items.len();
        self.waits.push(wait);
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use abi::signals;

    fn wait(key: u64, items: usize) -> Wait {
        let item = WaitItem { handle: 7, signals: signals::READABLE, ..WaitItem::default() };
        Wait::new(key, 0x1000, vec![item; items], 1000)
    }

    #[test]
    fn the_waits_that_go_on_fit_one_wait_of_the_kernels_with_the_waiters_event() {
        let mut p = Pending::default();
        p.admit(wait(1, 1000), 0).unwrap();
        assert_eq!(p.admit(wait(2, 24), 0), Err(Error::LimitReached));
        p.admit(wait(3, 23), 0).unwrap();
        assert_eq!(p.items + 1, abi::WAIT_MANY_MAX);
        assert_eq!(p.admit(wait(4, 1), 0), Err(Error::LimitReached));
        // A wait of as many items as the kernel takes does not go on: the
        // waiter's event is one more.
        assert!(p.cancel(1));
        assert!(!p.cancel(1));
        assert_eq!(p.admit(wait(5, abi::WAIT_MANY_MAX), 0), Err(Error::LimitReached));
        p.admit(wait(6, 1000), 0).unwrap();
        // Settled: what goes on comes back with its items.
        let all = p.take();
        assert_eq!((all.len(), p.items), (2, 0));
        assert_ne!(all[0].id(), all[1].id());
        for w in all {
            p.restore(w);
        }
        assert_eq!(p.items, 1023);
    }

    #[test]
    fn the_waits_that_go_on_and_those_not_in_the_ring_yet_fit_the_ring() {
        let mut p = Pending::default();
        for key in 0..MAX_WAITS as u64 - 10 {
            p.admit(wait(key, 1), 10).unwrap();
        }
        assert_eq!(p.admit(wait(999, 1), 10), Err(Error::LimitReached));
        assert_eq!(p.len(), MAX_WAITS - 10);
    }

    #[test]
    fn a_wait_names_its_handles() {
        let mut p = Pending::default();
        p.admit(wait(1, 2), 0).unwrap();
        assert!(p.names(7));
        assert!(!p.names(8));
        assert!(p.names_any(&[8, 7]));
        assert!(!p.names_any(&[]));
        p.cancel(1);
        assert!(!p.names(7));
    }

    #[test]
    fn a_wait_finishes_when_a_signal_comes_or_its_deadline_passes() {
        let mut w = wait(1, 2);
        let nothing = w.items.clone();
        assert!(w.outcome(Round::Missed, &[], 5000).is_none());
        assert!(w.outcome(Round::Waited, &nothing, 999).is_none());
        // A signal: satisfied, what was observed in the guest's items.
        let mut seen = nothing.clone();
        seen[1].observed = signals::READABLE | signals::WRITABLE;
        seen[0].observed = signals::WRITABLE;
        let done = w.outcome(Round::Waited, &seen, 10).unwrap();
        assert_eq!((done.key, done.status, done.satisfied), (1, b::OK, 1));
        assert_eq!(w.items[0].observed, signals::WRITABLE);
        // The deadline.
        let mut w = wait(2, 1);
        let done = w.outcome(Round::Waited, &w.items.clone(), 1000).unwrap();
        assert_eq!((done.status, done.satisfied), (Error::TimedOut as u32, 0));
        // A handle gone.
        let done = wait(3, 1).outcome(Round::Failed(Error::BadHandle), &[], 0).unwrap();
        assert_eq!(done.status, Error::BadHandle as u32);
    }

    #[test]
    fn an_items_address_is_checked() {
        assert_eq!(observed_at(0x1000, 2), Some(0x1000 + 2 * 16 + 8));
        assert_eq!(observed_at(u64::MAX - 8, 0), Some(u64::MAX));
        assert_eq!(observed_at(u64::MAX - 7, 0), None);
        assert_eq!(observed_at(u64::MAX - 100, 10), None);
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the guest makes Veda keep for it, within its budget: Veda's memory
//! and the monitor's books.
//!
//! Its budget of memory counts, in bytes:
//!
//! * each VMO it made (by its size: what its pages may come to, as the
//!   guest writes into it and maps it, and Veda's services map what it
//!   hands them), while it is the guest's: while a handle of the guest's
//!   names it, or a message on its way to the guest holds one;
//! * each message it sent on a channel whose other end is its own (its
//!   bytes, and what the kernel and the monitor keep for it), until it
//!   reads it or that end goes;
//! * each mapping in its window (a VMO of its own that it maps, twice: the
//!   budget is an upper bound of what it costs).
//!
//! And its mappings are counted too, as each takes the monitor's and the
//! kernel's books (the page tables of its processors and its devices).
//!
//! So nothing the guest keeps leaves the budget by going round through its
//! own channels: a message it sends to itself is charged, and what it holds
//! stays charged, until it reads it. What it hands to Veda (in a message on
//! a channel whose other end is Veda's: a service's, which bounds what it
//! keeps of a client) is no longer the guest's once it has no other handle
//! of it.
//!
//! The monitor knows objects by their kernel ids (koids): the VMOs the
//! guest made and the ends of the channels it made (a channel's end is one
//! handle, which cannot be duplicated: in the guest's table, in a message,
//! or gone), until the guest has let go of them.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

use vabi::Error;

/// What a message costs besides its bytes: what the kernel and the monitor
/// keep for it, at most.
pub const MESSAGE_COST: u64 = 256;

/// A VMO the guest made.
#[derive(Debug)]
struct Vmo {
    size: u64,
    /// The handles of the guest's that name it, and those in messages on
    /// their way to it.
    handles: u32,
    parked: u32,
}

/// An end of a channel the guest made, while it is the guest's: in its
/// table, or in a message on its way to it.
#[derive(Debug)]
struct End {
    /// The other end.
    peer: u64,
    /// In a message on its way to the guest (else in its table).
    parked: bool,
    /// The messages the guest sent it, in order, that are not read yet.
    queue: VecDeque<Queued>,
}

/// A message the guest sent to itself.
#[derive(Debug)]
struct Queued {
    bytes: u32,
    handles: u32,
    /// What it charged.
    cost: u64,
    /// The objects of the ledger's its handles name.
    known: Vec<u64>,
}

/// The guest's books: what it makes Veda keep, and its budget.
#[derive(Debug)]
pub struct Ledger {
    /// Bytes charged, and the most.
    charged: u64,
    limit: u64,
    /// Mappings, and the most.
    mappings: u32,
    max_mappings: u32,
    /// Handles in messages on their way to the guest.
    queued_handles: u32,
    vmos: BTreeMap<u64, Vmo>,
    ends: BTreeMap<u64, End>,
}

impl Ledger {
    /// A ledger of `limit` bytes and `max_mappings` mappings.
    pub fn new(limit: u64, max_mappings: u32) -> Ledger {
        Ledger {
            charged: 0,
            limit,
            mappings: 0,
            max_mappings,
            queued_handles: 0,
            vmos: BTreeMap::new(),
            ends: BTreeMap::new(),
        }
    }

    /// Bytes charged.
    pub fn charged(&self) -> u64 {
        self.charged
    }

    /// Handles in messages the guest sent to itself (which count as its
    /// own).
    pub fn queued_handles(&self) -> u32 {
        self.queued_handles
    }

    /// Whether the ledger knows of objects that may come back to the guest
    /// (so that the handles messages bring are to be looked up).
    pub fn knows_any(&self) -> bool {
        !self.vmos.is_empty() || !self.ends.is_empty()
    }

    /// Whether `bytes` more fit the budget.
    pub fn fits(&self, bytes: u64) -> bool {
        self.charged.checked_add(bytes).is_some_and(|c| c <= self.limit)
    }

    fn charge(&mut self, bytes: u64) -> Result<(), Error> {
        if !self.fits(bytes) {
            return Err(Error::LimitReached);
        }
        self.charged += bytes;
        Ok(())
    }

    fn release(&mut self, bytes: u64) {
        self.charged = self.charged.saturating_sub(bytes);
    }

    /// The guest made VMO `koid` of `size` bytes, which a handle of its
    /// names: charged (`LimitReached`, and nothing charged, beyond the
    /// budget).
    pub fn made_vmo(&mut self, koid: u64, size: u64) -> Result<(), Error> {
        self.charge(size)?;
        self.vmos.insert(koid, Vmo { size, handles: 1, parked: 0 });
        Ok(())
    }

    /// The guest made a channel whose ends are `a` and `b`, both in its
    /// table.
    pub fn made_channel(&mut self, a: u64, b: u64) {
        self.ends.insert(a, End { peer: b, parked: false, queue: VecDeque::new() });
        self.ends.insert(b, End { peer: a, parked: false, queue: VecDeque::new() });
    }

    /// Another handle of the guest's names `koid` (a duplicate).
    pub fn duplicated(&mut self, koid: u64) {
        if let Some(v) = self.vmos.get_mut(&koid) {
            v.handles += 1;
        }
    }

    /// A message brought the guest a handle that names `koid`: the ledger's
    /// object, if it is one, is in its table again. (One that was on its
    /// way to it comes off its way as [`Ledger::read`] reads its message.)
    pub fn received(&mut self, koid: u64) {
        if let Some(v) = self.vmos.get_mut(&koid) {
            v.handles += 1;
        } else if let Some(e) = self.ends.get_mut(&koid) {
            e.parked = false;
        }
    }

    /// A handle of the guest's that names `koid` is closed.
    pub fn closed(&mut self, koid: u64) {
        if let Some(v) = self.vmos.get_mut(&koid) {
            v.handles = v.handles.saturating_sub(1);
            self.forget_vmo_if_gone(koid);
        } else if self.ends.contains_key(&koid) {
            // The kernel closes the channel's end and what is queued on it.
            self.forget_end(koid);
        }
    }

    /// Whether a message on the guest's channel end `channel` goes to the
    /// guest itself: whether the other end is its own.
    fn to_itself(&self, channel: u64) -> Option<u64> {
        let peer = self.ends.get(&channel)?.peer;
        self.ends.contains_key(&peer).then_some(peer)
    }

    /// What a message of `bytes` bytes on the guest's channel end
    /// `channel` costs: nothing if it goes to Veda.
    fn cost(&self, channel: u64, bytes: u32) -> u64 {
        if self.to_itself(channel).is_some() { bytes as u64 + MESSAGE_COST } else { 0 }
    }

    /// Whether the guest may send a message of `bytes` bytes on its channel
    /// end `channel` (koid; 0 for one the ledger does not know): one it
    /// sends itself is charged until it reads it (`LimitReached` beyond the
    /// budget).
    pub fn may_send(&self, channel: u64, bytes: u32) -> Result<(), Error> {
        if self.fits(self.cost(channel, bytes)) { Ok(()) } else { Err(Error::LimitReached) }
    }

    /// The guest sent a message of `bytes` bytes on its channel end
    /// `channel`, with handles of its that name `koids` (0 for objects the
    /// ledger does not know), which leave its table: `delivered` if the
    /// kernel queued it (else it closed the handles). To the guest itself,
    /// the message is charged, and what the handles name is on its way to
    /// it; to Veda, the guest's objects that it no longer has a handle of
    /// are Veda's.
    pub fn sent(&mut self, channel: u64, bytes: u32, koids: &[u64], delivered: bool) {
        if !delivered {
            for &k in koids {
                self.closed(k);
            }
            return;
        }
        let Some(peer) = self.to_itself(channel) else {
            for &k in koids {
                if let Some(v) = self.vmos.get_mut(&k) {
                    v.handles = v.handles.saturating_sub(1);
                    self.forget_vmo_if_gone(k);
                } else if self.ends.contains_key(&k) {
                    // Veda's from now on, and what is queued on it.
                    self.forget_end(k);
                }
            }
            return;
        };
        let mut known = Vec::new();
        for &k in koids {
            if let Some(v) = self.vmos.get_mut(&k) {
                v.handles = v.handles.saturating_sub(1);
                v.parked += 1;
                known.push(k);
            } else if let Some(e) = self.ends.get_mut(&k) {
                e.parked = true;
                known.push(k);
            }
        }
        let cost = bytes as u64 + MESSAGE_COST;
        // Checked beforehand (`may_send`): charged whatever it comes to.
        self.charged = self.charged.saturating_add(cost);
        self.queued_handles += koids.len() as u32;
        let handles = koids.len() as u32;
        if let Some(e) = self.ends.get_mut(&peer) {
            e.queue.push_back(Queued { bytes, handles, cost, known });
        }
    }

    /// The guest read a message of `bytes` bytes and `handles` handles on
    /// its channel end `channel`: if it is the next one it sent itself, its
    /// charge goes, and its handles are no longer in a message. What of the
    /// ledger's they name: once their handles are in the guest's table
    /// ([`received`](Ledger::received)) or closed, [`arrived`](Ledger::arrived).
    pub fn read(&mut self, channel: u64, bytes: u32, handles: u32) -> Vec<u64> {
        let Some(e) = self.ends.get_mut(&channel) else { return Vec::new() };
        if !e.queue.front().is_some_and(|q| q.bytes == bytes && q.handles == handles) {
            return Vec::new();
        }
        let Some(q) = e.queue.pop_front() else { return Vec::new() };
        self.release(q.cost);
        self.queued_handles = self.queued_handles.saturating_sub(q.handles);
        q.known
    }

    /// The ledger's objects a message the guest sent itself brought
    /// (`known`, as [`read`](Ledger::read) gave them) are no longer on their
    /// way: their handles are in its table, or closed.
    pub fn arrived(&mut self, known: Vec<u64>) {
        for k in known {
            if let Some(v) = self.vmos.get_mut(&k) {
                v.parked = v.parked.saturating_sub(1);
                self.forget_vmo_if_gone(k);
            }
        }
    }

    /// A mapping of `len` bytes into the window: charged (`LimitReached`
    /// beyond the budget or the most mappings).
    pub fn map(&mut self, len: u64) -> Result<(), Error> {
        if self.mappings >= self.max_mappings {
            return Err(Error::LimitReached);
        }
        self.charge(len)?;
        self.mappings += 1;
        Ok(())
    }

    /// A mapping of `len` bytes is gone.
    pub fn unmapped(&mut self, len: u64) {
        self.release(len);
        self.mappings = self.mappings.saturating_sub(1);
    }

    #[cfg(test)]
    fn knows_vmo(&self, koid: u64) -> bool {
        self.vmos.contains_key(&koid)
    }

    fn forget_vmo_if_gone(&mut self, koid: u64) {
        if self.vmos.get(&koid).is_some_and(|v| v.handles == 0 && v.parked == 0)
            && let Some(v) = self.vmos.remove(&koid)
        {
            self.release(v.size);
        }
    }

    /// Forgets channel end `koid` (closed, or Veda's), and what is queued
    /// on it: the messages the guest sent it, the guest's objects in them.
    /// (A channel's end may hold others in its messages, and they theirs:
    /// one after another, not recursively.)
    fn forget_end(&mut self, koid: u64) {
        let mut going = alloc::vec![koid];
        while let Some(k) = going.pop() {
            let Some(e) = self.ends.remove(&k) else { continue };
            for q in e.queue {
                self.release(q.cost);
                self.queued_handles = self.queued_handles.saturating_sub(q.handles);
                for known in q.known {
                    if let Some(v) = self.vmos.get_mut(&known) {
                        v.parked = v.parked.saturating_sub(1);
                        self.forget_vmo_if_gone(known);
                    } else if self.ends.get(&known).is_some_and(|e| e.parked) {
                        going.push(known);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    #[test]
    fn a_vmo_the_guest_made_is_charged_while_it_has_a_handle_of_it() {
        let mut l = Ledger::new(64 * MIB, 16);
        l.made_vmo(1, 16 * MIB).unwrap();
        l.duplicated(1);
        assert_eq!(l.charged(), 16 * MIB);
        l.closed(1);
        assert_eq!(l.charged(), 16 * MIB);
        l.closed(1);
        assert_eq!(l.charged(), 0);
        assert!(!l.knows_any());
    }

    #[test]
    fn nothing_is_made_beyond_the_budget() {
        let mut l = Ledger::new(64 * MIB, 16);
        l.made_vmo(1, 48 * MIB).unwrap();
        assert!(!l.fits(32 * MIB));
        assert_eq!(l.made_vmo(2, 32 * MIB), Err(Error::LimitReached));
        assert_eq!(l.charged(), 48 * MIB);
        assert_eq!(l.map(32 * MIB), Err(Error::LimitReached));
        l.made_vmo(3, 16 * MIB).unwrap();
        assert_eq!(l.charged(), 64 * MIB);
        assert_eq!(l.made_vmo(4, u64::MAX), Err(Error::LimitReached));
    }

    #[test]
    fn mappings_are_charged_and_counted() {
        let mut l = Ledger::new(64 * MIB, 2);
        l.map(8 * MIB).unwrap();
        l.map(4096).unwrap();
        assert_eq!(l.map(4096), Err(Error::LimitReached));
        l.unmapped(4096);
        l.map(4096).unwrap();
        l.unmapped(4096);
        l.unmapped(8 * MIB);
        assert_eq!(l.charged(), 0);
    }

    /// A channel to Veda: one the guest made (ends 10 and 11), whose end 11
    /// it sent on its registry channel (0: one the ledger does not know).
    fn connection(l: &mut Ledger) -> u64 {
        l.made_channel(10, 11);
        l.may_send(0, 100).unwrap();
        l.sent(0, 100, &[11], true);
        assert_eq!(l.charged(), 0);
        10
    }

    #[test]
    fn what_the_guest_hands_veda_is_veda_s_once_it_has_no_handle_of_it() {
        let mut l = Ledger::new(64 * MIB, 16);
        let to_veda = connection(&mut l);
        // A picture: the guest keeps it, and hands Veda a duplicate.
        l.made_vmo(1, 8 * MIB).unwrap();
        l.duplicated(1);
        l.sent(to_veda, 64, &[1], true);
        assert_eq!(l.charged(), 8 * MIB);
        l.closed(1);
        assert_eq!(l.charged(), 0);
        // Its only handle, at once.
        l.made_vmo(2, 8 * MIB).unwrap();
        l.sent(to_veda, 64, &[2], true);
        assert_eq!(l.charged(), 0);
        assert!(l.ends.contains_key(&to_veda) && !l.ends.contains_key(&11));
    }

    #[test]
    fn what_the_guest_sends_itself_stays_charged_until_it_reads_it() {
        let mut l = Ledger::new(64 * MIB, 16);
        l.made_channel(10, 11);
        l.made_vmo(1, 8 * MIB).unwrap();
        l.duplicated(1);
        // A duplicate sent to itself, the other closed: still the guest's.
        l.may_send(10, 100).unwrap();
        l.sent(10, 100, &[1, 0], true);
        l.closed(1);
        assert_eq!(l.charged(), 8 * MIB + 100 + MESSAGE_COST);
        assert_eq!(l.queued_handles(), 2);
        // Read back, and closed.
        let known = l.read(11, 100, 2);
        assert_eq!(known, [1]);
        assert_eq!(l.queued_handles(), 0, "the handles are no longer in a message");
        l.received(1);
        l.arrived(known);
        assert_eq!(l.charged(), 8 * MIB);
        l.closed(1);
        assert_eq!(l.charged(), 0);
    }

    #[test]
    fn messages_to_itself_are_within_the_budget() {
        let mut l = Ledger::new(MIB, 16);
        l.made_channel(10, 11);
        let mut sent = 0;
        while l.may_send(10, 64 * 1024).is_ok() {
            l.sent(10, 64 * 1024, &[], true);
            sent += 1;
        }
        assert_eq!(sent, MIB / (64 * 1024 + MESSAGE_COST));
        // Veda's messages are not the guest's to pay for.
        l.made_channel(20, 21);
        l.sent(0, 0, &[21], true);
        assert_eq!(l.may_send(20, 64 * 1024), Ok(()));
        // Reading one makes room for another.
        assert!(l.read(11, 64 * 1024, 0).is_empty());
        assert!(l.may_send(10, 64 * 1024).is_ok());
        // The end closed: what was queued on it goes with it.
        l.closed(11);
        assert_eq!(l.charged(), 0);
        assert_eq!(l.may_send(10, 64 * 1024), Ok(()));
        assert!(l.to_itself(10).is_none());
    }

    #[test]
    fn a_message_the_guest_did_not_send_reads_nothing_off_its_queue() {
        let mut l = Ledger::new(64 * MIB, 16);
        l.made_channel(10, 11);
        l.sent(10, 100, &[], true);
        l.read(11, 50, 0);
        l.read(11, 100, 1);
        assert_eq!(l.charged(), 100 + MESSAGE_COST);
        l.read(11, 100, 0);
        assert_eq!(l.charged(), 0);
        // A VMO read back whose handle does not fit the table goes.
        l.made_vmo(1, MIB).unwrap();
        l.sent(10, 8, &[1], true);
        let known = l.read(11, 8, 1);
        l.received(1);
        l.closed(1);
        l.arrived(known);
        assert_eq!(l.charged(), 0);
        assert!(!l.knows_vmo(1));
    }

    #[test]
    fn a_failed_send_closes_the_handles() {
        let mut l = Ledger::new(64 * MIB, 16);
        l.made_channel(10, 11);
        l.made_vmo(1, MIB).unwrap();
        l.sent(10, 8, &[1, 11], false);
        assert_eq!(l.charged(), 0);
        assert!(!l.ends.contains_key(&11));
    }

    #[test]
    fn closing_an_end_lets_go_of_what_its_messages_hold() {
        let mut l = Ledger::new(64 * MIB, 16);
        // A channel in a message on another, holding a VMO in a message of
        // its own.
        l.made_channel(10, 11);
        l.made_channel(20, 21);
        l.made_vmo(1, 4 * MIB).unwrap();
        l.sent(20, 8, &[1], true);
        l.sent(10, 8, &[21], true);
        assert_eq!(l.charged(), 4 * MIB + 2 * (8 + MESSAGE_COST));
        assert_eq!(l.queued_handles(), 2);
        // The outer end closed: the inner one goes, the VMO with it.
        l.closed(11);
        assert_eq!(l.charged(), 0);
        assert_eq!(l.queued_handles(), 0);
        assert!(!l.vmos.contains_key(&1));
        assert!(!l.ends.contains_key(&21));
        // Its other end is no longer the guest's to send itself on.
        assert!(l.to_itself(20).is_none());
    }

    #[test]
    fn an_end_on_its_way_to_the_guest_is_still_its_own() {
        let mut l = Ledger::new(64 * MIB, 16);
        l.made_channel(10, 11);
        l.made_channel(20, 21);
        // 21 in a message to 11: what 20 sends still goes to the guest.
        l.sent(10, 8, &[21], true);
        assert_eq!(l.to_itself(20), Some(21));
        l.sent(20, 8, &[], true);
        // 21 read back: in the table again, its queue as it was.
        let known = l.read(11, 8, 1);
        l.received(21);
        l.arrived(known);
        assert_eq!(l.charged(), 8 + MESSAGE_COST);
        assert!(!l.ends[&21].parked);
        // Sent to Veda: Veda's, and its queue with it.
        l.sent(0, 8, &[21], true);
        assert_eq!(l.charged(), 0);
    }
}

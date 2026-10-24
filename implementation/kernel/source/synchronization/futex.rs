// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Futexes: the kernel half of user-space mutexes and condition variables.
//!
//! A futex is the word in the memory that holds it: a word of a VMO is
//! identified by (VMO, offset), so that every mapping of the VMO, in every
//! process, reaches the same futex (the locks and semaphores C programs keep
//! in memory they share wait and wake across them); a word of private memory
//! (`vm_allocate`), which no other mapping reaches, by (process, address).
//! Processes and VMOs take their ids from one counter, so the two kinds of
//! keys never meet.
//!
//! `wait` blocks only if the 32-bit word at the address still holds the
//! expected value, which closes the race between user space checking a lock
//! word and sleeping.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;

use abi::Error;

use super::SpinLock;
use crate::memory::user;
use crate::scheduler::{self, Thread, WakeReason};

/// Waiters per futex, keyed by (VMO, offset), or (process, user address) in
/// private memory, by kernel object ids.
type WaitQueues = BTreeMap<(u64, u64), VecDeque<Arc<Thread>>>;

static QUEUES: SpinLock<WaitQueues> = SpinLock::new(BTreeMap::new());

fn key(addr: u64) -> Result<(u64, u64), Error> {
    if !addr.is_multiple_of(4) {
        return Err(Error::InvalidArgs);
    }
    let cur = scheduler::current();
    let p = cur.process.as_ref().ok_or(Error::BadState)?;
    Ok(p.aspace().and_then(|a| a.shared_location(addr)).unwrap_or((p.koid, addr)))
}

pub fn wait(addr: u64, expected: u32, deadline: Option<u64>, flags: usize) -> Result<(), Error> {
    let how = scheduler::Interruptible::from_flags(flags, 0)?;
    let k = key(addr)?;
    let value: u32 = user::read(addr)?;
    if value != expected {
        return Err(Error::ShouldWait);
    }
    let cur = scheduler::current();
    QUEUES.lock().entry(k).or_default().push_back(cur.clone());
    let reason = scheduler::block_interruptible(deadline, how);
    // Remove ourselves if we were not dequeued by a waker.
    {
        let mut q = QUEUES.lock();
        if let Some(list) = q.get_mut(&k) {
            list.retain(|t| t.koid != cur.koid);
            if list.is_empty() {
                q.remove(&k);
            }
        }
    }
    match reason {
        WakeReason::TimedOut => Err(Error::TimedOut),
        WakeReason::Killed => Err(Error::Canceled),
        WakeReason::Interrupted => Err(Error::Interrupted),
        _ => Ok(()),
    }
}

pub fn wake(addr: u64, count: usize) -> Result<usize, Error> {
    let k = key(addr)?;
    let mut woken = alloc::vec::Vec::new();
    {
        let mut q = QUEUES.lock();
        if let Some(list) = q.get_mut(&k) {
            while woken.len() < count {
                match list.pop_front() {
                    Some(t) => woken.push(t),
                    None => break,
                }
            }
            if list.is_empty() {
                q.remove(&k);
            }
        }
    }
    let n = woken.len();
    for t in woken {
        scheduler::wake(&t, WakeReason::Signaled);
    }
    Ok(n)
}

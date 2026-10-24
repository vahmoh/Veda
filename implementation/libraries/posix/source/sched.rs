// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Scheduling: how favoured a process's threads are, and the processors
//! they run on.
//!
//! A process has one nice value (as POSIX has it; Linux keeps one per
//! thread), from -20, the most favoured, to 19, which the programs it
//! starts inherit (`inherit`). Its threads run at the kernel priority the
//! value makes ([`kernel_priority`]): 0 is `abi::priority::NORMAL`, where
//! the kernel starts threads and the most an application's may run at; 19
//! is `abi::priority::LOW` and -20 `abi::priority::HIGH`, in even steps.
//! The kernel's priorities are strict, where Linux shares the processors
//! out by weight: a thread runs while none above it is ready, or once it
//! has waited long enough to be given a turn.
//!
//! Every thread is scheduled as Linux's are by default (`SCHED_OTHER`):
//! real-time and idle scheduling cannot be had (`EPERM`). Every thread may
//! run on every processor: an affinity that leaves some out cannot be had
//! (`EOPNOTSUPP`).

use rt::sync::Mutex;

use crate::error::SysResult;
use crate::linux::errno::{EINVAL, EOPNOTSUPP, EPERM, ESRCH};
use crate::linux::{policy, prio};
use crate::user;

/// The process's nice value; held while it changes, so that its threads
/// run at the priority of the value it has.
static NICE: Mutex<i32> = Mutex::new(0);

/// The kernel priority the threads of nice value `nice` run at.
fn kernel_priority(nice: i32) -> usize {
    use abi::priority::{HIGH, LOW, NORMAL};
    let steps = nice.unsigned_abs() as usize;
    if nice >= 0 {
        NORMAL - (steps * (NORMAL - LOW)).div_ceil(19)
    } else {
        NORMAL + (steps * (HIGH - NORMAL)).div_ceil(20)
    }
}

/// The process's nice value, which the programs it starts inherit.
pub fn nice() -> i32 {
    *NICE.lock()
}

/// Takes up the nice value the process inherited, at startup: one that
/// would favour it more than it may be keeps it at 0.
pub fn inherit(nice: i32) {
    if nice != 0 {
        let _ = set_nice(nice);
    }
}

/// Has the process's threads, and those it starts, run at nice value
/// `nice` (from -20 to 19): `EACCES` if it is above what the process may run
/// at, which changes nothing.
fn set_nice(nice: i32) -> Result<(), isize> {
    let mut current = NICE.lock();
    crate::thread::set_priority(kernel_priority(nice))?;
    *current = nice;
    Ok(())
}

/// Whether `who` names this process for `which`: 0, or its id, its
/// group's or its user's. A process's priority is its own: those of other
/// processes, of its group or user too, are not this one's to read or set
/// (`ESRCH`, as for processes that are not there).
fn names_this_process(which: u32, who: u32) -> Result<(), isize> {
    let id = match which {
        prio::PROCESS | prio::PGRP => crate::process::pid() as u32,
        prio::USER => crate::vfs::UID,
        _ => return Err(EINVAL),
    };
    if who == 0 || who == id { Ok(()) } else { Err(ESRCH) }
}

/// `getpriority`: 20 less the nice value, as the system call has it (the C
/// library makes the nice value of it).
pub fn getpriority(which: u32, who: u32) -> SysResult {
    names_this_process(which, who)?;
    Ok((20 - nice()) as usize)
}

/// `setpriority`: the process's nice value, kept within -20 and 19. Below
/// 0 favours it above `priority::NORMAL`, which services may be and
/// applications not (`EACCES`, as for Linux's unprivileged programs).
pub fn setpriority(which: u32, who: u32, nice: i32) -> SysResult {
    names_this_process(which, who)?;
    set_nice(nice.clamp(-20, 19)).map(|()| 0)
}

/// Whether `tid` names one of this process's threads, as the affinity
/// calls take it: 0 (the caller), the process's id (its main thread's) or
/// another thread's.
fn names_a_thread(tid: i32) -> Result<(), isize> {
    if tid == 0 || tid == crate::process::pid() || crate::thread::exists(tid) { Ok(()) } else { Err(ESRCH) }
}

/// `sched_getscheduler`: `SCHED_OTHER`, every thread's.
pub fn sched_getscheduler(tid: i32) -> SysResult {
    names_a_thread(tid)?;
    Ok(policy::NORMAL as usize)
}

/// `sched_getparam`: the priority of `SCHED_OTHER`, 0.
///
/// # Safety
/// `param` must be a writable `struct sched_param`.
pub unsafe fn sched_getparam(tid: i32, param: usize) -> SysResult {
    names_a_thread(tid)?;
    // SAFETY: per the caller.
    unsafe { user::write(param, 0i32)? };
    Ok(0)
}

/// `sched_setscheduler`, and `sched_setparam` (no `policy`): `SCHED_OTHER`
/// at priority 0, which every thread has, and `SCHED_BATCH`, Linux's hint
/// that a thread computes, change nothing; the real-time policies and
/// `SCHED_IDLE` cannot be had (`EPERM`, as without Linux's real-time
/// limits).
///
/// # Safety
/// `param` must be a readable `struct sched_param`.
pub unsafe fn sched_setscheduler(tid: i32, policy: Option<u32>, param: usize) -> SysResult {
    names_a_thread(tid)?;
    // SAFETY: per the caller.
    let priority: i32 = unsafe { user::read(param)? };
    let (min, max) = priority_range(policy.map_or(policy::NORMAL, |p| p & !policy::RESET_ON_FORK))?;
    if !(min..=max).contains(&priority) {
        return Err(EINVAL);
    }
    match policy.map(|p| p & !policy::RESET_ON_FORK) {
        None | Some(policy::NORMAL | policy::BATCH) => Ok(0),
        Some(policy::DEADLINE) => Err(EINVAL),
        Some(_) => Err(EPERM),
    }
}

/// The priorities of a scheduling policy, as Linux has them
/// (`sched_get_priority_min`, `sched_get_priority_max`): 1 to 99 for the
/// real-time ones, 0 for the others.
pub fn priority_range(of: u32) -> Result<(i32, i32), isize> {
    match of {
        policy::FIFO | policy::RR => Ok((1, 99)),
        policy::NORMAL | policy::BATCH | policy::IDLE | policy::DEADLINE => Ok((0, 0)),
        _ => Err(EINVAL),
    }
}

/// The number of processors.
fn processors() -> usize {
    rt::object::system_info().map_or(1, |i| i.cpu_count.max(1)) as usize
}

/// `sched_getaffinity`: every processor, in a mask of 64-bit words (as
/// Linux stores it). Returns the mask's size in bytes.
///
/// # Safety
/// `mask` must be writable for `size` bytes.
pub unsafe fn sched_getaffinity(tid: i32, size: usize, mask: usize) -> SysResult {
    names_a_thread(tid)?;
    let cpus = processors();
    let bytes = cpus.div_ceil(64) * 8;
    if size < bytes {
        return Err(EINVAL);
    }
    // SAFETY: the program passed `size` bytes.
    let out = unsafe { user::slice_mut(mask, bytes)? };
    out.fill(0);
    for c in 0..cpus {
        out[c / 8] |= 1 << (c % 8);
    }
    Ok(bytes)
}

/// `sched_setaffinity`: a mask of every processor changes nothing; one
/// that leaves some out cannot be had (`EOPNOTSUPP`), and one with none of
/// them is refused (`EINVAL`, as on Linux). Bits beyond the last
/// processor are not looked at.
///
/// # Safety
/// `mask` must be readable for `size` bytes.
pub unsafe fn sched_setaffinity(tid: i32, size: usize, mask: usize) -> SysResult {
    names_a_thread(tid)?;
    let cpus = processors();
    // SAFETY: the program passed `size` bytes.
    let given = unsafe { user::slice(mask, size.min(cpus.div_ceil(8)))? };
    let named = (0..cpus).filter(|&c| given.get(c / 8).is_some_and(|b| b & (1 << (c % 8)) != 0)).count();
    match named {
        0 => Err(EINVAL),
        n if n == cpus => Ok(0),
        _ => Err(EOPNOTSUPP),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_values_are_kernel_priorities_in_even_steps() {
        use abi::priority::{HIGH, LOW, NORMAL};
        assert_eq!(kernel_priority(0), NORMAL);
        assert_eq!(kernel_priority(1), NORMAL - 1);
        assert_eq!(kernel_priority(19), LOW);
        assert_eq!(kernel_priority(-1), NORMAL + 1);
        assert_eq!(kernel_priority(-20), HIGH);
        // The nicer, the less favoured.
        for n in -20..19 {
            assert!(kernel_priority(n) >= kernel_priority(n + 1), "{n}");
        }
    }
}

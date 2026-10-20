// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! When memory runs out ([`abi::memory_class`]): the kernel ends the most
//! expendable process, of its class the one that holds the most, rather
//! than the process whose access found no page, which carries on; and a
//! process moves only towards the most expendable class.

use alloc::format;
use alloc::string::{String, ToString};

use abi::{Error, map_flags, memory_class, process_state};
use rt::object::Process;
use rt::time::Duration;
use rt::vm;

use crate::posix::start_copy_with;
use crate::{TestResult, check};

const MIB: usize = 1 << 20;
const PAGE: usize = abi::PAGE_SIZE;
/// What the memory copy fills memory down to, in MiB: enough for the
/// system's services to carry on meanwhile, which never touch so much.
const LEFT_MIB: usize = 32;
/// What the expendable copy holds, in MiB.
const HELD_MIB: usize = 48;

fn s<E: core::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Memory programs may still take, beyond the kernel's reserve.
fn left() -> usize {
    rt::object::system_info().map_or(0, |i| i.free_memory as usize)
}

/// Touches `len` bytes of new private memory, writing to each page a byte
/// of its own (`seed` and its number). Returns where it is.
fn touch(len: usize, seed: u8) -> Result<usize, Error> {
    let at = vm::allocate(None, len, 0, map_flags::READ | map_flags::WRITE)?;
    for (i, page) in (at..at + len).step_by(PAGE).enumerate() {
        // SAFETY: our fresh, writable memory.
        unsafe { core::ptr::write_volatile(page as *mut u8, seed.wrapping_add(i as u8)) };
    }
    Ok(at)
}

/// Whether the memory [`touch`] wrote still holds what it wrote.
fn intact(at: usize, len: usize, seed: u8) -> bool {
    (at..at + len).step_by(PAGE).enumerate().all(|(i, page)| {
        // SAFETY: memory `touch` mapped and wrote, mapped still.
        unsafe { core::ptr::read_volatile(page as *const u8) == seed.wrapping_add(i as u8) }
    })
}

/// Waits until `done`, looking every 50 ms, for `secs` at most.
fn until(secs: u64, what: &str, mut done: impl FnMut() -> bool) -> TestResult {
    let deadline = rt::time::deadline_after(Duration::from_secs(secs));
    while !done() {
        if rt::time::now_ns() >= deadline {
            return Err(format!("{what} not within {secs} s"));
        }
        rt::time::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// The state of `p` (`abi::process_state`), or 0 if it cannot be read.
fn state(p: &Process) -> u32 {
    p.info().map_or(0, |i| i.state)
}

/// An application that takes all but a little of the memory, and a smaller
/// process that made itself expendable: when this process then touches
/// more memory than is left, the expendable one is ended, not this one, nor
/// the application that holds the most. When it touches more again, the
/// application holding the most goes, never this one, which holds less.
/// Each is ended for memory, and what this process wrote is intact.
pub fn test_memory_runs_out() -> TestResult {
    let me = rt::object::process_self_info().map_err(s)?;
    check(
        me.memory_class as usize == memory_class::APPLICATION,
        "a program the kernel command line asked for is an application's memory class",
    )?;
    if left() < 256 * MIB {
        return Err(format!("only {} MiB left for programs to start with", left() / MIB));
    }
    let held_mib = HELD_MIB.to_string();
    let held = start_copy_with(&["hold", &held_mib, "expendable"])?;
    until(20, "the expendable copy holding its memory", || {
        held.info().is_ok_and(|i| {
            i.memory_bytes as usize >= HELD_MIB * MIB && i.memory_class as usize == memory_class::EXPENDABLE
        })
    })?;
    check(
        rt::process::set_memory_class(Some(&held), memory_class::APPLICATION) == Err(Error::AccessDenied),
        "a process is not moved back from the expendable class",
    )?;
    check(rt::process::set_memory_class(Some(&held), 3) == Err(Error::InvalidArgs), "an unknown class is refused")?;
    let left_mib = LEFT_MIB.to_string();
    let filler = start_copy_with(&["fill", &left_mib])?;
    let result = (|| {
        until(60, "memory filled", || left() <= (LEFT_MIB + 16) * MIB && state(&filler) == process_state::RUNNING)?;
        check(state(&held) == process_state::RUNNING, "the expendable copy holds its memory still")?;
        // More than is left: the expendable copy makes room.
        let first = 2 * LEFT_MIB * MIB;
        let a = touch(first, 1).map_err(s)?;
        check(state(&held) == process_state::OUT_OF_MEMORY, "the expendable copy is ended for memory")?;
        check(state(&filler) == process_state::RUNNING, "the application holding the most is not, yet")?;
        check(intact(a, first, 1), "this process's memory holds what it wrote")?;
        // Again: the application holding the most goes, not this one.
        let second = left() + 64 * MIB;
        let b = touch(second, 2).map_err(s)?;
        check(state(&filler) == process_state::OUT_OF_MEMORY, "the application holding the most is ended")?;
        check(intact(a, first, 1) && intact(b, second, 2), "this process's memory holds what it wrote still")?;
        vm::unmap(None, a, first).map_err(s)?;
        vm::unmap(None, b, second).map_err(s)
    })();
    // Whatever failed, nothing is left holding the memory.
    let _ = filler.kill();
    let _ = held.kill();
    result
}

/// `systest hold MIB [expendable]`: moves itself to the expendable class
/// if asked, touches MIB of memory, then waits to be ended.
pub fn hold() -> i32 {
    let args = rt::env::args();
    let Some(mib) = args.get(2).and_then(|a| a.parse::<usize>().ok()) else { return 2 };
    if args.get(3).is_some_and(|a| a == "expendable")
        && rt::process::set_memory_class(None, memory_class::EXPENDABLE).is_err()
    {
        return 3;
    }
    if touch(mib * MIB, 0).is_err() {
        return 4;
    }
    loop {
        rt::time::sleep(Duration::from_secs(60));
    }
}

/// `systest fill MIB`: touches memory until only MIB is left to programs,
/// then waits to be ended. It never touches more than is left, so that its
/// own accesses end no one.
pub fn fill() -> i32 {
    const STEP: usize = 16 * MIB;
    let Some(mib) = rt::env::args().get(2).and_then(|a| a.parse::<usize>().ok()) else { return 2 };
    let target = mib * MIB;
    loop {
        let spare = left().saturating_sub(target);
        if spare < PAGE {
            break;
        }
        // A little short of what is spare, as others take memory too.
        let step = spare.min(STEP);
        let len = (step - step / 8) & !(PAGE - 1);
        if len == 0 || touch(len, 0).is_err() {
            break;
        }
    }
    loop {
        rt::time::sleep(Duration::from_secs(60));
    }
}

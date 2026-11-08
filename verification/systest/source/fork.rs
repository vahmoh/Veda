// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `process_fork`: a process that is a copy of this one. Its memory is this
//! one's, copied as either writes it (each side's writes its own, even
//! where a side made memory writable again, a copy costing nothing until
//! written), but for shared memory, and memory mapped
//! through a handle that could not have gone to it, which it has not; the
//! handles named move to it under the same names, and its other names
//! reach nothing; it has one thread, and what this process gave up
//! (`process_restrict`). What the call refuses changes nothing.
//!
//! What it has of a range is as `vm_inherit` said: nothing of a range kept
//! from it, zeros where private memory was wiped for it, as its own copies
//! have; it holds no frame of either, so that this process's writes there
//! cost nothing, and what it unmaps there is free at once.
//!
//! What the kernel takes for copies is free again once they are gone,
//! however many were alive at once.
//!
//! The copy runs in the middle of this program: it touches neither the
//! heap nor a lock another thread of this one may have held as it was
//! made, and talks over a channel with system calls alone.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use abi::{Error, RawHandle, Rights, map_flags, nr, signals};
use runtime::object::{Channel, Event, Vmo};
use runtime::println;
use runtime::sys::call;
use runtime::vm;

use crate::{TestResult, check};

const PAGE: usize = abi::PAGE_SIZE;
/// The private memory the copy has: enough to see that a copy costs
/// nothing until it is written, and that writing it does.
const PRIVATE_LEN: usize = 32 << 20;

fn s<E: core::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Memory programs may still take.
fn left() -> usize {
    runtime::object::system_info().map_or(0, |i| i.free_memory as usize)
}

fn peek(at: usize) -> u8 {
    // SAFETY: memory the test mapped, readable.
    unsafe { core::ptr::read_volatile(at as *const u8) }
}

fn poke(at: usize, value: u8) {
    // SAFETY: memory the test mapped, writable.
    unsafe { core::ptr::write_volatile(at as *mut u8, value) }
}

/// Writes `value` to the first byte of every page of `[at, at+len)`.
fn fill(at: usize, len: usize, value: u8) {
    (at..at + len).step_by(PAGE).for_each(|p| poke(p, value));
}

/// Whether the first byte of every page of `[at, at+len)` is `value`.
fn holds(at: usize, len: usize, value: u8) -> bool {
    (at..at + len).step_by(PAGE).all(|p| peek(p) == value)
}

/// Sends a word on channel `ch`, as the copy can: a system call alone.
fn send(ch: RawHandle, word: u32) -> bool {
    let bytes = word.to_le_bytes();
    call(nr::CHANNEL_WRITE, [ch as usize, bytes.as_ptr() as usize, 4, 0, 0, 0]).is_ok()
}

/// The next word on channel `ch`, waiting for it for 20 s at most.
fn receive(ch: RawHandle) -> Option<u32> {
    let deadline = runtime::time::now_ns() + 20_000_000_000;
    call(nr::OBJECT_WAIT_ONE, [ch as usize, signals::READABLE as usize, deadline as usize, 0, 0, 0]).ok()?;
    let mut bytes = [0u8; 4];
    let mut actual = [0u32; 2];
    let args = [ch as usize, bytes.as_mut_ptr() as usize, 4, 0, 0, actual.as_mut_ptr() as usize];
    call(nr::CHANNEL_READ, args).ok()?;
    Some(u32::from_le_bytes(bytes))
}

/// Whether the name `h` reaches a handle in this process.
fn reaches(h: RawHandle) -> bool {
    let mut info = [0u8; 16];
    let basic = abi::info_topic::HANDLE_BASIC;
    call(nr::OBJECT_INFO, [h as usize, basic, info.as_mut_ptr() as usize, info.len(), 0, 0]).is_ok()
}

/// Ends the copy with `code`.
fn end(code: i64) -> ! {
    runtime::sys::process_exit(code)
}

/// Where the test's memory is.
#[derive(Clone, Copy)]
struct Memory {
    /// Private memory, [`PRIVATE_LEN`] long.
    private: usize,
    /// A page of a VMO whose handle is closed.
    own: usize,
    /// A page mapped `SHARED`, its handle closed.
    shared: usize,
    /// A page mapped through a handle without `TRANSFER`.
    hidden: usize,
}

/// What the copy does, talking on channel `ch`: what it found of its
/// memory and names, then (told to go on) whether what it wrote is its own
/// still, after the original wrote its own (in a page it protected anew,
/// too); then, writing all its private memory, it waits to be told to end.
fn copy(ch: RawHandle, m: Memory, stays: RawHandle) -> ! {
    let mut found = 0;
    if holds(m.private, PRIVATE_LEN, 1) && peek(m.own) == 2 && peek(m.shared) == 3 {
        found |= 1;
    }
    // No mapping there: protecting it finds nothing.
    if call(nr::VM_PROTECT, [0, m.hidden, PAGE, map_flags::READ, 0, 0]) == Err(Error::NotFound) {
        found |= 2;
    }
    if !reaches(stays) {
        found |= 4;
    }
    poke(m.private, 5);
    poke(m.own, 6);
    poke(m.shared, 7);
    if !send(ch, found) || receive(ch) != Some(0) {
        end(1);
    }
    let own = peek(m.private) == 5 && peek(m.private + PAGE) == 1 && peek(m.own) == 6 && peek(m.shared) == 9;
    fill(m.private, PRIVATE_LEN, 8);
    if !send(ch, u32::from(own)) || receive(ch) != Some(0) {
        end(2);
    }
    end(0)
}

pub fn test_fork() -> TestResult {
    let rw = map_flags::READ | map_flags::WRITE;
    // What the call refuses changes nothing: a name that is not this
    // process's, one twice, a handle that cannot be transferred.
    let event = Event::create().map_err(s)?;
    let fixed = Event::from_handle(event.0.duplicate(Some(Rights(Rights::WAIT.0 | Rights::SIGNAL.0))).map_err(s)?);
    let refused = [
        (runtime::process::fork(&[0x7fff_fff0]).err(), Error::BadHandle, "a name of no handle's"),
        (runtime::process::fork(&[event.raw(), event.raw()]).err(), Error::InvalidArgs, "a name twice"),
        (runtime::process::fork(&[fixed.raw()]).err(), Error::AccessDenied, "a handle that cannot be transferred"),
    ];
    for (got, expected, what) in refused {
        check(got == Some(expected), &format!("{what} is refused: {got:?}"))?;
    }
    check(event.0.basic_info().is_ok() && fixed.0.basic_info().is_ok(), "a refused call moves nothing")?;

    let private = vm::allocate(None, PRIVATE_LEN, 0, rw).map_err(s)?;
    fill(private, PRIVATE_LEN, 1);
    let vmo = Vmo::create(PAGE).map_err(s)?;
    let own = vm::map(None, &vmo, 0, PAGE, 0, rw).map_err(s)?;
    poke(own, 2);
    drop(vmo);
    let vmo = Vmo::create(PAGE).map_err(s)?;
    let shared = vm::map(None, &vmo, 0, PAGE, 0, rw | map_flags::SHARED).map_err(s)?;
    poke(shared, 3);
    drop(vmo);
    let vmo = Vmo::create(PAGE).map_err(s)?;
    let rights = vmo.0.basic_info().map_err(s)?.rights & !Rights::TRANSFER.0;
    let kept = Vmo::from_handle(vmo.0.duplicate(Some(Rights(rights))).map_err(s)?);
    drop(vmo);
    let hidden = vm::map(None, &kept, 0, PAGE, 0, rw).map_err(s)?;
    poke(hidden, 4);
    check(
        vm::map(None, &kept, 0, PAGE, 0, rw | map_flags::SHARED) == Err(Error::AccessDenied),
        "memory shared through a handle that cannot be transferred is refused",
    )?;
    let m = Memory { private, own, shared, hidden };

    let (mine, theirs) = Channel::create().map_err(s)?;
    let before = left();
    let me = runtime::object::process_self_info().map_err(s)?.koid;
    let child = match runtime::process::fork(&[theirs.raw()]) {
        Ok(Some(child)) => child,
        Ok(None) => copy(theirs.raw(), m, event.raw()),
        Err(e) => return Err(format!("process_fork: {e}")),
    };
    // It is the copy's now.
    let theirs = theirs.0.into_raw();
    let result = (|| {
        check(!reaches(theirs), "a handle moved is the copy's alone")?;
        let found = receive(mine.raw()).ok_or("the copy said nothing")?;
        check(found & 1 != 0, "the copy has the memory as it was")?;
        check(found & 2 != 0, "the copy has no mapping through a handle that could not have gone to it")?;
        check(found & 4 != 0, "the copy's names of handles that did not move reach nothing")?;
        check(peek(private) == 1 && peek(own) == 2, "the copy's writes are its own")?;
        check(peek(shared) == 7, "shared memory is shared")?;
        let info = child.info().map_err(s)?;
        check(info.threads == 1 && info.parent_koid == me, "the copy has one thread, and this process for its parent")?;
        let taken = before.saturating_sub(left());
        check(taken < PRIVATE_LEN / 2, &format!("a copy costs nothing until written ({} KiB taken)", taken >> 10))?;
        poke(private, 10);
        poke(own, 11);
        poke(shared, 9);
        // Made writable again, a page the copy shares is still copied as
        // it is written.
        vm::protect(None, private + PAGE, PAGE, map_flags::READ).map_err(s)?;
        vm::protect(None, private + PAGE, PAGE, rw).map_err(s)?;
        poke(private + PAGE, 12);
        check(send(mine.raw(), 0) && receive(mine.raw()) == Some(1), "what each wrote after the copy is its own")?;
        let taken = before.saturating_sub(left());
        check(taken >= PRIVATE_LEN / 2, &format!("a copy written costs memory ({} KiB taken)", taken >> 10))?;
        check(
            holds(private + 2 * PAGE, PRIVATE_LEN - 2 * PAGE, 1) && peek(private) == 10 && peek(private + PAGE) == 12,
            "this process's memory is its own",
        )?;
        check(send(mine.raw(), 0), "the copy told to end")?;
        let ended = child.0.wait(signals::TERMINATED, runtime::time::now_ns() + 20_000_000_000).map_err(s)?;
        check(ended & signals::TERMINATED != 0 && child.info().map_err(s)?.exit_code == 0, "the copy ended well")?;
        // A page this process has not written since the copy, whose frame
        // no one else holds now, is read-only still, as a TLB may remember
        // it once read: the kernel writes a system call's answer there all
        // the same.
        let page = private + 3 * PAGE;
        check(peek(page) == 1, "this process's memory is its own")?;
        let basic = abi::info_topic::HANDLE_BASIC;
        let answered = call(nr::OBJECT_INFO, [event.raw() as usize, basic, page, 16, 0, 0]).is_ok();
        check(answered && peek(page + PAGE) == 1, "the kernel writes where the copy's memory was shared")?;
        let back = left();
        check(
            back + PRIVATE_LEN / 2 >= before,
            &format!("its memory goes with it ({} KiB fewer free)", before.saturating_sub(back) >> 10),
        )
    })();
    let _ = child.kill();
    vm::unmap(None, private, PRIVATE_LEN).map_err(s)?;
    for page in [own, shared, hidden] {
        vm::unmap(None, page, PAGE).map_err(s)?;
    }
    result?;
    test_restrictions_inherited()
}

/// Pages of private memory that [`test_inherit`]'s copy has in each way
/// (see [`Inherited`]), before the range wiped for it.
const PAGES: usize = 6;
/// The private memory wiped for [`test_inherit`]'s copy: enough to see
/// what this process's writes there cost, and what unmapping it frees.
const WIPED_LEN: usize = 16 << 20;

/// Where [`test_inherit`]'s memory is.
#[derive(Clone, Copy)]
struct Inherited {
    /// Private memory: [`PAGES`] pages, each holding its number plus one,
    /// copied, kept from the copy, wiped for it, wiped no more, kept no
    /// more, and kept and wiped; then [`WIPED_LEN`] bytes of 7s, wiped.
    private: usize,
    /// A page mapped `SHARED`, its handle closed, kept from the copy.
    shared: usize,
    /// A page of private memory holding 9, then a page of a VMO holding
    /// 10, both of which a call refused to wipe.
    mixed: usize,
    /// Three pages of private memory, but for the second, which a call
    /// refused to keep from the copy: the others hold 13.
    holed: usize,
}

impl Inherited {
    fn page(&self, n: usize) -> usize {
        self.private + n * PAGE
    }

    fn wiped(&self) -> usize {
        self.page(PAGES)
    }
}

/// Whether nothing is mapped at `at`: protecting it finds nothing.
fn absent(at: usize) -> bool {
    call(nr::VM_PROTECT, [0, at, PAGE, map_flags::READ, 0, 0]) == Err(Error::NotFound)
}

/// What [`test_inherit`]'s copy does: says on `ch` what it found of its
/// memory, writes where it has zeros, then makes a copy of its own, which
/// says on `next` whether it has zeros there again; then waits on `ch` to
/// be told to end.
fn inheriting(ch: RawHandle, next: RawHandle, m: Inherited) -> ! {
    let mut found = 0;
    if peek(m.page(0)) == 1 && peek(m.page(3)) == 4 && peek(m.page(4)) == 5 && peek(m.mixed + PAGE) == 10 {
        found |= 1;
    }
    if absent(m.page(1)) && absent(m.page(5)) && absent(m.shared) {
        found |= 2;
    }
    if peek(m.page(2)) == 0 && holds(m.wiped(), WIPED_LEN, 0) {
        found |= 4;
    }
    if peek(m.mixed) == 9 && peek(m.holed) == 13 && peek(m.holed + 2 * PAGE) == 13 {
        found |= 8;
    }
    if !send(ch, found) {
        end(1);
    }
    poke(m.page(2), 11);
    poke(m.wiped(), 11);
    match runtime::process::fork(&[next]) {
        Ok(Some(copy)) => {
            if copy.0.wait(signals::TERMINATED, abi::DEADLINE_INFINITE).is_err() {
                end(2);
            }
        }
        Ok(None) => {
            let wiped = peek(m.page(2)) == 0 && peek(m.wiped()) == 0 && peek(m.page(0)) == 1 && absent(m.page(1));
            send(next, u32::from(wiped));
            end(0)
        }
        Err(_) => end(3),
    }
    end(if receive(ch) == Some(0) { 0 } else { 4 })
}

/// What a copy has of memory as `vm_inherit` says (see the module's
/// documentation), and what the call refuses.
pub fn test_inherit() -> TestResult {
    use abi::inherit_flags::{OMIT, ZERO};
    let rw = map_flags::READ | map_flags::WRITE;
    let private = vm::allocate(None, PAGES * PAGE + WIPED_LEN, 0, rw).map_err(s)?;
    let vmo = Vmo::create(PAGE).map_err(s)?;
    let shared = vm::map(None, &vmo, 0, PAGE, 0, rw | map_flags::SHARED).map_err(s)?;
    drop(vmo);
    let mixed = vm::allocate(None, 2 * PAGE, 0, rw).map_err(s)?;
    let holed = vm::allocate(None, 3 * PAGE, 0, rw).map_err(s)?;
    let m = Inherited { private, shared, mixed, holed };
    let result = (|| {
        for n in 0..PAGES {
            poke(m.page(n), n as u8 + 1);
        }
        fill(m.wiped(), WIPED_LEN, 7);
        poke(shared, 8);
        vm::unmap(None, mixed + PAGE, PAGE).map_err(s)?;
        let vmo = Vmo::create(PAGE).map_err(s)?;
        vm::map(None, &vmo, 0, PAGE, mixed + PAGE, rw | map_flags::FIXED).map_err(s)?;
        drop(vmo);
        poke(mixed, 9);
        poke(mixed + PAGE, 10);
        vm::unmap(None, holed + PAGE, PAGE).map_err(s)?;
        poke(holed, 13);
        poke(holed + 2 * PAGE, 13);
        let refused = [
            (vm::inherit(None, shared, PAGE, ZERO, 0), Error::NotSupported, "wiping memory that is not private"),
            (vm::inherit(None, mixed, 2 * PAGE, ZERO, 0), Error::NotSupported, "wiping a range not all private"),
            (vm::inherit(None, holed, 3 * PAGE, OMIT, 0), Error::NotFound, "a range not all mapped"),
            (vm::inherit(None, private, PAGE, 1 << 7, 0), Error::InvalidArgs, "an unknown flag"),
            (vm::inherit(None, private, PAGE, OMIT, OMIT), Error::InvalidArgs, "a flag both set and cleared"),
            (vm::inherit(None, private + 1, PAGE, OMIT, 0), Error::InvalidArgs, "a range from within a page"),
            (vm::inherit(None, private, 0, OMIT, 0), Error::InvalidArgs, "an empty range"),
        ];
        for (got, expected, what) in refused {
            check(got == Err(expected), &format!("{what} is refused: {got:?}"))?;
        }
        let changes = [
            (m.page(1), PAGE, OMIT, 0),
            (m.page(2), PAGE, ZERO, 0),
            (m.page(3), PAGE, ZERO, 0),
            (m.page(3), PAGE, 0, ZERO),
            (m.page(4), PAGE, OMIT, 0),
            (m.page(4), PAGE, 0, OMIT),
            (m.page(5), PAGE, OMIT | ZERO, 0),
            (m.wiped(), WIPED_LEN, ZERO, 0),
            (shared, PAGE, OMIT, 0),
        ];
        for (at, len, set, clear) in changes {
            vm::inherit(None, at, len, set, clear)
                .map_err(|e| format!("vm_inherit {at:#x}+{len:#x} {set}/{clear}: {e}"))?;
        }
        // A part protected anew is still wiped for the copy.
        let last = m.wiped() + WIPED_LEN - PAGE;
        vm::protect(None, last, PAGE, map_flags::READ).map_err(s)?;
        vm::protect(None, last, PAGE, rw).map_err(s)?;

        let (mine, theirs) = Channel::create().map_err(s)?;
        let (from_next, next) = Channel::create().map_err(s)?;
        let child = match runtime::process::fork(&[theirs.raw(), next.raw()]) {
            Ok(Some(child)) => child,
            Ok(None) => inheriting(theirs.raw(), next.raw(), m),
            Err(e) => return Err(format!("process_fork: {e}")),
        };
        // They are the copy's now.
        let _ = theirs.0.into_raw();
        let _ = next.0.into_raw();
        let outcome = (|| {
            let found = receive(mine.raw()).ok_or("the copy said nothing")?;
            check(found & 1 != 0, "the copy has a copy of the rest of its memory")?;
            check(found & 2 != 0, "the copy has nothing where memory was kept from it")?;
            check(found & 4 != 0, "the copy has zeros where memory was wiped for it")?;
            check(found & 8 != 0, "what the call refused changed nothing")?;
            check(receive(from_next.raw()) == Some(1), "memory wiped for the copy is wiped for its own copy too")?;
            let mine_still = (0..PAGES).all(|n| peek(m.page(n)) == n as u8 + 1);
            check(
                mine_still && holds(m.wiped(), WIPED_LEN, 7) && peek(shared) == 8,
                "this process's memory is as it was",
            )?;
            let before = left();
            fill(m.wiped(), WIPED_LEN, 12);
            let taken = before.saturating_sub(left());
            check(
                taken < WIPED_LEN / 2,
                &format!("writing memory wiped for the copy costs nothing ({} KiB taken)", taken >> 10),
            )?;
            // The copy holds none of it: unmapped here, it is free at once.
            let before = left();
            vm::unmap(None, m.wiped(), WIPED_LEN).map_err(s)?;
            let freed = left().saturating_sub(before);
            check(
                freed >= WIPED_LEN / 2,
                &format!("memory wiped for the copy is this process's alone ({} KiB freed)", freed >> 10),
            )?;
            check(send(mine.raw(), 0), "the copy told to end")?;
            let ended = child.0.wait(signals::TERMINATED, runtime::time::now_ns() + 20_000_000_000).map_err(s)?;
            let code = child.info().map_err(s)?.exit_code;
            check(ended & signals::TERMINATED != 0 && code == 0, &format!("the copy ended well (code {code})"))
        })();
        let _ = child.kill();
        outcome
    })();
    // What is left of it.
    vm::unmap(None, private, PAGES * PAGE + WIPED_LEN).map_err(s)?;
    vm::unmap(None, shared, PAGE).map_err(s)?;
    vm::unmap(None, mixed, 2 * PAGE).map_err(s)?;
    vm::unmap(None, holed, 3 * PAGE).map_err(s)?;
    result
}

/// How many copies [`test_burst`] has alive at once, as CPython's tests of
/// `multiprocessing` start.
const BURST: usize = 100;
/// The private memory they are copies of: each copy's map of its pages
/// takes the kernel some 1.5 MiB, so that a hundred take more than any
/// test before.
const BURST_LEN: usize = 128 << 20;

/// A burst of copies, ended at once: what the kernel took for them (each
/// one's map of the pages it shares, in the kernel's heap) is free again
/// once they are gone, not the heap's for good. A quarter of it (16 MiB at
/// least) may stay taken, by what the system's other programs take
/// meanwhile (the driver VM's Linux may still be starting).
pub fn test_burst() -> TestResult {
    let rw = map_flags::READ | map_flags::WRITE;
    let private = vm::allocate(None, BURST_LEN, 0, rw).map_err(s)?;
    fill(private, BURST_LEN, 1);
    let before = left();
    let mut copies = Vec::with_capacity(BURST);
    let made = (0..BURST).try_for_each(|_| match runtime::process::fork(&[]) {
        Ok(Some(copy)) => {
            copies.push(copy);
            Ok(())
        }
        // The copy waits to be ended.
        Ok(None) => loop {
            runtime::time::sleep_until(abi::DEADLINE_INFINITE);
        },
        Err(e) => Err(format!("process_fork: {e}")),
    });
    let taken = before.saturating_sub(left());
    for copy in &copies {
        let _ = copy.kill();
    }
    let deadline = runtime::time::now_ns() + 20_000_000_000;
    let ended = copies.iter().all(|c| c.0.wait(signals::TERMINATED, deadline).is_ok());
    drop(copies);
    let result = made.and_then(|()| {
        check(ended, "the copies ended")?;
        // Gone, a process's memory may take a moment to be free: a
        // processor may hold its address space until it runs another.
        let stayed = || before.saturating_sub(left());
        let slack = (taken / 4).max(16 << 20);
        let back = (0..200).any(|_| {
            if stayed() <= slack {
                return true;
            }
            runtime::time::sleep(runtime::time::Duration::from_millis(100));
            false
        });
        let what = format!(
            "{BURST} copies took {} MiB, of which {} MiB stayed taken once they were gone",
            taken >> 20,
            stayed() >> 20
        );
        check(back, &what)?;
        println!("a burst of copies: {what}");
        Ok(())
    });
    vm::unmap(None, private, BURST_LEN).map_err(s)?;
    result
}

/// What a process gave up, its copies have given up: a copy of one that
/// gave up executable memory makes none.
fn test_restrictions_inherited() -> TestResult {
    let (mine, theirs) = Channel::create().map_err(s)?;
    let child = match runtime::process::fork(&[theirs.raw()]) {
        Ok(Some(child)) => child,
        Ok(None) => {
            let ch = theirs.raw();
            if runtime::process::restrict(abi::restrict::NO_EXECUTE).is_err() {
                end(1);
            }
            match runtime::process::fork(&[ch]) {
                Ok(Some(grandchild)) => {
                    let ended = grandchild.0.wait(signals::TERMINATED, abi::DEADLINE_INFINITE);
                    end(if ended.is_ok() { 0 } else { 2 })
                }
                Ok(None) => {
                    let executable = map_flags::READ | map_flags::EXECUTE;
                    let refused = vm::allocate(None, PAGE, 0, executable) == Err(Error::AccessDenied);
                    send(ch, u32::from(refused));
                    end(0)
                }
                Err(_) => end(3),
            }
        }
        Err(e) => return Err(format!("process_fork: {e}")),
    };
    let _ = theirs.0.into_raw();
    let refused = receive(mine.raw());
    let ended = child.0.wait(signals::TERMINATED, runtime::time::now_ns() + 20_000_000_000);
    let code = child.info().map_or(-1, |i| i.exit_code);
    let _ = child.kill();
    check(refused == Some(1), "a copy of a copy of a process that gave executable memory up makes none")?;
    check(ended.is_ok() && code == 0, &format!("the copy that forked again ended well (code {code})"))
}

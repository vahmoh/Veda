// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Foreign code (`foreign_run`): a system call, an invalid instruction and
//! a page fault each come back to this program with the registers the
//! code left, its thread pointer in effect while it runs and this
//! program's again after; registers user code cannot have are refused;
//! and an interruption ends it as it ends a wait, deferred as the signal
//! entry defers it ([`interrupted`], in a copy with a signal entry).

use alloc::format;
use alloc::string::{String, ToString};

use abi::{Error, ForeignState, foreign_exit, map_flags, nr, wait_flags};
use runtime::foreign;
use runtime::sys::call;
use runtime::vm;

use crate::architecture::foreign::*;
use crate::{TestResult, check};

const PAGE: usize = abi::PAGE_SIZE;

fn s<E: core::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// The thread-local storage foreign code is given, and this program's own
/// for the test: what [`READ_TLS`] and [`tls_word`] read, 8 bytes in.
static FOREIGN_TLS: [u64; 2] = [0, 0x5eda_f0e1_9e70_0001];
static HOST_TLS: [u64; 2] = [0, 0x5eda_0005_7000_0002];

/// A page of code holding each snippet 64 bytes apart, executable, and a
/// page of stack: their addresses.
struct Code {
    at: usize,
    stack: usize,
}

const SNIPPETS: [&[u8]; 5] = [SYSCALL, READ_TLS, INVALID, FAULT, SPIN];

impl Code {
    fn new() -> Result<Code, String> {
        let at = vm::allocate(None, PAGE, 0, map_flags::READ | map_flags::WRITE).map_err(s)?;
        for (i, code) in SNIPPETS.iter().enumerate() {
            // SAFETY: our fresh page, 64 bytes for each snippet.
            unsafe { core::ptr::copy_nonoverlapping(code.as_ptr(), (at + 64 * i) as *mut u8, code.len()) };
        }
        vm::protect(None, at, PAGE, map_flags::READ | map_flags::EXECUTE).map_err(s)?;
        sync_code(at, PAGE);
        let stack = vm::allocate(None, PAGE, 0, map_flags::READ | map_flags::WRITE).map_err(s)?;
        Ok(Code { at, stack })
    }

    /// Where snippet `code` is.
    fn of(&self, code: &[u8]) -> usize {
        let i = SNIPPETS.iter().position(|c| c.as_ptr() == code.as_ptr()).unwrap_or(0);
        self.at + 64 * i
    }

    /// Foreign code that starts at snippet `code`.
    fn start(&self, code: &[u8]) -> ForeignState {
        start(self.of(code), self.stack + PAGE - 64, FOREIGN_TLS.as_ptr() as usize, KEPT)
    }
}

impl Drop for Code {
    fn drop(&mut self) {
        let _ = vm::unmap(None, self.at, PAGE);
        let _ = vm::unmap(None, self.stack, PAGE);
    }
}

/// What a register foreign code keeps holds as it starts.
const KEPT: u64 = 0x0123_4567_89ab_cdef;

fn set_tls(at: usize) -> TestResult {
    call(nr::THREAD_SET_TLS, [at, 0, 0, 0, 0, 0]).map(|_| ()).map_err(s)
}

pub fn test_foreign_code() -> TestResult {
    let code = Code::new()?;
    set_tls(HOST_TLS.as_ptr() as usize)?;
    let outcome = foreign_code(&code);
    set_tls(0)?;
    outcome?;
    let copy = crate::posix::start_copy_with(&["foreign"])?;
    let ended = copy.join(runtime::time::deadline_after(runtime::time::Duration::from_secs(20))).map_err(s)?;
    check(ended == 0, &format!("the copy that is interrupted ended with {ended} (see the log)"))
}

fn foreign_code(code: &Code) -> TestResult {
    // A system call: the code resumes after it, its registers as it left
    // them, and this thread's own thread pointer is in effect again.
    let mut state = code.start(SYSCALL);
    check(foreign::run(&mut state, 0) == Ok(foreign_exit::SYSCALL), "a system call ends foreign code")?;
    check(number(&state) == 39, &format!("the system call's number is {}", number(&state)))?;
    let after = code.of(SYSCALL) + SYSCALL.len();
    check(pc(&state) == after, &format!("foreign code stopped at {:#x}, not {after:#x}", pc(&state)))?;
    check(kept(&state) == KEPT, "a register the system call keeps changed")?;
    check(tls(&state) == FOREIGN_TLS.as_ptr() as usize, "foreign code's thread pointer changed")?;
    check(tls_word() == HOST_TLS[1], "this thread's own thread pointer is not in effect after foreign code")?;
    // The thread pointer foreign code runs with is its own.
    let mut state = code.start(READ_TLS);
    check(foreign::run(&mut state, 0) == Ok(foreign_exit::SYSCALL), "foreign code reads its thread-local storage")?;
    check(loaded(&state) == FOREIGN_TLS[1], "foreign code's thread pointer is not in effect")?;
    check(tls_word() == HOST_TLS[1], "this thread's own thread pointer is not in effect again")?;
    // Exceptions, where they happened.
    let mut state = code.start(INVALID);
    check(foreign::run(&mut state, 0) == Ok(foreign_exit::EXCEPTION), "an invalid instruction ends foreign code")?;
    check(invalid_instruction(&state) && pc(&state) == code.of(INVALID), "the invalid instruction and where")?;
    let mut state = code.start(FAULT);
    check(foreign::run(&mut state, 0) == Ok(foreign_exit::EXCEPTION), "a page fault ends foreign code")?;
    check(page_fault(&state) == Some(0x1000), &format!("the page fault is at {:x?}", page_fault(&state)))?;
    check(pc(&state) == code.of(FAULT) + FAULT_AT, "where the page fault happened")?;
    // Registers user code cannot have, and flags there are not.
    let mut state = start(0, code.stack + PAGE - 64, FOREIGN_TLS.as_ptr() as usize, KEPT);
    check(foreign::run(&mut state, 0) == Err(Error::InvalidArgs), "foreign code at address 0 is refused")?;
    // A thread pointer is what user code can have, any the processor
    // takes (on AMD64 a canonical address, as `wrfsbase` sets one), kept
    // as it was.
    let far_tls = 0xffff_8000_0000_0000;
    let mut state = start(code.of(SYSCALL), code.stack + PAGE - 64, far_tls, KEPT);
    let ran = foreign::run(&mut state, 0) == Ok(foreign_exit::SYSCALL) && tls(&state) == far_tls;
    check(ran, "a thread pointer outside user space is foreign code's own")?;
    #[cfg(target_arch = "x86_64")]
    {
        let mut state = start(code.of(SYSCALL), code.stack + PAGE - 64, 0x8000_0000_0000_0000, KEPT);
        let refused = foreign::run(&mut state, 0) == Err(Error::InvalidArgs);
        check(refused, "a thread pointer the processor cannot take is refused")?;
    }
    let mut state = code.start(SYSCALL);
    check(foreign::run(&mut state, 1 << 3) == Err(Error::InvalidArgs), "flags there are not are refused")?;
    Ok(())
}

/// `systest foreign`: foreign code that loops is interrupted by another
/// thread; with a signal entry that defers interruptions, as the POSIX
/// layer's does inside the layer, a deferred one ends the next run at once
/// until the signals are taken.
pub fn interrupted() -> i32 {
    match try_interrupted() {
        Ok(()) => 0,
        Err(e) => {
            runtime::println!("foreign: {e}");
            1
        }
    }
}

fn try_interrupted() -> TestResult {
    let code = Code::new()?;
    // Interruptions are for threads with a thread pointer, in a process
    // with a signal entry.
    set_tls(HOST_TLS.as_ptr() as usize)?;
    call(nr::SIGNAL_ENTRY_SET, [defer_entry(), 0, 0, 0, 0, 0]).map_err(s)?;
    let me = call(nr::THREAD_SELF, [0; 6]).map_err(s)?;
    let interrupter = runtime::thread::spawn(move || {
        runtime::time::sleep(runtime::time::Duration::from_millis(50));
        call(nr::THREAD_INTERRUPT, [me, 0, 0, 0, 0, 0])
    });
    let mut state = code.start(SPIN);
    let ran = foreign::run(&mut state, 0);
    let interrupted = interrupter.join().map_err(|_| "the interrupting thread failed".to_string())?;
    interrupted.map_err(|e| format!("interrupting: {e}"))?;
    check(ran == Err(Error::Interrupted), &format!("looping foreign code ended with {ran:?}"))?;
    check(pc(&state) == code.of(SPIN), "the interrupted code's registers")?;
    check(tls_word() == HOST_TLS[1], "this thread's own thread pointer after the interruption")?;
    // The signal entry deferred it: until the signals are taken, a run
    // that a deferred interruption ends ends at once.
    let mut state = code.start(SYSCALL);
    let deferred = foreign::run(&mut state, wait_flags::DEFERRED);
    check(deferred == Err(Error::Interrupted), &format!("a deferred interruption ended nothing: {deferred:?}"))?;
    check(pc(&state) == code.of(SYSCALL), "foreign code ran though a deferred interruption ends the run")?;
    check(foreign::run(&mut state, 0) == Ok(foreign_exit::SYSCALL), "a run that waits for no deferral")?;
    let mut sent = abi::SentSignals::default();
    call(nr::SIGNAL_TAKE, [&raw mut sent as usize, 0, 0, 0, 0, 0]).map_err(s)?;
    let mut state = code.start(SYSCALL);
    let ran = foreign::run(&mut state, wait_flags::DEFERRED);
    check(ran == Ok(foreign_exit::SYSCALL), &format!("once the signals are taken, the run ended with {ran:?}"))
}

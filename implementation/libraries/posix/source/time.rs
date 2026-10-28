// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Clocks and sleeping.

use core::fmt::Write;

use crate::error::SysResult;
use crate::linux::errno::{EFAULT, EINTR, EINVAL};
use crate::linux::{Timespec, Timeval, Tms, clock};
use crate::user;

/// Nanoseconds since boot (never goes backwards).
pub fn monotonic_ns() -> u64 {
    runtime::time::now_ns()
}

/// Nanoseconds since 1970-01-01 00:00:00 UTC.
pub fn realtime_ns() -> u64 {
    runtime::sys::clock_get(abi::clock::UTC)
}

/// The offset of the system's local time from UTC, in minutes east: what
/// the kernel keeps between its two real-time clocks (`tz=` at boot, or
/// the firmware's zone), with no rules of daylight saving.
pub fn utc_offset_minutes() -> i64 {
    let local = runtime::sys::clock_get(abi::clock::REALTIME) as i128;
    let utc = runtime::sys::clock_get(abi::clock::UTC) as i128;
    // The clocks are read a moment apart: to the nearest minute.
    const MINUTE: i128 = 60_000_000_000;
    let d = local - utc;
    (if d >= 0 { (d + MINUTE / 2) / MINUTE } else { (d - MINUTE / 2) / MINUTE }) as i64
}

/// The offset of the system's local time from UTC, in nanoseconds east (see
/// [`utc_offset_minutes`]).
pub fn utc_offset_ns() -> i64 {
    utc_offset_minutes() * 60_000_000_000
}

/// The system's time zone as a POSIX TZ string (`<+02>-2`, `<-0330>3:30`,
/// `UTC0`): its offset from UTC, named as the tz database names a zone of
/// that offset alone. The sign is POSIX's: hours west of UTC.
fn posix_tz(offset_minutes: i64) -> TzString {
    let mut out = TzString::default();
    let (h, m) = (offset_minutes.unsigned_abs() / 60, offset_minutes.unsigned_abs() % 60);
    let (east, west) = if offset_minutes > 0 { ('+', '-') } else { ('-', '+') };
    let _ = match (offset_minutes, m) {
        (0, _) => out.write_str("UTC0"),
        (_, 0) => write!(out, "<{east}{h:02}>{west}{h}"),
        _ => write!(out, "<{east}{h:02}{m:02}>{west}{h}:{m:02}"),
    };
    out
}

/// A POSIX TZ string, on the stack (the longest is `<+1445>-14:45`).
#[derive(Default)]
struct TzString {
    buf: [u8; 24],
    len: usize,
}

impl TzString {
    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl Write for TzString {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let room = self.buf.len() - self.len;
        let n = s.len().min(room);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        if n < s.len() { Err(core::fmt::Error) } else { Ok(()) }
    }
}

/// The system's time zone, for musl when the environment has no `TZ`
/// (there is no `/etc/localtime`): a POSIX TZ string of its offset from
/// UTC, NUL-terminated in `buf` (cut short to fit).
///
/// # Safety
/// `buf` must be valid for writes of `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __veda_tz(buf: *mut u8, size: usize) {
    let _layer = crate::interrupt::Layer::enter();
    if size == 0 {
        return;
    }
    let tz = posix_tz(utc_offset_minutes());
    let s = tz.as_bytes();
    let n = s.len().min(size - 1);
    // SAFETY: the caller gave `size` bytes; `n + 1 <= size`.
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), buf, n);
        *buf.add(n) = 0;
    }
}

/// CPU time this process has used.
fn cpu_ns() -> u64 {
    runtime::object::process_self_info().map_or(0, |i| i.cpu_time_ns)
}

/// CPU time thread `tid` of this process has used (`None`: no such
/// thread). Before the main thread is known, it is the only one: its time
/// is the process's.
fn thread_cpu_ns(tid: i32) -> Option<u64> {
    crate::thread::cpu_time_ns(tid).or_else(|| (tid == crate::signal::current_tid()).then(cpu_ns))
}

/// The CPU time a clock of `clock_getcpuclockid` or `pthread_getcpuclockid`
/// measures, by its id, which Linux makes of the process's or thread's id
/// (0: the caller's), inverted, above three bits: whether a thread's, and
/// what time. User and system time together (`CPUCLOCK_PROF`) and the time
/// run (`CPUCLOCK_SCHED`) are one here; user time alone (`CPUCLOCK_VIRT`)
/// is not kept. This process and its threads have them.
fn cpu_clock_ns(clk: u32) -> Result<u64, isize> {
    const PER_THREAD: i32 = 4;
    const PROF: i32 = 0;
    const SCHED: i32 = 2;
    let clk = clk as i32;
    let id = !(clk >> 3);
    if !matches!(clk & 3, PROF | SCHED) {
        return Err(EINVAL);
    }
    if clk & PER_THREAD != 0 {
        thread_cpu_ns(if id == 0 { crate::signal::current_tid() } else { id }).ok_or(EINVAL)
    } else if id == 0 || id == crate::process::pid() {
        Ok(cpu_ns())
    } else {
        Err(EINVAL)
    }
}

fn now(clk: u32) -> Result<u64, isize> {
    Ok(match clk {
        clock::REALTIME | clock::REALTIME_COARSE => realtime_ns(),
        clock::MONOTONIC | clock::MONOTONIC_RAW | clock::MONOTONIC_COARSE | clock::BOOTTIME => monotonic_ns(),
        clock::PROCESS_CPUTIME_ID => cpu_ns(),
        clock::THREAD_CPUTIME_ID => thread_cpu_ns(crate::signal::current_tid()).ok_or(EINVAL)?,
        // The clocks of processes and threads (`clock_getcpuclockid`).
        c if (c as i32) < 0 => cpu_clock_ns(c)?,
        _ => return Err(EINVAL),
    })
}

pub unsafe fn clock_gettime(clk: u32, ts: usize) -> SysResult {
    let t = Timespec::from_ns(now(clk)?);
    // SAFETY: the program passed a timespec.
    unsafe { user::write(ts, t)? };
    Ok(0)
}

pub unsafe fn clock_getres(clk: u32, ts: usize) -> SysResult {
    now(clk)?;
    if ts != 0 {
        // SAFETY: as above.
        unsafe { user::write(ts, Timespec { sec: 0, nsec: 1 })? };
    }
    Ok(0)
}

pub unsafe fn gettimeofday(tv: usize) -> SysResult {
    if tv != 0 {
        let ns = realtime_ns();
        // SAFETY: the program passed a timeval.
        unsafe {
            user::write(tv, Timeval { sec: (ns / 1_000_000_000) as i64, usec: ((ns % 1_000_000_000) / 1000) as i64 })?
        };
    }
    Ok(0)
}

pub unsafe fn time(out: usize) -> SysResult {
    let s = (realtime_ns() / 1_000_000_000) as i64;
    if out != 0 {
        // SAFETY: the program passed a time_t.
        unsafe { user::write(out, s)? };
    }
    Ok(s as usize)
}

/// `clock_nanosleep` (and `nanosleep`, which sleeps on the monotonic
/// clock): until the time comes, or a signal is to be acted on first
/// (`EINTR`, never started again, as on Linux; what is left of a relative
/// sleep in `rem` if it is not null).
pub unsafe fn clock_nanosleep(clk: u32, flags: u32, req: usize, rem: usize) -> SysResult {
    if req == 0 {
        return Err(EFAULT);
    }
    // SAFETY: the program passed a timespec.
    let t: Timespec = unsafe { user::read(req)? };
    let ns = t.to_ns().ok_or(EINVAL)?;
    let current = now(clk)?;
    let absolute = flags & clock::TIMER_ABSTIME != 0;
    let wait = if absolute { ns.saturating_sub(current) } else { ns };
    let deadline = monotonic_ns().saturating_add(wait);
    if !crate::signal::sleep_until(deadline) {
        return Ok(0);
    }
    if !absolute && rem != 0 {
        let left = Timespec::from_ns(deadline.saturating_sub(monotonic_ns()));
        // SAFETY: the program passed a timespec.
        unsafe { user::write(rem, left)? };
    }
    Err(EINTR)
}

/// `times`: the process's CPU time in clock ticks (100 a second).
pub unsafe fn times(buf: usize) -> SysResult {
    if buf != 0 {
        let ticks = (cpu_ns() / 10_000_000) as i64;
        // SAFETY: the program passed a struct tms.
        unsafe { user::write(buf, Tms { utime: ticks, ..Default::default() })? };
    }
    Ok((monotonic_ns() / 10_000_000) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_zones_are_posix_strings_of_their_offsets() {
        assert_eq!(posix_tz(0).as_bytes(), b"UTC0");
        assert_eq!(posix_tz(120).as_bytes(), b"<+02>-2");
        assert_eq!(posix_tz(-300).as_bytes(), b"<-05>+5");
        assert_eq!(posix_tz(330).as_bytes(), b"<+0530>-5:30");
        assert_eq!(posix_tz(-210).as_bytes(), b"<-0330>+3:30");
        assert_eq!(posix_tz(14 * 60 + 45).as_bytes(), b"<+1445>-14:45");
    }
}

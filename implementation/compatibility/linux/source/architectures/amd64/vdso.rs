// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The vDSO's code on x86-64 (`crate::vdso`): the functions of Linux's
//! x86-64 vDSO, of version `LINUX_2.6`, which read the clocks as
//! `runtime::time` reads them, from the TSC (read once every earlier load
//! has completed, so that a time read after another thread's that this one
//! has seen is never earlier) and the scale on the data page below the
//! image, and make the system call for what they cannot read.
//!
//! Each follows the System V ABI, and touches none of the registers its
//! caller keeps, nor the vector registers. Their frames are the calls'
//! alone: the stack pointer moves only for a call of their own helper, so
//! one rule finds the caller at every instruction ([`FRAME`]).

use core::mem::offset_of;

use abi::ClockInfo;
use linux_raw_sys::general::{__NR_clock_getres, __NR_clock_gettime, __NR_getcpu, __NR_gettimeofday, __NR_time};

use crate::vdso::{Code, Frame, Function};

/// `(ns >> 9) * PER_SECOND >> 75` is `ns / 10⁹` for every `ns` (`10⁹` is
/// `2⁹ * 1953125`, and the multiplier is `2⁷⁵ / 1953125` rounded up).
const PER_SECOND: u64 = (1u128 << 75).div_ceil(1_953_125) as u64;
/// `ns * PER_MICROSECOND >> 38` is `ns / 1000` for nanoseconds of a second.
const PER_MICROSECOND: u64 = (1u64 << 38).div_ceil(1000);

core::arch::global_asm!(
    r#"
    .pushsection .rodata.linux_compatibility_vdso, "a", @progbits

    // The code's size, then each function's offset in it and size, in
    // `FUNCTIONS`' order.
    .p2align 2
    .globl linux_compatibility_vdso_table
    .hidden linux_compatibility_vdso_table
linux_compatibility_vdso_table:
    .long .Lend - .Ltext
    .long .Lclock_gettime - .Ltext, .Lclock_gettime_end - .Lclock_gettime
    .long .Lgettimeofday - .Ltext, .Lgettimeofday_end - .Lgettimeofday
    .long .Ltime - .Ltext, .Ltime_end - .Ltime
    .long .Lclock_getres - .Ltext, .Lclock_getres_end - .Lclock_getres
    .long .Lgetcpu - .Ltext, .Lgetcpu_end - .Lgetcpu

    // The code, which runs where the image puts it: `{text}` bytes into the
    // image, the data page right below the image; between its functions,
    // `int3`, which stops a stray jump there.
    .p2align 4
    .globl linux_compatibility_vdso_text
    .hidden linux_compatibility_vdso_text
linux_compatibility_vdso_text:
.Ltext:
    .set .Ldata, .Ltext - {text} - {data}

    // int __vdso_clock_gettime(clockid_t clock, struct timespec *ts)
.Lclock_gettime:
    cmp edi, 31
    ja .Lclock_gettime_call
    mov eax, {clocks}
    bt eax, edi
    jnc .Lclock_gettime_call
    mov rcx, qword ptr [rip + .Ldata + {scale}]
    test rcx, rcx
    jz .Lclock_gettime_call
    xor r8d, r8d
    mov eax, {utc_clocks}
    bt eax, edi
    cmovc r8, qword ptr [rip + .Ldata + {utc_at_zero}]
    call .Lnow
    mov rcx, rax
    shr rax, 9
    mov rdx, {per_second}
    mul rdx
    shr rdx, 11
    mov qword ptr [rsi], rdx
    imul rdx, rdx, 1000000000
    sub rcx, rdx
    mov qword ptr [rsi + 8], rcx
    xor eax, eax
    ret
.Lclock_gettime_call:
    mov eax, {nr_clock_gettime}
    syscall
    ret
.Lclock_gettime_end:

    // int __vdso_gettimeofday(struct timeval *tv, struct timezone *tz): a
    // time zone is the system call's to give.
    .p2align 4, 0xcc
.Lgettimeofday:
    test rsi, rsi
    jnz .Lgettimeofday_call
    test rdi, rdi
    jz .Lgettimeofday_done
    mov rcx, qword ptr [rip + .Ldata + {scale}]
    test rcx, rcx
    jz .Lgettimeofday_call
    mov r8, qword ptr [rip + .Ldata + {utc_at_zero}]
    call .Lnow
    mov rcx, rax
    shr rax, 9
    mov rdx, {per_second}
    mul rdx
    shr rdx, 11
    mov qword ptr [rdi], rdx
    imul rdx, rdx, 1000000000
    sub rcx, rdx
    imul rcx, rcx, {per_microsecond}
    shr rcx, 38
    mov qword ptr [rdi + 8], rcx
.Lgettimeofday_done:
    xor eax, eax
    ret
.Lgettimeofday_call:
    mov eax, {nr_gettimeofday}
    syscall
    ret
.Lgettimeofday_end:

    // time_t __vdso_time(time_t *t)
    .p2align 4, 0xcc
.Ltime:
    mov rcx, qword ptr [rip + .Ldata + {scale}]
    test rcx, rcx
    jz .Ltime_call
    mov r8, qword ptr [rip + .Ldata + {utc_at_zero}]
    call .Lnow
    shr rax, 9
    mov rdx, {per_second}
    mul rdx
    shr rdx, 11
    mov rax, rdx
    test rdi, rdi
    jz .Ltime_done
    mov qword ptr [rdi], rax
.Ltime_done:
    ret
.Ltime_call:
    mov eax, {nr_time}
    syscall
    ret
.Ltime_end:

    // int __vdso_clock_getres(clockid_t clock, struct timespec *res)
    .p2align 4, 0xcc
.Lclock_getres:
    cmp edi, 31
    ja .Lclock_getres_call
    mov eax, {clocks}
    bt eax, edi
    jnc .Lclock_getres_call
    test rsi, rsi
    jz .Lclock_getres_done
    mov qword ptr [rsi], {resolution_s}
    mov qword ptr [rsi + 8], {resolution_ns}
.Lclock_getres_done:
    xor eax, eax
    ret
.Lclock_getres_call:
    mov eax, {nr_clock_getres}
    syscall
    ret
.Lclock_getres_end:

    // int __vdso_getcpu(unsigned *cpu, unsigned *node, void *cache): the
    // processor's number is the kernel's, which keeps it nowhere user code
    // reads.
    .p2align 4, 0xcc
.Lgetcpu:
    mov eax, {nr_getcpu}
    syscall
    ret
.Lgetcpu_end:

    // rax: a clock that is the monotonic one plus r8 (the real time at
    // boot, or 0), in nanoseconds, as `runtime::time` reads it: the TSC,
    // read once every earlier load has completed, less its reading at boot
    // (0 before it), by the scale in rcx (nanoseconds per tick, 32.32), the
    // product's high bits dropped and the sum saturated as the kernel's
    // are. Clobbers rdx.
    .p2align 4, 0xcc
.Lnow:
    lfence
    rdtsc
    shl rdx, 32
    or rax, rdx
    sub rax, qword ptr [rip + .Ldata + {ticks_at_zero}]
    sbb rdx, rdx
    not rdx
    and rax, rdx
    mul rcx
    shrd rax, rdx, 32
    add rax, r8
    sbb rdx, rdx
    or rax, rdx
    ret
.Lend:

    .popsection
    "#,
    text = const crate::vdso::TEXT,
    data = const crate::vdso::DATA,
    clocks = const crate::vdso::UTC_CLOCKS | crate::vdso::MONOTONIC_CLOCKS,
    utc_clocks = const crate::vdso::UTC_CLOCKS,
    scale = const offset_of!(ClockInfo, ns_per_tick),
    ticks_at_zero = const offset_of!(ClockInfo, ticks_at_zero),
    utc_at_zero = const offset_of!(ClockInfo, utc_at_zero),
    per_second = const PER_SECOND,
    per_microsecond = const PER_MICROSECOND,
    resolution_s = const posix::host::RESOLUTION_NS / 1_000_000_000,
    resolution_ns = const posix::host::RESOLUTION_NS % 1_000_000_000,
    nr_clock_gettime = const __NR_clock_gettime,
    nr_gettimeofday = const __NR_gettimeofday,
    nr_time = const __NR_time,
    nr_clock_getres = const __NR_clock_getres,
    nr_getcpu = const __NR_getcpu,
);

unsafe extern "C" {
    /// The table the assembly above lays out.
    static linux_compatibility_vdso_table: [u32; 1 + 2 * FUNCTIONS.len()];
    /// The code's first byte.
    static linux_compatibility_vdso_text: u8;
}

/// The functions, in the table's order: each by its name, and by Linux's
/// other (weak) name for it.
const FUNCTIONS: [(&str, &str); 5] = [
    ("__vdso_clock_gettime", "clock_gettime"),
    ("__vdso_gettimeofday", "gettimeofday"),
    ("__vdso_time", "time"),
    ("__vdso_clock_getres", "clock_getres"),
    ("__vdso_getcpu", "getcpu"),
];

/// Where a function's caller is, at any of its instructions: the return
/// address (`rip`, DWARF's register 16) on top of the stack, and the
/// caller's stack pointer just above it (`rsp`, register 7): DWARF's
/// `DW_CFA_def_cfa rsp, 8` and `DW_CFA_offset rip, 1` (one word of the
/// data alignment, -8, from the frame's address).
const FRAME: Frame =
    Frame { return_address: 16, data_alignment: -8, signal: false, instructions: &[0x0c, 7, 8, 0x80 | 16, 1] };

/// The vDSO's code, as the image has it.
pub fn code() -> Option<Code> {
    // SAFETY: the table and the code the assembly lays out, read-only data
    // of this program's own; the code is as long as the table's first
    // word says.
    let (table, text) = unsafe {
        let table = &linux_compatibility_vdso_table;
        (table, core::slice::from_raw_parts(&raw const linux_compatibility_vdso_text, table[0] as usize))
    };
    let functions = FUNCTIONS
        .iter()
        .enumerate()
        .map(|(i, &(name, alias))| Function {
            name,
            alias: Some(alias),
            offset: table[1 + 2 * i] as usize,
            size: table[2 + 2 * i] as usize,
        })
        .collect();
    // One rule for all of it; and no restorer, which every handler on
    // x86-64 has of its own (`SA_RESTORER`).
    Some(Code { text, functions, version: "LINUX_2.6", frames: alloc::vec![(0..text.len(), FRAME)], restorer: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Numbers the divisions are checked at: edges and a spread between.
    fn numbers() -> impl Iterator<Item = u64> {
        let edges = [0, 1, 999, 1000, 999_999_999, 1_000_000_000, 1_000_000_001, u64::MAX - 1, u64::MAX];
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let spread = core::iter::from_fn(move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            Some(x)
        });
        edges
            .into_iter()
            .chain((1..=19).flat_map(|e| {
                let p = 10u64.pow(e);
                [p - 1, p, p + 1]
            }))
            .chain(spread.take(100_000))
    }

    #[test]
    fn seconds_are_a_multiplication_away() {
        for ns in numbers() {
            let seconds = (((ns >> 9) as u128 * PER_SECOND as u128) >> 75) as u64;
            assert_eq!(seconds, ns / 1_000_000_000, "{ns}");
            let into = ns % 1_000_000_000;
            assert_eq!((into * PER_MICROSECOND) >> 38, into / 1000, "{into}");
        }
    }

    /// The code run where it runs in a program, on the host (Linux), whose
    /// kernel answers the system calls it makes as Linux compatibility
    /// would.
    #[cfg(target_os = "linux")]
    mod running {
        use std::vec::Vec;

        use linux_raw_sys::general::*;

        use super::*;
        use crate::vdso::{DATA, MONOTONIC_CLOCKS, TEXT, UTC_CLOCKS};

        #[repr(C)]
        #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        struct Timespec {
            sec: i64,
            nsec: i64,
        }

        fn split(ns: u64) -> Timespec {
            Timespec { sec: (ns / 1_000_000_000) as i64, nsec: (ns % 1_000_000_000) as i64 }
        }

        fn nanoseconds(t: Timespec) -> u64 {
            t.sec as u64 * 1_000_000_000 + t.nsec as u64
        }

        /// A system call of the host's.
        fn host(n: u32, a: [usize; 6]) -> isize {
            let r: isize;
            // SAFETY: the tests' calls, of their own memory.
            unsafe {
                core::arch::asm!(
                    "syscall",
                    inlateout("rax") n as isize => r,
                    in("rdi") a[0], in("rsi") a[1], in("rdx") a[2], in("r10") a[3], in("r8") a[4], in("r9") a[5],
                    lateout("rcx") _, lateout("r11") _,
                    options(nostack),
                );
            }
            r
        }

        /// The host's time of clock `clk`.
        fn host_clock(clk: u32) -> Timespec {
            let mut t = Timespec::default();
            assert_eq!(host(__NR_clock_gettime, [clk as usize, &raw mut t as usize, 0, 0, 0, 0]), 0);
            t
        }

        /// The counter, as the code reads it.
        fn ticks() -> u64 {
            // SAFETY: LFENCE and RDTSC, which every x86-64 processor has.
            unsafe {
                core::arch::x86_64::_mm_lfence();
                core::arch::x86_64::_rdtsc()
            }
        }

        /// The image and its data page, mapped as the loader maps them.
        struct Vdso {
            base: usize,
            len: usize,
            functions: Vec<Function>,
        }

        impl Vdso {
            fn map() -> Vdso {
                let code = code().unwrap();
                let image = crate::vdso::build(&code);
                let len = DATA + image.len().next_multiple_of(abi::PAGE_SIZE);
                let (rw, private) = ((PROT_READ | PROT_WRITE) as usize, (MAP_PRIVATE | MAP_ANONYMOUS) as usize);
                let base = host(__NR_mmap, [0, len, rw, private, usize::MAX, 0]);
                assert!(base > 0, "mmap: {base}");
                let base = base as usize;
                // SAFETY: memory just mapped, which the image fits above the
                // data page.
                unsafe { core::ptr::copy_nonoverlapping(image.as_ptr(), (base + DATA) as *mut u8, image.len()) };
                let rx = (PROT_READ | PROT_EXEC) as usize;
                assert_eq!(host(__NR_mprotect, [base + DATA, len - DATA, rx, 0, 0, 0]), 0);
                Vdso { base, len, functions: code.functions }
            }

            /// Puts the scale on the data page.
            fn scale(&self, scale: Option<ClockInfo>) {
                let page = crate::vdso::data(scale);
                // SAFETY: the data page, the mapping's first, writable.
                unsafe { core::ptr::copy_nonoverlapping(page.as_ptr(), self.base as *mut u8, page.len()) };
            }

            /// Function `name`, as a function of type `F`.
            fn function<F: Copy>(&self, name: &str) -> F {
                let f = self.functions.iter().find(|f| f.name == name).unwrap();
                let at = self.base + DATA + TEXT + f.offset;
                assert_eq!(core::mem::size_of::<F>(), core::mem::size_of::<usize>());
                // SAFETY: `F` is the function's type, as Linux declares it.
                unsafe { core::mem::transmute_copy(&at) }
            }

            fn clock_gettime(&self, clk: i32) -> (i32, Timespec) {
                let f: unsafe extern "C" fn(i32, *mut Timespec) -> i32 = self.function("__vdso_clock_gettime");
                let mut t = Timespec::default();
                // SAFETY: a clock and a timespec, as it takes them.
                (unsafe { f(clk, &mut t) }, t)
            }

            fn clock_getres(&self, clk: i32) -> (i32, Timespec) {
                let f: unsafe extern "C" fn(i32, *mut Timespec) -> i32 = self.function("__vdso_clock_getres");
                let mut t = Timespec { sec: -1, nsec: -1 };
                // SAFETY: as above.
                (unsafe { f(clk, &mut t) }, t)
            }

            /// `gettimeofday`, with a time zone if `zone`: what it returns,
            /// the time, and the zone.
            fn gettimeofday(&self, zone: bool) -> (i32, Timespec, [i32; 2]) {
                let f: unsafe extern "C" fn(*mut Timespec, *mut [i32; 2]) -> i32 = self.function("__vdso_gettimeofday");
                let (mut tv, mut tz) = (Timespec::default(), [-7, -7]);
                let zone_at = if zone { &raw mut tz } else { core::ptr::null_mut() };
                // SAFETY: a timeval (a timespec's layout) and a timezone.
                (unsafe { f(&mut tv, zone_at) }, tv, tz)
            }

            /// `time`: what it returns, and what it stored.
            fn time(&self) -> (i64, i64) {
                let f: unsafe extern "C" fn(*mut i64) -> i64 = self.function("__vdso_time");
                let mut t = 0;
                // SAFETY: a time_t, or none.
                let (stored, returned) = unsafe { (f(&mut t), f(core::ptr::null_mut())) };
                assert!(returned - stored <= 1);
                (stored, t)
            }
        }

        impl Drop for Vdso {
            fn drop(&mut self) {
                host(__NR_munmap, [self.base, self.len, 0, 0, 0, 0]);
            }
        }

        /// The clocks each is read as, of those below 32.
        fn read_here() -> impl Iterator<Item = (i32, bool)> {
            (0..32).filter(|c| (UTC_CLOCKS | MONOTONIC_CLOCKS) & 1 << c != 0).map(|c| (c, UTC_CLOCKS & 1 << c != 0))
        }

        #[test]
        fn the_clocks_are_the_counter_by_the_scale() {
            let v = Vdso::map();
            // The counter before the clocks started: the monotonic clocks
            // 0, the real-time ones what they were at the start, split
            // into seconds exactly, as the arithmetic is for every number.
            for utc in numbers().take(20_000) {
                let scale =
                    ClockInfo { ticks_at_zero: u64::MAX, ns_per_tick: 1 << 32, utc_at_zero: utc, ..Default::default() };
                v.scale(Some(scale));
                for (clk, real) in read_here() {
                    assert_eq!(v.clock_gettime(clk), (0, split(if real { utc } else { 0 })), "clock {clk}");
                }
                let tv = split(utc);
                assert_eq!(v.gettimeofday(false), (0, Timespec { sec: tv.sec, nsec: tv.nsec / 1000 }, [-7, -7]));
                assert_eq!(v.time(), (tv.sec, tv.sec));
            }
            // A scale that is no whole number of nanoseconds a tick, from
            // now: what the counter said before and after a reading bounds it.
            let (start, per_tick, utc) = (ticks(), 0x1_5555_5555u64, 1_762_000_000_123_456_789);
            let scale =
                ClockInfo { ticks_at_zero: start, ns_per_tick: per_tick, utc_at_zero: utc, ..Default::default() };
            v.scale(Some(scale));
            let at = |t: u64| ((u128::from(t - start) * u128::from(per_tick)) >> 32) as u64;
            for _ in 0..10_000 {
                for (clk, real) in read_here() {
                    let before = at(ticks()) + if real { utc } else { 0 };
                    let (r, t) = v.clock_gettime(clk);
                    let after = at(ticks()) + if real { utc } else { 0 };
                    assert_eq!(r, 0);
                    assert!((before..=after).contains(&nanoseconds(t)), "clock {clk}: {before} {t:?} {after}");
                }
            }
            // The real time saturates, as the kernel's sum.
            let scale =
                ClockInfo { ticks_at_zero: 0, ns_per_tick: 1 << 32, utc_at_zero: u64::MAX, ..Default::default() };
            v.scale(Some(scale));
            assert_eq!(v.clock_gettime(CLOCK_REALTIME as i32), (0, split(u64::MAX)));
            // Each clock read here counts in what the system call says.
            let resolution = split(posix::host::RESOLUTION_NS);
            for (clk, _) in read_here() {
                assert_eq!(v.clock_getres(clk), (0, resolution));
            }
        }

        #[test]
        fn what_is_not_read_here_is_the_system_call_s() {
            let v = Vdso::map();
            let scale = ClockInfo { ticks_at_zero: ticks(), ns_per_tick: 1 << 32, ..Default::default() };
            for scale in [Some(scale), None] {
                v.scale(scale);
                // The clocks of CPU time, a process's, Linux's TAI, and
                // clocks that are none: what the host says of each.
                let pid_clock = !(std::process::id() as i32) << 3 | 2;
                for clk in [2, 3, 11, 32, 12345, -1, pid_clock] {
                    let mut t = Timespec::default();
                    let expected = host(__NR_clock_gettime, [clk as usize, &raw mut t as usize, 0, 0, 0, 0]);
                    assert_eq!(v.clock_gettime(clk).0 as isize, expected, "clock {clk}");
                    let expected = host(__NR_clock_getres, [clk as usize, &raw mut t as usize, 0, 0, 0, 0]);
                    assert_eq!(v.clock_getres(clk).0 as isize, expected, "clock {clk}");
                }
                // A time zone is the system call's to give.
                let (r, _, zone) = v.gettimeofday(true);
                assert_eq!(r, 0);
                assert_ne!(zone, [-7, -7]);
                // The processor's number too.
                let f: unsafe extern "C" fn(*mut u32, *mut u32, usize) -> i64 = v.function("__vdso_getcpu");
                let (mut cpu, mut node) = (u32::MAX, u32::MAX);
                // SAFETY: two numbers to write, and no cache.
                assert_eq!(unsafe { f(&mut cpu, &mut node, 0) }, 0);
                assert!(cpu < 1 << 16 && node < 1 << 16);
            }
            // Without a scale every clock is the host's, and the time.
            let before = (host_clock(CLOCK_MONOTONIC), host_clock(CLOCK_REALTIME));
            let read = (v.clock_gettime(CLOCK_MONOTONIC as i32), v.clock_gettime(CLOCK_REALTIME as i32));
            let (seconds, _) = v.time();
            let (r, tv, _) = v.gettimeofday(false);
            let after = (host_clock(CLOCK_MONOTONIC), host_clock(CLOCK_REALTIME));
            assert_eq!((read.0.0, read.1.0, r), (0, 0, 0));
            assert!(before.0 <= read.0.1 && read.0.1 <= after.0);
            assert!(before.1 <= read.1.1 && read.1.1 <= after.1);
            assert!(before.1.sec <= seconds && seconds <= after.1.sec);
            assert!(before.1.sec <= tv.sec && tv.sec <= after.1.sec && tv.nsec < 1_000_000);
        }
    }
}

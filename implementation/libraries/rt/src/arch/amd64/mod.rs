// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The runtime on AMD64: how Veda's system calls are made, where programs
//! and threads start, the C runtime symbols of the user target
//! (`x86_64-unknown-linux-musl`), and what programs learn of the processor.
//! In the driver VM's Linux guest, Linux's system calls (`linux`).

#[cfg(feature = "runtime")]
mod crt;
#[cfg(veda_guest)]
pub(crate) mod linux;

/// The machine's name, as `uname -m` gives it.
pub const MACHINE: &str = "x86_64";

/// Makes Veda system call `n` (see `vabi::arch`): its status, and its
/// second result.
///
/// # Safety
/// Memory the call reads or writes through its arguments must be what the
/// call takes.
#[cfg(not(veda_guest))]
#[inline(always)]
pub unsafe fn syscall6(n: usize, a0: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize) -> (isize, usize) {
    let ret: isize;
    let rdx: usize;
    // SAFETY: the kernel preserves all registers except rax, rdx (second
    // result), rcx and r11.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") n as isize => ret,
            in("rdi") a0,
            in("rsi") a1,
            inlateout("rdx") a2 => rdx,
            in("r10") a3,
            in("r8") a4,
            in("r9") a5,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    (ret, rdx)
}

/// The process entry point (`--entry=_vrt_start`, see `vbuild`). The
/// kernel passes the bootstrap channel handle in `rdi`.
#[cfg(feature = "runtime")]
#[unsafe(no_mangle)]
pub extern "sysv64" fn _vrt_start(bootstrap: vabi::RawHandle, _arg: usize) -> ! {
    crate::start::start(bootstrap)
}

/// Where the threads [`crate::thread::Builder`] spawns start: the kernel
/// passes the thread's argument in `rdi`.
#[cfg(not(veda_guest))]
pub(crate) extern "sysv64" fn thread_entry(arg: usize, _unused: usize) -> ! {
    crate::thread::run(arg)
}

/// The processor's features as Linux's `AT_HWCAP` reports them on x86-64
/// (`CPUID.1:EDX`), for ELF programs' auxiliary vector.
pub(crate) fn hwcap() -> u64 {
    core::arch::x86_64::__cpuid(1).edx as u64
}

/// The clock's counter (the TSC), as [`vabi::ClockInfo`] describes it.
#[cfg(veda_guest)]
pub(crate) fn clock_ticks() -> u64 {
    // SAFETY: RDTSC is always available.
    unsafe { core::arch::x86_64::_rdtsc() }
}

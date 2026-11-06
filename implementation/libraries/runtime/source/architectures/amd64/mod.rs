// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The runtime on AMD64: how Veda's system calls are made, where programs
//! and threads start and how a thread frees its own stack as it ends, the
//! C runtime symbols of the user target
//! (`x86_64-unknown-linux-musl`), and what programs learn of the processor.
//! In the driver VM's Linux guest, Linux's system calls (`linux`). On the
//! standard library (`x86_64-unknown-veda`) programs and threads start as
//! it starts them, and the C library has the C runtime's symbols.

#[cfg(all(feature = "runtime", not(test), not(target_os = "veda")))]
mod crt;
#[cfg(veda_guest)]
pub(crate) mod linux;

/// The machine's name, as `uname -m` gives it.
pub const MACHINE: &str = "x86_64";

/// Makes Veda system call `n` (see `abi::architecture`): its status, and
/// its second result.
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

/// `foreign_run(state, flags)` (see `crate::foreign`): its raw result.
/// Foreign code may leave any register changed but those the kernel
/// restores (every general-purpose register but `rax`, and the thread
/// pointer): the x87, SSE and AVX registers are the caller's to keep, so
/// all are marked as the C ABI's calls mark them.
#[cfg(not(veda_guest))]
#[inline(always)]
pub fn foreign_run(state: *mut abi::ForeignState, flags: usize) -> isize {
    let ret: isize;
    // SAFETY: the kernel writes only `state`, which the caller lends; the
    // caller's own registers are as they were but `rax`, `rcx`, `r11` and
    // those of the vector unit.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") abi::nr::FOREIGN_RUN as isize => ret,
            in("rdi") state,
            in("rsi") flags,
            lateout("rcx") _,
            lateout("r11") _,
            clobber_abi("C"),
            options(nostack),
        );
    }
    ret
}

/// The process entry point (`--entry=_rt_start`, see `build`). The
/// kernel passes the bootstrap channel handle in `rdi`.
#[cfg(all(feature = "runtime", not(test), not(target_os = "veda")))]
#[unsafe(no_mangle)]
pub extern "sysv64" fn _rt_start(bootstrap: abi::RawHandle, _arg: usize) -> ! {
    crate::start::start(bootstrap)
}

/// Where the threads [`crate::thread::Builder`] spawns start: the kernel
/// passes the thread's argument in `rdi`.
#[cfg(not(any(veda_guest, target_os = "veda")))]
pub(crate) extern "sysv64" fn thread_entry(arg: usize, _unused: usize) -> ! {
    crate::thread::run(arg)
}

/// Unmaps `[base, base + len)` of the calling process, the calling thread's
/// stack, and ends the thread, touching no memory in between: both system
/// calls are in one block of code that uses no stack. If the unmap fails,
/// the thread ends all the same.
///
/// # Safety
/// Nothing may use the range afterwards: it is gone once the thread ends.
#[cfg(not(any(veda_guest, target_os = "veda")))]
pub(crate) unsafe fn exit_unmapping(base: usize, len: usize) -> ! {
    // SAFETY: `vm_unmap(self, base, len)`, then `thread_exit()`; the kernel
    // preserves the argument registers, and nothing between the two
    // `syscall`s reads or writes memory.
    unsafe {
        core::arch::asm!(
            "syscall",
            "mov eax, {exit}",
            "syscall",
            "ud2",
            exit = const abi::nr::THREAD_EXIT,
            in("rax") abi::nr::VM_UNMAP,
            in("rdi") abi::INVALID_HANDLE as usize,
            in("rsi") base,
            in("rdx") len,
            options(noreturn, nostack),
        )
    }
}

/// The processor's features as Linux's `AT_HWCAP` and `AT_HWCAP2` report
/// them on x86-64 (`CPUID.1:EDX`, and nothing), for ELF programs' auxiliary
/// vector.
pub(crate) fn hwcap() -> [u64; 2] {
    [core::arch::x86_64::__cpuid(1).edx as u64, 0]
}

/// The clock's counter (the TSC), as [`abi::ClockInfo`] describes it, read
/// once every earlier load has completed (`lfence`): a time read after
/// seeing another thread's is never earlier than it, as a system call's
/// reading never was.
pub(crate) fn clock_ticks() -> u64 {
    // SAFETY: LFENCE (SSE2) and RDTSC are always available on x86-64, and
    // the kernel lets user code read the TSC (CR4.TSD is clear).
    unsafe {
        core::arch::x86_64::_mm_lfence();
        core::arch::x86_64::_rdtsc()
    }
}

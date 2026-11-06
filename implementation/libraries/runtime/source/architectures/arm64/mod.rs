// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The runtime on ARM64: how Veda's system calls are made, where programs
//! and threads start and how a thread frees its own stack as it ends, the
//! C runtime symbols of the user target
//! (`aarch64-unknown-linux-musl`), and what programs learn of the
//! processor. In the driver VM's Linux guest, Linux's system calls
//! (`linux`). On the standard library (`aarch64-unknown-veda`) programs and
//! threads start as it starts them, and the C library has the C runtime's
//! symbols.

// On the soft-float target (a program that leaves the vector registers
// alone, `build::arm64::SOFT_FLOAT_TARGET`), whose memory primitives
// `compiler_builtins` has, as the target's precompiled `core` asks for no
// unwinder.
#[cfg(all(feature = "runtime", not(test), not(target_os = "veda"), target_feature = "neon"))]
mod crt;
#[cfg(veda_guest)]
pub(crate) mod linux;

/// The machine's name, as `uname -m` gives it.
pub const MACHINE: &str = "aarch64";

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
    let second: usize;
    // SAFETY: the kernel preserves all registers except x0 and x1 (the two
    // results).
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") n,
            inlateout("x0") a0 as isize => ret,
            inlateout("x1") a1 => second,
            in("x2") a2,
            in("x3") a3,
            in("x4") a4,
            in("x5") a5,
            options(nostack),
        );
    }
    (ret, second)
}

/// `foreign_run(state, flags)` (see `crate::foreign`): its raw result.
/// Foreign code may leave any register changed but those the kernel
/// restores (every general-purpose one but `x0`, and the thread pointer):
/// the floating point and SIMD registers are the caller's to keep, the
/// low halves of `v8` to `v15` too, which the AAPCS64 has calls keep.
#[cfg(not(veda_guest))]
#[inline(always)]
pub fn foreign_run(state: *mut abi::ForeignState, flags: usize) -> isize {
    let ret: isize;
    // SAFETY: the kernel writes only `state`, which the caller lends; the
    // caller's own registers are as they were but `x0` and the vector
    // unit's.
    #[cfg(target_feature = "neon")]
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::nr::FOREIGN_RUN,
            inlateout("x0") state as isize => ret,
            in("x1") flags,
            out("v8") _, out("v9") _, out("v10") _, out("v11") _,
            out("v12") _, out("v13") _, out("v14") _, out("v15") _,
            clobber_abi("C"),
            options(nostack),
        );
    }
    // SAFETY: as above; code built without the vector unit has nothing in it.
    #[cfg(not(target_feature = "neon"))]
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::nr::FOREIGN_RUN,
            inlateout("x0") state as isize => ret,
            in("x1") flags,
            clobber_abi("C"),
            options(nostack),
        );
    }
    ret
}

/// The process entry point (`--entry=_rt_start`, see `build`). The
/// kernel passes the bootstrap channel handle in `x0`.
#[cfg(all(feature = "runtime", not(test), not(target_os = "veda")))]
#[unsafe(no_mangle)]
pub extern "C" fn _rt_start(bootstrap: abi::RawHandle, _arg: usize) -> ! {
    crate::start::start(bootstrap)
}

/// Where the threads [`crate::thread::Builder`] spawns start: the kernel
/// passes the thread's argument in `x0`.
#[cfg(not(any(veda_guest, target_os = "veda")))]
pub(crate) extern "C" fn thread_entry(arg: usize, _unused: usize) -> ! {
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
    // SAFETY: `vm_unmap(self, base, len)`, then `thread_exit()`; nothing
    // between the two `svc`s reads or writes memory.
    unsafe {
        core::arch::asm!(
            "svc #0",
            "mov x8, {exit}",
            "svc #0",
            "brk #0",
            exit = const abi::nr::THREAD_EXIT,
            in("x8") abi::nr::VM_UNMAP,
            in("x0") abi::INVALID_HANDLE as usize,
            in("x1") base,
            in("x2") len,
            options(noreturn, nostack),
        )
    }
}

/// The processor's features as Linux's `AT_HWCAP` and `AT_HWCAP2` report
/// them on AArch64, for ELF programs' auxiliary vector: the kernel's word,
/// as programs cannot read the ID registers (`abi::arm64::hwcap`). Without
/// it, the two every processor Veda runs on has (`HWCAP_FP`, `HWCAP_ASIMD`).
pub(crate) fn hwcap() -> [u64; 2] {
    crate::object::system_info().map_or([0b11, 0], |info| info.hwcap)
}

/// The clock's counter (`CNTVCT_EL0`), as [`abi::ClockInfo`] describes it,
/// read once every earlier load has completed: a time read after seeing
/// another thread's is never earlier than it, as a system call's reading
/// never was. (`ISB` keeps the read from being done early, and `DSB` waits
/// for the loads: the counter is no memory access for `DMB` to order.)
pub(crate) fn clock_ticks() -> u64 {
    let ticks: u64;
    // SAFETY: the kernel lets user code read the counter (`CNTKCTL_EL1`).
    unsafe {
        core::arch::asm!("dsb ishld", "isb", "mrs {}, cntvct_el0", out(reg) ticks, options(nostack, preserves_flags));
    }
    ticks
}

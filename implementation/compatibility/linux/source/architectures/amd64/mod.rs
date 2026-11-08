// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Linux's x86-64 ABI, as a Linux program's code has it: where a system
//! call's number, arguments and result are, how a thread starts, Linux's
//! signal frame (`struct rt_sigframe`, which handlers and debuggers read
//! and change), what each exception is as a signal, the x87, SSE and AVX
//! registers, which the frame keeps in the processor's own format
//! (`XSAVE`'s, with Linux's markers), and the vDSO's code ([`vdso`]).

pub mod vdso;

use abi::ForeignState;

use crate::signal::{AltStack, Fault, Frame};
use crate::task::Restart;

/// Linux's name for the processor (`uname`'s machine, `AT_PLATFORM`).
pub const MACHINE: &str = "x86_64";

/// The bytes of the instruction a system call is made with (`syscall`),
/// which a call made again (`SA_RESTART`) goes back over.
const SYSCALL_LEN: u64 = 2;

/// The system call's number and arguments.
pub fn syscall(s: &ForeignState) -> (usize, [usize; 6]) {
    let r = &s.registers;
    (r.rax as usize, [r.rdi, r.rsi, r.rdx, r.r10, r.r8, r.r9].map(|v| v as usize))
}

/// Returns `value` from the system call.
pub fn set_result(s: &mut ForeignState, value: isize) {
    s.registers.rax = value as u64;
}

/// What the system call returned (or is to return).
pub fn result(s: &ForeignState) -> isize {
    s.registers.rax as isize
}

/// Has system call `n`, which returned, made again as the code resumes:
/// back over its instruction, its number where it was.
/// Has the call `r` made again: back over its `syscall` instruction, its
/// number in `rax` again (its result's register; the arguments' are
/// kept).
pub fn restart(s: &mut ForeignState, r: &Restart) {
    s.registers.rip = s.registers.rip.wrapping_sub(SYSCALL_LEN);
    s.registers.rax = r.call as u64;
}

pub fn pc(s: &ForeignState) -> u64 {
    s.registers.rip
}

pub fn sp(s: &ForeignState) -> u64 {
    s.registers.rsp
}

pub fn tls(s: &ForeignState) -> u64 {
    s.registers.fs_base
}

pub fn set_tls(s: &mut ForeignState, tls: u64) {
    s.registers.fs_base = tls;
}

/// The registers a program starts with: at `entry`, its stack pointer at
/// `sp` (`argc`), every other register zero (`rdx`, a function for
/// `atexit`, none).
pub fn start(entry: u64, sp: u64) -> ForeignState {
    let mut s = ForeignState::default();
    s.registers.rip = entry;
    s.registers.rsp = sp;
    s.registers.rflags = abi::architecture::INTERRUPT_FLAG;
    s
}

/// The registers of a thread `clone` starts, from its parent's as the call
/// left them: it returns 0, on stack `sp` (if not 0), with thread pointer
/// `tls` (if given).
pub fn child(parent: &ForeignState, sp: u64, tls: Option<u64>) -> ForeignState {
    let mut s = *parent;
    s.registers.rax = 0;
    if sp != 0 {
        s.registers.rsp = sp;
    }
    if let Some(tls) = tls {
        s.registers.fs_base = tls;
    }
    s
}

/// `arch_prctl`'s codes.
pub mod arch_prctl {
    pub const SET_GS: u32 = 0x1001;
    pub const SET_FS: u32 = 0x1002;
    pub const GET_FS: u32 = 0x1003;
    pub const GET_GS: u32 = 0x1004;
    pub const GET_CPUID: u32 = 0x1011;
    pub const SET_CPUID: u32 = 0x1012;
}

/// What a frame's context says of the exception a fault was: its vector
/// (`trapno`) and error code (`err`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Exception {
    pub trap: u64,
    pub error: u64,
}

/// The signal a fault stands for, as Linux raises it (`arch/x86/kernel/traps.c`,
/// `fault.c`): which, how (`si_code`) and where (`si_addr`).
pub fn fault(s: &ForeignState) -> Fault {
    use crate::linux::{si, sig};
    let at = s.registers.rip;
    let (signal, code, addr) = match s.vector {
        0 => (sig::FPE, si::FPE_INTDIV, at),
        1 => (sig::TRAP, si::TRAP_TRACE, at),
        3 => (sig::TRAP, si::KERNEL, 0),
        6 => (sig::ILL, si::ILL_ILLOPN, at),
        12 => (sig::BUS, si::KERNEL, 0),
        14 => {
            let code = if s.error_code & 1 != 0 { si::SEGV_ACCERR } else { si::SEGV_MAPERR };
            (sig::SEGV, code, s.fault_address)
        }
        16 | 19 => (sig::FPE, si::FPE_FLTINV, at),
        17 => (sig::BUS, si::BUS_ADRALN, s.fault_address),
        21 => (sig::SEGV, si::SEGV_CPERR, 0),
        // #GP, #OF, #BR and the rest.
        _ => (sig::SEGV, si::KERNEL, 0),
    };
    Fault { signal, code, addr, exception: Exception { trap: s.vector, error: s.error_code } }
}

/// The fault registers no code can have are (a frame's that `rt_sigreturn`
/// took back, whose return the processor refuses): a general protection
/// fault, `SIGSEGV`, as Linux's `iret` takes it.
pub fn refused(_s: &ForeignState) -> Fault {
    use crate::linux::{si, sig};
    Fault { signal: sig::SEGV, code: si::KERNEL, addr: 0, exception: Exception { trap: 13, error: 0 } }
}

/// The legacy virtual system calls' page (`vsyscall`): a call into it is
/// carried out as Linux's emulation does, for programs built before the
/// vDSO took its place. Returns the call it is (`gettimeofday`, `time` or
/// `getcpu`), if the fault is a call there.
pub fn vsyscall(s: &ForeignState) -> Option<usize> {
    use linux_raw_sys::general::{__NR_getcpu, __NR_gettimeofday, __NR_time};
    const PAGE: u64 = 0xffff_ffff_ff60_0000;
    if s.vector != 14 || s.registers.rip != s.fault_address || s.fault_address & !0xfff != PAGE {
        return None;
    }
    match s.fault_address & 0xfff {
        0x000 => Some(__NR_gettimeofday as usize),
        0x400 => Some(__NR_time as usize),
        0x800 => Some(__NR_getcpu as usize),
        _ => None,
    }
}

/// Returns from a virtual system call with `result`, as a `ret` would:
/// false if the return address cannot be read.
pub fn vsyscall_return(s: &mut ForeignState, result: isize) -> bool {
    let sp = s.registers.rsp;
    // SAFETY: the program's stack, which a fault reading it ends.
    let Some(ret) = (unsafe { crate::user::read::<u64>(sp as usize) }) else { return false };
    s.registers.rax = result as u64;
    s.registers.rip = ret;
    s.registers.rsp = sp + 8;
    true
}

/// `struct rt_sigframe` on x86-64, from where the handler finds its stack
/// pointer: the return address (`sa_restorer`), the context (`ucontext`:
/// flags, link, signal stack, `sigcontext`, mask) and what the signal says
/// (`siginfo`). The x87, SSE and AVX registers go below, 64-byte aligned,
/// as `XSAVE` writes them, their address in the context.
mod frame {
    pub const RESTORER: usize = 0;
    pub const UC_FLAGS: usize = 8;
    pub const UC_LINK: usize = 16;
    pub const UC_STACK: usize = 24;
    pub const MCONTEXT: usize = 48;
    pub const SIGMASK: usize = 304;
    pub const INFO: usize = 312;
    pub const SIZE: usize = 440;

    /// `sigcontext`'s registers come first, in its order: r8 to r15, rdi,
    /// rsi, rbp, rbx, rdx, rax, rcx, rsp, rip, eflags. Where the segments
    /// are (cs, gs, fs, ss, a 16-bit word each).
    pub const CSGSFS: usize = 8 * 18;
    pub const ERR: usize = 8 * 19;
    pub const TRAPNO: usize = 8 * 20;
    pub const OLDMASK: usize = 8 * 21;
    pub const CR2: usize = 8 * 22;
    pub const FPSTATE: usize = 8 * 23;

    /// The context's flags: the registers are in `XSAVE`'s format, and the
    /// stack segment is in the context (and is restored, strictly).
    pub const UC_FP_XSTATE: u64 = 0x1;
    pub const UC_SIGCONTEXT_SS: u64 = 0x2;
    pub const UC_STRICT_RESTORE_SS: u64 = 0x4;

    /// Linux's selectors of user code and data on x86-64, which the
    /// context reports.
    pub const USER_CS: u16 = 0x33;
    pub const USER_DS: u16 = 0x2b;
}

/// The flags a frame's context may change (Linux's `FIX_EFLAGS`, but the
/// trap flag, which no thread here is given).
const FIX_FLAGS: u64 = 0x40000 | 0x800 | 0x400 | 0x80 | 0x40 | 0x10 | 0x4 | 0x1;

/// Lays out a signal frame for a handler of `frame.signal` below the
/// stack pointer `sp` (the red zone left alone), or at the top of the
/// alternate stack `stack` if it is to run there, and sets the registers
/// to enter the handler. The frame keeps `s` as the code it interrupted
/// leaves them, and the vector registers as they are. `None` if the stack
/// cannot take the frame (unmapped, or the alternate stack too small): the
/// thread is then sent `SIGSEGV`, as on Linux.
pub fn push_frame(s: &mut ForeignState, frame: &Frame, stack: &AltStack) -> Option<()> {
    let regs = s.registers;
    let fpu = fpu::size();
    let top = stack.top_for(regs.rsp, frame.onstack);
    let mut sp = top.unwrap_or(regs.rsp.wrapping_sub(128));
    // The vector registers, then the frame, its stack pointer as after a
    // call (8 off a multiple of 16).
    sp = sp.checked_sub(fpu as u64 + 4)? & !63;
    let fpstate = sp;
    sp = ((sp.checked_sub(frame::SIZE as u64)?) & !15).checked_sub(8)?;
    if !stack.fits(sp, regs.rsp, top.is_some()) {
        return None;
    }
    let mut bytes = [0u8; frame::SIZE];
    let put = |b: &mut [u8], at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put(&mut bytes, frame::RESTORER, frame.restorer);
    let xstate = fpu::xsave_supported();
    let flags = frame::UC_SIGCONTEXT_SS | frame::UC_STRICT_RESTORE_SS | if xstate { frame::UC_FP_XSTATE } else { 0 };
    put(&mut bytes, frame::UC_FLAGS, flags);
    put(&mut bytes, frame::UC_LINK, 0);
    let (ss_sp, ss_flags, ss_size) = stack.reported(regs.rsp);
    put(&mut bytes, frame::UC_STACK, ss_sp);
    bytes[frame::UC_STACK + 8..frame::UC_STACK + 12].copy_from_slice(&ss_flags.to_le_bytes());
    put(&mut bytes, frame::UC_STACK + 16, ss_size);
    let m = frame::MCONTEXT;
    let gregs = [
        regs.r8,
        regs.r9,
        regs.r10,
        regs.r11,
        regs.r12,
        regs.r13,
        regs.r14,
        regs.r15,
        regs.rdi,
        regs.rsi,
        regs.rbp,
        regs.rbx,
        regs.rdx,
        regs.rax,
        regs.rcx,
        regs.rsp,
        regs.rip,
        regs.rflags,
    ];
    for (i, v) in gregs.iter().enumerate() {
        put(&mut bytes, m + 8 * i, *v);
    }
    let segments = (frame::USER_CS as u64) | (frame::USER_DS as u64) << 48;
    put(&mut bytes, m + frame::CSGSFS, segments);
    put(&mut bytes, m + frame::ERR, frame.fault.map_or(0, |f| f.exception.error));
    put(&mut bytes, m + frame::TRAPNO, frame.fault.map_or(0, |f| f.exception.trap));
    put(&mut bytes, m + frame::OLDMASK, frame.restore_mask);
    put(&mut bytes, m + frame::CR2, frame.fault.map_or(0, |f| f.addr));
    put(&mut bytes, m + frame::FPSTATE, fpstate);
    put(&mut bytes, frame::SIGMASK, frame.restore_mask);
    let info = &frame.info;
    // SAFETY: `Siginfo` is plain data, 128 bytes.
    let info_bytes: [u8; 128] = unsafe { core::mem::transmute_copy(info) };
    bytes[frame::INFO..frame::INFO + 128].copy_from_slice(&info_bytes);
    // SAFETY: the program's stack, where a fault (it is not mapped) ends
    // the copy.
    unsafe {
        crate::user::write_bytes(sp as usize, &bytes)?;
        fpu::save(fpstate as usize)?;
    }
    let r = &mut s.registers;
    r.rsp = sp;
    r.rip = frame.handler;
    r.rdi = frame.info.signo as u64;
    r.rsi = sp + frame::INFO as u64;
    r.rdx = sp + frame::UC_FLAGS as u64;
    r.rax = 0;
    // As a handler starts on Linux: no direction flag, no single-stepping.
    r.rflags &= !(0x400 | 0x100 | 0x10000);
    Some(())
}

/// What `rt_sigreturn` takes back from the frame below the stack pointer
/// (the handler returned to its restorer, which made the call): the
/// registers, the vector registers, and the mask and alternate stack to
/// restore. `None` for a frame that cannot be read, or holds registers the
/// processor would refuse: the thread is then sent `SIGSEGV`.
pub fn pop_frame(s: &mut ForeignState) -> Option<(u64, (u64, i32, u64))> {
    let at = s.registers.rsp.checked_sub(8)? as usize;
    let mut bytes = [0u8; frame::SIZE];
    // SAFETY: the program's stack, which a fault reading it ends.
    unsafe { crate::user::read_bytes(at, &mut bytes)? };
    let get = |b: &[u8], off: usize| u64::from_le_bytes(b[off..off + 8].try_into().unwrap());
    let m = frame::MCONTEXT;
    let g = |i: usize| get(&bytes, m + 8 * i);
    let r = &mut s.registers;
    [r.r8, r.r9, r.r10, r.r11, r.r12, r.r13, r.r14, r.r15] = [g(0), g(1), g(2), g(3), g(4), g(5), g(6), g(7)];
    [r.rdi, r.rsi, r.rbp, r.rbx, r.rdx, r.rax, r.rcx, r.rsp] = [g(8), g(9), g(10), g(11), g(12), g(13), g(14), g(15)];
    r.rip = g(16);
    r.rflags = (r.rflags & !FIX_FLAGS) | (g(17) & FIX_FLAGS);
    let fpstate = get(&bytes, m + frame::FPSTATE);
    let xstate = get(&bytes, frame::UC_FLAGS) & frame::UC_FP_XSTATE != 0;
    // SAFETY: the program's memory, which `restore` copies and checks
    // before the processor takes it.
    unsafe { fpu::restore(fpstate as usize, xstate)? };
    let mask = get(&bytes, frame::SIGMASK);
    let stack = (
        get(&bytes, frame::UC_STACK),
        i32::from_le_bytes(bytes[frame::UC_STACK + 8..frame::UC_STACK + 12].try_into().unwrap()),
        get(&bytes, frame::UC_STACK + 16),
    );
    Some((mask, stack))
}

/// The x87, SSE and AVX registers, as Linux keeps them in a signal frame:
/// `XSAVE`'s standard format for what the kernel enabled (`XCR0`), with
/// Linux's markers (`struct _fpx_sw_bytes` in the legacy area's spare
/// bytes, and a word after the area), or `FXSAVE`'s on a processor without
/// `XSAVE`. Linux compatibility touches none of these registers itself
/// (it is built without them), so they are the program's as it left them.
pub mod fpu {
    use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    /// `XSAVE`'s area for what the kernel enabled, or 0 before [`init`]
    /// (`FXSAVE`'s 512 bytes where there is no `XSAVE`).
    static SIZE: AtomicUsize = AtomicUsize::new(0);
    /// The features enabled (`XCR0`); 0 where there is no `XSAVE`.
    static FEATURES: AtomicU64 = AtomicU64::new(0);
    /// The bits of `MXCSR` that can be set (`MXCSR_MASK`).
    static MXCSR_MASK: AtomicU64 = AtomicU64::new(0xffbf);

    const MAGIC1: u32 = 0x4650_5853;
    const MAGIC2: u32 = 0x4650_5845;
    /// Where Linux's markers are in the legacy area (`sw_reserved`).
    const SW_BYTES: usize = 464;
    /// Where `XSAVE`'s header is.
    const HEADER: usize = 512;

    /// Learns the processor's registers: what `XSAVE` saves (CPUID leaf
    /// 0xD, `XCR0`) and which bits of `MXCSR` it takes.
    pub fn init() {
        // SAFETY: CPUID and XGETBV read the processor's features.
        unsafe {
            let osxsave = core::arch::x86_64::__cpuid(1).ecx & (1 << 27) != 0;
            if osxsave {
                let size = core::arch::x86_64::__cpuid_count(0xD, 0).ebx as usize;
                let (lo, hi): (u32, u32);
                core::arch::asm!("xgetbv", in("ecx") 0, out("eax") lo, out("edx") hi, options(nomem, nostack));
                FEATURES.store(u64::from(lo) | u64::from(hi) << 32, Ordering::Relaxed);
                SIZE.store(size.max(576), Ordering::Relaxed);
            } else {
                SIZE.store(512, Ordering::Relaxed);
            }
            #[repr(C, align(16))]
            struct Area([u8; 512]);
            let mut area = Area([0; 512]);
            core::arch::asm!("fxsave64 [{}]", in(reg) area.0.as_mut_ptr(), options(nostack));
            let mask = u32::from_le_bytes(area.0[28..32].try_into().unwrap());
            if mask != 0 {
                MXCSR_MASK.store(u64::from(mask), Ordering::Relaxed);
            }
        }
    }

    /// Has the registers be as a new program starts with them: their first
    /// state, with the control words Linux starts with.
    pub fn reset() {
        // SAFETY: no address: the first state, which the processor takes.
        let _ = unsafe { restore(0, false) };
    }

    /// The bytes the registers take in a frame.
    pub fn size() -> usize {
        SIZE.load(Ordering::Relaxed).max(512)
    }

    pub fn xsave_supported() -> bool {
        FEATURES.load(Ordering::Relaxed) != 0
    }

    /// Saves the registers at `at` (64-byte aligned, [`size`] bytes and 4
    /// more of the program's memory), with Linux's markers. `None` if the
    /// memory cannot take them.
    ///
    /// # Safety
    /// `at` must be the program's memory, which nothing of the host uses.
    pub unsafe fn save(at: usize) -> Option<()> {
        let size = size();
        let features = FEATURES.load(Ordering::Relaxed);
        // Only into the program's memory, writable, and held while it is
        // written: `XSAVE` to memory that is not there would end this
        // process.
        crate::memory::while_accessible(at, size + 4, true, || {
            // SAFETY: per the caller, and as above. The area is zeroed
            // first, its header too, which `XSAVE` leaves as it finds it
            // but `XRSTOR` checks.
            unsafe { save_into(at, size, features) }
        })
    }

    /// [`save`] into `at`, the program's memory, `size` bytes (and its
    /// end's marker) writable.
    unsafe fn save_into(at: usize, size: usize, features: u64) {
        // SAFETY: per the caller.
        unsafe {
            core::ptr::write_bytes(at as *mut u8, 0, size + 4);
            if features != 0 {
                core::arch::asm!("xsave64 [{}]", in(reg) at, in("eax") features as u32, in("edx") (features >> 32) as u32, options(nostack));
                let mut sw = [0u8; 48];
                sw[0..4].copy_from_slice(&MAGIC1.to_le_bytes());
                sw[4..8].copy_from_slice(&(size as u32 + 4).to_le_bytes());
                sw[8..16].copy_from_slice(&features.to_le_bytes());
                sw[16..20].copy_from_slice(&(size as u32).to_le_bytes());
                core::ptr::copy_nonoverlapping(sw.as_ptr(), (at + SW_BYTES) as *mut u8, 48);
                core::ptr::write_unaligned((at + size) as *mut u32, MAGIC2);
            } else {
                core::arch::asm!("fxsave64 [{}]", in(reg) at, options(nostack));
            }
        }
    }

    /// Loads the registers a frame's context has at `at` (0: none, which
    /// leaves them in their first state), in `XSAVE`'s format if `xstate`
    /// and the markers say so, else `FXSAVE`'s. They are copied and checked
    /// first, as the processor would refuse them (a bad `MXCSR` or header)
    /// in this process's place: `None` for those.
    ///
    /// # Safety
    /// `at` must be 0 or an address the program gave.
    pub unsafe fn restore(at: usize, xstate: bool) -> Option<()> {
        let size = size();
        #[repr(C, align(64))]
        struct Block([u8; 64]);
        let mut area: alloc::vec::Vec<Block> = (0..size.div_ceil(64)).map(|_| Block([0; 64])).collect();
        // SAFETY: the blocks are `size` rounded up to 64 bytes, contiguous.
        let bytes = unsafe { core::slice::from_raw_parts_mut(area.as_mut_ptr().cast::<u8>(), area.len() * 64) };
        let features = FEATURES.load(Ordering::Relaxed);
        // The x87 and SSE registers alone, unless the frame has more.
        let mut present = features & 0b11;
        if at == 0 {
            // The first state: the control words Linux starts with.
            bytes[0..2].copy_from_slice(&0x037fu16.to_le_bytes());
            bytes[24..28].copy_from_slice(&0x1f80u32.to_le_bytes());
        } else {
            if !at.is_multiple_of(64) {
                return None;
            }
            // SAFETY: per the caller; a fault ends the copy.
            unsafe { crate::user::read_bytes(at, &mut bytes[..512])? };
            let sw = &bytes[SW_BYTES..SW_BYTES + 20];
            let magic = u32::from_le_bytes(sw[0..4].try_into().unwrap());
            let frame_size = u32::from_le_bytes(sw[16..20].try_into().unwrap()) as usize;
            if features != 0 && xstate && magic == MAGIC1 && frame_size == size {
                // SAFETY: as above.
                unsafe { crate::user::read_bytes(at + 512, &mut bytes[512..size])? };
                let mut tail = [0u8; 4];
                // SAFETY: as above.
                unsafe { crate::user::read_bytes(at + size, &mut tail)? };
                if u32::from_le_bytes(tail) != MAGIC2 {
                    return None;
                }
                let header = &bytes[HEADER..HEADER + 64];
                let saved = u64::from_le_bytes(header[0..8].try_into().unwrap());
                let compacted = u64::from_le_bytes(header[8..16].try_into().unwrap());
                if saved & !features != 0 || compacted != 0 || header[16..].iter().any(|&b| b != 0) {
                    return None;
                }
                present = saved;
            } else {
                // The legacy registers alone; the rest start afresh.
                bytes[512..].fill(0);
            }
            let mxcsr = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
            if u64::from(mxcsr) & !MXCSR_MASK.load(Ordering::Relaxed) != 0 {
                return None;
            }
        }
        if features != 0 {
            // The header says what the area holds: the rest starts afresh.
            bytes[HEADER..HEADER + 8].copy_from_slice(&present.to_le_bytes());
            // SAFETY: an area checked as the processor checks it.
            unsafe {
                core::arch::asm!("xrstor64 [{}]", in(reg) bytes.as_ptr(), in("eax") features as u32, in("edx") (features >> 32) as u32, options(nostack));
            }
        } else {
            // SAFETY: as above.
            unsafe { core::arch::asm!("fxrstor64 [{}]", in(reg) bytes.as_ptr(), options(nostack)) };
        }
        Some(())
    }
}

/// What `AT_HWCAP` says of the processor, as Linux has it on x86-64: the
/// features of CPUID leaf 1 in `EDX`. (`AT_HWCAP2` says nothing: user code
/// is given no `FSGSBASE` it would use for the thread pointer of its own.)
pub fn hwcap() -> [u64; 2] {
    // SAFETY: CPUID reads the processor's features.
    [u64::from(core::arch::x86_64::__cpuid(1).edx), 0]
}

/// The registers as `/proc/self/stat` and crash reports name them: the
/// instruction and stack pointers.
pub fn describe(s: &ForeignState) -> (u64, u64) {
    (s.registers.rip, s.registers.rsp)
}

/// What `/proc/cpuinfo` says of processor `cpu` of `cpus`, named `brand`,
/// as Linux says it on x86-64: its name, where it is (one package, a core
/// each), and its features ([`cpu_flags`]).
pub fn cpuinfo(cpu: u32, cpus: u32, brand: &str) -> alloc::string::String {
    alloc::format!(
        "processor\t: {cpu}\nmodel name\t: {brand}\nphysical id\t: 0\nsiblings\t: {cpus}\ncore id\t\t: {cpu}\n\
         cpu cores\t: {cpus}\napicid\t\t: {cpu}\nflags\t\t: {}\n\n",
        cpu_flags()
    )
}

/// The processor's features, as `/proc/cpuinfo`'s `flags` name them: those
/// of CPUID leaf 1 and 7 that programs look for there.
fn cpu_flags() -> alloc::string::String {
    let (l1, l7) = (core::arch::x86_64::__cpuid(1), core::arch::x86_64::__cpuid_count(7, 0));
    let named: [(u32, u32, &str); 22] = [
        (l1.edx, 0, "fpu"),
        (l1.edx, 4, "tsc"),
        (l1.edx, 8, "cx8"),
        (l1.edx, 15, "cmov"),
        (l1.edx, 23, "mmx"),
        (l1.edx, 24, "fxsr"),
        (l1.edx, 25, "sse"),
        (l1.edx, 26, "sse2"),
        (l1.ecx, 0, "pni"),
        (l1.ecx, 1, "pclmulqdq"),
        (l1.ecx, 9, "ssse3"),
        (l1.ecx, 12, "fma"),
        (l1.ecx, 13, "cx16"),
        (l1.ecx, 19, "sse4_1"),
        (l1.ecx, 20, "sse4_2"),
        (l1.ecx, 22, "movbe"),
        (l1.ecx, 23, "popcnt"),
        (l1.ecx, 25, "aes"),
        (l1.ecx, 26, "xsave"),
        (l1.ecx, 28, "avx"),
        (l7.ebx, 5, "avx2"),
        (l7.ebx, 16, "avx512f"),
    ];
    let mut out = alloc::string::String::new();
    for (word, bit, name) in named {
        if word & (1 << bit) != 0 {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(name);
        }
    }
    out
}

/// `struct stat` on x86-64 for a file of Linux compatibility's own
/// (`crate::proc`): its type and permissions, and its size, owned by the
/// user, now.
pub fn stat_bytes(mode: u32, size: u64) -> [u8; 144] {
    let mut b = [0u8; 144];
    let now = runtime::time::utc_time_ns() / 1_000_000_000;
    b[16..24].copy_from_slice(&1u64.to_le_bytes());
    b[24..28].copy_from_slice(&mode.to_le_bytes());
    b[28..32].copy_from_slice(&1000u32.to_le_bytes());
    b[32..36].copy_from_slice(&1000u32.to_le_bytes());
    b[48..56].copy_from_slice(&size.to_le_bytes());
    b[56..64].copy_from_slice(&4096u64.to_le_bytes());
    for at in [72, 88, 104] {
        b[at..at + 8].copy_from_slice(&now.to_le_bytes());
    }
    b
}

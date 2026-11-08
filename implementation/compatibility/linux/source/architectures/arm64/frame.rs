// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Linux's signal frame on AArch64 (`struct rt_sigframe`,
//! `arch/arm64/kernel/signal.c`), which handlers, unwinders and debuggers
//! read and change: what the signal says (`siginfo`), then the context
//! (`ucontext`: flags, link, signal stack, mask, and `sigcontext`: the
//! fault's address, `x0` to `x30`, `sp`, `pc`, `pstate`), whose 4096 bytes
//! of records (`__reserved`) hold the floating-point and SIMD registers
//! (`fpsimd_context`), the fault's syndrome where a fault raised the
//! signal (`esr_context`), and a record of none, which ends them. A frame
//! record (`x29` and `x30` as the code had them) follows, which the
//! handler's frame pointer points at, for unwinders that follow frame
//! pointers. [`lay_out`] makes a frame, [`take_back`] reads one back for
//! `rt_sigreturn`, refusing what Linux refuses.
//!
//! It is plain data, of `abi::arm64`'s registers, so the host's tests check
//! it whatever the host is.

use abi::arm64::ThreadState;

/// Where the frame's parts are, from its start (16-byte aligned).
pub const INFO: usize = 0;
pub const UCONTEXT: usize = 128;
const UC_FLAGS: usize = UCONTEXT;
const UC_LINK: usize = UCONTEXT + 8;
const UC_STACK: usize = UCONTEXT + 16;
/// The mask: Linux's 64 signals, in the first word of the C libraries'
/// 1024 bits.
const UC_SIGMASK: usize = UCONTEXT + 40;
/// `sigcontext`, 16-byte aligned after the mask.
const MCONTEXT: usize = UCONTEXT + 176;
const FAULT_ADDRESS: usize = MCONTEXT;
const REGS: usize = MCONTEXT + 8;
const SP: usize = MCONTEXT + 256;
const PC: usize = MCONTEXT + 264;
const PSTATE: usize = MCONTEXT + 272;
/// The records (`__reserved`), 16-byte aligned.
const RESERVED: usize = MCONTEXT + 288;
const RESERVED_SIZE: usize = 4096;
/// The frame's size: the context's records whole, as Linux lays it out
/// when they fit (with no SVE, always).
pub const SIZE: usize = RESERVED + RESERVED_SIZE;
/// The frame record, after the frame: the code's `x29` and `x30`.
pub const RECORD: usize = 16;

const _: () = assert!(SIZE == 4688 && SIZE.is_multiple_of(16) && RESERVED.is_multiple_of(16));

/// The records' headers (`struct _aarch64_ctx`): a magic number and the
/// record's size, header included.
const HEADER: usize = 8;
const FPSIMD_MAGIC: u32 = 0x4650_8001;
/// `fpsimd_context`: the header, `fpsr`, `fpcr`, then `v0` to `v31`.
const FPSIMD_SIZE: usize = 16 + 32 * 16;
const ESR_MAGIC: u32 = 0x4553_5201;
/// `esr_context`: the header, then the syndrome.
const ESR_SIZE: usize = 16;

/// `pstate`'s fields that a thread in user code never has otherwise, which
/// a frame's must not have either: another exception level or stack
/// pointer (`M[3:0]`), AArch32 (`M[4]`), and masked exceptions (`DAIF`).
const PSTATE_PRIVILEGED: u64 = 0xF | 0x10 | 0x3C0;

/// The floating-point and SIMD registers: `v0` to `v31`, then `FPSR` and
/// `FPCR` (32 bits each, as Linux's record has them, though the registers
/// are 64 bits wide: the rest is reserved, 0).
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fpsimd {
    pub v: [u128; 32],
    pub fpsr: u32,
    pub fpcr: u32,
}

const _: () = assert!(size_of::<Fpsimd>() == 528);

/// What a frame keeps of the code a signal interrupted, and says of the
/// signal.
pub struct Interrupted<'a> {
    /// The registers as the code left them (its thread pointer is no part
    /// of the frame).
    pub registers: &'a ThreadState,
    pub vector: &'a Fpsimd,
    /// The mask the thread goes back to.
    pub mask: u64,
    /// The alternate stack as the context reports it: `ss_sp`, `ss_flags`,
    /// `ss_size`.
    pub stack: (u64, i32, u64),
    /// What the signal says (`siginfo_t`).
    pub info: &'a [u8; 128],
    /// The fault that raised the signal, if one did: its syndrome (0: none
    /// to report) and the address the context reports.
    pub fault: Option<(u64, u64)>,
}

/// What `rt_sigreturn` takes back from a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TakenBack {
    /// `x0` to `x30`, `sp`, `pc` and `pstate` (the thread pointer is not
    /// the frame's: 0).
    pub registers: ThreadState,
    pub vector: Fpsimd,
    pub mask: u64,
    /// The alternate stack to restore: `ss_sp`, `ss_flags`, `ss_size`.
    pub stack: (u64, i32, u64),
}

fn put(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn get(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn get32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// The frame of `i`, its frame record after it: the bytes to put where the
/// handler's stack pointer will be.
pub fn lay_out(i: &Interrupted) -> [u8; SIZE + RECORD] {
    let mut b = [0u8; SIZE + RECORD];
    b[INFO..INFO + 128].copy_from_slice(i.info);
    put(&mut b, UC_FLAGS, 0);
    put(&mut b, UC_LINK, 0);
    let (ss_sp, ss_flags, ss_size) = i.stack;
    put(&mut b, UC_STACK, ss_sp);
    put32(&mut b, UC_STACK + 8, ss_flags as u32);
    put(&mut b, UC_STACK + 16, ss_size);
    put(&mut b, UC_SIGMASK, i.mask);
    let r = i.registers;
    put(&mut b, FAULT_ADDRESS, i.fault.map_or(0, |(_, address)| address));
    for (n, x) in r.x.iter().enumerate() {
        put(&mut b, REGS + 8 * n, *x);
    }
    put(&mut b, SP, r.sp);
    put(&mut b, PC, r.pc);
    put(&mut b, PSTATE, r.pstate);
    // The records: the vector registers, then the syndrome of a fault, then
    // the end (zeros).
    let mut at = RESERVED;
    put32(&mut b, at, FPSIMD_MAGIC);
    put32(&mut b, at + 4, FPSIMD_SIZE as u32);
    put32(&mut b, at + 8, i.vector.fpsr);
    put32(&mut b, at + 12, i.vector.fpcr);
    for (n, v) in i.vector.v.iter().enumerate() {
        b[at + 16 + 16 * n..at + 32 + 16 * n].copy_from_slice(&v.to_le_bytes());
    }
    at += FPSIMD_SIZE;
    if let Some((syndrome, _)) = i.fault.filter(|&(syndrome, _)| syndrome != 0) {
        put32(&mut b, at, ESR_MAGIC);
        put32(&mut b, at + 4, ESR_SIZE as u32);
        put(&mut b, at + 8, syndrome);
    }
    put(&mut b, SIZE, r.x[29]);
    put(&mut b, SIZE + 8, r.x[30]);
    b
}

/// What the frame `b` (as the handler leaves it) has the thread go back
/// to: `None` for one Linux refuses, whose `rt_sigreturn` is `SIGSEGV`.
/// It refuses a `pstate` user code cannot have, records that are not
/// 16-byte aligned or run past the frame, a record of none of these, or
/// none of the vector registers (or two, or one of another size). An
/// `esr_context` is taken as it is, and changes nothing.
pub fn take_back(b: &[u8; SIZE]) -> Option<TakenBack> {
    let mut registers = ThreadState::default();
    for (n, x) in registers.x.iter_mut().enumerate() {
        *x = get(b, REGS + 8 * n);
    }
    registers.sp = get(b, SP);
    registers.pc = get(b, PC);
    registers.pstate = get(b, PSTATE);
    if registers.pstate & PSTATE_PRIVILEGED != 0 {
        return None;
    }
    let mut vector = None;
    let mut at = 0;
    loop {
        if RESERVED_SIZE - at < HEADER || !at.is_multiple_of(16) {
            return None;
        }
        let (magic, size) = (get32(b, RESERVED + at), get32(b, RESERVED + at + 4) as usize);
        if RESERVED_SIZE - at < size {
            return None;
        }
        match magic {
            0 if size == 0 => break,
            FPSIMD_MAGIC if vector.is_none() && size == FPSIMD_SIZE => {
                let record = RESERVED + at;
                let mut v = Fpsimd { fpsr: get32(b, record + 8), fpcr: get32(b, record + 12), ..Fpsimd::default() };
                for (n, reg) in v.v.iter_mut().enumerate() {
                    let from = record + 16 + 16 * n;
                    *reg = u128::from_le_bytes(b[from..from + 16].try_into().unwrap());
                }
                vector = Some(v);
            }
            ESR_MAGIC => {}
            _ => return None,
        }
        if size < HEADER {
            return None;
        }
        at += size;
    }
    let stack = (get(b, UC_STACK), get32(b, UC_STACK + 8) as i32, get(b, UC_STACK + 16));
    Some(TakenBack { registers, vector: vector?, mask: get(b, UC_SIGMASK), stack })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Registers and vector registers that are each other's, and a fault.
    fn interrupted() -> (ThreadState, Fpsimd, [u8; 128]) {
        let mut r = ThreadState { sp: 0x7fff_0000, pc: 0x40_1000, pstate: 0x6000_0000, tpidr: 0x5555, x: [0; 31] };
        for (n, x) in r.x.iter_mut().enumerate() {
            *x = 0x100 + n as u64;
        }
        let mut v = Fpsimd { fpsr: 0x0800_0010, fpcr: 0x0300_0000, ..Fpsimd::default() };
        for (n, reg) in v.v.iter_mut().enumerate() {
            *reg = (n as u128) << 64 | (0xa000 + n as u128);
        }
        let mut info = [0u8; 128];
        info[0] = 11;
        (r, v, info)
    }

    fn frame(fault: Option<(u64, u64)>) -> [u8; SIZE + RECORD] {
        let (r, v, info) = interrupted();
        let i = Interrupted {
            registers: &r,
            vector: &v,
            mask: 0x8000_0001,
            stack: (0x9000, 4, 0x2000),
            info: &info,
            fault,
        };
        lay_out(&i)
    }

    fn frame_only(b: &[u8; SIZE + RECORD]) -> [u8; SIZE] {
        b[..SIZE].try_into().unwrap()
    }

    #[test]
    fn the_frame_is_linux_s() {
        let b = frame(Some((0x9200_0046, 0x7000)));
        assert_eq!(b[0], 11);
        // `ucontext`: no flags or link, the signal stack, the mask.
        assert_eq!((get(&b, 128), get(&b, 136)), (0, 0));
        assert_eq!((get(&b, 144), get32(&b, 152), get(&b, 160)), (0x9000, 4, 0x2000));
        assert_eq!(get(&b, 168), 0x8000_0001);
        // `sigcontext`: the fault's address, the registers, `sp`, `pc`,
        // `pstate`.
        assert_eq!(get(&b, 304), 0x7000);
        assert_eq!((get(&b, 312), get(&b, 312 + 8 * 30)), (0x100, 0x100 + 30));
        assert_eq!((get(&b, 560), get(&b, 568), get(&b, 576)), (0x7fff_0000, 0x40_1000, 0x6000_0000));
        // The records, from 592: the vector registers, the syndrome, the end.
        assert_eq!((get32(&b, 592), get32(&b, 596)), (0x4650_8001, 528));
        assert_eq!((get32(&b, 600), get32(&b, 604)), (0x0800_0010, 0x0300_0000));
        assert_eq!((get(&b, 608), get(&b, 616)), (0xa000, 0));
        assert_eq!((get(&b, 608 + 16 * 31), get(&b, 616 + 16 * 31)), (0xa000 + 31, 31));
        assert_eq!((get32(&b, 1120), get32(&b, 1124), get(&b, 1128)), (0x4553_5201, 16, 0x9200_0046));
        assert_eq!(get(&b, 1136), 0);
        // The frame record: the code's frame pointer and link register.
        assert_eq!((get(&b, 4688), get(&b, 4696)), (0x100 + 29, 0x100 + 30));
        // A signal no fault raised has no syndrome to report.
        let b = frame(None);
        assert_eq!((get(&b, 304), get(&b, 1120)), (0, 0));
    }

    #[test]
    fn a_frame_gives_back_what_it_kept() {
        let (r, v, _) = interrupted();
        let back = take_back(&frame_only(&frame(Some((0x9200_0046, 0x7000))))).unwrap();
        assert_eq!(back.registers, ThreadState { tpidr: 0, ..r });
        assert_eq!(back.vector, v);
        assert_eq!((back.mask, back.stack), (0x8000_0001, (0x9000, 4, 0x2000)));
        // As a handler changed it.
        let mut b = frame_only(&frame(None));
        put(&mut b, PC, 0x40_1004);
        put(&mut b, REGS, 7);
        put32(&mut b, RESERVED + 12, 0x0040_0000);
        let back = take_back(&b).unwrap();
        assert_eq!((back.registers.pc, back.registers.x[0], back.vector.fpcr), (0x40_1004, 7, 0x0040_0000));
    }

    #[test]
    fn a_frame_linux_refuses_is_refused() {
        let good = frame_only(&frame(Some((0x9200_0046, 0x7000))));
        let changed = |at: usize, v: u32| {
            let mut b = good;
            put32(&mut b, at, v);
            take_back(&b)
        };
        // Another exception level, AArch32, masked interrupts.
        for pstate in [0x4, 0x5, 0x10, 0x80, 0x3C0] {
            assert_eq!(changed(PSTATE, pstate), None, "{pstate:#x}");
        }
        // The flags user code changes are its own.
        assert!(changed(PSTATE, 0xF000_0000).is_some());
        // No vector registers, or theirs of another size.
        assert_eq!(changed(RESERVED, 0), None);
        assert_eq!(changed(RESERVED + 4, 512), None);
        // A record no one knows, one that runs past the frame, one that
        // leaves the next misaligned, and a terminator with a size.
        assert_eq!(changed(1120, 0x5356_4501), None);
        assert_eq!(changed(1124, 4096), None);
        assert_eq!(changed(1124, 20), None);
        assert_eq!(changed(1124, 4), None);
        assert_eq!(changed(1136 + 4, 16), None);
        // Two of the vector registers.
        let mut b = good;
        b.copy_within(RESERVED..RESERVED + FPSIMD_SIZE, 1120);
        put(&mut b, 1120 + FPSIMD_SIZE, 0);
        assert_eq!(take_back(&b), None);
        // Records that fill the space, with no end, are refused; ending
        // right at its end is no end either.
        let mut b = good;
        for at in (1120..SIZE).step_by(ESR_SIZE) {
            put32(&mut b, at, ESR_MAGIC);
            put32(&mut b, at + 4, ESR_SIZE as u32);
        }
        assert_eq!(take_back(&b), None);
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Traps: the registers a trap saves ([`TrapFrame`]), what the rest of the
//! kernel learns from them (a system call's arguments, a page fault), and
//! the exceptions the kernel does not survive (its own).

use core::fmt::{self, Display};

use crate::diagnostics::log::emergency;

/// Exception classes (`ESR_ELx.EC`).
pub(in crate::architecture) const EC_SVC: u64 = 0x15;
const EC_INSTRUCTION_ABORT_LOWER: u64 = 0x20;
const EC_INSTRUCTION_ABORT: u64 = 0x21;
const EC_DATA_ABORT_LOWER: u64 = 0x24;
const EC_DATA_ABORT: u64 = 0x25;
const EC_PC_ALIGNMENT: u64 = 0x22;
const EC_WATCHPOINT_LOWER: u64 = 0x34;
const EC_WATCHPOINT: u64 = 0x35;

/// `ESR_ELx.ISS` of an abort: the access was a write; and it was a cache
/// operation, which is reported as a write and reads as much as it does.
const ISS_WRITE: u64 = 1 << 6;
const ISS_CACHE: u64 = 1 << 8;
/// A translation fault of any level (the page is not there) and a
/// permission fault (it is, but not for this access), in the status code.
const FSC_TRANSLATION: u64 = 0b000100;
const FSC_PERMISSION: u64 = 0b001100;

/// `SPSR_ELx`'s `M`: where the trap came from, 0 for EL0.
const SPSR_LEVEL: u64 = 0xF;

/// Register state saved on kernel entry, in the entry code's layout.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TrapFrame {
    /// `x0` to `x30`.
    pub(in crate::architecture) x: [u64; 31],
    /// The stack pointer of the code trapped.
    pub(in crate::architecture) sp: u64,
    pub(in crate::architecture) pc: u64,
    pub(in crate::architecture) pstate: u64,
    pub(in crate::architecture) esr: u64,
    pub(in crate::architecture) far: u64,
}

const _: () = assert!(core::mem::size_of::<TrapFrame>() == super::entry::FRAME_SIZE);

/// A page fault: the address, and the access that faulted.
#[derive(Debug, Clone, Copy)]
pub struct PageFault {
    pub address: u64,
    pub write: bool,
    pub execute: bool,
    /// The page is mapped, but not for this access.
    pub present: bool,
}

impl TrapFrame {
    pub(in crate::architecture) fn is_user(&self) -> bool {
        self.pstate & SPSR_LEVEL == 0
    }

    /// The exception's class.
    pub(in crate::architecture) fn class(&self) -> u64 {
        self.esr >> 26 & 0x3F
    }

    /// A frame that enters user mode at `pc` with stack `sp`.
    pub(in crate::architecture) fn new_user(pc: u64, sp: u64, arg0: u64, arg1: u64) -> TrapFrame {
        let mut frame = TrapFrame { pc, sp, ..TrapFrame::default() };
        frame.x[0] = arg0;
        frame.x[1] = arg1;
        frame
    }

    /// The system call's number and arguments (see `abi`'s calling
    /// convention).
    pub fn syscall(&self) -> (usize, [usize; 6]) {
        let x = &self.x;
        (x[8] as usize, [x[0], x[1], x[2], x[3], x[4], x[5]].map(|v| v as usize))
    }

    /// Returns `value` from the system call, and `second` from those that
    /// return two.
    pub fn set_syscall_result(&mut self, value: u64, second: Option<u64>) {
        self.x[0] = value;
        if let Some(s) = second {
            self.x[1] = s;
        }
    }

    /// Has the thread run its process's signal entry `entry` as it returns
    /// to user code (see `abi::architecture`'s "Signal entry"): its user
    /// registers as they are, with its thread pointer `tls`, go on its
    /// stack, whose address it starts with. `Fault` if its stack cannot
    /// take them (it is left as it was).
    pub fn divert(&mut self, entry: u64, tls: u64) -> Result<(), abi::Error> {
        let state = self.registers(tls);
        let at = abi::architecture::signal_frame(self.sp as usize);
        crate::memory::user::write(at as u64, &state)?;
        self.pc = entry;
        self.sp = abi::entry_stack_pointer(at) as u64;
        self.x[0] = at as u64;
        self.pstate = 0;
        Ok(())
    }

    /// The user registers trapped, with the thread pointer `tls`.
    fn registers(&self, tls: u64) -> abi::ThreadState {
        abi::ThreadState { x: self.x, sp: self.sp, pc: self.pc, pstate: self.pstate, tpidr: tls }
    }

    /// What foreign code trapped here leaves its host
    /// (`scheduler::foreign`): its registers, with its thread pointer
    /// `tls`, and if it took an `exception`, which and where.
    pub fn foreign_state(&self, tls: u64, exception: bool) -> abi::ForeignState {
        let mut state = abi::ForeignState { registers: self.registers(tls), ..abi::ForeignState::default() };
        if exception {
            state.syndrome = self.esr;
            state.fault_address = if self.far_is_valid() { self.far } else { 0 };
        }
        state
    }

    /// Whether the fault address register holds this exception's address:
    /// for an abort that gives one (no `FnV`), a misaligned pc and a
    /// watchpoint. For any other it holds what an earlier exception on the
    /// processor left there, perhaps another process's.
    fn far_is_valid(&self) -> bool {
        /// The syndrome's "FAR not valid" bit, of an abort.
        const FNV: u64 = 1 << 10;
        match self.class() {
            EC_INSTRUCTION_ABORT_LOWER | EC_INSTRUCTION_ABORT | EC_DATA_ABORT_LOWER | EC_DATA_ABORT => {
                self.esr & FNV == 0
            }
            EC_PC_ALIGNMENT | EC_WATCHPOINT_LOWER | EC_WATCHPOINT => true,
            _ => false,
        }
    }

    /// Resumes user registers `state` (`signal_return`), but its thread
    /// pointer: of the flags, those user code may change
    /// (`abi::architecture::SIGNAL_RETURN_FLAGS`). `InvalidArgs` for an
    /// instruction or stack pointer outside user space.
    pub fn resume(&mut self, state: &abi::ThreadState) -> Result<(), abi::Error> {
        let user = abi::USER_SPACE_START as u64..abi::USER_SPACE_END as u64;
        if !user.contains(&state.pc) || !user.contains(&state.sp) {
            return Err(abi::Error::InvalidArgs);
        }
        self.x = state.x;
        self.sp = state.sp;
        self.pc = state.pc;
        self.pstate = abi::architecture::resumed_flags(state.pstate);
        Ok(())
    }

    /// The page fault this trap is, if it is one.
    pub fn page_fault(&self) -> Option<PageFault> {
        let class = self.class();
        let execute = matches!(class, EC_INSTRUCTION_ABORT_LOWER | EC_INSTRUCTION_ABORT);
        if !execute && !matches!(class, EC_DATA_ABORT_LOWER | EC_DATA_ABORT) {
            return None;
        }
        let status = self.esr & 0b11_1100;
        (status == FSC_TRANSLATION || status == FSC_PERMISSION).then_some(PageFault {
            address: self.far,
            write: !execute && self.esr & ISS_WRITE != 0 && self.esr & ISS_CACHE == 0,
            execute,
            present: status == FSC_PERMISSION,
        })
    }

    /// The address of the trapped code (its instruction pointer).
    pub fn code_address(&self) -> u64 {
        self.pc
    }

    /// Where the trapped code was, for the log.
    pub fn location(&self) -> impl Display {
        let pc = self.pc;
        fmt::from_fn(move |f| write!(f, "pc {:#x}", pc))
    }

    /// The exception this trap is and where it happened, for the log.
    pub fn describe(&self) -> impl Display {
        let name = class_name(self.class());
        let (esr, far, pc, sp) = (self.esr, self.far, self.pc, self.sp);
        fmt::from_fn(move |f| {
            write!(f, "{} (syndrome {:#x}, address {:#x}) at pc {:#x}, sp {:#x}", name, esr, far, pc, sp)
        })
    }
}

/// What an exception class means, for the log.
fn class_name(class: u64) -> &'static str {
    match class {
        0x00 => "Undefined instruction",
        0x07 => "Trapped SIMD or floating-point access",
        0x0E => "Illegal execution state",
        EC_SVC => "System call",
        0x18 => "Trapped system register access",
        0x19 => "Trapped SVE access",
        EC_INSTRUCTION_ABORT_LOWER | EC_INSTRUCTION_ABORT => "Instruction abort",
        0x22 => "Misaligned instruction",
        EC_DATA_ABORT_LOWER | EC_DATA_ABORT => "Data abort",
        0x26 => "Misaligned stack pointer",
        0x2C => "SIMD floating-point exception",
        0x2F => "System error",
        0x3C => "Breakpoint",
        _ => "Unknown exception",
    }
}

fn dump(frame: &TrapFrame) {
    emergency(format_args!(
        "  pc {:#018x}  sp {:#018x}  pstate {:#x}\n  esr {:#x}  far {:#x}\n",
        frame.pc, frame.sp, frame.pstate, frame.esr, frame.far
    ));
    for (i, row) in frame.x.chunks(3).enumerate() {
        emergency(format_args!("  x{:<2}", i * 3));
        for v in row {
            emergency(format_args!(" {:#018x}", v));
        }
        emergency(format_args!("\n"));
    }
    // Walk the frame-pointer chain for a crude backtrace.
    emergency(format_args!("  backtrace:"));
    let mut fp = frame.x[29];
    for _ in 0..16 {
        if fp < 0xFFFF_8000_0000_0000 || !fp.is_multiple_of(16) {
            break;
        }
        // SAFETY: best effort on a kernel stack; a bad chain only ends the walk
        // early because we check the address range first.
        let (next, ret) = unsafe { (*(fp as *const u64), *((fp + 8) as *const u64)) };
        emergency(format_args!(" {:#x}", ret));
        if next <= fp {
            break;
        }
        fp = next;
    }
    emergency(format_args!("\n"));
}

/// An exception raised by kernel code.
pub(in crate::architecture) fn kernel_exception(frame: &TrapFrame) -> ! {
    crate::diagnostics::panic::begin();
    emergency(format_args!(
        "\n\n*** VEDA KERNEL PANIC: {} in kernel mode on CPU {} pc={:#x} far={:#x} sp={:#x} esr={:#x} ***\n",
        class_name(frame.class()),
        crate::scheduler::percpu::cpu_id_or_boot(),
        frame.pc,
        frame.far,
        frame.sp,
        frame.esr
    ));
    if let Some(fault) = frame.page_fault() {
        let addr = fault.address;
        let what = if crate::memory::kvirt::is_stack_guard(addr) { " (kernel stack overflow?)" } else { "" };
        emergency(format_args!("  faulting address {:#x}{}\n", addr, what));
    }
    dump(frame);
    crate::diagnostics::panic::end()
}

/// An exception the kernel cannot have provoked or survive: a system error
/// (an asynchronous abort), or one through a vector the kernel never uses.
pub(in crate::architecture) fn fatal(frame: &TrapFrame, what: &str) -> ! {
    if crate::diagnostics::panic::panicking() {
        super::cpu::halt_forever();
    }
    crate::diagnostics::panic::begin();
    emergency(format_args!("\n\n*** VEDA KERNEL PANIC: {} ***\n", what));
    dump(frame);
    crate::diagnostics::panic::end()
}

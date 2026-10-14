// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Traps: the registers a trap saves ([`TrapFrame`]), what the rest of the
//! kernel learns from them (a system call's arguments, a page fault), and
//! the exceptions the kernel does not survive (its own, and NMIs).

use core::fmt::{self, Display};

use super::cpu;
use super::gdt::{USER_CS, USER_DS};
use super::idt::EXCEPTION_NAMES;
use crate::log::emergency;

/// Vector number recorded in the trap frame of a system call.
pub(in crate::arch) const SYSCALL_VECTOR: u64 = 0x100;

/// The page-fault exception's vector.
const PAGE_FAULT: u64 = 14;

/// Register state saved on kernel entry. Field order mirrors the push order
/// in the entry code (lowest address first).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TrapFrame {
    pub(in crate::arch) r15: u64,
    pub(in crate::arch) r14: u64,
    pub(in crate::arch) r13: u64,
    pub(in crate::arch) r12: u64,
    pub(in crate::arch) r11: u64,
    pub(in crate::arch) r10: u64,
    pub(in crate::arch) r9: u64,
    pub(in crate::arch) r8: u64,
    pub(in crate::arch) rbp: u64,
    pub(in crate::arch) rdi: u64,
    pub(in crate::arch) rsi: u64,
    pub(in crate::arch) rdx: u64,
    pub(in crate::arch) rcx: u64,
    pub(in crate::arch) rbx: u64,
    pub(in crate::arch) rax: u64,
    pub(in crate::arch) vector: u64,
    pub(in crate::arch) error_code: u64,
    pub(in crate::arch) rip: u64,
    pub(in crate::arch) cs: u64,
    pub(in crate::arch) rflags: u64,
    pub(in crate::arch) rsp: u64,
    pub(in crate::arch) ss: u64,
}

const _: () = assert!(core::mem::size_of::<TrapFrame>() == 176);

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
    pub(in crate::arch) fn is_user(&self) -> bool {
        self.cs & 3 == 3
    }

    /// A frame that enters user mode at `rip` with stack `rsp`.
    pub(in crate::arch) fn new_user(rip: u64, rsp: u64, arg0: u64, arg1: u64) -> TrapFrame {
        TrapFrame {
            rip,
            rsp,
            rdi: arg0,
            rsi: arg1,
            cs: USER_CS as u64,
            ss: USER_DS as u64,
            rflags: 0x202, // IF
            ..TrapFrame::default()
        }
    }

    /// The system call's number and arguments (see `vabi`'s calling
    /// convention).
    pub fn syscall(&self) -> (usize, [usize; 6]) {
        (self.rax as usize, [self.rdi, self.rsi, self.rdx, self.r10, self.r8, self.r9].map(|x| x as usize))
    }

    /// Returns `value` from the system call, and `second` from those that
    /// return two.
    pub fn set_syscall_result(&mut self, value: u64, second: Option<u64>) {
        self.rax = value;
        if let Some(s) = second {
            self.rdx = s;
        }
    }

    /// The page fault this trap is, if it is one.
    pub fn page_fault(&self) -> Option<PageFault> {
        (self.vector == PAGE_FAULT).then(|| PageFault {
            address: cpu::read_cr2(),
            write: self.error_code & 2 != 0,
            execute: self.error_code & 16 != 0,
            present: self.error_code & 1 != 0,
        })
    }

    /// The address of the trapped code (its instruction pointer).
    pub fn code_address(&self) -> u64 {
        self.rip
    }

    /// Where the trapped code was, for the log.
    pub fn location(&self) -> impl Display {
        let rip = self.rip;
        fmt::from_fn(move |f| write!(f, "rip {:#x}", rip))
    }

    /// The exception this trap is and where it happened, for the log.
    pub fn describe(&self) -> impl Display {
        let name = EXCEPTION_NAMES.get(self.vector as usize).copied().unwrap_or("?");
        let (error, rip, rsp) = (self.error_code, self.rip, self.rsp);
        fmt::from_fn(move |f| write!(f, "{} (error {:#x}) at rip {:#x}, rsp {:#x}", name, error, rip, rsp))
    }
}

fn dump(frame: &TrapFrame) {
    emergency(format_args!(
        "  rip {:#018x}  rsp {:#018x}  rflags {:#x}\n  cs {:#x}  ss {:#x}  error {:#x}\n",
        frame.rip, frame.rsp, frame.rflags, frame.cs, frame.ss, frame.error_code
    ));
    emergency(format_args!(
        "  rax {:#018x} rbx {:#018x} rcx {:#018x} rdx {:#018x}\n  rsi {:#018x} rdi {:#018x} rbp {:#018x}\n",
        frame.rax, frame.rbx, frame.rcx, frame.rdx, frame.rsi, frame.rdi, frame.rbp
    ));
    emergency(format_args!(
        "  r8  {:#018x} r9  {:#018x} r10 {:#018x} r11 {:#018x}\n  r12 {:#018x} r13 {:#018x} r14 {:#018x} r15 {:#018x}\n",
        frame.r8, frame.r9, frame.r10, frame.r11, frame.r12, frame.r13, frame.r14, frame.r15
    ));
    // Walk the frame-pointer chain for a crude backtrace.
    emergency(format_args!("  backtrace:"));
    let mut rbp = frame.rbp;
    for _ in 0..16 {
        if rbp < 0xFFFF_8000_0000_0000 || !rbp.is_multiple_of(8) {
            break;
        }
        // SAFETY: best effort on a kernel stack; a bad chain only ends the walk
        // early because we check the address range first.
        let (next, ret) = unsafe { (*(rbp as *const u64), *((rbp + 8) as *const u64)) };
        emergency(format_args!(" {:#x}", ret));
        if next <= rbp {
            break;
        }
        rbp = next;
    }
    emergency(format_args!("\n"));
}

/// A CPU exception raised by kernel code.
pub(in crate::arch) fn kernel_exception(frame: &TrapFrame) -> ! {
    crate::panic::begin();
    let v = frame.vector as usize;
    emergency(format_args!(
        "\n\n*** VEDA KERNEL PANIC: {} (vector {}) in kernel mode on CPU {} rip={:#x} cr2={:#x} rsp={:#x} err={:#x} ***\n",
        EXCEPTION_NAMES.get(v).copied().unwrap_or("?"),
        v,
        crate::percpu::cpu_id_or_boot(),
        frame.rip,
        cpu::read_cr2(),
        frame.rsp,
        frame.error_code
    ));
    if let Some(fault) = frame.page_fault() {
        let addr = fault.address;
        let what = if crate::mm::kvirt::is_stack_guard(addr) { " (kernel stack overflow?)" } else { "" };
        emergency(format_args!("  faulting address {:#x}{}\n", addr, what));
    }
    dump(frame);
    crate::panic::end()
}

/// Non-maskable interrupt: only expected as part of a panic.
pub(in crate::arch) fn nmi(frame: &TrapFrame) -> ! {
    if crate::panic::panicking() {
        cpu::halt_forever();
    }
    crate::panic::begin();
    emergency(format_args!("\n\n*** VEDA KERNEL PANIC: unexpected NMI ***\n"));
    dump(frame);
    crate::panic::end()
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Virtual processors: a guest's processors, run at EL1 under the kernel at
//! EL2.
//!
//! A thread of the virtual machine monitor runs a virtual processor with
//! `vcpu_run`: the kernel enters the guest and handles what the guest does
//! that the processor does not let it do alone, until something needs the
//! monitor (a hypercall, memory that is not mapped). The kernel handles
//! itself what must be fast or is the processor's: `wfi`, and the interrupt
//! controller: the interrupts it raises through the list registers, the
//! registers of its distributor and redistributors (`hv::arm64::gic`),
//! which nothing maps, so that reaching them is a fault that says what it
//! wanted, and the SGIs one processor sends another.
//!
//! The guest runs with the BKL released, as user code does; any exit takes
//! it again. Interrupts of the host make the guest exit (they stay pending
//! and are taken right after); the CPU leaves the guest for the scheduler
//! as it would leave user code. Another thread that raises an interrupt
//! for a virtual processor in guest mode on another CPU makes that CPU
//! leave it ([`Vcpu::kick`]); one waiting in its `wfi` is woken.
//!
//! The guest's EL1 registers are the processor's, under the names the
//! host reaches them by (`*_EL12`), only while a thread runs it: on the way
//! out of the kernel's loop (to the monitor, or to the scheduler) they, its
//! timer and its floating point go, so a virtual processor may run on any
//! CPU next. Its translations stay in every TLB, where its guest's own
//! invalidations reach them (`HCR_EL2.FB`).
//!
//! The virtual timer is the guest's: it reads and arms it without the
//! kernel. When it fires, it makes the guest exit, the kernel masks it,
//! which the guest does anyway, and raises its interrupt. While the guest
//! waits off the CPU, its thread sleeps until the timer's deadline.

use alloc::sync::{Arc, Weak};
use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use abi::arm64::vcpu_exit;
use abi::{Error, VcpuExit, VcpuState};
use bootinfo::architecture::HCR_HOST;
use hv::arm64::features;
use hv::arm64::gic::{self, Access, INTIDS, Private, Registers};
use hv::arm64::memory::is_gic;
use hv::arm64::platform::{affinity_of, is_guest_pstate};

use super::super::cpu::{self, read_register, write_register};
use super::super::thread::FpuState;
use super::super::{entry, gic as host_gic, percpu};
use super::{MAINTENANCE_PPI, VIRTUAL_TIMER_PPI, active_priority_registers, list_registers};
use crate::interrupt::Interrupt;
use crate::scheduler::{self, Thread, WakeReason};
use crate::synchronization::{SpinLock, bkl};
use crate::virtualization::Guest;

/// `HCR_EL2` while a guest runs: its stage 2 translation (`VM`), with its
/// tables' walks kept off devices (`PTW`), its physical interrupts and
/// system errors the kernel's (`FMO`, `IMO`, `AMO`, which also give it the
/// virtual interface), its invalidation of caches by set and way cleaning
/// them too (`SWIO`), its TLB and cache maintenance broadcast (`FB`, `BSU`).
/// Trapped: `wfi` and `smc` (`TWI`, `TSC`), the ID registers (`TID3`), and
/// whatever differs from one processor to the next or belongs to features
/// it has not been shown (`TIDCP`, `TACR`, `TLOR`, `TERR`). 64-bit, on the
/// host's VHE (`RW`, `E2H`).
const HCR_GUEST: u64 = 1 << 0
    | 1 << 1
    | 1 << 2
    | 1 << 3
    | 1 << 4
    | 1 << 5
    | 1 << 9
    | 1 << 10
    | 1 << 13
    | 1 << 18
    | 1 << 19
    | 1 << 20
    | 1 << 21
    | 1 << 31
    | 1 << 34
    | 1 << 35
    | 1 << 36;

/// `SCTLR_EL1` of a processor that has just reset, as far as a guest can
/// tell: its MMU and caches off, and what the architecture wants as 1.
const SCTLR_RESET: u64 = 0x30D0_0800;

/// Exception classes in `ESR_EL2`.
const EC_WFI: u64 = 0x01;
const EC_HVC: u64 = 0x16;
const EC_SMC: u64 = 0x17;
const EC_SYSTEM_REGISTER: u64 = 0x18;
const EC_INSTRUCTION_ABORT: u64 = 0x20;
const EC_DATA_ABORT: u64 = 0x24;

/// A data abort's syndrome: the rest of it is valid, how many bytes (a
/// power of two), which register, and whether it was a write.
const ISS_VALID: u64 = 1 << 24;
const ISS_SIZE: u32 = 22;
const ISS_REGISTER: u32 = 16;
const ISS_WRITE: u64 = 1 << 6;
/// An abort's syndrome: it came of a stage 1 table walk; and the kind of
/// fault, here a translation fault of any level.
const ISS_TABLE_WALK: u64 = 1 << 7;
const ISS_FAULT_KIND: u64 = 0x3C;
const ISS_TRANSLATION_FAULT: u64 = 0x04;

/// What brought the guest back (`vk_guest_run`).
const LEFT_SYNCHRONOUS: u64 = 0;
const LEFT_INTERRUPT: u64 = 1;
const LEFT_SYSTEM_ERROR: u64 = 2;

/// A list register: pending, in group 1, at Linux's priority; end of
/// interrupt reported (for what has to know).
const LR_PENDING: u64 = 1 << 62;
const LR_GROUP1: u64 = 1 << 60;
const LR_PRIORITY: u64 = 0xA0 << 48;
const LR_EOI: u64 = 1 << 41;
/// `ICH_LR`'s state: 0 is free.
const LR_STATE: u64 = 3 << 62;
const LR_INTID: u64 = 0xFFFF_FFFF;

/// `ICH_HCR_EL2`: the virtual interface is on; a maintenance interrupt
/// when few list registers are left, to fill them again.
const ICH_ENABLE: u64 = 1 << 0;
const ICH_UNDERFLOW: u64 = 1 << 1;

/// The virtual timer's `CTL`: on, masked, and fired.
const CTL_ENABLE: u64 = 1 << 0;
const CTL_MASKED: u64 = 1 << 1;
const CTL_FIRED: u64 = 1 << 2;

/// The most list registers a processor has.
const MAX_LIST_REGISTERS: usize = 16;

/// The most active priority registers a processor has.
const MAX_ACTIVE_PRIORITY_REGISTERS: usize = 4;

/// A guest's general registers and those that go with them, as
/// `vk_guest_run` takes and leaves them.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct GuestRegs {
    /// `x0` to `x30`.
    x: [u64; 31],
    sp_el0: u64,
    sp_el1: u64,
    pc: u64,
    pstate: u64,
    tpidr_el1: u64,
}

const _: () = assert!(core::mem::size_of::<GuestRegs>() == 288);

unsafe extern "C" {
    /// Runs the guest until something brings it back: what, among
    /// `LEFT_*`. With interrupts masked, the processor set up for the guest.
    fn vk_guest_run(regs: *mut GuestRegs) -> u64;
    static vk_guest_vectors: u8;
}

// The frame `vk_guest_run` keeps on the kernel's stack, which is where
// exceptions from the guest find it, EL2's stack pointer being its own: the
// host's callee-saved registers, then the registers' address and the host's
// `TPIDR_EL1`, which a guest has for its own use. Everything else the host
// has of EL2 the guest cannot touch. `TPIDR_EL2` is not needed: the frame
// says all.
global_asm!(
    r#"
    .macro vk_guest_exit kind
        .balign 128
        stp     x0, x1, [sp, #-16]!
        mov     x0, #\kind
        b       vk_guest_left
    .endm

    .section .text
    .balign 16
    .globl vk_guest_run
vk_guest_run:
    sub     sp, sp, #112
    stp     x19, x20, [sp, #0]
    stp     x21, x22, [sp, #16]
    stp     x23, x24, [sp, #32]
    stp     x25, x26, [sp, #48]
    stp     x27, x28, [sp, #64]
    stp     x29, x30, [sp, #80]
    mrs     x1, tpidr_el1
    stp     x0, x1, [sp, #96]
    ldp     x1, x2, [x0, #248]
    msr     sp_el0, x1
    msr     sp_el1, x2
    ldp     x1, x2, [x0, #264]
    msr     elr_el1, x1
    msr     spsr_el1, x2
    ldr     x1, [x0, #280]
    msr     tpidr_el1, x1
    ldp     x2, x3, [x0, #16]
    ldp     x4, x5, [x0, #32]
    ldp     x6, x7, [x0, #48]
    ldp     x8, x9, [x0, #64]
    ldp     x10, x11, [x0, #80]
    ldp     x12, x13, [x0, #96]
    ldp     x14, x15, [x0, #112]
    ldp     x16, x17, [x0, #128]
    ldp     x18, x19, [x0, #144]
    ldp     x20, x21, [x0, #160]
    ldp     x22, x23, [x0, #176]
    ldp     x24, x25, [x0, #192]
    ldp     x26, x27, [x0, #208]
    ldp     x28, x29, [x0, #224]
    ldr     x30, [x0, #240]
    ldp     x0, x1, [x0, #0]
    eret
    dsb     nsh
    isb

    .balign 16
vk_guest_left:
    ldr     x1, [sp, #112]
    stp     x2, x3, [x1, #16]
    stp     x4, x5, [x1, #32]
    stp     x6, x7, [x1, #48]
    stp     x8, x9, [x1, #64]
    stp     x10, x11, [x1, #80]
    stp     x12, x13, [x1, #96]
    stp     x14, x15, [x1, #112]
    stp     x16, x17, [x1, #128]
    stp     x18, x19, [x1, #144]
    stp     x20, x21, [x1, #160]
    stp     x22, x23, [x1, #176]
    stp     x24, x25, [x1, #192]
    stp     x26, x27, [x1, #208]
    stp     x28, x29, [x1, #224]
    str     x30, [x1, #240]
    ldp     x2, x3, [sp], #16
    stp     x2, x3, [x1, #0]
    mrs     x2, sp_el0
    mrs     x3, sp_el1
    stp     x2, x3, [x1, #248]
    mrs     x2, elr_el1
    mrs     x3, spsr_el1
    stp     x2, x3, [x1, #264]
    mrs     x2, tpidr_el1
    str     x2, [x1, #280]
    ldr     x2, [sp, #104]
    msr     tpidr_el1, x2
    ldp     x19, x20, [sp, #0]
    ldp     x21, x22, [sp, #16]
    ldp     x23, x24, [sp, #32]
    ldp     x25, x26, [sp, #48]
    ldp     x27, x28, [sp, #64]
    ldp     x29, x30, [sp, #80]
    add     sp, sp, #112
    // A system error the guest's accesses have on their way is the guest's:
    // here is where it is taken, not in whatever runs next.
    dsb     sy
    isb
    msr     daifclr, #4
    isb
    msr     daifset, #4
    ret

    // What VBAR_EL2 holds while a guest runs: exceptions from the guest
    // bring it back; any from EL2 go where they always do, but for the
    // system error `vk_guest_left` waits for, which says so as it returns.
    .section .text
    .balign 2048
    .globl vk_guest_vectors
vk_guest_vectors:
    .set vk_entry, 0
    .rept 7
        .balign 128
        b       vk_vectors + vk_entry
        .set vk_entry, vk_entry + 128
    .endr
    .balign 128
    mov     x0, #{system_error}
    eret
    vk_guest_exit {synchronous}
    vk_guest_exit {interrupt}
    vk_guest_exit {interrupt}
    vk_guest_exit {system_error}
    vk_guest_exit {synchronous}
    vk_guest_exit {interrupt}
    vk_guest_exit {interrupt}
    vk_guest_exit {system_error}
"#,
    synchronous = const LEFT_SYNCHRONOUS,
    interrupt = const LEFT_INTERRUPT,
    system_error = const LEFT_SYSTEM_ERROR,
);

/// Reads list register `index`.
fn read_lr(index: usize) -> u64 {
    match index {
        0 => read_register!("s3_4_c12_c12_0"),
        1 => read_register!("s3_4_c12_c12_1"),
        2 => read_register!("s3_4_c12_c12_2"),
        3 => read_register!("s3_4_c12_c12_3"),
        4 => read_register!("s3_4_c12_c12_4"),
        5 => read_register!("s3_4_c12_c12_5"),
        6 => read_register!("s3_4_c12_c12_6"),
        7 => read_register!("s3_4_c12_c12_7"),
        8 => read_register!("s3_4_c12_c13_0"),
        9 => read_register!("s3_4_c12_c13_1"),
        10 => read_register!("s3_4_c12_c13_2"),
        11 => read_register!("s3_4_c12_c13_3"),
        12 => read_register!("s3_4_c12_c13_4"),
        13 => read_register!("s3_4_c12_c13_5"),
        14 => read_register!("s3_4_c12_c13_6"),
        _ => read_register!("s3_4_c12_c13_7"),
    }
}

/// Writes list register `index`.
///
/// # Safety
/// The guest takes what it says.
unsafe fn write_lr(index: usize, value: u64) {
    // SAFETY: forwarded to the caller.
    unsafe {
        match index {
            0 => write_register!("s3_4_c12_c12_0", value),
            1 => write_register!("s3_4_c12_c12_1", value),
            2 => write_register!("s3_4_c12_c12_2", value),
            3 => write_register!("s3_4_c12_c12_3", value),
            4 => write_register!("s3_4_c12_c12_4", value),
            5 => write_register!("s3_4_c12_c12_5", value),
            6 => write_register!("s3_4_c12_c12_6", value),
            7 => write_register!("s3_4_c12_c12_7", value),
            8 => write_register!("s3_4_c12_c13_0", value),
            9 => write_register!("s3_4_c12_c13_1", value),
            10 => write_register!("s3_4_c12_c13_2", value),
            11 => write_register!("s3_4_c12_c13_3", value),
            12 => write_register!("s3_4_c12_c13_4", value),
            13 => write_register!("s3_4_c12_c13_5", value),
            14 => write_register!("s3_4_c12_c13_6", value),
            _ => write_register!("s3_4_c12_c13_7", value),
        }
    }
}

/// Reads active priority register `index` of group 0 and of group 1.
fn read_active_priorities(index: usize) -> [u64; 2] {
    match index {
        0 => [read_register!("s3_4_c12_c8_0"), read_register!("s3_4_c12_c9_0")],
        1 => [read_register!("s3_4_c12_c8_1"), read_register!("s3_4_c12_c9_1")],
        2 => [read_register!("s3_4_c12_c8_2"), read_register!("s3_4_c12_c9_2")],
        _ => [read_register!("s3_4_c12_c8_3"), read_register!("s3_4_c12_c9_3")],
    }
}

/// Writes active priority register `index` of group 0 and of group 1.
///
/// # Safety
/// The guest takes what they say.
unsafe fn write_active_priorities(index: usize, [group0, group1]: [u64; 2]) {
    // SAFETY: forwarded to the caller.
    unsafe {
        match index {
            0 => {
                write_register!("s3_4_c12_c8_0", group0);
                write_register!("s3_4_c12_c9_0", group1);
            }
            1 => {
                write_register!("s3_4_c12_c8_1", group0);
                write_register!("s3_4_c12_c9_1", group1);
            }
            2 => {
                write_register!("s3_4_c12_c8_2", group0);
                write_register!("s3_4_c12_c9_2", group1);
            }
            _ => {
                write_register!("s3_4_c12_c8_3", group0);
                write_register!("s3_4_c12_c9_3", group1);
            }
        }
    }
}

/// What a guest reads in ID register `S3_0_C0_C<crm>_<op2>`.
fn id_register(crm: u8, op2: u8) -> u64 {
    let value = match (crm, op2) {
        (4, 0) => read_register!("id_aa64pfr0_el1"),
        (4, 1) => read_register!("id_aa64pfr1_el1"),
        (6, 0) => read_register!("id_aa64isar0_el1"),
        (6, 1) => read_register!("id_aa64isar1_el1"),
        (6, 2) => read_register!("s3_0_c0_c6_2"), // ID_AA64ISAR2_EL1
        (7, 0) => read_register!("id_aa64mmfr0_el1"),
        (7, 1) => read_register!("id_aa64mmfr1_el1"),
        (7, 2) => read_register!("id_aa64mmfr2_el1"),
        _ => 0,
    };
    features::id_register(crm, op2, value)
}

/// The guest's interrupts that wait for a list register.
#[derive(Debug, Clone, Copy, Default)]
struct Pending([u64; INTIDS / 64]);

impl Pending {
    fn set(&mut self, intid: usize) {
        self.0[intid / 64] |= 1 << (intid % 64);
    }

    fn clear(&mut self, intid: usize) {
        self.0[intid / 64] &= !(1 << (intid % 64));
    }

    /// Those pending, lowest first.
    fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..INTIDS).filter(|&i| self.0[i / 64] & (1 << (i % 64)) != 0)
    }
}

/// What other threads touch: interrupts on their way in, and how to reach
/// the virtual processor.
struct Shared {
    pending: Pending,
    /// The guest's registers of its private interrupts.
    private: Private,
    /// The thread that sleeps in the guest's `wfi`.
    halted: Option<Arc<Thread>>,
    /// The line each level-triggered interrupt is raised for: the guest's
    /// end of it unmasks the line.
    level: [Option<Weak<Interrupt>>; INTIDS],
}

/// What the guest's EL1 keeps while it is away: the registers that are
/// reached under the host's names, and those that are not.
#[derive(Debug, Clone, Copy, Default)]
struct Saved {
    sctlr: u64,
    cpacr: u64,
    ttbr0: u64,
    ttbr1: u64,
    tcr: u64,
    esr: u64,
    afsr0: u64,
    afsr1: u64,
    far: u64,
    elr: u64,
    spsr: u64,
    mair: u64,
    amair: u64,
    vbar: u64,
    contextidr: u64,
    cntkctl: u64,
    cntv_ctl: u64,
    cntv_cval: u64,
    tpidr_el0: u64,
    tpidrro_el0: u64,
    par: u64,
    csselr: u64,
    /// The virtual interface's state: its control register and active
    /// priorities.
    vmcr: u64,
    apr: [[u64; 2]; MAX_ACTIVE_PRIORITY_REGISTERS],
}

/// What the virtual processor waits for from the monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Awaiting {
    Nothing,
    HypercallResult,
}

/// The guest's registers that only the thread running the virtual
/// processor touches.
struct Context {
    regs: GuestRegs,
    saved: Saved,
    awaiting: Awaiting,
    fpu: alloc::boxed::Box<FpuState>,
    /// The list registers as the guest last left them (0: free).
    lrs: [u64; MAX_LIST_REGISTERS],
    /// Its registers are the processor's (`load`).
    loaded: bool,
    /// The host's thread registers while the guest's are loaded.
    host_tpidr_el0: u64,
    host_tpidrro_el0: u64,
    /// It has run: the guest's memory reached memory.
    started: bool,
}

pub struct Vcpu {
    pub koid: u64,
    pub guest: Arc<Guest>,
    /// The processor's number.
    pub id: u32,
    /// A thread runs the virtual processor.
    running: AtomicBool,
    /// The CPU (its id + 1) the virtual processor is in guest mode on; 0
    /// if none.
    in_guest: AtomicU32,
    shared: SpinLock<Shared>,
    ctx: UnsafeCell<Context>,
}

// SAFETY: `ctx` is only reached by the thread that holds `running`;
// `shared` is behind a lock.
unsafe impl Sync for Vcpu {}
// SAFETY: as above.
unsafe impl Send for Vcpu {}

/// What the run loop does after an exit.
enum Next {
    Resume,
    /// Back to the monitor, with the exit filled in.
    Monitor,
}

/// Whether each CPU's guest timer has fired: set in the interrupt's
/// handler, seen by [`Vcpu::take_timer`] before the guest leaves this CPU.
static TIMER_FIRED: [AtomicBool; crate::scheduler::percpu::MAX_CPUS] =
    [const { AtomicBool::new(false) }; crate::scheduler::percpu::MAX_CPUS];

/// Whether the guest's timer, as its own registers say, has fired and is
/// not masked: what the guest's handler checks before it takes the timer's
/// interrupt for one. Only such a timer may be masked and its interrupt
/// raised: a guest that takes the interrupt while its timer says it has not
/// fired takes it for spurious, and leaves the timer masked for good.
fn fired(ctl: u64) -> bool {
    ctl & (CTL_ENABLE | CTL_MASKED | CTL_FIRED) == CTL_ENABLE | CTL_FIRED
}

/// The guest's virtual timer fired on this CPU: its interrupt is a level,
/// which masking it ends, before the handler returns. The guest's handler
/// masks it too, so it notices nothing. An interrupt from a timer that has
/// not fired is stale, one that came before the guest moved its deadline
/// on: it is let go.
pub(super) fn timer_fired() {
    let ctl = read_register!("s3_5_c14_c3_1");
    if !fired(ctl) {
        return;
    }
    // SAFETY: the guest's timer, loaded on this CPU.
    unsafe { write_register!("s3_5_c14_c3_1", ctl | CTL_MASKED) };
    TIMER_FIRED[percpu::current_id()].store(true, Ordering::Release);
}

impl Vcpu {
    pub fn new(guest: Arc<Guest>, id: u32, state: VcpuState) -> Result<Arc<Vcpu>, Error> {
        super::available().map_err(|_| Error::NotSupported)?;
        if !is_guest_pstate(state.pstate) {
            return Err(Error::InvalidArgs);
        }
        let fpu = FpuState::new().ok_or(Error::NoMemory)?;
        let regs = GuestRegs {
            x: state.x,
            sp_el0: state.sp_el0,
            sp_el1: state.sp_el1,
            pc: state.pc,
            pstate: state.pstate,
            tpidr_el1: 0,
        };
        let saved = Saved {
            sctlr: if state.sctlr == 0 { SCTLR_RESET } else { state.sctlr },
            tcr: state.tcr,
            ttbr0: state.ttbr0,
            ttbr1: state.ttbr1,
            vbar: state.vbar,
            ..Saved::default()
        };
        let vcpu = Arc::new(Vcpu {
            koid: crate::capability::new_koid(),
            guest: guest.clone(),
            id,
            running: AtomicBool::new(false),
            in_guest: AtomicU32::new(0),
            shared: SpinLock::new(Shared {
                pending: Pending::default(),
                private: Private::default(),
                halted: None,
                level: [const { None }; INTIDS],
            }),
            ctx: UnsafeCell::new(Context {
                regs,
                saved,
                awaiting: Awaiting::Nothing,
                fpu,
                lrs: [0; MAX_LIST_REGISTERS],
                loaded: false,
                host_tpidr_el0: 0,
                host_tpidrro_el0: 0,
                started: false,
            }),
        });
        if !guest.add_vcpu(&vcpu) {
            return Err(Error::AlreadyExists);
        }
        Ok(vcpu)
    }

    /// Raises interrupt `vector` at the virtual processor.
    pub fn interrupt(&self, vector: u8) {
        self.shared.lock().pending.set(vector as usize);
        self.kick();
    }

    /// Raises level-triggered interrupt `vector` for `line`, which the
    /// guest's end-of-interrupt ends ([`Interrupt::end_of_level`]).
    pub fn interrupt_level(&self, vector: u8, line: &Arc<Interrupt>) {
        let mut shared = self.shared.lock();
        shared.pending.set(vector as usize);
        shared.level[vector as usize] = Some(Arc::downgrade(line));
        drop(shared);
        self.kick();
    }

    /// Makes the virtual processor look at its interrupts: wakes it if it
    /// is halted, makes its CPU leave the guest if it is in it.
    pub fn kick(&self) {
        if let Some(t) = self.shared.lock().halted.clone() {
            scheduler::wake(&t, WakeReason::Signaled);
        }
        let on = self.in_guest.load(Ordering::Acquire);
        if on != 0 && on - 1 != percpu::cpu_id_or_boot() {
            host_gic::send_sgi(percpu::get(on as usize - 1).affinity, host_gic::SGI_KICK);
        }
    }

    /// The registers, while no thread runs the virtual processor.
    pub fn read_state(&self) -> Result<VcpuState, Error> {
        if self.running.swap(true, Ordering::AcqRel) {
            return Err(Error::Busy);
        }
        // SAFETY: `running` gives this thread the context.
        let ctx = unsafe { &*self.ctx.get() };
        let state = VcpuState {
            x: ctx.regs.x,
            sp_el0: ctx.regs.sp_el0,
            sp_el1: ctx.regs.sp_el1,
            pc: ctx.regs.pc,
            pstate: ctx.regs.pstate,
            sctlr: ctx.saved.sctlr,
            tcr: ctx.saved.tcr,
            ttbr0: ctx.saved.ttbr0,
            ttbr1: ctx.saved.ttbr1,
            vbar: ctx.saved.vbar,
            esr: ctx.saved.esr,
            far: ctx.saved.far,
        };
        self.running.store(false, Ordering::Release);
        Ok(state)
    }

    /// Runs the virtual processor until it needs the monitor; `exit`
    /// brings the monitor's answer to the last exit, and takes the next.
    pub fn run(&self, exit: &mut VcpuExit) -> Result<(), Error> {
        if self.running.swap(true, Ordering::AcqRel) {
            return Err(Error::Busy);
        }
        // SAFETY: `running` gives this thread the context.
        let ctx = unsafe { &mut *self.ctx.get() };
        if ctx.awaiting == Awaiting::HypercallResult {
            ctx.regs.x[0] = exit.data[0];
        }
        ctx.awaiting = Awaiting::Nothing;
        if !ctx.started {
            ctx.started = true;
            self.guest.tables().clean();
        }
        self.load(ctx);
        let result = self.run_loop(ctx, exit);
        self.put(ctx);
        self.running.store(false, Ordering::Release);
        result
    }

    /// Swaps the guest's registers in, on this CPU.
    fn load(&self, ctx: &mut Context) {
        // SAFETY: the running thread's own context.
        unsafe { (*scheduler::current().ctx_ptr()).save_fpu() };
        ctx.fpu.restore();
        ctx.host_tpidr_el0 = read_register!("tpidr_el0");
        ctx.host_tpidrro_el0 = read_register!("tpidrro_el0");
        let s = &ctx.saved;
        let mpidr = 1 << 31 | affinity_of(self.id as u64);
        // SAFETY: registers of EL1 and of the virtual interface, which do
        // nothing for the host: it runs at EL2, and the guest's are given
        // back before anything else uses them.
        unsafe {
            write_register!("vpidr_el2", read_register!("midr_el1"));
            write_register!("vmpidr_el2", mpidr);
            write_register!("s3_5_c1_c0_0", s.sctlr);
            write_register!("s3_5_c1_c0_2", s.cpacr);
            write_register!("s3_5_c2_c0_0", s.ttbr0);
            write_register!("s3_5_c2_c0_1", s.ttbr1);
            write_register!("s3_5_c2_c0_2", s.tcr);
            write_register!("s3_5_c5_c2_0", s.esr);
            write_register!("s3_5_c5_c1_0", s.afsr0);
            write_register!("s3_5_c5_c1_1", s.afsr1);
            write_register!("s3_5_c6_c0_0", s.far);
            write_register!("s3_5_c4_c0_0", s.spsr);
            write_register!("s3_5_c4_c0_1", s.elr);
            write_register!("s3_5_c10_c2_0", s.mair);
            write_register!("s3_5_c10_c3_0", s.amair);
            write_register!("s3_5_c12_c0_0", s.vbar);
            write_register!("s3_5_c13_c0_1", s.contextidr);
            write_register!("s3_5_c14_c1_0", s.cntkctl);
            write_register!("s3_5_c14_c3_2", s.cntv_cval);
            write_register!("s3_5_c14_c3_1", s.cntv_ctl);
            write_register!("tpidr_el0", s.tpidr_el0);
            write_register!("tpidrro_el0", s.tpidrro_el0);
            write_register!("par_el1", s.par);
            write_register!("csselr_el1", s.csselr);
            write_register!("s3_4_c12_c11_7", s.vmcr); // ICH_VMCR_EL2
            for at in 0..active_priority_registers() {
                write_active_priorities(at, s.apr[at]);
            }
            asm!("isb", options(nostack, preserves_flags));
        }
        ctx.loaded = true;
    }

    /// Gives the host its registers back.
    fn put(&self, ctx: &mut Context) {
        if !core::mem::replace(&mut ctx.loaded, false) {
            return;
        }
        let s = &mut ctx.saved;
        s.sctlr = read_register!("s3_5_c1_c0_0");
        s.cpacr = read_register!("s3_5_c1_c0_2");
        s.ttbr0 = read_register!("s3_5_c2_c0_0");
        s.ttbr1 = read_register!("s3_5_c2_c0_1");
        s.tcr = read_register!("s3_5_c2_c0_2");
        s.esr = read_register!("s3_5_c5_c2_0");
        s.afsr0 = read_register!("s3_5_c5_c1_0");
        s.afsr1 = read_register!("s3_5_c5_c1_1");
        s.far = read_register!("s3_5_c6_c0_0");
        s.spsr = read_register!("s3_5_c4_c0_0");
        s.elr = read_register!("s3_5_c4_c0_1");
        s.mair = read_register!("s3_5_c10_c2_0");
        s.amair = read_register!("s3_5_c10_c3_0");
        s.vbar = read_register!("s3_5_c12_c0_0");
        s.contextidr = read_register!("s3_5_c13_c0_1");
        s.cntkctl = read_register!("s3_5_c14_c1_0");
        s.cntv_cval = read_register!("s3_5_c14_c3_2");
        s.cntv_ctl = read_register!("s3_5_c14_c3_1");
        s.tpidr_el0 = read_register!("tpidr_el0");
        s.tpidrro_el0 = read_register!("tpidrro_el0");
        s.par = read_register!("par_el1");
        s.csselr = read_register!("csselr_el1");
        s.vmcr = read_register!("s3_4_c12_c11_7");
        for at in 0..active_priority_registers() {
            s.apr[at] = read_active_priorities(at);
        }
        // SAFETY: its timer goes with it, and the host's registers come
        // back.
        unsafe {
            write_register!("s3_5_c14_c3_1", 0u64);
            write_register!("tpidr_el0", ctx.host_tpidr_el0);
            write_register!("tpidrro_el0", ctx.host_tpidrro_el0);
        }
        // Its timer has stopped, and what it did so far stays with it.
        self.take_timer();
        ctx.fpu.save();
        // SAFETY: the running thread's own context.
        unsafe { (*scheduler::current().ctx_ptr()).restore_fpu() };
    }

    fn run_loop(&self, ctx: &mut Context, exit: &mut VcpuExit) -> Result<(), Error> {
        let pointer = self.guest.tables().pointer();
        loop {
            let thread = scheduler::current();
            if thread.kill_pending.load(Ordering::Acquire) {
                return Err(Error::Canceled);
            }
            if scheduler::need_resched() {
                self.put(ctx);
                scheduler::schedule();
                self.load(ctx);
                continue;
            }
            drop(thread);
            self.flush_interrupts(ctx);
            let cpu = percpu::cpu_id_or_boot();
            self.in_guest.store(cpu + 1, Ordering::Release);
            bkl::release();
            // SAFETY: the guest's registers are in `ctx.regs`, and those of
            // EL1 in the processor; the host's that go are put back below,
            // with interrupts masked meanwhile.
            let left = unsafe {
                write_register!("vttbr_el2", pointer);
                write_register!("vbar_el1", core::ptr::addr_of!(vk_guest_vectors) as u64);
                write_register!("hcr_el2", HCR_GUEST);
                asm!("isb", options(nostack, preserves_flags));
                vk_guest_run(&mut ctx.regs)
            };
            let (esr, far, hpfar) = (read_register!("esr_el1"), read_register!("far_el1"), read_register!("hpfar_el2"));
            // SAFETY: the host's own values.
            unsafe {
                write_register!("hcr_el2", HCR_HOST);
                write_register!("vbar_el1", entry::vectors());
                asm!("isb", options(nostack, preserves_flags));
                if cpu::features().pan {
                    cpu::set_pan(true);
                }
            }
            let ended = self.save_interrupts(ctx);
            self.in_guest.store(0, Ordering::Release);
            if left == LEFT_INTERRUPT {
                // Take the host's interrupt that made the guest exit.
                cpu::take_pending_interrupts();
            }
            bkl::acquire();
            self.finish_interrupts(ended);
            if left == LEFT_SYSTEM_ERROR {
                self.fail(ctx, exit, esr, far);
                return Ok(());
            }
            if left == LEFT_SYNCHRONOUS {
                match self.handle_exit(ctx, esr, far, hpfar, exit)? {
                    Next::Resume => {}
                    Next::Monitor => return Ok(()),
                }
            }
        }
    }

    /// Raises the guest's timer interrupt, if the timer fired on this CPU.
    fn take_timer(&self) {
        if TIMER_FIRED[percpu::current_id()].swap(false, Ordering::AcqRel) {
            self.shared.lock().pending.set(VIRTUAL_TIMER_PPI as usize);
        }
    }

    /// The exit failed the guest: the monitor learns why.
    fn fail(&self, ctx: &Context, exit: &mut VcpuExit, esr: u64, far: u64) {
        let pc = ctx.regs.pc;
        crate::kwarn!("vcpu {}: syndrome {:#x} at pc {:#x}, address {:#x}", self.id, esr, pc, far);
        *exit = VcpuExit { reason: vcpu_exit::FAILED, _reserved: 0, data: [esr, far, pc, 0, 0, 0, 0] };
    }

    /// The guest's registers of its interrupt controller, for `f`.
    fn registers<R>(&self, shared: &mut Shared, f: impl FnOnce(&mut Registers<'_>) -> R) -> R {
        let mut tables = self.guest.tables();
        let cpus = self.guest.cpus;
        f(&mut Registers { shared: &mut tables.distributor, private: &mut shared.private, cpus, cpu: self.id })
    }

    /// Fills the free list registers with the interrupts the guest may
    /// have, and asks to come back if more wait. With the BKL held, and no
    /// guest on this CPU.
    fn flush_interrupts(&self, ctx: &mut Context) {
        self.take_timer();
        let count = list_registers().min(MAX_LIST_REGISTERS);
        let mut control = ICH_ENABLE;
        {
            let mut guard = self.shared.lock();
            let s = &mut *guard;
            let pending = s.pending;
            let ready: alloc::vec::Vec<usize> = self.registers(s, |r| {
                let on = gic::distributor_on(r.shared);
                pending.iter().filter(|&i| r.enabled(i) && (on || i < gic::PRIVATE)).collect()
            });
            let mut waiting = false;
            for intid in ready {
                if ctx.lrs[..count].iter().any(|lr| lr & LR_STATE != 0 && lr & LR_INTID == intid as u64) {
                    continue;
                }
                let Some(lr) = ctx.lrs[..count].iter_mut().find(|lr| **lr & LR_STATE == 0) else {
                    waiting = true;
                    break;
                };
                s.pending.clear(intid);
                let eoi = if s.level[intid].is_some() { LR_EOI } else { 0 };
                *lr = LR_PENDING | LR_GROUP1 | LR_PRIORITY | eoi | intid as u64;
            }
            if waiting {
                control |= ICH_UNDERFLOW;
            }
        }
        // SAFETY: what the guest may take.
        unsafe {
            for (at, &lr) in ctx.lrs[..count].iter().enumerate() {
                write_lr(at, lr);
            }
            write_register!("s3_4_c12_c11_0", control); // ICH_HCR_EL2
        }
    }

    /// Reads the list registers back, and turns the virtual interface off
    /// for the host: the level-triggered interrupts the guest ended
    /// meanwhile, by their INTIDs' bits.
    fn save_interrupts(&self, ctx: &mut Context) -> Pending {
        let mut ended = Pending::default();
        let count = list_registers().min(MAX_LIST_REGISTERS);
        for (at, slot) in ctx.lrs[..count].iter_mut().enumerate() {
            let lr = read_lr(at);
            if lr & LR_STATE == 0 {
                if *slot & LR_EOI != 0 {
                    ended.set((*slot & LR_INTID) as usize);
                }
                *slot = 0;
            } else {
                *slot = lr;
            }
        }
        // SAFETY: the host has no use for it.
        unsafe { write_register!("s3_4_c12_c11_0", 0u64) };
        ended
    }

    /// Lets the lines of the interrupts `ended` go, with the BKL held.
    fn finish_interrupts(&self, ended: Pending) {
        for intid in ended.iter() {
            let line = self.shared.lock().level[intid].take().and_then(|l| l.upgrade());
            if let Some(line) = line {
                line.end_of_level();
            }
        }
    }

    fn handle_exit(
        &self,
        ctx: &mut Context,
        esr: u64,
        far: u64,
        hpfar: u64,
        exit: &mut VcpuExit,
    ) -> Result<Next, Error> {
        let class = (esr >> 26) & 0x3F;
        match class {
            EC_WFI => {
                ctx.regs.pc += 4;
                self.halt(ctx)?;
            }
            EC_HVC => {
                let x = &ctx.regs.x;
                *exit = VcpuExit {
                    reason: vcpu_exit::HYPERCALL,
                    _reserved: 0,
                    data: [x[0], x[1], x[2], x[3], x[4], x[5], 0],
                };
                ctx.awaiting = Awaiting::HypercallResult;
                return Ok(Next::Monitor);
            }
            // No firmware: it answers as a call that does not exist does.
            EC_SMC => {
                ctx.regs.x[0] = u64::MAX;
                ctx.regs.pc += 4;
            }
            EC_SYSTEM_REGISTER if self.system_register(ctx, esr) => ctx.regs.pc += 4,
            EC_DATA_ABORT | EC_INSTRUCTION_ABORT => {
                // `HPFAR_EL2` says which page, but not of a permission fault
                // outside a table walk, of which only the offset is known.
                let page_known = esr & ISS_TABLE_WALK != 0 || esr & ISS_FAULT_KIND == ISS_TRANSLATION_FAULT;
                let gpa = if page_known { (hpfar & !0xF) << 8 } else { 0 } | (far & 0xFFF);
                if class == EC_DATA_ABORT && page_known && is_gic(gpa) {
                    if esr & ISS_VALID == 0 {
                        self.fail(ctx, exit, esr, far);
                        return Ok(Next::Monitor);
                    }
                    self.gic_access(ctx, esr, gpa);
                    return Ok(Next::Resume);
                }
                let access = if class == EC_INSTRUCTION_ABORT {
                    4
                } else if esr & ISS_WRITE != 0 {
                    2
                } else {
                    1
                };
                *exit = VcpuExit {
                    reason: vcpu_exit::MEMORY,
                    _reserved: 0,
                    data: [gpa, access, ctx.regs.pc, esr, 0, 0, 0],
                };
                return Ok(Next::Monitor);
            }
            _ => {
                self.fail(ctx, exit, esr, far);
                return Ok(Next::Monitor);
            }
        }
        Ok(Next::Resume)
    }

    /// Answers an access to the interrupt controller's registers, as the
    /// fault's syndrome describes it.
    fn gic_access(&self, ctx: &mut Context, esr: u64, gpa: u64) {
        let size = 1u64 << ((esr >> ISS_SIZE) & 3);
        let register = ((esr >> ISS_REGISTER) & 0x1F) as usize;
        let write = esr & ISS_WRITE != 0;
        let value = if register == 31 { 0 } else { ctx.regs.x[register] };
        let access = if write { Access::Write(value) } else { Access::Read };
        let result = self.registers(&mut self.shared.lock(), |r| r.access_sized(gpa, size, access));
        if !write && register != 31 {
            ctx.regs.x[register] = result;
        }
        ctx.regs.pc += 4;
    }

    /// Answers a system register the guest's access to which trapped, and
    /// says whether it is one the guest may reach: `ICC_SGI1R_EL1`, a
    /// processor's message to others; the ID registers (`hv::arm64::features`);
    /// and those of debugging (`MDCR_EL2` traps them) and `ACTLR_EL1`, which
    /// are the host's to leave alone, and read as 0 and take any write.
    fn system_register(&self, ctx: &mut Context, esr: u64) -> bool {
        let field = |at: u32, bits: u32| ((esr >> at) & ((1 << bits) - 1)) as u8;
        let (op0, op2, op1, crn, crm) = (field(20, 2), field(17, 3), field(14, 3), field(10, 4), field(1, 4));
        let register = field(5, 5) as usize;
        let write = esr & 1 == 0;
        let value = if register == 31 { 0 } else { ctx.regs.x[register] };
        let read = match (op0, op1, crn, crm, op2) {
            (3, 0, 12, 11, 5) if write => {
                self.send_sgi(value);
                0
            }
            (3, 0, 0, 1..=7, _) if !write => id_register(crm, op2),
            (3, 0, 1, 0, 1) | (2, ..) => 0,
            _ => return false,
        };
        if !write && register != 31 {
            ctx.regs.x[register] = read;
        }
        true
    }

    /// `ICC_SGI1R_EL1` written with `value`: a processor's message to others.
    fn send_sgi(&self, value: u64) {
        let intid = ((value >> 24) & 0xF) as u8;
        let all = value & (1 << 40) != 0;
        let (list, aff1) = (value & 0xFFFF, (value >> 16) & 0xFF);
        for vcpu in self.guest.vcpus() {
            let id = vcpu.id as u64;
            let named = id >> 4 == aff1 && list & (1 << (id & 0xF)) != 0;
            if if all { vcpu.id != self.id } else { named } {
                vcpu.interrupt(intid);
            }
        }
    }

    /// `wfi`: waits off the CPU until the guest has an interrupt it can
    /// take, or its timer fires.
    fn halt(&self, ctx: &mut Context) -> Result<(), Error> {
        loop {
            let ctl = read_register!("s3_5_c14_c3_1");
            let cval = read_register!("s3_5_c14_c3_2");
            let armed = ctl & CTL_ENABLE != 0 && ctl & CTL_MASKED == 0;
            // Fired as the timer says, not as the counter does, which may
            // be past the deadline a moment before the timer says it fired
            // (under QEMU): the sleep below then ends at once.
            if fired(ctl) {
                // SAFETY: as if its interrupt had come ([`timer_fired`]).
                unsafe { write_register!("s3_5_c14_c3_1", ctl | CTL_MASKED) };
                TIMER_FIRED[percpu::current_id()].store(true, Ordering::Release);
            }
            self.take_timer();
            {
                let mut guard = self.shared.lock();
                let s = &mut *guard;
                let pending = s.pending;
                let any = self.registers(s, |r| pending.iter().any(|i| r.enabled(i)));
                if any || ctx.lrs.iter().any(|lr| lr & LR_PENDING != 0) {
                    return Ok(());
                }
            }
            let thread = scheduler::current();
            self.put(ctx);
            self.shared.lock().halted = Some(thread);
            let deadline = armed.then(|| crate::time::monotonic_ns_at(cval));
            let woke = scheduler::block(deadline);
            self.shared.lock().halted = None;
            self.load(ctx);
            if woke == WakeReason::Killed {
                return Err(Error::Canceled);
            }
        }
    }
}

const _: () = assert!(MAINTENANCE_PPI < gic::PRIVATE as u32);

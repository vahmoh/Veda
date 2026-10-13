//! What AMD64 keeps for each CPU, reached through the `GS` segment base
//! while in the kernel: the scratch slots the entry code uses at fixed
//! offsets (`gs:[OFFSET]`), the CPU's index, its descriptor tables and its
//! local APIC's id, and its requests for TLB and EPT shootdowns.

use core::cell::UnsafeCell;
use core::mem::{MaybeUninit, offset_of};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::cpu::{MSR_GS_BASE, MSR_KERNEL_GS_BASE, wrmsr};
use super::gdt::{Gdt, Tss};
use crate::percpu::MAX_CPUS;

/// Offsets used by assembly.
pub(in crate::arch) const OFF_USER_RSP: usize = 8;
pub(in crate::arch) const OFF_KERNEL_RSP: usize = 16;
const OFF_CPU_ID: usize = 24;

#[repr(C, align(64))]
pub(in crate::arch) struct Local {
    /// Pointer to this structure (`gs:[0]`).
    self_ptr: *const Local,
    /// Scratch slot for the user stack pointer during `syscall` entry.
    pub user_rsp: u64,
    /// Top of the running thread's kernel stack (loaded on `syscall` entry).
    pub kernel_rsp: u64,
    /// The CPU's index (`crate::percpu`).
    pub cpu_id: u32,
    pub apic_id: u32,
    pub gdt: UnsafeCell<Gdt>,
    pub tss: UnsafeCell<Tss>,
    /// Set by another CPU that wants this one to flush its TLB.
    pub tlb_flush_pending: AtomicBool,
    /// Acknowledgement counter for TLB shootdowns.
    pub tlb_flush_done: AtomicU64,
    /// Set by another CPU that wants this one to forget the guests'
    /// physical address translations it caches (EPT).
    pub ept_flush_pending: AtomicBool,
    /// This CPU is in VMX operation (it can run virtual processors).
    pub vmx_on: AtomicBool,
}

const _: () = {
    assert!(offset_of!(Local, user_rsp) == OFF_USER_RSP);
    assert!(offset_of!(Local, kernel_rsp) == OFF_KERNEL_RSP);
    assert!(offset_of!(Local, cpu_id) == OFF_CPU_ID);
};

// SAFETY: each Local is mutated only by its own CPU (or under the BKL);
// cross-CPU fields are atomics.
unsafe impl Sync for Local {}

const fn new_local(id: u32) -> Local {
    Local {
        self_ptr: core::ptr::null(),
        user_rsp: 0,
        kernel_rsp: 0,
        cpu_id: id,
        apic_id: 0,
        gdt: UnsafeCell::new(Gdt::new()),
        tss: UnsafeCell::new(Tss::new()),
        tlb_flush_pending: AtomicBool::new(false),
        tlb_flush_done: AtomicU64::new(0),
        ept_flush_pending: AtomicBool::new(false),
        vmx_on: AtomicBool::new(false),
    }
}

struct LocalArray([Local; MAX_CPUS]);
// SAFETY: see `Local`.
unsafe impl Sync for LocalArray {}

static LOCAL: LocalArray = {
    let mut i = 0;
    // `[expr; N]` needs Copy, so build the array element by element.
    let mut arr: [MaybeUninit<Local>; MAX_CPUS] = [const { MaybeUninit::uninit() }; MAX_CPUS];
    while i < MAX_CPUS {
        arr[i] = MaybeUninit::new(new_local(i as u32));
        i += 1;
    }
    // SAFETY: every element was initialised above.
    LocalArray(unsafe { core::mem::transmute::<[MaybeUninit<Local>; MAX_CPUS], [Local; MAX_CPUS]>(arr) })
};

static GS_READY: AtomicBool = AtomicBool::new(false);

/// Points this CPU's `GS` base at its block. Must run before anything else
/// touches per-CPU state.
///
/// # Safety
/// Called once per CPU, with `id` unique.
pub(in crate::arch) unsafe fn install(id: usize, apic_id: u32) {
    let p = &LOCAL.0[id];
    let ptr = p as *const Local as *mut Local;
    // SAFETY: this CPU is the only user of its slot during bring-up.
    unsafe {
        (*ptr).self_ptr = ptr;
        (*ptr).apic_id = apic_id;
        wrmsr(MSR_GS_BASE, ptr as u64);
        wrmsr(MSR_KERNEL_GS_BASE, 0);
    }
    GS_READY.store(true, Ordering::Release);
}

/// The current CPU's block.
#[inline]
pub(in crate::arch) fn current() -> &'static Local {
    let p: *const Local;
    // SAFETY: GS points at our Local while in kernel mode.
    unsafe { core::arch::asm!("mov {}, gs:[0]", out(reg) p, options(nostack, preserves_flags, readonly)) };
    // SAFETY: the pointer refers to a static.
    unsafe { &*p }
}

/// The current CPU's index (once [`install`]ed).
#[inline]
pub fn current_id() -> usize {
    let id: u32;
    // SAFETY: GS points at our Local while in kernel mode.
    unsafe {
        core::arch::asm!("mov {:e}, gs:[{off}]", out(reg) id, off = const OFF_CPU_ID,
            options(nostack, preserves_flags, readonly))
    };
    id as usize
}

/// Records the APIC id of CPU `id` before it is started.
///
/// # Safety
/// The CPU must not be running yet.
pub(in crate::arch) unsafe fn set_apic_id(id: usize, apic_id: u32) {
    let p = &LOCAL.0[id] as *const Local as *mut Local;
    // SAFETY: guaranteed by the caller.
    unsafe { (*p).apic_id = apic_id };
}

/// Current CPU index, or 0 before per-CPU data is installed.
#[inline]
pub fn cpu_id_or_boot() -> u32 {
    if !GS_READY.load(Ordering::Relaxed) {
        return 0;
    }
    current_id() as u32
}

/// Block of CPU `id`.
pub(in crate::arch) fn get(id: usize) -> &'static Local {
    &LOCAL.0[id]
}

/// The blocks of all CPUs that have come online.
pub(in crate::arch) fn online() -> impl Iterator<Item = &'static Local> {
    crate::percpu::online().map(|p| get(p.cpu_id as usize))
}

/// Updates the stack used on entry from user mode (syscall and interrupts).
pub(in crate::arch) fn set_kernel_stack(top: u64) {
    let p = current() as *const Local as *mut Local;
    // SAFETY: only this CPU writes its own entry-stack fields, with
    // interrupts disabled.
    unsafe {
        (*p).kernel_rsp = top;
        (*(*p).tss.get()).rsp[0] = top;
    }
}

//! The kernel ABI on AMD64: how system calls are made, the user address
//! space and its pages, how threads start, and virtual processors as Intel
//! VMX presents an x86-64 processor.
//!
//! # Calling convention
//!
//! System calls use the `syscall` instruction. `rax` holds the call number
//! (see [`nr`](crate::nr)), arguments are passed in `rdi, rsi, rdx, r10,
//! r8, r9`. On return `rax` holds a status: non-negative values are success
//! results, negative values are `-(error code)` (see
//! [`Error`](crate::Error)). Some calls return a second value in `rdx`.
//! `rcx` and `r11` are clobbered by the instruction; all other registers
//! are preserved.
//!
//! # Threads
//!
//! A thread starts at its entry point with its two arguments in `rdi` and
//! `rsi`, and the stack pointer [`entry_stack_pointer`] gives. Its thread
//! pointer (`thread_set_tls`) is its FS base. The clock's counter
//! ([`ClockInfo`](crate::ClockInfo)) is the TSC (`rdtsc`).

/// Page size used by the kernel for all mappings.
pub const PAGE_SIZE: usize = 4096;

/// Lowest address user space may map.
pub const USER_SPACE_START: usize = 0x0000_0000_0001_0000;
/// One past the highest user-space address.
pub const USER_SPACE_END: usize = 0x0000_7FFF_FFFF_0000;

/// The stack pointer a thread of a Veda program starts with, its stack's
/// top being `top` (16-byte aligned): aligned as on entry to a function
/// (8 bytes, a return address's room, off 16-byte alignment), below 64
/// bytes left free, room for the Win64 shadow space of the calls it makes.
pub const fn entry_stack_pointer(top: usize) -> usize {
    top - 72
}

/// A segment register of a virtual processor.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VcpuSegment {
    pub base: u64,
    pub limit: u32,
    /// The descriptor's attributes as Intel's VMX keeps them: type (bits
    /// 0-3), S (4), DPL (5-6), P (7), AVL (12), L (13), D/B (14), G (15),
    /// and bit 16 for a segment that cannot be used.
    pub access: u32,
    pub selector: u16,
    pub _reserved: [u16; 3],
}

/// The registers of a virtual processor (`vcpu_create`,
/// `vcpu_read_state`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VcpuState {
    /// rax, rcx, rdx, rbx, rsp, rbp, rsi, rdi, r8 to r15: the order in
    /// which instructions number them.
    pub gprs: [u64; 16],
    pub rip: u64,
    pub rflags: u64,
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub efer: u64,
    pub cs: VcpuSegment,
    pub ds: VcpuSegment,
    pub es: VcpuSegment,
    pub fs: VcpuSegment,
    pub gs: VcpuSegment,
    pub ss: VcpuSegment,
    pub tr: VcpuSegment,
    pub ldtr: VcpuSegment,
    pub gdt_base: u64,
    pub idt_base: u64,
    pub gdt_limit: u32,
    pub idt_limit: u32,
}

/// Reasons of a [`VcpuExit`].
pub mod vcpu_exit {
    /// The guest made a hypercall (`vmcall`): `data` holds its rax (the
    /// call), rbx, rcx, rdx, rsi and rdi. The next `vcpu_run` gives the
    /// guest `data[0]` in rax.
    pub const HYPERCALL: u32 = 1;
    /// The guest read or wrote an I/O port: `data[0]` is the port,
    /// `data[1]` the size (1, 2 or 4), `data[2]` 1 for a write, `data[3]`
    /// the value written. After a read, the next `vcpu_run` gives the
    /// guest `data[3]`.
    pub const IO: u32 = 2;
    /// The guest reached guest-physical memory that is not mapped (or not
    /// for that access): `data[0]` is the address, `data[1]` the access
    /// (1 read, 2 write, 4 instruction fetch), `data[2]` the guest's rip.
    pub const MEMORY: u32 = 3;
    /// The guest shut its processor down (a triple fault).
    pub const SHUTDOWN: u32 = 4;
    /// The processor would not run the guest, or did something the kernel
    /// does not handle: `data[0]` is the processor's exit reason,
    /// `data[1]` its qualification (or the instruction error of a failed
    /// entry), `data[2]` the guest's rip.
    pub const FAILED: u32 = 5;
}

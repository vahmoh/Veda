//! What Veda's hypervisor knows of AMD64 machines that touches no hardware.
//!
//! * [`lapic`]: the local APIC of a guest's virtual processor, in x2APIC
//!   mode: registers, interrupt priorities, the timer, inter-processor
//!   interrupts.
//! * [`cpuid`]: what `cpuid` tells a guest: the processor's own features,
//!   less those a guest cannot use, and the platform's leaves.
//! * [`platform`]: the paravirtual platform as x86-64 guests have it: how
//!   they find it, make its hypercalls and start their processors.
//! * [`linux`]: booting a Linux kernel on it (the x86 boot protocol).
//! * [`acpi`]: the PC's devices the ACPI tables describe (its keyboard
//!   controller), and its boot architecture's flags.
//! * [`memory`]: where a guest has what is below 1 MiB.

pub mod acpi;
pub mod cpuid;
pub mod lapic;
pub mod linux;
pub mod memory;
pub mod platform;

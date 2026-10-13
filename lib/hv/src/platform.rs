//! The paravirtual platform Veda's guests run on.
//!
//! A guest finds no emulated hardware: no interrupt controllers or timers
//! but its processors' own, no serial port or firmware code. It has
//! processors, memory, ACPI tables that describe its devices, and the
//! platform's hypercalls ([`hypercall`]). A hypercall takes up to five
//! arguments, all of them 64-bit words, and returns one, negative for an
//! error ([`error`]). How the guest finds the platform, makes a hypercall
//! and starts its processors is the architecture's
//! ([`crate::amd64::platform`]); a processor's number is as its interrupt
//! controller knows it.
//!
//! The ACPI tables (hardware-reduced ACPI: no fixed hardware, no SCI, no
//! MADT) are where the boot parameters say. They
//! describe the PCI root and its functions, the devices of the PC that the
//! guest has (on AMD64, its keyboard controller), and the interrupt lines
//! those use:
//! the PC's global system interrupts (GSIs), which the guest routes to its
//! processors with [`hypercall::GSI`]. GPIO pins of the PC's that the
//! guest's devices are wired to are on GPIO controllers of the platform's
//! (`VEDA0001`), which [`hypercall::GPIO`] drives, by the PC's numbers for
//! the pins; their interrupts are lines too, GSIs from
//! [`PLATFORM_GSIS`] on.
//!
//! The PC's firmware variables that an operating system may read (UEFI's,
//! with runtime access, as they were when the PC started) the guest reads
//! with [`hypercall::FIRMWARE_VARIABLE`], which does what UEFI's
//! `GetVariable` and `GetNextVariableName` do; it cannot change them.
//!
//! PCI functions given to the guest are on its PCI segment 0, where the
//! platform puts them (bus 0); the guest finds them by reading their
//! configuration space, which it reaches only through hypercalls. Their
//! memory BARs hold the addresses the platform mapped them at, which stay.
//! Their MSIs and MSI-X the guest routes with a hypercall that gives back
//! the message the function must send; a function's INTx, where the PC
//! wires one, is a GSI the root's `_PRT` names. Their DMA
//! reaches the guest's memory, at its guest-physical addresses, and
//! nothing else.

/// The platform's hypercalls, by number.
pub mod hypercall {
    /// `console_write(count, w0, w1, w2, w3)`: writes `count` bytes (at
    /// most 32) to the console, from the words `w0` to `w3` (the first
    /// byte in the low byte of `w0`). Arguments rather than memory, so
    /// that the guest can write at any moment of its life, however early
    /// or broken.
    pub const CONSOLE_WRITE: u64 = 1;
    /// `start_cpu(cpu, entry)`: starts processor `cpu` at `entry`, in the
    /// state the architecture's platform starts processors in. A processor
    /// starts once.
    pub const START_CPU: u64 = 2;
    /// `power(what)`: powers the machine off ([`super::power::OFF`]),
    /// restarts it ([`super::power::RESTART`]) or reports that the guest
    /// crashed ([`super::power::CRASHED`]). Does not return.
    pub const POWER: u64 = 3;
    /// `wallclock()`: the time of day: nanoseconds since 1970 (UTC).
    pub const WALLCLOCK: u64 = 4;
    /// An operation of the bridge ([`crate::bridge::HYPERCALL`]).
    pub const BRIDGE: u64 = crate::bridge::HYPERCALL;
    /// `pci_config_read(function, offset, size)`: reads `size` bytes (1, 2
    /// or 4) at `offset` of the configuration space of PCI function
    /// `function` (bus << 8 | device << 3 | function): the value, all ones
    /// where there is nothing.
    pub const PCI_CONFIG_READ: u64 = 6;
    /// `pci_config_write(function, offset, size, value)`: writes `value`
    /// (`size` bytes) at `offset` of the configuration space of PCI
    /// function `function`.
    pub const PCI_CONFIG_WRITE: u64 = 7;
    /// `pci_msi(function, msi, cpu, vector)`: routes MSI `msi` of PCI
    /// function `function` (an MSI-X entry, or one of MSI's vectors) to
    /// `vector` of processor `cpu`, and gives the message the function must
    /// send for it, as the architecture's platform packs it. Routing the
    /// same MSI again moves it; the message stays the same.
    pub const PCI_MSI: u64 = 8;
    /// `gsi(line, cpu, vector)`: routes GSI `line` (below [`GSIS`]; one
    /// the ACPI tables name) to `vector` of processor `cpu`, or to nothing
    /// (vector 0), which masks the line. Routing one again moves it. An
    /// edge needs no end-of-interrupt; a level-triggered line is masked
    /// when it fires until the processor's end-of-interrupt of its vector
    /// (as with a machine's interrupt controller). An edge that came while
    /// the line was masked is raised when it is routed again.
    pub const GSI: u64 = 9;
    /// `gpio(controller, pin, operation, level)`: `operation`
    /// ([`super::gpio`]) on pin `pin` (the PC's number, as the ACPI tables
    /// give it) of the platform's GPIO controller `controller` (its
    /// `_UID`), with `level` for a level. Returns the level, whether the
    /// pin is an output, or 0; [`super::error::INVALID`] if the guest has
    /// no such pin, or the PC would not do it.
    pub const GPIO: u64 = 10;
    /// `firmware_variable(operation, request)`: `operation`
    /// ([`super::variable`]) on the PC's firmware variables, with the
    /// [`super::Variable`] request at guest-physical address `request`.
    /// Returns 0; [`super::error::NOT_FOUND`] if there is no such variable
    /// (or none after it); [`super::error::TOO_SMALL`] if a buffer is too
    /// small for what goes there (the request then holds the size it
    /// needs); [`super::error::INVALID`] if the request is not in the
    /// guest's memory, or what it names is not a variable's name.
    pub const FIRMWARE_VARIABLE: u64 = 11;
}

/// What [`hypercall::FIRMWARE_VARIABLE`] does.
pub mod variable {
    /// UEFI's `GetVariable`: copies the data of the variable `name` (of
    /// vendor `guid`) to `data`, and sets `data_size` to its size and
    /// `attributes` to its attributes (with [`super::error::TOO_SMALL`]
    /// too).
    pub const GET: u64 = 0;
    /// UEFI's `GetNextVariableName`: replaces `name` and `guid` with those
    /// of the variable after them (the first after the empty name), and
    /// sets `name_size` to its name's size, the NUL included.
    pub const NEXT: u64 = 1;
}

/// A [`hypercall::FIRMWARE_VARIABLE`] request, in the guest's memory.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Variable {
    /// The variable's vendor (a GUID, as UEFI lays it out).
    pub guid: [u8; 16],
    /// Where its name is (guest-physical): UTF-16, NUL-terminated, in a
    /// buffer of `name_size` bytes.
    pub name: u64,
    pub name_size: u64,
    /// Where its data goes ([`variable::GET`]), a buffer of `data_size`
    /// bytes.
    pub data: u64,
    pub data_size: u64,
    /// Its attributes (UEFI's), from [`variable::GET`].
    pub attributes: u32,
    pub reserved: u32,
}

// The layout Linux's side (`asm/veda_para.h`) has.
const _: () = assert!(core::mem::size_of::<Variable>() == 56);

/// GSIs are below this: the guest's IRQs of the same numbers.
pub const GSIS: u32 = 512;
/// GSIs below this are the PC's own; from it, lines the platform makes
/// (the interrupts of GPIO pins).
pub const PLATFORM_GSIS: u32 = 256;

/// What [`hypercall::GPIO`] does with a pin.
pub mod gpio {
    /// The level: what the pin drives if it is an output, else what it
    /// reads (made an input if it is neither).
    pub const READ: u64 = 0;
    /// Drives the call's `level` (0 low, else high), making the pin an
    /// output.
    pub const WRITE: u64 = 1;
    /// Makes the pin an input.
    pub const INPUT: u64 = 2;
    /// 1 if the pin is an output, 0 if an input.
    pub const DIRECTION: u64 = 3;
}

/// What [`hypercall::POWER`] asks for.
pub mod power {
    pub const OFF: u64 = 0;
    pub const RESTART: u64 = 1;
    pub const CRASHED: u64 = 2;
}

/// Errors of hypercalls, as they return them.
pub mod error {
    /// No such hypercall.
    pub const UNKNOWN: u64 = -1i64 as u64;
    /// An argument was invalid.
    pub const INVALID: u64 = -2i64 as u64;
    /// There is no such thing.
    pub const NOT_FOUND: u64 = -3i64 as u64;
    /// A buffer is too small for what goes there.
    pub const TOO_SMALL: u64 = -4i64 as u64;
}

/// The bytes of a [`hypercall::CONSOLE_WRITE`], from its arguments.
pub fn console_bytes(count: u64, words: [u64; 4]) -> ([u8; 32], usize) {
    let mut out = [0u8; 32];
    for (i, w) in words.iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
    }
    (out, count.min(32) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_bytes_come_in_order() {
        let words = [u64::from_le_bytes(*b"hello, w"), u64::from_le_bytes(*b"orld\n\0\0\0"), 0, 0];
        let (bytes, n) = console_bytes(13, words);
        assert_eq!(&bytes[..n], b"hello, world\n");
        assert_eq!(console_bytes(99, words).1, 32);
    }
}

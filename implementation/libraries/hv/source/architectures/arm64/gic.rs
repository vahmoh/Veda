// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A guest's interrupt controller, as far as the processor does not do it
//! itself: a GICv3's distributor and redistributors, as registers.
//!
//! The processor gives a guest its CPU interface (`ICC_*_EL1`, taken over
//! by the virtual interface) and delivers what the kernel puts in its list
//! registers. What the guest's Linux wants of the rest is little: to turn
//! interrupts on and off, and set their priorities, triggers and routes,
//! at boot and as drivers come. The kernel answers those accesses from
//! here, with the syndrome of the fault as the only decoding (so an access
//! that does not say what it is goes unanswered).
//!
//! The registers are kept as they were written. A guest's interrupts reach
//! a processor by where the platform routed them (its hypercalls), so
//! `GICD_IROUTER` says nothing: it reads back. This code says only which
//! interrupts are on ([`Registers::enabled`]). It has no LPIs: a guest has
//! no ITS, its devices' MSIs being SPIs of the platform's routing.

use super::memory::{GIC_DISTRIBUTOR, GIC_REDISTRIBUTOR_SIZE, GIC_REDISTRIBUTORS};
use super::platform::affinity_of;

/// INTIDs a guest has: SGIs, PPIs and SPIs, the last 224 of which a vector
/// of the platform can name.
pub const INTIDS: usize = 256;

/// The private interrupts, each processor's own.
pub const PRIVATE: usize = 32;

/// `GICD_TYPER`: `ITLinesNumber`, so that [`INTIDS`] are there; `IDbits`,
/// 10 bits of INTID; no LPIs.
const GICD_TYPER: u32 = (INTIDS as u32 / 32 - 1) | 9 << 19;
/// `GICD_IIDR`, `GICR_IIDR`: Arm's, as no one needs it to be anything else.
const IIDR: u32 = 0x43B;
/// The peripheral ID's `ArchRev`: GICv3.
const PIDR2: u32 = 3 << 4;
/// `GICD_CTLR`: the two bits that matter (affinity routing, group 1) stay
/// as written; affinity routing is all there is.
const GICD_CTLR_ARE: u32 = 1 << 4;
const GICD_CTLR_GROUP1: u32 = 1 << 1;
const GICD_CTLR_WRITABLE: u32 = GICD_CTLR_GROUP1 | GICD_CTLR_ARE;
/// `GICR_TYPER`'s `Last`: the last redistributor; `GICR_WAKER`'s
/// `ChildrenAsleep`, which a redistributor never is.
const GICR_TYPER_LAST: u64 = 1 << 4;

/// One redistributor's registers: its SGIs' and PPIs'.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Private {
    enabled: u32,
    config: [u32; 2],
    priority: [u32; PRIVATE / 4],
    group: u32,
}

/// The distributor's registers, for the shared interrupts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shared {
    control: u32,
    /// Bit by INTID, from 0 (the bits of the private ones are unused).
    enabled: [u32; INTIDS / 32],
    group: [u32; INTIDS / 32],
    priority: [u32; INTIDS / 4],
    config: [u32; INTIDS / 16],
    route: [u64; INTIDS],
}

impl Shared {
    pub const fn new() -> Shared {
        Shared {
            control: GICD_CTLR_ARE,
            enabled: [0; INTIDS / 32],
            group: [0; INTIDS / 32],
            priority: [0; INTIDS / 4],
            config: [0; INTIDS / 16],
            route: [0; INTIDS],
        }
    }

    /// Whether the distributor passes interrupts on.
    fn on(&self) -> bool {
        self.control & GICD_CTLR_GROUP1 != 0
    }
}

impl Default for Shared {
    fn default() -> Shared {
        Shared::new()
    }
}

/// What an access to an interrupt controller's register is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write(u64),
}

/// A guest's interrupt controller: the distributor's registers `shared`,
/// and those of its `cpus` redistributors, of which `private` is the one of
/// the processor that makes the access.
#[derive(Debug)]
pub struct Registers<'a> {
    pub shared: &'a mut Shared,
    pub private: &'a mut Private,
    pub cpus: u32,
    pub cpu: u32,
}

/// Bank `bits` set in `*word`, for a set-enable register.
fn set(word: &mut u32, bits: u32) {
    *word |= bits;
}

/// Bank `bits` clear in `*word`, for a clear-enable register.
fn clear(word: &mut u32, bits: u32) {
    *word &= !bits;
}

impl Registers<'_> {
    /// Whether interrupt `intid` reaches its processor.
    pub fn enabled(&self, intid: usize) -> bool {
        let word = (intid / 32).min(INTIDS / 32 - 1);
        let bit = 1 << (intid % 32);
        if intid < PRIVATE { self.private.enabled & bit != 0 } else { self.shared.enabled[word] & bit != 0 }
    }

    /// Answers the access to the register at guest-physical `address` of the
    /// interrupt controller; a read's value, 0 otherwise. What is not here
    /// reads as zero and takes no writes, as reserved registers do.
    pub fn access(&mut self, address: u64, access: Access) -> u64 {
        if address >= GIC_REDISTRIBUTORS {
            let offset = address - GIC_REDISTRIBUTORS;
            let (cpu, offset) = (offset / GIC_REDISTRIBUTOR_SIZE, offset % GIC_REDISTRIBUTOR_SIZE);
            self.redistributor(cpu as u32, offset, access)
        } else {
            self.distributor(address - GIC_DISTRIBUTOR, access)
        }
    }

    /// [`Self::access`] of `size` bytes: registers of priorities are also
    /// reached a byte at a time, which goes through the word they are in.
    pub fn access_sized(&mut self, address: u64, size: u64, access: Access) -> u64 {
        if size >= 4 {
            return self.access(address, access);
        }
        let shift = (address % 4) * 8;
        let mask = ((1u64 << (size * 8)) - 1) << shift;
        let word = self.access(address & !3, Access::Read);
        if let Access::Write(v) = access {
            self.access(address & !3, Access::Write((word & !mask) | ((v << shift) & mask)));
        }
        (word & mask) >> shift
    }

    fn distributor(&mut self, offset: u64, access: Access) -> u64 {
        let s = &mut *self.shared;
        let word = |base: u64| ((offset - base) / 4) as usize;
        let value = if let Access::Write(v) = access { v as u32 } else { 0 };
        let write = matches!(access, Access::Write(_));
        match offset {
            0x0000 => {
                if write {
                    s.control = (value & GICD_CTLR_WRITABLE) | GICD_CTLR_ARE;
                }
                s.control as u64
            }
            0x0004 => GICD_TYPER as u64,
            0x0008 => IIDR as u64,
            0xFFE8 => PIDR2 as u64,
            0x0080..0x0100 if word(0x0080) < s.group.len() => {
                if write {
                    s.group[word(0x0080)] = value;
                }
                s.group[word(0x0080)] as u64
            }
            0x0100..0x0180 if word(0x0100) < s.enabled.len() => {
                set(&mut s.enabled[word(0x0100)], value);
                s.enabled[word(0x0100)] as u64
            }
            0x0180..0x0200 if word(0x0180) < s.enabled.len() => {
                clear(&mut s.enabled[word(0x0180)], value);
                s.enabled[word(0x0180)] as u64
            }
            0x0400..0x0800 if word(0x0400) < s.priority.len() => {
                if write {
                    s.priority[word(0x0400)] = value;
                }
                s.priority[word(0x0400)] as u64
            }
            0x0C00..0x0D00 if word(0x0C00) < s.config.len() => {
                if write {
                    s.config[word(0x0C00)] = value;
                }
                s.config[word(0x0C00)] as u64
            }
            0x6000..0x8000 if ((offset - 0x6000) / 8) < INTIDS as u64 => {
                let at = ((offset - 0x6000) / 8) as usize;
                if let Access::Write(v) = access {
                    s.route[at] = v;
                }
                s.route[at]
            }
            _ => 0,
        }
    }

    fn redistributor(&mut self, cpu: u32, offset: u64, access: Access) -> u64 {
        let value = if let Access::Write(v) = access { v as u32 } else { 0 };
        let write = matches!(access, Access::Write(_));
        let word = |base: u64| ((offset - base) / 4) as usize;
        match offset {
            0x0004 => IIDR as u64,
            0x0008 => {
                // Which processor it is, as `MPIDR_EL1` has its affinity.
                let last = if cpu + 1 == self.cpus { GICR_TYPER_LAST } else { 0 };
                affinity_of(cpu as u64) << 32 | (cpu as u64) << 8 | last
            }
            0xFFE8 => PIDR2 as u64,
            // A processor's own bank is all a guest touches; another's reads
            // as nothing.
            _ if cpu != self.cpu => 0,
            0x1_0080 => {
                if write {
                    self.private.group = value;
                }
                self.private.group as u64
            }
            0x1_0100 => {
                set(&mut self.private.enabled, value);
                self.private.enabled as u64
            }
            0x1_0180 => {
                clear(&mut self.private.enabled, value);
                self.private.enabled as u64
            }
            0x1_0400..0x1_0420 => {
                if write {
                    self.private.priority[word(0x1_0400)] = value;
                }
                self.private.priority[word(0x1_0400)] as u64
            }
            0x1_0C00..0x1_0C08 => {
                if write {
                    self.private.config[word(0x1_0C00)] = value;
                }
                self.private.config[word(0x1_0C00)] as u64
            }
            _ => 0,
        }
    }
}

/// Whether the distributor lets the guest have shared interrupts at all.
pub fn distributor_on(shared: &Shared) -> bool {
    shared.on()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(cpu: u32, f: impl FnOnce(&mut Registers<'_>)) {
        let (mut shared, mut private) = (Shared::new(), Private::default());
        f(&mut Registers { shared: &mut shared, private: &mut private, cpus: 2, cpu });
    }

    #[test]
    fn linux_finds_a_gicv3_of_our_size() {
        with(0, |r| {
            let typer = r.access(GIC_DISTRIBUTOR + 4, Access::Read) as u32;
            assert_eq!(32 * ((typer & 0x1F) + 1), INTIDS as u32);
            assert_eq!(r.access(GIC_DISTRIBUTOR + 0xFFE8, Access::Read) >> 4 & 0xF, 3);
            assert_eq!(r.access(GIC_REDISTRIBUTORS + 0xFFE8, Access::Read) >> 4 & 0xF, 3);
        });
    }

    #[test]
    fn redistributors_say_whose_they_are_and_the_last_says_so() {
        with(0, |r| {
            let first = r.access(GIC_REDISTRIBUTORS + 8, Access::Read);
            let second = r.access(GIC_REDISTRIBUTORS + GIC_REDISTRIBUTOR_SIZE + 8, Access::Read);
            assert_eq!((first >> 32, first & GICR_TYPER_LAST), (0, 0));
            assert_eq!((second >> 32, second & GICR_TYPER_LAST), (1, GICR_TYPER_LAST));
        });
    }

    #[test]
    fn shared_interrupts_turn_on_and_off() {
        with(0, |r| {
            assert!(!r.enabled(40));
            r.access(GIC_DISTRIBUTOR + 0x104, Access::Write(1 << 8));
            assert!(r.enabled(40));
            r.access(GIC_DISTRIBUTOR + 0x184, Access::Write(1 << 8));
            assert!(!r.enabled(40));
        });
    }

    #[test]
    fn private_interrupts_are_a_processors_own() {
        with(1, |r| {
            let own = GIC_REDISTRIBUTORS + GIC_REDISTRIBUTOR_SIZE + 0x1_0100;
            r.access(own, Access::Write(1 << 27));
            assert!(r.enabled(27));
            // The other processor's bank is left alone.
            r.access(GIC_REDISTRIBUTORS + 0x1_0180, Access::Write(1 << 27));
            assert!(r.enabled(27));
        });
    }

    #[test]
    fn what_is_written_reads_back() {
        with(0, |r| {
            let priority = GIC_DISTRIBUTOR + 0x400 + 40;
            r.access(priority, Access::Write(0xA0A0_A0A0));
            assert_eq!(r.access(priority, Access::Read), 0xA0A0_A0A0);
            let route = GIC_DISTRIBUTOR + 0x6000 + 40 * 8;
            r.access(route, Access::Write(0x101));
            assert_eq!(r.access(route, Access::Read), 0x101);
        });
    }

    #[test]
    fn priorities_are_reached_a_byte_at_a_time() {
        with(0, |r| {
            let priority = GIC_DISTRIBUTOR + 0x400 + 41;
            r.access_sized(priority, 1, Access::Write(0xA0));
            assert_eq!(r.access_sized(priority, 1, Access::Read), 0xA0);
            assert_eq!(r.access(GIC_DISTRIBUTOR + 0x400 + 40, Access::Read), 0xA000);
        });
    }

    #[test]
    fn what_is_beyond_the_registers_is_nothing() {
        with(0, |r| {
            assert_eq!(r.access(GIC_DISTRIBUTOR + 0x1000 - 4, Access::Read), 0);
            assert_eq!(r.access(GIC_DISTRIBUTOR + 0x6000 + 8 * INTIDS as u64, Access::Write(1)), 0);
            assert_eq!(r.access(GIC_DISTRIBUTOR + 0x0100 + 4 * 16, Access::Write(1)), 0);
        });
    }
}

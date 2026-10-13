//! What `init` knows of AMD64 machines: the extent of their hardware
//! resources, which it delegates to the device manager.

use core::ops::Range;

/// The I/O port space.
pub const IO_PORTS: Range<u64> = 0..0x1_0000;
/// The interrupt lines (GSIs) devices may have.
pub const IRQS: Range<u64> = 0..256;
/// The physical addresses devices' registers may be at.
pub const MMIO: Range<u64> = 0..1 << 46;

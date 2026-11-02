// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What `init` knows of ARM64 machines: the extent of their hardware
//! resources, which it delegates to the device manager.

use core::ops::Range;

/// The I/O port space, which these processors do not have: the resource is
/// there as on every machine, and no port object does anything.
pub const IO_PORTS: Range<u64> = 0..0x1_0000;
/// The interrupt lines (GSIs) devices may have: the GIC's SPIs.
pub const IRQS: Range<u64> = 32..1020;
/// The physical addresses devices' registers may be at.
pub const MMIO: Range<u64> = 0..1 << 48;

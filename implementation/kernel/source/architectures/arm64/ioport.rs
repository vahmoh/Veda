// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The I/O port space, which ARM64 processors do not have: no I/O port
//! object reads or writes anything.

/// Reads from `port`: nothing.
///
/// # Safety
/// None needed; as on architectures with ports.
pub unsafe fn read(_port: u16, _width: usize) -> Option<u32> {
    None
}

/// Writes to `port`: false, as nothing is there.
///
/// # Safety
/// None needed; as on architectures with ports.
pub unsafe fn write(_port: u16, _width: usize, _value: u32) -> bool {
    false
}

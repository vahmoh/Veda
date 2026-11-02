// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Linux's boot protocol on ARM64 (`Documentation/arch/arm64/booting.rst`):
//! the header of an `Image`, where it goes in memory, and how the first
//! processor enters it: in EL1 with its MMU off, the device tree's address
//! in `x0`.

use abi::arm64::VcpuState;

/// The header's magic number ("ARM\x64") and where it is.
const MAGIC: u32 = 0x644D_5241;
const MAGIC_AT: usize = 56;
/// Where the text starts in the image's place, and the image's size in
/// memory, little-endian 64-bit words.
const TEXT_OFFSET_AT: usize = 8;
const IMAGE_SIZE_AT: usize = 16;

/// An image is placed on a 2 MiB boundary.
pub const ALIGNMENT: u64 = 2 << 20;

/// The processor's state on entry: EL1 on its own stack pointer, every
/// exception masked.
const PSTATE_EL1H: u64 = 0xF << 6 | 5;

/// What the header of an `Image` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image {
    /// How far above a 2 MiB boundary it goes, to start at its first byte.
    pub text_offset: u64,
    /// How much memory it takes from there, what it clears included.
    pub memory_size: u64,
}

/// What a kernel of bytes `file` says of itself. `Err` if it is no `Image`.
pub fn parse(file: &[u8]) -> Result<Image, &'static str> {
    let word = |at: usize| -> Option<u64> { Some(u64::from_le_bytes(file.get(at..at + 8)?.try_into().ok()?)) };
    let magic = file.get(MAGIC_AT..MAGIC_AT + 4).ok_or("the kernel is too short to be an Image")?;
    if u32::from_le_bytes([magic[0], magic[1], magic[2], magic[3]]) != MAGIC {
        return Err("the kernel is no ARM64 Image");
    }
    let (Some(text_offset), Some(size)) = (word(TEXT_OFFSET_AT), word(IMAGE_SIZE_AT)) else {
        return Err("the kernel is too short to be an Image");
    };
    Ok(Image { text_offset, memory_size: if size == 0 { file.len() as u64 } else { size } })
}

/// The state the first processor enters Linux in, at `entry`, with its
/// device tree at `fdt`.
pub fn state(entry: u64, fdt: u64) -> VcpuState {
    let mut state = VcpuState { pc: entry, pstate: PSTATE_EL1H, ..VcpuState::default() };
    state.x[0] = fdt;
    state
}

/// The state a processor starts in at `entry`, with `context` in `x0`: what
/// PSCI's `CPU_ON` starts it with.
pub fn secondary(entry: u64, context: u64) -> VcpuState {
    state(entry, context)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(text_offset: u64, size: u64) -> alloc::vec::Vec<u8> {
        let mut file = alloc::vec![0u8; 64];
        file[TEXT_OFFSET_AT..TEXT_OFFSET_AT + 8].copy_from_slice(&text_offset.to_le_bytes());
        file[IMAGE_SIZE_AT..IMAGE_SIZE_AT + 8].copy_from_slice(&size.to_le_bytes());
        file[MAGIC_AT..MAGIC_AT + 4].copy_from_slice(&MAGIC.to_le_bytes());
        file
    }

    #[test]
    fn an_image_says_where_it_goes() {
        assert_eq!(parse(&image(0, 0x200_0000)), Ok(Image { text_offset: 0, memory_size: 0x200_0000 }));
        // No size (an old kernel): the file's.
        assert_eq!(parse(&image(0x8_0000, 0)), Ok(Image { text_offset: 0x8_0000, memory_size: 64 }));
    }

    #[test]
    fn what_is_no_image_is_refused() {
        assert!(parse(&[0; 8]).is_err());
        assert!(parse(&[0; 64]).is_err());
    }

    #[test]
    fn a_processor_starts_in_el1_with_exceptions_masked() {
        let s = state(0x20_0000, 0x400_0000);
        assert_eq!((s.pc, s.x[0], s.pstate), (0x20_0000, 0x400_0000, 0x3C5));
    }
}

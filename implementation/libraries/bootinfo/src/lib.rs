// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The boot handoff protocol between `bootloader` (the UEFI loader) and `kernel`.
//!
//! The loader jumps to the kernel's entry point with paging enabled, on
//! page tables of its own that map:
//!
//! * low physical memory where it is (so the loader can survive the switch;
//!   the kernel drops it as soon as it installs its own tables),
//! * all physical RAM, the direct map, at [`HHDM_BASE`],
//! * the kernel image at its link address, [`KERNEL_BASE`], each page
//!   with the permissions of its segment ([`KernelImage::runs`]).
//!
//! The entry point takes the [`BootInfo`]. Every pointer stored inside it is
//! a *virtual* address inside the direct map, so the kernel can use it
//! immediately. How the processor is entered, and where the direct map and
//! the kernel are, is the architecture's part of the protocol
//! ([`architecture`]).
//!
//! All structures are `#[repr(C)]` and contain only plain data, because the
//! loader and kernel are separate binaries that must agree on the layout.

#![no_std]

#[cfg(target_arch = "x86_64")]
#[path = "architectures/amd64/mod.rs"]
pub mod architecture;
pub mod variables;

pub use architecture::{HHDM_BASE, KERNEL_BASE, memory_type};

/// `BootInfo::magic` value (`"VEDABOOT"` in little-endian).
pub const BOOTINFO_MAGIC: u64 = u64::from_le_bytes(*b"VEDABOOT");

/// Version of this protocol. Bump on any layout change.
pub const BOOTINFO_VERSION: u32 = 7;

/// Maximum length of the kernel command line, in bytes.
pub const CMDLINE_MAX: usize = 256;

/// Size of the entropy seed the loader passes to the kernel.
pub const ENTROPY_MAX: usize = 64;

/// Everything the kernel needs to know about the machine at boot.
#[repr(C)]
#[derive(Debug)]
pub struct BootInfo {
    /// Must equal [`BOOTINFO_MAGIC`].
    pub magic: u64,
    /// Must equal [`BOOTINFO_VERSION`].
    pub version: u32,
    /// Size of this structure in bytes (sanity check).
    pub size: u32,
    /// Virtual address at which physical address 0 is mapped.
    pub hhdm_base: u64,
    /// Highest physical address (exclusive) covered by the direct map.
    pub hhdm_limit: u64,
    /// Physical memory layout, sorted by start address, non-overlapping.
    pub memory_map: MemoryMap,
    /// The loaded kernel image.
    pub kernel: KernelImage,
    /// Linear framebuffer configured through UEFI GOP.
    pub framebuffer: Framebuffer,
    /// The initial ramdisk (a `VINITRD` archive), see the `initrd` crate.
    pub initrd: PhysRegion,
    /// Physical address of the ACPI RSDP, or 0 if the firmware has none.
    pub rsdp_phys: u64,
    /// Wall-clock time read from the firmware just before handoff.
    pub boot_time: BootTime,
    /// Stack the kernel was entered on (physical range).
    pub boot_stack: PhysRegion,
    /// Kernel command line (UTF-8, not NUL terminated).
    pub cmdline: [u8; CMDLINE_MAX],
    /// Number of valid bytes in [`BootInfo::cmdline`].
    pub cmdline_len: u32,
    /// Number of valid bytes in [`BootInfo::entropy`].
    pub entropy_len: u32,
    /// Random bytes from the firmware's `EFI_RNG_PROTOCOL` (if it has one),
    /// which seed the kernel's random number generator.
    pub entropy: [u8; ENTROPY_MAX],
    /// How the loader cleared the screen, for the kernel's log.
    pub paint: PaintReport,
    /// The screen's modes and the display's own size, for the kernel's log.
    pub screen: ScreenReport,
    /// The firmware's variables that an operating system may read, as the
    /// loader read them ([`variables`]); empty if it found none.
    pub firmware_variables: PhysRegion,
}

/// How the loader cleared the screen (it paints it black): how long it
/// took, and the framebuffer's memory type. Firmware often leaves the
/// framebuffer uncached, where every write is a bus transaction of its own
/// and the screen is seen being painted; write-combining makes it a burst.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PaintReport {
    /// Ticks of the clock's counter the painting took (the counter the
    /// kernel's clock counts; 0: nothing was painted).
    pub paint_ticks: u64,
    /// Ticks spent making the framebuffer write-combining.
    pub setup_ticks: u64,
    /// The framebuffer's memory type ([`memory_type`]) as the loader found
    /// it, and as it painted.
    pub found: u8,
    pub painted: u8,
    /// How it was made write-combining ([`made_wc`]).
    pub made_wc: u8,
    pub _pad: [u8; 5],
}

/// How many of the firmware's graphics modes [`ScreenReport`] holds.
pub const SCREEN_MODES: usize = 32;

/// The graphics modes the firmware offers (of 32-bit pixels: those the
/// loader can use), which one the loader chose and which one the firmware
/// had on, and the display's own size as its EDID gives it (the panel's
/// native resolution), for the kernel's log.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ScreenReport {
    /// Width and height of each, in the firmware's order.
    pub modes: [[u16; 2]; SCREEN_MODES],
    pub count: u8,
    /// Indices into `modes` (`u8::MAX`: none of them).
    pub chosen: u8,
    pub entered: u8,
    pub _pad: u8,
    /// Width and height (0: the firmware gave no EDID).
    pub own: [u16; 2],
}

/// How the framebuffer was made write-combining for painting.
pub mod made_wc {
    /// It was not: it was already, or there is no framebuffer.
    pub const ALREADY: u8 = 0;
    /// The loader painted through page tables of its own, which map the
    /// framebuffer write-combining by its page attributes, whatever the
    /// firmware made of its range.
    pub const PAGE_ATTRIBUTES: u8 = 1;
    /// It could not be.
    pub const NOT: u8 = 2;
}

impl BootInfo {
    /// Returns the command line as a string slice (empty if not valid UTF-8).
    pub fn cmdline(&self) -> &str {
        let len = (self.cmdline_len as usize).min(CMDLINE_MAX);
        core::str::from_utf8(&self.cmdline[..len]).unwrap_or("")
    }

    /// The firmware entropy seed (empty if the firmware had none).
    pub fn entropy(&self) -> &[u8] {
        &self.entropy[..(self.entropy_len as usize).min(ENTROPY_MAX)]
    }

    /// Returns `true` if the magic, version and size fields are what this
    /// build of the protocol expects.
    pub fn is_valid(&self) -> bool {
        self.magic == BOOTINFO_MAGIC
            && self.version == BOOTINFO_VERSION
            && self.size as usize == core::mem::size_of::<BootInfo>()
    }
}

/// A physical address range.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhysRegion {
    pub base: u64,
    pub size: u64,
}

impl PhysRegion {
    pub const EMPTY: PhysRegion = PhysRegion { base: 0, size: 0 };

    pub const fn end(&self) -> u64 {
        self.base + self.size
    }

    pub const fn is_empty(&self) -> bool {
        self.size == 0
    }
}

/// Most runs of pages ([`KernelImage::runs`]) a kernel image may have:
/// twice what a linker makes of one (read-only data, code, RELRO, data).
pub const KERNEL_RUNS_MAX: usize = 8;

/// Where the kernel image lives in physical and virtual memory, and how
/// its pages may be used.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KernelImage {
    /// Physical address of its first page (the image is contiguous).
    pub phys_base: u64,
    /// Virtual address of its first page, [`KERNEL_BASE`].
    pub virt_base: u64,
    /// Its size in bytes, whole pages.
    pub size: u64,
    /// How many of `runs` there are.
    pub run_count: u64,
    /// The image's pages by permissions, in address order, from its
    /// executable's segments (RELRO read-only): the loader maps them so,
    /// and so does the kernel's own address space. Pages of no run are not
    /// mapped.
    pub runs: [KernelRun; KERNEL_RUNS_MAX],
}

impl KernelImage {
    /// The runs of pages there are.
    pub fn runs(&self) -> &[KernelRun] {
        &self.runs[..(self.run_count as usize).min(KERNEL_RUNS_MAX)]
    }
}

/// Pages of the kernel image with the same permissions (all are readable).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KernelRun {
    /// Where they start, from the image's first page, and how many bytes
    /// (whole pages) they take.
    pub offset: u64,
    pub len: u64,
    pub writable: bool,
    pub executable: bool,
    pub _pad: [u8; 6],
}

/// The physical memory map, an array of [`MemoryRegion`]s.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MemoryMap {
    /// Virtual (direct-map) address of the first entry.
    pub entries: *const MemoryRegion,
    pub len: u64,
}

impl MemoryMap {
    /// # Safety
    /// `entries` must point to `len` valid regions that outlive the slice.
    pub unsafe fn as_slice(&self) -> &[MemoryRegion] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: guaranteed by the caller (the loader built this array).
        unsafe { core::slice::from_raw_parts(self.entries, self.len as usize) }
    }
}

/// One contiguous range of physical memory.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegion {
    pub base: u64,
    pub pages: u64,
    pub kind: MemoryKind,
    pub _pad: u32,
}

impl MemoryRegion {
    pub const PAGE_SIZE: u64 = 4096;

    pub const fn end(&self) -> u64 {
        self.base + self.pages * Self::PAGE_SIZE
    }
}

/// How a physical memory range may be used by the kernel.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryKind {
    /// Free RAM.
    Usable = 1,
    /// Firmware reserved; never touch.
    Reserved = 2,
    /// ACPI tables; reclaimable once the kernel has parsed them.
    AcpiReclaimable = 3,
    /// ACPI non-volatile storage; never touch.
    AcpiNvs = 4,
    /// Memory-mapped I/O.
    Mmio = 5,
    /// Loader code and scratch data. Free once the kernel no longer needs the
    /// loader's identity map (i.e. after it has switched page tables).
    LoaderReclaimable = 6,
    /// The kernel image.
    Kernel = 7,
    /// The initial ramdisk.
    Initrd = 8,
    /// Boot info, memory map, boot stack and the loader-built page tables.
    BootData = 9,
    /// Defective or otherwise unusable RAM.
    Unusable = 10,
    /// The firmware's variables ([`variables`]).
    FirmwareVariables = 12,
}

impl MemoryKind {
    /// RAM that the kernel may hand out once boot-time data is consumed.
    pub const fn is_reclaimable_ram(self) -> bool {
        matches!(self, MemoryKind::Usable | MemoryKind::LoaderReclaimable)
    }
}

/// Pixel layout of a 32-bit-per-pixel framebuffer.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// Byte order B, G, R, X in memory (`0xXXRRGGBB` as a little-endian u32).
    Bgrx = 1,
    /// Byte order R, G, B, X in memory.
    Rgbx = 2,
}

/// A linear framebuffer.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Framebuffer {
    /// Physical address of the first pixel; 0 if no framebuffer is present.
    pub phys_base: u64,
    /// Size of the framebuffer memory in bytes.
    pub size: u64,
    pub width: u32,
    pub height: u32,
    /// Pixels (not bytes) per scan line.
    pub stride: u32,
    pub format: PixelFormat,
}

/// Calendar time as reported by the firmware's real-time clock.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct BootTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// 1 if the fields above are valid.
    pub valid: u8,
    /// Offset from UTC in minutes, or `i16::MAX` if unknown (assume UTC).
    pub utc_offset_minutes: i16,
    pub _pad: [u8; 6],
}

// The layouts are shared between two separately compiled binaries: catch
// accidental changes at compile time.
const _: () = {
    assert!(core::mem::size_of::<MemoryRegion>() == 24);
    assert!(core::mem::size_of::<Framebuffer>() == 32);
    assert!(core::mem::size_of::<BootTime>() == 16);
    assert!(core::mem::size_of::<PaintReport>() == 24);
};

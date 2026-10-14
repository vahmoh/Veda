// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `velf` — Veda's executables: ELF64.
//!
//! Every program Veda runs, and its kernel, is a static ELF64 executable for
//! the System V ABI of its processor (x86-64's on AMD64):
//!
//! * the kernel, linked at its fixed address, which the boot loader maps;
//! * Veda's own programs, position-independent, which their loader places
//!   at a random address and relocates;
//! * POSIX programs (C, built by Veda's GCC; Rust on its standard library),
//!   which start as on Linux and relocate themselves if they are
//!   position-independent.
//!
//! Loading one takes:
//!
//! 1. [`Elf::parse`], which validates the header and every program header,
//!    so a loader can trust what it gets from then on;
//! 2. copying each [`Segment`]'s file bytes into memory at its address
//!    (plus the load bias of a position-independent executable), the rest
//!    of the segment zero;
//! 3. for a program that does not relocate itself, [`Elf::relocate`] with
//!    its load bias;
//! 4. mapping the [`page_runs`], the image's pages grouped by permissions;
//! 5. starting it at [`Elf::entry`]: a POSIX program with the stack
//!    [`initial_stack`] lays out (arguments, environment, auxiliary vector).
//!
//! Executables that need a dynamic linker (`PT_INTERP`, `DT_NEEDED`) are
//! refused: Veda links every program statically.
//!
//! An executable is for one processor ([`Machine`]): each architecture's
//! is in `implementation/libraries/elf/src/arch/<architecture>/`
//! ([`amd64`]), on every host, so that the build machine's tools check
//! executables for the target. [`NATIVE`] is the processor the code runs
//! on, whose programs it loads.
//!
//! The crate is plain data processing, unit-tested on the host, and
//! allocates nothing: the boot loader loads the kernel with it before any
//! allocator exists.

#![no_std]

use core::fmt;

#[path = "arch/amd64/mod.rs"]
pub mod amd64;

/// The processor an executable's code is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Machine {
    /// The ELF header's `e_machine`.
    pub code: u16,
    /// Its name, for errors.
    pub name: &'static str,
    /// The platform the auxiliary vector names (`AT_PLATFORM`).
    pub platform: &'static [u8],
    /// The type of its relative relocation: a word that becomes the load
    /// bias plus its addend, the only relocation a static
    /// position-independent executable has.
    pub relative: u32,
}

/// The processor this code runs on.
#[cfg(target_arch = "x86_64")]
pub const NATIVE: Machine = amd64::MACHINE;

/// The note that marks a Veda program, one built on Veda's runtime (`vrt`,
/// which carries it): its loader relocates it, makes its RELRO read-only,
/// and starts it with Veda's startup message (`vabi::startup`). POSIX
/// programs have none. Its descriptor is the version of these conventions
/// the program follows ([`VERSION`](veda::VERSION), little-endian), so that
/// a loader refuses a program of others ([`Kind`]).
pub mod veda {
    /// The note's owner (`n_name`, without its terminating NUL).
    pub const NOTE_NAME: &[u8] = b"Veda";
    /// Its type (`n_type`).
    pub const NOTE_TYPE: u32 = 1;
    /// The version of the conventions.
    pub const VERSION: u32 = 1;
}

/// Which kind of program an executable is, by its note ([`veda`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A Veda program: its loader relocates it.
    Veda,
    /// A Veda program of conventions of another version than this crate's.
    OtherVeda,
    /// A POSIX program: it starts as the System V ABI has it, and relocates
    /// itself if it is position-independent.
    Posix,
}

/// Errors produced while parsing an executable, relocating it or laying
/// out its stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    /// The file is shorter than a header it claims to contain.
    Truncated,
    /// Not an ELF file.
    BadMagic,
    /// Not a 64-bit little-endian ELF file of the current version.
    WrongClass,
    /// Not a file for the processor named.
    WrongMachine(&'static str),
    /// A relocatable object or core dump, not an executable.
    NotExecutable,
    /// The executable needs a dynamic linker or libraries.
    Dynamic,
    /// A program header is malformed, segments overlap, or there are more
    /// than [`MAX_SEGMENTS`].
    BadSegment,
    /// Nothing to load, or the program headers are not in memory.
    NoSegments,
    /// The entry point is not in an executable segment.
    BadEntry,
    /// A relocation is malformed, of a type a static executable does not
    /// have, or outside the writable segments.
    BadRelocation,
    /// The image or its arguments are larger than the space for them.
    TooLarge,
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ElfError::Truncated => "the file is truncated",
            ElfError::BadMagic => "not an ELF file",
            ElfError::WrongClass => "not a 64-bit little-endian ELF file",
            ElfError::WrongMachine(machine) => return write!(f, "not an {machine} program"),
            ElfError::NotExecutable => "not an executable (an object file or core dump)",
            ElfError::Dynamic => "dynamically linked (Veda runs static executables)",
            ElfError::BadSegment => "malformed program header",
            ElfError::NoSegments => "nothing to load",
            ElfError::BadEntry => "the entry point is not in executable code",
            ElfError::BadRelocation => "malformed relocation",
            ElfError::TooLarge => "too large",
        })
    }
}

pub const PAGE_SIZE: u64 = 4096;
/// Size of an ELF64 file header.
const EHDR_SIZE: usize = 64;
/// Size of an ELF64 program header.
pub const PHDR_SIZE: usize = 56;
/// Size of an entry of the dynamic section, of a `DT_RELA` relocation and
/// of a `DT_RELR` word.
const DYN_SIZE: usize = 16;
const RELA_SIZE: usize = 24;
const RELR_SIZE: usize = 8;
/// Largest image (from its lowest to its highest page) accepted.
const MAX_IMAGE: u64 = 4 << 30;
/// Most loadable segments accepted: many more than linkers make (four or
/// five), and few enough that the loader's work on them stays small.
pub const MAX_SEGMENTS: usize = 64;

const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;

/// Program header types.
pub mod pt {
    pub const LOAD: u32 = 1;
    pub const DYNAMIC: u32 = 2;
    pub const INTERP: u32 = 3;
    pub const NOTE: u32 = 4;
    pub const PHDR: u32 = 6;
    pub const TLS: u32 = 7;
    pub const GNU_STACK: u32 = 0x6474_e551;
    pub const GNU_RELRO: u32 = 0x6474_e552;
}

/// Segment permission flags (`p_flags`).
pub mod pf {
    pub const X: u32 = 1;
    pub const W: u32 = 2;
    pub const R: u32 = 4;
}

/// Dynamic section tags (`d_tag`) that the loader reads.
mod dt {
    pub const NULL: u64 = 0;
    pub const NEEDED: u64 = 1;
    pub const PLTRELSZ: u64 = 2;
    pub const RELA: u64 = 7;
    pub const RELASZ: u64 = 8;
    pub const RELAENT: u64 = 9;
    pub const RELSZ: u64 = 18;
    pub const TEXTREL: u64 = 22;
    pub const FLAGS: u64 = 30;
    pub const RELRSZ: u64 = 35;
    pub const RELR: u64 = 36;
    pub const RELRENT: u64 = 37;
    /// `DT_FLAGS`' flag for relocations of read-only segments.
    pub const DF_TEXTREL: u64 = 4;
}

/// The relocation type that does nothing, on every processor.
const R_NONE: u32 = 0;

/// Memory permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Perms {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
}

impl Perms {
    fn from_flags(flags: u32) -> Perms {
        Perms { read: flags & pf::R != 0, write: flags & pf::W != 0, exec: flags & pf::X != 0 }
    }

    fn union(self, o: Perms) -> Perms {
        Perms { read: self.read || o.read, write: self.write || o.write, exec: self.exec || o.exec }
    }
}

fn u16_at(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([d[off], d[off + 1]])
}

fn u32_at(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(d[off..off + 4].try_into().unwrap())
}

fn u64_at(d: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(d[off..off + 8].try_into().unwrap())
}

fn page_down(a: u64) -> u64 {
    a & !(PAGE_SIZE - 1)
}

fn page_up(a: u64) -> Option<u64> {
    Some(a.checked_add(PAGE_SIZE - 1)? & !(PAGE_SIZE - 1))
}

/// The `size` bytes of the file at `offset`, if it has them.
fn file_range(data: &[u8], offset: u64, size: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    data.get(start..start.checked_add(usize::try_from(size).ok()?)?)
}

/// A program header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramHeader {
    pub kind: u32,
    pub flags: u32,
    pub offset: u64,
    pub vaddr: u64,
    pub file_size: u64,
    pub mem_size: u64,
    pub align: u64,
}

impl ProgramHeader {
    fn read(d: &[u8], off: usize) -> ProgramHeader {
        ProgramHeader {
            kind: u32_at(d, off),
            flags: u32_at(d, off + 4),
            offset: u64_at(d, off + 8),
            vaddr: u64_at(d, off + 16),
            file_size: u64_at(d, off + 32),
            mem_size: u64_at(d, off + 40),
            align: u64_at(d, off + 48),
        }
    }
}

/// A loadable segment: `data` belongs at `vaddr`, followed by zeros up to
/// `mem_size` bytes.
#[derive(Debug, Clone, Copy)]
pub struct Segment<'a> {
    pub vaddr: u64,
    pub mem_size: u64,
    pub data: &'a [u8],
    pub perms: Perms,
}

impl Segment<'_> {
    /// One past its last byte in memory (before the load bias).
    pub fn end(&self) -> u64 {
        self.vaddr + self.mem_size
    }
}

/// A note of the executable's (in a `PT_NOTE` segment): whose it is, its
/// type, and what it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Note<'a> {
    /// The owner's name, without its terminating NUL.
    pub name: &'a [u8],
    pub kind: u32,
    pub desc: &'a [u8],
}

/// A validated ELF64 executable.
#[derive(Debug, Clone, Copy)]
pub struct Elf<'a> {
    data: &'a [u8],
    machine: Machine,
    pie: bool,
    entry: u64,
    phoff: usize,
    phnum: usize,
    phdr_vaddr: u64,
    lo: u64,
    hi: u64,
    /// The dynamic section's file bytes (`PT_DYNAMIC`): offset and size.
    dynamic: Option<(u64, u64)>,
    /// `PT_GNU_RELRO`: address and size.
    relro: Option<(u64, u64)>,
}

/// Whether `data` starts like an ELF file.
pub fn is_elf(data: &[u8]) -> bool {
    data.starts_with(b"\x7fELF")
}

impl<'a> Elf<'a> {
    /// Parses and validates an executable for the processor this code runs
    /// on ([`NATIVE`]).
    #[cfg(target_arch = "x86_64")]
    pub fn parse(data: &'a [u8]) -> Result<Elf<'a>, ElfError> {
        Self::parse_for(data, NATIVE)
    }

    /// Parses and validates an executable for `machine`.
    pub fn parse_for(data: &'a [u8], machine: Machine) -> Result<Elf<'a>, ElfError> {
        if data.len() < EHDR_SIZE {
            return Err(if is_elf(data) || data.len() < 4 { ElfError::Truncated } else { ElfError::BadMagic });
        }
        if !is_elf(data) {
            return Err(ElfError::BadMagic);
        }
        // Class 64, little endian, version 1.
        if data[4] != 2 || data[5] != 1 || data[6] != 1 || u32_at(data, 20) != 1 {
            return Err(ElfError::WrongClass);
        }
        if u16_at(data, 18) != machine.code {
            return Err(ElfError::WrongMachine(machine.name));
        }
        let pie = match u16_at(data, 16) {
            ET_EXEC => false,
            ET_DYN => true,
            _ => return Err(ElfError::NotExecutable),
        };
        let entry = u64_at(data, 24);
        let phoff = u64_at(data, 32);
        let phentsize = u16_at(data, 54) as usize;
        let phnum = u16_at(data, 56) as usize;
        if phnum == 0 {
            return Err(ElfError::NoSegments);
        }
        if phentsize != PHDR_SIZE {
            return Err(ElfError::BadSegment);
        }
        let ph_end = phoff.checked_add((phnum * PHDR_SIZE) as u64).ok_or(ElfError::Truncated)?;
        if ph_end > data.len() as u64 {
            return Err(ElfError::Truncated);
        }
        let phoff = phoff as usize;

        let mut lo = u64::MAX;
        let mut hi = 0u64;
        let mut prev_end = 0u64;
        let mut phdr_vaddr = None;
        let mut dynamic = None;
        let mut relro = None;
        let mut loads = 0;
        for i in 0..phnum {
            let ph = ProgramHeader::read(data, phoff + i * PHDR_SIZE);
            match ph.kind {
                pt::INTERP => return Err(ElfError::Dynamic),
                pt::PHDR => phdr_vaddr = Some(ph.vaddr),
                pt::DYNAMIC => {
                    file_range(data, ph.offset, ph.file_size).ok_or(ElfError::BadSegment)?;
                    dynamic = Some((ph.offset, ph.file_size));
                }
                pt::GNU_RELRO => {
                    ph.vaddr.checked_add(ph.mem_size).ok_or(ElfError::BadSegment)?;
                    relro = Some((ph.vaddr, ph.mem_size));
                }
                pt::LOAD if ph.mem_size > 0 => {
                    let file_end = ph.offset.checked_add(ph.file_size).ok_or(ElfError::BadSegment)?;
                    let end = ph.vaddr.checked_add(ph.mem_size).ok_or(ElfError::BadSegment)?;
                    if ph.file_size > ph.mem_size || file_end > data.len() as u64 || loads == MAX_SEGMENTS {
                        return Err(ElfError::BadSegment);
                    }
                    // Segments come in ascending address order and must not
                    // overlap (they may share a page).
                    if loads > 0 && ph.vaddr < prev_end {
                        return Err(ElfError::BadSegment);
                    }
                    prev_end = end;
                    lo = lo.min(page_down(ph.vaddr));
                    hi = hi.max(page_up(end).ok_or(ElfError::BadSegment)?);
                    // The program headers live wherever their file bytes
                    // were loaded.
                    if phdr_vaddr.is_none() && ph.offset <= phoff as u64 && ph_end <= file_end {
                        phdr_vaddr = Some(ph.vaddr + (phoff as u64 - ph.offset));
                    }
                    loads += 1;
                }
                _ => {}
            }
        }
        if loads == 0 {
            return Err(ElfError::NoSegments);
        }
        let phdr_vaddr = phdr_vaddr.ok_or(ElfError::NoSegments)?;
        if hi - lo > MAX_IMAGE {
            return Err(ElfError::TooLarge);
        }
        let elf = Elf { data, machine, pie, entry, phoff, phnum, phdr_vaddr, lo, hi, dynamic, relro };
        if !elf.segments().any(|s| s.perms.exec && s.vaddr <= entry && entry < s.end()) {
            return Err(ElfError::BadEntry);
        }
        // The C library reads the program headers where AT_PHDR says (to
        // find its thread-local storage): among the bytes loaded from the
        // file.
        let phdr_end = phdr_vaddr.checked_add((phnum * PHDR_SIZE) as u64).ok_or(ElfError::NoSegments)?;
        if !elf.segments().any(|s| s.vaddr <= phdr_vaddr && phdr_end <= s.vaddr + s.data.len() as u64) {
            return Err(ElfError::NoSegments);
        }
        Ok(elf)
    }

    /// A position-independent executable, loadable at any page-aligned bias.
    pub fn is_position_independent(&self) -> bool {
        self.pie
    }

    /// The entry point (before the load bias).
    pub fn entry(&self) -> u64 {
        self.entry
    }

    /// Where the program headers are in memory (before the load bias).
    pub fn phdr_vaddr(&self) -> u64 {
        self.phdr_vaddr
    }

    /// The number of program headers.
    pub fn phnum(&self) -> usize {
        self.phnum
    }

    /// The page-aligned address range `[lo, hi)` the image occupies (before
    /// the load bias).
    pub fn span(&self) -> (u64, u64) {
        (self.lo, self.hi)
    }

    /// Every program header.
    pub fn program_headers(&self) -> impl Iterator<Item = ProgramHeader> + 'a {
        let (data, phoff) = (self.data, self.phoff);
        (0..self.phnum).map(move |i| ProgramHeader::read(data, phoff + i * PHDR_SIZE))
    }

    /// The loadable segments, in ascending address order.
    pub fn segments(&self) -> impl Iterator<Item = Segment<'a>> + 'a {
        let data = self.data;
        self.program_headers().filter(|ph| ph.kind == pt::LOAD && ph.mem_size > 0).map(move |ph| Segment {
            vaddr: ph.vaddr,
            mem_size: ph.mem_size,
            data: &data[ph.offset as usize..(ph.offset + ph.file_size) as usize],
            perms: Perms::from_flags(ph.flags),
        })
    }

    /// The main thread's stack size the program asks for (`PT_GNU_STACK`),
    /// if any.
    pub fn stack_size(&self) -> Option<u64> {
        self.program_headers().find(|ph| ph.kind == pt::GNU_STACK && ph.mem_size > 0).map(|ph| ph.mem_size)
    }

    /// The pages that are read-only once the image is relocated
    /// (`PT_GNU_RELRO`), `[start, end)` before the load bias: from the page
    /// its first byte is in to the last page it fills (linkers end it on a
    /// page boundary). `None` if it covers no page.
    pub fn relro(&self) -> Option<(u64, u64)> {
        let (vaddr, size) = self.relro?;
        let (start, end) = (page_down(vaddr), page_down(vaddr + size));
        (start < end).then_some((start, end))
    }

    /// The executable's notes (its `PT_NOTE` segments'), in file order; a
    /// segment's are read up to the first malformed one.
    pub fn notes(&self) -> impl Iterator<Item = Note<'a>> + 'a {
        let data = self.data;
        self.program_headers().filter(|ph| ph.kind == pt::NOTE).flat_map(move |ph| Notes {
            bytes: file_range(data, ph.offset, ph.file_size).unwrap_or(&[]),
            // Notes are 4-byte aligned, but for those of 8-byte aligned
            // segments (GNU's properties).
            align: if ph.align == 8 { 8 } else { 4 },
        })
    }

    /// Whether the executable has a note of `name` and `kind`, and what
    /// that note says.
    pub fn note(&self, name: &[u8], kind: u32) -> Option<&'a [u8]> {
        self.notes().find(|n| n.name == name && n.kind == kind).map(|n| n.desc)
    }

    /// Which kind of program the executable is.
    pub fn kind(&self) -> Kind {
        match self.note(veda::NOTE_NAME, veda::NOTE_TYPE) {
            None => Kind::Posix,
            Some(desc) if desc == veda::VERSION.to_le_bytes() => Kind::Veda,
            Some(_) => Kind::OtherVeda,
        }
    }

    /// How many of the file's bytes loading reads: up to the furthest end of
    /// the program headers and of the file bytes they point at (segments,
    /// notes, the dynamic section). What lies beyond, the section headers
    /// and what only they name (symbols, comments), only tools read.
    pub fn loaded_len(&self) -> usize {
        let ends = self.program_headers().map(|ph| usize::try_from(ph.offset.saturating_add(ph.file_size)));
        let headers = self.phoff + self.phnum * PHDR_SIZE;
        ends.fold(headers, |len, end| len.max(end.unwrap_or(usize::MAX))).min(self.data.len())
    }

    /// Applies the executable's relocations for the load bias `bias` to
    /// `image`, its memory: the [`span`](Elf::span), `[lo, hi)`, holding its
    /// segments as step 2 of loading leaves them (`image[0]` at `lo`).
    ///
    /// A static position-independent executable has only relative
    /// relocations (in its `DT_RELA` and `DT_RELR` tables), each a word of a
    /// writable segment; anything else is refused ([`ElfError::Dynamic`]
    /// for libraries it needs, [`ElfError::BadRelocation`] for the rest).
    /// After an error the image is relocated in part.
    pub fn relocate(&self, image: &mut [u8], bias: u64) -> Result<(), ElfError> {
        if image.len() as u64 != self.hi - self.lo {
            return Err(ElfError::BadRelocation);
        }
        let tables = self.relocation_tables()?;
        let mut image = Image { bytes: image, lo: self.lo, loads: Loads::of(self) };
        for r in tables.rela.as_chunks::<RELA_SIZE>().0 {
            let (at, info, addend) = (u64_at(r, 0), u64_at(r, 8), u64_at(r, 16));
            let (symbol, kind) = (info >> 32, info as u32);
            if kind == R_NONE {
                continue;
            }
            if kind != self.machine.relative || symbol != 0 {
                return Err(ElfError::BadRelocation);
            }
            *image.word(at)? = bias.wrapping_add(addend).to_le_bytes();
        }
        // `DT_RELR`: an address (even), whose word is relocated, then
        // bitmaps (odd), whose bits 1 to 63 say which of the 63 words after
        // the last one relocated are too.
        let mut next: Option<u64> = None;
        for w in tables.relr.as_chunks::<RELR_SIZE>().0 {
            let w = u64::from_le_bytes(*w);
            next = Some(if w & 1 == 0 {
                image.add_bias(w, bias)?;
                w.checked_add(8).ok_or(ElfError::BadRelocation)?
            } else {
                let base = next.ok_or(ElfError::BadRelocation)?;
                for bit in (1..64).filter(|&bit| w >> bit & 1 != 0) {
                    image.add_bias(base.checked_add(8 * (bit - 1)).ok_or(ElfError::BadRelocation)?, bias)?;
                }
                base.checked_add(8 * 63).ok_or(ElfError::BadRelocation)?
            });
        }
        Ok(())
    }

    /// The relocation tables, from the dynamic section.
    fn relocation_tables(&self) -> Result<Tables<'a>, ElfError> {
        let Some((offset, size)) = self.dynamic else { return Ok(Tables { rela: &[], relr: &[] }) };
        // Validated by `parse_for`.
        let dynamic = file_range(self.data, offset, size).ok_or(ElfError::BadSegment)?;
        let (mut rela, mut rela_size, mut rela_entry) = (0, 0, RELA_SIZE as u64);
        let (mut relr, mut relr_size, mut relr_entry) = (0, 0, RELR_SIZE as u64);
        for d in dynamic.as_chunks::<DYN_SIZE>().0 {
            let (tag, value) = (u64_at(d, 0), u64_at(d, 8));
            match tag {
                dt::NULL => break,
                dt::NEEDED => return Err(ElfError::Dynamic),
                dt::RELA => rela = value,
                dt::RELASZ => rela_size = value,
                dt::RELAENT => rela_entry = value,
                dt::RELR => relr = value,
                dt::RELRSZ => relr_size = value,
                dt::RELRENT => relr_entry = value,
                // Relocations without addends (other processors'), of the
                // procedure linkage table, or of read-only segments.
                dt::RELSZ | dt::PLTRELSZ if value != 0 => return Err(ElfError::BadRelocation),
                dt::TEXTREL => return Err(ElfError::BadRelocation),
                dt::FLAGS if value & dt::DF_TEXTREL != 0 => return Err(ElfError::BadRelocation),
                _ => {}
            }
        }
        let table = |addr: u64, size: u64, entry: u64, expected: usize| -> Result<&'a [u8], ElfError> {
            if size == 0 {
                return Ok(&[]);
            }
            if entry != expected as u64 || !size.is_multiple_of(expected as u64) {
                return Err(ElfError::BadRelocation);
            }
            self.loaded_bytes(addr, size).ok_or(ElfError::BadRelocation)
        };
        Ok(Tables {
            rela: table(rela, rela_size, rela_entry, RELA_SIZE)?,
            relr: table(relr, relr_size, relr_entry, RELR_SIZE)?,
        })
    }

    /// The file bytes loaded at `[vaddr, vaddr + size)` (before the load
    /// bias), if one segment's file bytes hold them all.
    fn loaded_bytes(&self, vaddr: u64, size: u64) -> Option<&'a [u8]> {
        let end = vaddr.checked_add(size)?;
        let s = self.segments().find(|s| s.vaddr <= vaddr && end <= s.vaddr + s.data.len() as u64)?;
        Some(&s.data[(vaddr - s.vaddr) as usize..(end - s.vaddr) as usize])
    }
}

/// The relocation tables of an executable.
struct Tables<'a> {
    rela: &'a [u8],
    relr: &'a [u8],
}

/// The loadable segments' addresses and permissions, gathered once: the
/// program headers may be many more.
#[derive(Debug, Clone, Copy)]
struct Loads {
    /// Each segment's start and end (before the load bias), and its
    /// permissions.
    list: [(u64, u64, Perms); MAX_SEGMENTS],
    len: usize,
}

impl Loads {
    fn of(elf: &Elf) -> Loads {
        let mut loads = Loads { list: [(0, 0, Perms::default()); MAX_SEGMENTS], len: 0 };
        // `parse_for` allows no more.
        for (slot, s) in loads.list.iter_mut().zip(elf.segments()) {
            *slot = (s.vaddr, s.end(), s.perms);
            loads.len += 1;
        }
        loads
    }

    fn iter(&self) -> impl Iterator<Item = (u64, u64, Perms)> + '_ {
        self.list[..self.len].iter().copied()
    }
}

/// An image being relocated: its memory, from its lowest page (`lo`).
struct Image<'i> {
    bytes: &'i mut [u8],
    lo: u64,
    loads: Loads,
}

impl Image<'_> {
    /// The word at `at` (before the load bias), if relocations may change
    /// it: it is in a writable segment.
    fn word(&mut self, at: u64) -> Result<&mut [u8; 8], ElfError> {
        let end = at.checked_add(8).ok_or(ElfError::BadRelocation)?;
        if !self.loads.iter().any(|(start, stop, perms)| perms.write && start <= at && end <= stop) {
            return Err(ElfError::BadRelocation);
        }
        // Segments are in the span, from `lo` on.
        let i = (at - self.lo) as usize;
        self.bytes.get_mut(i..i + 8).and_then(|w| w.try_into().ok()).ok_or(ElfError::BadRelocation)
    }

    /// Adds `bias` to the word at `at` (before the load bias), which holds
    /// an address of the image as linked.
    fn add_bias(&mut self, at: u64, bias: u64) -> Result<(), ElfError> {
        let w = self.word(at)?;
        *w = u64::from_le_bytes(*w).wrapping_add(bias).to_le_bytes();
        Ok(())
    }
}

/// The notes of one `PT_NOTE` segment.
struct Notes<'a> {
    bytes: &'a [u8],
    align: usize,
}

impl<'a> Iterator for Notes<'a> {
    type Item = Note<'a>;

    fn next(&mut self) -> Option<Note<'a>> {
        let b = self.bytes;
        // Ends the segment's notes if this one is malformed.
        self.bytes = &[];
        if b.len() < 12 {
            return None;
        }
        let (name_size, desc_size, kind) = (u32_at(b, 0) as usize, u32_at(b, 4) as usize, u32_at(b, 8));
        let name_end = 12usize.checked_add(name_size)?;
        let desc_start = name_end.checked_next_multiple_of(self.align)?;
        let desc_end = desc_start.checked_add(desc_size)?;
        let name = b.get(12..name_end)?;
        let desc = b.get(desc_start..desc_end)?;
        self.bytes = b.get(desc_end.checked_next_multiple_of(self.align)?..).unwrap_or(&[]);
        Some(Note { name: name.strip_suffix(&[0]).unwrap_or(name), kind, desc })
    }
}

/// Cuts the executable `file` (for `machine`) to what loading reads
/// ([`Elf::loaded_len`]), as a system image ships it, without symbols: its
/// header then names no section headers, which are cut away with what they
/// name. Returns the length of what is left of `file`, for the caller to
/// keep.
pub fn strip(file: &mut [u8], machine: Machine) -> Result<usize, ElfError> {
    let len = Elf::parse_for(file, machine)?.loaded_len();
    // `e_shoff`; then `e_shentsize`, `e_shnum` and `e_shstrndx`.
    file[40..48].fill(0);
    file[58..64].fill(0);
    Ok(len)
}

/// Pages `[start, start+len)` of an image, all with the same permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRun {
    pub start: u64,
    pub len: u64,
    pub perms: Perms,
}

/// What becomes of a program's RELRO pages ([`Elf::relro`]) as it is
/// mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relro {
    /// They keep their segment's permissions: the program relocates itself
    /// (as musl's start of a static position-independent executable does).
    Writable,
    /// They are read-only: the loader relocated the program, or it has
    /// nothing to relocate.
    ReadOnly,
}

/// The image's pages grouped into runs of equal permissions, in address
/// order (before the load bias). A page shared by two segments gets the
/// permissions of both; pages no segment touches are left out.
pub fn page_runs(elf: &Elf, relro: Relro) -> PageRuns {
    let relro = match relro {
        Relro::ReadOnly => elf.relro(),
        Relro::Writable => None,
    };
    let mut pages = Loads::of(elf);
    for (start, end, _) in &mut pages.list[..pages.len] {
        // `parse_for` checked that the ends round up.
        (*start, *end) = (page_down(*start), page_up(*end).unwrap_or(u64::MAX));
    }
    PageRuns { pages, relro, at: elf.lo }
}

/// The iterator of [`page_runs`]: it walks from boundary to boundary (the
/// first and last pages of segments and of RELRO), at most
/// 2 × ([`MAX_SEGMENTS`] + 1) steps of as many comparisons each.
#[derive(Debug, Clone)]
pub struct PageRuns {
    /// Each segment's pages, `[first, end)`, and permissions.
    pages: Loads,
    relro: Option<(u64, u64)>,
    /// Where the next run starts looking.
    at: u64,
}

impl PageRuns {
    /// The first boundary after `at`.
    fn next_boundary(&self, at: u64) -> Option<u64> {
        let segments = self.pages.iter().flat_map(|(first, end, _)| [first, end]);
        segments.chain(self.relro.into_iter().flat_map(|(start, end)| [start, end])).filter(|&b| b > at).min()
    }

    /// The permissions of the pages `[start, end)`, which no boundary
    /// divides; `None` if no segment touches them.
    fn perms(&self, start: u64, end: u64) -> Option<Perms> {
        let touching = self.pages.iter().filter(|&(first, last, _)| first <= start && end <= last);
        let mut perms = touching.map(|(_, _, perms)| perms).reduce(Perms::union)?;
        if self.relro.is_some_and(|(r, e)| r <= start && end <= e) {
            perms.write = false;
        }
        Some(perms)
    }
}

impl Iterator for PageRuns {
    type Item = PageRun;

    fn next(&mut self) -> Option<PageRun> {
        let mut run: Option<PageRun> = None;
        while let Some(end) = self.next_boundary(self.at) {
            let start = self.at;
            match (self.perms(start, end), &mut run) {
                (Some(perms), Some(r)) if r.perms == perms => r.len += end - start,
                // Another run starts here: the next call takes it up.
                (Some(_), Some(_)) => return run,
                (Some(perms), None) => run = Some(PageRun { start, len: end - start, perms }),
                (None, Some(_)) => {
                    self.at = end;
                    return run;
                }
                (None, None) => {}
            }
            self.at = end;
        }
        run
    }
}

/// Auxiliary vector entry types.
pub mod at {
    pub const NULL: u64 = 0;
    pub const PHDR: u64 = 3;
    pub const PHENT: u64 = 4;
    pub const PHNUM: u64 = 5;
    pub const PAGESZ: u64 = 6;
    pub const BASE: u64 = 7;
    pub const FLAGS: u64 = 8;
    pub const ENTRY: u64 = 9;
    pub const UID: u64 = 11;
    pub const EUID: u64 = 12;
    pub const GID: u64 = 13;
    pub const EGID: u64 = 14;
    pub const PLATFORM: u64 = 15;
    pub const HWCAP: u64 = 16;
    pub const CLKTCK: u64 = 17;
    pub const SECURE: u64 = 23;
    pub const RANDOM: u64 = 25;
    pub const HWCAP2: u64 = 26;
    pub const EXECFN: u64 = 31;
}

/// What the auxiliary vector tells a starting program.
#[derive(Debug, Clone, Copy)]
pub struct AuxInfo<'a> {
    /// Address of the program headers in memory (load bias applied).
    pub phdr: u64,
    pub phnum: u64,
    /// The entry point (load bias applied).
    pub entry: u64,
    /// The processor's features, as Linux's `AT_HWCAP` gives them.
    pub hwcap: u64,
    /// The platform's name (`AT_PLATFORM`: [`Machine::platform`]).
    pub platform: &'a [u8],
    pub uid: u32,
    pub gid: u32,
    /// 16 random bytes (stack protector and pointer guard seeds).
    pub random: [u8; 16],
    /// The path the program was started from.
    pub execfn: &'a [u8],
}

/// The entries of the auxiliary vector, before `AT_NULL`.
const AUXV_LEN: usize = 18;

/// The bytes of the strings [`initial_stack`] places.
fn strings_len(args: &[&[u8]], env: &[&[u8]], aux: &AuxInfo) -> usize {
    16 + aux.platform.len() + 1 + aux.execfn.len() + 1 + args.iter().chain(env).map(|s| s.len() + 1).sum::<usize>()
}

/// The words [`initial_stack`] places: `argc`, the argument and environment
/// vectors with their ends, and the auxiliary vector's pairs.
fn stack_words(args: &[&[u8]], env: &[&[u8]]) -> usize {
    1 + args.len() + 1 + env.len() + 1 + 2 * (AUXV_LEN + 1)
}

/// The most bytes [`initial_stack`] needs below the top of the stack for
/// these arguments, environment and auxiliary vector (the alignment of the
/// stack pointer included).
pub fn initial_stack_size(args: &[&[u8]], env: &[&[u8]], aux: &AuxInfo) -> usize {
    strings_len(args, env, aux) + 8 * stack_words(args, env) + 15
}

/// Lays out the initial stack of a program whose stack ends at `top`:
///
/// ```text
/// sp -> argc, argv[0..argc], NULL, envp[..], NULL, auxv pairs, AT_NULL
///       padding, argument and environment strings, the platform,
///       the program path, 16 random bytes                    <- top
/// ```
///
/// `out` is the memory below `top` (its last byte at `top - 1`), at least
/// [`initial_stack_size`] bytes ([`ElfError::TooLarge`] otherwise); bytes
/// of it below the returned stack pointer (16-byte aligned, pointing at
/// `argc`) are left as they were. Strings must not contain NUL bytes (they
/// would end early).
pub fn initial_stack(top: u64, args: &[&[u8]], env: &[&[u8]], aux: &AuxInfo, out: &mut [u8]) -> Result<u64, ElfError> {
    // Strings from the top down: random bytes, platform, path, environment
    // and arguments; their addresses are fixed once the total is known.
    let strings_start = top.checked_sub(strings_len(args, env, aux) as u64).ok_or(ElfError::TooLarge)?;
    let sp = strings_start.checked_sub(8 * stack_words(args, env) as u64).ok_or(ElfError::TooLarge)? & !15;
    let bottom = top.checked_sub(out.len() as u64).ok_or(ElfError::TooLarge)?;
    if sp < bottom {
        return Err(ElfError::TooLarge);
    }
    let at = |addr: u64| (addr - bottom) as usize;

    // The strings, each placed below the last one.
    let mut cursor = top;
    let mut place = |out: &mut [u8], s: &[u8], nul: bool| -> u64 {
        cursor -= (s.len() + nul as usize) as u64;
        out[at(cursor)..at(cursor) + s.len()].copy_from_slice(s);
        if nul {
            out[at(cursor) + s.len()] = 0;
        }
        cursor
    };
    let random = place(out, &aux.random, false);
    let platform = place(out, aux.platform, true);
    let execfn = place(out, aux.execfn, true);
    // Environment strings, last first, then the arguments: the vectors
    // read them back in order from the addresses (each below the next).
    let env_last = env.iter().rev().fold(0, |_, s| place(out, s, true));
    let arg_last = args.iter().rev().fold(0, |_, s| place(out, s, true));
    debug_assert_eq!(cursor, strings_start);

    // The vectors: each string's address is where the one before it in
    // the vector ends.
    let mut word = sp;
    let mut push = |out: &mut [u8], v: u64| {
        out[at(word)..at(word) + 8].copy_from_slice(&v.to_le_bytes());
        word += 8;
    };
    push(out, args.len() as u64);
    let mut addr = arg_last;
    for a in args {
        push(out, addr);
        addr += a.len() as u64 + 1;
    }
    push(out, 0);
    let mut addr = env_last;
    for e in env {
        push(out, addr);
        addr += e.len() as u64 + 1;
    }
    push(out, 0);
    let auxv: [(u64, u64); AUXV_LEN] = [
        (at::PHDR, aux.phdr),
        (at::PHENT, PHDR_SIZE as u64),
        (at::PHNUM, aux.phnum),
        (at::PAGESZ, PAGE_SIZE),
        (at::BASE, 0),
        (at::FLAGS, 0),
        (at::ENTRY, aux.entry),
        (at::UID, aux.uid as u64),
        (at::EUID, aux.uid as u64),
        (at::GID, aux.gid as u64),
        (at::EGID, aux.gid as u64),
        (at::SECURE, 0),
        (at::CLKTCK, 100),
        (at::HWCAP, aux.hwcap),
        (at::HWCAP2, 0),
        (at::RANDOM, random),
        (at::PLATFORM, platform),
        (at::EXECFN, execfn),
    ];
    for (kind, value) in auxv {
        push(out, kind);
        push(out, value);
    }
    push(out, at::NULL);
    push(out, 0);
    // The padding between the vectors and the strings.
    out[at(word)..at(strings_start)].fill(0);
    Ok(sp)
}

#[cfg(test)]
mod tests;

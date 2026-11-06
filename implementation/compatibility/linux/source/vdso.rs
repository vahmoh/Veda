// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The vDSO: the shared object Linux puts in every program, which the
//! auxiliary vector says where it is (`AT_SYSINFO_EHDR`) and whose
//! functions the C libraries and Go call for the time (`clock_gettime`,
//! `gettimeofday`, `time`, `clock_getres`) and the processor (`getcpu`).
//! Without it each is a system call, two passes through the kernel here
//! and the POSIX layer's work; with it, the program reads the time in its
//! own code, as Veda's programs do (`runtime::time`): the processor's
//! clock counter, scaled as the kernel scales it.
//!
//! **The image** is made here ([`image`]) as Linux's linker makes Linux's:
//! an ELF shared object linked at 0, its headers and tables in its first
//! page and its code, the architecture's (`architecture::vdso`), from its
//! second ([`TEXT`]). Its functions are found as glibc, musl and Go find
//! them: by name, through its dynamic section's symbols and either hash
//! table (`DT_HASH`, `DT_GNU_HASH`), and by version (`DT_VERSYM`,
//! `DT_VERDEF`), each under Linux's other, weak name too. Its call frame
//! information (`PT_GNU_EH_FRAME`) leads an unwinder (a profiler's, a
//! debugger's) through them to their callers, and its note says which
//! Linux it is, as `uname` does.
//!
//! **The data page**, right below the image ([`DATA`]), holds the scale the
//! clocks are read with ([`data`]), where the code reads it, as Linux's
//! code reads its `[vvar]`. It is the scale Linux compatibility's own
//! clocks are read with (`runtime::time::scale`), for the program's system
//! calls: so the vDSO and the system call give the same time for the same
//! reading of the counter, to the nanosecond, for as long as the program
//! runs. Nothing is taken once that the clocks could drift from: the scale
//! is the kernel's, which never changes (`abi::ClockInfo`), and the time is
//! counted from the clocks' start every time, as the kernel counts it.
//! Each of Linux's clocks is read as the POSIX layer reads it
//! (`posix::host::kernel_clock`): the real-time ones are Veda's UTC clock,
//! the rest that tell the time its monotonic one. What the code cannot read
//! so (the clocks of CPU time, a time zone, the processor's number, and
//! every clock where the kernel gives user space no scale, which the data
//! page says with zeros) is the system call's: the code makes it, as a C
//! library without a vDSO would.
//!
//! The loader maps both into the program (`crate::loader`).

use alloc::vec::Vec;
use core::mem::{offset_of, size_of};

use abi::ClockInfo;
use runtime::sync::Lazy;

use crate::architecture;

const PAGE: usize = abi::PAGE_SIZE;

/// Where the image's code is: its second page, the first holding its
/// headers and tables.
pub const TEXT: usize = PAGE;
/// The data page's size: the data starts this far below the image.
pub const DATA: usize = PAGE;

/// The Linux clocks (a bit for each, below 32) that the vDSO reads as
/// Veda's UTC clock, as the system call does: the real-time ones.
pub const UTC_CLOCKS: u32 = clocks_read_as(abi::clock::UTC);
/// The Linux clocks the vDSO reads as Veda's monotonic clock.
pub const MONOTONIC_CLOCKS: u32 = clocks_read_as(abi::clock::MONOTONIC);

/// The Linux clocks the POSIX layer reads as Veda's clock `id`.
const fn clocks_read_as(id: usize) -> u32 {
    let mut clocks = 0;
    let mut clk = 0;
    while clk < 32 {
        if matches!(posix::host::kernel_clock(clk), Some(read_as) if read_as == id) {
            clocks |= 1 << clk;
        }
        clk += 1;
    }
    clocks
}

/// The image's name (`DT_SONAME`, and its base version's), as Linux's.
const SONAME: &str = "linux-vdso.so.1";

/// The vDSO's code, as the architecture has it.
pub struct Code {
    /// The machine code, which runs [`TEXT`] bytes into the image, the
    /// data page [`DATA`] bytes below the image.
    pub text: &'static [u8],
    /// The functions, where they are in it.
    pub functions: Vec<Function>,
    /// The version of Linux's they are of (`LINUX_2.6` on x86-64).
    pub version: &'static str,
    /// Where the caller of a function is.
    pub frame: Frame,
}

/// One of the vDSO's functions.
pub struct Function {
    /// Its name (`__vdso_clock_gettime`).
    pub name: &'static str,
    /// Linux's other name for it, a weak one (`clock_gettime`).
    pub alias: &'static str,
    /// Where it is in the code, and its size.
    pub offset: usize,
    pub size: usize,
}

/// Where a function's caller is, at every instruction of the code, as
/// DWARF's call frame information says it: the column of the return
/// address, the data alignment factor (-64 to 63), and the instructions
/// (`DW_CFA_*`) of the rule, which a common information entry starts with.
pub struct Frame {
    pub return_address: u8,
    pub data_alignment: i8,
    pub instructions: &'static [u8],
}

/// The image, made once: `None` where the architecture has no vDSO.
pub fn image() -> Option<&'static [u8]> {
    static IMAGE: Lazy<Option<Vec<u8>>> = Lazy::new(|| architecture::vdso::code().map(|code| build(&code)));
    IMAGE.as_deref()
}

/// The data page's bytes, from its start: the clocks' scale (the kernel's
/// `abi::ClockInfo`, as the code reads it), or zeros where there is none,
/// for which the code makes the system call.
pub fn data(scale: Option<ClockInfo>) -> [u8; size_of::<ClockInfo>()] {
    let mut bytes = [0; size_of::<ClockInfo>()];
    if let Some(s) = scale {
        let fields = [
            (offset_of!(ClockInfo, ticks_at_zero), s.ticks_at_zero),
            (offset_of!(ClockInfo, ns_per_tick), s.ns_per_tick),
            (offset_of!(ClockInfo, ticks_hz), s.ticks_hz),
            (offset_of!(ClockInfo, realtime_at_zero), s.realtime_at_zero),
            (offset_of!(ClockInfo, utc_at_zero), s.utc_at_zero),
        ];
        for (at, value) in fields {
            bytes[at..at + 8].copy_from_slice(&value.to_ne_bytes());
        }
    }
    bytes
}

/// The System V ABI's hash of a name (`DT_HASH`'s, and a version's).
fn elf_hash(name: &[u8]) -> u32 {
    name.iter().fold(0u32, |h, &c| {
        let h = (h << 4).wrapping_add(u32::from(c));
        let high = h & 0xf000_0000;
        (h ^ (high >> 24)) & !high
    })
}

/// GNU's hash of a name (`DT_GNU_HASH`'s): Bernstein's.
fn gnu_hash(name: &[u8]) -> u32 {
    name.iter().fold(5381u32, |h, &c| h.wrapping_mul(33).wrapping_add(u32::from(c)))
}

/// The `LINUX_VERSION_CODE` of a release as `uname` gives it
/// (`6.6.0-veda`): its version, patch level and sublevel, a byte each.
fn version_code(release: &str) -> u32 {
    let mut parts = release.split(|c: char| !c.is_ascii_digit()).map(|n| n.parse::<u32>().unwrap_or(0));
    let mut next = || parts.next().unwrap_or(0).min(255);
    (next() << 16) | (next() << 8) | next()
}

/// ELF's numbers for what the image has, and their sizes.
mod format {
    pub const EHDR: usize = 64;
    pub const PHDR: usize = 56;
    pub const SHDR: usize = 64;
    pub const SYM: usize = 24;
    pub const DYN: usize = 16;
    pub const VERDEF: usize = 20;
    pub const VERDAUX: usize = 8;

    pub const ET_DYN: u16 = 3;

    pub const SHT_PROGBITS: u32 = 1;
    pub const SHT_STRTAB: u32 = 3;
    pub const SHT_HASH: u32 = 5;
    pub const SHT_DYNAMIC: u32 = 6;
    pub const SHT_NOTE: u32 = 7;
    pub const SHT_DYNSYM: u32 = 11;
    pub const SHT_GNU_HASH: u32 = 0x6fff_fff6;
    pub const SHT_GNU_VERDEF: u32 = 0x6fff_fffd;
    pub const SHT_GNU_VERSYM: u32 = 0x6fff_ffff;
    pub const SHF_ALLOC: u64 = 2;
    pub const SHF_EXECINSTR: u64 = 4;

    pub const DT_NULL: u64 = 0;
    pub const DT_HASH: u64 = 4;
    pub const DT_STRTAB: u64 = 5;
    pub const DT_SYMTAB: u64 = 6;
    pub const DT_STRSZ: u64 = 10;
    pub const DT_SYMENT: u64 = 11;
    pub const DT_SONAME: u64 = 14;
    pub const DT_GNU_HASH: u64 = 0x6fff_fef5;
    pub const DT_VERSYM: u64 = 0x6fff_fff0;
    pub const DT_VERDEF: u64 = 0x6fff_fffc;
    pub const DT_VERDEFNUM: u64 = 0x6fff_fffd;

    pub const STB_GLOBAL: u8 = 1;
    pub const STB_WEAK: u8 = 2;
    pub const STT_FUNC: u8 = 2;
    pub const VER_FLG_BASE: u16 = 1;
    /// The version index of the object itself, and of the version its
    /// functions are of.
    pub const VER_NDX_BASE: u16 = 1;
    pub const VER_NDX_FUNCTIONS: u16 = 2;

    /// How the call frame information encodes an address (`DW_EH_PE_*`):
    /// a 4-byte number, signed or not, from where it is or from the start
    /// of `.eh_frame_hdr`.
    pub const UDATA4: u8 = 0x03;
    pub const SDATA4: u8 = 0x0b;
    pub const PCREL: u8 = 0x10;
    pub const DATAREL: u8 = 0x30;

    /// The GNU hash table's Bloom filter, one word: each name sets the bit
    /// of its hash, and that of its hash shifted by this.
    pub const BLOOM_SHIFT: u32 = 26;
}

/// The image's sections, in their order: their indices (`st_shndx`,
/// `sh_link`, `e_shstrndx`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Index {
    None,
    Hash,
    GnuHash,
    DynSym,
    DynStr,
    VerSym,
    VerDef,
    Dynamic,
    Note,
    EhFrameHdr,
    EhFrame,
    Text,
    ShStrTab,
}

/// A section's header.
struct Section {
    name: &'static str,
    kind: u32,
    flags: u64,
    at: usize,
    len: usize,
    link: Index,
    info: u32,
    align: usize,
    entry: usize,
}

/// An image being laid out, little-endian, and its sections.
struct Image {
    bytes: Vec<u8>,
    sections: Vec<Section>,
}

impl Image {
    fn at(&self) -> usize {
        self.bytes.len()
    }

    fn align(&mut self, align: usize) {
        self.bytes.resize(self.at().next_multiple_of(align), 0);
    }

    fn u8(&mut self, v: u8) {
        self.bytes.push(v);
    }

    fn u16(&mut self, v: u16) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    fn u32(&mut self, v: u32) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    /// A 4-byte, signed offset from `from` to `to` (the image is far
    /// smaller than it takes to overflow one).
    fn offset(&mut self, to: usize, from: usize) {
        self.u32(to.wrapping_sub(from) as u32);
    }

    /// Sets the 4-byte word at `at` to the offset from `from` to `to`.
    fn set_offset(&mut self, at: usize, to: usize, from: usize) {
        self.bytes[at..at + 4].copy_from_slice(&(to.wrapping_sub(from) as u32).to_le_bytes());
    }

    /// Starts the next section, at a multiple of `align`, holding entries
    /// of `entry` bytes (0: not a table), linked to section `link`.
    fn start(&mut self, name: &'static str, kind: u32, align: usize, link: Index, info: u32, entry: usize) {
        self.align(align);
        let (at, flags) = (self.at(), format::SHF_ALLOC);
        self.sections.push(Section { name, kind, flags, at, len: 0, link, info, align, entry });
    }

    /// Ends the last section started, where the bytes are now: its start.
    fn end(&mut self, index: Index) -> usize {
        let at = self.at();
        debug_assert!(self.sections.len() == index as usize);
        let s = self.sections.last_mut().expect("a section");
        s.len = at - s.at;
        s.at
    }

    fn section(&self, index: Index) -> &Section {
        &self.sections[index as usize - 1]
    }
}

/// A symbol of the dynamic symbol table: a function, by one of its names.
struct Symbol {
    name: &'static str,
    bind: u8,
    value: usize,
    size: usize,
}

/// Lays out the image of `code`'s functions, linked at 0: the ELF header
/// and program headers, the dynamic section's tables, the note, the call
/// frame information and its index, the code at [`TEXT`], and the section
/// headers. One loadable segment holds it all, readable and executable,
/// as Linux's.
pub(crate) fn build(code: &Code) -> Vec<u8> {
    use format::*;
    // Each function by both its names, in the order of their buckets of the
    // GNU hash table, which needs them so; the first symbol is none.
    let mut symbols: Vec<Symbol> = code
        .functions
        .iter()
        .flat_map(|f| {
            let value = TEXT + f.offset;
            [
                Symbol { name: f.name, bind: STB_GLOBAL, value, size: f.size },
                Symbol { name: f.alias, bind: STB_WEAK, value, size: f.size },
            ]
        })
        .collect();
    let buckets = symbols.len().div_ceil(4).max(1) as u32;
    symbols.sort_by_key(|s| gnu_hash(s.name.as_bytes()) % buckets);
    let count = symbols.len() as u32 + 1;

    let mut strings = alloc::vec![0u8];
    let mut string = |s: &str| {
        let at = strings.len() as u32;
        strings.extend_from_slice(s.as_bytes());
        strings.push(0);
        at
    };
    let soname = string(SONAME);
    let version = string(code.version);
    let names: Vec<u32> = symbols.iter().map(|s| string(s.name)).collect();

    const PHNUM: usize = 5;
    let mut image = Image { bytes: alloc::vec![0; EHDR + PHNUM * PHDR], sections: Vec::new() };

    // `.hash`: its buckets' first symbols, then each symbol's next in its
    // bucket's chain.
    image.start(".hash", SHT_HASH, 8, Index::DynSym, 0, 4);
    let mut bucket = alloc::vec![0u32; buckets as usize];
    let mut chain = alloc::vec![0u32; count as usize];
    for (i, s) in symbols.iter().enumerate() {
        let b = (elf_hash(s.name.as_bytes()) % buckets) as usize;
        chain[i + 1] = bucket[b];
        bucket[b] = i as u32 + 1;
    }
    image.u32(buckets);
    image.u32(count);
    bucket.iter().chain(&chain).for_each(|&w| image.u32(w));
    let hash = image.end(Index::Hash);

    // `.gnu.hash`: every symbol but the first, its Bloom filter, each
    // bucket's first symbol, and each symbol's hash, its lowest bit set at
    // its bucket's last.
    image.start(".gnu.hash", SHT_GNU_HASH, 8, Index::DynSym, 0, 0);
    let hashes: Vec<u32> = symbols.iter().map(|s| gnu_hash(s.name.as_bytes())).collect();
    let bloom = hashes.iter().fold(0u64, |w, h| w | 1 << (h % 64) | 1 << ((h >> BLOOM_SHIFT) % 64));
    let mut first = alloc::vec![0u32; buckets as usize];
    for (i, h) in hashes.iter().enumerate().rev() {
        first[(h % buckets) as usize] = i as u32 + 1;
    }
    image.u32(buckets);
    image.u32(1);
    image.u32(1);
    image.u32(BLOOM_SHIFT);
    image.u64(bloom);
    first.iter().for_each(|&w| image.u32(w));
    for (i, h) in hashes.iter().enumerate() {
        let last = hashes.get(i + 1).is_none_or(|next| next % buckets != h % buckets);
        image.u32((h & !1) | u32::from(last));
    }
    let gnu_hash = image.end(Index::GnuHash);

    // `.dynsym`, its first symbol none: every other a function, global or
    // weak, in the code.
    image.start(".dynsym", SHT_DYNSYM, 8, Index::DynStr, 1, SYM);
    image.bytes.resize(image.at() + SYM, 0);
    for (s, &name) in symbols.iter().zip(&names) {
        image.u32(name);
        image.u8(s.bind << 4 | STT_FUNC);
        image.u8(0);
        image.u16(Index::Text as u16);
        image.u64(s.value as u64);
        image.u64(s.size as u64);
    }
    let dynsym = image.end(Index::DynSym);

    image.start(".dynstr", SHT_STRTAB, 1, Index::None, 0, 0);
    image.bytes.extend_from_slice(&strings);
    let dynstr = image.end(Index::DynStr);

    // `.gnu.version`: each symbol's version, the functions'.
    image.start(".gnu.version", SHT_GNU_VERSYM, 2, Index::DynSym, 0, 2);
    image.u16(0);
    (1..count).for_each(|_| image.u16(VER_NDX_FUNCTIONS));
    let versym = image.end(Index::VerSym);

    // `.gnu.version_d`: the object's own version, then its functions'.
    let definitions = [(VER_FLG_BASE, VER_NDX_BASE, SONAME, soname), (0, VER_NDX_FUNCTIONS, code.version, version)];
    image.start(".gnu.version_d", SHT_GNU_VERDEF, 8, Index::DynStr, definitions.len() as u32, 0);
    for (i, &(flags, index, name, at)) in definitions.iter().enumerate() {
        let last = i + 1 == definitions.len();
        image.u16(1);
        image.u16(flags);
        image.u16(index);
        image.u16(1);
        image.u32(elf_hash(name.as_bytes()));
        image.u32(VERDEF as u32);
        image.u32(if last { 0 } else { (VERDEF + VERDAUX) as u32 });
        image.u32(at);
        image.u32(0);
    }
    let verdef = image.end(Index::VerDef);

    // `.dynamic`, from which the C libraries and Go find the rest.
    image.start(".dynamic", SHT_DYNAMIC, 8, Index::DynStr, 0, DYN);
    let entries = [
        (DT_SONAME, soname as usize),
        (DT_HASH, hash),
        (DT_GNU_HASH, gnu_hash),
        (DT_STRTAB, dynstr),
        (DT_SYMTAB, dynsym),
        (DT_STRSZ, strings.len()),
        (DT_SYMENT, SYM),
        (DT_VERSYM, versym),
        (DT_VERDEF, verdef),
        (DT_VERDEFNUM, definitions.len()),
        (DT_NULL, 0),
    ];
    for (tag, value) in entries {
        image.u64(tag);
        image.u64(value as u64);
    }
    image.end(Index::Dynamic);

    // `.note`: the Linux it is (`LINUX_VERSION_CODE`), which glibc once
    // read in place of `uname`'s release.
    image.start(".note", SHT_NOTE, 4, Index::None, 0, 0);
    image.u32(b"Linux\0".len() as u32);
    image.u32(4);
    image.u32(0);
    image.bytes.extend_from_slice(b"Linux\0\0\0");
    image.u32(version_code(crate::RELEASE));
    image.end(Index::Note);

    // `.eh_frame_hdr`, the index of `.eh_frame`: where it is, and a table
    // of its frame description entries by address, one for all the code.
    image.start(".eh_frame_hdr", SHT_PROGBITS, 4, Index::None, 0, 0);
    let header = image.at();
    image.u8(1);
    image.u8(PCREL | SDATA4);
    image.u8(UDATA4);
    image.u8(DATAREL | SDATA4);
    let frames_at = image.at();
    image.u32(0);
    image.u32(1);
    image.offset(TEXT, header);
    let entry_at = image.at();
    image.u32(0);
    image.end(Index::EhFrameHdr);

    // `.eh_frame`: a common information entry, whose rule holds at every
    // instruction, and the frame description entry of the code, which adds
    // nothing to it; then the entries' end.
    image.start(".eh_frame", SHT_PROGBITS, 8, Index::None, 0, 0);
    let cie = image.at();
    image.set_offset(frames_at, cie, frames_at);
    let frame = &code.frame;
    debug_assert!((-64..64).contains(&frame.data_alignment), "a one-byte SLEB128");
    image.u32(0);
    image.u32(0);
    image.u8(1);
    image.bytes.extend_from_slice(b"zR\0");
    image.u8(1);
    image.u8(frame.data_alignment as u8 & 0x7f);
    image.u8(frame.return_address);
    image.u8(1);
    image.u8(PCREL | SDATA4);
    image.bytes.extend_from_slice(frame.instructions);
    image.align(8);
    image.set_offset(cie, image.at() - 4, cie);
    let fde = image.at();
    image.set_offset(entry_at, fde, header);
    image.u32(0);
    image.offset(fde + 4, cie);
    image.offset(TEXT, image.at());
    image.u32(code.text.len() as u32);
    image.u8(0);
    image.align(8);
    image.set_offset(fde, image.at() - 4, fde);
    image.u32(0);
    image.end(Index::EhFrame);

    // The code, where it runs.
    assert!(image.at() <= TEXT, "the vDSO's tables take more than their page");
    image.start(".text", SHT_PROGBITS, TEXT, Index::None, 0, 0);
    image.sections.last_mut().expect("a section").flags = SHF_ALLOC | SHF_EXECINSTR;
    image.bytes.extend_from_slice(code.text);
    image.end(Index::Text);

    // The sections' names, which take no memory, and their headers, the
    // first none.
    image.start(".shstrtab", SHT_STRTAB, 1, Index::None, 0, 0);
    image.sections.last_mut().expect("a section").flags = 0;
    let shstrtab = image.at();
    let titles: Vec<&str> = image.sections.iter().map(|s| s.name).collect();
    let mut names = Vec::new();
    image.u8(0);
    for title in titles {
        names.push(image.at() - shstrtab);
        image.bytes.extend_from_slice(title.as_bytes());
        image.u8(0);
    }
    image.end(Index::ShStrTab);
    image.align(8);
    let shoff = image.at();
    let mut headers = Image { bytes: alloc::vec![0; SHDR], sections: Vec::new() };
    for (s, name) in image.sections.iter().zip(names) {
        let address = if s.flags & SHF_ALLOC != 0 { s.at } else { 0 };
        headers.u32(name as u32);
        headers.u32(s.kind);
        headers.u64(s.flags);
        headers.u64(address as u64);
        headers.u64(s.at as u64);
        headers.u64(s.len as u64);
        headers.u32(s.link as u32);
        headers.u32(s.info);
        headers.u64(s.align as u64);
        headers.u64(s.entry as u64);
    }
    image.bytes.extend_from_slice(&headers.bytes);
    let len = image.at();

    // The ELF header, and the program headers: the image whole, readable and
    // executable; the dynamic section; the note; the call frame
    // information's index; and a stack that is not executable.
    let mut head = Image { bytes: Vec::with_capacity(EHDR + PHNUM * PHDR), sections: Vec::new() };
    head.bytes.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    head.u16(ET_DYN);
    head.u16(elf::NATIVE.code);
    head.u32(1);
    head.u64(0);
    head.u64(EHDR as u64);
    head.u64(shoff as u64);
    head.u32(0);
    head.u16(EHDR as u16);
    head.u16(PHDR as u16);
    head.u16(PHNUM as u16);
    head.u16(SHDR as u16);
    head.u16(image.sections.len() as u16 + 1);
    head.u16(Index::ShStrTab as u16);
    let (r, w, x) = (elf::pf::R, elf::pf::W, elf::pf::X);
    let part = |i: Index| (image.section(i).at, image.section(i).len);
    let segments = [
        (elf::pt::LOAD, r | x, (0, len), PAGE),
        (elf::pt::DYNAMIC, r, part(Index::Dynamic), 8),
        (elf::pt::NOTE, r, part(Index::Note), 4),
        (elf::pt::GNU_EH_FRAME, r, part(Index::EhFrameHdr), 4),
        (elf::pt::GNU_STACK, r | w, (0, 0), 16),
    ];
    for (kind, flags, (at, size), align) in segments {
        head.u32(kind);
        head.u32(flags);
        head.u64(at as u64);
        head.u64(at as u64);
        head.u64(at as u64);
        head.u64(size as u64);
        head.u64(size as u64);
        head.u64(align as u64);
    }
    image.bytes[..head.at()].copy_from_slice(&head.bytes);
    image.bytes
}

#[cfg(test)]
mod tests;

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;

/// A segment of a test executable.
struct Seg {
    flags: u32,
    vaddr: u64,
    data: Vec<u8>,
    mem_size: u64,
}

fn seg(flags: u32, vaddr: u64, file: usize, mem: u64) -> Seg {
    Seg { flags, vaddr, data: (0..file).map(|i| (i % 251) as u8 + 1).collect(), mem_size: mem }
}

/// A program header of a test executable other than a loadable segment's:
/// its type, address, memory size, alignment, and file bytes (put after
/// the segments').
struct Extra {
    kind: u32,
    vaddr: u64,
    mem_size: u64,
    align: u64,
    file: Vec<u8>,
}

fn extra(kind: u32, vaddr: u64, mem_size: u64) -> Extra {
    Extra { kind, vaddr, mem_size, align: 8, file: Vec::new() }
}

/// Builds an executable: header and program headers in the first page,
/// segment contents after it. The first segment (if it starts at file
/// offset 0) holds the headers, as with GNU ld.
struct Exe {
    kind: u16,
    machine: u16,
    entry: u64,
    segs: Vec<Seg>,
    extra: Vec<Extra>,
    headers_in_first: bool,
}

impl Exe {
    fn new(entry: u64, segs: Vec<Seg>) -> Exe {
        Exe { kind: ET_EXEC, machine: amd64::MACHINE.code, entry, segs, extra: Vec::new(), headers_in_first: true }
    }

    fn build(&self) -> Vec<u8> {
        let phnum = self.segs.len() + self.extra.len();
        let mut out = vec![0u8; 64 + phnum * PHDR_SIZE];
        out[..4].copy_from_slice(b"\x7fELF");
        out[4] = 2;
        out[5] = 1;
        out[6] = 1;
        out[16..18].copy_from_slice(&self.kind.to_le_bytes());
        out[18..20].copy_from_slice(&self.machine.to_le_bytes());
        out[20..24].copy_from_slice(&1u32.to_le_bytes());
        out[24..32].copy_from_slice(&self.entry.to_le_bytes());
        out[32..40].copy_from_slice(&64u64.to_le_bytes());
        out[52..54].copy_from_slice(&64u16.to_le_bytes());
        out[54..56].copy_from_slice(&(PHDR_SIZE as u16).to_le_bytes());
        out[56..58].copy_from_slice(&(phnum as u16).to_le_bytes());
        let mut ph = 64;
        let mut write_ph =
            |out: &mut Vec<u8>, kind: u32, flags: u32, offset: u64, vaddr: u64, file: u64, mem: u64, align: u64| {
                let fields = [(0, kind as u64, 4), (4, flags as u64, 4), (8, offset, 8), (16, vaddr, 8), (32, file, 8)];
                for (at, v, n) in fields.into_iter().chain([(40, mem, 8), (48, align, 8)]) {
                    out[ph + at..ph + at + n].copy_from_slice(&v.to_le_bytes()[..n]);
                }
                ph += PHDR_SIZE;
            };
        for (i, s) in self.segs.iter().enumerate() {
            if i == 0 && self.headers_in_first {
                // The first segment maps the headers too: file offset 0.
                out.extend_from_slice(&s.data);
                let file = out.len() as u64;
                write_ph(&mut out, pt::LOAD, s.flags, 0, s.vaddr, file, s.mem_size.max(file), 4096);
            } else {
                let offset = out.len().next_multiple_of(4096) + (s.vaddr % 4096) as usize;
                out.resize(offset, 0);
                out.extend_from_slice(&s.data);
                let file = s.data.len() as u64;
                write_ph(&mut out, pt::LOAD, s.flags, offset as u64, s.vaddr, file, s.mem_size, 4096);
            }
        }
        for e in &self.extra {
            let offset = out.len().next_multiple_of(8);
            out.resize(offset, 0);
            out.extend_from_slice(&e.file);
            let file = e.file.len() as u64;
            write_ph(&mut out, e.kind, pf::R, offset as u64, e.vaddr, file, e.mem_size, e.align);
        }
        out
    }
}

const R: u32 = pf::R;
const RX: u32 = pf::R | pf::X;
const RW: u32 = pf::R | pf::W;

fn p(flags: u32) -> Perms {
    Perms::from_flags(flags)
}

/// The layout of a typical static executable from GNU ld.
fn typical() -> Exe {
    Exe::new(
        0x401040,
        vec![
            seg(R, 0x400000, 0x100, 0),
            seg(RX, 0x401000, 0x1800, 0x1800),
            seg(R, 0x403000, 0x400, 0x400),
            seg(RW, 0x404e10, 0x200, 0x1400),
        ],
    )
}

fn runs(elf: &Elf, relro: Relro) -> Vec<PageRun> {
    page_runs(elf, relro).collect()
}

#[test]
fn parses_a_static_executable() {
    let bytes = typical().build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    assert!(!elf.is_position_independent());
    assert_eq!(elf.entry(), 0x401040);
    assert_eq!(elf.phdr_vaddr(), 0x400040);
    assert_eq!(elf.phnum(), 4);
    assert_eq!(elf.span(), (0x400000, 0x407000));
    let segs: Vec<Segment> = elf.segments().collect();
    assert_eq!(segs.len(), 4);
    assert_eq!(segs[1].vaddr, 0x401000);
    assert_eq!(segs[1].data.len(), 0x1800);
    assert_eq!(segs[1].data[0], 1);
    assert_eq!(segs[3].mem_size, 0x1400);
    assert_eq!(segs[3].end(), 0x406210);
    assert_eq!(segs[3].perms, Perms { read: true, write: true, exec: false });
    assert_eq!(elf.stack_size(), None);
    assert_eq!(elf.relro(), None);
    assert_eq!(elf.notes().count(), 0);
}

#[test]
fn groups_pages_by_permissions() {
    let bytes = typical().build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    let expected = vec![
        PageRun { start: 0x400000, len: 0x1000, perms: p(R) },
        PageRun { start: 0x401000, len: 0x2000, perms: p(RX) },
        PageRun { start: 0x403000, len: 0x1000, perms: p(R) },
        PageRun { start: 0x404000, len: 0x3000, perms: p(RW) },
    ];
    assert_eq!(runs(&elf, Relro::Writable), expected);
    // Without PT_GNU_RELRO, nothing is made read-only.
    assert_eq!(runs(&elf, Relro::ReadOnly), expected);
}

#[test]
fn shared_pages_get_both_permissions() {
    let mut exe = Exe::new(0x401000, vec![seg(RX, 0x401000, 0x900, 0x900), seg(RW, 0x401a00, 0x10, 0x1000)]);
    exe.headers_in_first = false;
    // The program headers are not in a segment of their own here: PT_PHDR
    // says they are at the start of the code.
    exe.extra.push(extra(pt::PHDR, 0x401000, 0));
    let bytes = exe.build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    assert_eq!(
        runs(&elf, Relro::Writable),
        vec![
            PageRun { start: 0x401000, len: 0x1000, perms: p(RX | RW) },
            PageRun { start: 0x402000, len: 0x1000, perms: p(RW) },
        ]
    );
}

/// What [`Elf::file_pages`] says of each run.
fn file_pages(elf: &Elf, relro: Relro) -> Vec<Option<u64>> {
    page_runs(elf, relro).map(|r| elf.file_pages(&r)).collect()
}

#[test]
fn pages_nothing_writes_can_be_the_files() {
    let bytes = typical().build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    // The headers with read-only data, the code and more read-only data;
    // not the data, which is written.
    assert_eq!(file_pages(&elf, Relro::Writable), vec![Some(0), Some(0x1000), Some(0x3000), None]);
    // There, the file has what the segments put in memory.
    for run in runs(&elf, Relro::Writable) {
        let Some(offset) = elf.file_pages(&run) else { continue };
        for s in elf.segments().filter(|s| run.start <= s.vaddr && s.end() <= run.start + run.len) {
            let at = (offset + s.vaddr - run.start) as usize;
            assert_eq!(&bytes[at..at + s.data.len()], s.data);
        }
    }
}

#[test]
fn zeros_writes_and_shifts_need_memory_of_their_own() {
    // Read-only data that ends in zeros past its bytes in the file.
    let bytes = Exe::new(0x401000, vec![seg(RX, 0x401000, 0x10, 0x10), seg(R, 0x402000, 0x10, 0x2000)]).build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    assert_eq!(file_pages(&elf, Relro::Writable), vec![Some(0), None]);

    // A page code and data share is written.
    let mut exe = Exe::new(0x401000, vec![seg(RX, 0x401000, 0x900, 0x900), seg(RW, 0x401a00, 0x10, 0x1000)]);
    exe.headers_in_first = false;
    exe.extra.push(extra(pt::PHDR, 0x401000, 0));
    let bytes = exe.build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    assert_eq!(file_pages(&elf, Relro::Writable), vec![None, None]);

    // Two read-only segments in one page, a page apart from each other in
    // the file but not in memory.
    let bytes = Exe::new(0x400000, vec![seg(RX, 0x400000, 0x100, 0x100), seg(RX, 0x400800, 0x100, 0x100)]).build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    assert_eq!(runs(&elf, Relro::Writable).len(), 1);
    assert_eq!(file_pages(&elf, Relro::Writable), vec![None]);
}

#[test]
fn relocated_pages_are_never_the_files() {
    // A static position-independent executable, relocated by the loader:
    // its RELRO pages become read-only, but relocations wrote them. (Its
    // relocation table makes the last segment bytes of the file's.)
    let bytes = static_pie(&[], &[0x3000], &[]).build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    let writable: Vec<(u64, u64)> = elf
        .segments()
        .filter(|s| s.perms.write)
        .map(|s| (s.vaddr & !0xfff, s.end().next_multiple_of(0x1000)))
        .collect();
    for relro in [Relro::Writable, Relro::ReadOnly] {
        for run in runs(&elf, relro) {
            let written = writable.iter().any(|&(first, end)| first < run.start + run.len && run.start < end);
            assert_eq!(elf.file_pages(&run).is_none(), written, "{run:?} ({relro:?})");
        }
    }
}

#[test]
fn runs_skip_the_pages_between_segments() {
    // Code, then data two pages further on.
    let exe = Exe::new(0x1000, vec![seg(RX, 0x1000, 0x10, 0x10), seg(RW, 0x4000, 0x10, 0x1800)]);
    let bytes = exe.build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    assert_eq!(
        runs(&elf, Relro::ReadOnly),
        vec![
            PageRun { start: 0x1000, len: 0x1000, perms: p(RX) },
            PageRun { start: 0x4000, len: 0x2000, perms: p(RW) },
        ]
    );
}

/// The layout of a static position-independent executable from LLVM's
/// linker: read-only data with the headers and relocation tables; code;
/// RELRO data (`.data.rel.ro`, the dynamic section, padded to a page);
/// data and bss. Where the relocation tables are, the dynamic section says
/// (`dynamic`, a list of tags and values).
fn static_pie(rela: &[(u64, u64, u64)], relr: &[u64], dynamic: &[(u64, u64)]) -> Exe {
    let mut tables = Vec::new();
    for &(offset, info, addend) in rela {
        tables.extend([offset, info, addend].iter().flat_map(|v| v.to_le_bytes()));
    }
    tables.extend(relr.iter().flat_map(|v| v.to_le_bytes()));
    let mut exe = Exe::new(
        0x1040,
        vec![
            seg(R, 0, 0x100, 0),
            seg(RX, 0x1040, 0x200, 0x200),
            seg(RW, 0x2240, 0x40, 0xdc0),
            seg(RW, 0x3000, 0x80, 0x2000),
            Seg { flags: R, vaddr: 0x5000, mem_size: tables.len().max(1) as u64, data: tables },
        ],
    );
    exe.kind = ET_DYN;
    let mut dynamic_bytes: Vec<u8> =
        dynamic.iter().flat_map(|&(t, v)| [t.to_le_bytes(), v.to_le_bytes()]).flatten().collect();
    dynamic_bytes.extend([0u8; 16]);
    let dynamic_size = dynamic_bytes.len() as u64;
    exe.extra.push(Extra { kind: pt::DYNAMIC, vaddr: 0x2280, mem_size: dynamic_size, align: 8, file: dynamic_bytes });
    exe.extra.push(extra(pt::GNU_RELRO, 0x2240, 0xdc0));
    exe
}

/// `DT_RELA` entries at 0x5000 (`count` of them), with `DT_RELAENT`.
fn rela_tags(count: u64) -> Vec<(u64, u64)> {
    vec![(dt::RELA, 0x5000), (dt::RELASZ, 24 * count), (dt::RELAENT, 24)]
}

/// The memory image of `elf` as a loader copies it.
fn image_of(elf: &Elf) -> Vec<u8> {
    let (lo, hi) = elf.span();
    let mut image = vec![0u8; (hi - lo) as usize];
    for s in elf.segments() {
        let at = (s.vaddr - lo) as usize;
        image[at..at + s.data.len()].copy_from_slice(s.data);
    }
    image
}

fn word_at(image: &[u8], at: u64) -> u64 {
    u64::from_le_bytes(image[at as usize..at as usize + 8].try_into().unwrap())
}

#[test]
fn relro_pages_become_read_only() {
    let bytes = static_pie(&[], &[], &[]).build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    assert!(elf.is_position_independent());
    assert_eq!(elf.relro(), Some((0x2000, 0x3000)));
    assert_eq!(
        runs(&elf, Relro::ReadOnly),
        vec![
            PageRun { start: 0, len: 0x1000, perms: p(R) },
            PageRun { start: 0x1000, len: 0x1000, perms: p(RX) },
            PageRun { start: 0x2000, len: 0x1000, perms: p(R) },
            PageRun { start: 0x3000, len: 0x2000, perms: p(RW) },
            PageRun { start: 0x5000, len: 0x1000, perms: p(R) },
        ]
    );
    // A program that relocates itself writes them.
    assert_eq!(runs(&elf, Relro::Writable)[2], PageRun { start: 0x2000, len: 0x3000, perms: p(RW) });
    // RELRO that ends inside a page, which other data shares: no page of it
    // can be read-only.
    let mut short = static_pie(&[], &[], &[]);
    short.extra.retain(|e| e.kind != pt::GNU_RELRO);
    short.extra.push(extra(pt::GNU_RELRO, 0x2240, 0x400));
    let bytes = short.build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    assert_eq!(elf.relro(), None);
    assert_eq!(runs(&elf, Relro::ReadOnly), runs(&elf, Relro::Writable));
}

#[test]
fn applies_relative_relocations() {
    let relative = amd64::MACHINE.relative as u64;
    let rela = [(0x2240, relative, 0x1040), (0x3008, 0, 0), (0x3010, relative, 0x3000)];
    // An address, then a bitmap of the 63 words after it, 0x2250 to
    // 0x2440 (bits 1 and 3: 0x2250 and 0x2260), then one of the next 63
    // (0x2448 and 0x2450).
    let relr = [0x2248, 1 | 1 << 1 | 1 << 3, 1 | 1 << 1 | 1 << 2];
    let mut tags = rela_tags(3);
    tags.extend([(dt::RELR, 0x5048), (dt::RELRSZ, 24), (dt::RELRENT, 8)]);
    let bytes = static_pie(&rela, &relr, &tags).build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    let mut image = image_of(&elf);
    let implicit = [0x2248, 0x2250, 0x2260, 0x2448, 0x2450];
    for at in implicit {
        image[at..at + 8].copy_from_slice(&0x1100u64.to_le_bytes());
    }
    let before = image.clone();
    let bias = 0x5555_0000_0000;
    elf.relocate(&mut image, bias).unwrap();
    assert_eq!(word_at(&image, 0x2240), bias + 0x1040);
    assert_eq!(word_at(&image, 0x3010), bias + 0x3000);
    for at in implicit {
        assert_eq!(word_at(&image, at as u64), bias + 0x1100, "{at:#x}");
    }
    // Nothing else changed: the words between those relocated, the one
    // the empty relocation names, and the rest of the image.
    let changed = [0x2240, 0x2248, 0x2250, 0x2260, 0x2448, 0x2450, 0x3010];
    for (i, (a, b)) in before.chunks(8).zip(image.chunks(8)).enumerate() {
        if !changed.contains(&(i as u64 * 8)) {
            assert_eq!(a, b, "{:#x}", i * 8);
        }
    }
}

#[test]
fn an_executable_without_relocations_is_left_alone() {
    let bytes = typical().build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    let mut image = image_of(&elf);
    let before = image.clone();
    elf.relocate(&mut image, 0x1000).unwrap();
    assert_eq!(image, before);
}

#[test]
fn refuses_relocations_a_static_executable_has_not() {
    let relative = amd64::MACHINE.relative as u64;
    let refused = |rela: &[(u64, u64, u64)], relr: &[u64], tags: &[(u64, u64)]| {
        let bytes = static_pie(rela, relr, tags).build();
        let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
        elf.relocate(&mut image_of(&elf), 0x1000).unwrap_err()
    };
    let one = |r| refused(&[r], &[], &rela_tags(1));
    // R_X86_64_64, of a symbol; and a relative relocation with a symbol.
    assert_eq!(one((0x2240, 1 << 32 | 1, 0)), ElfError::BadRelocation);
    assert_eq!(one((0x2240, 1 << 32 | relative, 0)), ElfError::BadRelocation);
    // In the code, in read-only data, across a segment's end, and past the
    // end of the data.
    for at in [0x1040, 0x10, 0x5000, 0x2ffc, 0x4ffc] {
        assert_eq!(one((at, relative, 0)), ElfError::BadRelocation, "{at:#x}");
    }
    assert_eq!(one((u64::MAX - 4, relative, 0)), ElfError::BadRelocation);
    // A bitmap without an address before it.
    let relr_tags = [(dt::RELR, 0x5000), (dt::RELRSZ, 8)];
    assert_eq!(refused(&[], &[3], &relr_tags), ElfError::BadRelocation);
    // A bitmap that names a word past the data (the one after 0x4ff8).
    assert_eq!(refused(&[], &[0x4ff8, 1 | 1 << 1], &[(dt::RELR, 0x5000), (dt::RELRSZ, 16)]), ElfError::BadRelocation);
    // Tables of another layout, or beyond what is loaded.
    let mut tags = rela_tags(1);
    tags[2].1 = 16;
    assert_eq!(refused(&[(0x2240, relative, 0)], &[], &tags), ElfError::BadRelocation);
    assert_eq!(refused(&[], &[], &[(dt::RELA, 0x5000), (dt::RELASZ, 25)]), ElfError::BadRelocation);
    assert_eq!(refused(&[], &[], &[(dt::RELA, 0x4ff0), (dt::RELASZ, 24)]), ElfError::BadRelocation);
    assert_eq!(refused(&[], &[], &[(dt::RELA, u64::MAX), (dt::RELASZ, 24)]), ElfError::BadRelocation);
    // Libraries, relocations of the code, of the PLT, or without addends.
    assert_eq!(refused(&[], &[], &[(dt::NEEDED, 1)]), ElfError::Dynamic);
    assert_eq!(refused(&[], &[], &[(dt::TEXTREL, 0)]), ElfError::BadRelocation);
    assert_eq!(refused(&[], &[], &[(dt::FLAGS, dt::DF_TEXTREL)]), ElfError::BadRelocation);
    assert_eq!(refused(&[], &[], &[(dt::PLTRELSZ, 24)]), ElfError::BadRelocation);
    assert_eq!(refused(&[], &[], &[(dt::RELSZ, 16)]), ElfError::BadRelocation);
    // An image that is not the span's.
    let bytes = static_pie(&[], &[], &[]).build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    assert_eq!(elf.relocate(&mut [0u8; 4096], 0), Err(ElfError::BadRelocation));
}

#[test]
fn strips_what_loading_does_not_read() {
    let relative = amd64::MACHINE.relative as u64;
    let mut exe = static_pie(&[(0x2240, relative, 0x1040)], &[], &rela_tags(1));
    exe.extra.push(Extra { kind: pt::NOTE, vaddr: 0, mem_size: 0, align: 4, file: note_bytes(b"Veda", 1, &[], 4) });
    let mut bytes = exe.build();
    let loaded = bytes.len();
    // Symbols and section headers after what is loaded, as linkers put them.
    bytes.extend([0x5a; 300]);
    bytes[40..48].copy_from_slice(&(loaded as u64 + 100).to_le_bytes());
    bytes[58..64].copy_from_slice(&[64, 0, 3, 0, 2, 0]);
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    assert_eq!(elf.loaded_len(), loaded);

    let mut stripped = bytes.clone();
    let len = strip(&mut stripped, amd64::MACHINE).unwrap();
    assert_eq!(len, loaded);
    stripped.truncate(len);
    assert!(stripped[40..48].iter().chain(&stripped[58..64]).all(|&b| b == 0));
    // It loads as it did.
    let (a, b) = (Elf::parse_for(&bytes, amd64::MACHINE).unwrap(), Elf::parse_for(&stripped, amd64::MACHINE).unwrap());
    assert_eq!(a.span(), b.span());
    assert_eq!(a.entry(), b.entry());
    assert_eq!(b.note(b"Veda", 1), Some(&[][..]));
    let (mut image_a, mut image_b) = (image_of(&a), image_of(&b));
    a.relocate(&mut image_a, 0x1000).unwrap();
    b.relocate(&mut image_b, 0x1000).unwrap();
    // The same image, but for the header's naming of section headers (the
    // first segment holds the header).
    image_a[40..48].fill(0);
    image_a[58..64].fill(0);
    assert_eq!(image_a, image_b);
    assert_eq!(runs(&a, Relro::ReadOnly), runs(&b, Relro::ReadOnly));
    // Stripping again changes nothing.
    let mut again = stripped.clone();
    assert_eq!(strip(&mut again, amd64::MACHINE), Ok(len));
    assert_eq!(again, stripped);
    assert_eq!(strip(&mut [0u8; 8], amd64::MACHINE), Err(ElfError::BadMagic));
}

/// A note's bytes, padded to `align`.
fn note_bytes(name: &[u8], kind: u32, desc: &[u8], align: usize) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend((name.len() as u32 + 1).to_le_bytes());
    b.extend((desc.len() as u32).to_le_bytes());
    b.extend(kind.to_le_bytes());
    b.extend(name);
    b.push(0);
    b.resize(b.len().next_multiple_of(align), 0);
    b.extend(desc);
    b.resize(b.len().next_multiple_of(align), 0);
    b
}

#[test]
fn reads_notes() {
    let mut exe = typical();
    let mut notes = note_bytes(b"Veda", 1, &2u32.to_le_bytes(), 4);
    notes.extend(note_bytes(b"GNU", 3, &[0xab; 20], 4));
    exe.extra.push(Extra { kind: pt::NOTE, vaddr: 0x400100, mem_size: notes.len() as u64, align: 4, file: notes });
    // GNU's properties, 8-byte aligned: a 4-byte name padded to 8.
    let property = note_bytes(b"GNU", 5, &[1, 2, 3, 4, 5, 6, 7, 8], 8);
    exe.extra.push(Extra { kind: pt::NOTE, vaddr: 0x400200, mem_size: 32, align: 8, file: property });
    let bytes = exe.build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    let notes: Vec<Note> = elf.notes().collect();
    assert_eq!(
        notes,
        vec![
            Note { name: b"Veda", kind: 1, desc: &2u32.to_le_bytes() },
            Note { name: b"GNU", kind: 3, desc: &[0xab; 20] },
            Note { name: b"GNU", kind: 5, desc: &[1, 2, 3, 4, 5, 6, 7, 8] },
        ]
    );
    assert_eq!(elf.note(b"Veda", 1), Some(&2u32.to_le_bytes()[..]));
    assert_eq!(elf.note(b"Veda", 2), None);
    assert_eq!(elf.note(b"Ved", 1), None);
    // Version 2 of Veda's conventions, which this crate does not know.
    assert_eq!(elf.kind(), Kind::OtherVeda);
}

#[test]
fn tells_veda_programs_from_posix_ones() {
    let with_note = |desc: &[u8]| {
        let mut exe = typical();
        let note = note_bytes(veda::NOTE_NAME, veda::NOTE_TYPE, desc, 4);
        exe.extra.push(Extra { kind: pt::NOTE, vaddr: 0, mem_size: 0, align: 4, file: note });
        exe.build()
    };
    let kind = |bytes: &[u8]| Elf::parse_for(bytes, amd64::MACHINE).unwrap().kind();
    assert_eq!(kind(&with_note(&veda::VERSION.to_le_bytes())), Kind::Veda);
    assert_eq!(kind(&with_note(&(veda::VERSION + 1).to_le_bytes())), Kind::OtherVeda);
    assert_eq!(kind(&with_note(&[])), Kind::OtherVeda);
    assert_eq!(kind(&typical().build()), Kind::Posix);
}

#[test]
fn malformed_notes_end_their_segment() {
    let mut exe = typical();
    let mut notes = note_bytes(b"Veda", 1, &[], 4);
    // A descriptor longer than the segment, then a note never reached.
    let mut long = note_bytes(b"X", 1, &[0; 8], 4);
    long[4..8].copy_from_slice(&64u32.to_le_bytes());
    notes.extend(long);
    notes.extend(note_bytes(b"Y", 1, &[], 4));
    exe.extra.push(Extra { kind: pt::NOTE, vaddr: 0, mem_size: 0, align: 4, file: notes });
    // A segment shorter than a note's header.
    exe.extra.push(Extra { kind: pt::NOTE, vaddr: 0, mem_size: 0, align: 4, file: vec![5, 0, 0, 0] });
    let bytes = exe.build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    let names: Vec<&[u8]> = elf.notes().map(|n| n.name).collect();
    assert_eq!(names, [b"Veda"]);
}

#[test]
fn position_independent_executables() {
    let mut exe = Exe::new(0x1040, vec![seg(R, 0, 0x100, 0), seg(RX, 0x1000, 0x100, 0x100)]);
    exe.kind = ET_DYN;
    let bytes = exe.build();
    let elf = Elf::parse_for(&bytes, amd64::MACHINE).unwrap();
    assert!(elf.is_position_independent());
    assert_eq!(elf.span(), (0, 0x2000));
}

#[test]
fn stack_size_request() {
    let mut exe = typical();
    exe.extra.push(extra(pt::GNU_STACK, 0, 16 << 20));
    let bytes = exe.build();
    assert_eq!(Elf::parse_for(&bytes, amd64::MACHINE).unwrap().stack_size(), Some(16 << 20));
}

#[test]
fn refuses_what_it_cannot_run() {
    let good = typical().build();
    let with = |f: &dyn Fn(&mut Vec<u8>)| {
        let mut b = good.clone();
        f(&mut b);
        Elf::parse_for(&b, amd64::MACHINE).unwrap_err()
    };
    assert_eq!(Elf::parse_for(b"MZ\x90\x00", amd64::MACHINE).unwrap_err(), ElfError::BadMagic);
    assert_eq!(Elf::parse_for(&good[..40], amd64::MACHINE).unwrap_err(), ElfError::Truncated);
    assert_eq!(Elf::parse_for(&good[..100], amd64::MACHINE).unwrap_err(), ElfError::Truncated);
    assert_eq!(with(&|b| b[4] = 1), ElfError::WrongClass);
    assert_eq!(with(&|b| b[5] = 2), ElfError::WrongClass);
    assert_eq!(with(&|b| b[18] = 3), ElfError::WrongMachine("x86-64"));
    assert_eq!(with(&|b| b[16] = 1), ElfError::NotExecutable);
    assert_eq!(with(&|b| b[56] = 0), ElfError::NoSegments);
    assert_eq!(with(&|b| b[54] = 32), ElfError::BadSegment);
    // The entry point in the read-only data.
    assert_eq!(with(&|b| b[24..32].copy_from_slice(&0x403000u64.to_le_bytes())), ElfError::BadEntry);

    let ph = |i: usize, at: usize| 64 + i * PHDR_SIZE + at;
    // A segment's file bytes beyond the end of the file.
    assert_eq!(with(&|b| b[ph(2, 32)..ph(2, 40)].copy_from_slice(&(1u64 << 40).to_le_bytes())), ElfError::BadSegment);
    // More file bytes than memory.
    assert_eq!(with(&|b| b[ph(3, 40)..ph(3, 48)].copy_from_slice(&0x100u64.to_le_bytes())), ElfError::BadSegment);
    // Overlapping segments.
    assert_eq!(with(&|b| b[ph(2, 16)..ph(2, 24)].copy_from_slice(&0x401100u64.to_le_bytes())), ElfError::BadSegment);
    // An address range that wraps around.
    assert_eq!(with(&|b| b[ph(3, 16)..ph(3, 24)].copy_from_slice(&(u64::MAX - 8).to_le_bytes())), ElfError::BadSegment);
    // A huge image.
    assert_eq!(with(&|b| b[ph(3, 40)..ph(3, 48)].copy_from_slice(&(8u64 << 30).to_le_bytes())), ElfError::TooLarge);

    let mut dynamic = typical();
    dynamic.extra.push(extra(pt::INTERP, 0, 0));
    assert_eq!(Elf::parse_for(&dynamic.build(), amd64::MACHINE).unwrap_err(), ElfError::Dynamic);
    // Program headers said to be where nothing is loaded.
    let mut stray = typical();
    stray.extra.push(extra(pt::PHDR, 0x1000_0000, 0));
    assert_eq!(Elf::parse_for(&stray.build(), amd64::MACHINE).unwrap_err(), ElfError::NoSegments);
    // A dynamic section beyond the end of the file.
    let mut beyond = typical();
    beyond.extra.push(Extra { kind: pt::DYNAMIC, vaddr: 0, mem_size: 16, align: 8, file: vec![0; 16] });
    let mut b = beyond.build();
    let at = ph(4, 32);
    b[at..at + 8].copy_from_slice(&4096u64.to_le_bytes());
    assert_eq!(Elf::parse_for(&b, amd64::MACHINE).unwrap_err(), ElfError::BadSegment);
    // RELRO that wraps around.
    let mut wraps = typical();
    wraps.extra.push(extra(pt::GNU_RELRO, u64::MAX - 8, 4096));
    assert_eq!(Elf::parse_for(&wraps.build(), amd64::MACHINE).unwrap_err(), ElfError::BadSegment);
}

#[test]
fn takes_at_most_max_segments() {
    // Code with the program headers, then a page of data after another.
    let segments = |n: u64| {
        (0..n)
            .map(|i| if i == 0 { seg(RX, 0x1000, 0x1000, 0x1000) } else { seg(RW, 0x1000 * (i + 1), 0x10, 0x10) })
            .collect()
    };
    let mut exe = Exe::new(0x1000, segments(MAX_SEGMENTS as u64));
    exe.headers_in_first = false;
    exe.extra.push(extra(pt::PHDR, 0x1000, 0));
    let bytes = exe.build();
    assert_eq!(page_runs(&Elf::parse_for(&bytes, amd64::MACHINE).unwrap(), Relro::ReadOnly).count(), 2);
    exe.segs = segments(MAX_SEGMENTS as u64 + 1);
    assert_eq!(Elf::parse_for(&exe.build(), amd64::MACHINE).unwrap_err(), ElfError::BadSegment);
}

/// A stack image: the bytes below `top` that [`initial_stack`] filled,
/// from the stack pointer.
struct StackImage {
    bytes: Vec<u8>,
    sp: u64,
}

fn stack(top: u64, args: &[&[u8]], env: &[&[u8]], aux: &AuxInfo) -> StackImage {
    let size = initial_stack_size(args, env, aux);
    // Bytes below the stack pointer are left as they were.
    let mut out = vec![0xee; size + 64];
    let sp = initial_stack(top, args, env, aux, &mut out).unwrap();
    let bottom = top - out.len() as u64;
    assert!(sp - bottom >= 64, "{} bytes used, at most {size} expected", top - sp);
    assert!(out[..(sp - bottom) as usize].iter().all(|&b| b == 0xee));
    StackImage { bytes: out[(sp - bottom) as usize..].to_vec(), sp }
}

fn word(img: &StackImage, addr: u64) -> u64 {
    let top = img.sp + img.bytes.len() as u64;
    assert!(addr >= img.sp && addr + 8 <= top, "{addr:#x} is outside the stack image");
    let at = (addr - img.sp) as usize;
    u64::from_le_bytes(img.bytes[at..at + 8].try_into().unwrap())
}

fn c_string(img: &StackImage, addr: u64) -> Vec<u8> {
    let at = (addr - img.sp) as usize;
    let len = img.bytes[at..].iter().position(|&b| b == 0).unwrap();
    img.bytes[at..at + len].to_vec()
}

fn aux(execfn: &[u8]) -> AuxInfo<'_> {
    AuxInfo {
        phdr: 0x400040,
        phnum: 4,
        entry: 0x401040,
        hwcap: 0x178bfbff,
        platform: amd64::MACHINE.platform,
        uid: 1000,
        gid: 1000,
        random: *b"0123456789abcdef",
        execfn,
    }
}

#[test]
fn lays_out_the_initial_stack() {
    let top = 0x7fff_0000_0000u64;
    let args: [&[u8]; 3] = [b"cc", b"-o", b"hello world"];
    let env: [&[u8]; 2] = [b"HOME=/home/user", b"PATH=/system/bin"];
    let img = stack(top, &args, &env, &aux(b"/system/bin/cc"));
    assert_eq!(img.sp % 16, 0);
    assert_eq!(img.sp + img.bytes.len() as u64, top);

    let mut p = img.sp;
    let mut next = || {
        let v = word(&img, p);
        p += 8;
        v
    };
    assert_eq!(next(), 3);
    for a in args {
        let ptr = next();
        assert_eq!(c_string(&img, ptr), a);
    }
    assert_eq!(next(), 0);
    for e in env {
        let ptr = next();
        assert_eq!(c_string(&img, ptr), e);
    }
    assert_eq!(next(), 0);
    let mut auxv = Vec::new();
    loop {
        let (k, v) = (next(), next());
        if k == at::NULL {
            break;
        }
        auxv.push((k, v));
    }
    let vectors_end = p;
    let get = |k| auxv.iter().find(|(kind, _)| *kind == k).map(|&(_, v)| v).unwrap();
    assert_eq!(get(at::PHDR), 0x400040);
    assert_eq!(get(at::PHENT), 56);
    assert_eq!(get(at::PHNUM), 4);
    assert_eq!(get(at::PAGESZ), 4096);
    assert_eq!(get(at::ENTRY), 0x401040);
    assert_eq!(get(at::UID), 1000);
    assert_eq!(get(at::EGID), 1000);
    assert_eq!(get(at::SECURE), 0);
    assert_eq!(c_string(&img, get(at::PLATFORM)), b"x86_64");
    assert_eq!(c_string(&img, get(at::EXECFN)), b"/system/bin/cc");
    let random = get(at::RANDOM);
    assert_eq!(random, top - 16);
    assert_eq!(&img.bytes[(random - img.sp) as usize..], b"0123456789abcdef");
    // The padding between the vectors and the strings (the first argument
    // the lowest) is zero.
    let first_string = word(&img, img.sp + 8);
    assert!(img.bytes[(vectors_end - img.sp) as usize..(first_string - img.sp) as usize].iter().all(|&b| b == 0));
}

#[test]
fn the_stack_needs_its_size() {
    let big = vec![b'x'; 5000];
    let args: [&[u8]; 2] = [b"prog", &big];
    let size = initial_stack_size(&args, &[], &aux(b"/p"));
    assert!(size > 5000);
    let mut short = vec![0u8; 5000];
    assert_eq!(initial_stack(1 << 40, &args, &[], &aux(b"/p"), &mut short).unwrap_err(), ElfError::TooLarge);
    let mut enough = vec![0u8; size];
    assert!(initial_stack(1 << 40, &args, &[], &aux(b"/p"), &mut enough).is_ok());
    // Below the address space.
    assert_eq!(initial_stack(64, &args, &[], &aux(b"/p"), &mut enough).unwrap_err(), ElfError::TooLarge);
    // No arguments at all still gives a valid (empty) vector.
    let img = stack(1 << 40, &[], &[], &aux(b""));
    assert_eq!(word(&img, img.sp), 0);
    assert_eq!(word(&img, img.sp + 8), 0);
    assert_eq!(word(&img, img.sp + 16), 0);
}

/// Random numbers for the mutation tests (xorshift*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

/// Executables of every layout the tests build: a static one, a static PIE
/// with relocations of both kinds and a note, one whose segments share a
/// page.
fn corpus() -> Vec<Vec<u8>> {
    let relative = amd64::MACHINE.relative as u64;
    let mut tags = rela_tags(2);
    tags.extend([(dt::RELR, 0x5030), (dt::RELRSZ, 16), (dt::RELRENT, 8)]);
    let mut pie = static_pie(&[(0x2240, relative, 0x1040), (0x3010, relative, 0x3000)], &[0x2248, 3], &tags);
    let note = note_bytes(veda::NOTE_NAME, veda::NOTE_TYPE, &veda::VERSION.to_le_bytes(), 4);
    pie.extra.push(Extra { kind: pt::NOTE, vaddr: 0, mem_size: 0, align: 4, file: note });
    pie.extra.push(extra(pt::GNU_STACK, 0, 1 << 20));
    let mut shared = Exe::new(0x401000, vec![seg(RX, 0x401000, 0x900, 0x900), seg(RW, 0x401a00, 0x10, 0x1000)]);
    shared.headers_in_first = false;
    shared.extra.push(extra(pt::PHDR, 0x401000, 0));
    vec![typical().build(), pie.build(), shared.build()]
}

/// `data` with a few random changes, most of them in the headers, where
/// the parser trusts nothing.
fn mutate(rng: &mut Rng, data: &[u8]) -> Vec<u8> {
    let mut d = data.to_vec();
    for _ in 0..1 + rng.below(4) {
        if d.is_empty() {
            d.push(rng.next() as u8);
            continue;
        }
        // The file header and program headers, or anywhere.
        let headers = d.len().min(EHDR_SIZE + 8 * PHDR_SIZE);
        let pos = if rng.below(4) == 0 { rng.below(d.len()) } else { rng.below(headers) };
        match rng.below(8) {
            0 | 1 => d[pos] ^= 1 << rng.below(8),
            2 => d[pos] = rng.next() as u8,
            3 => d.truncate(pos),
            4 => {
                let end = (pos + 1 + rng.below(32)).min(d.len());
                d.drain(pos..end);
            }
            _ => {
                // Large numbers where headers keep addresses, offsets and
                // sizes (8-byte aligned, as their fields are).
                let at = pos & !7;
                let v = [u64::MAX, 1 << 63, (1 << 32) - 1, 0x7fff_ffff_f000, 0x1000, 0][rng.below(6)];
                let end = (at + 8).min(d.len());
                d[at..end].copy_from_slice(&v.to_le_bytes()[..end - at]);
            }
        }
    }
    d
}

/// Everything a loader does with an executable, which must fail cleanly on
/// any input, never panic.
fn exercise(bytes: &[u8]) {
    let Ok(elf) = Elf::parse_for(bytes, amd64::MACHINE) else { return };
    let _ = elf.kind();
    let _ = (elf.stack_size(), elf.relro(), elf.notes().count(), elf.loaded_len());
    for relro in [Relro::ReadOnly, Relro::Writable] {
        let runs: Vec<PageRun> = page_runs(&elf, relro).collect();
        let (lo, hi) = elf.span();
        assert!(runs.iter().all(|r| r.len > 0 && lo <= r.start && r.start + r.len <= hi));
        assert!(runs.windows(2).all(|w| w[0].start + w[0].len <= w[1].start));
    }
    let (lo, hi) = elf.span();
    if hi - lo <= 16 << 20 {
        let _ = elf.relocate(&mut image_of(&elf), 0x5555_0000_0000);
    }
    let _ = strip(&mut bytes.to_vec(), amd64::MACHINE);
}

fn run_mutations(seed: u64, iterations: usize) {
    let mut rng = Rng(seed);
    for exe in corpus() {
        for _ in 0..iterations {
            exercise(&mutate(&mut rng, &exe));
        }
    }
}

#[test]
fn mutations_never_panic() {
    run_mutations(0x9E37_79B9_7F4A_7C15, 4000);
}

/// A much longer run (half a million executables); run manually.
#[test]
#[ignore]
fn mutations_never_panic_long() {
    for seed in 1..=10 {
        run_mutations(seed * 0x1234_5678_9ABC_DEF1, 16_000);
    }
}

#[test]
fn every_truncation() {
    for exe in corpus() {
        for len in 0..exe.len() {
            exercise(&exe[..len]);
        }
    }
}

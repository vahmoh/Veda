// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The image read as its readers read it: glibc's dynamic linker and Go
//! (each hash table, the versions), musl (its symbols counted from
//! `DT_HASH`, then each looked at), an unwinder (`PT_GNU_EH_FRAME`) and a
//! debugger (the section headers).

use std::string::String;
use std::vec::Vec;

use super::format::*;
use super::*;

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn i32_at(b: &[u8], at: usize) -> i64 {
    i64::from(u32_at(b, at) as i32)
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn c_string(b: &[u8], at: usize) -> String {
    let len = b[at..].iter().position(|&c| c == 0).unwrap();
    String::from_utf8(b[at..at + len].to_vec()).unwrap()
}

/// Code for the image: bytes that are no instructions, and functions in
/// them, their names those of x86-64's vDSO, each with its other name, one
/// rule for all of them.
fn code() -> Code {
    let text: &'static [u8] = std::vec![0xcc; 200].leak();
    let names = [
        ("__vdso_clock_gettime", "clock_gettime"),
        ("__vdso_gettimeofday", "gettimeofday"),
        ("__vdso_time", "time"),
        ("__vdso_clock_getres", "clock_getres"),
        ("__vdso_getcpu", "getcpu"),
    ];
    let functions = names
        .iter()
        .enumerate()
        .map(|(i, &(name, alias))| Function { name, alias: Some(alias), offset: 40 * i, size: 30 })
        .collect();
    let frame = Frame { return_address: 16, data_alignment: -8, signal: false, instructions: &[0x0c, 7, 8, 0x90, 1] };
    Code { text, functions, version: "LINUX_2.6", frames: std::vec![(0..text.len(), frame)], restorer: None }
}

/// Code for the image as ARM64's has it: functions of one name each, the
/// signal return last, in a part of its own whose rule is a signal
/// frame's, after a no-op.
fn arm64_code() -> Code {
    let text: &'static [u8] = std::vec![0; 208].leak();
    let names = ["__kernel_clock_gettime", "__kernel_gettimeofday", "__kernel_clock_getres", "__kernel_rt_sigreturn"];
    let functions = names
        .iter()
        .enumerate()
        .map(|(i, &name)| Function { name, alias: None, offset: if i == 3 { 196 } else { 64 * i }, size: 8 })
        .collect();
    let called = Frame { return_address: 30, data_alignment: -8, signal: false, instructions: &[0x0c, 31, 0] };
    let signal = Frame {
        return_address: 30,
        data_alignment: 8,
        signal: true,
        instructions: &[0x0c, 29, 0, 0x80 | 29, 0, 0x80 | 30, 1],
    };
    Code {
        text,
        functions,
        version: "LINUX_2.6.39",
        frames: std::vec![(0..192, called), (192..208, signal)],
        restorer: Some(196),
    }
}

/// What the dynamic section says, as its readers take it: where each table
/// is from the image's start (it is linked at 0).
struct Dynamic {
    strtab: usize,
    symtab: usize,
    hash: usize,
    gnu_hash: usize,
    versym: usize,
    verdef: usize,
    soname: String,
}

/// The program headers of type `kind`: their offset, address and size.
fn segments(image: &[u8], kind: u32) -> Vec<(u64, u64, u64)> {
    let (phoff, phnum) = (u64_at(image, 32) as usize, u16_at(image, 56) as usize);
    (0..phnum)
        .map(|i| phoff + i * PHDR)
        .filter(|&p| u32_at(image, p) == kind)
        .map(|p| (u64_at(image, p + 8), u64_at(image, p + 16), u64_at(image, p + 40)))
        .collect()
}

fn dynamic(image: &[u8]) -> Dynamic {
    let (offset, _, _) = segments(image, elf::pt::DYNAMIC)[0];
    let mut at = offset as usize;
    let mut tags = std::collections::BTreeMap::new();
    while u64_at(image, at) != DT_NULL {
        tags.insert(u64_at(image, at), u64_at(image, at + 8) as usize);
        at += DYN;
    }
    let strtab = tags[&DT_STRTAB];
    assert_eq!(tags[&DT_SYMENT], SYM);
    assert_eq!(tags[&DT_VERDEFNUM], 2);
    Dynamic {
        strtab,
        symtab: tags[&DT_SYMTAB],
        hash: tags[&DT_HASH],
        gnu_hash: tags[&DT_GNU_HASH],
        versym: tags[&DT_VERSYM],
        verdef: tags[&DT_VERDEF],
        soname: c_string(image, strtab + tags[&DT_SONAME]),
    }
}

/// Symbol `i`: its name, binding, type, section, value and size.
fn symbol(image: &[u8], d: &Dynamic, i: usize) -> (String, u8, u8, u16, u64, u64) {
    let at = d.symtab + i * SYM;
    let info = image[at + 4];
    let name = c_string(image, d.strtab + u32_at(image, at) as usize);
    (name, info >> 4, info & 0xf, u16_at(image, at + 6), u64_at(image, at + 8), u64_at(image, at + 16))
}

/// The name of the version symbol `i` is of, as its `DT_VERSYM` index
/// finds it among the definitions that are not the object's own.
fn version(image: &[u8], d: &Dynamic, i: usize) -> Option<String> {
    let index = u16_at(image, d.versym + 2 * i) & 0x7fff;
    let mut def = d.verdef;
    loop {
        if u16_at(image, def + 2) & VER_FLG_BASE == 0 && u16_at(image, def + 4) == index {
            let aux = def + u32_at(image, def + 12) as usize;
            return Some(c_string(image, d.strtab + u32_at(image, aux) as usize));
        }
        match u32_at(image, def + 16) {
            0 => return None,
            next => def += next as usize,
        }
    }
}

/// Symbol `name` through `DT_HASH`, as Go and old dynamic linkers look.
fn by_hash(image: &[u8], d: &Dynamic, name: &str) -> Option<usize> {
    let buckets = u32_at(image, d.hash);
    let chains = d.hash + 8 + 4 * buckets as usize;
    let mut i = u32_at(image, d.hash + 8 + 4 * (elf_hash(name.as_bytes()) % buckets) as usize);
    while i != 0 {
        if symbol(image, d, i as usize).0 == name {
            return Some(i as usize);
        }
        i = u32_at(image, chains + 4 * i as usize);
    }
    None
}

/// Symbol `name` through `DT_GNU_HASH`, as glibc's dynamic linker looks:
/// past the Bloom filter, along its bucket's run of hashes.
fn by_gnu_hash(image: &[u8], d: &Dynamic, name: &str) -> Option<usize> {
    let g = d.gnu_hash;
    let (buckets, first, words, shift) =
        (u32_at(image, g), u32_at(image, g + 4), u32_at(image, g + 8), u32_at(image, g + 12));
    assert!(words.is_power_of_two());
    let h = gnu_hash(name.as_bytes());
    let word = u64_at(image, g + 16 + 8 * ((h / 64) % words) as usize);
    if (word >> (h % 64)) & (word >> ((h >> shift) % 64)) & 1 == 0 {
        return None;
    }
    let bucket_at = g + 16 + 8 * words as usize;
    let chains = bucket_at + 4 * buckets as usize;
    let mut i = u32_at(image, bucket_at + 4 * (h % buckets) as usize);
    if i == 0 {
        return None;
    }
    loop {
        let hash = u32_at(image, chains + 4 * (i - first) as usize);
        if hash | 1 == h | 1 && symbol(image, d, i as usize).0 == name {
            return Some(i as usize);
        }
        if hash & 1 != 0 {
            return None;
        }
        i += 1;
    }
}

#[test]
fn hashes_are_the_abi_s() {
    // As Go's runtime has them for x86-64's vDSO...
    assert_eq!(elf_hash(b"LINUX_2.6"), 0x3ae75f6);
    assert_eq!(elf_hash(b"__vdso_gettimeofday"), 0x315ca59);
    assert_eq!(gnu_hash(b"__vdso_gettimeofday"), 0xb01bca00);
    assert_eq!(elf_hash(b"__vdso_clock_gettime"), 0xd35ec75);
    assert_eq!(gnu_hash(b"__vdso_clock_gettime"), 0x6e43a318);
    // ... and for ARM64's.
    assert_eq!(elf_hash(b"LINUX_2.6.39"), 0x75fcb89);
    assert_eq!(elf_hash(b"__kernel_clock_gettime"), 0xb0cd725);
    assert_eq!(gnu_hash(b"__kernel_clock_gettime"), 0xdfa941fd);
    assert_eq!((elf_hash(b""), gnu_hash(b"")), (0, 5381));
}

#[test]
fn the_note_says_the_release() {
    assert_eq!(version_code("6.6.0-veda"), 0x06_06_00);
    assert_eq!(version_code("5.15.300"), 0x05_0f_ff);
    assert_eq!(version_code(crate::RELEASE), 0x06_06_00);
}

#[test]
fn clocks_are_read_as_the_posix_layer_reads_them() {
    use linux_raw_sys::general::*;
    assert_eq!(UTC_CLOCKS, 1 << CLOCK_REALTIME | 1 << CLOCK_REALTIME_COARSE);
    let monotonic = 1 << CLOCK_MONOTONIC | 1 << CLOCK_MONOTONIC_RAW | 1 << CLOCK_MONOTONIC_COARSE | 1 << CLOCK_BOOTTIME;
    assert_eq!(MONOTONIC_CLOCKS, monotonic);
}

#[test]
fn the_data_page_holds_the_scale() {
    let scale = ClockInfo { ticks_at_zero: 1, ns_per_tick: 2, ticks_hz: 3, realtime_at_zero: 4, utc_at_zero: 5 };
    let page = data(Some(scale));
    let word = |at| u64::from_ne_bytes(page[at..at + 8].try_into().unwrap());
    assert_eq!(word(offset_of!(ClockInfo, ticks_at_zero)), 1);
    assert_eq!(word(offset_of!(ClockInfo, ns_per_tick)), 2);
    assert_eq!(word(offset_of!(ClockInfo, utc_at_zero)), 5);
    // None: the system call reads the clocks.
    assert!(data(None).iter().all(|&b| b == 0));
}

#[test]
fn functions_are_found_by_name_and_version() {
    for code in [code(), arm64_code()] {
        let image = build(&code);
        // Veda's own reader of executables takes it.
        elf::Elf::parse(&image).unwrap();
        assert_eq!(u16_at(&image, 16), ET_DYN);
        // One loadable segment, the image whole, linked at 0.
        assert_eq!(segments(&image, elf::pt::LOAD), [(0, 0, image.len() as u64)]);
        let d = dynamic(&image);
        assert_eq!(d.soname, "linux-vdso.so.1");
        // musl's count of the symbols, from `DT_HASH`: each function by its
        // names.
        let count = u32_at(&image, d.hash + 4) as usize;
        let names: usize = code.functions.iter().map(|f| 1 + usize::from(f.alias.is_some())).sum();
        assert_eq!(count, 1 + names);
        assert_eq!(symbol(&image, &d, 0), (String::new(), 0, 0, 0, 0, 0));
        for f in &code.functions {
            for (name, bind) in [(Some(f.name), STB_GLOBAL), (f.alias, STB_WEAK)] {
                let Some(name) = name else { continue };
                let i = by_hash(&image, &d, name).unwrap_or_else(|| panic!("{name} by DT_HASH"));
                assert_eq!(by_gnu_hash(&image, &d, name), Some(i), "{name} by DT_GNU_HASH");
                assert!((1..count).contains(&i));
                let value = (TEXT + f.offset) as u64;
                let size = f.size as u64;
                assert_eq!(
                    symbol(&image, &d, i),
                    (String::from(name), bind, STT_FUNC, Index::Text as u16, value, size)
                );
                assert_eq!(version(&image, &d, i).as_deref(), Some(code.version), "{name}");
            }
        }
        // What is not there is not found, by either table.
        for name in ["__vdso_getrandom", "__vdso_clock_gettime64", "__kernel_time", code.version, ""] {
            assert_eq!(by_hash(&image, &d, name), None);
            assert_eq!(by_gnu_hash(&image, &d, name), None);
        }
        // The versions: the object's own, which no symbol is of, then the
        // functions', each with its hash, as Go matches them.
        let (base, functions) = (d.verdef, d.verdef + VERDEF + VERDAUX);
        assert_eq!(u32_at(&image, base + 16) as usize, VERDEF + VERDAUX);
        assert_eq!((u16_at(&image, base + 2), u16_at(&image, base + 4)), (VER_FLG_BASE, 1));
        assert_eq!(u32_at(&image, base + 8), elf_hash(b"linux-vdso.so.1"));
        assert_eq!((u16_at(&image, functions + 2), u16_at(&image, functions + 4)), (0, 2));
        assert_eq!(u32_at(&image, functions + 8), elf_hash(code.version.as_bytes()));
        assert_eq!(u32_at(&image, functions + 16), 0);
        // The code, where the symbols say.
        assert_eq!(&image[TEXT..TEXT + code.text.len()], code.text);
    }
}

#[test]
fn the_note_and_the_call_frame_information_are_found() {
    let code = code();
    let image = build(&code);
    // The note: `LINUX_VERSION_CODE`, as Linux's.
    let [(note, _, size)] = segments(&image, elf::pt::NOTE)[..] else { panic!("one note") };
    let note = note as usize;
    assert_eq!((u32_at(&image, note), u32_at(&image, note + 4), u32_at(&image, note + 8)), (6, 4, 0));
    assert_eq!(&image[note + 12..note + 18], b"Linux\0");
    assert_eq!(u32_at(&image, note + 20), version_code(crate::RELEASE));
    assert_eq!(size, 24);
    // The index of `.eh_frame`: one entry, for the code, whose frame
    // description entry covers it all and follows its common entry's rule.
    let parts = frames(&image);
    assert_eq!(parts.len(), 1);
    let (start, len, augmentation, rule) = &parts[0];
    assert_eq!((*start, *len), (TEXT, code.text.len()));
    assert_eq!(augmentation.as_slice(), b"zR");
    assert_eq!(rule.as_slice(), &[1, 0x78, 16, 1, PCREL | SDATA4, 0x0c, 7, 8, 0x90, 1]);
}

/// What `.eh_frame_hdr` indexes, as an unwinder finds it: each frame
/// description entry's code (its start in the image and its length, from
/// the entry itself), and its common entry's augmentation and the rest of
/// it (alignments, return address column, augmentation data and
/// instructions, as far as the padding). The common entries come first
/// each time, and the entries end with a word of none.
fn frames(image: &[u8]) -> Vec<(usize, usize, Vec<u8>, Vec<u8>)> {
    let [(header, _, _)] = segments(image, elf::pt::GNU_EH_FRAME)[..] else { panic!("one index") };
    let header = header as usize;
    assert_eq!(&image[header..header + 4], &[1, PCREL | SDATA4, UDATA4, DATAREL | SDATA4]);
    let first = (header as i64 + 4 + i32_at(image, header + 4)) as usize;
    let count = u32_at(image, header + 8) as usize;
    let mut parts = Vec::new();
    let mut last = None;
    for n in 0..count {
        let entry = header + 12 + 8 * n;
        let start = (header as i64 + i32_at(image, entry)) as usize;
        let fde = (header as i64 + i32_at(image, entry + 4)) as usize;
        let cie = fde + 4 - u32_at(image, fde + 4) as usize;
        assert_eq!(cie, if n == 0 { first } else { last.unwrap() }, "a common entry before each");
        assert_eq!(u32_at(image, cie + 4), 0, "a common entry");
        assert_eq!(image[cie + 8], 1, "version 1");
        let augmentation: Vec<u8> = image[cie + 9..].iter().take_while(|&&b| b != 0).copied().collect();
        let rule_at = cie + 10 + augmentation.len();
        let cie_end = cie + 4 + u32_at(image, cie) as usize;
        assert_eq!(cie_end, fde);
        assert_eq!((fde - cie) % 8, 0);
        let rule = image[rule_at..cie_end].iter().rev().skip_while(|&&b| b == 0).collect::<Vec<_>>();
        let rule: Vec<u8> = rule.into_iter().rev().copied().collect();
        assert_eq!(fde as i64 + 8 + i32_at(image, fde + 8), start as i64);
        let len = u32_at(image, fde + 12) as usize;
        parts.push((start, len, augmentation, rule));
        // The next common entry, or the entries' end.
        let next = fde + 4 + u32_at(image, fde) as usize;
        last = Some(next);
        if n + 1 == count {
            assert_eq!(u32_at(image, next), 0);
        }
    }
    // The table is in the order of the code, and leaves none of it out.
    assert!(parts.windows(2).all(|w| w[0].0 + w[0].1 == w[1].0));
    parts
}

#[test]
fn a_signal_return_is_a_signal_frame() {
    let code = arm64_code();
    let image = build(&code);
    let parts = frames(&image);
    // The functions, whose caller is at the link register, then the signal
    // return, whose caller is the interrupted code, by the frame record.
    assert_eq!(parts.len(), 2);
    assert_eq!((parts[0].0, parts[0].1, parts[0].2.as_slice()), (TEXT, 192, b"zR".as_slice()));
    assert_eq!(parts[0].3, [1, 0x78, 30, 1, PCREL | SDATA4, 0x0c, 31]);
    assert_eq!((parts[1].0, parts[1].1, parts[1].2.as_slice()), (TEXT + 192, 16, b"zRS".as_slice()));
    assert_eq!(parts[1].3, [1, 8, 30, 1, PCREL | SDATA4, 0x0c, 29, 0, 0x9d, 0, 0x9e, 1]);
}

#[test]
fn the_sections_say_what_the_image_holds() {
    let code = code();
    let image = build(&code);
    let (shoff, shnum, names) = (u64_at(&image, 40) as usize, u16_at(&image, 60) as usize, u16_at(&image, 62) as usize);
    let header = |i: usize| shoff + i * SHDR;
    let strings = u64_at(&image, header(names) + 24) as usize;
    let sections: Vec<(String, u32, u64, u64, u64, u32)> = (0..shnum)
        .map(|i| {
            let h = header(i);
            let name = c_string(&image, strings + u32_at(&image, h) as usize);
            (
                name,
                u32_at(&image, h + 4),
                u64_at(&image, h + 8),
                u64_at(&image, h + 24),
                u64_at(&image, h + 32),
                u32_at(&image, h + 40),
            )
        })
        .collect();
    let find = |name: &str| sections.iter().position(|s| s.0 == name).unwrap();
    assert_eq!(sections[0], (String::new(), 0, 0, 0, 0, 0));
    let text = find(".text");
    assert_eq!(text, Index::Text as usize);
    assert_eq!(sections[text].1, SHT_PROGBITS);
    assert_eq!(sections[text].2, SHF_ALLOC | SHF_EXECINSTR);
    assert_eq!((sections[text].3, sections[text].4), (TEXT as u64, code.text.len() as u64));
    let d = dynamic(&image);
    assert_eq!(sections[find(".dynsym")].3 as usize, d.symtab);
    assert_eq!(sections[find(".dynsym")].5 as usize, find(".dynstr"));
    assert_eq!(sections[find(".hash")].5 as usize, find(".dynsym"));
    assert_eq!(sections[find(".gnu.version_d")].1, SHT_GNU_VERDEF);
    // Every section is in the image, and every one that takes memory where
    // the image has it.
    for (name, _, flags, at, size, _) in &sections[1..] {
        assert!(at + size <= image.len() as u64, "{name}");
        assert!(flags & SHF_ALLOC == 0 || u64_at(&image, header(find(name)) + 16) == *at, "{name}");
    }
}

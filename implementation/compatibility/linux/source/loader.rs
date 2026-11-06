// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Loading a Linux program into this process, as Linux's ELF loader does
//! (`load_elf_binary`): its image where it was linked, or at a random
//! address if it is position-independent; its dynamic linker, if it names
//! one (`PT_INTERP`, found in its own tree: `crate::namespace`), at a
//! random address of its own; its program break after its image; and its
//! stack, at the top of the address space, with its arguments,
//! environment and auxiliary vector as the System V ABI lays them out
//! (`elf::initial_stack`).
//!
//! Pages its image only reads are its file's own where they are a whole
//! number of pages from where they go (as a file of the system image's
//! are, which every process that runs it shares); the rest is memory of
//! the program's own, its bytes copied in. Each area is the program's, as
//! `crate::memory` keeps them.

use alloc::string::String;
use alloc::vec::Vec;

use abi::map_flags;
use elf::{Elf, PageRun, Relro};
use runtime::object::Vmo;
use runtime::vm;

use crate::linux::errno::*;
use crate::linux::mman::{PROT_EXEC, PROT_READ, PROT_WRITE};
use crate::memory::{self, Kind};

const PAGE: u64 = abi::PAGE_SIZE as u64;

/// The main thread's stack (Linux's `RLIMIT_STACK` default, which the
/// POSIX layer reports), committed only as it is used.
pub const STACK: usize = 8 << 20;
/// Inaccessible pages below it.
const STACK_GUARD: usize = 1 << 20;

/// Where position-independent programs go on Linux (`ELF_ET_DYN_BASE`, two
/// thirds of the 47-bit space), give or take a random number of pages.
const PROGRAM_BASE: u64 = 0x5555_5555_4000;
/// Where its libraries and dynamic linker go, below the stack.
const LIBRARY_TOP: u64 = 0x7f00_0000_0000;
/// How far below the top of the address space the stack may be placed.
const STACK_SPAN: u64 = 16 << 30;
/// How far after the image the program break may start (Linux's 32 MiB).
const BREAK_SPAN: u64 = 32 << 20;

/// A program loaded, ready to run.
pub struct Loaded {
    /// Where its main thread starts: its dynamic linker's entry, or its
    /// own.
    pub entry: u64,
    /// Its main thread's stack pointer, at `argc`.
    pub sp: u64,
    /// Its auxiliary vector, as `/proc/self/auxv` gives it.
    pub auxv: Vec<u8>,
}

/// A program file: its bytes, and the program's path of it.
pub struct File {
    pub vmo: Vmo,
    pub len: usize,
    pub path: String,
}

/// A random number, for where things go.
fn random() -> u64 {
    let mut b = [0u8; 8];
    let _ = runtime::object::try_random_bytes(&mut b);
    u64::from_le_bytes(b)
}

/// Checks that `image` is a Linux program this processor runs, and finds
/// its dynamic linker in the program's tree: what [`load`] loads, checked
/// before anything of the running program goes (`execve`'s point of no
/// return comes after).
pub fn check(image: &[u8]) -> Result<Option<File>, isize> {
    let elf = Elf::parse(image).map_err(|_| ENOEXEC)?;
    let Some(path) = elf.interpreter() else { return Ok(None) };
    let path = core::str::from_utf8(path).map_err(|_| ENOEXEC)?;
    let (vmo, len) = posix::host::read_file(path).map_err(|e| if e == ENOENT { ENOENT } else { ENOEXEC })?;
    let linker = File { vmo, len, path: String::from(path) };
    with_image(&linker, |image| {
        let elf = Elf::parse(image).map_err(|_| ELIBBAD)?;
        // Linux's dynamic linkers are position-independent, and name none.
        if elf.interpreter().is_some() || !elf.is_position_independent() {
            return Err(ELIBBAD);
        }
        Ok(())
    })?;
    Ok(Some(linker))
}

const ELIBBAD: isize = linux_raw_sys::errno::ELIBBAD as isize;

/// Runs `f` with the bytes of `file`, mapped to read.
pub fn with_image<R>(file: &File, f: impl FnOnce(&[u8]) -> Result<R, isize>) -> Result<R, isize> {
    let dup = file.vmo.0.duplicate(None).map_err(|_| ENOMEM)?;
    let map = vm::Mapping::new(Vmo(dup), file.len.max(1), map_flags::READ).map_err(|_| ENOMEM)?;
    // SAFETY: a read-only mapping of the file, `len` bytes, which lives
    // until `f` returns; nothing changes the file meanwhile (a program's
    // file is the system image's or a copy of its own).
    f(unsafe { &map.as_slice()[..file.len] })
}

/// Loads `program` (and `linker`, its dynamic linker, if it has one) and
/// lays out its stack with `args`, `env` and the auxiliary vector: the
/// program's areas from then on.
pub fn load(program: &File, linker: Option<&File>, args: &[&[u8]], env: &[&[u8]]) -> Result<Loaded, isize> {
    let (entry, phdr, phnum, end) = with_image(program, |image| {
        let elf = Elf::parse(image).map_err(|_| ENOEXEC)?;
        let base = PROGRAM_BASE + (random() % (1 << 28)) * PAGE;
        let bias = map_image(&elf, program, base)?;
        let (_, hi) = elf.span();
        Ok((elf.entry().wrapping_add(bias), elf.phdr_vaddr().wrapping_add(bias), elf.phnum(), hi.wrapping_add(bias)))
    })?;
    // The program break, a random distance after the image.
    let brk = end.next_multiple_of(PAGE) + (random() % (BREAK_SPAN / PAGE)) * PAGE;
    memory::set_break(brk);
    let (start, base) = match linker {
        Some(linker) => with_image(linker, |image| {
            let elf = Elf::parse(image).map_err(|_| ELIBBAD)?;
            let (lo, hi) = elf.span();
            let below = LIBRARY_TOP - (random() % (1 << 28)) * PAGE - (hi - lo).next_multiple_of(PAGE);
            let bias = map_image(&elf, linker, below)?;
            Ok((elf.entry().wrapping_add(bias), lo.wrapping_add(bias)))
        })?,
        None => (entry, 0),
    };
    let (sp, auxv) = stack(
        program,
        args,
        env,
        elf::AuxInfo {
            phdr,
            phnum: phnum as u64,
            entry,
            base,
            hwcap: crate::architecture::hwcap(),
            platform: crate::architecture::MACHINE.as_bytes(),
            uid: runtime::process::UID,
            gid: runtime::process::UID,
            random: [0; 16],
            execfn: program.path.as_bytes(),
        },
    )?;
    Ok(Loaded { entry: start, sp, auxv })
}

/// Maps `elf`'s image (the bytes of `file`): where it was linked, or near
/// `near` if it is position-independent (at the first place the kernel
/// finds free from there). Returns its load bias.
fn map_image(elf: &Elf, file: &File, near: u64) -> Result<u64, isize> {
    let (lo, hi) = elf.span();
    let len = (hi - lo) as usize;
    // The place: held whole while the image is put there, so that nothing
    // else is placed in it meanwhile.
    let hold = if elf.is_position_independent() {
        vm::allocate(None, len, near as usize, 0).map_err(|_| ENOMEM)?
    } else {
        vm::allocate(None, len, lo as usize, map_flags::FIXED).map_err(|_| ENOMEM)?
    };
    let base = hold as u64;
    let bias = base.wrapping_sub(lo);
    let _ = vm::unmap(None, hold, len);
    for run in elf::page_runs(elf, Relro::Writable) {
        if let Err(e) = map_run(elf, file, &run, bias) {
            // What was mapped of it goes with it.
            let _ = vm::unmap(None, base as usize, len);
            return Err(e);
        }
    }
    Ok(bias)
}

/// Maps one run of an image's pages: the file's own pages where it can,
/// else memory of the program's own, the segments' bytes copied in.
fn map_run(elf: &Elf, file: &File, run: &PageRun, bias: u64) -> Result<(), isize> {
    let at = run.start.wrapping_add(bias) as usize;
    let len = run.len as usize;
    let mut prot = 0;
    let mut flags = map_flags::FIXED;
    if run.perms.read {
        (prot, flags) = (prot | PROT_READ, flags | map_flags::READ);
    }
    if run.perms.write {
        (prot, flags) = (prot | PROT_WRITE, flags | map_flags::READ | map_flags::WRITE);
    }
    if run.perms.exec {
        (prot, flags) = (prot | PROT_EXEC, flags | map_flags::READ | map_flags::EXECUTE);
    }
    match elf.file_pages(run) {
        Some(offset) => {
            vm::map(None, &file.vmo, offset as usize, len, at, flags).map_err(|_| ENOMEM)?;
        }
        None => {
            let rw = map_flags::FIXED | map_flags::READ | map_flags::WRITE;
            vm::allocate(None, len, at, rw).map_err(|_| ENOMEM)?;
            let end = run.start + run.len;
            for s in elf.segments() {
                // The bytes of segment `s` in this run.
                let (from, to) = (s.vaddr.max(run.start), (s.vaddr + s.data.len() as u64).min(end));
                if from >= to {
                    continue;
                }
                let bytes = &s.data[(from - s.vaddr) as usize..(to - s.vaddr) as usize];
                // SAFETY: memory just allocated for the run, `[at, at+len)`.
                unsafe {
                    let dest = (from.wrapping_add(bias)) as *mut u8;
                    core::ptr::copy_nonoverlapping(bytes.as_ptr(), dest, bytes.len());
                }
            }
            if flags & !(map_flags::FIXED) != map_flags::READ | map_flags::WRITE {
                vm::protect(None, at, len, flags & !map_flags::FIXED).map_err(|_| ENOMEM)?;
            }
        }
    }
    memory::record(at as u64, len as u64, prot, Kind::Image(file.path.clone()));
    Ok(())
}

/// Makes the main thread's stack, near the top of the address space, its
/// top holding the arguments, environment and auxiliary vector: the stack
/// pointer, and the auxiliary vector's bytes.
fn stack(program: &File, args: &[&[u8]], env: &[&[u8]], mut aux: elf::AuxInfo) -> Result<(u64, Vec<u8>), isize> {
    let _ = program;
    let _ = runtime::object::try_random_bytes(&mut aux.random);
    let len = STACK_GUARD + STACK;
    let top_most = abi::USER_SPACE_END as u64 - (random() % (STACK_SPAN / PAGE)) * PAGE;
    let rw = map_flags::READ | map_flags::WRITE;
    let base = vm::allocate(None, len, (top_most - len as u64) as usize, rw).map_err(|_| ENOMEM)?;
    let _ = vm::protect(None, base, STACK_GUARD, 0);
    let top = (base + len) as u64;
    memory::record((base + STACK_GUARD) as u64, STACK as u64, PROT_READ | PROT_WRITE, Kind::Stack);
    // Like Linux, a quarter of the stack at most for the arguments.
    let need = elf::initial_stack_size(args, env, &aux);
    if need > STACK / 4 {
        return Err(E2BIG);
    }
    let mut image = alloc::vec![0u8; need];
    let sp = elf::initial_stack(top, args, env, &aux, &mut image).map_err(|_| E2BIG)?;
    // SAFETY: the top of the stack just made, `need` bytes.
    unsafe { core::ptr::copy_nonoverlapping(image.as_ptr(), (top - need as u64) as *mut u8, need) };
    // The auxiliary vector: after argc, the arguments and the environment
    // with their ends, pairs of words to its end (`AT_NULL`).
    let start = (sp + 8 * (1 + args.len() as u64 + 1 + env.len() as u64 + 1)) as usize;
    let mut auxv = Vec::new();
    let mut at = start;
    loop {
        // SAFETY: the stack just laid out.
        let (kind, value) = unsafe { (*(at as *const u64), *((at + 8) as *const u64)) };
        auxv.extend_from_slice(&kind.to_le_bytes());
        auxv.extend_from_slice(&value.to_le_bytes());
        if kind == 0 {
            break;
        }
        at += 16;
    }
    Ok((sp, auxv))
}

const E2BIG: isize = linux_raw_sys::errno::E2BIG as isize;

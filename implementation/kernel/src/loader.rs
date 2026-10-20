// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Starts the first user process, `init`, from the initrd.
//!
//! This is the only program the kernel loads itself. `init` receives the
//! root resource and handles to the boot modules; every other process is
//! created from user space. Like every Veda program, `init` is a
//! position-independent ELF64 executable: the kernel places it at a random
//! address and relocates it there, as `rt::process` does the others.

use alloc::sync::Arc;
use alloc::vec::Vec;

use abi::startup::{self, role};
use abi::{KernelBootInfo, Rights};
use bootinfo::{BootInfo, MemoryKind};

use crate::ipc::channel::{self, Message};
use crate::memory::aspace::Perms;
use crate::memory::paging::Cache;
use crate::memory::phys_to_virt;
use crate::memory::vmo::Vmo;
use crate::object::KObject;
use crate::object::handle::Handle;
use crate::object::process::Process;
use crate::object::resource::Resource;
use crate::scheduler::{self, Thread};

const INIT_PATH: &str = "bin/init";
const STACK_SIZE: u64 = 1 << 20;

pub fn spawn_init(boot: &BootInfo) {
    // SAFETY: the initrd lies in RAM covered by the direct map and is never
    // freed.
    let initrd =
        unsafe { core::slice::from_raw_parts(phys_to_virt(boot.initrd.base) as *const u8, boot.initrd.size as usize) };
    let archive = initrd::Archive::open(initrd).expect("the initrd is corrupt");
    crate::kinfo!("initrd: {} files, {} KiB", archive.len(), initrd.len() / 1024);
    let file = archive.find(INIT_PATH).expect("bin/init is missing from the initrd");
    let elf = elf::Elf::parse(file.data).expect("init is not a valid ELF64 executable");
    assert!(elf.kind() == elf::Kind::Veda, "init is not a Veda program of this version");

    // Every priority there is, for init to give the system's services, and
    // the system's memory class, which its services keep and the
    // applications it starts leave.
    let process = Process::new("init", 0, abi::priority::REALTIME as u8, abi::memory_class::SYSTEM as u8)
        .expect("out of memory creating init");
    let aspace = process.aspace().unwrap();

    // The image: its segments, relocated for a random place, in a VMO mapped
    // there run by run with their permissions (RELRO read-only).
    let (lo, hi) = elf.span();
    let base = if elf.is_position_independent() {
        let mut random = [0u8; 8];
        crate::random::fill(&mut random);
        abi::program_base((hi - lo) as usize, u64::from_le_bytes(random)) as u64
    } else {
        lo
    };
    let bias = base.wrapping_sub(lo);
    let mut image = alloc::vec![0u8; (hi - lo) as usize];
    for s in elf.segments() {
        let at = (s.vaddr - lo) as usize;
        image[at..at + s.data.len()].copy_from_slice(s.data);
    }
    elf.relocate(&mut image, bias).expect("init's relocations are invalid");
    let vmo = Vmo::new_anonymous(hi - lo, Cache::WriteBack).expect("init image too large");
    assert!(vmo.write(0, &image));
    drop(image);
    for run in elf::page_runs(&elf, elf::Relro::ReadOnly) {
        let perms = Perms { read: true, write: run.perms.write, exec: run.perms.exec };
        let at = run.start.wrapping_add(bias);
        aspace.map(vmo.clone(), run.start - lo, run.len, at, true, perms, Perms::ALL, false).expect("mapping init");
    }

    let stack = Vmo::new_anonymous(STACK_SIZE, Cache::WriteBack).unwrap();
    let rw = Perms { read: true, write: true, exec: false };
    let stack_base = aspace.map(stack, 0, STACK_SIZE, 0, false, rw, Perms::ALL, false).expect("mapping init's stack");

    // Boot modules handed to init.
    let fb = &boot.framebuffer;
    let mut cmdline = [0u8; 256];
    cmdline[..boot.cmdline_len as usize].copy_from_slice(&boot.cmdline[..boot.cmdline_len as usize]);
    // The firmware's ACPI memory, adjacent ranges merged.
    let mut acpi_memory = [[0u64; 2]; abi::ACPI_MEMORY_RANGES];
    let mut acpi_memory_count = 0;
    // SAFETY: the loader built a valid memory map.
    for r in unsafe { boot.memory_map.as_slice() } {
        if !matches!(r.kind, MemoryKind::AcpiReclaimable | MemoryKind::AcpiNvs) {
            continue;
        }
        if acpi_memory_count > 0 && acpi_memory[acpi_memory_count - 1][1] == r.base {
            acpi_memory[acpi_memory_count - 1][1] = r.end();
        } else if acpi_memory_count < acpi_memory.len() {
            acpi_memory[acpi_memory_count] = [r.base, r.end()];
            acpi_memory_count += 1;
        }
    }
    let info = KernelBootInfo {
        framebuffer_width: fb.width,
        framebuffer_height: fb.height,
        framebuffer_pitch: fb.stride * 4,
        framebuffer_format: fb.format as u32,
        framebuffer_size: fb.size,
        framebuffer_phys: fb.phys_base,
        initrd_size: boot.initrd.size,
        cmdline,
        cmdline_len: boot.cmdline_len,
        cpu_count: crate::percpu::online().count() as u32,
        acpi_rsdp: boot.rsdp_phys,
        acpi_memory,
        acpi_memory_count: acpi_memory_count as u32,
        platform: platform(),
    };
    let info_vmo = Vmo::new_anonymous(4096, Cache::WriteBack).unwrap();
    // SAFETY: viewing a plain #[repr(C)] struct as bytes.
    let info_bytes = unsafe {
        core::slice::from_raw_parts(&info as *const KernelBootInfo as *const u8, core::mem::size_of::<KernelBootInfo>())
    };
    assert!(info_vmo.write(0, info_bytes));

    let read_only = Rights(Rights::BASIC.0 | Rights::READ.0 | Rights::MAP.0 | Rights::GET_INFO.0);
    let read_write = Rights(read_only.0 | Rights::WRITE.0);
    let mut handles: Vec<Handle> = Vec::new();
    let mut roles: Vec<u32> = Vec::new();
    handles.push(Handle {
        object: KObject::Resource(Resource::root()),
        rights: Rights(Rights::BASIC.0 | Rights::DUPLICATE.0),
    });
    roles.push(role::ROOT_RESOURCE);
    // The system image's programs run from its pages (slices of it).
    handles.push(Handle {
        object: KObject::Vmo(Vmo::new_physical(boot.initrd.base, boot.initrd.size, Cache::WriteBack)),
        rights: Rights(read_only.0 | Rights::EXECUTE.0),
    });
    roles.push(role::INITRD);
    if fb.phys_base != 0 {
        handles.push(Handle {
            object: KObject::Vmo(Vmo::new_physical(fb.phys_base, fb.size, Cache::WriteCombining)),
            rights: read_write,
        });
        roles.push(role::FRAMEBUFFER);
    }
    handles.push(Handle { object: KObject::Vmo(info_vmo), rights: read_only });
    roles.push(role::BOOT_INFO);
    let variables = &boot.firmware_variables;
    if !variables.is_empty() {
        handles.push(Handle {
            object: KObject::Vmo(Vmo::new_physical(variables.base, variables.size, Cache::WriteBack)),
            rights: read_only,
        });
        roles.push(role::FIRMWARE_VARIABLES);
    }

    let mut data = Vec::new();
    startup::Startup { args: &["init"], roles: &roles, ..Default::default() }
        .encode(&mut |b| data.extend_from_slice(b));
    let (kernel_end, init_end) = channel::create();
    if kernel_end.write(Message { data, handles }).is_err() {
        panic!("could not send init its startup message");
    }
    drop(kernel_end);
    let channel_rights = Rights(Rights::TRANSFER.0 | Rights::READ.0 | Rights::WRITE.0 | Rights::WAIT.0);
    let bootstrap = process
        .handles
        .lock()
        .insert(Handle { object: KObject::Channel(init_end), rights: channel_rights })
        .expect("inserting init's bootstrap handle");

    let thread = Thread::new_user(&process, "main").expect("creating init's thread");
    process.add_thread(thread.clone()).expect("init's process takes its first thread");
    let sp = abi::entry_stack_pointer((stack_base + STACK_SIZE) as usize) as u64;
    let entry = elf.entry().wrapping_add(bias);
    thread.prepare_user_entry(entry, sp, bootstrap as u64, 0);
    process.mark_started();
    scheduler::make_ready(&thread);
    // Every process may read the log: where init is stays out of it (its
    // entry is given as the offset in its image, as crashes are).
    crate::kinfo!("started init (process {}), entry {:#x} in its image", process.koid, elf.entry());
    keep_alive(process);
}

/// The kernel keeps a reference to init so it can report if init dies.
static INIT: crate::synchronization::SpinLock<Option<Arc<Process>>> = crate::synchronization::SpinLock::new(None);

fn keep_alive(p: Arc<Process>) {
    *INIT.lock() = Some(p);
}

/// What the machine offers beyond what Veda needs (`abi::platform`).
fn platform() -> u32 {
    let mut bits = 0;
    if crate::arch::hv::available().is_ok() {
        bits |= abi::platform::VIRTUALIZATION;
    }
    if crate::arch::iommu::active() {
        bits |= abi::platform::IOMMU;
    }
    bits
}

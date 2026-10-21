// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `drivervm` — the driver VM.
//!
//! Runs Linux in a virtual machine, for the drivers Veda takes from it
//! (see `documentation/DRIVERVM.md`). The machine is Veda's paravirtual
//! platform (`hv::platform`): processors, memory, and hypercalls; no
//! emulated hardware. This program is its monitor: it lays out the guest's
//! memory, loads the kernel and its initial RAM file system from the system
//! image (`linux/kernel`, `linux/initramfs.cpio`), runs the virtual
//! processors, one thread each, and serves the hypercalls. What is
//! particular to the architecture (booting Linux, the PC's own devices,
//! the guest's memory map below 4 GiB) is in `architecture`
//! (`implementation/services/drivervm/src/architectures/<architecture>/`).
//!
//! Programs in the guest reach Veda's objects through the bridge
//! (`hv::bridge`): this program keeps the guest's handles and carries out
//! its operations on them, within bounds of what the guest makes Veda keep
//! (handles, waits, memory, mappings), and gives it a registry narrowed to
//! the services drivers attach to.
//!
//! What the monitor decides that reaches no object of Veda's (the bridge's
//! books, where the guest's functions and their BARs go, the keyboard
//! controller's commands) is its library (`drivervm`, `src/lib.rs`), tested
//! on the host.
//!
//! It gives the guest the PCI functions devmgr hands over (`pci`): their
//! DMA into the guest's memory only, their BARs, their interrupts (MSIs,
//! and the lines their INTx are wired to: `lines`); the PC's own devices,
//! if devmgr hands them over (`architecture::platform`: the keyboard
//! controller); ACPI tables that describe them (`hv::acpi`); and the PC's
//! firmware variables, read-only (`variables`).
//!
//! The guest's life is this process's: when Linux powers the machine off,
//! restarts it, crashes or does something the platform does not allow,
//! the process ends, and whoever started it decides what comes next.
//!
//! Arguments: `memory=MIB` (default 256), `cpus=N` (default 2),
//! `cmdline=...` (added to the kernel's command line, commas for spaces),
//! and for tests `run=PROGRAM,...` (programs of the guest's `/bin` its
//! `init` runs), `poweroff` (once they are done) and `crash=SECONDS`
//! (Linux crashes that long after it starts: devmgr passes it to the first
//! run only), `renderer=softpipe` (the guest's renderer renders on
//! softpipe, whatever the devices).

#![no_std]
#![no_main]

extern crate alloc;

#[cfg(target_arch = "x86_64")]
#[path = "architectures/amd64/mod.rs"]
mod architecture;
mod bridge;
mod gpio;
mod lines;
mod machine;
mod memory;
mod pci;
mod registry;
mod variables;

use alloc::string::String;
use alloc::vec::Vec;

use proto::startup::drivervm as roles;
use rt::object::{Channel, Resource, Vmo};
use rt::println;

use lines::Lines;
use machine::{Config, Machine, Pc};
use variables::FirmwareVariables;

rt::entry!(main);

/// Maps the system image (the initrd), where the guest's kernel is.
fn system_image() -> Option<initrd::Archive<'static>> {
    let vmo = Vmo::from_handle(rt::env::take_handle(abi::startup::role::INITRD)?);
    let size = vmo.size().ok()?;
    let addr = vmo.map(0, size, abi::map_flags::READ).ok()?;
    // SAFETY: a read-only mapping that lives as long as the process.
    let bytes: &'static [u8] = unsafe { core::slice::from_raw_parts(addr as *const u8, size) };
    initrd::Archive::open(bytes).ok()
}

fn config() -> Config {
    let mut c = Config::default();
    for arg in rt::env::args() {
        match arg.split_once('=') {
            Some(("memory", v)) => c.memory_mib = v.parse().unwrap_or(c.memory_mib),
            Some(("cpus", v)) => c.cpus = v.parse().unwrap_or(c.cpus),
            Some(("cmdline", v)) => c.cmdline.push_str(&alloc::format!(" {}", v.replace(',', " "))),
            Some(("run", v)) => c.cmdline.push_str(&alloc::format!(" veda.run={v}")),
            Some(("crash", v)) => c.cmdline.push_str(&alloc::format!(" veda.crash={v}")),
            Some(("renderer", v)) => c.cmdline.push_str(&alloc::format!(" veda.renderer={v}")),
            None if arg == "poweroff" => c.cmdline.push_str(" veda.poweroff"),
            _ => {}
        }
    }
    c
}

fn main() -> i32 {
    let Some(hypervisor) = rt::env::take_handle(roles::HYPERVISOR_RESOURCE).map(Resource::from_handle) else {
        println!("no hypervisor resource");
        return 1;
    };
    let Some(image) = system_image() else {
        println!("no system image");
        return 1;
    };
    let (Some(kernel), Some(initramfs)) = (image.find("linux/kernel"), image.find("linux/initramfs.cpio")) else {
        println!("the system image has no Linux guest");
        return 1;
    };
    let config = config();
    let devices: Vec<Channel> =
        core::iter::from_fn(|| rt::env::take_handle(roles::DEVICE)).map(Channel::from_handle).collect();
    let mut lines = Lines::default();
    let platform = architecture::platform::Platform::take(&mut lines);
    let variables =
        rt::env::take_handle(roles::VARIABLES).map(Vmo::from_handle).map(FirmwareVariables::map).unwrap_or_default();
    let pc = Pc { devices, platform, lines, variables };
    match Machine::start(&hypervisor, kernel.data, initramfs.data, pc, &config) {
        Ok(machine) => machine.wait(),
        Err(e) => {
            let e: String = e;
            println!("{}", e);
            1
        }
    }
}

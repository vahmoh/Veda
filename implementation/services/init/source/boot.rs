// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The system start-up sequence: which services run, with which
//! capabilities, and which of them are restarted if they fail.

use alloc::vec::Vec;

use abi::resource_kind;
use abi::startup::role;
use proto::startup::devmgr as roles;
use rt::object::Handle;
use rt::println;

use crate::{Init, Origin};

/// System services in start order. File systems come first (everything
/// else loads data through them), then the network and Wi-Fi services
/// (drivers attach to them as they start), device management and drivers,
/// the window system and the lock screen (which unlocks it), audio, the
/// voice agent and the desktop shell.
const SERVICES: [&str; 9] = ["vfs", "netd", "wlan", "devmgr", "compositor", "lock", "audio", "agent", "shell"];

/// Services that are started again if they exit. They hold no state other
/// processes cannot recover: drivers reconnect to a restarted compositor,
/// network or Wi-Fi service, the shell rebuilds its windows, and the lock
/// screen shows itself again on the screen the window system kept locked.
/// (The file system and the device manager are not restarted: one holds
/// the user's files, the other owns the running drivers.)
pub const RESTARTABLE: [&str; 7] = ["compositor", "lock", "shell", "audio", "netd", "wlan", "agent"];

fn dup(h: &Option<rt::Vmo>) -> Option<Handle> {
    h.as_ref().and_then(|v| v.0.duplicate(None).ok())
}

fn init_resource(init: &Init, kind: usize, base: u64, size: u64) -> Option<Handle> {
    init.root.create(kind, base, size).ok().map(|r| r.into_handle())
}

/// The capabilities a system service is started with.
fn handles_for(init: &Init, name: &str) -> Vec<(u32, Handle)> {
    let mut out = Vec::new();
    match name {
        "vfs" => out.extend(init.initrd_vmo.0.duplicate(None).ok().map(|h| (role::INITRD, h))),
        // Hardware resources to delegate to drivers (but not the root
        // resource), and the system image to load the drivers from.
        "devmgr" => {
            out.extend(init.initrd_vmo.0.duplicate(None).ok().map(|h| (role::INITRD, h)));
            // Where the ACPI tables are, for the devices they describe; the
            // firmware's variables, for the driver VM.
            out.extend(dup(&init.boot_info).map(|h| (role::BOOT_INFO, h)));
            out.extend(dup(&init.firmware_variables).map(|h| (role::FIRMWARE_VARIABLES, h)));
            // The machine's ports, lines and physical addresses are the
            // architecture's (`architecture`); PCI functions are named by
            // requester ids.
            for (r, kind, range) in [
                (roles::IOPORT_RESOURCE, resource_kind::IOPORT, crate::architecture::IO_PORTS),
                (roles::IRQ_RESOURCE, resource_kind::IRQ, crate::architecture::IRQS),
                (roles::MMIO_RESOURCE, resource_kind::MMIO, crate::architecture::MMIO),
                (roles::DMA_RESOURCE, resource_kind::DMA, 0..0),
                (roles::HYPERVISOR_RESOURCE, resource_kind::HYPERVISOR, 0..0),
                (roles::PCI_RESOURCE, resource_kind::PCI, 0..0x1_0000),
            ] {
                out.extend(init_resource(init, kind, range.start, range.end - range.start).map(|h| (r, h)));
            }
        }
        "compositor" => {
            out.extend(dup(&init.framebuffer).map(|h| (role::FRAMEBUFFER, h)));
            out.extend(dup(&init.boot_info).map(|h| (role::BOOT_INFO, h)));
        }
        _ => {}
    }
    out
}

/// Starts all system services.
pub fn start_system(init: &mut Init) {
    for name in SERVICES {
        start_service(init, name, true);
    }
}

/// Starts one system service, with the system (`at_boot`) or again after
/// it exited.
pub fn start_service(init: &mut Init, name: &str, at_boot: bool) {
    let path = alloc::format!("bin/{name}");
    if init.initrd.find(&path).is_none() {
        println!("{} is not installed; skipping", name);
        return;
    }
    let handles = handles_for(init, name);
    // The agent takes its test options (`agent.endpoint=...`) from the
    // kernel command line. A live system (`live`, booted from a USB stick)
    // tells the file system and the device manager to leave the computer's
    // disks alone; tests ask the device manager for the driver VM
    // (`drivervm`, with its options as `drivervm.NAME=VALUE`) and give it
    // options of its own (`devmgr.NAME=VALUE`). At system start the window
    // system plays the startup sequence (`splash`) and the shell the
    // startup sound (`startup`, unless `startup-sound=off`); started again,
    // they do neither. With the lock screen installed, the window system
    // starts locked (`locked`), at system start and after a crash alike:
    // the lock service unlocks it, at once if no password is set.
    let cmdline = cmdline(init);
    let has = |option: &str| cmdline.split_whitespace().any(|a| a == option);
    let args: Vec<alloc::string::String> = match name {
        "agent" => cmdline.split_whitespace().filter(|a| a.starts_with("agent.")).map(Into::into).collect(),
        "devmgr" => cmdline
            .split_whitespace()
            .filter(|a| *a == "live" || *a == "drivervm" || a.starts_with("drivervm.") || a.starts_with("devmgr."))
            .map(Into::into)
            .collect(),
        "vfs" if has("live") => alloc::vec!["live".into()],
        "compositor" => {
            let mut args = Vec::new();
            if at_boot {
                args.push("splash".into());
            }
            if init.initrd.find("bin/lock").is_some() {
                args.push("locked".into());
            }
            args
        }
        "shell" if at_boot && !has("startup-sound=off") => alloc::vec!["startup".into()],
        _ => Vec::new(),
    };
    if let Err(e) = init.spawn(name, &path, &args, handles, Origin::Service) {
        println!("could not start {}: {:?}", name, e);
    }
}

/// The kernel command line (from the boot information VMO).
pub fn cmdline(init: &Init) -> alloc::string::String {
    let Some(vmo) = &init.boot_info else { return alloc::string::String::new() };
    let mut buf = [0u8; core::mem::size_of::<abi::KernelBootInfo>()];
    if vmo.read(0, &mut buf).is_err() {
        return alloc::string::String::new();
    }
    // SAFETY: KernelBootInfo is plain old data written by the kernel.
    let info: abi::KernelBootInfo = unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const abi::KernelBootInfo) };
    let len = (info.cmdline_len as usize).min(info.cmdline.len());
    alloc::string::String::from_utf8_lossy(&info.cmdline[..len]).into_owned()
}

/// Starts optional programs requested on the kernel command line:
/// `run=NAME` or `run=NAME:ARG1,ARG2,...`.
pub fn start_requested(init: &mut Init) {
    let cmdline = cmdline(init);
    for arg in cmdline.split_whitespace() {
        if let Some(spec) = arg.strip_prefix("run=") {
            let (name, args): (&str, Vec<alloc::string::String>) = match spec.split_once(':') {
                Some((name, args)) => (name, args.split(',').filter(|a| !a.is_empty()).map(Into::into).collect()),
                None => (spec, Vec::new()),
            };
            let path = alloc::format!("bin/{name}");
            match init.spawn(name, &path, &args, Vec::new(), Origin::Requested) {
                Ok(_) => {}
                Err(e) => println!("cannot start {}: {:?}", name, e),
            }
        }
    }
    if cmdline.split_whitespace().any(|a| a == "systest") {
        match init.spawn("systest", "bin/systest", &[], Vec::new(), Origin::Requested) {
            Ok(_) => {}
            Err(e) => println!("systest: FAIL (cannot start: {:?})", e),
        }
    }
}

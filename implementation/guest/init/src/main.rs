// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `init` — the first program of Linux in Veda's driver VM.
//!
//! It mounts the kernel's file systems, says on the console (which is
//! Veda's log) how Linux came up, starts `logkeeper` first where there is
//! a USB controller (the stick the live system started from keeps the
//! system's log), and Veda's drivers for Linux that the guest's devices
//! need (`input` for whatever input devices come,
//! `alsa` for sound cards, `net` for Ethernet cards, `wifi` for Wi-Fi radios
//! and `airlink` for QEMU's virtual one, `kms` for displays and `renderer`
//! for their GPUs), again if they end. With `veda.renderer=softpipe` the
//! renderer renders on softpipe, for tests, whatever the devices.
//! Then it runs the programs the kernel's command line names
//! (`veda.run=bridgetest,...`, from `/bin`, one after the other) and stays:
//! the first program may never end. With `veda.poweroff` it powers the
//! machine off once they are done instead, for the tests of the driver VM;
//! with `veda.crash=SECONDS` Linux crashes (a panic) that long after it
//! started, for the tests of its restart, and with `veda.kill=NAME:SECONDS`
//! driver `NAME` is ended that long after (by SIGKILL, as Linux's OOM
//! killer ends one), for the tests of a driver's.

use std::process::Command;
use std::time::Duration;

use rustix::mount::{MountFlags, mount};
use rustix::process::{Pid, Signal, kill_process};
use rustix::system::{RebootCommand, reboot};

/// Veda's drivers for Linux, and the PCI classes (their first bytes, as
/// sysfs gives them) of the functions each serves: multimedia devices,
/// Ethernet cards, other network cards (Wi-Fi's), the communication
/// controllers whose ports QEMU's virtual radio is on, displays with their
/// GPUs, and USB controllers, whose devices may be network or Wi-Fi
/// adapters. (`input` takes input devices on whatever they are.)
const DRIVERS: &[(&str, &[&str])] = &[
    ("alsa", &["0x04"]),
    ("net", &["0x0200", "0x0c03"]),
    ("wifi", &["0x0280", "0x0780", "0x0c03"]),
    ("airlink", &["0x0780"]),
    ("kms", &["0x03"]),
    ("renderer", &["0x03"]),
];

/// Whether the guest has a PCI function of one of `classes`.
fn has_class(classes: &[&str]) -> bool {
    std::fs::read_dir("/sys/bus/pci/devices")
        .map(|d| {
            d.flatten().any(|f| {
                std::fs::read_to_string(f.path().join("class")).is_ok_and(|c| classes.iter().any(|k| c.starts_with(k)))
            })
        })
        .unwrap_or(false)
}

/// Runs driver `name` (from `/bin`, with `args`) on a thread of its own,
/// and again a second after it ends.
fn keep_running(name: &'static str, args: &'static [&'static str]) {
    std::thread::spawn(move || {
        loop {
            match Command::new(format!("/bin/{name}")).args(args).status() {
                Ok(status) => println!("veda: {name} ended ({status}); starting it again"),
                Err(e) => {
                    println!("veda: cannot run {name}: {e}");
                    return;
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

/// Ends the processes called `name`, by SIGKILL.
fn kill_named(name: &str) {
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry.file_name().to_string_lossy().parse().ok().and_then(Pid::from_raw) else { continue };
        if std::fs::read_to_string(entry.path().join("comm")).is_ok_and(|c| c.trim() == name) {
            let _ = kill_process(pid, Signal::KILL);
        }
    }
}

fn main() {
    for (source, target, kind) in
        [("proc", "/proc", "proc"), ("sysfs", "/sys", "sysfs"), ("devtmpfs", "/dev", "devtmpfs")]
    {
        let _ = std::fs::create_dir_all(target);
        if let Err(e) = mount(source, target, kind, MountFlags::empty(), None) {
            println!("veda: cannot mount {target}: {e}");
        }
    }
    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let cpus = std::fs::read_to_string("/sys/devices/system/cpu/online").unwrap_or_default();
    let memory = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| {
            m.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .map(|l| l.split_whitespace().nth(1).unwrap_or("?").to_string())
        })
        .unwrap_or_default();
    println!("veda: Linux {} is up, processors {}, {} KiB of memory", release.trim(), cpus.trim(), memory);
    let options: Vec<&str> = cmdline.split_whitespace().collect();
    if has_class(&["0x0c03"]) {
        keep_running("logkeeper", &[]);
    }
    keep_running("input", &[]);
    let softpipe = options.contains(&"veda.renderer=softpipe");
    if softpipe {
        keep_running("renderer", &["softpipe"]);
    }
    for &(name, classes) in DRIVERS {
        if has_class(classes) && !(softpipe && name == "renderer") {
            keep_running(name, &[]);
        }
    }
    if let Some(seconds) = options.iter().find_map(|o| o.strip_prefix("veda.crash=")?.parse().ok()) {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(seconds));
            println!("veda: crashing, as asked");
            let _ = std::fs::write("/proc/sysrq-trigger", "c");
        });
    }
    let kill = options.iter().find_map(|o| {
        let (name, seconds) = o.strip_prefix("veda.kill=")?.split_once(':')?;
        Some((name.to_string(), seconds.parse::<u64>().ok()?))
    });
    if let Some((name, seconds)) = kill {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(seconds));
            println!("veda: ending {name}, as asked");
            kill_named(&name);
        });
    }
    for name in options.iter().filter_map(|o| o.strip_prefix("veda.run=")).flat_map(|l| l.split(',')) {
        match Command::new(format!("/bin/{name}")).status() {
            Ok(status) => println!("veda: {name} ended ({status})"),
            Err(e) => println!("veda: cannot run {name}: {e}"),
        }
    }
    if options.contains(&"veda.poweroff") {
        println!("veda: powering off, as asked");
        if let Err(e) = reboot(RebootCommand::PowerOff) {
            println!("veda: cannot power off: {e}");
        }
    }
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

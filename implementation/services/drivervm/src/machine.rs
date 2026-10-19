// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The virtual machine: its memory, its processors, its devices and its
//! hypercalls.
//!
//! Guest-physical memory is one VMO, mapped at 0 in the guest and here,
//! which Linux is loaded into as the architecture boots it (`arch::boot`,
//! which also says what of it stays reserved).
//!
//! The rest of the guest-physical address space:
//!
//! | Where | What |
//! |-------|------|
//! | below 4 GiB, where the architecture's machine has room (`arch::PCI_LOW`) | BARs of the guest's PCI functions |
//! | 64 GiB to 128 GiB | the bridge's window: VMOs guest programs map |
//! | 128 GiB to 256 GiB | BARs that do not fit below 4 GiB |
//!
//! Memory the firmware keeps for a function (a GPU's stolen memory) is
//! where the PC has it, in what is free of that: from the end of RAM to
//! the BARs' window below 4 GiB, from 4 GiB to 64 GiB, from 256 GiB; the
//! memory map says it is reserved.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use abi::{VcpuExit, map_flags, vcpu_exit};
use hv::acpi;
use hv::platform::{self, error, hypercall, power};
use rt::object::{Channel, Guest, Resource, Vcpu, Vmo};
use rt::println;
use rt::sync::Mutex;
use rt::vm::Mapping;

use crate::arch::{self, PCI_LOW, platform::Platform};
use crate::bridge::{self, Bridge};
use crate::lines::Lines;
use crate::memory::GuestMemory;
use crate::pci::{Devices, Windows};
use crate::variables::FirmwareVariables;

/// The priority of the threads that run the guest's processors and wake it
/// for Veda's events: above the applications' (`NORMAL`), so that the
/// guest's timers and its devices' interrupts are acted on as they come,
/// not once a busy thread's time slice ends. Veda's scheduler gives a
/// process's threads above `NORMAL` half a processor's time at most, so a
/// guest that keeps its processors busy cannot starve the rest.
const PROCESSOR_PRIORITY: usize = abi::priority::HIGH;

/// How the machine is made.
pub struct Config {
    pub memory_mib: u32,
    pub cpus: u32,
    /// Added to the kernel's command line (each option after a space).
    pub cmdline: String,
}

impl Default for Config {
    fn default() -> Config {
        Config { memory_mib: 256, cpus: 2, cmdline: String::new() }
    }
}

const MIB: u64 = 1 << 20;
/// The kernel's console is the platform's, from its first message on. Its
/// display drivers say in detail what they do (DRM's driver, mode-setting
/// and PRIME debugging messages), which only its log has (`logkeeper`
/// keeps it on the live system's stick), with room for all they say while
/// Linux starts.
const CMDLINE: &str = "earlycon=veda console=hvc0 panic=-1 rdinit=/init log_buf_len=4M drm.debug=0xe";
/// Where PCI functions' BARs go that do not fit below 4 GiB
/// (`arch::PCI_LOW`): above the bridge's window.
const PCI_HIGH: core::ops::Range<u64> = (128 << 30)..(256 << 30);

/// How the process ends, for the device manager, which started it.
pub use proto::startup::drivervm::exit_code;

/// What the guest has of the PC.
pub struct Pc {
    /// The `pcidev` channels of its PCI functions.
    pub devices: Vec<Channel>,
    /// The architecture's devices of the PC (a keyboard controller).
    pub platform: Platform,
    /// Its interrupt lines (to which the functions' are added).
    pub lines: Lines,
    /// The firmware's variables.
    pub variables: FirmwareVariables,
}

pub struct Machine {
    guest: Guest,
    memory: GuestMemory,
    bridge: Bridge,
    devices: Devices,
    /// The architecture's devices of the PC, and what the processors do
    /// that is not a hypercall.
    platform: Platform,
    /// The interrupt lines the guest has.
    lines: Lines,
    /// The PC's firmware variables.
    variables: FirmwareVariables,
    /// The guest's first processor, which the bridge's interrupts go to.
    notify: Vcpu,
    /// The processors that have started (by their number), for the
    /// devices' interrupts.
    vcpus: Mutex<Vec<Option<Vcpu>>>,
    /// The console line each processor is writing (the kernel's messages
    /// and a program's output, written at once on two processors, stay
    /// apart).
    console: Mutex<Vec<Vec<u8>>>,
}

impl Machine {
    /// Makes the machine, gives it what it has of the PC, describes that
    /// in ACPI tables, loads Linux and starts its first processor.
    pub fn start(
        hypervisor: &Resource,
        kernel: &[u8],
        initramfs: &[u8],
        pc: Pc,
        config: &Config,
    ) -> Result<Arc<Machine>, String> {
        let Pc { devices, platform, mut lines, variables } = pc;
        let size = config.memory_mib as u64 * MIB;
        if size > PCI_LOW.start {
            return Err(format!("the guest can have at most {} MiB", PCI_LOW.start / MIB));
        }
        let image = arch::boot::parse(kernel)?;
        let vmo = Vmo::create_committed(size as usize).map_err(|e| format!("no memory for the guest: {e}"))?;
        let memory = Mapping::new(vmo, size as usize, map_flags::READ | map_flags::WRITE)
            .map_err(|e| format!("cannot map the guest's memory: {e}"))?;
        let memory = GuestMemory::new(memory);
        let guest = Guest::create(hypervisor, config.cpus).map_err(|e| format!("no virtual machine: {e}"))?;
        guest
            .map(memory.vmo(), 0, size as usize, 0, map_flags::READ | map_flags::WRITE | map_flags::EXECUTE)
            .map_err(|e| format!("cannot give the guest its memory: {e}"))?;
        let devices = if devices.is_empty() {
            Devices::none()
        } else {
            let free = alloc::vec![size..PCI_LOW.start, (4 << 30)..bridge::WINDOW_BASE, PCI_HIGH.end..u64::MAX];
            Devices::attach(&guest, devices, &mut Windows { low: PCI_LOW, high: PCI_HIGH, free }, &mut lines)
        };
        let functions = devices.functions();
        let gpio = devices.gpio_controllers();
        let pc = platform.acpi();
        let description = acpi::Machine {
            functions: &functions,
            gpio: &gpio,
            platform: &pc,
            at: arch::ACPI_AT,
            pci_low: PCI_LOW,
            pci_high: PCI_HIGH,
            firmware_tables: devices.firmware_tables(),
        };
        let acpi::Description { tables: acpi_tables, left_out } = acpi::describe(&description);
        for what in &left_out {
            println!("the guest's ACPI tables leave out {}", what);
        }
        if acpi_tables.len() > arch::ACPI_ROOM {
            return Err(format!("the ACPI tables take {} KiB, more than they have", acpi_tables.len() / 1024));
        }

        let mut cmdline = String::from(CMDLINE);
        cmdline.push_str(&devices.host_options());
        cmdline.push_str(config.cmdline.trim_end());
        let state = arch::boot::load(&arch::boot::Load {
            memory: &memory,
            size,
            reserved: devices.reserved(),
            kernel: &image,
            initramfs,
            cmdline: &cmdline,
            acpi_tables: &acpi_tables,
            opregion: devices.opregion(),
        })?;
        let vcpu = guest.create_vcpu(0, &state).map_err(|e| format!("no virtual processor: {e}"))?;
        let duplicate = |v: &Vcpu| v.0.duplicate(None).map(Vcpu::from_handle).map_err(|e| format!("{e}"));
        let notify = duplicate(&vcpu)?;
        let mut vcpus: Vec<Option<Vcpu>> = (0..config.cpus).map(|_| None).collect();
        vcpus[0] = Some(duplicate(&vcpu)?);
        let machine = Arc::new(Machine {
            guest,
            memory,
            bridge: Bridge::new().map_err(|e| format!("no bridge: {e}"))?,
            devices,
            platform,
            lines,
            variables,
            notify,
            vcpus: Mutex::new(vcpus),
            console: Mutex::new((0..config.cpus).map(|_| Vec::new()).collect()),
        });
        // The bridge's waits and the guest's registry, on threads of their
        // own.
        let m = machine.clone();
        rt::thread::Builder::new()
            .name("bridge-waits")
            .priority(PROCESSOR_PRIORITY)
            .spawn(move || m.bridge.run_waits(&m.memory, &m.notify))
            .map_err(|e| format!("no thread for the bridge: {e}"))?;
        let m = machine.clone();
        rt::thread::Builder::new()
            .name("registry")
            .spawn(move || m.bridge.registry().run())
            .map_err(|e| format!("no thread for the registry: {e}"))?;
        println!(
            "starting Linux ({} KiB, initramfs {} KiB) with {} MiB and {} processors",
            kernel.len() / 1024,
            initramfs.len() / 1024,
            config.memory_mib,
            config.cpus
        );
        machine.spawn(0, vcpu)?;
        Ok(machine)
    }

    /// Runs virtual processor `id` on a thread of its own.
    fn spawn(self: &Arc<Self>, id: u32, vcpu: Vcpu) -> Result<(), String> {
        let machine = self.clone();
        rt::thread::Builder::new()
            .name("vcpu")
            .priority(PROCESSOR_PRIORITY)
            .spawn(move || machine.run(id, vcpu))
            .map(|_| ())
            .map_err(|e| format!("no thread for processor {id}: {e}"))
    }

    /// The machine runs until a processor ends it (and this process).
    pub fn wait(&self) -> ! {
        loop {
            rt::time::sleep(rt::time::Duration::from_secs(3600));
        }
    }

    fn run(self: Arc<Self>, id: u32, vcpu: Vcpu) {
        let mut exit = VcpuExit::default();
        loop {
            if let Err(e) = vcpu.run(&mut exit) {
                self.stop(&format!("processor {id} cannot run: {e}"), exit_code::FAULT);
            }
            match exit.reason {
                vcpu_exit::HYPERCALL => exit.data[0] = self.hypercall(id, &exit.data),
                _ => {
                    if let Err((why, code)) = self.platform.exit(id, &vcpu, &mut exit) {
                        self.stop(&why, code)
                    }
                }
            }
        }
    }

    /// Serves processor `id`'s hypercall: the call in `data[0]`, its
    /// arguments after.
    fn hypercall(self: &Arc<Self>, id: u32, data: &[u64; 7]) -> u64 {
        match data[0] {
            hypercall::CONSOLE_WRITE => {
                self.console_write(id, data[1], [data[2], data[3], data[4], data[5]]);
                0
            }
            hypercall::START_CPU => self.start_cpu(data[1], data[2]),
            hypercall::POWER => match data[1] {
                power::OFF => self.stop("Linux powered the machine off", exit_code::POWER_OFF),
                power::RESTART => self.stop("Linux restarts the machine", exit_code::RESTART),
                power::CRASHED => self.stop("Linux crashed", exit_code::CRASHED),
                _ => error::INVALID,
            },
            hypercall::WALLCLOCK => rt::time::utc_time_ns(),
            hv::bridge::HYPERCALL => self.bridge.call(&self.memory, &self.guest, data[1] as u32, data[2]),
            hypercall::PCI_CONFIG_READ => self.devices.config_read(data[1], data[2], data[3]),
            hypercall::PCI_CONFIG_WRITE => self.devices.config_write(data[1], data[2], data[3], data[4]),
            // (devmgr makes the MSI: not with the processors' lock held.)
            hypercall::PCI_MSI => match self.vcpu(data[3]) {
                Some(vcpu) => self.devices.msi(data[1], data[2], &vcpu, data[4]),
                None => error::INVALID,
            },
            hypercall::GSI => match self.vcpus.lock().get(data[2] as usize) {
                Some(Some(vcpu)) if self.lines.route(data[1], vcpu, data[3]) => 0,
                _ => error::INVALID,
            },
            hypercall::GPIO => self.devices.gpio(data[1], data[2], data[3], data[4]),
            hypercall::FIRMWARE_VARIABLE => self.variables.call(&self.memory, data[1], data[2]),
            _ => error::UNKNOWN,
        }
    }

    /// Processor `id`, if it has started: a handle of its own.
    fn vcpu(&self, id: u64) -> Option<Vcpu> {
        let vcpus = self.vcpus.lock();
        let vcpu = vcpus.get(usize::try_from(id).ok()?)?.as_ref()?;
        vcpu.0.duplicate(None).ok().map(Vcpu::from_handle)
    }

    /// Starts processor `id` at `entry`, as the architecture starts them.
    fn start_cpu(self: &Arc<Self>, id: u64, entry: u64) -> u64 {
        let mut vcpus = self.vcpus.lock();
        let Some(slot) = vcpus.get_mut(id as usize).filter(|s| s.is_none()) else { return error::INVALID };
        let state = arch::boot::secondary(entry);
        let made = self.guest.create_vcpu(id as u32, &state).and_then(|v| Ok((v.0.duplicate(None)?, v)));
        let vcpu = match made {
            Ok((copy, v)) => {
                *slot = Some(Vcpu::from_handle(copy));
                v
            }
            Err(e) => {
                println!("no virtual processor {}: {}", id, e);
                return error::INVALID;
            }
        };
        drop(vcpus);
        match self.spawn(id as u32, vcpu) {
            Ok(()) => 0,
            Err(e) => {
                println!("{}", e);
                error::INVALID
            }
        }
    }

    /// Collects processor `id`'s console output into lines, for the log.
    fn console_write(&self, id: u32, count: u64, words: [u64; 4]) {
        let (bytes, n) = platform::console_bytes(count, words);
        let mut lines = self.console.lock();
        let Some(line) = lines.get_mut(id as usize) else { return };
        for &b in &bytes[..n] {
            match b {
                b'\n' => {
                    println!("linux: {}", String::from_utf8_lossy(line));
                    line.clear();
                }
                b'\r' => {}
                _ if line.len() < 1024 => line.push(b),
                _ => {}
            }
        }
    }

    /// Ends the machine (and this process).
    fn stop(&self, why: &str, code: i32) -> ! {
        for line in self.console.lock().iter().filter(|l| !l.is_empty()) {
            println!("linux: {}", String::from_utf8_lossy(line));
        }
        println!("{}", why);
        rt::sys::process_exit(code as i64)
    }
}

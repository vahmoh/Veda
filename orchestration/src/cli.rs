// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The command line of `cargo orchestration`: its help, and the options the
//! commands that build and boot Veda share.

use std::path::PathBuf;

use crate::components::Profile;
use crate::qemu;
use crate::util::{self, Result};

pub const HELP: &str = "\
Veda developer tool

USAGE:
    cargo orchestration <COMMAND> [OPTIONS]

COMMANDS:
    build       Build every component and assemble target/veda/veda.img
    iso         Build every component and write target/veda/veda.iso: a live system
                that boots a PC from a USB stick (write it with Rufus, or as it is
                with any image writer); it runs from memory and leaves the PC's
                disks alone
    run         Build, then boot Veda in QEMU
    shot        Boot headless, wait, and save a screenshot (dev aid)
    script FILE Boot headless and run an automation script (its language: see
                orchestration/src/script.rs)
    scripts FILE...
                Build once, then run the scripts, several at once (--jobs N);
                each one's log goes to target/veda/test-logs
    test        Run host unit tests, then the in-system integration tests
                (--ui also runs the GUI automation scripts in verification/ui
                and the agent's in verification/agent, several at once: --jobs N)
    clippy      Check every crate with Clippy, warnings as errors, each for what it
                is built for: the crates with host tests (their tests too), the
                loader, the kernel and the POSIX layer, the programs on vrt for the
                user target, and those on Rust's standard library for Veda's own
                target, with the toolchain's Clippy
    toolchain   Build the toolchain from implementation/ports/: GCC, binutils and
                musl (a cross compiler for this machine and the native one the
                image installs in /system), the media libraries, and Rust for
                Veda's target (needs build tools; see documentation/C.md and
                RUST.md; --jobs N)
    linux       Build the Linux kernel of the driver VM from
                implementation/ports/linux (see documentation/DRIVERVM.md;
                --jobs N); later builds put it in the image, with the guest's
                programs
    acpi DIR    What devmgr makes of a PC's ACPI tables, worked out on the host: DIR
                holds them as Linux has them (sudo cp -r /sys/firmware/acpi/tables DIR);
                the firmware's variables read as zeros but those given with
                --set NAME=VALUE; PCI configuration space comes from DIR/pci, or from
                this machine
    clean       Remove build outputs
    doctor      Check that the required tools are installed
    help        Show this message

Every command but help and clean first prepares the crates of
implementation/ports/ (smoltcp, zune-jpeg) for Cargo, when a port has
changed: see implementation/ports/README.md.

BUILD OPTIONS:
    --debug             Build without optimisations (slow under emulation)
    --resolution WxH    Screen resolution (default 1920x1080 for run and iso; 1280x800 for
                        build, shot and the scripts, which are laid out for it). QEMU's
                        display takes it for its own size; a PC gets its largest mode up to
                        it, of the display's own shape if the firmware gives its EDID and
                        has such a mode about as wide
    --cmdline \"...\"     Extra kernel command line arguments (with what the build adds, at
                        most 256 bytes: the loader takes no more)
    --no-generate       Reuse the generated media instead of running the generators
    --skip PROGRAM      Leave a program out of the image (repeatable)

RUN OPTIONS:
    --smp N             Number of virtual CPUs (default 4)
    --memory MiB        Guest RAM in MiB (default 1024)
    --headless          No display window (serial console only)
    --no-iommu          Leave the IOMMU out of QEMU's machine (it has one, remapping
                        interrupts, as the driver VM's devices need)
    --gpu, --no-gpu     Give QEMU's machine a 3D GPU (virtio-gpu with virgl, rendering on the
                        host's GPU) or not (default: if this QEMU has one)
    --no-audio          Do not attach a sound device
    --sound CARD        The sound card: virtio (default) or hda (Intel HD Audio, as most PCs
                        have)
    --serial FILE       Write the serial console to FILE instead of the terminal
    --gdb               Wait for a debugger on localhost:1234
    --qemu-arg ARG      Pass ARG through to QEMU (repeatable)
    --fresh-home        Start with a new home directory (deletes target/veda/home.img)
    --net MODE          Network: ethernet (default, a wired card on QEMU's NAT) or none
    --nic MODEL         Model of the wired card: virtio-net-pci (default), e1000e, igb
    --disk-bus BUS      How QEMU attaches the disks: virtio (default) or nvme
    --input DEVICES     Keyboard and pointer: standard (default: the machine's keyboard, a
                        PC's PS/2 one, and the virtio tablet) or usb (on the xHCI controller: a
                        keyboard and a tablet behind a hub, and a mouse)
    --live              Boot the live system from a USB stick, as a PC would (the stick's
                        image is target/veda/stick.iso, not iso's)

SCRIPT OPTIONS:
    --jobs N            How many scripts run at once (scripts, test --ui; default: one per
                        five processors, at least one)

SHOT OPTIONS:
    --wait SECS         Seconds to wait before the screenshot (default 10)
    --until TEXT        Instead, wait until the serial log contains TEXT
    --out FILE          Output PNG (default target/veda/screen.png)

ISO OPTIONS:
    --out FILE          The ISO image (default target/veda/veda.iso)
";

/// Options shared by the build-related commands.
#[derive(Clone)]
pub struct Options {
    pub profile: Profile,
    /// `--resolution` (otherwise each boot has its own: see
    /// `system::screen`).
    pub resolution: Option<(u32, u32)>,
    pub cmdline: String,
    pub vm: qemu::VmConfig,
    pub wait: f64,
    pub until: Option<String>,
    pub out: Option<PathBuf>,
    /// `test`: also run the GUI scripts in `verification/ui` and
    /// `verification/agent`.
    pub ui: bool,
    /// Run the media generators (off: reuse what they made last).
    pub generate: bool,
    /// Programs left out of the image.
    pub skip: Vec<String>,
    /// `run`: start with a new, empty home directory.
    pub fresh_home: bool,
    /// `run`, `shot`, `script`: boot the live system from a USB stick.
    pub live: bool,
    /// `script`: the system as `scripts` and `test` built it, instead of
    /// building it.
    pub prebuilt: Option<PathBuf>,
    /// `scripts`, `test --ui`: how many scripts run at once.
    pub jobs: usize,
    /// The options as given, for the scripts' own runs.
    pub given: Vec<String>,
}

pub fn parse_options(args: &[String]) -> Result<Options> {
    let mut o = Options {
        profile: Profile::Release,
        resolution: None,
        cmdline: String::new(),
        vm: qemu::VmConfig::default(),
        wait: 10.0,
        until: None,
        out: None,
        ui: false,
        generate: true,
        skip: Vec::new(),
        fresh_home: false,
        live: false,
        prebuilt: None,
        jobs: std::thread::available_parallelism().map_or(1, |n| (n.get() / 5).max(1)),
        given: args.to_vec(),
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--debug" => o.profile = Profile::Debug,
            "--release" => o.profile = Profile::Release,
            "--resolution" => {
                let v = value(arg)?;
                o.resolution =
                    Some(resolution_size(&v).ok_or("--resolution expects WxH, at least 640x480 (such as 1920x1080)")?);
            }
            "--live" => o.live = true,
            "--cmdline" => o.cmdline = value(arg)?,
            "--smp" => {
                o.vm.cpus = value(arg)?.parse().ok().filter(|&n| n > 0).ok_or("--smp expects a number of processors")?
            }
            "--memory" => {
                o.vm.memory_mib =
                    value(arg)?.parse().ok().filter(|&m| m >= 256).ok_or("--memory expects MiB, 256 at least")?
            }
            "--headless" => o.vm.display = false,
            "--no-iommu" => o.vm.iommu = false,
            "--gpu" => o.vm.gpu = Some(true),
            "--no-gpu" => o.vm.gpu = Some(false),
            "--no-audio" => o.vm.audio = false,
            "--sound" => {
                let card = value(arg)?;
                if !matches!(card.as_str(), "virtio" | "hda") {
                    return Err(format!("--sound: unknown card '{card}' (virtio or hda)"));
                }
                o.vm.sound = card;
            }
            "--serial" => o.vm.serial_file = Some(PathBuf::from(value(arg)?)),
            "--gdb" => o.vm.gdb = true,
            "--qemu-arg" => o.vm.extra.push(value(arg)?),
            "--wait" => {
                o.wait = value(arg)?
                    .parse()
                    .ok()
                    .filter(|w: &f64| w.is_finite() && *w >= 0.0)
                    .ok_or("--wait expects seconds")?
            }
            "--until" => o.until = Some(value(arg)?),
            "--out" => o.out = Some(PathBuf::from(value(arg)?)),
            "--ui" => o.ui = true,
            "--prebuilt" => o.prebuilt = Some(PathBuf::from(value(arg)?)),
            "--jobs" | "-j" => o.jobs = util::jobs(it.next())?,
            "--no-generate" => o.generate = false,
            "--skip" => o.skip.push(value(arg)?),
            "--fresh-home" => o.fresh_home = true,
            "--net" => {
                let v = value(arg)?;
                o.vm.net = qemu::NetMode::parse(&v).ok_or(format!("unknown network mode '{v}'"))?;
            }
            "--nic" => o.vm.nic_model = Some(value(arg)?),
            "--disk-bus" => {
                let v = value(arg)?;
                o.vm.disk_bus = qemu::DiskBus::parse(&v).ok_or(format!("unknown disk bus '{v}' (virtio, nvme)"))?;
            }
            "--input" => {
                let v = value(arg)?;
                o.vm.input =
                    qemu::InputDevices::parse(&v).ok_or(format!("unknown input devices '{v}' (standard, usb)"))?;
            }
            other => return Err(format!("unknown option '{other}' (see `cargo orchestration help`)")),
        }
    }
    Ok(o)
}

/// The width and height in a resolution such as "1920x1080".
fn resolution_size(v: &str) -> Option<(u32, u32)> {
    let (w, h) = v.split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w >= 640 && h >= 480).then_some((w, h))
}

/// What the scripts' own runs are given of `given`, the options of the
/// command that runs them: all but the command's own (`--ui`, `--jobs`,
/// `--prebuilt`, the scripts).
pub fn passed_on(given: &[String]) -> Vec<String> {
    let mut passed = Vec::new();
    let mut it = given.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--ui" => {}
            "--jobs" | "-j" | "--prebuilt" => {
                it.next();
            }
            a if a.ends_with(".vts") => {}
            _ => passed.push(a.clone()),
        }
    }
    passed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn options_are_checked() {
        let o = parse_options(&args("--resolution 1920x1080 --jobs 3 --net none --sound hda")).unwrap();
        assert_eq!(
            (o.resolution, o.jobs, o.vm.net, o.vm.sound.as_str()),
            (Some((1920, 1080)), 3, qemu::NetMode::None, "hda")
        );
        for bad in [
            "--jobs 0",
            "--jobs",
            "--smp 0",
            "--wait -1",
            "--wait nan",
            "--resolution 320x200",
            "--sound sb16",
            "--frob",
        ] {
            assert!(parse_options(&args(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn scripts_get_the_options_but_the_commands_own() {
        let given = args("--ui --jobs 2 --no-gpu a.vts --prebuilt dir --sound hda");
        assert_eq!(passed_on(&given), ["--no-gpu", "--sound", "hda"]);
    }
}

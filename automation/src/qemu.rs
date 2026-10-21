// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Locating QEMU and its UEFI firmware, and assembling the command line.
//! The machine is the architecture's (`architecture::qemu`): its emulator,
//! its firmware, and the arguments and devices particular to it.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::architecture;
use crate::util::{self, Result};

/// A path as the value of one of QEMU's options, where a comma ends the
/// value unless it is doubled.
pub fn option_path(path: &Path) -> String {
    path.to_string_lossy().replace(',', ",,")
}

/// Makes the program `cmd` starts (QEMU) end with this process, however
/// this one ends (a deadline's kill of a script's process among the ways):
/// Linux kills it when the thread that started it ends, which for
/// `automation` is the main thread, which runs the scripts.
pub fn ends_with_us(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    let parent = std::process::id();
    // SAFETY: the closure runs in the child between `fork` and `exec`,
    // where only async-signal-safe functions may be called: `prctl`,
    // `getppid` and `_exit` are, and it allocates nothing.
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // The parent may have ended before the signal was asked for.
            if libc::getppid() as u32 != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
}

/// Where to find QEMU and its UEFI firmware.
pub struct QemuInstall {
    pub binary: PathBuf,
    pub firmware_code: PathBuf,
    pub firmware_vars_template: PathBuf,
    /// Memory the firmware needs blank at every reset (its start and size),
    /// if any: see `architecture::qemu::blank_at_reset`.
    pub blank_at_reset: Option<(u64, u64)>,
}

impl QemuInstall {
    /// Finds QEMU via `$QEMU` or `PATH`, and its UEFI firmware (EDK II's)
    /// via `$OVMF_CODE`/`$OVMF_VARS`, QEMU's data directory or where Linux
    /// distributions install it.
    pub fn locate() -> Result<Self> {
        let binary = std::env::var_os("QEMU")
            .map(PathBuf::from)
            .or_else(|| util::find_on_path(architecture::qemu::BINARY))
            .ok_or("QEMU not found: install it or set the QEMU environment variable")?;

        let qemu_dir = binary.parent().unwrap_or(Path::new("."));
        let dirs = [qemu_dir.join("share"), qemu_dir.join("../share/qemu"), PathBuf::from("/usr/share/qemu")]
            .into_iter()
            .chain(architecture::qemu::FIRMWARE_DIRS.iter().map(PathBuf::from));
        let found = dirs
            .flat_map(|d| {
                architecture::qemu::FIRMWARE_FILES.iter().map(move |(code, vars)| (d.join(code), d.join(vars)))
            })
            .find(|(code, vars)| code.is_file() && vars.is_file());
        let missing = format!(
            "the UEFI firmware was not found: install it ({}) or set OVMF_CODE and OVMF_VARS",
            architecture::qemu::FIRMWARE_PACKAGES
        );
        let firmware_code = std::env::var_os("OVMF_CODE")
            .map(PathBuf::from)
            .or_else(|| found.as_ref().map(|(code, _)| code.clone()))
            .ok_or(&missing)?;
        let firmware_vars_template =
            std::env::var_os("OVMF_VARS").map(PathBuf::from).or_else(|| found.map(|(_, vars)| vars)).ok_or(missing)?;
        let code = util::read(&firmware_code)?;
        let blank_at_reset = architecture::qemu::blank_at_reset(&code);
        Ok(QemuInstall { binary, firmware_code, firmware_vars_template, blank_at_reset })
    }

    /// Whether this QEMU has the 3D virtio-gpu (built with virglrenderer):
    /// it lists `virtio-gpu-gl-pci` among its devices.
    pub fn has_gl_gpu(&self) -> bool {
        static HAS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *HAS.get_or_init(|| {
            std::process::Command::new(&self.binary)
                .args(["-device", "help"])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("\"virtio-gpu-gl-pci\""))
        })
    }
}

/// How the virtual machine reaches the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetMode {
    /// No network card.
    None,
    /// A wired card on QEMU's NAT (DHCP 10.0.2.15, gateway 10.0.2.2, DNS
    /// 10.0.2.3; the host is reachable at 10.0.2.2).
    Ethernet,
}

impl NetMode {
    pub fn parse(s: &str) -> Option<NetMode> {
        match s {
            "none" | "off" => Some(NetMode::None),
            "ethernet" | "wired" | "user" => Some(NetMode::Ethernet),
            _ => None,
        }
    }
}

/// A Unix socket's path as a QEMU option value.
pub fn socket_option(path: &Path) -> Result<String> {
    let s = path.to_str().ok_or_else(|| format!("{} is not UTF-8", path.display()))?;
    // A socket's address holds at most 107 bytes of path.
    if s.len() > 107 {
        return Err(format!("{s}: no socket can be there (its path is longer than 107 bytes)"));
    }
    Ok(option_path(path))
}

/// The keyboard and pointing devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputDevices {
    /// The machine's own keyboard (a PC's is PS/2's) and the virtio tablet.
    Standard,
    /// USB devices on the xHCI controller, as most PCs have: a keyboard and
    /// a tablet behind a hub, and a mouse.
    Usb,
}

impl InputDevices {
    pub fn parse(s: &str) -> Option<InputDevices> {
        match s {
            "standard" => Some(InputDevices::Standard),
            "usb" => Some(InputDevices::Usb),
            _ => None,
        }
    }
}

/// The display's own size, which its EDID gives as its preferred mode, and
/// its refresh rate: the firmware starts in that mode (OVMF reads the size
/// from the EDID's first detailed timing) and the driver VM's Linux offers
/// it first. [`Screen::new`] makes only sizes QEMU can present that way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Screen {
    pub width: u32,
    pub height: u32,
    /// The refresh rate (mHz).
    refresh: u32,
}

impl Screen {
    /// QEMU's own: 1280x800 at 75 Hz.
    pub const DEFAULT: Screen = Screen { width: 1280, height: 800, refresh: 75_000 };

    /// A display of `width` by `height`, at QEMU's 75 Hz, or at the fastest
    /// whole rate below it at which the EDID's detailed timing can still
    /// describe it: QEMU makes up its timings with 35% horizontal and 3.5%
    /// vertical blanking (hw/display/edid-generate.c), and the timing holds
    /// a pixel clock below 655.36 MHz and sizes below 4096 each way. The
    /// VGA's widths are multiples of 8 (QEMU rounds others down).
    pub fn new(width: u32, height: u32) -> Result<Screen> {
        if !width.is_multiple_of(8) {
            return Err(format!("QEMU's display cannot be {width} pixels wide: its widths are multiples of 8"));
        }
        if width >= 4096 || height >= 4096 {
            return Err(format!(
                "QEMU's display cannot take {width}x{height} for its own size: at most 4095 pixels each way"
            ));
        }
        let (w, h) = (width as u64, height as u64);
        // The pixels of a frame with its blanking, and the fastest rate
        // (whole hertz) whose clock, in units of 10 kHz, is below 65 536.
        let total = (w + w * 35 / 100) * (h + h * 35 / 1000);
        let hz = ((65_536 * 10_000_000 - 1) / total / 1000).min(75) as u32;
        Ok(Screen { width, height, refresh: hz * 1000 })
    }
}

/// User-tunable virtual machine settings.
#[derive(Debug, Clone)]
pub struct VmConfig {
    pub cpus: u32,
    pub memory_mib: u32,
    /// Show a window (`false` = headless).
    pub display: bool,
    /// The display's own size: the screen's.
    pub screen: Screen,
    pub audio: bool,
    /// The sound card: `virtio` (the default) or `hda` (Intel HD Audio, the
    /// machine's controller).
    pub sound: String,
    /// Record guest audio to this WAV file instead of playing it.
    pub audio_wav: Option<PathBuf>,
    /// QEMU's silent sound system instead (`none`): nothing records the
    /// output; the input records silence, at the card's pace.
    pub audio_silent: bool,
    /// Serial output destination: `None` = this terminal.
    pub serial_file: Option<PathBuf>,
    /// QMP's socket (a Unix socket), which automated tests drive QEMU by.
    pub qmp: Option<PathBuf>,
    /// Wait for a GDB connection on port 1234.
    pub gdb: bool,
    /// Allow the guest to terminate QEMU with an exit code (tests).
    pub debug_exit: bool,
    /// The boot disk's format: `raw`, or `qcow2` for an overlay of a
    /// system's image (see `image::qcow2`).
    pub boot_format: &'static str,
    /// Disk image holding the user's home directory (serial `veda-home`).
    pub home_disk: Option<PathBuf>,
    /// The boot image is the live system's ISO on a USB stick (read-only,
    /// unless `usb_stick_writable`), not the boot disk.
    pub usb_stick: bool,
    pub usb_stick_writable: bool,
    /// ACPI tables added to the firmware's (tests' descriptions of devices).
    pub acpi_tables: Vec<PathBuf>,
    /// Variables added to the firmware's store, for a scripted run's.
    pub firmware_variables: Vec<crate::ovmfvars::Variable>,
    /// The keyboard and pointing devices.
    pub input: InputDevices,
    /// Let the guest reboot (otherwise a reset, e.g. after a triple fault,
    /// stops QEMU).
    pub allow_reboot: bool,
    /// The network connection.
    pub net: NetMode,
    /// Model of the wired card (`None`: virtio-net).
    pub nic_model: Option<String>,
    /// A display that is also a 3D GPU (virtio-gpu with virgl): `None` uses
    /// one if QEMU has it.
    pub gpu: Option<bool>,
    /// An IOMMU, remapping interrupts, as PCs have (the machine's: see
    /// `architecture::qemu::machine_args`): the driver VM's devices need
    /// it.
    pub iommu: bool,
    /// QEMU's USB network adapter (`usb-net`: CDC Ethernet, which Veda has
    /// no driver for) on the xHCI controller, on a NAT of its own.
    pub usb_net: bool,
    /// Extra raw QEMU arguments.
    pub extra: Vec<String>,
}

impl Default for VmConfig {
    fn default() -> Self {
        VmConfig {
            cpus: 4,
            // The system image is in memory (some 490 MiB, the web engine
            // most of it) and the driver VM takes 256 MiB: 1 GiB would leave
            // too little for a page composited at a laptop's size.
            memory_mib: 2048,
            display: true,
            screen: Screen::DEFAULT,
            audio: true,
            sound: "virtio".into(),
            audio_wav: None,
            audio_silent: false,
            serial_file: None,
            qmp: None,
            gdb: false,
            debug_exit: false,
            boot_format: "raw",
            home_disk: None,
            usb_stick: false,
            usb_stick_writable: false,
            acpi_tables: Vec::new(),
            firmware_variables: Vec::new(),
            input: InputDevices::Standard,
            allow_reboot: false,
            net: NetMode::Ethernet,
            nic_model: None,
            gpu: None,
            iommu: true,
            usb_net: false,
            extra: Vec::new(),
        }
    }
}

/// QEMU id of the wired network card (for QMP `set_link`).
pub const WIRED_NIC_ID: &str = "nic0";

/// QEMU id of the display device.
const DISPLAY_ID: &str = "video0";

/// Size of a new home disk: the VFS keeps two snapshots of `/home` on it,
/// so half of it is the home directory's room, enough for a web game's
/// data (a page keeps it in the browser's storage: Vice City's 135 MB),
/// where 64 MiB left 32. The file takes room on this computer only as
/// Veda writes it.
const HOME_DISK_BYTES: u64 = 1 << 30;

/// Creates an empty home disk at `path` (replacing any existing one when
/// `fresh`). An empty disk makes Veda start a new home directory.
pub fn prepare_home_disk(path: &Path, fresh: bool) -> Result {
    if fresh || !path.exists() {
        if let Some(dir) = path.parent() {
            util::create_dir_all(dir)?;
        }
        let f = std::fs::File::create(path).map_err(|e| format!("creating {}: {e}", path.display()))?;
        f.set_len(HOME_DISK_BYTES).map_err(|e| format!("sizing {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Builds the QEMU command line for booting `disk`.
pub fn command(install: &QemuInstall, disk: &Path, vars: &Path, cfg: &VmConfig) -> Result<Command> {
    let mut cmd = Command::new(&install.binary);
    let flash = |file: &Path, ro: bool| {
        format!(
            "if=pflash,format=raw,unit={},{}file={}",
            if ro { 0 } else { 1 },
            if ro { "readonly=on," } else { "" },
            option_path(file)
        )
    };
    cmd.args(["-name", "Veda"]);
    // The machine, with its own keyboard unless input is USB's; its IOMMU,
    // before the devices it translates; and its processors.
    cmd.args(architecture::qemu::machine_args(cfg.input == InputDevices::Standard, cfg.iommu));
    for t in &cfg.acpi_tables {
        cmd.args(["-acpitable", &format!("file={}", option_path(t))]);
    }
    // KVM when the host offers it; otherwise the TCG emulator
    // (multi-threaded, one host thread per vCPU).
    cmd.args(["-accel", "kvm", "-accel", "tcg,thread=multi"]);
    cmd.args(["-smp", &cfg.cpus.to_string()]);
    cmd.args(["-m", &format!("{}M", cfg.memory_mib)]);
    cmd.args(["-drive", &flash(&install.firmware_code, true)]);
    cmd.args(["-drive", &flash(vars, false)]);
    // What the firmware needs blank at every reset, from a file of zeros
    // QEMU's loader writes there (see
    // `architecture::qemu::blank_at_reset`).
    if let Some((start, size)) = install.blank_at_reset {
        let blank = util::out_dir().join("firmware-blank.bin");
        if std::fs::metadata(&blank).map(|m| m.len()).ok() != Some(size) {
            util::create_dir_all(&util::out_dir())?;
            util::write(&blank, vec![0u8; size as usize])?;
        }
        cmd.args(["-device", &format!("loader,file={},addr={start:#x},force-raw=on", option_path(&blank))]);
    }
    // The display, at 00:01.0 (where `-vga std` puts it, before any other
    // device takes it), named so that the tablet can be bound to it (see
    // `-display` below): QEMU's standard VGA, whose memory holds the
    // firmware's framebuffer, and which Linux's bochs driver drives in the
    // driver VM; its EDID gives the screen's size as its own. With a GPU, a
    // virtio-gpu with virgl beside it, whose virglrenderer runs on the
    // host's OpenGL and needs an OpenGL display backend (a window or an
    // offscreen one): its render node is the renderer's in the driver VM,
    // while the screen stays the VGA's, which QEMU can take screenshots of
    // (not of a GPU's scanouts). It is at 00:10.0, so that the other
    // devices are where they are with it or without it.
    let gpu = cfg.gpu.unwrap_or_else(|| install.has_gl_gpu());
    let Screen { width: w, height: h, refresh } = cfg.screen;
    // Its memory holds a frame of the screen, for the firmware and for
    // Linux's driver: QEMU's 16 MiB up to 2560x1600, a power of two above.
    let memory = (w as u64 * h as u64 * 4).div_ceil(1 << 20).next_power_of_two().max(16);
    cmd.args([
        "-vga",
        "none",
        "-device",
        &format!("VGA,id={DISPLAY_ID},addr=0x1,xres={w},yres={h},refresh_rate={refresh},vgamem_mb={memory}"),
    ]);
    if gpu {
        cmd.args(["-device", &virtio(cfg, "virtio-gpu-gl-pci,id=gpu0,addr=0x10")]);
    }
    // In a window, the tablet is bound to the display: the window's pointer
    // is then absolute from power-on, not only once the guest's driver has
    // started (see `-display` below).
    let bound = if cfg.display { format!(",display={DISPLAY_ID}") } else { String::new() };
    if cfg.usb_stick || cfg.input == InputDevices::Usb || cfg.usb_net {
        cmd.args(["-device", "qemu-xhci,id=xhci"]);
    }
    if cfg.usb_net {
        cmd.args(["-netdev", "user,id=usbnet0,net=10.0.4.0/24"]);
        cmd.args(["-device", "usb-net,netdev=usbnet0,bus=xhci.0,mac=52:54:00:12:34:58"]);
    }
    if cfg.usb_stick {
        // As a PC sees a stick the ISO was written to.
        let readonly = if cfg.usb_stick_writable { "" } else { "readonly=on," };
        cmd.args(["-drive", &format!("id=disk0,if=none,format=raw,{readonly}file={}", option_path(disk))]);
        cmd.args(["-device", "usb-storage,bus=xhci.0,drive=disk0,bootindex=0,removable=on"]);
    } else {
        // Each disk on an NVM Express controller of its own, as on most PCs.
        let format = cfg.boot_format;
        cmd.args(["-drive", &format!("id=disk0,if=none,format={format},file={}", option_path(disk))]);
        cmd.args(["-device", "nvme,drive=disk0,bootindex=0,serial=veda-boot"]);
    }
    if let Some(home) = &cfg.home_disk {
        cmd.args(["-drive", &format!("id=home,if=none,format=raw,file={}", option_path(home))]);
        cmd.args(["-device", "nvme,drive=home,serial=veda-home"]);
    }
    match cfg.input {
        InputDevices::Standard => {
            cmd.args(["-device", &virtio(cfg, &format!("virtio-tablet-pci{bound}"))]);
        }
        InputDevices::Usb => {
            // A keyboard and a tablet behind a hub, and a mouse on a root
            // port (after the live system's stick, if any). QMP's absolute
            // pointer events go to the tablet.
            cmd.args(["-device", "usb-hub,bus=xhci.0,port=2"]);
            cmd.args(["-device", "usb-kbd,bus=xhci.0,port=2.1"]);
            cmd.args(["-device", &format!("usb-tablet,bus=xhci.0,port=2.2{bound}")]);
            cmd.args(["-device", "usb-mouse,bus=xhci.0,port=3"]);
        }
    }
    // Host entropy for the firmware's EFI_RNG_PROTOCOL, which seeds the
    // kernel's random number generator.
    cmd.args(["-device", &virtio(cfg, "virtio-rng-pci")]);
    if cfg.audio {
        match &cfg.audio_wav {
            _ if cfg.audio_silent => cmd.args(["-audiodev", "none,id=audio0"]),
            Some(wav) => cmd.args(["-audiodev", &format!("wav,id=audio0,path={}", option_path(wav))]),
            None => cmd.args(["-audiodev", "sdl,id=audio0"]),
        };
        // With the host's sound system the card also has an input stream:
        // the host's microphone (with the silent one, silence). Recorded
        // runs (WAV) have none, so a test microphone can take its place.
        let streams = if cfg.audio_wav.is_some() { 1 } else { 2 };
        match cfg.sound.as_str() {
            // The machine's HD Audio controller with a codec that has a
            // line output and, with the host's sound system, a line input.
            "hda" => {
                let codec = if streams == 2 { "hda-duplex" } else { "hda-output" };
                cmd.args(["-device", &format!("{},id=hda", architecture::qemu::HDA_CONTROLLER)]);
                cmd.args(["-device", &format!("{codec},bus=hda.0,audiodev=audio0")])
            }
            _ => cmd.args(["-device", &virtio(cfg, &format!("virtio-sound-pci,audiodev=audio0,streams={streams}"))]),
        };
    }
    if cfg.net == NetMode::Ethernet {
        let model = cfg.nic_model.as_deref().unwrap_or("virtio-net-pci");
        let nic = format!("{model},id={WIRED_NIC_ID},netdev=net0,mac=52:54:00:12:34:56");
        cmd.args(["-netdev", "user,id=net0"]);
        cmd.args(["-device", &if model.starts_with("virtio") { virtio(cfg, &nic) } else { nic }]);
    } else {
        // Without this QEMU adds a default card.
        cmd.args(["-nic", "none"]);
    }
    if cfg.display {
        // GTK, whose window grabs the mouse at a click while the pointer is
        // relative; the tablet is bound to the display, so the pointer is
        // absolute from power-on, and a click before the guest's driver
        // runs grabs nothing. The window takes the screen's size: with a
        // display that can follow the window's size, as the GPU's could,
        // QEMU would otherwise zoom to fit and open the window at its
        // smallest, a quarter of the screen each way, the screen shrunk
        // into it. Veda's screen is the VGA's, which keeps its size.
        cmd.args(["-display", if gpu { "gtk,gl=on,zoom-to-fit=off" } else { "gtk,zoom-to-fit=off" }]);
    } else if gpu {
        // Headless, on the host's GPU of `render_node`.
        let display = match render_node() {
            Some(node) => format!("egl-headless,rendernode={node}"),
            None => String::from("egl-headless"),
        };
        cmd.args(["-display", &display]);
    } else {
        cmd.args(["-display", "none"]);
    }
    let serial = match &cfg.serial_file {
        Some(path) => format!("file,id=serial0,path={}", option_path(path)),
        // The terminal, and a copy in the output directory (for looking at
        // a session afterwards).
        None => format!("stdio,id=serial0,logfile={}", option_path(&util::out_dir().join("serial.log"))),
    };
    cmd.args(["-chardev", &serial, "-serial", "chardev:serial0"]);
    if let Some(path) = &cfg.qmp {
        cmd.args(["-qmp", &format!("unix:{},server=on,wait=off", socket_option(path)?)]);
    }
    if cfg.debug_exit {
        cmd.args(["-device", architecture::qemu::DEBUG_EXIT]);
    }
    if cfg.gdb {
        cmd.args(["-s", "-S"]);
    }
    if !cfg.allow_reboot {
        cmd.args(["-no-reboot"]);
    }
    cmd.args(&cfg.extra);
    Ok(cmd)
}

/// The host's render node QEMU's 3D GPU renders on, headless: the one
/// `VEDA_RENDERNODE` names (`/dev/dri/renderD129`), or else, where the host
/// has several GPUs, the one the firmware showed its screen on (a laptop's
/// integrated GPU, whose OpenGL its desktop runs on); `None` leaves it to
/// QEMU, which takes the first.
pub fn render_node() -> Option<String> {
    if let Ok(node) = std::env::var("VEDA_RENDERNODE")
        && !node.is_empty()
    {
        return Some(node);
    }
    let boot_vga =
        |n: &str| std::fs::read_to_string(format!("/sys/class/drm/{n}/device/boot_vga")).is_ok_and(|v| v.trim() == "1");
    std::fs::read_dir("/sys/class/drm")
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with("renderD") && boot_vga(n))
        .map(|n| format!("/dev/dri/{n}"))
}

/// A virtio device of `spec`'s (`virtio-net-pci,netdev=...`): behind the
/// machine's IOMMU when it has one, as devices are on a PC that has one, so
/// that a device can go to a virtual machine of Veda's (whose addresses
/// only the IOMMU turns into the machine's). Translated devices are modern
/// ones only.
fn virtio(cfg: &VmConfig, spec: &str) -> String {
    if cfg.iommu { format!("{spec},disable-legacy=on,iommu_platform=on") } else { spec.to_string() }
}

/// A scripted run's UEFI variable store: a copy of the firmware's template
/// (every run starts with the same), with `variables` added.
pub fn script_vars_file(install: &QemuInstall, variables: &[crate::ovmfvars::Variable]) -> Result<PathBuf> {
    let path = util::out_dir().join("script-vars.fd");
    let template = &install.firmware_vars_template;
    let mut image = util::read(template)?;
    if !variables.is_empty() {
        crate::ovmfvars::add(&mut image, variables).map_err(|e| format!("{}: {e}", template.display()))?;
    }
    util::create_dir_all(&util::out_dir())?;
    util::write(&path, image)?;
    Ok(path)
}

/// Returns the path of this machine's writable UEFI variable store, creating
/// it from the firmware template on first use.
pub fn vars_file(install: &QemuInstall) -> Result<PathBuf> {
    let path = util::out_dir().join("ovmf-vars.fd");
    if !path.exists() {
        util::create_dir_all(&util::out_dir())?;
        util::copy(&install.firmware_vars_template, &path)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screens_are_what_qemu_can_present() {
        assert_eq!(Screen::new(1280, 800), Ok(Screen::DEFAULT));
        for (w, h) in [(1920, 1080), (2560, 1600), (3200, 1800)] {
            assert_eq!(Screen::new(w, h).map(|s| s.refresh), Ok(75_000), "{w}x{h}");
        }
        // 4K's pixel clock at 75 Hz is beyond the timing's: it is as fast
        // as it can be (QEMU's clock at 56 Hz is 64 882, at 57 Hz 66 041).
        assert_eq!(Screen::new(3840, 2160).map(|s| s.refresh), Ok(56_000));
        assert_eq!(Screen::new(3840, 2400).map(|s| s.refresh), Ok(50_000));
        assert!(Screen::new(1366, 768).is_err());
        assert!(Screen::new(4096, 2160).is_err());
    }

    /// Every path in QEMU's options has its commas doubled, and the serial
    /// log goes through a character device, whose options read them so.
    #[test]
    fn paths_in_options_keep_their_commas() {
        assert_eq!(option_path(Path::new("/a,b/c,,d")), "/a,,b/c,,,,d");
        let dir = Path::new("/tmp/x,y");
        let install = QemuInstall {
            binary: PathBuf::from("qemu-system-x86_64"),
            firmware_code: dir.join("code.fd"),
            firmware_vars_template: dir.join("vars.fd"),
            blank_at_reset: None,
        };
        let cfg = VmConfig {
            gpu: Some(false),
            serial_file: Some(dir.join("serial.log")),
            audio_wav: Some(dir.join("audio.wav")),
            home_disk: Some(dir.join("home.img")),
            acpi_tables: vec![dir.join("t.aml")],
            qmp: Some(dir.join("qmp.sock")),
            boot_format: "qcow2",
            ..VmConfig::default()
        };
        let cmd = command(&install, &dir.join("disk.img"), &dir.join("v.fd"), &cfg).unwrap();
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        for path in args.iter().filter(|a| a.contains("/tmp/x")) {
            assert!(path.contains("/tmp/x,,y") && !path.contains("/tmp/x,y"), "{path}");
        }
        assert!(args.contains(&"id=disk0,if=none,format=qcow2,file=/tmp/x,,y/disk.img".to_string()), "{args:?}");
        assert!(args.contains(&"file,id=serial0,path=/tmp/x,,y/serial.log".to_string()), "{args:?}");
        assert!(args.contains(&"wav,id=audio0,path=/tmp/x,,y/audio.wav".to_string()), "{args:?}");
        assert!(args.windows(2).any(|w| w == ["-serial", "chardev:serial0"]));
    }
}

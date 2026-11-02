// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The architectures Veda is built for.
//!
//! Everything about building Veda and running it in QEMU that depends on
//! the architecture Veda is built for (not on the one `automation` runs
//! on) is in `automation/source/architectures/<architecture>/`: AMD64's
//! and ARM64's. A command builds for one of them, which it chooses as it
//! starts ([`choose`]): `--architecture`, `$VEDA_ARCHITECTURE`, or else
//! the build machine's own. The rest of `automation` reaches it only
//! through [`current`], an [`Architecture`] whose fields every architecture
//! fills with the same meanings.
//!
//! Code in `source/architectures/<architecture>/` may use the rest of
//! `automation`; the rest of `automation` uses nothing of it but this.

use std::sync::OnceLock;

use crate::qemu::Screen;
use crate::util::Result;

#[path = "architectures/amd64/mod.rs"]
mod amd64;
#[path = "architectures/arm64/mod.rs"]
mod arm64;

/// The architectures Veda is built for.
const ALL: [&Architecture; 2] = [&amd64::ARCHITECTURE, &arm64::ARCHITECTURE];

/// The environment variable that names the architecture to build for, as
/// `--architecture` does. Commands that start others leave it set.
const VARIABLE: &str = "VEDA_ARCHITECTURE";

/// An architecture Veda is built for.
pub struct Architecture {
    /// The architecture's directories' name (`architectures/<name>`
    /// throughout the tree).
    pub name: &'static str,
    /// Where what is built for it goes, from the repository: the
    /// toolchain, the driver VM's Linux, the web engine, Python and the
    /// system's images each in a directory of their own there
    /// ([`crate::util::architecture_dir`]).
    pub outputs: &'static [&'static str],
    /// Rust's target of the loader.
    pub uefi_target: &'static str,
    /// Rust's target of the kernel.
    pub kernel_target: &'static str,
    /// Rust's target of user-space programs.
    pub user_target: &'static str,
    /// Rust's target of the code C programs link: the POSIX layer and the
    /// media decoder's Rust half.
    pub bare_target: &'static str,
    /// Rust's target of the driver VM's programs.
    pub guest_target: &'static str,
    /// Rust's target of programs on its standard library.
    pub std_target: &'static str,
    /// The C toolchain's target, Veda's.
    pub c_target: &'static str,
    /// The C toolchain's target, the driver VM's.
    pub guest_c_target: &'static str,
    /// What the ports need from the system besides what every
    /// architecture's need.
    pub port_tools: &'static [&'static str],
    /// Whether [`Self::port_tools`], which are there, are recent enough.
    pub check_port_tools: fn() -> Result,
    /// Whether its Mesa has iris, the driver of Intel's GPUs
    /// (`implementation/ports/architectures/<architecture>/settings.sh`).
    pub intel_gpus: bool,
    /// The machine of Veda's executables (the kernel's and the programs').
    pub machine: elf::Machine,
    /// Where the loader goes on the boot partition.
    pub uefi_boot_path: &'static str,
    /// Whether QEMU lets Veda's hypervisor run.
    pub nested_virtualization: fn() -> Result,
    /// QEMU's machine.
    pub qemu: Qemu,
    /// ACPI tables that tests add to the machine's.
    pub acpi_tests: AcpiTests,
}

/// QEMU's machine of an architecture.
pub struct Qemu {
    /// Its emulator.
    pub binary: &'static str,
    /// Where Linux distributions install its firmware, besides QEMU's own
    /// data directories.
    pub firmware_dirs: &'static [&'static str],
    /// The firmware and its variable store, as QEMU's own builds name them
    /// and as Linux distributions do: both from one place, as their sizes
    /// must match.
    pub firmware_files: &'static [(&'static str, &'static str)],
    /// What installs the firmware.
    pub firmware_packages: &'static str,
    /// Memory that firmware `code` needs blank at every reset (its start
    /// and size), if any.
    pub blank_at_reset: fn(&[u8]) -> Option<(u64, u64)>,
    /// The machine has a keyboard of its own (on a PC, PS/2's), which it
    /// can do without.
    pub has_keyboard: bool,
    /// The machine's own arguments, with its keyboard if `keyboard` and an
    /// IOMMU if `iommu`, its processors and how they run.
    pub machine_args: fn(keyboard: bool, iommu: bool) -> Vec<String>,
    /// The display the firmware shows its framebuffer on, as `id`, with
    /// `screen` for its own size if it takes one.
    pub display_args: fn(id: &str, screen: Screen) -> Vec<String>,
    /// The size of the screen, where the firmware has no other for that
    /// display.
    pub fixed_screen: Option<(u32, u32)>,
    /// The machine's HD Audio controller.
    pub hda_controller: &'static str,
    /// The device through which the guest ends QEMU with an exit code, if
    /// it needs one to.
    pub debug_exit: Option<&'static str>,
    /// Where each processor is, from QEMU's `info registers -a`.
    pub processors: fn(&str) -> String,
}

/// ACPI tables that tests add to the machine's.
pub struct AcpiTests {
    /// The tables [`Self::table`] makes.
    pub names: &'static [&'static str],
    /// The table a name stands for.
    pub table: fn(&str) -> Option<Vec<u8>>,
}

static CURRENT: OnceLock<&'static Architecture> = OnceLock::new();

/// The architecture named `name`.
fn named(name: &str) -> Result<&'static Architecture> {
    ALL.iter().copied().find(|a| a.name == name).ok_or_else(|| {
        let names: Vec<&str> = ALL.iter().map(|a| a.name).collect();
        format!("Veda is not built for '{name}': the architectures are {}", names.join(", "))
    })
}

/// The architecture of the build machine, where it is one of Veda's;
/// otherwise AMD64.
fn build_machine() -> &'static Architecture {
    if cfg!(target_arch = "aarch64") { &arm64::ARCHITECTURE } else { &amd64::ARCHITECTURE }
}

/// Takes `--architecture NAME` out of `args`, and chooses that architecture
/// (or else `$VEDA_ARCHITECTURE`'s, or else the build machine's) for this
/// command and the commands it starts. At most once, before any thread
/// starts.
pub fn choose(args: &mut Vec<String>) -> Result {
    let given = match args.iter().position(|a| a == "--architecture") {
        Some(at) if at + 1 < args.len() => {
            let name = args.remove(at + 1);
            args.remove(at);
            Some(name)
        }
        Some(_) => return Err("--architecture needs a value".into()),
        None => std::env::var(VARIABLE).ok().filter(|v| !v.is_empty()),
    };
    let architecture = given.map_or(Ok(build_machine()), |name| named(&name))?;
    let _ = CURRENT.set(architecture);
    // SAFETY: as this function asks, no other thread reads the environment.
    unsafe { std::env::set_var(VARIABLE, architecture.name) };
    Ok(())
}

/// The architecture Veda is built for.
pub fn current() -> &'static Architecture {
    CURRENT.get().copied().unwrap_or_else(build_machine)
}

/// Whether Veda is built for the build machine's own architecture.
pub fn is_build_machine() -> bool {
    current().name == build_machine().name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_architecture_has_its_own_targets() {
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(a.name, b.name);
                assert_ne!(a.outputs, b.outputs);
                assert_ne!(a.kernel_target, b.kernel_target);
                assert_ne!(a.user_target, b.user_target);
                assert_ne!(a.std_target, b.std_target);
                assert_ne!(a.uefi_boot_path, b.uefi_boot_path);
                assert_ne!(a.machine.code, b.machine.code);
            }
        }
    }

    #[test]
    fn architectures_are_found_by_name() {
        assert_eq!(named("amd64").map(|a| a.name), Ok("amd64"));
        assert_eq!(named("arm64").map(|a| a.name), Ok("arm64"));
        assert!(named("mips").is_err_and(|e| e.contains("amd64, arm64")));
    }
}

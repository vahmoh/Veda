// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `cargo automation` — the Veda developer tool.
//!
//! Run `cargo automation help` for the list of commands. Its options
//! are `cli`'s; what goes into the system and its images is `system`'s;
//! running the tests and the automation scripts is `runner`'s, whose
//! language is `script`'s and whose interpreter is `automate`.

mod acpi;
mod agentsim;
mod arch;
mod automate;
mod checks;
mod cli;
mod components;
mod crates;
mod hostweb;
mod image;
#[cfg(test)]
mod licence;
mod lint;
mod linux;
mod mic;
mod ovmfvars;
mod ports;
mod qemu;
mod qmp;
mod runner;
mod script;
mod system;
mod toolchain;
mod util;
mod voice;

use std::path::PathBuf;
use std::process::ExitCode;

use cli::{HELP, Options};
use util::Result;

/// `cargo automation iso`: the live system, for a USB stick.
fn iso(o: &Options) -> Result {
    let path = o.out.clone().unwrap_or_else(system::iso_image);
    runner::build_iso(o, &path)?;
    println!(
        "\nWrite it to a USB stick with Rufus (partition scheme GPT, target system UEFI,\n\
         ISO mode), or as it is with any image writer. Start the PC from the stick\n\
         with UEFI, with Secure Boot off: Veda runs from memory and leaves the PC's\n\
         disks alone. `cargo automation run --live` tries it in QEMU first."
    );
    Ok(())
}

/// `cargo automation run`: builds, then boots Veda in a QEMU window.
fn run(o: &Options) -> Result {
    let install = qemu::QemuInstall::locate()?;
    // A window about as large as a laptop's screen, unless asked otherwise.
    let o = &Options { resolution: Some(o.resolution.unwrap_or(system::LARGE_SCREEN)), ..o.clone() };
    let mut vm = o.vm.clone();
    let (w, h) = system::screen(o, o.live);
    vm.screen = qemu::Screen::new(w, h)?;
    let disk = if o.live {
        // A stick with the ISO on it, and no home disk.
        let stick = system::live_stick();
        runner::build_iso(o, &stick)?;
        vm.usb_stick = true;
        stick
    } else {
        let disk = runner::build(o)?;
        // The home directory lives on its own disk, kept across builds and runs.
        let home = util::out_dir().join("home.img");
        qemu::prepare_home_disk(&home, o.fresh_home)?;
        vm.home_disk = Some(home);
        disk
    };
    let vars = qemu::vars_file(&install)?;
    let mut cmd = qemu::command(&install, &disk, &vars, &vm)?;
    util::status("Running", format!("{}", install.binary.display()));
    util::run(&mut cmd)
}

/// `cargo automation doctor`.
fn doctor() -> Result {
    let mut ok = true;
    let mut check = |name: &str, r: std::result::Result<String, String>| match r {
        Ok(v) => println!("  [ok]   {name}: {v}"),
        Err(e) => {
            ok = false;
            println!("  [FAIL] {name}: {e}")
        }
    };
    println!("Checking the Veda build environment...");
    check("rustc", tool_version(std::process::Command::new("rustc").arg("--version")));
    let targets = std::process::Command::new("rustup").args(["target", "list", "--installed"]).output();
    let targets = targets.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).map_err(|e| e.to_string());
    for (name, target) in
        [("uefi target", arch::UEFI_TARGET), ("kernel target", arch::KERNEL_TARGET), ("user target", arch::USER_TARGET)]
    {
        check(
            name,
            targets.clone().and_then(|s| {
                if s.contains(target) {
                    Ok("installed".into())
                } else {
                    Err(format!("run `rustup target add {target}`"))
                }
            }),
        );
    }
    check("linker", rust_lld().map(|p| p.display().to_string()));
    check("rust toolchain", toolchain::rust_status());
    check("ported crates", crates::status());
    match qemu::QemuInstall::locate() {
        Ok(q) => {
            check("qemu", Ok(q.binary.display().to_string()));
            check("firmware", Ok(q.firmware_code.display().to_string()));
        }
        Err(e) => check("qemu", Err(e)),
    }
    // QEMU emulates the processor where it cannot use KVM, which works but
    // is several times slower.
    match std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm") {
        Ok(_) => println!("  [ok]   kvm: /dev/kvm"),
        Err(e) => println!(
            "  [--]   kvm: /dev/kvm: {e} (QEMU emulates the processor instead, several times slower; \
             members of the kvm group can use it)"
        ),
    }
    match toolchain::status() {
        Ok(v) => println!("  [ok]   c toolchain: {v}"),
        Err(e) => println!("  [--]   c toolchain: {e}"),
    }
    match toolchain::media_status() {
        Ok(v) => println!("  [ok]   media decoders: {v}"),
        Err(e) => println!("  [--]   media decoders: {e}"),
    }
    match linux::status() {
        Ok(v) => println!("  [ok]   driver vm: {v}"),
        Err(e) => println!("  [--]   driver vm: {e}"),
    }
    if ok { Ok(()) } else { Err("some checks failed".into()) }
}

/// What a tool says of its version, if it runs and succeeds.
fn tool_version(cmd: &mut std::process::Command) -> std::result::Result<String, String> {
    let out = cmd.output().map_err(|e| format!("cannot run it: {e}"))?;
    let said = |bytes: &[u8]| String::from_utf8_lossy(bytes).trim().to_string();
    if out.status.success() {
        Ok(said(&out.stdout))
    } else {
        Err(format!("it fails ({}): {}", out.status, said(&out.stderr)))
    }
}

/// The linker of the kernel and of user-space programs
/// (`components::USER_LINKER`): LLVM's, which comes with Rust's toolchain,
/// beside the host's libraries.
fn rust_lld() -> Result<PathBuf> {
    let libdir = PathBuf::from(tool_version(std::process::Command::new("rustc").args(["--print", "target-libdir"]))?);
    let lld = libdir.parent().unwrap_or(&libdir).join("bin").join(components::USER_LINKER);
    if lld.is_file() {
        Ok(lld)
    } else {
        Err(format!("{} not found (it comes with Rust's toolchains from rustup)", lld.display()))
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = match args.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => ("help", &[][..]),
    };
    // Before anything else, Cargo is made to build the crates in
    // implementation/ports/ as Veda patches them (see crates.rs), but for
    // the commands that build nothing; `doctor` reports on them.
    let prepared = match cmd {
        "help" | "--help" | "-h" | "clean" | "doctor" => Ok(()),
        _ => crates::prepare(),
    };
    let result = prepared.and_then(|()| match cmd {
        "build" => cli::parse_options(rest).and_then(|o| runner::build(&o).map(|_| ())),
        "iso" => cli::parse_options(rest).and_then(|o| iso(&o)),
        "run" => cli::parse_options(rest).and_then(|o| run(&o)),
        "shot" => cli::parse_options(rest).and_then(|o| runner::shot(&o)),
        "test" => cli::parse_options(rest).and_then(|o| runner::test(&o)),
        "scripts" => {
            let files: Vec<PathBuf> = rest.iter().filter(|a| a.ends_with(".vts")).map(PathBuf::from).collect();
            if files.is_empty() {
                Err("usage: cargo automation scripts FILE... [options]".into())
            } else {
                cli::parse_options(&rest.iter().filter(|a| !a.ends_with(".vts")).cloned().collect::<Vec<_>>())
                    .and_then(|o| runner::scripts(&o, &files))
            }
        }
        "script" => match rest.split_first() {
            Some((file, opts)) => cli::parse_options(opts).and_then(|o| runner::script(&o, file)),
            None => Err("usage: cargo automation script FILE [options]".into()),
        },
        "acpi" => acpi::command(rest),
        "toolchain" => toolchain::command(rest),
        "clippy" => lint::command(rest),
        "linux" => linux::command(rest),
        "clean" => util::run(util::cargo().arg("clean")),
        "doctor" => doctor(),
        "help" | "--help" | "-h" => {
            print!("{HELP}");
            Ok(())
        }
        other => Err(format!("unknown command '{other}'\n\n{HELP}")),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("\x1b[1;31merror:\x1b[0m {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_that_fails_is_no_version() {
        assert!(tool_version(std::process::Command::new("sh").args(["-c", "echo 1.0"])).is_ok_and(|v| v == "1.0"));
        let failing = tool_version(std::process::Command::new("sh").args(["-c", "echo broken >&2; exit 3"]));
        assert!(failing.is_err_and(|e| e.contains("broken")));
        assert!(tool_version(&mut std::process::Command::new("/nonexistent/rustc")).is_err());
    }
}

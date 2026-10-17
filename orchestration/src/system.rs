// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The system: what goes into the image (the programs, the generated
//! media, the toolchain's programs and tests, the driver VM's Linux), its
//! boot configuration, and the images it is written as (a disk, the live
//! system's ISO, a GUI script's overlay of a disk).
//!
//! The media generators (`generation/`) run when their executable has
//! changed since they last ran (it holds every library they use, the fonts
//! they set text in too): each writes into a directory of its own and
//! lists what it wrote in a manifest, and the image takes what the
//! manifests list. What tells whether a generator has run with its
//! executable (its stamp) is kept beside its directory, out of the image.

use std::path::{Path, PathBuf};

use crate::arch;
use crate::cli::Options;
use crate::components;
use crate::image;
use crate::linux;
use crate::ports;
use crate::toolchain;
use crate::util::{self, Result};

/// The media generators (`generation/`), whose output goes into the image
/// at the same relative paths.
const GENERATORS: [&str; 3] = ["assetgen", "musicgen", "videogen"];

/// The lock over the generated media, which builds at once share.
fn media_lock() -> PathBuf {
    util::generated_dir().join(".lock")
}

/// Where a generator writes, and where it notes the executable that last
/// wrote it.
fn media_dir(generator: &str) -> PathBuf {
    util::generated_dir().join(generator)
}

fn stamp(generator: &str) -> PathBuf {
    util::generated_dir().join(format!("{generator}.stamp"))
}

/// Builds the generators and runs those whose executable is not the one
/// that wrote their media.
fn generate_media() -> Result {
    util::status("Building", "the media generators");
    let mut build = util::cargo();
    build.args(["build", "--quiet", "--release"]);
    for g in GENERATORS {
        build.args(["--package", g]);
    }
    util::run(&mut build)?;
    for g in GENERATORS {
        let exe = util::target_dir().join("release").join(g);
        let digest = util::sha256_file(&exe)?;
        let dir = media_dir(g);
        if std::fs::read_to_string(stamp(g)).is_ok_and(|s| s == digest) && gencommon::output::manifest(&dir)?.is_some()
        {
            continue;
        }
        util::status("Generating", format!("media with {g}"));
        util::remove_file(&stamp(g))?;
        util::run(std::process::Command::new(&exe).arg(&dir).current_dir(util::workspace_root()))?;
        util::write_atomically(&stamp(g), &digest)?;
    }
    Ok(())
}

/// The generated media for the image: each file's path in it, and where
/// it is (what the generators' manifests list).
fn media_files() -> Result<Vec<(String, PathBuf)>> {
    let mut files = Vec::new();
    for g in GENERATORS {
        let dir = media_dir(g);
        match gencommon::output::manifest(&dir)? {
            Some(listed) => {
                for rel in listed {
                    if rel.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
                        return Err(format!("{}: {rel} is not a path in its directory", dir.display()));
                    }
                    files.push((rel.clone(), dir.join(rel)));
                }
            }
            None => util::status(
                "Note",
                format!("no media of {g}'s in the image (it has not run: build without --no-generate)"),
            ),
        }
    }
    files.sort();
    Ok(files)
}

/// The contents of the boot disk apart from its configuration file.
pub struct System {
    pub bootloader: Vec<u8>,
    pub kernel: Vec<u8>,
    pub initrd: Vec<u8>,
}

/// Builds all components and packs the initrd.
pub fn build(o: &Options) -> Result<System> {
    let artifacts = components::build_all(o.profile, &o.skip)?;

    util::status("Packing", "initrd");
    let mut initrd = initrd::builder::Builder::new();
    for (program, path) in &artifacts.programs {
        // A program on the standard library is a POSIX program, which the C
        // library starts; one on `rt` alone carries its note.
        let kind = if program.std { elf::Kind::Posix } else { elf::Kind::Veda };
        initrd.add(&format!("bin/{}", program.binary), shipped(path, Some(kind))?);
    }
    // What the toolchain makes for the image: the C and Rust test programs,
    // the C toolchain itself (gcc, as, ld, the C library and its headers, as
    // /system has them), the media decoder and its libraries' tests.
    let toolchain = toolchain::image_files(!o.skip.iter().any(|s| s == "mediadec"))?;
    for (path, data) in toolchain {
        initrd.add(&path, data);
    }
    // Videos for the stand-in decoder, fakedec (the GUI scripts').
    let clips = util::workspace_root().join("verification/fakedec/clips");
    for path in util::files_under(&clips)? {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if name != "README.md" {
            initrd.add(&format!("tests/videos/fake/{name}"), util::read(&path)?);
        }
    }
    // The driver VM's Linux (`cargo orchestration linux`, or the first time
    // an image needs it) and its programs.
    linux::build_once()?;
    match linux::guest()? {
        Some((kernel, initramfs)) => {
            initrd.add("linux/kernel", kernel);
            initrd.add("linux/initramfs.cpio", initramfs);
        }
        None => util::status(
            "Note",
            "no driver VM in the image: nothing but the disks and sound is driven \
             (`cargo orchestration linux` builds its Linux)",
        ),
    }
    initrd.add("etc/version", format!("Veda {}\n", env!("CARGO_PKG_VERSION")).into_bytes());
    // The licence, which About Veda refers to.
    initrd.add("LICENSE.txt", util::read(&util::workspace_root().join("LICENSE"))?);
    // The fonts, with their licence (implementation/ports/fonts/X ->
    // /system/fonts/X); README files are documentation.
    let fonts = ports::dir().join("fonts");
    for path in util::files_under(&fonts)? {
        if path.file_name().is_some_and(|n| n != "README.md") {
            let rel = path.strip_prefix(&fonts).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            initrd.add(&format!("fonts/{rel}"), util::read(&path)?);
        }
    }
    // The generated media, at their relative paths: made (where their
    // generators changed) and read while no other build makes them.
    {
        let _media = if o.generate {
            let lock = util::Lock::exclusive(&media_lock(), "the generated media")?;
            generate_media()?;
            lock
        } else {
            util::Lock::shared(&media_lock(), "the generated media")?
        };
        for (rel, path) in media_files()? {
            initrd.add(&rel, util::read(&path)?);
        }
    }
    // The applications' manifests, which the start menu lists
    // (implementation/applications/<name>/<name>.app ->
    // /system/apps/<name>.app).
    let applications = util::workspace_root().join("implementation").join("applications");
    let entries = std::fs::read_dir(&applications).map_err(|e| format!("reading {}: {e}", applications.display()))?;
    for app in entries.flatten() {
        let name = format!("{}.app", app.file_name().to_string_lossy());
        let manifest = app.path().join(&name);
        if manifest.is_file() {
            initrd.add(&format!("apps/{name}"), util::read(&manifest)?);
        }
    }
    let initrd = initrd.build();
    let bootloader = util::read(&artifacts.bootloader)?;
    let kernel = shipped(&artifacts.kernel, None)?;
    Ok(System { bootloader, kernel, initrd })
}

/// The executable at `path` as the system ships it, checked: of `kind`, if
/// given (a Veda program has `rt`'s note, a POSIX program none), and
/// without its symbols and section headers (`elf::strip`), which the
/// build's own copy keeps for debugging.
fn shipped(path: &Path, kind: Option<elf::Kind>) -> Result<Vec<u8>> {
    let mut file = util::read(path)?;
    let invalid = |e: elf::ElfError| format!("{}: {e}", path.display());
    let found = elf::Elf::parse_for(&file, arch::MACHINE).map_err(invalid)?.kind();
    match kind {
        Some(elf::Kind::Veda) if found != elf::Kind::Veda => {
            return Err(format!("{}: not a Veda program (it lacks the note `rt` gives one)", path.display()));
        }
        Some(elf::Kind::Posix) if found != elf::Kind::Posix => {
            return Err(format!(
                "{}: a program on the standard library, but marked as a Veda program (by `rt`'s runtime)",
                path.display()
            ));
        }
        _ => {}
    }
    let len = elf::strip(&mut file, arch::MACHINE).map_err(invalid)?;
    file.truncate(len);
    Ok(file)
}

/// The screen of the GUI scripts, of the shots and of the image `build`
/// writes: the scripts' clicks and the README's pictures are laid out for
/// it.
pub const SCRIPTED_SCREEN: (u32, u32) = (1280, 800);

/// The screen of `run`'s window, and of the live system, which takes a
/// PC's largest mode up to it.
pub const LARGE_SCREEN: (u32, u32) = (1920, 1080);

/// The screen resolution a boot asks for: `--resolution`, else the live
/// system's or the scripts' (`run` asks for [`LARGE_SCREEN`] itself).
/// QEMU's display has it for its own size (`qemu::VmConfig::screen`).
pub fn screen(o: &Options, live: bool) -> (u32, u32) {
    o.resolution.unwrap_or(if live { LARGE_SCREEN } else { SCRIPTED_SCREEN })
}

/// The boot configuration of `o` (screen resolution and kernel command
/// line), for the disk image or for the live system. The loader takes a
/// command line of [`bootinfo::CMDLINE_MAX`] bytes at most: a longer one is
/// an error, not one cut short.
pub fn boot_config(o: &Options, live: bool) -> Result<String> {
    let tz = if live { None } else { util::host_utc_offset() };
    let cmdline = cmdline(&o.cmdline, live, tz.as_deref())?;
    let (w, h) = screen(o, live);
    Ok(format!("# Veda boot configuration (read by the UEFI loader)\nresolution={w}x{h}\ncmdline={cmdline}\n"))
}

/// The kernel command line of `given`: on the live system with `live`
/// first; elsewhere with the host's time zone `tz`, unless it gives one.
fn cmdline(given: &str, live: bool, tz: Option<&str>) -> Result<String> {
    let mut words: Vec<String> = given.split_whitespace().map(String::from).collect();
    if live {
        // The live system runs from memory and leaves the PC's disks alone.
        // A PC's clock keeps local time under Windows, so it shows no time
        // zone.
        words.insert(0, "live".into());
    } else if let Some(tz) = tz
        && !words.iter().any(|a| a.starts_with("tz="))
    {
        // The machine's clock keeps UTC; local time is the host's, unless the
        // command line says otherwise.
        words.push(format!("tz={tz}"));
    }
    let cmdline = words.join(" ");
    if cmdline.len() > bootinfo::CMDLINE_MAX {
        return Err(format!(
            "the kernel command line is {} bytes, more than the {} the loader takes (bootinfo::CMDLINE_MAX): {cmdline}",
            cmdline.len(),
            bootinfo::CMDLINE_MAX
        ));
    }
    Ok(cmdline)
}

/// The boot partition's files for `system` with `boot_cfg`.
fn esp_contents<'a>(system: &'a System, boot_cfg: &'a str) -> image::EspContents<'a> {
    image::EspContents { bootloader: &system.bootloader, kernel: &system.kernel, initrd: &system.initrd, boot_cfg }
}

/// Writes the disk image of `system` with the boot configuration of `o`
/// to `path`; returns its size.
pub fn write_image(o: &Options, system: &System, path: &Path) -> Result<u64> {
    let boot_cfg = boot_config(o, false)?;
    if let Some(dir) = path.parent() {
        util::create_dir_all(dir)?;
    }
    util::status("Imaging", path.display());
    image::write_disk_image(path, &esp_contents(system, &boot_cfg))
}

/// Writes the live system's ISO image of `system` to `path` (see
/// `image::iso9660`); returns its size.
pub fn write_iso(o: &Options, system: &System, path: &Path) -> Result<u64> {
    let boot_cfg = boot_config(o, true)?;
    if let Some(dir) = path.parent() {
        util::create_dir_all(dir)?;
    }
    util::status("Imaging", path.display());
    image::write_iso_image(path, &esp_contents(system, &boot_cfg))
}

/// Where `build` writes the disk image.
pub fn disk_image() -> PathBuf {
    util::out_dir().join("veda.img")
}

/// Where `iso` writes the live system's ISO image unless told otherwise.
pub fn iso_image() -> PathBuf {
    util::out_dir().join("veda.iso")
}

/// The stick `run --live` and the scripts boot the live system from: an
/// image of their own, which `live writable` lets the guest write its logs
/// into, so that `iso`'s stays as it was written.
pub fn live_stick() -> PathBuf {
    util::out_dir().join("stick.iso")
}

/// Where `scripts` and `test` keep the system they built, for the scripts'
/// runs (`--prebuilt`): its parts, and a disk image the scripts' overlays
/// are of.
pub fn system_dir() -> PathBuf {
    util::out_dir().join("system")
}

/// The parts of a system, as `save` keeps them.
const SYSTEM_PARTS: [&str; 3] = ["bootloader.efi", "kernel.elf", "initrd.img"];

/// The disk image a saved system keeps beside its parts.
pub const SAVED_IMAGE: &str = "veda.img";

/// Keeps `system` in `dir`, for the scripts' runs: its parts, and its disk
/// image with the boot configuration of `o`.
pub fn save(o: &Options, system: &System, dir: &Path) -> Result {
    util::create_dir_all(dir)?;
    for (name, data) in SYSTEM_PARTS.iter().zip([&system.bootloader, &system.kernel, &system.initrd]) {
        util::write_atomically(&dir.join(name), data)?;
    }
    write_image(o, system, &dir.join(SAVED_IMAGE)).map(drop)
}

/// The system `save` kept in `dir`.
pub fn load(dir: &Path) -> Result<System> {
    let [bootloader, kernel, initrd] = SYSTEM_PARTS.map(|name| util::read(&dir.join(name)));
    Ok(System { bootloader: bootloader?, kernel: kernel?, initrd: initrd? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_fits_the_loader() {
        assert_eq!(cmdline("run=about", false, Some("+02:00")).unwrap(), "run=about tz=+02:00");
        assert_eq!(cmdline("run=about  tz=-05:00", false, Some("+02:00")).unwrap(), "run=about tz=-05:00");
        assert_eq!(cmdline("run=about", true, Some("+02:00")).unwrap(), "live run=about");
        assert_eq!(cmdline("", false, None).unwrap(), "");
        // The loader takes 256 bytes: up to that, and no more.
        let at_most = format!("a={}", "x".repeat(bootinfo::CMDLINE_MAX - 2));
        assert_eq!(cmdline(&at_most, false, None).unwrap().len(), bootinfo::CMDLINE_MAX);
        let error = cmdline(&format!("{at_most} b"), false, None).unwrap_err();
        assert!(error.contains("258 bytes, more than the 256"), "{error}");
        // The time zone the build adds counts too.
        assert!(cmdline(&at_most, false, Some("+01:00")).is_err());
    }
}

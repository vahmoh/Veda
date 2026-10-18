// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Linux guest of the driver VM (see `documentation/DRIVERVM.md`).
//!
//! * `cargo automation linux` builds its kernel from
//!   `implementation/ports/linux` (first zlib and elfutils' libelf, from
//!   `implementation/ports/zlib` and `implementation/ports/elfutils`, for
//!   the kernel's build tool objtool): `target/linux/kernel`, the
//!   architecture's image of it. It also builds a cross toolchain for the
//!   guest's programs in C and C++ (`target/linux/toolchain`, for Linux on
//!   musl) and configures Mesa's build with it for the renderer, and for
//!   this machine (`vgallium.so`, the renderer's decoder on softpipe, virgl
//!   and iris, for the OpenGL ES tests).
//! * Once it is built, every image build also builds the guest's programs
//!   (`implementation/guests/linux/`: Rust for Linux on musl, static; the
//!   renderer, Mesa's build around its Rust half) and packs them into the
//!   initial RAM file system the kernel starts with, with the firmware its
//!   drivers load (`implementation/ports/linux/firmware.txt`, and the
//!   architecture's in
//!   `implementation/ports/linux/architectures/<architecture>/`). Both go
//!   into the system image (`linux/kernel`, `linux/initramfs.cpio`), where
//!   `drivervm` finds them.
//!
//! The renderer's Rust half goes to Mesa's build through a place of the
//! Linux build's own (`build/librenderer.a`), where every build copies the
//! library it has just built: Mesa links that, whatever target directory
//! made it. The Linux build is one, which builds at once share: what
//! changes it (`cargo automation linux`, Mesa's builds) holds its lock.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch::{self, GUEST_TARGET};
use crate::notices;
use crate::ports::{self, Port};
use crate::util::{self, Result};

/// The guest's programs: the package, its binary, and where it goes in the
/// initial RAM file system.
const GUEST_PROGRAMS: &[(&str, &str, &str)] = &[
    ("guest-init", "init", "init"),
    ("guest-bridgetest", "bridgetest", "bin/bridgetest"),
    ("guest-pcitest", "pcitest", "bin/pcitest"),
    ("guest-gpiotest", "gpiotest", "bin/gpiotest"),
    ("guest-efivartest", "efivartest", "bin/efivartest"),
    ("guest-kmspause", "kmspause", "bin/kmspause"),
    ("guest-logkeeper", "logkeeper", "bin/logkeeper"),
    ("guest-alsa", "alsa", "bin/alsa"),
    ("guest-net", "net", "bin/net"),
    ("guest-wifi", "wifi", "bin/wifi"),
    ("guest-kms", "kms", "bin/kms"),
    ("guest-input", "input", "bin/input"),
];

/// Cargo, for the driver VM's programs (`implementation/guests/linux/`),
/// running `args`: in their directory, so that their configuration is read
/// (`implementation/guests/linux/.cargo/config.toml`: musl, and
/// `veda_guest`), and into a target directory of their own, as their target
/// is that of Veda's programs ([`guest_target_dir`]).
pub fn guest_cargo(args: &[&str]) -> Command {
    let mut cmd = util::cargo_for(GUEST_TARGET);
    cmd.current_dir(util::workspace_root().join("implementation").join("guests").join("linux"));
    cmd.args(args).arg("--target-dir").arg(guest_target_dir());
    cmd
}

/// Where the driver VM's programs are built (what their configuration says
/// for Cargo run by hand, beside `$CARGO_TARGET_DIR`'s other builds).
fn guest_target_dir() -> PathBuf {
    util::target_dir().join("guest")
}

/// Tools the build runs: the kernel's, beyond a C compiler; GCC's (m4, and
/// the GMP, MPFR and MPC it links with); meson and ninja for Mesa.
const BUILD_TOOLS: [&str; 14] =
    ["gcc", "g++", "make", "m4", "flex", "bison", "bc", "perl", "tar", "xz", "patch", "curl", "meson", "ninja"];

/// Where the guest's kernel is built (`$VEDA_LINUX` overrides it).
pub fn root() -> PathBuf {
    util::env_dir("VEDA_LINUX", &["target", "linux"])
}

/// The lock over the Linux build.
fn lock() -> Result<util::Lock> {
    util::Lock::exclusive(&root().join(".lock"), "the driver VM's Linux build")
}

/// The ports the Linux build is made from, and what builds them, whose
/// fingerprint `cargo automation linux` notes. Of the toolchain's ports
/// (binutils, GCC, musl) only the releases count: the guest's toolchain is
/// built from them as their authors released them, without Veda's patches.
fn ports_fingerprint() -> Result<String> {
    let dir = ports::dir();
    let mut extra = vec![dir.join("common.sh"), ports::settings()];
    extra.extend(["binutils", "gcc", "musl"].map(|name| dir.join(name).join("port.toml")));
    ports::fingerprint(&["zlib", "elfutils", "linux", "mesa"], &extra)
}

fn ports_note() -> PathBuf {
    root().join("build").join("ports-fingerprint")
}

/// The guest's kernel, once built.
fn image() -> PathBuf {
    root().join("kernel")
}

/// Whether the guest's kernel has been built.
pub fn built() -> bool {
    image().is_file()
}

/// Mesa's build directories, which `implementation/ports/linux/build.sh`
/// configures: for the guest (the renderer), and for this machine (the
/// renderer's decoder as a library, for the OpenGL ES tests).
fn mesa_build() -> PathBuf {
    root().join("build").join("mesa")
}

fn mesa_host_build() -> PathBuf {
    root().join("build").join("mesa-host")
}

/// The renderer's sources (`implementation/guests/linux/renderer`), whose
/// decoder and libdrm Mesa's builds compile.
fn renderer_source() -> Result<PathBuf> {
    let dir = util::workspace_root().join("implementation").join("guests").join("linux").join("renderer");
    std::fs::canonicalize(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// The options of Mesa's build in `dir` that name the renderer's files
/// (`veda-renderer`, its sources; `veda-renderer-lib`, its Rust half, for
/// the guest's build) and what they should be, which differs from what
/// they are where the checkout or the target directory has moved since
/// `cargo automation linux` configured the build: `-Dname=value` for
/// each that does.
fn renderer_options(info: &str, source: &Path, lib: Option<&Path>) -> Result<Vec<std::ffi::OsString>> {
    let options = serde_json::from_str::<serde_json::Value>(info).map_err(|e| e.to_string())?;
    let configured = |name: &str| {
        options
            .as_array()
            .into_iter()
            .flatten()
            .find(|option| option.get("name").and_then(serde_json::Value::as_str) == Some(name))
            .and_then(|option| option.get("value").and_then(serde_json::Value::as_str))
            .ok_or_else(|| format!("Mesa's build has no {name} option"))
    };
    let mut changes = Vec::new();
    for (name, path) in [("veda-renderer", Some(source)), ("veda-renderer-lib", lib)] {
        let Some(path) = path else { continue };
        if Path::new(configured(name)?) != path {
            let mut option = std::ffi::OsString::from(format!("-D{name}="));
            option.push(path);
            changes.push(option);
        }
    }
    Ok(changes)
}

/// Keeps the Mesa build in `dir` on the renderer's files: its sources and,
/// for the guest's build, `lib`, its Rust half (see [`renderer_options`]).
/// Mesa builds again only what they reach.
fn follow_renderer(dir: &Path, lib: Option<&Path>) -> Result {
    let info = dir.join("meson-info").join("intro-buildoptions.json");
    let text = util::read_to_string(&info)?;
    let changes = renderer_options(&text, &renderer_source()?, lib).map_err(|e| format!("{}: {e}", info.display()))?;
    if changes.is_empty() {
        return Ok(());
    }
    util::status("Configuring", format!("{} for the renderer where it is now", dir.display()));
    util::run(Command::new("meson").arg("configure").arg(dir).args(changes))
}

/// Where Mesa's build for the guest finds the renderer's Rust half: in the
/// Linux build, whatever target directory built it.
fn renderer_lib_place() -> PathBuf {
    root().join("build").join("librenderer.a")
}

/// Puts the renderer's Rust half `built` (a static library, which Mesa's
/// build links the program around) where Mesa's build finds it, unless it
/// is there already: a copy with its time, so that ninja relinks when it
/// changes, and only then.
fn place_renderer_lib(built: &Path) -> Result<PathBuf> {
    let place = renderer_lib_place();
    let stamp = |p: &Path| std::fs::metadata(p).ok().map(|m| (m.len(), m.modified().ok()));
    let source = stamp(built).ok_or_else(|| format!("{}: not built", built.display()))?;
    if stamp(&place) != Some(source) {
        util::create_dir_all(place.parent().unwrap_or(&place))?;
        let partial = place.with_extension("part");
        util::copy(built, &partial)?;
        if let (_, Some(time)) = source {
            std::fs::File::options()
                .write(true)
                .open(&partial)
                .and_then(|f| f.set_modified(time))
                .map_err(|e| format!("{}: {e}", partial.display()))?;
        }
        util::rename(&partial, &place)?;
    }
    Ok(place)
}

/// The renderer's Rust half (`implementation/guests/linux/renderer`), built.
fn renderer_lib() -> Result<PathBuf> {
    util::run(&mut guest_cargo(&["build", "--release", "--target", GUEST_TARGET, "--package", "guest-renderer"]))?;
    Ok(guest_target_dir().join(GUEST_TARGET).join("release").join("librenderer.a"))
}

/// The renderer's decoder for this machine (`vgallium.so`: on softpipe, on
/// virgl over vtest, on iris), up to date; None if Mesa has not been
/// configured for it.
pub fn vgallium() -> Result<Option<PathBuf>> {
    let dir = mesa_host_build();
    if !dir.join("build.ninja").is_file() {
        return Ok(None);
    }
    let _lock = lock()?;
    follow_renderer(&dir, None)?;
    util::status("Building", "the renderer for the host (Mesa)");
    util::ninja(&dir, "src/gallium/targets/veda/vgallium.so")?;
    Ok(Some(dir.join("src").join("gallium").join("targets").join("veda").join("vgallium.so")))
}

/// Whether this machine has an Intel GPU (a render node of i915's or
/// xe's), which Veda's renderer can be tested on.
pub fn intel_gpu() -> bool {
    std::fs::read_dir("/sys/class/drm").is_ok_and(|d| {
        d.flatten().any(|e| {
            e.file_name().to_string_lossy().starts_with("renderD")
                && std::fs::read_link(e.path().join("device/driver"))
                    .is_ok_and(|p| p.file_name().is_some_and(|f| f == "i915" || f == "xe"))
        })
    })
}

/// Whether the driver VM can run in this machine's QEMU: its Linux is
/// built, and KVM lets Veda's hypervisor run (nested virtualization).
pub fn runnable() -> Result {
    if !built() {
        return Err("its Linux is not built (`cargo automation linux`)".into());
    }
    arch::nested_virtualization()
}

/// For `cargo automation doctor`: whether the guest has been built
/// (`Ok`), and if not, whether it can be (`Err`).
pub fn status() -> std::result::Result<String, String> {
    if built() {
        return Ok(format!("built ({})", image().display()));
    }
    let how = match tools() {
        Ok(()) => "`cargo automation linux` builds it".to_string(),
        Err(e) => format!("`cargo automation linux` builds it, but {e}"),
    };
    Err(format!("not built (the driver VM drives every device but the disks and sound); {how}"))
}

fn tools() -> Result {
    let missing: Vec<&str> = BUILD_TOOLS.into_iter().filter(|t| util::find_on_path(t).is_none()).collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("{} missing: install from the distribution's packages", missing.join(", ")))
    }
}

/// `cargo automation linux [--jobs N]`.
pub fn command(args: &[String]) -> Result {
    let mut jobs = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--jobs" | "-j" => jobs = util::jobs(it.next())?,
            other => return Err(format!("unknown option '{other}' (linux takes --jobs N)")),
        }
    }
    tools()?;
    let started = std::time::Instant::now();
    let _lock = lock()?;
    let downloads = root().join("downloads");
    for name in ["zlib", "elfutils", "linux", "binutils", "gcc", "musl", "mesa"] {
        ports::fetch(&Port::named(name)?, &downloads)?;
    }
    firmware()?;
    let lib = place_renderer_lib(&renderer_lib()?)?;
    let ports = ports::dir();
    util::run(
        Command::new("bash")
            .arg(ports.join("linux").join("build.sh"))
            .env("VEDA_PORTS", &ports)
            .env("VEDA_ARCH", arch::NAME)
            .env("VEDA_LINUX", root())
            .env("VEDA_RENDERER", renderer_source()?)
            .env("VEDA_RENDERER_LIB", &lib)
            .env("JOBS", jobs.to_string()),
    )?;
    util::write(&ports_note(), ports_fingerprint()?)?;
    util::status("Finished", format!("the driver VM's Linux in {:.0} s", started.elapsed().as_secs_f32()));
    Ok(())
}

/// Says so if the guest was built from other ports than
/// `implementation/ports/` has now.
fn check_ports() {
    let now = match ports_fingerprint() {
        Ok(print) => print,
        Err(e) => return util::status("Note", format!("the driver VM's ports: {e}")),
    };
    match std::fs::read_to_string(ports_note()) {
        Ok(built) if built == now => {}
        Ok(_) => util::status(
            "Note",
            "the driver VM's Linux was built from other ports than implementation/ports/ has now: `cargo \
             automation linux` builds it again",
        ),
        Err(_) => util::status(
            "Note",
            "which ports the driver VM's Linux was built from is not known (it was built before that was noted): \
             `cargo automation linux` notes it",
        ),
    }
}

/// Builds the guest the first time an image needs it, where this machine
/// has the tools for it (afterwards `cargo automation linux` builds it
/// again, once its port changes).
pub fn build_once() -> Result {
    if built() {
        return Ok(());
    }
    match tools() {
        Ok(()) => command(&[]),
        Err(e) => {
            util::status("Note", format!("the driver VM's Linux cannot be built here: {e}"));
            Ok(())
        }
    }
}

/// What the system image has of the driver VM.
pub struct Guest {
    /// Its kernel.
    pub kernel: Vec<u8>,
    /// Its initial RAM file system: its programs and firmware.
    pub initramfs: Vec<u8>,
    /// The licence texts of what it has of others' (its kernel's, Mesa's,
    /// its firmware's), at their paths in the image (`licences/<part>/`).
    pub licences: Vec<(String, Vec<u8>)>,
    /// The packages of its programs, whose crates the image's licences
    /// list too.
    pub packages: Vec<&'static str>,
}

/// The guest for the system image, if its kernel has been built.
pub fn guest() -> Result<Option<Guest>> {
    if !built() {
        return Ok(None);
    }
    check_ports();
    util::status("Building", format!("the driver VM's programs ({GUEST_TARGET})"));
    let mut cmd = guest_cargo(&["build", "--release", "--target", GUEST_TARGET, "--package", "guest-renderer"]);
    for (package, ..) in GUEST_PROGRAMS {
        cmd.args(["--package", package]);
    }
    util::run(&mut cmd)?;
    let mut packages: Vec<&'static str> = GUEST_PROGRAMS.iter().map(|(package, ..)| *package).collect();
    // Its kernel's licence texts (linux/build.sh's `licences` stage).
    let gathered = root().join("licences");
    let mut licences = notices::port_texts(&gathered, "linux", "`cargo automation linux`")?;
    let bin = guest_target_dir().join(GUEST_TARGET).join("release");
    let mut archive = cpio::Archive::new();
    archive.directory("dev");
    archive.directory("bin");
    // The console the kernel opens for the first program, before /dev is
    // mounted.
    archive.device("dev/console", 5, 1);
    for (_, binary, path) in GUEST_PROGRAMS {
        archive.file(path, 0o755, &util::read(&bin.join(binary))?);
    }
    // The renderer: Mesa's build links its decoder and drivers around the
    // Rust half just built (once `cargo automation linux` has configured
    // it), while no other build changes the Linux build.
    if mesa_build().join("build.ninja").is_file() {
        let _lock = lock()?;
        let lib = place_renderer_lib(&bin.join("librenderer.a"))?;
        follow_renderer(&mesa_build(), Some(&lib))?;
        util::status("Building", "the driver VM's renderer (Mesa)");
        util::ninja(&mesa_build(), "src/gallium/targets/veda/renderer")?;
        let exe = mesa_build().join("src").join("gallium").join("targets").join("veda").join("renderer");
        let stripped = root().join("build").join("renderer");
        let strip = root().join("toolchain").join("bin").join(format!("{}-strip", arch::GUEST_C_TARGET));
        util::run(Command::new(strip).arg("-o").arg(&stripped).arg(&exe))?;
        archive.file("bin/renderer", 0o755, &util::read(&stripped)?);
        packages.push("guest-renderer");
        licences.extend(notices::port_texts(&gathered, "mesa", "`cargo automation linux`")?);
    }
    let firmware = firmware()?;
    let mut directories = std::collections::BTreeSet::new();
    for file in &firmware.files {
        let path = format!("lib/firmware/{}", file.name);
        // Its directories first, each once.
        for (at, _) in path.match_indices('/') {
            if directories.insert(path[..at].to_string()) {
                archive.directory(&path[..at]);
            }
        }
        archive.file(&path, 0o644, &util::read(&file.at)?);
    }
    licences.extend(firmware.licence_files()?);
    Ok(Some(Guest { kernel: util::read(&image())?, initramfs: archive.finish(), licences, packages }))
}

/// The firmware the guest's Linux loads ([`firmware`]): its files, and
/// the licences of their sources.
struct Firmware {
    files: Vec<FirmwareFile>,
    /// Each licence of the sources the files are from: its source, its
    /// path there, and where it is.
    licences: Vec<(String, String, PathBuf)>,
}

/// A file of firmware: the name the kernel asks for it by, where it is,
/// the address it was fetched from, its source and the licence of its
/// source's it comes under (its path there).
struct FirmwareFile {
    name: String,
    at: PathBuf,
    url: String,
    source: String,
    licence: String,
}

impl Firmware {
    /// Its licences' texts, for `/system/licences/firmware/<source>/<path>`,
    /// and `README` there, which says which file comes from where under
    /// which.
    fn licence_files(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let mut files = Vec::new();
        for (source, path, at) in &self.licences {
            files.push((format!("licences/firmware/{source}/{path}"), util::read(at)?));
        }
        let mut readme = String::from(
            "The firmware the driver VM's Linux loads into devices (its /lib/firmware), each file as its maker \
             released it: the address it comes from, and the licence it comes under, which allows its \
             redistribution, in the directory of its source here. linux-firmware's WHENCE gives the maker and \
             licence of each of its files.\n",
        );
        for file in &self.files {
            readme
                .push_str(&format!("\n{}\n  from {}\n  under {}/{}\n", file.name, file.url, file.source, file.licence));
        }
        files.push(("licences/firmware/README".into(), readme.into_bytes()));
        Ok(files)
    }
}

/// The lines of the firmware lists ([`firmware`]), as read: the sources
/// (each one's address, `{}` standing for a file's path in it), the
/// sources' licences (source, path, SHA-256) and the files (name, source,
/// path, SHA-256, licence), each with the list it is in.
#[derive(Default)]
struct FirmwareLists {
    sources: std::collections::BTreeMap<String, String>,
    licences: Vec<([String; 3], PathBuf)>,
    files: Vec<([String; 5], PathBuf)>,
}

impl FirmwareLists {
    /// Reads the lines of `text`, the list at `list`: a source, a licence
    /// of a source above, or a file of a source above, under a licence of
    /// its source's above it.
    fn read(&mut self, text: &str, list: &Path) -> Result {
        let sha256 = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
        let relative = |s: &str| !s.starts_with('/') && !s.split('/').any(|part| part.is_empty() || part == "..");
        for (n, line) in text.lines().enumerate() {
            let bad = |why: &str| format!("{}:{}: {why}: {line}", list.display(), n + 1);
            let words: Vec<&str> = line.split_whitespace().collect();
            match words.as_slice() {
                [] => {}
                [first, ..] if first.starts_with('#') => {}
                ["source", name, url] if url.contains("{}") => {
                    self.sources.insert(name.to_string(), url.to_string());
                }
                ["source", ..] => return Err(bad("not a source: its name, and its address with {} for a path")),
                ["licence", source, path, hash] if sha256(hash) && relative(path) => {
                    if !self.sources.contains_key(*source) {
                        return Err(bad("a licence of a source not named above"));
                    }
                    self.licences.push(([source, path, hash].map(|w| w.to_string()), list.to_path_buf()));
                }
                ["licence", ..] => return Err(bad("not a licence: its source, its path there and its SHA-256")),
                [name, source, path, hash, licence] if sha256(hash) && relative(name) => {
                    if !self.sources.contains_key(*source) {
                        return Err(bad("a file of a source not named above"));
                    }
                    if !self.licences.iter().any(|([s, p, _], _)| s == source && p == licence) {
                        return Err(bad("a file under a licence its source has not above"));
                    }
                    self.files.push(([name, source, path, hash, licence].map(|w| w.to_string()), list.to_path_buf()));
                }
                [_, _, _, hash] if sha256(hash) => return Err(bad("a file without the licence it comes under")),
                _ => return Err(bad("not a source, a licence or a file")),
            }
        }
        Ok(())
    }
}

/// The firmware of `implementation/ports/linux/firmware.txt` and of the
/// architecture's list
/// (`implementation/ports/linux/architectures/<architecture>/firmware.txt`,
/// which may name the sources and licences of the first): each file, and
/// the licences of the sources it has files of, where they are (fetched now
/// if they are not, or are not the files pinned).
fn firmware() -> Result<Firmware> {
    let port = ports::dir().join("linux");
    let arch_list = port.join("architectures").join(arch::NAME).join("firmware.txt");
    let mut lists = FirmwareLists::default();
    for list in std::iter::once(port.join("firmware.txt")).chain(arch_list.is_file().then_some(arch_list)) {
        lists.read(&util::read_to_string(&list)?, &list)?;
    }
    let url = |source: &str, path: &str| lists.sources[source].replace("{}", path);
    let mut licences = Vec::new();
    for ([source, path, sha256], list) in &lists.licences {
        if lists.files.iter().any(|([_, s, ..], _)| s == source) {
            let at = root().join("firmware-licences").join(source).join(path);
            fetch_firmware(&at, &url(source, path), sha256, list)?;
            licences.push((source.clone(), path.clone(), at));
        }
    }
    let mut files = Vec::new();
    for ([name, source, path, sha256, licence], list) in &lists.files {
        let at = root().join("firmware").join(name);
        let url = url(source, path);
        fetch_firmware(&at, &url, sha256, list)?;
        files.push(FirmwareFile { name: name.clone(), at, url, source: source.clone(), licence: licence.clone() });
    }
    Ok(Firmware { files, licences })
}

/// Makes `file` the one of `url` whose SHA-256 is `sha256` (as `list`
/// pins it).
fn fetch_firmware(file: &Path, url: &str, sha256: &str, list: &Path) -> Result {
    if file.is_file() && ports::sha256_of(file)? == sha256 {
        return Ok(());
    }
    util::status("Downloading", url);
    util::create_dir_all(file.parent().unwrap_or(file))?;
    let partial = file.with_extension("part");
    util::run(&mut ports::curl(url, &partial))?;
    let got = util::sha256_file(&partial)?;
    if got != sha256 {
        util::remove_file(&partial)?;
        return Err(format!("{url}: SHA-256 {got}, expected {sha256} ({})", list.display()));
    }
    util::rename(&partial, file)
}

/// Archives in the `newc` format of cpio, as Linux unpacks its initial RAM
/// file system.
pub mod cpio {
    /// An archive being written.
    pub struct Archive {
        bytes: Vec<u8>,
        inode: u32,
    }

    const DIRECTORY: u32 = 0o040_000;
    const FILE: u32 = 0o100_000;
    const CHARACTER_DEVICE: u32 = 0o020_000;

    impl Archive {
        pub fn new() -> Archive {
            Archive { bytes: Vec::new(), inode: 1 }
        }

        pub fn directory(&mut self, path: &str) {
            self.entry(path, DIRECTORY | 0o755, &[], (0, 0));
        }

        pub fn file(&mut self, path: &str, permissions: u32, data: &[u8]) {
            self.entry(path, FILE | permissions, data, (0, 0));
        }

        pub fn device(&mut self, path: &str, major: u32, minor: u32) {
            self.entry(path, CHARACTER_DEVICE | 0o600, &[], (major, minor));
        }

        fn entry(&mut self, path: &str, mode: u32, data: &[u8], (major, minor): (u32, u32)) {
            let fields = [
                self.inode,
                mode,
                0, // uid
                0, // gid
                1, // links
                0, // mtime
                data.len() as u32,
                0, // the device it is on
                0,
                major,
                minor,
                path.len() as u32 + 1,
                0, // checksum
            ];
            self.inode += 1;
            self.bytes.extend_from_slice(b"070701");
            for f in fields {
                self.bytes.extend_from_slice(format!("{f:08X}").as_bytes());
            }
            self.bytes.extend_from_slice(path.as_bytes());
            self.bytes.push(0);
            self.pad();
            self.bytes.extend_from_slice(data);
            self.pad();
        }

        fn pad(&mut self) {
            while !self.bytes.len().is_multiple_of(4) {
                self.bytes.push(0);
            }
        }

        /// The archive, with its trailer.
        pub fn finish(mut self) -> Vec<u8> {
            self.entry("TRAILER!!!", 0, &[], (0, 0));
            self.bytes
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn entries_are_aligned() {
            let mut a = Archive::new();
            a.directory("dev");
            a.device("dev/console", 5, 1);
            a.file("init", 0o755, b"#!/bin/sh\n");
            let bytes = a.finish();
            assert!(bytes.len().is_multiple_of(4));
            // The first header, and the directory's name after it.
            assert_eq!(&bytes[..6], b"070701");
            assert_eq!(&bytes[14..22], b"000041ED");
            assert_eq!(&bytes[110..114], b"dev\0");
            // The console's header, after the directory's (110 bytes and
            // its name, to a multiple of four), with its device numbers.
            let header = &bytes[116..];
            assert_eq!(&header[..6], b"070701");
            assert_eq!(&header[78..86], b"00000005");
            assert_eq!(&header[86..94], b"00000001");
            assert!(bytes.windows(10).any(|w| w == b"TRAILER!!!"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Meson's description of a build's options, as `intro-buildoptions.json`
    /// has it.
    fn info(source: &str, lib: &str) -> String {
        serde_json::json!([
            { "name": "buildtype", "value": "release" },
            { "name": "veda-renderer", "value": source },
            { "name": "veda-renderer-lib", "value": lib },
        ])
        .to_string()
    }

    #[test]
    fn firmware_comes_under_the_licences_of_its_sources() {
        let list = Path::new("firmware.txt");
        let hash = "ab".repeat(32);
        let text = format!(
            "# A list\nsource fw https://example.org/{{}}?h=1\nsource db https://example.org/db/{{}}\n\
             licence  fw  WHENCE  {hash}\nlicence  fw  LICENSES/LICENCE.a  {hash}\nlicence  db  LICENSE  {hash}\n\n\
             a/b.bin  fw  a/b-1.bin  {hash}  LICENSES/LICENCE.a\n"
        );
        let mut lists = FirmwareLists::default();
        lists.read(&text, list).unwrap();
        assert_eq!(lists.licences.len(), 3);
        assert_eq!(lists.files[0].0, ["a/b.bin", "fw", "a/b-1.bin", hash.as_str(), "LICENSES/LICENCE.a"]);
        for (line, why) in [
            (format!("c.bin  fw  c.bin  {hash}"), "without the licence"),
            (format!("c.bin  fw  c.bin  {hash}  LICENSE"), "its source has not above"),
            (format!("c.bin  other  c.bin  {hash}  WHENCE"), "source not named above"),
            (format!("../c.bin  fw  c.bin  {hash}  WHENCE"), "not a source, a licence or a file"),
            (format!("licence  other  LICENSE  {hash}"), "source not named above"),
            (format!("licence  fw  ../LICENSE  {hash}"), "not a licence"),
        ] {
            let error = FirmwareLists::default().read(&format!("{text}{line}\n"), list).unwrap_err();
            assert!(error.contains(why) && error.contains("firmware.txt:9:"), "{line}: {error}");
        }
    }

    /// Veda's lists: every file under a licence of its source's.
    #[test]
    fn the_firmware_lists_name_each_files_licence() {
        let port = ports::dir().join("linux");
        let mut lists = FirmwareLists::default();
        for list in [port.join("firmware.txt"), port.join("architectures").join(arch::NAME).join("firmware.txt")] {
            lists.read(&util::read_to_string(&list).unwrap(), &list).unwrap();
        }
        assert!(!lists.files.is_empty());
        // linux-firmware's WHENCE comes with its files.
        assert!(lists.licences.iter().any(|([source, path, _], _)| source == "linux-firmware" && path == "WHENCE"));
    }

    #[test]
    fn mesa_follows_the_renderer_where_it_is() {
        let (source, lib) = (Path::new("/veda/renderer"), Path::new("/veda/target/linux/build/librenderer.a"));
        let same = info("/veda/renderer", "/veda/target/linux/build/librenderer.a");
        assert!(renderer_options(&same, source, Some(lib)).unwrap().is_empty());
        // Configured with the library another target directory built: the
        // guest's build is told where it is now; the host's has none.
        let other = info("/veda/renderer", "/home/a/old-target/guest/release/librenderer.a");
        assert_eq!(
            renderer_options(&other, source, Some(lib)).unwrap(),
            ["-Dveda-renderer-lib=/veda/target/linux/build/librenderer.a"]
        );
        assert!(renderer_options(&other, source, None).unwrap().is_empty());
        let moved = info("/old/renderer", "/old/librenderer.a");
        assert_eq!(renderer_options(&moved, source, Some(lib)).unwrap().len(), 2);
        assert!(renderer_options("[]", source, None).is_err());
    }

    #[test]
    fn the_renderer_library_is_copied_once_with_its_time() {
        let dir = std::env::temp_dir().join(format!("automation-renderer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        util::create_dir_all(&dir).unwrap();
        let built = dir.join("librenderer.a");
        util::write(&built, b"first").unwrap();
        // SAFETY: no other thread of this test program reads VEDA_LINUX.
        unsafe { std::env::set_var("VEDA_LINUX", dir.join("linux")) };
        let place = place_renderer_lib(&built).unwrap();
        unsafe { std::env::remove_var("VEDA_LINUX") };
        assert_eq!(place, dir.join("linux/build/librenderer.a"));
        assert_eq!(std::fs::read(&place).unwrap(), b"first");
        let time = |p: &Path| std::fs::metadata(p).unwrap().modified().unwrap();
        assert_eq!(time(&place), time(&built));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

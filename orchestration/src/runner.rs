// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Running the tests: the host's unit tests (every crate's that has some:
//! see [`HOST_TESTED`]), the in-system tests, and the automation scripts,
//! each in a QEMU of its own, several at once, each within a deadline.
//!
//! A script's run boots the system its run built: `scripts` and `test`
//! build it once, keep it (`system::save`), and give every script an
//! overlay of its disk image with the script's own boot configuration
//! (`image::write_boot_overlay`), so that no run copies the image.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::agentsim::AgentSim;
use crate::arch;
use crate::automate;
use crate::cli::{self, Options};
use crate::hostweb::HostWeb;
use crate::image;
use crate::linux;
use crate::mic::MicServer;
use crate::qemu::{self, QemuInstall};
use crate::script::{Audio, Live, Script, Step};
use crate::system::{self, System};
use crate::toolchain;
use crate::util::{self, Result};

/// Crates with host unit tests, which `test` runs in one Cargo invocation,
/// with the features their tests need. Every crate of the workspace with
/// tests is here, among [`HOST_TESTED_ALONE`], [`GUEST_TESTED`] or
/// [`NOT_TESTED`] (a test checks it).
const HOST_TESTED: &[(&str, &[&str])] = &[
    ("vabi", &[]),
    ("bootinfo", &[]),
    ("velf", &[]),
    ("vposix", &[]),
    ("mediadec", &[]),
    ("initrd", &["std"]),
    ("vheap", &[]),
    ("vipc", &[]),
    ("vraster", &[]),
    ("vfont", &[]),
    ("vimage", &["encode"]),
    ("vaudio", &[]),
    ("vproto", &[]),
    ("vvirtio", &[]),
    ("vgfx", &[]),
    ("vui", &[]),
    ("vtext", &[]),
    ("vfiles", &["thumbnails"]),
    ("ventropy", &[]),
    ("vnetstack", &[]),
    ("vtls", &[]),
    ("vweb", &[]),
    ("vagent", &[]),
    ("vwlan", &[]),
    ("vacpi", &[]),
    ("vgpio", &[]),
    ("vsplash", &[]),
    ("vglsl", &[]),
    ("vgl", &[]),
    ("vhv", &[]),
    ("drivervm-core", &[]),
    ("viommu", &[]),
    ("interceptor-game", &[]),
    ("videos-core", &[]),
    ("terminal-core", &[]),
    ("apps-common", &[]),
    ("music-core", &[]),
    ("calculator-core", &[]),
    ("paint-core", &[]),
    ("compositor-core", &[]),
    ("vfs-core", &[]),
    ("gencommon", &[]),
    ("assetgen", &[]),
    ("musicgen", &[]),
    ("videogen", &[]),
    ("orchestration", &[]),
];

/// Crates with host unit tests that run in a Cargo invocation of their
/// own: `vrt`'s tests turn on its default `runtime` feature, which Cargo
/// would otherwise unify into the build of `vrt` that the POSIX layer's
/// tests link (a second panic handler and allocator beside `std`'s).
const HOST_TESTED_ALONE: &[&str] = &["vrt"];

/// The driver VM's programs with tests, which run on Linux, as the
/// programs do (`implementation/guest/`, on musl).
const GUEST_TESTED: &[&str] = &["guest-input", "guest-alsa", "guest-kms", "guest-logkeeper", "guest-sys", "guest-wifi"];

/// Crates with tests that `test` does not run, each with the reason: one
/// that builds for none of the host's targets (only for a Veda target)
/// would be here. There is none.
const NOT_TESTED: &[(&str, &str)] = &[];

/// The command that runs the host's unit tests.
fn host_tests() -> std::process::Command {
    let mut cmd = util::cargo();
    cmd.args(["test", "--quiet"]);
    for (package, _) in HOST_TESTED {
        cmd.args(["--package", package]);
    }
    let features: Vec<String> = HOST_TESTED
        .iter()
        .flat_map(|(package, features)| features.iter().map(move |f| format!("{package}/{f}")))
        .collect();
    if !features.is_empty() {
        cmd.args(["--features", &features.join(",")]);
    }
    cmd
}

/// `cargo orchestration test`.
pub fn test(o: &Options) -> Result {
    // The GUI scripts' pointers and networks are the driver VM's.
    if o.ui
        && let Err(why) = linux::runnable()
    {
        return Err(format!("the GUI scripts need the driver VM: {why}"));
    }
    // Every script, checked before anything is built.
    let scripts = if o.ui { parse_all(&ui_scripts()?)? } else { Vec::new() };
    util::status("Testing", "unit tests on the host");
    util::run(&mut host_tests())?;
    for package in HOST_TESTED_ALONE {
        util::run(util::cargo().args(["test", "--quiet", "--package", package]))?;
    }
    for (package, why) in NOT_TESTED {
        util::status("Note", format!("{package}'s tests do not run: {why}"));
    }
    // The driver VM's programs' logic (touchpads, sound cards' devices, a
    // display's mode and where pictures go on it, the logs a stick keeps,
    // network interfaces and Wi-Fi's frames, devices served again after
    // their threads end), on Linux as they run.
    let mut guest = linux::guest_cargo(&["test", "--quiet", "--target", arch::GUEST_TARGET]);
    for package in GUEST_TESTED {
        guest.args(["--package", package]);
    }
    util::run(&mut guest)?;
    opengl_tests()?;
    // One build serves every boot below.
    let started = Instant::now();
    let system = system::build(o)?;
    util::status("Built", format!("the system in {:.1}s", started.elapsed().as_secs_f32()));
    system::save(o, &system, &system::system_dir())?;
    drop(system);
    util::status("Testing", "integration tests inside Veda");
    let mut systest = o.clone();
    systest.cmdline = format!("{} systest", o.cmdline).trim().to_string();
    // The last test restarts the window system; give the desktop a moment
    // to come back before the screenshot.
    let checks = Script::parse(
        "fail-on \"systest: FAIL\"\nwait-serial \"systest: PASS\" 600\nwait 3\nshot target/veda/test-desktop.png\n",
    )?;
    script_on(&systest, &checks, Some(&system::system_dir()))?;
    if o.ui {
        run_scripts(o, scripts)?;
    }
    util::status("Passed", "all tests");
    Ok(())
}

/// The OpenGL ES tests on the host's GPU and through Veda's renderer, where
/// this machine has them.
fn opengl_tests() -> Result {
    // Tests of OpenGL ES on the host's GPU drive the system's virglrenderer,
    // which QEMU's 3D GPU is built on, where QEMU has one.
    let virgl = QemuInstall::locate().is_ok_and(|q| q.has_gl_gpu());
    // On QEMU's GPU, unless `VGL_TEST_RENDERNODE` names another.
    let node = std::env::var("VGL_TEST_RENDERNODE").ok().filter(|n| !n.is_empty()).or_else(qemu::render_node);
    let vgl = |package: &str| {
        let mut cmd = util::cargo();
        cmd.args(["test", "--quiet", "--package", package]);
        cmd
    };
    // Through Veda's renderer, where it has been built: on softpipe; on
    // virgl over virglrenderer's test server, as the driver VM's renderer
    // renders under QEMU; and on this machine's Intel GPU (iris), as it
    // renders on a PC's.
    if let Some(dll) = linux::vgallium()? {
        // As OpenGL hosts keep depth, then as iris does (VR_DEPTH_LOW).
        for (depth, how) in [("0", ""), ("1", ", depth kept as on iris")] {
            util::status("Testing", format!("OpenGL ES through Veda's renderer (Mesa's softpipe{how})"));
            util::run(
                vgl("vgl").env("VGL_TEST_BACKEND", "gallium").env("VGL_GALLIUM_DLL", &dll).env("VR_DEPTH_LOW", depth),
            )?;
        }
        if let Some(server) = util::find_on_path("virgl_test_server") {
            util::status("Testing", "OpenGL ES through Veda's renderer (Mesa's virgl, on virglrenderer)");
            let socket = util::out_dir().join("vtest.sock");
            util::create_dir_all(&util::out_dir())?;
            util::remove_file(&socket)?;
            let mut run = std::process::Command::new(server);
            run.args(["--multi-clients", "--socket-path"]).arg(&socket);
            if let Some(node) = &node {
                run.arg("--rendernode").arg(node);
            }
            let server = run
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| format!("starting virgl_test_server: {e}"))?;
            let server = Killed(server);
            let started = Instant::now();
            while !socket.exists() && started.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(50));
            }
            util::run(
                vgl("vgl")
                    .env("VGL_TEST_BACKEND", "gallium")
                    .env("VGL_GALLIUM_DEVICE", "virgl")
                    .env("VGL_GALLIUM_DLL", &dll)
                    .env("VTEST_SOCKET_NAME", &socket),
            )?;
            drop(server);
        }
        if linux::intel_gpu() {
            util::status("Testing", "OpenGL ES through Veda's renderer (Mesa's iris, on this machine's Intel GPU)");
            // Interceptor's game too: its shadows, sparks and cars on the
            // GPU.
            for package in ["vgl", "interceptor-game"] {
                util::run(
                    vgl(package)
                        .env("VGL_TEST_BACKEND", "gallium")
                        .env("VGL_GALLIUM_DEVICE", "iris")
                        .env("VGL_GALLIUM_DLL", &dll),
                )?;
            }
        }
    }
    if virgl {
        // QEMU renders on desktop OpenGL, and on OpenGL ES with `gl=es`.
        for (host, what) in [("desktop", "desktop OpenGL"), ("gles", "OpenGL ES")] {
            util::status("Testing", format!("OpenGL ES on the host's GPU (QEMU's virglrenderer on {what})"));
            let mut cmd = vgl("vgl");
            cmd.env("VGL_TEST_BACKEND", "virgl").env("VGL_TEST_HOST", host);
            if let Some(node) = &node {
                cmd.env("VGL_TEST_RENDERNODE", node);
            }
            util::run(&mut cmd)?;
        }
    }
    Ok(())
}

/// A program that is killed (and waited for) when this ends.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The GUI scripts `test --ui` runs: verification/ui's, then the agent's
/// (with the stand-in for Deepgram; verification/real, which talks to the
/// real one, is left out), each group's own and those that hold only on
/// the architecture's machine (`arch/<architecture>/`).
fn ui_scripts() -> Result<Vec<PathBuf>> {
    let mut scripts = Vec::new();
    for group in ["ui", "agent"] {
        let dir = util::workspace_root().join("verification").join(group);
        for dir in [dir.clone(), dir.join("arch").join(arch::NAME)] {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            let mut found: Vec<PathBuf> =
                entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "vts")).collect();
            found.sort();
            scripts.extend(found);
        }
    }
    Ok(scripts)
}

/// Reads and checks every script of `files`; fails with what is wrong in
/// each.
fn parse_all(files: &[PathBuf]) -> Result<Vec<(PathBuf, Script)>> {
    let mut scripts = Vec::new();
    let mut errors = Vec::new();
    for path in files {
        match util::read_to_string(path).and_then(|text| Script::parse(&text)) {
            Ok(script) => scripts.push((path.clone(), script)),
            Err(e) => errors.push(format!("{}:\n{e}", path.display())),
        }
    }
    if errors.is_empty() { Ok(scripts) } else { Err(errors.join("\n")) }
}

/// `cargo orchestration script FILE`: boots headless and runs it, on the
/// system `--prebuilt` names or on a fresh build.
pub fn script(o: &Options, file: &str) -> Result {
    let script = Script::parse(&util::read_to_string(Path::new(file))?).map_err(|e| format!("{file}:\n{e}"))?;
    script_on(o, &script, o.prebuilt.as_deref())
}

/// `cargo orchestration scripts FILE...`: builds the system once and runs
/// the scripts on it.
pub fn scripts(o: &Options, files: &[PathBuf]) -> Result {
    let scripts = parse_all(files)?;
    let started = Instant::now();
    let system = system::build(o)?;
    util::status("Built", format!("the system in {:.1}s", started.elapsed().as_secs_f32()));
    system::save(o, &system, &system::system_dir())?;
    drop(system);
    run_scripts(o, scripts)
}

/// Why a script cannot run on this machine, if it cannot: the part of the
/// toolchain it needs is not built, or QEMU has not the 3D GPU it asks for.
fn unrunnable(script: &Script, gl_gpu: impl Fn() -> bool) -> Option<String> {
    if let Some(what) = script.boot.requires.iter().find(|r| !toolchain::built(r.part())) {
        return Some(format!("needs the {}: `cargo orchestration toolchain`", what.part()));
    }
    if script.boot.gpu == Some(true) && !gl_gpu() {
        return Some("needs QEMU's 3D GPU (virtio-gpu-gl-pci), which this QEMU has not".into());
    }
    None
}

/// How long a script's run may take: four times what it took last
/// (machines running beside it slow it), at least five minutes; half an
/// hour for one that has not run yet.
fn deadline(last: Option<f32>) -> Duration {
    match last {
        Some(secs) => Duration::from_secs_f32((secs * 4.0).max(300.0)),
        None => Duration::from_secs(1800),
    }
}

/// Runs `scripts`, `o.jobs` at a time, each in a process of its own on the
/// system `system::system_dir` holds, with a directory of its own
/// (`$VEDA_OUT`: its overlay of the disk image, serial log, home disk,
/// firmware variables). Each one's output goes to
/// `target/veda/test-logs/NAME.log` (and a failing one's serial log beside
/// it); each result is told as it comes, the longest scripts (as the last
/// run timed them) first, and those that need the machine to themselves
/// last, one at a time. A run that outlives its [`deadline`] is killed,
/// and its QEMU with it. Fails if any script did.
fn run_scripts(o: &Options, scripts: Vec<(PathBuf, Script)>) -> Result {
    let logs = util::out_dir().join("test-logs");
    util::create_dir_all(&logs)?;
    let name_of = |p: &Path| p.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let install = QemuInstall::locate()?;
    let mut runnable = Vec::new();
    for (path, script) in scripts {
        match unrunnable(&script, || install.has_gl_gpu()) {
            Some(why) => util::status("Skipping", format!("GUI script {} ({why})", name_of(&path))),
            None => runnable.push((path, script.alone())),
        }
    }
    // The longest first, so that the last to finish are short.
    let timings_file = logs.join("durations");
    let timings: HashMap<String, f32> = std::fs::read_to_string(&timings_file)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(' ').and_then(|(n, s)| Some((n.to_string(), s.parse().ok()?))))
        .collect();
    let time = |p: &Path| timings.get(&name_of(p)).copied().unwrap_or(60.0);
    runnable.sort_by(|(a, _), (b, _)| time(b).total_cmp(&time(a)));
    let (alone, together): (Vec<_>, Vec<_>) = runnable.into_iter().partition(|&(_, alone)| alone);
    let [alone, together] = [alone, together].map(|b| b.into_iter().map(|(p, _)| p).collect::<Vec<_>>());
    let total = alone.len() + together.len();
    let jobs = o.jobs.min(together.len()).max(1);
    let then = match alone.len() {
        0 => String::new(),
        n => format!(", then {n} that need the machine to themselves"),
    };
    util::status("Testing", format!("{total} GUI scripts, {jobs} at a time{then}"));
    let given = cli::passed_on(&o.given);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let results: Mutex<Vec<(String, bool, f32)>> = Mutex::new(Vec::new());
    // Runs `batch`, `jobs` at a time.
    let run = |batch: Vec<PathBuf>, jobs: usize| {
        let queue = Mutex::new(batch.into_iter().collect::<VecDeque<_>>());
        std::thread::scope(|s| {
            for slot in 0..jobs {
                let (queue, results, given, exe, logs, timings) = (&queue, &results, &given, &exe, &logs, &timings);
                s.spawn(move || {
                    let run_dir = util::out_dir().join("jobs").join(slot.to_string());
                    loop {
                        // The queue is locked for taking the next only.
                        let next = queue.lock().unwrap().pop_front();
                        let Some(path) = next else { break };
                        let name = name_of(&path);
                        let log = logs.join(format!("{name}.log"));
                        let started = Instant::now();
                        let limit = deadline(timings.get(&name).copied());
                        let mut run = std::process::Command::new(exe);
                        run.arg("script").arg(&path).arg("--prebuilt").arg(system::system_dir());
                        run.args(given).env("VEDA_OUT", &run_dir);
                        let outcome = run_one(run, &log, limit);
                        let secs = started.elapsed().as_secs_f32();
                        let ok = outcome.is_ok();
                        if ok {
                            util::status("Passed", format!("{name} ({secs:.0} s)"));
                        } else {
                            let serial = logs.join(format!("{name}.serial.log"));
                            let _ = std::fs::copy(run_dir.join("serial.log"), &serial);
                            let why = outcome.err().unwrap_or_default();
                            util::failure("Failed", format!("{name} ({secs:.0} s) {why} — {}", log.display()));
                        }
                        results.lock().unwrap().push((name, ok, secs));
                    }
                });
            }
        });
    };
    run(together, jobs);
    run(alone, 1);
    let results = results.into_inner().unwrap();
    let mut timings = timings;
    for (name, _, secs) in &results {
        timings.insert(name.clone(), *secs);
    }
    let mut lines: Vec<String> = timings.iter().map(|(n, s)| format!("{n} {s:.0}")).collect();
    lines.sort();
    let _ = util::write_atomically(&timings_file, lines.join("\n") + "\n");
    let failed: Vec<&str> = results.iter().filter(|r| !r.1).map(|r| r.0.as_str()).collect();
    if failed.is_empty() {
        util::status("Passed", format!("all {total} GUI scripts"));
        Ok(())
    } else {
        Err(format!("{} of {total} GUI scripts failed: {}", failed.len(), failed.join(", ")))
    }
}

/// Runs `cmd` (a script's run: `exe script PATH --prebuilt DIR`), its
/// output into `log`, for `limit` at most: past it the process is killed
/// (its QEMU dies with it). The error says why the run failed.
fn run_one(mut cmd: std::process::Command, log: &Path, limit: Duration) -> Result {
    let out = std::fs::File::create(log).map_err(|e| format!("creating {}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| format!("{}: {e}", log.display()))?;
    let mut child = cmd.stdout(out).stderr(err).spawn().map_err(|e| format!("starting {cmd:?}: {e}"))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("killed after {:.0} s, its deadline", limit.as_secs_f32()));
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    if status.success() {
        return Ok(());
    }
    let why = std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .rev()
        .find(|l| l.contains("error:"))
        .map(str::to_string)
        .unwrap_or_else(|| format!("({status})"));
    Err(why)
}

/// Runs an automation script on the system `prebuilt` keeps (saved by
/// `system::save`), or on a fresh build if `None`.
fn script_on(o: &Options, script: &Script, prebuilt: Option<&Path>) -> Result {
    let install = QemuInstall::locate()?;
    if let Some(why) = unrunnable(script, || install.has_gl_gpu()) {
        return Err(format!("the script cannot run here: it {why}"));
    }
    let boot = &script.boot;
    let mut o = o.clone();
    let mut extra: Vec<String> = Vec::new();
    // The test microphone's address goes on the command line.
    let mic = if script.needs_mic() { Some(MicServer::start()?) } else { None };
    if let Some(m) = &mic {
        extra.push(m.boot_arg());
    }
    let agent = if script.needs_agent() { Some(AgentSim::start()?) } else { None };
    if let Some(a) = &agent {
        // This computer is the gateway of the guest's network, QEMU's NAT.
        extra.push(a.boot_arg("10.0.2.2", !boot.agent_asleep));
    }
    // nettest's local checks, against a server of this computer's.
    let web = if boot.nettest_local { Some(HostWeb::start()?) } else { None };
    if let Some(w) = &web {
        extra.push(w.boot_arg());
    }
    extra.extend(boot.cmdline.iter().cloned());
    o.cmdline = std::iter::once(o.cmdline.clone()).chain(extra).collect::<Vec<_>>().join(" ").trim().to_string();
    let o = &o;
    let live = o.live || boot.live.is_some();
    let (w, h) = system::screen(o, live);
    let mut vm = o.vm.clone();
    vm.screen = qemu::Screen::new(w, h)?;
    let disk = if live {
        let stick = system::live_stick();
        match prebuilt {
            Some(dir) => system::write_iso(o, &system::load(dir)?, &stick)?,
            None => build_iso(o, &stick)?,
        };
        stick
    } else {
        match prebuilt {
            // An overlay of the image the run wrote, with this boot's
            // configuration.
            Some(dir) => {
                let base = dir.join(system::SAVED_IMAGE);
                if !base.is_file() {
                    return Err(format!(
                        "{} has no {}: `cargo orchestration scripts` writes it",
                        dir.display(),
                        system::SAVED_IMAGE
                    ));
                }
                let overlay = util::out_dir().join("boot.qcow2");
                util::create_dir_all(&util::out_dir())?;
                image::write_boot_overlay(&base, &overlay, &system::boot_config(o, false)?)?;
                vm.boot_format = "qcow2";
                overlay
            }
            None => build(o)?,
        }
    };
    vm.audio_wav = (boot.audio != Audio::Host).then(|| util::out_dir().join("audio.wav"));
    if boot.audio == Audio::Silent {
        vm.audio_wav = None;
        vm.audio_silent = true;
    }
    // Scripts that reboot the machine need QEMU to stay up across it (and
    // the HD Audio card, see `script::Script::parse`).
    vm.allow_reboot = script.resets();
    if let Some(net) = boot.net {
        vm.net = net;
    }
    if let Some(card) = &boot.sound {
        vm.sound = card.clone();
    }
    if let Some(model) = &boot.nic {
        vm.nic_model = Some(model.clone());
    }
    if let Some(bus) = boot.disk {
        vm.disk_bus = bus;
    }
    if let Some(mib) = boot.memory {
        vm.memory_mib = mib;
    }
    vm.acpi_tables = acpi_tables(&boot.acpi_tables)?;
    vm.firmware_variables = boot.firmware_variables.clone();
    if let Some(input) = boot.input {
        vm.input = input;
    }
    if let Some(gpu) = boot.gpu {
        vm.gpu = Some(gpu);
    }
    vm.usb_net |= boot.usb_net;
    let home = util::out_dir().join("test-home.img");
    if live {
        // A stick with the ISO on it, and no home disk.
        vm.usb_stick = true;
        vm.usb_stick_writable = boot.live == Some(Live::Writable);
    } else {
        // Every scripted run starts with a new, empty home directory.
        qemu::prepare_home_disk(&home, true)?;
        vm.home_disk = Some(home.clone());
    }
    let result = automate::run_script(&install, &disk, vm, script, mic, agent);
    drop(web);
    // A secret typed in (an API key Settings stored) is not left on a disk.
    if script.types_secret() {
        util::remove_file(&home)?;
    }
    let log = result?;
    println!("--- serial log (tail) ---\n{}", automate::tail(&log, 40));
    Ok(())
}

/// The ACPI tables `names` (the machine's: `arch::acpitest`), written out
/// for QEMU.
fn acpi_tables(names: &[String]) -> Result<Vec<PathBuf>> {
    let mut tables = Vec::new();
    for name in names {
        let table = arch::acpitest::table(name).ok_or_else(|| format!("no ACPI table '{name}'"))?;
        let path = util::out_dir().join(format!("acpi-{name}.aml"));
        util::create_dir_all(&util::out_dir())?;
        util::write(&path, table)?;
        tables.push(path);
    }
    Ok(tables)
}

/// Builds all components and writes the bootable disk image.
pub fn build(o: &Options) -> Result<PathBuf> {
    let started = Instant::now();
    let system = system::build(o)?;
    let disk = system::disk_image();
    let size = system::write_image(o, &system, &disk)?;
    util::status(
        "Finished",
        format!(
            "{} (kernel {}, initrd {}) in {:.1}s",
            util::human_size(size),
            util::human_size(system.kernel.len() as u64),
            util::human_size(system.initrd.len() as u64),
            started.elapsed().as_secs_f32()
        ),
    );
    Ok(disk)
}

/// Builds all components and writes the live system's ISO image to `path`.
pub fn build_iso(o: &Options, path: &Path) -> Result<u64> {
    let started = Instant::now();
    let system: System = system::build(o)?;
    let size = system::write_iso(o, &system, path)?;
    util::status("Finished", format!("{} in {:.1}s", util::human_size(size), started.elapsed().as_secs_f32()));
    Ok(size)
}

/// `cargo orchestration shot`: boots headless, waits, and saves a
/// screenshot.
pub fn shot(o: &Options) -> Result {
    let out = o.out.clone().unwrap_or_else(|| util::out_dir().join("screen.png"));
    let mut steps = match &o.until {
        Some(text) => vec![
            Step::WaitSerial { text: text.clone(), timeout: Duration::from_secs(120) },
            Step::Wait(Duration::from_secs(1)),
        ],
        None => vec![Step::Wait(Duration::try_from_secs_f64(o.wait).map_err(|_| "--wait expects seconds")?)],
    };
    steps.push(Step::Shot { path: out.to_string_lossy().into_owned(), settle: true });
    let script = Script { boot: Default::default(), steps: steps.into_iter().map(|s| (1, s)).collect() };
    script_on(o, &script, o.prebuilt.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The workspace's members: each one's name and directory.
    fn members() -> Vec<(String, PathBuf)> {
        let out = util::cargo()
            .args(["metadata", "--no-deps", "--offline", "--format-version", "1"])
            .output()
            .expect("cargo metadata runs");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let meta: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        meta["packages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let manifest = PathBuf::from(p["manifest_path"].as_str().unwrap());
                (p["name"].as_str().unwrap().to_string(), manifest.parent().unwrap().to_path_buf())
            })
            .collect()
    }

    /// Whether a crate in `dir` has tests (`#[test]` functions), leaving out
    /// the crates within it (`others`).
    fn has_tests(dir: &Path, others: &[&Path]) -> bool {
        util::files_under(dir).unwrap().iter().any(|f| {
            f.extension().is_some_and(|e| e == "rs")
                && !others.iter().any(|o| f.starts_with(o))
                && !f.components().any(|c| c.as_os_str() == "target")
                && std::fs::read_to_string(f).is_ok_and(|t| t.lines().any(|l| l.trim_start().starts_with("#[test]")))
        })
    }

    /// `cargo orchestration test` runs every crate's tests: none is left
    /// off the lists, and the lists name crates with tests.
    #[test]
    fn every_crate_with_tests_is_tested() {
        let members = members();
        let dirs: Vec<&Path> = members.iter().map(|(_, d)| d.as_path()).collect();
        let with_tests: Vec<&str> = members
            .iter()
            .filter(|(_, dir)| {
                let nested: Vec<&Path> = dirs.iter().copied().filter(|d| d != dir && d.starts_with(dir)).collect();
                has_tests(dir, &nested)
            })
            .map(|(name, _)| name.as_str())
            .collect();
        assert!(with_tests.contains(&"orchestration") && with_tests.contains(&"vhv"), "{with_tests:?}");
        let listed: Vec<&str> = HOST_TESTED
            .iter()
            .map(|(p, _)| *p)
            .chain(HOST_TESTED_ALONE.iter().copied())
            .chain(GUEST_TESTED.iter().copied())
            .chain(NOT_TESTED.iter().map(|(p, _)| *p))
            .collect();
        let untested: Vec<&&str> = with_tests.iter().filter(|p| !listed.contains(p)).collect();
        assert!(
            untested.is_empty(),
            "crates with tests that `cargo orchestration test` does not run: {untested:?} (add them to HOST_TESTED, \
             GUEST_TESTED or NOT_TESTED in orchestration/src/runner.rs)"
        );
        let without: Vec<&&str> = listed.iter().filter(|p| !with_tests.contains(p)).collect();
        assert!(without.is_empty(), "listed crates without tests (or not in the workspace): {without:?}");
        // The guest's crates run on Linux, the others on the host.
        let guest_dir = util::workspace_root().join("implementation").join("guest");
        for (name, dir) in &members {
            let guest = dir.starts_with(&guest_dir);
            let host = HOST_TESTED.iter().any(|(p, _)| p == name) || HOST_TESTED_ALONE.contains(&name.as_str());
            assert!(!(guest && host), "{name} is the driver VM's");
            assert!(!(!guest && GUEST_TESTED.contains(&name.as_str())), "{name} is not the driver VM's");
        }
    }

    #[test]
    fn host_tests_run_in_one_invocation() {
        let args: Vec<String> = host_tests().get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(&args[..2], ["test", "--quiet"]);
        assert_eq!(args.iter().filter(|a| *a == "--package").count(), HOST_TESTED.len());
        let features = &args[args.iter().position(|a| a == "--features").unwrap() + 1];
        assert_eq!(features, "initrd/std,vimage/encode,vfiles/thumbnails");
    }

    #[test]
    fn deadlines_are_several_times_the_last_run() {
        assert_eq!(deadline(Some(200.0)), Duration::from_secs(800));
        assert_eq!(deadline(Some(10.0)), Duration::from_secs(300));
        assert_eq!(deadline(None), Duration::from_secs(1800));
    }

    #[test]
    fn scripts_that_cannot_run_here_are_said_why() {
        let gpu = Script::parse("gpu on\nwait 1\n").unwrap();
        assert!(unrunnable(&gpu, || false).is_some_and(|why| why.contains("3D GPU")));
        assert_eq!(unrunnable(&gpu, || true), None);
        assert_eq!(unrunnable(&Script::parse("gpu off\n").unwrap(), || false), None);
    }

    /// A run past its deadline is killed, and says so.
    #[test]
    fn a_run_past_its_deadline_is_killed() {
        let log = std::env::temp_dir().join(format!("orchestration-deadline-{}.log", std::process::id()));
        let mut sleeper = std::process::Command::new("sleep");
        sleeper.arg("30");
        let started = Instant::now();
        let e = run_one(sleeper, &log, Duration::from_millis(500)).unwrap_err();
        assert!(e.contains("killed after 0 s, its deadline"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(10));
        let mut quick = std::process::Command::new("sh");
        quick.args(["-c", "echo 'error: it failed'; exit 1"]);
        assert_eq!(run_one(quick, &log, Duration::from_secs(10)), Err("error: it failed".into()));
        assert_eq!(run_one(std::process::Command::new("true"), &log, Duration::from_secs(10)), Ok(()));
        std::fs::remove_file(&log).unwrap();
    }
}

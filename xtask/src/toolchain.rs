//! The C toolchain: GCC, binutils and musl for Veda.
//!
//! `cargo xtask toolchain` builds, from the sources in `ports/`:
//!
//! * a **cross toolchain** that runs on this machine and makes Veda
//!   programs (`<target>-gcc` and friends, for the architecture's target,
//!   `arch::C_TARGET`, with a sysroot laid out like Veda's `/system`), used
//!   to build the C tests and ports;
//! * the **native toolchain**, the same compiler built to run inside Veda,
//!   which the system image installs in `/system` (`gcc`, `as`, `ld`, the C
//!   library and its headers);
//! * the **media libraries**, dav1d and FFmpeg built with the cross
//!   toolchain, which `mediadec` (the decoder of Videos) is linked with.
//!
//! Everything lives under `target/toolchain` (`$VEDA_TOOLCHAIN` overrides
//! it): `downloads/` (the verified source archives), `src/` (unpacked and
//! patched), `build/`, `cross/`, `native/` and `media/`. Everything is built
//! with `ports/build.sh`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch::{self, BARE_TARGET};
use crate::util::{self, Result};

/// Where the toolchain is built.
pub fn root() -> PathBuf {
    match std::env::var_os("VEDA_TOOLCHAIN") {
        Some(p) => {
            let p = PathBuf::from(p);
            if p.is_absolute() { p } else { util::workspace_root().join(p) }
        }
        None => util::workspace_root().join("target").join("toolchain"),
    }
}

/// Whether the `cross toolchain` or the `native toolchain` has been built.
pub fn built(what: &str) -> bool {
    match what {
        "cross toolchain" => cross_gcc().is_some(),
        "media libraries" => cross_gcc().is_some() && media_libraries().is_some(),
        _ => root().join("native").join("system").join("bin").join("gcc").is_file(),
    }
}

/// For `cargo xtask doctor`: whether the toolchain, which is optional, has
/// been built (`Ok`), and if not, whether it can be (`Err`).
pub fn status() -> std::result::Result<String, String> {
    if built("native toolchain") && built("cross toolchain") {
        return Ok(format!("built ({})", root().display()));
    }
    let how = match shell() {
        Ok(_) => "`cargo xtask toolchain` builds it".to_string(),
        Err(e) => format!("`cargo xtask toolchain` builds it, but {e}"),
    };
    Err(format!("not built (optional: C and GCC in the image); {how}"))
}

/// Where the media libraries (dav1d, FFmpeg) and their tests are.
pub fn media_root() -> PathBuf {
    root().join("media")
}

/// The media libraries' static libraries, in link order, if all are built.
pub fn media_libraries() -> Option<Vec<PathBuf>> {
    let lib = |port: &str, name: &str| media_root().join(port).join("system").join("lib").join(format!("lib{name}.a"));
    let libs = vec![
        lib("ffmpeg", "avformat"),
        lib("ffmpeg", "avcodec"),
        lib("ffmpeg", "swscale"),
        lib("ffmpeg", "swresample"),
        lib("ffmpeg", "avutil"),
        lib("dav1d", "dav1d"),
    ];
    libs.iter().all(|l| l.is_file()).then_some(libs)
}

/// For `cargo xtask doctor`: whether the media libraries, which videos need,
/// have been built.
pub fn media_status() -> std::result::Result<String, String> {
    if media_libraries().is_some() {
        let version = |p: &str| Port::named(p).map(|p| p.version).unwrap_or_default();
        return Ok(format!("FFmpeg {} and dav1d {} ({})", version("ffmpeg"), version("dav1d"), media_root().display()));
    }
    let how = match shell() {
        Ok(_) => "`cargo xtask toolchain` builds them".to_string(),
        Err(e) => format!("`cargo xtask toolchain` builds them, but {e}"),
    };
    Err(format!("not built (optional: videos play with them); {how}"))
}

/// The cross compiler, if it has been built.
pub fn cross_gcc() -> Option<PathBuf> {
    let gcc = root().join("cross").join("bin").join(format!("{}-gcc", arch::C_TARGET));
    gcc.is_file().then_some(gcc)
}

/// A tool of the cross toolchain (`ld`, `objcopy`, `ar`, ...).
fn cross_tool(name: &str) -> PathBuf {
    root().join("cross").join("bin").join(format!("{}-{name}", arch::C_TARGET))
}

/// The symbols the POSIX layer gives the C library (and, for the `veda_`
/// ones, C programs: `<veda/ipc.h>`); all else in it stays private.
const POSIX_LAYER_EXPORTS: [&str; 22] = [
    "__veda_syscall",
    "__veda_init",
    "__veda_spawn",
    "__veda_clone",
    "__veda_set_thread_area",
    "__veda_unmapself",
    "__veda_bootstrap",
    "veda_close",
    "veda_duplicate",
    "veda_startup_handle",
    "veda_service_register",
    "veda_service_accept",
    "veda_service_connect",
    "veda_channel_write",
    "veda_channel_read",
    "veda_wait",
    "veda_now_ns",
    "veda_vmo_create",
    "veda_vmo_map",
    "veda_vmo_unmap",
    "veda_event_create",
    "veda_object_signal",
];

/// Builds the POSIX layer (`lib/posix`) as a static library for the
/// architecture's bare target; returns its path.
pub fn posix_library() -> Result<PathBuf> {
    util::status("Building", format!("the POSIX layer ({BARE_TARGET})"));
    util::run(util::cargo().args([
        "rustc",
        "--package",
        "vposix",
        "--target",
        BARE_TARGET,
        "--release",
        "--crate-type",
        "staticlib",
    ]))?;
    Ok(util::target_dir().join(BARE_TARGET).join("release").join("libvposix.a"))
}

/// Links the POSIX layer's static library into the one relocatable object
/// the C library holds, which exports [`POSIX_LAYER_EXPORTS`] and nothing
/// else (no Rust or compiler-builtins symbol can clash with the C
/// library's). `ports/build.sh` does the same for the first build.
pub fn posix_object(lib: &Path, out: &Path) -> Result {
    isolated_object(lib, out, &POSIX_LAYER_EXPORTS)
}

/// Links a Rust static library into one relocatable object that defines
/// `exports` and nothing else globally: its Rust runtime, compiler builtins
/// (`memcpy` ...) and allocator stay its own, beside the C library's.
/// Symbols it uses but does not define stay global, for the final link.
fn isolated_object(lib: &Path, out: &Path, exports: &[&str]) -> Result {
    let whole = out.with_extension("all.o");
    let mut ld = Command::new(cross_tool("ld"));
    ld.args(["-r", "--gc-sections"]);
    for s in exports {
        ld.args(["-u", s]);
    }
    util::run(ld.arg("-o").arg(&whole).arg(lib))?;
    let mut objcopy = Command::new(cross_tool("objcopy"));
    objcopy.args(["--strip-debug", "--remove-section=.llvmbc", "--remove-section=.llvmcmd"]);
    for s in exports {
        objcopy.arg(format!("--keep-global-symbol={s}"));
    }
    util::run(objcopy.arg(&whole).arg(out))?;
    let _ = std::fs::remove_file(&whole);
    Ok(())
}

/// The C library's object of the POSIX layer, as last made.
fn posix_layer_object() -> PathBuf {
    root().join("build").join("veda.o")
}

/// Puts the current POSIX layer into the cross toolchain's C library (and
/// the native toolchain's, if built), so that C programs built from now
/// on get it.
pub fn refresh_posix_layer() -> Result {
    let object = posix_layer_object();
    let library = posix_library()?;
    if !is_newer(&object, &[&library]) {
        std::fs::create_dir_all(object.parent().unwrap()).map_err(|e| format!("{e}"))?;
        posix_object(&library, &object)?;
    }
    let headers = util::workspace_root().join("lib").join("posix").join("include").join("veda");
    for sysroot in [root().join("cross").join("sysroot"), root().join("native")] {
        let libc = sysroot.join("system").join("lib").join("libc.a");
        if libc.is_file() && !is_newer(&libc, &[&object]) {
            util::run(Command::new(cross_tool("ar")).arg("rcs").arg(&libc).arg(&object))?;
        }
        // Its own header, `<veda/ipc.h>`.
        if libc.is_file() {
            let dir = sysroot.join("system").join("include").join("veda");
            std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
            for entry in
                std::fs::read_dir(&headers).map_err(|e| format!("reading {}: {e}", headers.display()))?.flatten()
            {
                let to = dir.join(entry.file_name());
                if !is_newer(&to, &[&entry.path()]) {
                    std::fs::copy(entry.path(), &to).map_err(|e| format!("copying to {}: {e}", to.display()))?;
                }
            }
        }
    }
    Ok(())
}

/// How each C test program is linked: once at a fixed address, as GCC links
/// by default, and once position-independent. The suffix of the
/// executable's name, and the flags.
const C_TEST_VARIANTS: &[(&str, &[&str])] = &[("", &[]), ("-pie", &["-fPIE", "-static-pie"])];

/// Compiles the test programs in C and C++ (`tests/c/*.c` and `*.cc`, each
/// in every one of [`C_TEST_VARIANTS`]) for the system image: `(name,
/// executable)`. None without a cross compiler. What they expect of the
/// architecture is in the headers of `tests/c/arch/<architecture>/`.
pub fn c_tests() -> Result<Vec<(String, Vec<u8>)>> {
    let Some(gcc) = cross_gcc() else { return Ok(Vec::new()) };
    let gxx = cross_tool("g++");
    refresh_posix_layer()?;
    let libc = root().join("cross").join("sysroot").join("system").join("lib").join("libc.a");
    let src = util::workspace_root().join("tests").join("c");
    let arch_headers = src.join("arch").join(arch::NAME);
    let headers: Vec<PathBuf> = std::fs::read_dir(&arch_headers)
        .map_err(|e| format!("reading {}: {e}", arch_headers.display()))?
        .flatten()
        .map(|e| e.path())
        .collect();
    let out = root().join("tests");
    std::fs::create_dir_all(&out).map_err(|e| format!("creating {}: {e}", out.display()))?;
    let mut sources: Vec<PathBuf> = std::fs::read_dir(&src)
        .map_err(|e| format!("reading {}: {e}", src.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "c" || x == "cc"))
        .collect();
    sources.sort();
    let mut programs = Vec::new();
    for source in sources {
        let stem = source.file_stem().unwrap().to_string_lossy().into_owned();
        let compiler = if source.extension().is_some_and(|x| x == "cc") { &gxx } else { &gcc };
        for (suffix, flags) in C_TEST_VARIANTS {
            let name = format!("{stem}{suffix}");
            let exe = out.join(&name);
            let inputs: Vec<&Path> =
                [source.as_path(), compiler, &libc].into_iter().chain(headers.iter().map(PathBuf::as_path)).collect();
            if !is_newer(&exe, &inputs) {
                util::run(
                    Command::new(compiler)
                        .args(["-O2", "-Wall", "-Wextra", "-Werror", "-g0", "-s"])
                        .arg("-I")
                        .arg(&arch_headers)
                        .args(*flags)
                        .arg("-o")
                        .arg(&exe)
                        .arg(&source),
                )?;
            }
            programs.push((name, util::read(&exe)?));
        }
    }
    Ok(programs)
}

/// The flags the media decoder's shim is compiled with.
const SHIM_CFLAGS: &[&str] =
    &["-std=gnu17", "-O2", "-fPIE", "-Wall", "-Wextra", "-Werror", "-fstack-protector-strong", "-g0"];

/// C source without its comments (`/* */` and `//`), each replaced by a
/// space.
fn without_c_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let block = rest.find("/*");
        let line = rest.find("//");
        match (block, line) {
            (Some(b), l) if l.is_none_or(|l| b < l) => {
                out.push_str(&rest[..b]);
                out.push(' ');
                rest = rest[b + 2..].find("*/").map_or("", |e| &rest[b + 2 + e + 2..]);
            }
            (_, Some(l)) => {
                out.push_str(&rest[..l]);
                out.push(' ');
                rest = rest[l..].find('\n').map_or("", |e| &rest[l + e..]);
            }
            _ => {
                out.push_str(rest);
                return out;
            }
        }
    }
}

/// The media decoder (`services/mediadec`) for the system image, if the
/// cross toolchain and the media libraries are built: its Rust half (built
/// for the architecture's bare target and made one object exporting `mediadec_main`
/// alone), its C shim, FFmpeg and dav1d, linked by the cross GCC as a
/// static position-independent program. Built again when any of it changed.
pub fn mediadec() -> Result<Option<Vec<u8>>> {
    let (Some(gcc), Some(libraries)) = (cross_gcc(), media_libraries()) else { return Ok(None) };
    refresh_posix_layer()?;
    let crate_dir = util::workspace_root().join("services").join("mediadec");
    let shim = crate_dir.join("shim");
    let header = shim.join("vmd.h");
    // No floating point crosses the shim: the Rust half passes it otherwise.
    let text = std::fs::read_to_string(&header).map_err(|e| format!("reading {}: {e}", header.display()))?;
    let code = without_c_comments(&text);
    if code.split(|c: char| !c.is_alphanumeric() && c != '_').any(|w| w == "float" || w == "double") {
        return Err(format!("{}: no `float` or `double` may cross the shim", header.display()));
    }
    util::status("Building", format!("the media decoder ({BARE_TARGET} and C)"));
    util::run(util::cargo().args([
        "rustc",
        "--package",
        "mediadec",
        "--target",
        BARE_TARGET,
        "--release",
        "--crate-type",
        "staticlib",
    ]))?;
    let rust_lib = util::target_dir().join(BARE_TARGET).join("release").join("libmediadec.a");
    let out = root().join("build").join("mediadec");
    std::fs::create_dir_all(&out).map_err(|e| format!("creating {}: {e}", out.display()))?;
    let rust_obj = out.join("mediadec-rs.o");
    if !is_newer(&rust_obj, &[&rust_lib]) {
        isolated_object(&rust_lib, &rust_obj, &["mediadec_main"])?;
        // What it defines globally must be that and nothing else.
        let nm = Command::new(cross_tool("nm"))
            .args(["-g", "--defined-only", "--format=just-symbols"])
            .arg(&rust_obj)
            .output()
            .map_err(|e| format!("running nm: {e}"))?;
        let defined: Vec<String> = String::from_utf8_lossy(&nm.stdout).lines().map(str::to_string).collect();
        if defined != ["mediadec_main"] {
            let _ = std::fs::remove_file(&rust_obj);
            return Err(format!("the media decoder's Rust half exports more than mediadec_main: {defined:?}"));
        }
    }
    let media = media_root();
    let includes =
        [media.join("ffmpeg").join("system").join("include"), media.join("dav1d").join("system").join("include")];
    let libc = root().join("cross").join("sysroot").join("system").join("lib").join("libc.a");
    let mut sources: Vec<PathBuf> = std::fs::read_dir(&shim)
        .map_err(|e| format!("reading {}: {e}", shim.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "c"))
        .collect();
    sources.sort();
    let headers: Vec<PathBuf> = ["vmd.h", "vmd_internal.h"].iter().map(|h| shim.join(h)).collect();
    let mut objects = Vec::new();
    for source in &sources {
        let object = out.join(source.file_name().unwrap()).with_extension("o");
        let mut inputs: Vec<&Path> = vec![source, &gcc];
        inputs.extend(headers.iter().map(PathBuf::as_path));
        inputs.extend(libraries.iter().map(PathBuf::as_path));
        if !is_newer(&object, &inputs) {
            let mut cc = Command::new(&gcc);
            cc.args(SHIM_CFLAGS);
            for i in &includes {
                cc.arg("-I").arg(i);
            }
            util::run(cc.arg("-c").arg("-o").arg(&object).arg(source))?;
        }
        objects.push(object);
    }
    let exe = out.join("mediadec");
    let mut inputs: Vec<&Path> = vec![&rust_obj, &libc];
    inputs.extend(objects.iter().map(PathBuf::as_path));
    inputs.extend(libraries.iter().map(PathBuf::as_path));
    if !is_newer(&exe, &inputs) {
        let unstripped = out.join("mediadec.debug");
        let mut link = Command::new(&gcc);
        link.args(["-static-pie", "-pthread", "-Wl,--gc-sections", "-Wl,-z,stack-size=2097152", "-o"]).arg(&unstripped);
        link.args(&objects).arg(&rust_obj).arg("-Wl,--start-group").args(&libraries).arg("-Wl,--end-group").arg("-lm");
        util::run(&mut link)?;
        // The unstripped program stays, for addr2line.
        util::run(Command::new(cross_tool("strip")).arg("--strip-all").arg("-o").arg(&exe).arg(&unstripped))?;
    }
    Ok(Some(util::read(&exe)?))
}

/// The media libraries' tests for the system image (`/system/tests/media`):
/// FFmpeg's and dav1d's checkasm.
pub fn media_tests() -> Result<Vec<(String, Vec<u8>)>> {
    let dir = media_root().join("tests");
    let mut tests = Vec::new();
    for name in ["checkasm-dav1d", "checkasm-ffmpeg"] {
        let path = dir.join(name);
        if path.is_file() {
            tests.push((name.to_string(), util::read(&path)?));
        }
    }
    Ok(tests)
}

/// Where [`command`] notes the POSIX layer the native programs were linked
/// with: the SHA-256 of its object.
fn native_layer_note() -> PathBuf {
    root().join("build").join("native-posix-layer")
}

/// The native toolchain's programs are C programs too, with the POSIX layer
/// linked in: when it has changed since they were linked, links them again
/// (`ports/build.sh` redoes only that). Without the build tools it says so
/// and leaves them as they are.
pub fn relink_native() -> Result {
    let object = posix_layer_object();
    if !built("native toolchain") || !object.is_file() {
        return Ok(());
    }
    let current = sha256_of(&object)?;
    if std::fs::read_to_string(native_layer_note()).is_ok_and(|linked| linked.trim() == current) {
        return Ok(());
    }
    if let Err(e) = shell() {
        util::status("Note", format!("the C toolchain in the image has an older POSIX layer. {e}"));
        return Ok(());
    }
    util::status("Linking", "the C toolchain's programs with the current POSIX layer");
    command(&[])
}

/// The native toolchain's files for the system image: paths relative to
/// `/system` and their contents. None if it has not been built.
pub fn native_files() -> Result<Vec<(String, Vec<u8>)>> {
    let base = root().join("native").join("system");
    if !base.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    let mut stack = vec![base.clone()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(&base).unwrap().to_string_lossy().replace('\\', "/");
                files.push((rel, util::read(&path)?));
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

/// A source package of `ports/` (its `port.toml`).
pub struct Port {
    name: String,
    version: String,
    url: String,
    sha256: String,
}

impl Port {
    /// The port in `ports/NAME`.
    pub fn named(name: &str) -> Result<Port> {
        Port::load(&util::workspace_root().join("ports").join(name))
    }

    fn load(dir: &Path) -> Result<Port> {
        let file = dir.join("port.toml");
        let text = std::fs::read_to_string(&file).map_err(|e| format!("reading {}: {e}", file.display()))?;
        let value = |key: &str| -> Result<String> {
            text.lines()
                .filter_map(|l| l.split_once('='))
                .find(|(k, _)| k.trim() == key)
                .map(|(_, v)| v.trim().trim_matches('"').to_string())
                .ok_or_else(|| format!("{}: no {key}", file.display()))
        };
        Ok(Port { name: value("name")?, version: value("version")?, url: value("url")?, sha256: value("sha256")? })
    }

    /// Where its archive is downloaded to, in `downloads`.
    fn archive(&self, downloads: &Path) -> PathBuf {
        downloads.join(self.url.rsplit('/').next().unwrap_or(&self.name))
    }
}

/// The ports the toolchain and the media libraries are built from.
fn ports() -> Result<Vec<Port>> {
    ["binutils", "gcc", "gmp", "mpfr", "mpc", "musl", "dav1d", "ffmpeg"].iter().map(|p| Port::named(p)).collect()
}

pub fn sha256_of(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let data = util::read(path)?;
    Ok(Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect())
}

/// Downloads a port's source archive into `downloads` unless it is there,
/// and checks it against the SHA-256 the port pins. An archive the other
/// build (the C toolchain's, or the driver VM's Linux) has is copied.
pub fn fetch(port: &Port, downloads: &Path) -> Result {
    let archive = port.archive(downloads);
    if archive.is_file() && sha256_of(&archive)? == port.sha256 {
        return Ok(());
    }
    for other in [root().join("downloads"), crate::linux::root().join("downloads")] {
        let copy = port.archive(&other);
        if other != downloads && copy.is_file() && sha256_of(&copy)? == port.sha256 {
            std::fs::create_dir_all(downloads).map_err(|e| format!("{e}"))?;
            std::fs::copy(&copy, &archive).map_err(|e| format!("copying {}: {e}", copy.display()))?;
            return Ok(());
        }
    }
    util::status("Downloading", &port.url);
    std::fs::create_dir_all(archive.parent().unwrap()).map_err(|e| format!("{e}"))?;
    let partial = archive.with_extension("part");
    util::run(
        Command::new("curl")
            .args(["-fL", "--retry", "3", "--silent", "--show-error", "-o"])
            .arg(&partial)
            .arg(&port.url),
    )?;
    let got = sha256_of(&partial)?;
    if got != port.sha256 {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("{}: SHA-256 {got}, expected {} (ports/{}/port.toml)", port.url, port.sha256, port.name));
    }
    std::fs::rename(&partial, &archive).map_err(|e| format!("{e}"))
}

/// The programs the build needs from the system: compilers and GNU tools,
/// curl for the downloads, and meson and ninja for the media libraries;
/// and the architecture's own (`arch::PORT_TOOLS`). `docs/C.md` names the
/// packages.
const BUILD_TOOLS: [&str; 10] = ["gcc", "g++", "make", "m4", "bison", "flex", "patch", "curl", "meson", "ninja"];

/// The shell that runs `ports/build.sh`, once the build tools are there.
fn shell() -> Result<Command> {
    let missing: Vec<&str> =
        BUILD_TOOLS.iter().chain(arch::PORT_TOOLS).copied().filter(|t| util::find_on_path(t).is_none()).collect();
    match missing.as_slice() {
        [] => arch::check_port_tools().map(|()| Command::new("bash")),
        [tool] => Err(format!("{tool} is missing: install it from the distribution's packages (see docs/C.md)")),
        [tools @ .., last] => Err(format!(
            "{} and {last} are missing: install them from the distribution's packages (see docs/C.md)",
            tools.join(", ")
        )),
    }
}

/// `cargo xtask toolchain [--jobs N]`.
pub fn command(args: &[String]) -> Result {
    let mut jobs = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--jobs" | "-j" => {
                jobs = it.next().and_then(|v| v.parse().ok()).ok_or("--jobs takes a number")?;
            }
            other => return Err(format!("unknown option '{other}' (toolchain takes --jobs N)")),
        }
    }
    let started = std::time::Instant::now();
    // One build at a time: a second would empty the first's build
    // directories under it.
    std::fs::create_dir_all(root()).map_err(|e| format!("creating {}: {e}", root().display()))?;
    let lock_path = root().join(".lock");
    let lock = std::fs::File::create(&lock_path).map_err(|e| format!("{}: {e}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err("another `cargo xtask toolchain` is building the toolchain: wait for it to finish".into());
        }
        Err(std::fs::TryLockError::Error(e)) => return Err(format!("locking {}: {e}", lock_path.display())),
    }
    // The tools first: the downloads are large.
    let mut sh = shell()?;
    for port in ports()? {
        fetch(&port, &root().join("downloads"))?;
    }
    // The script makes the C library's object of the POSIX layer once its
    // cross linker exists.
    let library = posix_library()?;
    let script = util::workspace_root().join("ports").join("build.sh");
    sh.arg(&script)
        .env("VEDA_PORTS", util::workspace_root().join("ports"))
        .env("VEDA_ARCH", arch::NAME)
        .env("VEDA_TOOLCHAIN", root())
        .env("VEDA_POSIX_LIB", &library)
        .env("VEDA_POSIX_EXPORTS", POSIX_LAYER_EXPORTS.join(" "))
        .env("JOBS", jobs.to_string());
    util::run(&mut sh)?;
    // The native programs now have the POSIX layer's object as it is.
    let object = posix_layer_object();
    let note = native_layer_note();
    std::fs::write(&note, sha256_of(&object)?).map_err(|e| format!("writing {}: {e}", note.display()))?;
    util::status("Finished", format!("the C toolchain in {:.0} min", started.elapsed().as_secs_f32() / 60.0));
    Ok(())
}

/// Whether `target` exists and is newer than every input.
fn is_newer(target: &Path, inputs: &[&Path]) -> bool {
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let Some(t) = modified(target) else { return false };
    inputs.iter().all(|i| modified(i).is_some_and(|m| m <= t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_are_left_out() {
        let c = "int a; /* a float */ int b; // a double\nint c; /* no end";
        assert_eq!(without_c_comments(c), "int a;   int b;  \nint c;  ");
        assert_eq!(without_c_comments("x // y"), "x  ");
        assert_eq!(without_c_comments("/**/f"), " f");
    }
}

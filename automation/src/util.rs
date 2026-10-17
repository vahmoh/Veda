// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Small helpers shared by the `cargo automation` commands: where things
//! are, running programs (Cargo among them, with the build machine's paths
//! kept out of what it compiles), files (whose errors say which file and
//! what was being done with it), locks between commands that run at once,
//! and the recipes of outputs (what made them, so that a change of the
//! command makes them again).

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

pub type Result<T = ()> = std::result::Result<T, String>;

/// The repository root (the directory containing the workspace `Cargo.toml`).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("automation lives in the workspace").to_path_buf()
}

/// The directory the environment variable `var` names (relative to the
/// repository when it is relative), or else `default`'s components in the
/// repository.
pub fn env_dir(var: &str, default: &[&str]) -> PathBuf {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(p) => {
            let p = PathBuf::from(p);
            if p.is_absolute() { p } else { workspace_root().join(p) }
        }
        None => default.iter().fold(workspace_root(), |path, part| path.join(part)),
    }
}

/// Directory that receives the assembled image and QEMU state
/// (`$VEDA_OUT` overrides it, so several builds can run side by side).
pub fn out_dir() -> PathBuf {
    env_dir("VEDA_OUT", &["target", "veda"])
}

/// Caches every run shares, wherever `$VEDA_OUT` puts the rest (the test
/// microphone's speech).
pub fn cache_dir() -> PathBuf {
    workspace_root().join("target").join("veda")
}

/// Cargo's target directory (`$CARGO_TARGET_DIR` overrides it).
pub fn target_dir() -> PathBuf {
    env_dir("CARGO_TARGET_DIR", &["target"])
}

/// Where the media generators keep what they make (each in a directory of
/// its own), which the system image installs at the same relative paths:
/// in the target directory, so that builds with targets of their own have
/// media of their own.
pub fn generated_dir() -> PathBuf {
    target_dir().join("generated")
}

/// Runs `cmd`, streaming its output, and fails if it does not succeed.
pub fn run(cmd: &mut Command) -> Result {
    let shown = format!("{cmd:?}");
    let status = cmd.status().map_err(|e| format!("failed to start {shown}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("command failed ({status}): {shown}")) }
}

/// The `cargo` executable that invoked us (falls back to `cargo` on PATH).
pub fn cargo() -> Command {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(workspace_root());
    cmd
}

/// Cargo, for a build of Veda's for `target`, whose output goes into the
/// system image: what it compiles names the build machine's directories
/// by fixed names ([`path_remaps`]), so that no path of this machine's (a
/// user's home, the checkout) goes into the image, whatever panics. The
/// remapping is given for `target`'s builds alone, through Cargo's
/// configuration, which adds it to the flags its configuration files give;
/// the build scripts and procedural macros it runs on this machine keep
/// theirs.
pub fn cargo_for(target: &str) -> Command {
    let mut cmd = cargo();
    cmd.arg("--config").arg(remap_config(target, &path_remaps()));
    cmd
}

/// The configuration value that gives `target`'s builds `remaps`.
fn remap_config(target: &str, remaps: &[(PathBuf, &str)]) -> String {
    let flags: Vec<String> = remaps
        .iter()
        .map(|(from, to)| toml_string(&format!("--remap-path-prefix={}={to}", from.to_string_lossy())))
        .collect();
    format!("target.{target}.rustflags=[{}]", flags.join(", "))
}

/// `text` as a TOML basic string.
fn toml_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The build machine's directories that Veda's programs are compiled from,
/// and the fixed names they have in them instead: Cargo's home (the
/// crates' sources), the checkout and the target directory (the code
/// build scripts generate). The compilers take the last of those that
/// matches, so the most particular comes last.
pub fn path_remaps() -> Vec<(PathBuf, &'static str)> {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cargo")));
    let mut remaps = Vec::new();
    if let Some(home) = cargo_home {
        remaps.push((home, "/cargo"));
    }
    remaps.push((workspace_root(), "/veda"));
    remaps.push((target_dir(), "/veda/target"));
    remaps
}

/// What a C compiler is given for the same: the checkout's paths (its
/// sources' and the toolchain's) by the fixed name of [`path_remaps`].
pub fn c_path_remap() -> OsString {
    let mut flag = OsString::from("-ffile-prefix-map=");
    flag.push(workspace_root());
    flag.push("=/veda");
    flag
}

/// The contents of the file at `path`.
pub fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))
}

/// The contents of the text file at `path`.
pub fn read_to_string(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))
}

/// Writes `data` to the file at `path`.
pub fn write(path: &Path, data: impl AsRef<[u8]>) -> Result {
    std::fs::write(path, data).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Writes `data` to `path` whole or not at all: into a file of this
/// process's beside it, then renamed over it, so that a reader finds the
/// old file or the new one, and a run that stops leaves no half file.
pub fn write_atomically(path: &Path, data: impl AsRef<[u8]>) -> Result {
    let name = path.file_name().ok_or_else(|| format!("{}: not a file's path", path.display()))?;
    let mut partial = OsString::from(".");
    partial.push(name);
    partial.push(format!(".{}.part", std::process::id()));
    let partial = path.with_file_name(partial);
    write(&partial, data).and_then(|()| rename(&partial, path)).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })
}

/// Makes the directory `path` and its parents.
pub fn create_dir_all(path: &Path) -> Result {
    std::fs::create_dir_all(path).map_err(|e| format!("creating {}: {e}", path.display()))
}

/// Moves `from` to `to`.
pub fn rename(from: &Path, to: &Path) -> Result {
    std::fs::rename(from, to).map_err(|e| format!("moving {} to {}: {e}", from.display(), to.display()))
}

/// Copies the file `from` to `to`.
pub fn copy(from: &Path, to: &Path) -> Result<u64> {
    std::fs::copy(from, to).map_err(|e| format!("copying {} to {}: {e}", from.display(), to.display()))
}

/// Removes the file at `path`, if there is one.
pub fn remove_file(path: &Path) -> Result {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("removing {}: {e}", path.display())),
        _ => Ok(()),
    }
}

/// Every file under `dir` (not its directories), in the order of their
/// paths; none if `dir` is not there.
pub fn files_under(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = match std::fs::read_dir(&d) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && d == dir => return Ok(files),
            Err(e) => return Err(format!("reading {}: {e}", d.display())),
        };
        for entry in entries {
            let path = entry.map_err(|e| format!("reading {}: {e}", d.display()))?.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// The files directly in `dir` (not in its subdirectories) whose names
/// end in one of `extensions`, in the order of their names.
pub fn files_in(dir: &Path, extensions: &[&str]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))? {
        let path = entry.map_err(|e| format!("reading {}: {e}", dir.display()))?.path();
        if path.is_file() && path.extension().is_some_and(|x| extensions.iter().any(|e| x == *e)) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// `bytes` in lower-case hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of the file at `path`, in hexadecimal (read a piece at a
/// time: some are archives of hundreds of megabytes).
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("reading {}: {e}", path.display()))?;
        if n == 0 {
            return Ok(hex(&hash.finalize()));
        }
        hash.update(&buf[..n]);
    }
}

/// `--jobs N`'s value: a number of jobs, one at least (`make -j0` would
/// wait for ever, and no script runs at all with none at a time).
pub fn jobs(value: Option<&String>) -> Result<usize> {
    let value = value.ok_or("--jobs needs a number")?;
    value.parse().ok().filter(|&n: &usize| n > 0).ok_or_else(|| format!("--jobs: not a number of jobs: {value}"))
}

/// A lock between `cargo automation` commands over what they share (the
/// toolchain, the driver VM's build, the generated media), held while it
/// lives. Its file stays: a lock goes with the process that held it, so
/// one left by a stopped command holds nothing.
pub struct Lock {
    _file: std::fs::File,
}

impl Lock {
    /// Takes the lock at `path` alone, waiting while another command has it
    /// (and saying so): for changing `what`.
    pub fn exclusive(path: &Path, what: &str) -> Result<Lock> {
        Lock::take(path, what, true)
    }

    /// Takes the lock at `path` beside other readers, waiting while a
    /// command changes `what`.
    pub fn shared(path: &Path, what: &str) -> Result<Lock> {
        Lock::take(path, what, false)
    }

    fn take(path: &Path, what: &str, exclusive: bool) -> Result<Lock> {
        if let Some(dir) = path.parent() {
            create_dir_all(dir)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|e| format!("opening {}: {e}", path.display()))?;
        let tried = if exclusive { file.try_lock() } else { file.try_lock_shared() };
        match tried {
            Ok(()) => return Ok(Lock { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => {
                status("Waiting", format!("for {what}: another `cargo automation` has it ({})", path.display()));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(format!("locking {}: {e}", path.display())),
        }
        let locked = if exclusive { file.lock() } else { file.lock_shared() };
        locked.map_err(|e| format!("locking {}: {e}", path.display()))?;
        Ok(Lock { _file: file })
    }
}

/// How an output was made: the command that wrote it and what it read
/// (each input's path, size and modification time), as a digest kept
/// beside it (`NAME.recipe`). The output is made again when it is missing
/// or its recipe has changed: when an input did, or the command (its
/// flags, the variants a list gives), which comparing times alone misses.
pub struct Recipe {
    output: PathBuf,
    digest: String,
}

impl Recipe {
    /// The recipe of `output`, made by `command` from `inputs`.
    pub fn of(output: &Path, command: &Command, inputs: &[&Path]) -> Recipe {
        Recipe::new(output, &command_words(command), inputs)
    }

    /// The recipe of `output`, made by what `words` say (the commands of a
    /// step that runs several) from `inputs`.
    pub fn new(output: &Path, words: &[OsString], inputs: &[&Path]) -> Recipe {
        let mut hash = Sha256::new();
        let mut add = |part: &[u8]| {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part);
        };
        for word in words {
            add(word.as_encoded_bytes());
        }
        for input in inputs {
            add(input.as_os_str().as_encoded_bytes());
            let stamp = std::fs::metadata(input).ok().and_then(|m| {
                let since = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
                Some(format!("{} {}", m.len(), since.as_nanos()))
            });
            add(stamp.as_deref().unwrap_or("missing").as_bytes());
        }
        Recipe { output: output.to_path_buf(), digest: hex(&hash.finalize()) }
    }

    /// Where the recipe of the output is kept.
    fn note(&self) -> PathBuf {
        let mut name = self.output.file_name().map(OsStr::to_os_string).unwrap_or_default();
        name.push(".recipe");
        self.output.with_file_name(name)
    }

    /// Whether the output is there, made by this recipe.
    pub fn fresh(&self) -> bool {
        self.output.is_file() && std::fs::read_to_string(self.note()).is_ok_and(|kept| kept == self.digest)
    }

    /// Notes that the output has just been made by this recipe.
    pub fn record(&self) -> Result {
        write(&self.note(), &self.digest)
    }
}

/// What a command runs: its program, arguments, environment and directory.
fn command_words(command: &Command) -> Vec<OsString> {
    let mut words = vec![command.get_program().to_os_string()];
    words.extend(command.get_args().map(OsStr::to_os_string));
    for (key, value) in command.get_envs() {
        let mut pair = key.to_os_string();
        pair.push("=");
        pair.push(value.unwrap_or_default());
        words.push(pair);
    }
    if let Some(dir) = command.get_current_dir() {
        words.push(dir.as_os_str().to_os_string());
    }
    words
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

/// The host's current offset from UTC as `+HH:MM` (daylight saving
/// included), for the guest's local time.
pub fn host_utc_offset() -> Option<String> {
    let minutes = host_offset_minutes()?;
    let sign = if minutes < 0 { '-' } else { '+' };
    Some(format!("{sign}{:02}:{:02}", minutes.abs() / 60, minutes.abs() % 60))
}

fn host_offset_minutes() -> Option<i64> {
    let out = Command::new("date").arg("+%z").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let sign = if s.starts_with('-') { -1 } else { 1 };
    let digits = s.trim_start_matches(['+', '-']);
    let (h, m) = (digits.get(..2)?.parse::<i64>().ok()?, digits.get(2..4)?.parse::<i64>().ok()?);
    Some(sign * (h * 60 + m))
}

/// Searches `PATH` for an executable.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

/// Runs ninja for `target` in the build directory `dir` (one of Mesa's),
/// showing its output only if it fails.
pub fn ninja(dir: &Path, target: &str) -> Result {
    let log = dir.join("ninja.log.txt");
    let file = std::fs::File::create(&log).map_err(|e| format!("creating {}: {e}", log.display()))?;
    let out = file.try_clone().map_err(|e| format!("{}: {e}", log.display()))?;
    let status = Command::new("ninja")
        .arg("-C")
        .arg(dir)
        .arg(target)
        .stdout(out)
        .stderr(file)
        .status()
        .map_err(|e| format!("running ninja: {e}"))?;
    if status.success() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    for line in &lines[lines.len().saturating_sub(40)..] {
        eprintln!("{line}");
    }
    Err(format!("building {target} failed (log: {})", log.display()))
}

/// Prints a status line in the style of cargo.
pub fn status(verb: &str, msg: impl std::fmt::Display) {
    eprintln!("\x1b[1;32m{verb:>12}\x1b[0m {msg}");
}

/// The same, for what failed (in red).
pub fn failure(verb: &str, msg: impl std::fmt::Display) {
    eprintln!("\x1b[1;31m{verb:>12}\x1b[0m {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of the test's own, removed first if an earlier run left
    /// it.
    pub fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("automation-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn veda_builds_name_the_machines_directories_by_fixed_names() {
        let remaps = [(PathBuf::from("/home/a b/.cargo"), "/cargo"), (PathBuf::from("/home/a b/Veda"), "/veda")];
        assert_eq!(
            remap_config("x86_64-unknown-none", &remaps),
            "target.x86_64-unknown-none.rustflags=[\"--remap-path-prefix=/home/a b/.cargo=/cargo\", \
             \"--remap-path-prefix=/home/a b/Veda=/veda\"]"
        );
        assert_eq!(toml_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000A\"");
        // The checkout and the target directory, the target directory last
        // (it may be in the checkout), and every Veda target's build gets
        // them.
        let remaps = path_remaps();
        let names: Vec<&str> = remaps.iter().map(|(_, to)| *to).collect();
        assert!(names.ends_with(&["/veda", "/veda/target"]), "{names:?}");
        assert_eq!(remaps[remaps.len() - 2].0, workspace_root());
        let cmd = cargo_for("x86_64-unknown-uefi");
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args[0], "--config");
        assert!(args[1].starts_with("target.x86_64-unknown-uefi.rustflags=["), "{}", args[1]);
        assert!(args[1].contains(&format!("--remap-path-prefix={}=/veda\"", workspace_root().display())));
        assert_eq!(c_path_remap(), OsString::from(format!("-ffile-prefix-map={}=/veda", workspace_root().display())));
    }

    #[test]
    fn jobs_are_at_least_one() {
        assert_eq!(jobs(Some(&"3".to_string())), Ok(3));
        for bad in ["0", "-1", "two", ""] {
            assert!(jobs(Some(&bad.to_string())).is_err(), "{bad}");
        }
        assert!(jobs(None).is_err());
    }

    #[test]
    fn files_are_written_whole_and_their_errors_say_which() {
        let dir = scratch("files");
        let path = dir.join("a.txt");
        write_atomically(&path, b"first").unwrap();
        write_atomically(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, ["a.txt"]);
        let missing = dir.join("none").join("b.txt");
        assert!(write_atomically(&missing, b"x").unwrap_err().contains(&dir.join("none").display().to_string()));
        assert!(read(&missing).unwrap_err().starts_with(&format!("reading {}", missing.display())));
        assert!(remove_file(&missing).is_ok());
        create_dir_all(&dir.join("d/e")).unwrap();
        write(&dir.join("d/e/f"), b"").unwrap();
        write(&dir.join("d/c"), b"").unwrap();
        assert_eq!(files_under(&dir).unwrap(), [dir.join("a.txt"), dir.join("d/c"), dir.join("d/e/f")]);
        assert_eq!(files_under(&dir.join("absent")).unwrap(), Vec::<PathBuf>::new());
        write(&dir.join("b.c"), b"").unwrap();
        assert_eq!(files_in(&dir, &["c", "txt"]).unwrap(), [dir.join("a.txt"), dir.join("b.c")]);
        assert!(files_in(&dir.join("absent"), &["c"]).is_err());
        assert_eq!(sha256_file(&path).unwrap(), hex(&Sha256::digest(b"second")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn outputs_are_made_again_when_their_command_changes() {
        let dir = scratch("recipe");
        let (input, output) = (dir.join("in.c"), dir.join("out"));
        write(&input, b"int x;").unwrap();
        let mut cc = Command::new("cc");
        cc.args(["-O2", "-c"]).arg(&input);
        let recipe = Recipe::of(&output, &cc, &[&input]);
        assert!(!recipe.fresh(), "no output yet");
        write(&output, b"made").unwrap();
        recipe.record().unwrap();
        assert!(Recipe::of(&output, &cc, &[&input]).fresh());
        // Another flag, an input changed, another input: made again.
        let mut other = Command::new("cc");
        other.args(["-O0", "-c"]).arg(&input);
        assert!(!Recipe::of(&output, &other, &[&input]).fresh());
        let mut env = Command::new("cc");
        env.args(["-O2", "-c"]).arg(&input).env("CFLAGS", "-g");
        assert!(!Recipe::of(&output, &env, &[&input]).fresh());
        assert!(!Recipe::of(&output, &cc, &[&input, &dir.join("missing.h")]).fresh());
        write(&input, b"int xy;").unwrap();
        assert!(!Recipe::of(&output, &cc, &[&input]).fresh());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn locks_are_shared_or_exclusive() {
        let dir = scratch("lock");
        let path = dir.join("sub").join(".lock");
        let a = Lock::shared(&path, "the test").unwrap();
        let b = Lock::shared(&path, "the test").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        assert!(matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
        drop((a, b));
        let c = Lock::exclusive(&path, "the test").unwrap();
        assert!(matches!(file.try_lock_shared(), Err(std::fs::TryLockError::WouldBlock)));
        drop(c);
        assert!(file.try_lock().is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

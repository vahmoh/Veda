// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Scripted, headless runs of Veda in QEMU: the interpreter of the
//! automation scripts (`script` has their language), which drives the
//! virtual machine through QMP and watches its serial console. It powers
//! `cargo automation shot`, `script` and the integration tests.
//!
//! QEMU lives as long as the run that started it: it is killed when the
//! run ends however it ends, an error on the way included, and it dies
//! with the process that started it (a deadline's kill of a script's
//! process among the ways).
//!
//! A wait (`wait-serial`) is satisfied by a whole line of the serial log
//! (one whose end has been written) after the last `checkpoint` or `reset`
//! that holds its text where no earlier wait matched: the matches of waits
//! are claimed, so that a wait after a repeated action needs a new line.
//! Expectations (`expect-serial`, `reject-serial`) look at the whole log
//! since the last checkpoint or reset, and failure patterns (`fail-on`) at
//! all of it.
//!
//! Scripts that use the microphone's commands boot with `testmic`, which
//! connects back to the host (see `mic`). The pointer's and the
//! keyboard's commands wait, the first time in a boot, until the guest's
//! tablet or keyboard is Veda's (Linux drives them in the driver VM, which
//! may come after the desktop).

use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use crate::agentsim::AgentSim;
use crate::arch;
use crate::checks;
use crate::mic::{self, MicServer};
use crate::qemu::{self, QemuInstall, VmConfig};
use crate::qmp::Qmp;
use crate::script::{Point, Script, Step};
use crate::util::{self, Result};

/// What Veda's boot loader prints first: a machine that was reset has
/// started again.
const LOADER_BANNER: &str = "[bootloader] Veda boot loader";
/// How long a reset may take to reach the loader (it takes about a second).
const RESTART: Duration = Duration::from_secs(30);
/// How long a screenshot waits at most for the screen to settle, and how
/// often it looks meanwhile.
const SETTLE: Duration = Duration::from_secs(3);
const SETTLE_STEP: Duration = Duration::from_millis(100);
/// How long QEMU may take to open its QMP socket.
const QMP_CONNECT: Duration = Duration::from_secs(20);

/// QEMU, running: killed when this ends, unless it has ended already, and
/// waited for, so that nothing of it is left behind.
struct Qemu(Child);

impl Qemu {
    /// Why QEMU stopped, if it did.
    fn stopped(&mut self) -> Option<String> {
        match self.0.try_wait() {
            Ok(Some(status)) => Some(format!("QEMU exited ({status})")),
            _ => None,
        }
    }
}

impl Drop for Qemu {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

/// The machine a script drives: QEMU, through QMP.
struct Machine {
    // The connection goes first, then QEMU.
    qmp: Qmp,
    qemu: Qemu,
}

impl Machine {
    /// Plugs in or unplugs the wired card's cable.
    fn set_wired_link(&mut self, up: bool) -> Result {
        let name = qemu::WIRED_NIC_ID;
        self.qmp.execute("set_link", serde_json::json!({ "name": name, "up": up })).map(|_| ())
    }

    fn reset(&mut self) -> Result {
        self.qmp.execute("system_reset", serde_json::json!({})).map(|_| ())
    }

    /// QEMU's state and its processors', for a machine that does not do
    /// what it should: whether QEMU answers at all, how it runs, and where
    /// each processor is.
    fn examine(&mut self) -> String {
        self.qmp.set_timeout(Duration::from_secs(5));
        let status = match self.qmp.status() {
            Ok(s) => s,
            Err(e) => return format!("QEMU does not answer ({e})"),
        };
        match self.qmp.human("info registers -a") {
            Ok(registers) => format!("QEMU is {status}, {}", arch::qemu::processors(&registers)),
            Err(e) => format!("QEMU is {status}, its processors unread ({e})"),
        }
    }

    /// Asks QEMU to quit, and waits a moment for it (it is killed if it
    /// does not).
    fn finish(mut self) {
        self.qmp.quit();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if let Ok(Some(_)) = self.qemu.0.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The serial log QEMU writes, as much as has been read of it.
struct SerialLog {
    path: PathBuf,
    file: Option<File>,
    bytes: Vec<u8>,
    /// Whether QEMU has stopped: its last line, whole or not, is all there
    /// will be of it.
    ended: bool,
}

impl SerialLog {
    /// Reads what QEMU has written since the last time.
    fn refresh(&mut self) {
        if self.file.is_none() {
            self.file = File::open(&self.path).ok();
        }
        if let Some(file) = self.file.as_mut() {
            let _ = file.read_to_end(&mut self.bytes);
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

/// Where `needle` is in `log` from byte `from` on, at the first place that
/// overlaps nothing in `claimed`: `Ok` with the place once its line is
/// whole (a line feed follows, or the log has `ended`), `Err(true)` if its
/// line is still being written, `Err(false)` if it is nowhere.
fn find_line(
    log: &[u8],
    from: usize,
    needle: &str,
    claimed: &[Range<usize>],
    ended: bool,
) -> std::result::Result<Range<usize>, bool> {
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return Err(false);
    }
    let mut at = from.min(log.len());
    while let Some(i) = find(&log[at..], needle) {
        let found = at + i..at + i + needle.len();
        if !claimed.iter().any(|c| c.start < found.end && found.start < c.end) {
            return if ended || log[found.end..].contains(&b'\n') { Ok(found) } else { Err(true) };
        }
        at = found.start + 1;
    }
    Err(false)
}

/// The places of `needle` in whole lines of `log` from byte `from` on,
/// none overlapping another.
fn find_lines(log: &[u8], from: usize, needle: &str, ended: bool) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut at = from.min(log.len());
    while let Ok(place) = find_line(log, at, needle, &[], ended) {
        at = place.end;
        found.push(place);
    }
    found
}

/// Where `needle` first is in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let first = *needle.first()?;
    let mut at = 0;
    while at + needle.len() <= haystack.len() {
        let i = haystack[at..=haystack.len() - needle.len()].iter().position(|&b| b == first)?;
        if haystack[at + i..].starts_with(needle) {
            return Some(at + i);
        }
        at += i + 1;
    }
    None
}

/// A running headless VM under script control.
struct Session {
    machine: Machine,
    log: SerialLog,
    /// Serial log patterns that abort any wait immediately, each with how
    /// far the log has been searched for it.
    fail_patterns: Vec<(String, usize)>,
    /// Where waits and expectations start looking: after the last
    /// checkpoint or reset.
    checkpoint: usize,
    /// What earlier waits matched, which later ones do not match again.
    claimed: Vec<Range<usize>>,
    /// Where this boot's output starts, and whether its pointer and its
    /// keyboard are up.
    boot: usize,
    pointer: bool,
    keyboard: bool,
}

impl Session {
    fn start(install: &QemuInstall, disk: &Path, mut vm: VmConfig) -> Result<Self> {
        let out = util::out_dir();
        util::create_dir_all(&out)?;
        let serial_log = out.join("serial.log");
        util::remove_file(&serial_log)?;
        // QMP's socket, the run's own (no port another run could take).
        let socket = out.join("qmp.sock");
        qemu::socket_option(&socket)?;
        util::remove_file(&socket)?;
        vm.display = false;
        vm.serial_file = Some(serial_log.clone());
        vm.qmp = Some(socket.clone());
        vm.debug_exit = true;
        let vars = qemu::script_vars_file(install, &vm.firmware_variables)?;
        let mut cmd = qemu::command(install, disk, &vars, &vm)?;
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::inherit());
        qemu::ends_with_us(&mut cmd);
        let mut qemu = Qemu(cmd.spawn().map_err(|e| format!("starting QEMU: {e}"))?);
        let qmp = Qmp::connect(&socket, QMP_CONNECT, || qemu.stopped())?;
        // Kernel panics print "PANIC", user-space panics "panicked at".
        let fail_patterns = ["PANIC", "panicked at"].map(|p| (p.to_string(), 0)).to_vec();
        Ok(Session {
            machine: Machine { qmp, qemu },
            log: SerialLog { path: serial_log, file: None, bytes: Vec::new(), ended: false },
            fail_patterns,
            checkpoint: 0,
            claimed: Vec::new(),
            boot: 0,
            pointer: false,
            keyboard: false,
        })
    }

    /// The serial log since the last checkpoint or reset.
    fn recent(&mut self) -> &[u8] {
        self.log.refresh();
        &self.log.bytes[self.checkpoint.min(self.log.bytes.len())..]
    }

    /// Makes later waits and expectations look only at output from now on.
    fn checkpoint(&mut self) {
        self.log.refresh();
        self.checkpoint = self.log.bytes.len();
    }

    /// The first failure pattern the log holds, if it holds one.
    fn failed(&mut self) -> Option<String> {
        let log = &self.log.bytes;
        for (pattern, searched) in &mut self.fail_patterns {
            let from = searched.saturating_sub(pattern.len());
            if find(&log[from.min(log.len())..], pattern.as_bytes()).is_some() {
                return Some(format!("serial log contains failure pattern \"{pattern}\""));
            }
            *searched = log.len();
        }
        None
    }

    /// Looks in the log with `look` until it finds something (`Some`) or
    /// `timeout` passes (`None`); an error if the log shows a failure
    /// pattern first, or QEMU stops.
    fn poll<T>(&mut self, what: &str, timeout: Duration, look: impl Fn(&Session) -> Option<T>) -> Result<Option<T>> {
        let start = Instant::now();
        loop {
            self.log.refresh();
            if let Some(found) = look(self) {
                return Ok(Some(found));
            }
            if let Some(failure) = self.failed() {
                return Err(failure);
            }
            if let Some(why) = self.machine.qemu.stopped() {
                // What it wrote before it stopped, its last line too.
                self.log.ended = true;
                self.log.refresh();
                if let Some(found) = look(self) {
                    return Ok(Some(found));
                }
                return Err(self.failed().unwrap_or(format!("{why} before \"{what}\" appeared")));
            }
            if start.elapsed() > timeout {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Waits for a whole line holding `needle` after the checkpoint where
    /// no earlier wait matched, and claims the match.
    fn wait_serial(&mut self, needle: &str, timeout: Duration) -> Result {
        let found = self
            .poll(needle, timeout, |s| find_line(&s.log.bytes, s.checkpoint, needle, &s.claimed, s.log.ended).ok())?;
        let place = found.ok_or_else(|| format!("timed out after {}s waiting for \"{needle}\"", timeout.as_secs()))?;
        self.claimed.push(place);
        Ok(())
    }

    /// Waits until `needle` has appeared `count` times since the checkpoint
    /// (in whole lines), and claims them.
    fn wait_serial_count(&mut self, needle: &str, count: usize, timeout: Duration) -> Result {
        let enough = |s: &Session| {
            Some(find_lines(&s.log.bytes, s.checkpoint, needle, s.log.ended)).filter(|f| f.len() >= count)
        };
        match self.poll(needle, timeout, enough)? {
            Some(places) => {
                self.claimed.extend(places.into_iter().take(count));
                Ok(())
            }
            None => {
                let seen = find_lines(&self.log.bytes, self.checkpoint, needle, self.log.ended).len();
                Err(format!("timed out: \"{needle}\" appeared {seen} of {count} times in {}s", timeout.as_secs()))
            }
        }
    }

    /// Waits until a whole line from byte `from` on holds `needle`, for the
    /// interpreter's own purposes (claiming nothing); whether it did.
    fn appears(&mut self, from: usize, needle: &str, timeout: Duration) -> Result<bool> {
        self.poll(needle, timeout, |s| find_line(&s.log.bytes, from, needle, &[], s.log.ended).ok())
            .map(|f| f.is_some())
    }

    /// Resets the machine (a reboot that keeps the disks), and waits until
    /// Veda's boot loader starts again.
    fn reset(&mut self) -> Result {
        self.checkpoint();
        let before = self.checkpoint;
        self.boot = before;
        self.pointer = false;
        self.keyboard = false;
        self.machine.reset()?;
        if self.appears(before, LOADER_BANNER, RESTART)? {
            return Ok(());
        }
        let printed = self.log.bytes.len().saturating_sub(before);
        let what = match printed {
            0 => String::from("the firmware printed nothing"),
            n => format!("the firmware printed {n} bytes, but Veda's boot loader did not start"),
        };
        Err(format!(
            "the machine did not start again: {what} in the {} s after the reset; {}",
            RESTART.as_secs(),
            self.machine.examine()
        ))
    }

    /// The first pointer action of a boot waits until the guest's tablet is
    /// Veda's: Linux drives it, in the driver VM, which may not have done so
    /// yet when the desktop is ready.
    fn pointer(&mut self) -> Result {
        if !self.pointer {
            self.wait_device("): tablet")?;
            self.pointer = true;
        }
        Ok(())
    }

    /// The first key of a boot waits until the guest's keyboard is Veda's,
    /// as the pointer does for its tablet.
    fn keyboard(&mut self) -> Result {
        if !self.keyboard {
            self.wait_device("): keyboard")?;
            self.keyboard = true;
        }
        Ok(())
    }

    fn wait_device(&mut self, needle: &str) -> Result {
        match self.appears(self.boot, needle, Duration::from_secs(60))? {
            true => Ok(()),
            false => Err(format!("timed out after 60s waiting for \"{needle}\"")),
        }
    }

    /// A screenshot at `path`: once two taken one after the other are
    /// alike (the screen has settled), at most for [`SETTLE`], if `settle`;
    /// else at once.
    fn shot(&mut self, path: &Path, settle: bool) -> Result {
        if let Some(dir) = path.parent() {
            util::create_dir_all(dir)?;
        }
        let start = Instant::now();
        let mut previous: Option<Vec<u8>> = None;
        loop {
            self.machine.qmp.screenshot(path)?;
            if !settle {
                return Ok(());
            }
            // QEMU writes a picture the same way every time: alike
            // pictures are alike files.
            let picture = util::read(path)?;
            if previous.as_ref() == Some(&picture) {
                return Ok(());
            }
            if start.elapsed() >= SETTLE {
                util::status(
                    "Note",
                    format!("the screen did not settle in {} s for {}", SETTLE.as_secs(), path.display()),
                );
                return Ok(());
            }
            previous = Some(picture);
            std::thread::sleep(SETTLE_STEP);
        }
    }

    fn click(&mut self, at: Point, button: &str) -> Result {
        self.pointer()?;
        let qmp = &mut self.machine.qmp;
        qmp.move_mouse(at.x, at.y)?;
        std::thread::sleep(Duration::from_millis(80));
        qmp.mouse_button(button, true)?;
        std::thread::sleep(Duration::from_millis(80));
        qmp.mouse_button(button, false)
    }
}

/// A screenshot's path as a script gives it: relative to the workspace, with
/// `target/veda/` standing for the output directory (`$VEDA_OUT`).
fn shot_path(given: &str) -> PathBuf {
    if let Some(rest) = given.strip_prefix("target/veda/") {
        return util::out_dir().join(rest);
    }
    let path = PathBuf::from(given);
    if path.is_absolute() { path } else { util::workspace_root().join(path) }
}

/// QEMU's recording of the sound output.
fn recording() -> Result<Vec<u8>> {
    util::read(&util::out_dir().join("audio.wav"))
}

/// What a run has besides the machine: the test microphone, the stand-in
/// for Deepgram, and what the agent's steps remember.
struct Extras {
    mic: Option<MicServer>,
    agent: Option<AgentSim>,
    /// The result of the last `agent-call`.
    agent_result: String,
    /// Where `agent-mark` left the agent's messages and microphone levels.
    agent_mark: usize,
    mic_mark: usize,
}

impl Extras {
    fn mic(&self) -> Result<&MicServer> {
        self.mic.as_ref().ok_or_else(|| "this run has no test microphone".to_string())
    }

    fn agent(&mut self) -> Result<&mut AgentSim> {
        self.agent.as_mut().ok_or_else(|| "this run has no agent simulator".to_string())
    }
}

/// Runs `script` against a fresh VM booted from `disk`; returns the serial
/// log.
pub fn run_script(
    install: &QemuInstall,
    disk: &Path,
    vm: VmConfig,
    script: &Script,
    mic: Option<MicServer>,
    agent: Option<AgentSim>,
) -> Result<String> {
    let mut s = Session::start(install, disk, vm)?;
    let mut extras = Extras { mic, agent, agent_result: String::new(), agent_mark: 0, mic_mark: 0 };
    let mut result = Ok(());
    for (line, step) in &script.steps {
        if let Err(e) = run_step(&mut s, &mut extras, step, disk) {
            result = Err(format!("script line {line}: {e}"));
            break;
        }
    }
    s.log.refresh();
    // Something may also have failed after the last wait.
    let result = result.and_then(|()| s.failed().map_or(Ok(()), Err));
    let log = s.log.text();
    s.machine.finish();
    result.map(|()| log.clone()).map_err(|e| format!("{e}\n--- serial log ---\n{}", tail(&log, 60)))
}

/// Takes one step of a script.
fn run_step(s: &mut Session, x: &mut Extras, step: &Step, disk: &Path) -> Result {
    match step {
        Step::Wait(time) => std::thread::sleep(*time),
        Step::WaitSerial { text, timeout } => s.wait_serial(text, *timeout)?,
        Step::WaitSerialCount { text, count, timeout } => s.wait_serial_count(text, *count, *timeout)?,
        Step::Shot { path, settle } => {
            let path = shot_path(path);
            s.shot(&path, *settle)?;
            util::status("Screenshot", path.display());
        }
        // Two screenshots show the same in a region: what came and went
        // between them left nothing behind.
        Step::ExpectSame { first, second, region } => {
            checks::same_pictures(&shot_path(first), &shot_path(second), *region)?
        }
        // Two screenshots differ in a region: what was to come, move or go
        // there did.
        Step::ExpectDifferent { first, second, region } => {
            checks::different_pictures(&shot_path(first), &shot_path(second), *region)?
        }
        // A screenshot is black in a region: nothing is shown there yet.
        Step::ExpectBlack { shot, region } => checks::black_picture(&shot_path(shot), *region)?,
        Step::Move(at) => {
            s.pointer()?;
            s.machine.qmp.move_mouse(at.x, at.y)?
        }
        Step::Click { at, button } => s.click(*at, button)?,
        Step::DoubleClick(at) => {
            s.pointer()?;
            s.machine.qmp.move_mouse(at.x, at.y)?;
            for _ in 0..2 {
                std::thread::sleep(Duration::from_millis(60));
                s.machine.qmp.mouse_button("left", true)?;
                std::thread::sleep(Duration::from_millis(60));
                s.machine.qmp.mouse_button("left", false)?;
            }
        }
        Step::Drag { from, to } => {
            s.pointer()?;
            let qmp = &mut s.machine.qmp;
            qmp.move_mouse(from.x, from.y)?;
            std::thread::sleep(Duration::from_millis(80));
            qmp.mouse_button("left", true)?;
            for step in 1..=10 {
                let t = step as f64 / 10.0;
                qmp.move_mouse(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t)?;
                std::thread::sleep(Duration::from_millis(40));
            }
            qmp.mouse_button("left", false)?;
        }
        Step::MouseDown(at) => {
            s.pointer()?;
            s.machine.qmp.move_mouse(at.x, at.y)?;
            std::thread::sleep(Duration::from_millis(80));
            s.machine.qmp.mouse_button("left", true)?;
        }
        Step::MouseUp => s.machine.qmp.mouse_button("left", false)?,
        Step::Key(combo) => {
            s.keyboard()?;
            s.machine.qmp.send_keys(combo)?
        }
        Step::KeyDown(key) => {
            s.keyboard()?;
            s.machine.qmp.key_event(key, true)?
        }
        Step::KeyUp(key) => s.machine.qmp.key_event(key, false)?,
        Step::Type(text) => {
            s.keyboard()?;
            s.machine.qmp.type_text(text)?
        }
        // Types a secret (an API key) from the environment, so it appears
        // in neither the script nor the logs.
        Step::TypeEnv(name) => {
            let value = std::env::var(name).map_err(|_| format!("${name} is not set"))?;
            s.keyboard()?;
            s.machine.qmp.type_text(value.trim()).map_err(|e| format!("cannot type ${name}: {e}"))?;
        }
        Step::ExpectSerial(needle) => {
            if find(s.recent(), needle.as_bytes()).is_none() {
                return Err(format!("serial log does not contain \"{needle}\""));
            }
        }
        Step::RejectSerial(needle) => {
            if find(s.recent(), needle.as_bytes()).is_some() {
                return Err(format!("serial log contains \"{needle}\""));
            }
        }
        Step::FailOn(pattern) => s.fail_patterns.push((pattern.clone(), 0)),
        Step::Reset => s.reset()?,
        Step::Checkpoint => s.checkpoint(),
        Step::Link { up } => s.machine.set_wired_link(*up)?,
        Step::Qmp { command, arguments } => s.machine.qmp.execute(command, arguments.clone()).map(|_| ())?,
        // A file the guest wrote on the live system's stick (its EFI
        // system partition) contains the text.
        Step::ExpectStick { path, text } => {
            let image = File::open(disk).map_err(|e| format!("{}: {e}", disk.display()))?;
            let file = crate::image::esp_file(&image, path)?;
            if !String::from_utf8_lossy(&file).contains(text.as_str()) {
                return Err(format!("{path} on the stick does not contain \"{text}\""));
            }
        }
        // The guest's sound output (QEMU records it to a WAV file in
        // scripts) holds more than silence.
        Step::ExpectAudio => {
            let peak = checks::recorded_peak(&recording()?)?;
            if peak < 1000 {
                return Err(format!("the sound output is silent (peak {peak})"));
            }
        }
        // While it plays, the sound output never drops out: no stretch of
        // digital silence longer than the limit between the first sound
        // and the last.
        Step::ExpectAudioGapless(limit) => {
            let gaps = checks::silent_gaps(&recording()?, *limit)?;
            if !gaps.is_empty() {
                let list: Vec<String> =
                    gaps.iter().take(5).map(|(at, ms)| format!("{ms:.0} ms at {at:.2} s")).collect();
                return Err(format!("the sound output drops out {} time(s): {}", gaps.len(), list.join(", ")));
            }
        }
        Step::Say(text) => {
            let m = x.mic()?;
            let speech = mic::synthesize(text, mic::USER_VOICE)?;
            m.enqueue(&speech);
            m.silence(0.6);
            m.wait_drained(Duration::from_secs_f64(speech.len() as f64 / mic::RATE as f64 + 30.0))?;
        }
        Step::MicWav(file) => {
            let m = x.mic()?;
            let audio = mic::read_wav(&util::workspace_root().join(file))?;
            m.enqueue(&audio);
            m.wait_drained(Duration::from_secs_f64(audio.len() as f64 / mic::RATE as f64 + 30.0))?;
        }
        Step::MicSilence(secs) => x.mic()?.silence(*secs),
        Step::MicTone { secs, peak } => x.mic()?.tone(*secs, *peak),
        Step::MicEcho { gain_db, delay_ms } => x.mic()?.echo(util::out_dir().join("audio.wav"), *gain_db, *delay_ms),
        Step::MicMusic(Some((title, gain_db))) => {
            let track = mic::read_sample_track(title)?;
            x.mic()?.room_sound(&track, *gain_db);
        }
        Step::MicMusic(None) => x.mic()?.room_quiet(),
        Step::AgentConnected(timeout) => x.agent()?.wait_connected(*timeout)?,
        Step::AgentCall { name, arguments, timeout } => {
            let result = x.agent()?.call(name, arguments, *timeout)?;
            println!("agent-call {name}: {}", result.chars().take(300).collect::<String>());
            x.agent_result = result;
        }
        Step::AgentResult(needle) => {
            x.agent()?;
            if !checks::result_has(&x.agent_result, needle) {
                return Err(format!("the last result does not contain \"{needle}\": {}", x.agent_result));
            }
        }
        Step::AgentMark => {
            let a = x.agent()?;
            let (texts, levels) = (a.texts().len(), a.mic_levels_len());
            (x.agent_mark, x.mic_mark) = (texts, levels);
        }
        // What the agent sent of the microphone since the mark: never
        // louder than (quiet) or at least (heard) this level in dBFS.
        Step::AgentMicQuiet(limit) => {
            let mark = x.mic_mark;
            let peak = x.agent()?.mic_peak_since(mark);
            if peak >= *limit {
                return Err(format!("the agent sent microphone audio at {peak:.1} dBFS (limit {limit})"));
            }
        }
        Step::AgentMicHeard(limit) => {
            let mark = x.mic_mark;
            let peak = x.agent()?.mic_peak_since(mark);
            if peak < *limit {
                return Err(format!("the agent sent no microphone audio louder than {peak:.1} dBFS (wanted {limit})"));
            }
        }
        Step::AgentExpect { text, timeout } => {
            let mark = x.agent_mark;
            let t = x.agent()?.wait_text(text, mark, *timeout)?;
            println!("agent-expect: {}", t.chars().take(300).collect::<String>());
        }
        Step::AgentSpeak(secs) => x.agent()?.speak(*secs)?,
        Step::AgentInterrupt => x.agent()?.send_json(r#"{"type":"UserStartedSpeaking"}"#)?,
        Step::AgentHear { text, timeout } => x.agent()?.hear(text, *timeout)?,
        Step::AgentListens(n) => {
            let opened = x.agent()?.listen_connections();
            if opened != *n {
                return Err(format!("the agent opened {opened} recognition streams, not {n}"));
            }
        }
        Step::AgentListensAtMost(n) => {
            let opened = x.agent()?.listen_connections();
            if opened > *n {
                return Err(format!("the agent opened {opened} recognition streams, more than {n}"));
            }
        }
        Step::AgentNotListening(timeout) => x.agent()?.wait_not_listening(*timeout)?,
        Step::AgentAudio(want) => {
            let got = x.agent()?.audio_bytes();
            if got < *want {
                return Err(format!("the agent streamed only {got} bytes of microphone audio"));
            }
        }
    }
    Ok(())
}

/// The last `n` lines of `text`.
pub fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_take_whole_lines_that_no_wait_took() {
        let log = b"[ 1.0] videos: seek to 0:05\r\n[ 2.0] videos: seek to 0:07\r\n[ 3.0] videos: seek to 0:0";
        // The first line, then (the first claimed) the second; the third is
        // still being written.
        let first = find_line(log, 0, "seek to 0:0", &[], false).unwrap();
        assert_eq!(&log[first.clone()], b"seek to 0:0");
        let second = find_line(log, 0, "seek to 0:0", std::slice::from_ref(&first), false).unwrap();
        assert!(second.start > first.end);
        assert_eq!(find_line(log, 0, "seek to 0:0", &[first.clone(), second.clone()], false), Err(true));
        assert_eq!(find_line(log, 0, "rewind", &[], false), Err(false));
        // Once QEMU has stopped, its last line is whole as it is.
        assert!(find_line(log, 0, "seek to 0:0", &[first.clone(), second.clone()], true).is_ok());
        // A wait for less than an earlier wait matched needs a line of its
        // own too ("0:1" of "going on from 0:15").
        let going = b"a: going on from 0:15\nb: going on from 0:1\n";
        let long = find_line(going, 0, "going on from 0:15", &[], false).unwrap();
        let short = find_line(going, 0, "going on from 0:1", &[long], false).unwrap();
        assert_eq!(&going[short.start - 3..short.start], b"b: ");
        // Only after the checkpoint.
        assert_eq!(find_line(log, 20, "seek to 0:05", &[], false), Err(false));
        // A prefix of a line is taken once the line is whole: its rest can
        // then be expected.
        assert_eq!(find_line(b"devices: 8086:10c9 func", 0, "devices: ", &[], false), Err(true));
        assert!(find_line(b"devices: 8086:10c9 function-level reset\n", 0, "devices: ", &[], false).is_ok());
        assert_eq!(find_line(log, 0, "", &[], false), Err(false));
    }

    #[test]
    fn counts_are_of_whole_lines() {
        let log = b"step to 0:1\nstep to 0:1\nstep to 0:1";
        assert_eq!(find_lines(log, 0, "step to 0:1", false).len(), 2);
        assert_eq!(find_lines(log, 0, "step to 0:1", true).len(), 3);
        assert_eq!(find_lines(log, 1, "step to 0:1", false).len(), 1);
        assert_eq!(find_lines(b"aaaa\n", 0, "aa", false).len(), 2);
    }

    #[test]
    fn needles_are_found_where_they_first_are() {
        assert_eq!(find(b"abcabd", b"abd"), Some(3));
        assert_eq!(find(b"abc", b"abcd"), None);
        assert_eq!(find(b"", b"a"), None);
        assert_eq!(find(b"xxa", b"a"), Some(2));
    }

    #[test]
    fn screenshots_go_to_the_output_directory() {
        assert_eq!(shot_path("target/veda/a.png"), util::out_dir().join("a.png"));
        assert_eq!(shot_path("documentation/images/b.png"), util::workspace_root().join("documentation/images/b.png"));
        assert_eq!(shot_path("/tmp/c.png"), PathBuf::from("/tmp/c.png"));
    }
}

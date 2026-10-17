// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The automation scripts' language (`.vts` files): how the machine is to
//! be started, and the steps a script then takes, parsed and checked as a
//! whole before anything is built or booted ([`Script::parse`]), each
//! malformed line reported with its number. `automate` runs the steps.
//!
//! One command per line. Words are separated by spaces; `"..."` quotes a
//! word, and so does `'...'`, which keeps double quotes inside (JSON
//! arguments); `#` outside quotes starts a comment. Numbers are seconds,
//! fractions of the screen (0 to 1, from the top left) or as said:
//!
//! ```text
//! wait-serial "desktop ready" 60   # wait for a log line (timeout in s; see below)
//! wait-serial-count "joined" 3 60  # wait until the text has appeared 3 times since the last checkpoint
//! wait 2                           # sleep
//! shot target/veda/desktop.png [now]  # save a screenshot, once the screen has settled (`now`: at once)
//! expect-same A.png B.png [L T R B]  # fail unless two screenshots are alike (in a region: left, top, right, bottom)
//! expect-black A.png [L T R B]     # fail unless a screenshot is black (in a region)
//! move 0.5 0.5                     # move the pointer
//! click 0.1 0.97 [BUTTON]          # move, press and release (left, right, middle, wheel-up, wheel-down, ...)
//! drag 0.3 0.3 0.6 0.6             # press at A, move to B, release
//! mouse-down 0.3 0.3 / mouse-up    # press (at a point) and release separately
//! double-click 0.05 0.44           # two quick clicks
//! key ctrl-alt-t                   # press a key combination (QEMU's key codes)
//! key-down alt / key-up alt        # hold or release a single key
//! type "hello world"               # type text (a US keyboard's characters)
//! type-env DEEPGRAM_API_KEY        # type an environment variable (a secret: in neither script nor log)
//! expect-serial "PASS"             # fail unless the log since the last checkpoint contains the text
//! reject-serial "panic"            # fail if it does
//! fail-on "PANIC"                  # abort later waits as soon as the text appears (anywhere in the log)
//! reset                            # reboot (disks are kept) until the loader starts; a checkpoint
//! checkpoint                       # later waits and expectations see only output from now on
//! link wired off                   # unplug (off) or plug in (on) the wired card's cable
//! qmp device_del '{"id":"kbd2"}'   # a QMP command, such as plugging USB devices in and out
//! expect-stick VEDA/LOGS/0001/VEDA.TXT "desktop ready"  # fail unless a file on the live system's stick contains the text
//! expect-audio                     # fail unless the recorded sound output holds more than silence
//! expect-audio-gapless [20]        # fail if it drops out (digital silence over 20 ms between its first and last sound)
//! say "open the text editor"       # speak into the test microphone (Deepgram's speech, cached) and wait until said
//! mic-wav FILE                     # play a WAV file (relative to the repository) into the microphone and wait
//! mic-silence 2                    # queue seconds of silence
//! mic-tone 1.5 [6000]              # queue seconds of a voice-like buzz (peak amplitude), for voice detection
//! mic-echo -6 250                  # the machine's sound output comes back into the microphone (gain dB, delay ms)
//! mic-music "Neon Horizon" -6      # a sample track plays into the microphone beside what is queued (gain dB), as music elsewhere in the room would
//! mic-music off                    # stop it (it also stops at its end)
//! agent-connected 120              # wait until the agent opened a conversation with the simulator
//! agent-call open_app '{"app":"editor"}' [60]  # the simulated model calls a function; waits for the result
//! agent-result "opened"            # fail unless the last function result contains the text
//! agent-result '"page":"processes","shown":1'  # ... or these JSON members, in one object, in any order
//! agent-mark                       # later agent-expect looks only at messages from now on
//! agent-expect "InjectUserMessage" 30  # wait until the agent sends a message containing the text
//! agent-speak 3                    # the simulated agent talks for 3 s (a voice-like buzz)
//! agent-interrupt                  # the simulated recogniser hears the user start speaking
//! agent-audio 3200                 # fail unless the agent streamed at least this many bytes of microphone audio
//! agent-hear "Hey Veda, hello" 30  # the simulated recogniser hears this, once the agent streams speech to it
//! agent-hear "Hey Veda. | Hi" 30   # " | " splits it into final transcripts 0.3 s apart (the last ends the utterance)
//! agent-listens 1                  # fail unless the agent opened exactly this many recognition streams
//! agent-listens-at-most 2          # fail if the agent opened more recognition streams than this
//! agent-not-listening 20           # wait until the agent has no recognition stream open
//! agent-mic-quiet -60              # fail if the agent sent microphone audio this loud (dBFS) since agent-mark
//! agent-mic-heard -30              # fail unless it sent microphone audio at least this loud since agent-mark
//! ```
//!
//! A wait (`wait-serial`) is satisfied by a line of the serial log once
//! the line is whole (its end has been written), that holds the text after
//! the last `checkpoint` or `reset`, where no earlier wait matched: a wait
//! that follows a repeated action needs a new line, never the one the
//! first action's wait matched. Lines that no wait matched, which an
//! action could find already there, are left behind by a `checkpoint`.
//!
//! How the machine starts, which must be known before it does, any line
//! may say (each once, but for the ones that add up):
//!
//! ```text
//! boot-cmdline "run=about"         # extra kernel command line (adds up)
//! net none                         # network: ethernet or none
//! nic e1000e                       # QEMU's model of the wired card
//! memory 2048                      # the machine's memory (MiB)
//! acpi-table touchpad              # add a table to the firmware's (automation/src/architectures/*/acpitest.rs; adds up)
//! firmware-variable Name GUID 7 0a0b  # add a variable (attributes, hex data) to the firmware's store (adds up)
//! sound hda                        # the sound card: virtio or hda (hda in a script that resets)
//! audio host                       # the host's loudspeakers and microphone instead of a WAV file (echo on real hardware)
//! audio silent                     # QEMU's silent sound system: the output is not recorded, the input records silence
//! live [writable]                  # boot the live system (`cargo automation iso`) from a USB stick (read-only unless writable)
//! input usb                        # USB keyboard and pointer (see `--input`)
//! usb net                          # QEMU's USB network adapter (CDC Ethernet) on the xHCI controller
//! gpu on                           # QEMU's 3D GPU (`gpu off`: the VGA alone); skipped where QEMU has none
//! nettest local                    # nettest checks the network against a server of the host's (no Internet)
//! agent-asleep                     # boot with the agent asleep (no conversation at start)
//! requires c-toolchain             # only with the C tests (`cargo automation toolchain`); skipped otherwise
//! requires native-toolchain        # only with GCC in the image (the same)
//! requires media                   # only with the media decoder (FFmpeg and dav1d, the same)
//! requires rust-toolchain          # only with the Rust test programs (the same)
//! ```

use std::time::Duration;

use serde_json::Value;

use crate::arch;
use crate::ovmfvars::{self, Variable};
use crate::qemu::{InputDevices, NetMode};
use crate::qmp;
use crate::util::Result;

/// A script, parsed and checked.
#[derive(Debug)]
pub struct Script {
    /// How the machine starts.
    pub boot: Boot,
    /// What it does then, each with its line (from one).
    pub steps: Vec<(usize, Step)>,
}

/// How a script's machine starts.
#[derive(Debug, Default, PartialEq)]
pub struct Boot {
    /// `boot-cmdline`: the kernel command line's arguments, in order.
    pub cmdline: Vec<String>,
    pub net: Option<NetMode>,
    pub nic: Option<String>,
    /// The machine's memory (MiB).
    pub memory: Option<u32>,
    /// `acpi-table`: the names of `arch::acpitest`'s tables.
    pub acpi_tables: Vec<String>,
    pub firmware_variables: Vec<Variable>,
    /// The sound card: `virtio` or `hda` (`hda` for a script that resets,
    /// whether or not it says so).
    pub sound: Option<String>,
    pub audio: Audio,
    pub live: Option<Live>,
    pub input: Option<InputDevices>,
    pub usb_net: bool,
    /// QEMU's 3D GPU (`Some(true)`), its VGA alone (`Some(false)`).
    pub gpu: Option<bool>,
    /// `nettest local`: nettest checks the network against the host's
    /// server.
    pub nettest_local: bool,
    pub agent_asleep: bool,
    pub requires: Vec<Requirement>,
}

/// Where a machine's sound goes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Audio {
    /// The output recorded to a WAV file, no input.
    #[default]
    Recorded,
    /// The host's loudspeakers and microphone.
    Host,
    /// QEMU's silent sound system: an input that records silence at the
    /// card's pace, an output nothing records.
    Silent,
}

/// The live system's stick: read-only or writable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Live {
    ReadOnly,
    Writable,
}

/// A part of the toolchain a script needs (`requires`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    /// The C test programs the cross compiler builds.
    CToolchain,
    /// GCC in the image.
    NativeToolchain,
    /// The media decoder.
    Media,
    /// The Rust test programs.
    RustToolchain,
}

impl Requirement {
    /// The part of the toolchain, as `toolchain::built` names it.
    pub fn part(self) -> &'static str {
        match self {
            Requirement::CToolchain => "cross toolchain",
            Requirement::NativeToolchain => "native toolchain",
            Requirement::Media => "media libraries",
            Requirement::RustToolchain => "rust toolchain",
        }
    }
}

/// A point on the screen, as fractions of its width and height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// A region of the screen: left, top, right and bottom, as fractions.
pub type Region = [f64; 4];

/// The whole screen.
pub const SCREEN: Region = [0.0, 0.0, 1.0, 1.0];

/// What a script does, line by line.
#[derive(Debug, PartialEq)]
pub enum Step {
    Wait(Duration),
    WaitSerial {
        text: String,
        timeout: Duration,
    },
    WaitSerialCount {
        text: String,
        count: usize,
        timeout: Duration,
    },
    /// A screenshot: its path as the script gives it, and whether it waits
    /// for the screen to settle (otherwise it is of the moment, as a
    /// game's or a video's must be).
    Shot {
        path: String,
        settle: bool,
    },
    ExpectSame {
        first: String,
        second: String,
        region: Region,
    },
    ExpectBlack {
        shot: String,
        region: Region,
    },
    Move(Point),
    Click {
        at: Point,
        button: String,
    },
    Drag {
        from: Point,
        to: Point,
    },
    MouseDown(Point),
    MouseUp,
    DoubleClick(Point),
    Key(String),
    KeyDown(String),
    KeyUp(String),
    Type(String),
    /// Types the value of an environment variable.
    TypeEnv(String),
    ExpectSerial(String),
    RejectSerial(String),
    FailOn(String),
    Reset,
    Checkpoint,
    Link {
        up: bool,
    },
    Qmp {
        command: String,
        arguments: Value,
    },
    ExpectStick {
        path: String,
        text: String,
    },
    ExpectAudio,
    /// The longest silence allowed (ms).
    ExpectAudioGapless(f64),
    Say(String),
    MicWav(String),
    MicSilence(f64),
    MicTone {
        secs: f64,
        peak: f64,
    },
    MicEcho {
        gain_db: f64,
        delay_ms: f64,
    },
    /// A sample track (its title, and gain in dB) playing in the room, or
    /// none.
    MicMusic(Option<(String, f64)>),
    AgentConnected(Duration),
    AgentCall {
        name: String,
        arguments: String,
        timeout: Duration,
    },
    AgentResult(String),
    AgentMark,
    AgentExpect {
        text: String,
        timeout: Duration,
    },
    AgentSpeak(f64),
    AgentInterrupt,
    AgentHear {
        text: String,
        timeout: Duration,
    },
    AgentListens(u32),
    AgentListensAtMost(u32),
    AgentNotListening(Duration),
    AgentAudio(usize),
    AgentMicQuiet(f32),
    AgentMicHeard(f32),
}

impl Script {
    /// Parses and checks `text`; the error lists every line that is wrong.
    pub fn parse(text: &str) -> Result<Script> {
        let mut boot = Boot::default();
        let mut steps = Vec::new();
        let mut errors = Vec::new();
        // Where each setting that is given once was given.
        let mut given: Vec<(&'static str, usize)> = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let number = index + 1;
            let parsed = words(line).and_then(|words| {
                let mut line = Line { words, at: 1 };
                let Some(command) = line.words.first().cloned() else { return Ok(None) };
                let parsed = line.parse(&command, &mut boot, &mut given, number)?;
                line.end(&command)?;
                Ok(parsed)
            });
            match parsed {
                Ok(Some(step)) => steps.push((number, step)),
                Ok(None) => {}
                Err(e) => errors.push(format!("script line {number}: {e}")),
            }
        }
        // QEMU's virtio sound card keeps the buffers its streams hold across
        // a reset, and completes them once the next start has set the card
        // up again, writing their status into memory that is the next
        // start's by then (QEMU frees them since its 75d28444c7, of
        // September 2026): a machine that is reset has the HD Audio card.
        if steps.iter().any(|(_, s)| *s == Step::Reset) {
            match boot.sound.as_deref() {
                None | Some("hda") => boot.sound = Some("hda".into()),
                Some(other) => errors.push(format!(
                    "sound {other}: a script that resets the machine has the HD Audio card (`sound hda`), as \
                     QEMU's virtio sound card completes its buffers from before a reset into the next start's memory"
                )),
            }
        }
        if errors.is_empty() { Ok(Script { boot, steps }) } else { Err(errors.join("\n")) }
    }

    fn any(&self, f: impl Fn(&Step) -> bool) -> bool {
        self.steps.iter().any(|(_, step)| f(step))
    }

    /// Whether the script uses the test microphone.
    pub fn needs_mic(&self) -> bool {
        self.any(|s| {
            matches!(
                s,
                Step::Say(_)
                    | Step::MicWav(_)
                    | Step::MicSilence(_)
                    | Step::MicTone { .. }
                    | Step::MicEcho { .. }
                    | Step::MicMusic(_)
            )
        })
    }

    /// Whether the script talks to the simulated Voice Agent service.
    pub fn needs_agent(&self) -> bool {
        self.boot.agent_asleep
            || self.any(|s| {
                matches!(
                    s,
                    Step::AgentConnected(_)
                        | Step::AgentCall { .. }
                        | Step::AgentResult(_)
                        | Step::AgentMark
                        | Step::AgentExpect { .. }
                        | Step::AgentSpeak(_)
                        | Step::AgentInterrupt
                        | Step::AgentHear { .. }
                        | Step::AgentListens(_)
                        | Step::AgentListensAtMost(_)
                        | Step::AgentNotListening(_)
                        | Step::AgentAudio(_)
                        | Step::AgentMicQuiet(_)
                        | Step::AgentMicHeard(_)
                )
            })
    }

    /// Whether the script needs the machine to itself: the machine's sound
    /// coming back into the microphone (`mic-echo`) must keep in time with
    /// it, as loudspeakers' does, for the echo canceller to take it out,
    /// and machines running beside it put it out of time now and then.
    pub fn alone(&self) -> bool {
        self.any(|s| matches!(s, Step::MicEcho { .. }))
    }

    /// Whether the script resets the machine, which QEMU must then stay up
    /// across.
    pub fn resets(&self) -> bool {
        self.any(|s| *s == Step::Reset)
    }

    /// Whether the script types a secret (`type-env`), which the machine
    /// may keep (Settings stores a key on the home disk).
    pub fn types_secret(&self) -> bool {
        self.any(|s| matches!(s, Step::TypeEnv(_)))
    }
}

/// Splits a script line into words: see the module's overview.
fn words(line: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut single = false;
    let mut any = false;
    for c in line.chars() {
        match c {
            '\'' if !quoted => {
                single = !single;
                any = true;
            }
            c if single => cur.push(c),
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '#' if !quoted => break,
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if quoted || single {
        return Err(format!("a quote ({}) is not closed", if quoted { '"' } else { '\'' }));
    }
    if any {
        out.push(cur);
    }
    Ok(out)
}

/// The words of a line, taken one at a time.
struct Line {
    words: Vec<String>,
    at: usize,
}

impl Line {
    fn next(&mut self) -> Option<String> {
        let word = self.words.get(self.at).cloned();
        self.at += 1;
        word
    }

    /// The next word, which must be there.
    fn text(&mut self, what: &str) -> Result<String> {
        self.next().ok_or_else(|| format!("missing {what}"))
    }

    /// The next word, a number.
    fn number(&mut self, what: &str) -> Result<f64> {
        let word = self.text(what)?;
        number(&word, what)
    }

    /// The next word, if there is one, as a number.
    fn optional_number(&mut self, what: &str) -> Result<Option<f64>> {
        match self.next() {
            Some(word) => number(&word, what).map(Some),
            None => Ok(None),
        }
    }

    /// The next word, seconds (`default` if there is none).
    fn seconds(&mut self, default: Option<f64>) -> Result<Duration> {
        let secs = match (self.optional_number("seconds")?, default) {
            (Some(secs), _) | (None, Some(secs)) => secs,
            (None, None) => return Err("missing seconds".into()),
        };
        Duration::try_from_secs_f64(secs).map_err(|_| format!("{secs} is not a time in seconds"))
    }

    /// The next word, a whole number.
    fn count<T: std::str::FromStr>(&mut self, what: &str) -> Result<T> {
        let word = self.text(what)?;
        word.parse().map_err(|_| format!("'{word}' is not {what}"))
    }

    /// The next two words, a point on the screen.
    fn point(&mut self) -> Result<Point> {
        Ok(Point { x: self.fraction("x")?, y: self.fraction("y")? })
    }

    /// The next word, a fraction of the screen.
    fn fraction(&mut self, what: &str) -> Result<f64> {
        let v = self.number(what)?;
        if (0.0..=1.0).contains(&v) {
            Ok(v)
        } else {
            Err(format!("{what} {v} is not a fraction of the screen (0 to 1)"))
        }
    }

    /// The rest of the line: a region (four fractions), or the whole
    /// screen without one.
    fn region(&mut self) -> Result<Region> {
        if self.at >= self.words.len() {
            return Ok(SCREEN);
        }
        let region = [self.fraction("left")?, self.fraction("top")?, self.fraction("right")?, self.fraction("bottom")?];
        if region[0] >= region[2] || region[1] >= region[3] {
            return Err(format!("the region {region:?} is empty (left, top, right, bottom)"));
        }
        Ok(region)
    }

    /// The next word, a value of `known`.
    fn one_of(&mut self, what: &str, known: &[&str]) -> Result<String> {
        let word = self.text(what)?;
        if known.contains(&word.as_str()) {
            Ok(word)
        } else {
            Err(format!("unknown {what} '{word}' ({})", known.join(", ")))
        }
    }

    /// Fails if words are left over.
    fn end(&self, command: &str) -> Result {
        match self.words.get(self.at..).filter(|rest| !rest.is_empty()) {
            Some(rest) => Err(format!("{command}: too many words ({})", rest.join(" "))),
            None => Ok(()),
        }
    }

    /// The step `command` stands for, or the boot setting it makes.
    fn parse(
        &mut self,
        command: &str,
        boot: &mut Boot,
        given: &mut Vec<(&'static str, usize)>,
        line: usize,
    ) -> Result<Option<Step>> {
        // A setting given once: where it was, if it was.
        let mut once = |name: &'static str| match given.iter().find(|(n, _)| *n == name) {
            Some((_, first)) => Err(format!("{name} is given on line {first} already")),
            None => {
                given.push((name, line));
                Ok(())
            }
        };
        let step = match command {
            "wait" => Step::Wait(self.seconds(None)?),
            "wait-serial" => Step::WaitSerial { text: self.text("text")?, timeout: self.seconds(Some(60.0))? },
            "wait-serial-count" => {
                let text = self.text("text")?;
                let count = self.count("a count")?;
                Step::WaitSerialCount { text, count, timeout: self.seconds(Some(60.0))? }
            }
            "shot" => {
                let path = self.text("file name")?;
                let settle = match self.next().as_deref() {
                    None => true,
                    Some("now") => false,
                    Some(other) => return Err(format!("shot: '{other}' is not `now`")),
                };
                Step::Shot { path, settle }
            }
            "expect-same" => Step::ExpectSame {
                first: self.text("file name")?,
                second: self.text("file name")?,
                region: self.region()?,
            },
            "expect-black" => Step::ExpectBlack { shot: self.text("file name")?, region: self.region()? },
            "move" => Step::Move(self.point()?),
            "click" => {
                let at = self.point()?;
                let button =
                    if self.at < self.words.len() { self.one_of("button", qmp::BUTTONS)? } else { "left".into() };
                Step::Click { at, button }
            }
            "drag" => Step::Drag { from: self.point()?, to: self.point()? },
            "mouse-down" => Step::MouseDown(self.point()?),
            "mouse-up" => Step::MouseUp,
            "double-click" => Step::DoubleClick(self.point()?),
            "key" => {
                let combo = self.text("keys")?;
                qmp::check_combo(&combo)?;
                Step::Key(combo)
            }
            "key-down" | "key-up" => {
                let key = self.text("key")?;
                if key.contains('-') {
                    return Err(format!("{command} takes one key, not '{key}'"));
                }
                qmp::check_combo(&key)?;
                if command == "key-down" { Step::KeyDown(key) } else { Step::KeyUp(key) }
            }
            "type" => {
                let text = self.text("text")?;
                if let Some(c) = text.chars().find(|&c| qmp::keys_for(c).is_none()) {
                    return Err(format!("no key types {c:?}"));
                }
                Step::Type(text)
            }
            "type-env" => Step::TypeEnv(self.text("variable name")?),
            "expect-serial" => Step::ExpectSerial(self.text("text")?),
            "reject-serial" => Step::RejectSerial(self.text("text")?),
            "fail-on" => Step::FailOn(self.text("text")?),
            "reset" => Step::Reset,
            "checkpoint" => Step::Checkpoint,
            "link" => {
                self.one_of("card", &["wired"])?;
                let state = self.one_of("state", &["on", "off", "up", "down"])?;
                Step::Link { up: matches!(state.as_str(), "on" | "up") }
            }
            "qmp" => {
                let command = self.text("command")?;
                let arguments = match self.next() {
                    Some(text) => serde_json::from_str::<Value>(&text).map_err(|e| format!("arguments: {e}"))?,
                    None => Value::Object(Default::default()),
                };
                if !arguments.is_object() {
                    return Err("arguments: not a JSON object".into());
                }
                Step::Qmp { command, arguments }
            }
            "expect-stick" => {
                let path = self.text("path")?;
                if path.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
                    return Err(format!("{path} is not a path on the stick (such as VEDA/LOGS/0001/VEDA.TXT)"));
                }
                Step::ExpectStick { path, text: self.text("text")? }
            }
            "expect-audio" => Step::ExpectAudio,
            "expect-audio-gapless" => {
                let limit = self.optional_number("milliseconds")?.unwrap_or(20.0);
                if limit < 0.0 {
                    return Err(format!("{limit} is not a time in milliseconds"));
                }
                Step::ExpectAudioGapless(limit)
            }
            "say" => Step::Say(self.text("text")?),
            "mic-wav" => Step::MicWav(self.text("file")?),
            "mic-silence" => Step::MicSilence(self.seconds(None)?.as_secs_f64()),
            "mic-tone" => {
                let secs = self.seconds(None)?.as_secs_f64();
                let peak = self.optional_number("peak")?.unwrap_or(6000.0);
                if !(0.0..=32767.0).contains(&peak) {
                    return Err(format!("peak {peak} is not a sample's amplitude (0 to 32767)"));
                }
                Step::MicTone { secs, peak }
            }
            "mic-echo" => {
                let gain_db = self.number("gain (dB)")?;
                let delay_ms = self.number("delay (ms)")?;
                if !(0.0..=10_000.0).contains(&delay_ms) {
                    return Err(format!("delay {delay_ms} is not milliseconds (0 to 10000)"));
                }
                Step::MicEcho { gain_db, delay_ms }
            }
            "mic-music" => match self.text("track title")?.as_str() {
                "off" => Step::MicMusic(None),
                title => Step::MicMusic(Some((title.into(), self.optional_number("gain (dB)")?.unwrap_or(0.0)))),
            },
            "agent-connected" => Step::AgentConnected(self.seconds(Some(120.0))?),
            "agent-call" => {
                let name = self.text("function name")?;
                let arguments = self.next().unwrap_or_else(|| "{}".into());
                if !serde_json::from_str::<Value>(&arguments).is_ok_and(|v| v.is_object()) {
                    return Err(format!("arguments {arguments}: not a JSON object"));
                }
                Step::AgentCall { name, arguments, timeout: self.seconds(Some(60.0))? }
            }
            "agent-result" => Step::AgentResult(self.text("text")?),
            "agent-mark" => Step::AgentMark,
            "agent-expect" => Step::AgentExpect { text: self.text("text")?, timeout: self.seconds(Some(60.0))? },
            "agent-speak" => Step::AgentSpeak(self.seconds(None)?.as_secs_f64()),
            "agent-interrupt" => Step::AgentInterrupt,
            "agent-hear" => Step::AgentHear { text: self.text("text")?, timeout: self.seconds(Some(30.0))? },
            "agent-listens" => Step::AgentListens(self.count("a number of streams")?),
            "agent-listens-at-most" => Step::AgentListensAtMost(self.count("a number of streams")?),
            "agent-not-listening" => Step::AgentNotListening(self.seconds(Some(30.0))?),
            "agent-audio" => Step::AgentAudio(self.count("a number of bytes")?),
            "agent-mic-quiet" => Step::AgentMicQuiet(self.number("level (dBFS)")? as f32),
            "agent-mic-heard" => Step::AgentMicHeard(self.number("level (dBFS)")? as f32),
            // How the machine starts.
            "boot-cmdline" => {
                let argument = self.text("arguments")?;
                if argument.trim().is_empty() {
                    return Err("no arguments".into());
                }
                boot.cmdline.push(argument);
                return Ok(None);
            }
            "net" => {
                once("net")?;
                let mode = self.text("mode")?;
                boot.net = Some(NetMode::parse(&mode).ok_or(format!("unknown network '{mode}' (ethernet, none)"))?);
                return Ok(None);
            }
            "nic" => {
                once("nic")?;
                let model = self.text("model")?;
                if model.is_empty() || !model.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    return Err(format!("'{model}' is not one of QEMU's network card models (such as e1000e, igb)"));
                }
                boot.nic = Some(model);
                return Ok(None);
            }
            "memory" => {
                once("memory")?;
                let mib: u32 = self.count("a size in MiB")?;
                if mib < 256 {
                    return Err(format!("{mib} MiB is too little for Veda"));
                }
                boot.memory = Some(mib);
                return Ok(None);
            }
            "acpi-table" => {
                let name = self.one_of("ACPI table", arch::acpitest::NAMES)?;
                boot.acpi_tables.push(name);
                return Ok(None);
            }
            "firmware-variable" => {
                let (name, guid) = (self.text("name")?, self.text("GUID")?);
                let (attributes, data) = (self.text("attributes")?, self.text("data")?);
                let guid = ovmfvars::guid(&guid).ok_or(format!("{guid} is not a GUID"))?;
                let attributes = u32::from_str_radix(attributes.trim_start_matches("0x"), 16)
                    .map_err(|_| format!("attributes {attributes} are not hex"))?;
                let bytes = hex_bytes(&data).ok_or(format!("data {data} is not hex bytes"))?;
                boot.firmware_variables.push(Variable { name, guid, attributes, data: bytes });
                return Ok(None);
            }
            "sound" => {
                once("sound")?;
                boot.sound = Some(self.one_of("sound card", &["virtio", "hda"])?);
                return Ok(None);
            }
            "audio" => {
                once("audio")?;
                boot.audio = match self.one_of("sound system", &["host", "silent"])?.as_str() {
                    "host" => Audio::Host,
                    _ => Audio::Silent,
                };
                return Ok(None);
            }
            "live" => {
                once("live")?;
                boot.live = Some(match self.next().as_deref() {
                    None => Live::ReadOnly,
                    Some("writable") => Live::Writable,
                    Some(other) => return Err(format!("live: '{other}' is not `writable`")),
                });
                return Ok(None);
            }
            "input" => {
                once("input")?;
                let v = self.text("devices")?;
                boot.input =
                    Some(InputDevices::parse(&v).ok_or(format!("unknown input devices '{v}' (standard, usb)"))?);
                return Ok(None);
            }
            "usb" => {
                once("usb")?;
                self.one_of("USB device", &["net"])?;
                boot.usb_net = true;
                return Ok(None);
            }
            "gpu" => {
                once("gpu")?;
                boot.gpu = Some(self.one_of("GPU", &["on", "off"])? == "on");
                return Ok(None);
            }
            "nettest" => {
                once("nettest")?;
                self.one_of("network checks", &["local"])?;
                boot.nettest_local = true;
                return Ok(None);
            }
            "agent-asleep" => {
                once("agent-asleep")?;
                boot.agent_asleep = true;
                return Ok(None);
            }
            "requires" => {
                let what =
                    self.one_of("requirement", &["c-toolchain", "native-toolchain", "media", "rust-toolchain"])?;
                boot.requires.push(match what.as_str() {
                    "c-toolchain" => Requirement::CToolchain,
                    "native-toolchain" => Requirement::NativeToolchain,
                    "media" => Requirement::Media,
                    _ => Requirement::RustToolchain,
                });
                return Ok(None);
            }
            other => return Err(format!("unknown command '{other}'")),
        };
        Ok(Some(step))
    }
}

/// `word` as a number (`what`, for the error).
fn number(word: &str, what: &str) -> Result<f64> {
    word.parse::<f64>().ok().filter(|v| v.is_finite()).ok_or_else(|| format!("'{word}' is not a number ({what})"))
}

/// Hexadecimal digits as bytes, two digits each; none for no digits.
fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util;

    fn steps(text: &str) -> Vec<Step> {
        Script::parse(text).unwrap().steps.into_iter().map(|(_, s)| s).collect()
    }

    fn error(text: &str) -> String {
        Script::parse(text).unwrap_err()
    }

    #[test]
    fn words_honour_quotes_and_comments() {
        assert_eq!(words(r#"type "a # b" 'c "d"' e # f"#).unwrap(), ["type", "a # b", r#"c "d""#, "e"]);
        assert_eq!(words(r#"say """#).unwrap(), ["say", ""]);
        assert_eq!(words("   # only a comment").unwrap(), Vec::<String>::new());
        assert!(words(r#"wait-serial "open"#).is_err_and(|e| e.contains('"')));
        assert!(words("qmp x '{").is_err_and(|e| e.contains('\'')));
    }

    #[test]
    fn steps_have_their_lines_and_defaults() {
        let script = Script::parse("# a comment\n\nwait 1.5\nwait-serial \"ready\"\nclick 0.5 0.25 right\n").unwrap();
        assert_eq!(
            script.steps,
            [
                (3, Step::Wait(Duration::from_millis(1500))),
                (4, Step::WaitSerial { text: "ready".into(), timeout: Duration::from_secs(60) }),
                (5, Step::Click { at: Point { x: 0.5, y: 0.25 }, button: "right".into() }),
            ]
        );
        assert_eq!(steps("expect-black a.png"), [Step::ExpectBlack { shot: "a.png".into(), region: SCREEN }]);
        assert_eq!(
            steps("expect-same a.png b.png 0 0 1 0.93"),
            [Step::ExpectSame { first: "a.png".into(), second: "b.png".into(), region: [0.0, 0.0, 1.0, 0.93] }]
        );
        assert_eq!(steps("link wired off"), [Step::Link { up: false }]);
        assert_eq!(
            steps("mic-music \"Neon Horizon\" -6\nmic-music Rain\nmic-music off"),
            [
                Step::MicMusic(Some(("Neon Horizon".into(), -6.0))),
                Step::MicMusic(Some(("Rain".into(), 0.0))),
                Step::MicMusic(None)
            ]
        );
        assert_eq!(
            steps(r#"agent-call open_app '{"app":"editor"}'"#),
            [Step::AgentCall {
                name: "open_app".into(),
                arguments: r#"{"app":"editor"}"#.into(),
                timeout: Duration::from_secs(60)
            }]
        );
    }

    #[test]
    fn malformed_lines_are_refused_with_their_numbers() {
        let cases = [
            ("wait -1", "-1 is not a time"),
            ("wait NaN", "not a number"),
            ("wait inf", "not a number"),
            ("wait-serial \"x\" -5", "-5 is not a time"),
            ("wait-serial", "missing text"),
            ("wait-serial \"x\" 10 more", "too many words"),
            ("wait-serial-count x many", "not a count"),
            ("click 1.5 0.5", "not a fraction"),
            ("click 0.5 0.5 sideways", "unknown button"),
            ("expect-same a.png b.png 0 0 1", "missing bottom"),
            ("expect-black a.png 0.5 0 0.5 1", "is empty"),
            ("key ctrl-enter", "'enter'"),
            ("key-down altgr", "'altgr' is not one of QEMU's key codes"),
            ("key-up ctrl-alt", "takes one key"),
            ("shot a.png later", "not `now`"),
            ("type \"café\"", "no key types 'é'"),
            ("link wifi off", "unknown card"),
            ("link wired sideways", "unknown state"),
            ("qmp device_del '{\"id\":'", "arguments"),
            ("qmp device_del '[1]'", "not a JSON object"),
            ("agent-call open_app '{nope}'", "not a JSON object"),
            ("expect-stick VEDA/../X \"y\"", "not a path"),
            ("mic-tone 1 40000", "peak"),
            ("mic-echo -6", "missing delay"),
            ("mic-music", "missing track title"),
            ("mic-music Rain loud", "not a number"),
            ("net wifi", "unknown network"),
            ("requires gcc", "unknown requirement"),
            ("usb disk", "unknown USB device"),
            ("audio loud", "unknown sound system"),
            ("live sometimes", "not `writable`"),
            ("gpu maybe", "unknown GPU"),
            ("acpi-table battery", "unknown ACPI table"),
            ("firmware-variable A 1234 7 00", "not a GUID"),
            ("firmware-variable A 02f9af02-7734-4233-b43d-93fe5aa35db3 7 0", "not hex bytes"),
            ("memory 64", "too little"),
            ("frobnicate", "unknown command 'frobnicate'"),
            ("say \"open", "not closed"),
        ];
        for (line, why) in cases {
            let e = error(&format!("wait 1\n{line}\n"));
            assert!(e.starts_with("script line 2: ") && e.contains(why), "{line}: {e}");
        }
        // Every wrong line is reported, each with its number.
        let e = error("wait x\nwait 1\nnet none\nnet none\n");
        assert_eq!(e.lines().count(), 2, "{e}");
        assert!(e.contains("script line 1: ") && e.contains("script line 4: net is given on line 3 already"), "{e}");
    }

    #[test]
    fn how_the_machine_starts() {
        let script = Script::parse(
            "boot-cmdline \"run=a\"\nnet none\nnic igb\nmemory 2048\nacpi-table touchpad\nacpi-table nhlt\n\
             firmware-variable A 02f9af02-7734-4233-b43d-93fe5aa35db3 0x7 0a0b\nsound virtio\naudio silent\n\
             live writable\ninput usb\nusb net\ngpu on\nnettest local\nagent-asleep\nrequires media\n\
             boot-cmdline \"b=c\"\nwait 1\n",
        )
        .unwrap();
        let boot = &script.boot;
        assert_eq!(boot.cmdline, ["run=a", "b=c"]);
        assert_eq!((boot.net, boot.nic.as_deref()), (Some(NetMode::None), Some("igb")));
        assert_eq!(
            (boot.memory, boot.acpi_tables.as_slice()),
            (Some(2048), &["touchpad".to_string(), "nhlt".into()][..])
        );
        assert_eq!(boot.firmware_variables[0].attributes, 7);
        assert_eq!(boot.firmware_variables[0].data, [10, 11]);
        assert_eq!(
            (boot.sound.as_deref(), boot.audio, boot.live),
            (Some("virtio"), Audio::Silent, Some(Live::Writable))
        );
        assert_eq!((boot.input, boot.usb_net, boot.gpu), (Some(InputDevices::Usb), true, Some(true)));
        assert!(boot.nettest_local && boot.agent_asleep && script.needs_agent());
        assert_eq!(boot.requires, [Requirement::Media]);
        assert_eq!(script.steps.len(), 1);
        assert_eq!(Script::parse("wait 1\n").unwrap().boot, Boot::default());
    }

    #[test]
    fn a_machine_that_is_reset_has_the_hd_audio_card() {
        let sound = |text: &str| Script::parse(text).map(|s| s.boot.sound);
        assert_eq!(sound("wait 1\n"), Ok(None));
        assert_eq!(sound("sound virtio\n"), Ok(Some("virtio".into())));
        assert_eq!(sound("wait 1\nreset\n"), Ok(Some("hda".into())));
        assert_eq!(sound("sound hda\nreset\n"), Ok(Some("hda".into())));
        assert!(sound("sound virtio\nreset\n").is_err_and(|e| e.contains("sound hda")));
        assert!(Script::parse("reset  # and start again\n").unwrap().resets());
        assert!(Script::parse("# reset\n").unwrap().steps.is_empty());
        assert!(Script::parse("reset-later\n").is_err());
    }

    #[test]
    fn what_a_script_needs() {
        let script = Script::parse("mic-echo -6 250\nagent-speak 2\ntype-env KEY\n").unwrap();
        assert!(script.needs_mic() && script.needs_agent() && script.alone() && script.types_secret());
        let plain = Script::parse("wait 1\nkey ret\n").unwrap();
        assert!(!plain.needs_mic() && !plain.needs_agent() && !plain.alone() && !plain.types_secret());
        // Music in the room keeps no time with the machine.
        let music = Script::parse("mic-music Rain -6\n").unwrap();
        assert!(music.needs_mic() && !music.alone());
    }

    /// Every script in the repository is one the interpreter runs.
    #[test]
    fn every_script_in_the_repository_parses() {
        let root = util::workspace_root();
        let mut count = 0;
        for dir in ["verification", "documentation"] {
            for path in util::files_under(&root.join(dir)).unwrap() {
                if path.extension().is_some_and(|e| e == "vts") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    if let Err(e) = Script::parse(&text) {
                        panic!("{}:\n{e}", path.display());
                    }
                    count += 1;
                }
            }
        }
        assert!(count > 50, "only {count} scripts found");
    }
}

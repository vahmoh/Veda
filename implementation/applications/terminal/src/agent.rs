// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Terminal for the voice agent: what it shows (the latest output, the
//! folder it is in, the command running) and what it does (running a
//! command line as the user would type it, stopping it, clearing the
//! screen, the text size), through the same paths as the keyboard.
//!
//! Running a command line takes the user's consent, which the user may
//! give once and for all for `run_command` ("always allow"). That consent
//! covers only command lines that look or change something without losing
//! anything: `run_command` refuses one that deletes, replaces, moves or
//! ends something, forgets the history or runs a program (as
//! `terminal_core::classify` reads it), which only `run_risky_command`
//! runs, with the user's consent every time.
//!
//! Command lines read back by the agent have Wi-Fi passwords hidden, as in
//! the command history.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use terminal_core::classify::Effect;
use terminal_core::syntax::redact;
use vui::agent::{self, Action, AppAgentInfo, Risk, Value, arg_f64, arg_opt_int, arg_str, clip, json, show_path};

use crate::{DEFAULT_FONT_SIZE, Terminal};

/// The longest command line the agent may run: short enough for the
/// approval card to show all of it.
const MAX_COMMAND: usize = 80;
/// What the command of `run_command` is.
const COMMAND_PARAM: &str = "The command line exactly as the user would type it: one line of at most 80 characters, \
                             such as ls ~/Documents. It runs in the terminal's current folder (cd changes it). One \
                             that deletes, replaces, moves or ends something, forgets the history or runs a program \
                             (rm, mv, cp, kill, history -c, > onto a file that exists, programs) is refused: \
                             run_risky_command runs those. Returns what it printed, waiting up to 10 seconds; a \
                             longer command keeps running.";
/// What the command of `run_risky_command` is.
const RISKY_COMMAND_PARAM: &str = "The command line exactly as the user would type it: one line of at most 80 \
                                   characters. It runs in the terminal's current folder (cd changes it). To copy or \
                                   move files, Files' actions, which never replace anything, are better. Returns what \
                                   it printed, waiting up to 10 seconds; a longer command keeps running.";
/// How long `run_command` waits for the command (an application has 30
/// seconds to answer the agent; most commands finish well within this).
const RUN_WAIT_NS: u64 = 10_000_000_000;
/// How long `interrupt` waits for the command to stop.
const STOP_WAIT_NS: u64 = 1_000_000_000;
/// The most output the agent gets at once (bytes).
const MAX_OUTPUT: usize = 6000;
/// The most output in the state (bytes).
const STATE_OUTPUT: usize = 2500;
/// The lines of output `read_output` returns unless asked for more, and the
/// most it returns.
const READ_LINES: i64 = 40;
const MAX_READ_LINES: i64 = 200;

pub fn info() -> AppAgentInfo {
    agent::info(
        "A terminal with vsh, the Veda command shell: it runs commands for files, programs, the network and the \
         system, and shows what they print.",
        vec![
            Action::new("run_command", "Runs a command in the terminal")
                .param("command", "string", COMMAND_PARAM, true)
                .risk(Risk::Sensitive)
                .build(),
            Action::new("run_risky_command", "Runs a command that may delete, replace or end something")
                .param("command", "string", RISKY_COMMAND_PARAM, true)
                .risk(Risk::Destructive)
                .build(),
            Action::new("read_output", "Returns the terminal's latest output and whether a command is still running")
                .param("lines", "integer", "How many lines from the end (40 by default, at most 200)", false)
                .build(),
            Action::new("interrupt", "Stops the command that is running, like Ctrl+C").build(),
            Action::new("clear_screen", "Clears the terminal's screen, like Ctrl+L (the command history stays)")
                .build(),
            Action::new("set_text_size", "Changes the size of the terminal's text")
                .param("points", "number", "The size in points, 9 to 28 (14 is normal)", true)
                .build(),
        ],
    )
}

/// `text`, or its start and its end when it is longer than `max` bytes
/// (whole lines, with a note of how many were left out).
fn clip_middle(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let lines: Vec<&str> = text.lines().collect();
    // Whole lines from the start (about two thirds), then from the end.
    let (mut head, mut size) = (0, 0);
    while head < lines.len() && size + lines[head].len() < max * 2 / 3 {
        size += lines[head].len() + 1;
        head += 1;
    }
    let (mut tail, mut tail_size) = (lines.len(), 0);
    while tail > head && tail_size + lines[tail - 1].len() < max / 3 {
        tail_size += lines[tail - 1].len() + 1;
        tail -= 1;
    }
    let mut out = String::new();
    for l in &lines[..head] {
        out.push_str(l);
        out.push('\n');
    }
    if head == 0 {
        // A first line longer than all that: its start.
        out.push_str(clip(lines[0], max / 2).0);
        out.push_str("\u{2026}\n");
        head = 1;
    }
    if tail > head {
        out.push_str(&format!("[{} lines left out]\n", tail - head));
    }
    for l in &lines[tail.max(head)..] {
        out.push_str(l);
        out.push('\n');
    }
    (out, true)
}

pub fn state(t: &Terminal) -> Value {
    let (output, earlier) = t.tail(usize::MAX, STATE_OUTPUT);
    let mut v = json!({ "folder": t.folder(), "running": t.running() });
    if t.job.is_none() {
        v["last_exit_status"] = json!(t.last_status);
    }
    if !t.line.is_empty() {
        v["typed"] = json!(redact(t.line.text()));
    }
    v["output"] = output.into();
    v["earlier_lines"] = earlier.into();
    v["text_size"] = agent::number(t.font_size);
    v
}

impl Terminal {
    /// The working folder as it reads aloud (as of the last command while
    /// one runs).
    fn folder(&self) -> String {
        show_path(self.shell.as_ref().map_or(self.cwd.as_str(), |s| s.cwd.as_str()))
    }

    /// The command running and for how long, or null.
    fn running(&self) -> Value {
        match &self.job {
            Some(job) if !job.finished() => json!({
                "command": job.shown.as_str(),
                "seconds": vrt::time::now_ns().saturating_sub(job.started) / 1_000_000_000,
            }),
            _ => Value::Null,
        }
    }

    /// Line `abs` of the screen as text (a command line as the agent may
    /// read it).
    fn line_text(&self, abs: u64) -> String {
        if let Ok(i) = self.echoes.binary_search_by_key(&abs, |(line, _)| *line) {
            return self.echoes[i].1.clone();
        }
        let text: String = self.screen.line(abs).unwrap_or(&[]).iter().map(|c| c.ch).collect();
        text.trim_end().to_string()
    }

    /// Lines `from..to` of the screen as text.
    fn text(&self, from: u64, to: u64) -> String {
        let mut out = String::new();
        for abs in from.max(self.screen.base())..to.min(self.screen.end()) {
            out.push_str(&self.line_text(abs));
            out.push('\n');
        }
        out
    }

    /// The last `lines` lines of the screen, at most `max` bytes of them,
    /// and how many lines come before them.
    fn tail(&self, lines: usize, max: usize) -> (String, u64) {
        let (base, end) = (self.screen.base(), self.screen.end());
        let (mut start, mut size) = (end, 0);
        while start > base && ((end - start) as usize) < lines {
            let len = self.line_text(start - 1).len() + 1;
            if size + len > max {
                break;
            }
            size += len;
            start -= 1;
        }
        if start == end && end > base && lines > 0 {
            // A last line longer than all that: its end.
            let line = self.line_text(end - 1);
            let mut cut = line.len() - max.min(line.len());
            while !line.is_char_boundary(cut) {
                cut += 1;
            }
            return (format!("\u{2026}{}\n", &line[cut..]), end - 1 - base);
        }
        (self.text(start, end), start - base)
    }

    pub(crate) fn agent_invoke(&mut self, action: &str, args: &Value) -> Result<Value, String> {
        self.collect();
        match action {
            "run_command" | "run_risky_command" => {
                let command = arg_str(args, "command")?.trim();
                if command.is_empty() {
                    return Err("which command? Give the command line to run".into());
                }
                if command.chars().any(char::is_control) {
                    return Err("the command must be a single line".into());
                }
                if command.chars().count() > MAX_COMMAND {
                    return Err(format!(
                        "the command is longer than {MAX_COMMAND} characters; run it in shorter steps"
                    ));
                }
                if let Some(job) = &self.job {
                    return Err(format!(
                        "\u{201c}{}\u{201d} is still running: read_output shows what it prints, interrupt stops it",
                        job.shown
                    ));
                }
                // The standing consent of run_command covers only what loses
                // nothing (read as the shell will run it, now).
                if action == "run_command"
                    && self.shell.as_mut().is_none_or(|shell| shell.classify(command) == Effect::Destroys)
                {
                    return Err("this command line deletes, replaces, moves or ends something, or runs a program: \
                                run it with run_risky_command, which asks the user"
                        .into());
                }
                let shown = redact(command);
                // What the user was typing waits while the agent's command
                // runs.
                let typed = core::mem::take(&mut self.line);
                self.hsearch = None;
                self.line.set(command);
                // As Enter does: the line is echoed and starts.
                self.submit();
                let from = self.echoes.back().map_or(self.screen.end(), |(line, _)| line + 1);
                let finished = self.wait_for_job(RUN_WAIT_NS);
                self.line = typed;
                let (output, cut) = clip_middle(&self.text(from, self.screen.end()), MAX_OUTPUT);
                let mut v = json!({ "command": shown, "finished": finished, "output": output });
                if cut {
                    v["output_cut_short"] = json!(true);
                }
                if finished {
                    v["exit_status"] = json!(self.last_status);
                    v["folder"] = json!(self.folder());
                } else {
                    v["note"] = json!("it is still running: read_output shows what it prints next, interrupt stops it");
                }
                Ok(v)
            }
            "read_output" => {
                let lines = arg_opt_int(args, "lines")?.unwrap_or(READ_LINES).clamp(1, MAX_READ_LINES) as usize;
                let (output, earlier) = self.tail(lines, MAX_OUTPUT);
                let mut v = json!({
                    "output": output,
                    "earlier_lines": earlier,
                    "folder": self.folder(),
                    "running": self.running(),
                });
                if self.job.is_none() {
                    v["last_exit_status"] = json!(self.last_status);
                }
                Ok(v)
            }
            "interrupt" => {
                let Some(shown) = self.job.as_ref().map(|j| j.shown.clone()) else {
                    return Err("no command is running in the terminal".into());
                };
                self.interrupt();
                let stopped = self.wait_for_job(STOP_WAIT_NS);
                let mut v = json!({ "command": shown, "stopped": stopped });
                if !stopped {
                    v["note"] = json!(
                        "it is waiting for something it cannot leave at once (such as the network) and stops when \
                         that ends"
                    );
                }
                Ok(v)
            }
            "clear_screen" => {
                self.clear_screen();
                Ok(json!({ "cleared": true }))
            }
            "set_text_size" => {
                let points = arg_f64(args, "points")? as f32;
                self.font_size = if points <= 0.0 { DEFAULT_FONT_SIZE } else { points.clamp(9.0, 28.0) };
                Ok(json!({ "text_size": agent::number(self.font_size) }))
            }
            other => Err(format!("the Terminal has no action called {other}")),
        }
    }
}

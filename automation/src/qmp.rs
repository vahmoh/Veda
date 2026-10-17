// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A tiny client for the QEMU Machine Protocol (QMP), used to automate a
//! running virtual machine: screenshots, keyboard/mouse input, shutdown;
//! and what QEMU's input takes (its key codes and buttons, the keys that
//! type a character), which scripts are checked against before they run.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::util::Result;

pub struct Qmp {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Qmp {
    /// Connects to QEMU's QMP server at its socket, trying again until
    /// `timeout` expires (QEMU needs a moment to open the socket after
    /// starting), but not once `stopped` says why QEMU is gone.
    pub fn connect(socket: &Path, timeout: Duration, mut stopped: impl FnMut() -> Option<String>) -> Result<Self> {
        let start = Instant::now();
        let stream = loop {
            match UnixStream::connect(socket) {
                Ok(s) => break s,
                Err(e) => {
                    if let Some(why) = stopped() {
                        return Err(format!("{why} before its QMP socket opened"));
                    }
                    if start.elapsed() > timeout {
                        return Err(format!("cannot connect to QMP at {}: {e}", socket.display()));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
        let mut qmp = Qmp { reader: BufReader::new(stream.try_clone().map_err(|e| e.to_string())?), writer: stream };
        qmp.read_line()?; // greeting
        qmp.execute("qmp_capabilities", json!({}))?;
        Ok(qmp)
    }

    fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        self.reader.read_line(&mut line).map_err(|e| format!("QMP read: {e}"))?;
        if line.is_empty() {
            return Err("QMP connection closed".into());
        }
        Ok(line)
    }

    /// Executes a command and waits for its result (the value of its
    /// `return`), skipping the events QEMU sends meanwhile.
    pub fn execute(&mut self, command: &str, arguments: Value) -> Result<Value> {
        let msg = json!({ "execute": command, "arguments": arguments }).to_string() + "\n";
        self.writer.write_all(msg.as_bytes()).map_err(|e| format!("QMP write: {e}"))?;
        loop {
            let line = self.read_line()?;
            if let Some(reply) = reply(&line) {
                return reply.map_err(|why| format!("QMP {command} failed: {why}"));
            }
        }
    }

    /// Gives up on answers QEMU takes longer than `timeout` for.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.writer.set_read_timeout(Some(timeout)).ok();
    }

    /// QEMU's state, as `query-status` tells it: `running`, `paused`,
    /// `internal-error`, ...
    pub fn status(&mut self) -> Result<String> {
        let reply = self.execute("query-status", json!({}))?;
        reply["status"].as_str().map(String::from).ok_or_else(|| format!("query-status: {reply}"))
    }

    /// What a command of QEMU's monitor (its human interface) prints, with
    /// `\n` line ends.
    pub fn human(&mut self, command: &str) -> Result<String> {
        let reply = self.execute("human-monitor-command", json!({ "command-line": command }))?;
        reply.as_str().map(|text| text.replace("\r\n", "\n")).ok_or_else(|| format!("{command}: {reply}"))
    }

    pub fn screenshot(&mut self, path: &Path) -> Result {
        self.execute("screendump", json!({ "filename": path.to_string_lossy(), "format": "png" })).map(|_| ())
    }

    /// Presses and releases a key combination given in QEMU's `qcode` names
    /// joined by `-` (e.g. `ctrl-alt-t`, `ret`, `a`).
    pub fn send_keys(&mut self, combo: &str) -> Result {
        let keys: Vec<Value> = combo.split('-').map(|k| json!({ "type": "qcode", "data": k })).collect();
        self.execute("send-key", json!({ "keys": keys })).map(|_| ())
    }

    /// Types `text` by sending the keys of each character ([`keys_for`]).
    /// A character no key types is an error, which does not show the text
    /// (it may be a secret).
    pub fn type_text(&mut self, text: &str) -> Result {
        if text.chars().any(|c| keys_for(c).is_none()) {
            return Err("the text has a character no key types".into());
        }
        for c in text.chars() {
            self.send_keys(&keys_for(c).unwrap_or_default())?;
            std::thread::sleep(Duration::from_millis(30));
        }
        Ok(())
    }

    /// Moves the absolute pointer (virtio-tablet) to a position given as a
    /// fraction of the screen (0.0 ..= 1.0).
    pub fn move_mouse(&mut self, fx: f64, fy: f64) -> Result {
        let (x, y) = ((fx.clamp(0.0, 1.0) * 32767.0) as u32, (fy.clamp(0.0, 1.0) * 32767.0) as u32);
        let events = json!([
            { "type": "abs", "data": { "axis": "x", "value": x } },
            { "type": "abs", "data": { "axis": "y", "value": y } },
        ]);
        self.execute("input-send-event", json!({ "events": events })).map(|_| ())
    }

    /// Presses or releases a mouse button (one of [`BUTTONS`]).
    pub fn mouse_button(&mut self, button: &str, down: bool) -> Result {
        let event = json!({ "type": "btn", "data": { "button": button, "down": down } });
        self.execute("input-send-event", json!({ "events": [event] })).map(|_| ())
    }

    /// Presses or releases one key (QEMU qcode), e.g. to hold a modifier.
    pub fn key_event(&mut self, qcode: &str, down: bool) -> Result {
        let event = json!({ "type": "key", "data": { "down": down, "key": { "type": "qcode", "data": qcode } } });
        self.execute("input-send-event", json!({ "events": [event] })).map(|_| ())
    }

    pub fn quit(&mut self) {
        let _ = self.execute("quit", json!({}));
    }
}

/// QEMU's key codes (`QKeyCode` in its `qapi/ui.json`), the names the
/// keyboard's commands take.
pub const KEY_CODES: &[&str] = &[
    "unmapped",
    "shift",
    "shift_r",
    "alt",
    "alt_r",
    "ctrl",
    "ctrl_r",
    "menu",
    "esc",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "0",
    "minus",
    "equal",
    "backspace",
    "tab",
    "q",
    "w",
    "e",
    "r",
    "t",
    "y",
    "u",
    "i",
    "o",
    "p",
    "bracket_left",
    "bracket_right",
    "ret",
    "a",
    "s",
    "d",
    "f",
    "g",
    "h",
    "j",
    "k",
    "l",
    "semicolon",
    "apostrophe",
    "grave_accent",
    "backslash",
    "z",
    "x",
    "c",
    "v",
    "b",
    "n",
    "m",
    "comma",
    "dot",
    "slash",
    "asterisk",
    "spc",
    "caps_lock",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "num_lock",
    "scroll_lock",
    "kp_divide",
    "kp_multiply",
    "kp_subtract",
    "kp_add",
    "kp_enter",
    "kp_decimal",
    "sysrq",
    "kp_0",
    "kp_1",
    "kp_2",
    "kp_3",
    "kp_4",
    "kp_5",
    "kp_6",
    "kp_7",
    "kp_8",
    "kp_9",
    "less",
    "f11",
    "f12",
    "print",
    "home",
    "pgup",
    "pgdn",
    "end",
    "left",
    "up",
    "down",
    "right",
    "insert",
    "delete",
    "stop",
    "again",
    "props",
    "undo",
    "front",
    "copy",
    "open",
    "paste",
    "find",
    "cut",
    "lf",
    "help",
    "meta_l",
    "meta_r",
    "compose",
    "pause",
    "ro",
    "hiragana",
    "henkan",
    "yen",
    "muhenkan",
    "katakanahiragana",
    "kp_comma",
    "kp_equals",
    "power",
    "sleep",
    "wake",
    "audionext",
    "audioprev",
    "audiostop",
    "audioplay",
    "audiomute",
    "volumeup",
    "volumedown",
    "mediaselect",
    "mail",
    "calculator",
    "computer",
    "ac_home",
    "ac_back",
    "ac_forward",
    "ac_refresh",
    "ac_bookmarks",
    "lang1",
    "lang2",
    "f13",
    "f14",
    "f15",
    "f16",
    "f17",
    "f18",
    "f19",
    "f20",
    "f21",
    "f22",
    "f23",
    "f24",
];

/// The buttons of QEMU's pointers (`InputButton`).
pub const BUTTONS: &[&str] =
    &["left", "middle", "right", "wheel-up", "wheel-down", "side", "extra", "wheel-left", "wheel-right"];

/// Checks a key combination (key codes joined by `-`, such as `ctrl-alt-t`).
pub fn check_combo(combo: &str) -> Result {
    match combo.split('-').find(|key| !KEY_CODES.contains(key)) {
        Some(key) => Err(format!("'{key}' is not one of QEMU's key codes (such as ret, spc, ctrl, meta_l, f1, a)")),
        None => Ok(()),
    }
}

/// The keys that type `c` on a US keyboard, as a combination for
/// [`Qmp::send_keys`]; `None` for a character no key types.
pub fn keys_for(c: char) -> Option<String> {
    Some(match c {
        'a'..='z' | '0'..='9' => c.to_string(),
        'A'..='Z' => format!("shift-{}", c.to_ascii_lowercase()),
        ' ' => "spc".into(),
        '\n' => "ret".into(),
        '.' => "dot".into(),
        ',' => "comma".into(),
        '-' => "minus".into(),
        '/' => "slash".into(),
        '!' => "shift-1".into(),
        '?' => "shift-slash".into(),
        ':' => "shift-semicolon".into(),
        ';' => "semicolon".into(),
        '\'' => "apostrophe".into(),
        '(' => "shift-9".into(),
        ')' => "shift-0".into(),
        '\t' => "tab".into(),
        '=' => "equal".into(),
        '+' => "shift-equal".into(),
        '_' => "shift-minus".into(),
        '[' => "bracket_left".into(),
        ']' => "bracket_right".into(),
        '{' => "shift-bracket_left".into(),
        '}' => "shift-bracket_right".into(),
        '\\' => "backslash".into(),
        '|' => "shift-backslash".into(),
        '"' => "shift-apostrophe".into(),
        '<' => "shift-comma".into(),
        '>' => "shift-dot".into(),
        '`' => "grave_accent".into(),
        '~' => "shift-grave_accent".into(),
        '@' => "shift-2".into(),
        '#' => "shift-3".into(),
        '$' => "shift-4".into(),
        '%' => "shift-5".into(),
        '^' => "shift-6".into(),
        '&' => "shift-7".into(),
        '*' => "shift-8".into(),
        _ => return None,
    })
}

/// What a line from QEMU answers: `Some(Ok(value))` for a command's
/// `return`, `Some(Err(why))` for its `error`, and `None` for anything else
/// (an event, or a line that is not a message).
fn reply(line: &str) -> Option<std::result::Result<Value, String>> {
    let mut message: Value = serde_json::from_str(line).ok()?;
    if let Some(value) = message.get_mut("return") {
        return Some(Ok(value.take()));
    }
    let error = message.get("error")?;
    let why = error["desc"].as_str().map_or_else(|| error.to_string(), String::from);
    Some(Err(why))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_and_events() {
        let status = r#"{"return": {"status": "running", "singlestep": false, "running": true}}"#;
        assert_eq!(reply(status).unwrap().unwrap()["status"], "running");
        let text = r#"{"return": "CPU#0\r\nsaid \"hi\" é\r\n"}"#;
        assert_eq!(reply(text).unwrap().unwrap(), "CPU#0\r\nsaid \"hi\" é\r\n");
        assert_eq!(reply(r#"{"return": {}}"#).unwrap().unwrap(), json!({}));
        let error = r#"{"error": {"class": "GenericError", "desc": "No such device"}}"#;
        assert_eq!(reply(error).unwrap(), Err("No such device".to_string()));
        // An event is not the answer, even one that reports an error.
        let event = r#"{"event": "BLOCK_JOB_ERROR", "data": {"error": "nospace"}, "timestamp": {"seconds": 1}}"#;
        assert_eq!(reply(event), None);
        assert_eq!(reply("not json"), None);
    }

    #[test]
    fn keys_are_qemus() {
        for combo in ["ret", "ctrl-alt-t", "meta_l-up", "shift-delete", "ctrl-shift-v", "alt-f4", "kp_enter"] {
            assert_eq!(check_combo(combo), Ok(()), "{combo}");
        }
        assert!(check_combo("ctrl-enter").is_err_and(|e| e.contains("'enter'")));
        assert!(check_combo("").is_err());
        // Every key a character is typed with is one of QEMU's.
        for c in (' '..='~').chain(['\n', '\t']) {
            let keys = keys_for(c).unwrap_or_else(|| panic!("{c:?} has no keys"));
            assert_eq!(check_combo(&keys), Ok(()), "{c:?}");
        }
        assert_eq!(keys_for('é'), None);
    }

    #[test]
    fn connecting_gives_up_once_qemu_has_stopped() {
        let socket = std::env::temp_dir().join(format!("automation-qmp-{}.sock", std::process::id()));
        let started = Instant::now();
        let mut asked = 0;
        let result = Qmp::connect(&socket, Duration::from_secs(20), || {
            asked += 1;
            (asked == 3).then(|| String::from("QEMU exited (exit status: 1)"))
        });
        assert!(result.is_err_and(|e| e == "QEMU exited (exit status: 1) before its QMP socket opened"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}

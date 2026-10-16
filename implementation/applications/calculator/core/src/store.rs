// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the Calculator remembers between runs, in `~/.config/calculator/state`: its mode and
//! settings, the memory and the history.
//!
//! The file is text, one setting a line; numbers are kept exactly (`3/10`, `pi 1/6`, a float's
//! bits), so that a remembered result is the same number:
//!
//! ```text
//! calculator 1
//! mode scientific
//! angle radians
//! grouping 1
//! scientific 0
//! base hex
//! word qword
//! converter 4 0 1
//! memory 3/10
//! history<TAB>number<TAB>base or -<TAB>size or -<TAB>expression
//! ```
//!
//! Lines that do not read are skipped, so a damaged file loses only what it damaged.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::calculator::{Base, Calculator, HistoryItem, Mode};
use crate::eval::WordSize;
use crate::number::{AngleUnit, Number};

/// What is kept.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Saved {
    pub mode: Mode,
    pub angle: AngleUnit,
    pub grouping: bool,
    pub scientific: bool,
    pub base: Base,
    pub word: WordSize,
    pub category: usize,
    pub units: Option<(usize, usize)>,
    pub memory: Option<Number>,
    pub history: Vec<HistoryItem>,
}

fn number_text(n: Number) -> String {
    match n {
        Number::Exact(r) => format!("{}/{}", r.numer(), r.denom()),
        Number::Pi(r) => format!("pi {}/{}", r.numer(), r.denom()),
        Number::Approx(x) => format!("float {:016x}", x.to_bits()),
    }
}

fn read_number(text: &str) -> Option<Number> {
    let fraction = |t: &str| -> Option<Number> {
        let (n, d) = t.split_once('/')?;
        Number::fraction(n.parse().ok()?, d.parse().ok()?)
    };
    if let Some(bits) = text.strip_prefix("float ") {
        let x = f64::from_bits(u64::from_str_radix(bits, 16).ok()?);
        return Number::from_f64(x).ok();
    }
    if let Some(r) = text.strip_prefix("pi ") {
        return fraction(r)?.mul(Number::PI).ok();
    }
    fraction(text)
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Standard => "standard",
        Mode::Scientific => "scientific",
        Mode::Programmer => "programmer",
        Mode::Converter => "converter",
    }
}

fn base_name(b: Base) -> &'static str {
    match b {
        Base::Hex => "hex",
        Base::Dec => "dec",
        Base::Oct => "oct",
        Base::Bin => "bin",
    }
}

fn read_base(t: &str) -> Option<Base> {
    Base::ALL.into_iter().find(|b| base_name(*b) == t)
}

fn read_word(t: &str) -> Option<WordSize> {
    WordSize::ALL.into_iter().find(|w| w.label().eq_ignore_ascii_case(t))
}

/// The calculator's state as the file's text.
pub fn save(c: &Calculator) -> String {
    let s = c.saved();
    let mut out = String::from("calculator 1\n");
    out.push_str(&format!("mode {}\n", mode_name(s.mode)));
    out.push_str(&format!("angle {}\n", s.angle.name()));
    out.push_str(&format!("grouping {}\n", s.grouping as u8));
    out.push_str(&format!("scientific {}\n", s.scientific as u8));
    out.push_str(&format!("base {}\n", base_name(s.base)));
    out.push_str(&format!("word {}\n", s.word.label().to_ascii_lowercase()));
    let (from, to) = s.units.unwrap_or((0, 0));
    out.push_str(&format!("converter {} {from} {to}\n", s.category));
    if let Some(m) = s.memory {
        out.push_str(&format!("memory {}\n", number_text(m)));
    }
    for h in &s.history {
        let (base, word) = match h.programmer {
            Some((b, w)) => (base_name(b), w.label().to_ascii_lowercase()),
            None => ("-", String::from("-")),
        };
        let expression: String = h.expression.chars().map(|c| if c == '\t' || c == '\n' { ' ' } else { c }).collect();
        out.push_str(&format!("history\t{}\t{base}\t{word}\t{expression}\n", number_text(h.result)));
    }
    out
}

/// A calculator as `text` (the file's) left it: the defaults for what it lacks.
pub fn load(text: &str) -> Calculator {
    let mut c = Calculator::new();
    let mut s = c.saved();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("history\t") {
            let mut parts = rest.splitn(4, '\t');
            let (Some(n), Some(base), Some(word), Some(expression)) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let Some(result) = read_number(n) else { continue };
            let programmer = read_base(base).zip(read_word(word));
            s.history.push(HistoryItem { expression: expression.into(), result, programmer });
            continue;
        }
        let Some((key, value)) = line.split_once(' ') else { continue };
        match key {
            "mode" => {
                if let Some(m) = Mode::ALL.into_iter().find(|m| mode_name(*m) == value) {
                    s.mode = m;
                }
            }
            "angle" => {
                if let Some(a) = [AngleUnit::Degrees, AngleUnit::Radians, AngleUnit::Gradians]
                    .into_iter()
                    .find(|a| a.name() == value)
                {
                    s.angle = a;
                }
            }
            "grouping" => s.grouping = value == "1",
            "scientific" => s.scientific = value == "1",
            "base" => s.base = read_base(value).unwrap_or(s.base),
            "word" => s.word = read_word(value).unwrap_or(s.word),
            "converter" => {
                let v: Vec<usize> = value.split(' ').filter_map(|x| x.parse().ok()).collect();
                if let [category, from, to] = v[..] {
                    s.category = category;
                    s.units = Some((from, to));
                }
            }
            "memory" => s.memory = read_number(value),
            _ => {}
        }
    }
    let keep = s.history.len().saturating_sub(crate::calculator::HISTORY_MAX);
    s.history.drain(..keep);
    c.restore(s);
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calculator::Key;

    #[test]
    fn what_is_saved_reads_back() {
        let mut c = Calculator::new();
        c.set_mode(Mode::Scientific);
        c.angle = AngleUnit::Radians;
        c.type_text("1/3=");
        c.press(Key::MemoryStore);
        c.type_text("2^0.5=");
        c.set_mode(Mode::Programmer);
        c.set_base(Base::Hex);
        c.set_word_size(WordSize::Word);
        c.type_text("FF+1=");
        c.converter.set_category(4);
        c.converter.set_to(2);
        let text = save(&c);
        let back = load(&text);
        assert_eq!(save(&back), text);
        assert_eq!(back.mode(), Mode::Programmer);
        assert_eq!(back.angle, AngleUnit::Radians);
        assert_eq!(back.memory(), Number::fraction(1, 3));
        assert_eq!(back.history().len(), 3);
        assert_eq!(back.history()[1].result, Number::Approx(core::f64::consts::SQRT_2));
        assert_eq!(back.history()[2].programmer, Some((Base::Hex, WordSize::Word)));
        assert_eq!((back.converter.category, back.converter.to), (4, 2));
        assert_eq!(back.ans(), Number::Approx(core::f64::consts::SQRT_2), "the last result carries on");
    }

    #[test]
    fn damage_loses_only_what_it_damaged() {
        let c = load(
            "calculator 1\nmode nonsense\nangle gradians\nmemory 1/0\nhistory\tbad\t-\t-\t1+1\nhistory\t2/1\t-\t-\t1+1\nconverter 99 99\n",
        );
        assert_eq!(c.mode(), Mode::Standard);
        assert_eq!(c.angle, AngleUnit::Gradians);
        assert_eq!(c.memory(), None);
        assert_eq!(c.history().len(), 1);
        assert_eq!(c.history()[0].result, Number::int(2));
        let c = load("");
        assert_eq!(c.mode(), Mode::Standard);
        assert_eq!(read_number("pi 1/6"), Number::fraction(1, 6).unwrap().mul(Number::PI).ok());
        assert_eq!(read_number(&number_text(Number::Approx(0.1))), Some(Number::Approx(0.1)));
    }
}

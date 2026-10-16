// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Calculator for the voice agent: what it shows (the expression, its result, the mode and
//! its settings, the memory, the latest calculations) and what it does (calculating,
//! converting units, switching modes and settings, the memory), through the same calculator
//! the keys drive, so that the window shows what the agent did.
//!
//! Everything is routine but clearing the history, which forgets calculations for good.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use calculator_core::format::{Style, format, fraction_text};
use calculator_core::units::{CATEGORIES, find_unit};
use calculator_core::{AngleUnit, Base, Calculator, Key, Mode, Number, WordSize, format_word};
use vui::agent::{self, Action, AppAgentInfo, Risk, Value, arg_bool, arg_f64, arg_opt_str, arg_str, json};

/// The calculations the agent is shown, the latest last.
const HISTORY_SHOWN: usize = 10;

pub fn info() -> AppAgentInfo {
    agent::info(
        "A calculator with exact fractions, scientific functions, a programmer's mode (hexadecimal, binary, bitwise operators) and unit conversions; it shows calculations on screen.",
        vec![
            Action::new(
                "calculate",
                "Works out a calculation on the calculator's display, as if typed, and returns the result. Write it \
                 with + - * / ^ and parentheses, % for percent (80*15% is 15% of 80; 50+10% adds 10%), ! for \
                 factorial, functions such as sqrt, cbrt, root(x, n), sin, cos, tan, asin, ln, log, log2, exp, abs, \
                 round, floor, ceil, gcd, lcm, nCr, nPr, and the constants pi and e. Angles follow the angle unit.",
            )
            .param("expression", "string", "The calculation, such as 12*(3+4) or sqrt(2)/2", true)
            .choice(
                "base",
                "For whole-number (programmer) arithmetic: the base the numbers are written in. Leave it out for \
                 ordinary calculations",
                false,
                &["hex", "dec", "oct", "bin"],
            )
            .build(),
            Action::new("convert", "Converts a quantity between units of the same kind, on the calculator's converter")
                .param("value", "number", "How much", true)
                .param("from", "string", "The unit converted from, by name or symbol: km, miles, °F, kg, GiB", true)
                .param("to", "string", "The unit converted to", true)
                .build(),
            Action::new("set_mode", "Shows one of the calculator's modes")
                .choice("mode", "The mode", true, &["standard", "scientific", "programmer", "converter"])
                .build(),
            Action::new("set_angle_unit", "Sets the unit trigonometric functions take and give")
                .choice("unit", "The angle unit", true, &["degrees", "radians", "gradians"])
                .build(),
            Action::new("set_programmer", "Sets the programmer mode's base and number size, and shows the mode")
                .choice("base", "The base numbers are shown and typed in", false, &["hex", "dec", "oct", "bin"])
                .choice("size", "The size of the numbers", false, &["qword", "dword", "word", "byte"])
                .build(),
            Action::new("memory", "Uses the calculator's memory")
                .choice("operation", "What to do", true, &["store", "add", "subtract", "recall", "clear"])
                .param(
                    "value",
                    "number",
                    "A number to store, add or subtract (what the display shows if not given)",
                    false,
                )
                .build(),
            Action::new("clear", "Clears the expression on the display (the history and memory stay)").build(),
            Action::new("settings", "Changes how numbers are shown")
                .param("digit_grouping", "boolean", "Thousands separators (1,234,567)", false)
                .param("scientific_notation", "boolean", "Always scientific notation (1.2E+3)", false)
                .build(),
            Action::new("clear_history", "Forgets every calculation in the history").risk(Risk::Destructive).build(),
        ],
    )
}

fn mode_key(m: Mode) -> &'static str {
    match m {
        Mode::Standard => "standard",
        Mode::Scientific => "scientific",
        Mode::Programmer => "programmer",
        Mode::Converter => "converter",
    }
}

fn base_arg(name: &str) -> Option<Base> {
    Base::ALL.into_iter().find(|b| b.label().eq_ignore_ascii_case(name))
}

/// A number as the agent reads it: plain digits, ASCII signs, no separators.
fn plain(n: Number) -> String {
    format(n, &Style::PLAIN)
}

/// A whole number of Programmer mode in every base.
fn word_json(v: i64, size: WordSize) -> Value {
    let w = |b| format_word(v, b, size, false).replace('\u{2212}', "-");
    json!({ "hex": w(Base::Hex), "dec": w(Base::Dec), "oct": w(Base::Oct), "bin": w(Base::Bin) })
}

pub fn state(c: &Calculator) -> Value {
    let screen = c.screen();
    let ascii = |s: &str| s.replace('\u{2212}', "-");
    let history: Vec<Value> = c
        .history()
        .iter()
        .rev()
        .take(HISTORY_SHOWN)
        .rev()
        .map(|h| {
            let result = match h.programmer {
                Some((base, size)) => format_word(h.result.as_integer().unwrap_or(0) as i64, base, size, false),
                None => plain(h.result),
            };
            json!({ "expression": ascii(&h.expression), "result": ascii(&result) })
        })
        .collect();
    let mut v = json!({
        "mode": mode_key(c.mode()),
        "memory": c.memory().map(plain),
        "history": history,
        "calculations_in_history": c.history().len(),
    });
    match c.mode() {
        Mode::Converter => {
            let conv = &c.converter;
            v["converter"] = json!({
                "category": conv.category().name,
                "from": conv.from_unit().name,
                "to": conv.to_unit().name,
                "value": ascii(conv.input()),
                "result": conv.result().ok().map(plain),
            });
        }
        mode => {
            v["expression"] = json!(ascii(&if screen.is_result { screen.above.clone() } else { screen.main.clone() }));
            v["result"] = json!(if screen.is_result { Some(ascii(&screen.main)) } else { None });
            v["result_so_far"] = json!(screen.preview.as_deref().map(ascii));
            v["error"] = json!(screen.error);
            if mode == Mode::Scientific {
                v["angle_unit"] = json!(c.angle.name());
                v["scientific_notation"] = json!(c.style.scientific);
            }
            if mode == Mode::Programmer {
                v["programmer"] = json!({
                    "base": c.base().label(),
                    "size": c.word_size().label(),
                    "value": c.word_value().map(|w| word_json(w, c.word_size())),
                });
            }
        }
    }
    v
}

/// An expression as a language model may write it, in the calculator's terms: `**` is a power,
/// and an `x` between numbers a multiplication.
fn normalize(expression: &str) -> String {
    let text = expression.trim().replace("**", "^");
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &ch) in chars.iter().enumerate() {
        let before = chars[..i].iter().rev().find(|c| !c.is_whitespace());
        let after = chars[i + 1..].iter().find(|c| !c.is_whitespace());
        let operand_end = |c: Option<&char>| c.is_some_and(|c| c.is_ascii_digit() || *c == ')' || *c == '.');
        let operand_start = |c: Option<&char>| c.is_some_and(|c| c.is_ascii_digit() || *c == '(' || *c == '.');
        if (ch == 'x' || ch == 'X') && operand_end(before) && operand_start(after) {
            out.push('×');
        } else {
            out.push(ch);
        }
    }
    out
}

pub fn invoke(c: &mut Calculator, action: &str, args: &Value) -> Result<Value, String> {
    match action {
        "calculate" => {
            let expression = normalize(arg_str(args, "expression")?);
            if expression.is_empty() {
                return Err("the expression is empty".into());
            }
            match arg_opt_str(args, "base") {
                Some(b) => {
                    let base = base_arg(b).ok_or_else(|| format!("there is no base called {b}"))?;
                    c.set_mode(Mode::Programmer);
                    c.set_base(base);
                }
                None if !c.mode().is_decimal() => c.set_mode(Mode::Standard),
                None => {}
            }
            c.set_expression(&expression);
            c.press(Key::Equals);
            let screen = c.screen();
            if let Some(e) = screen.error {
                return Err(format!("{e} (in {expression})"));
            }
            let mut v = json!({
                "expression": screen.above.trim_end_matches(" =").replace('\u{2212}', "-"),
                "result": screen.main.replace('\u{2212}', "-").replace(',', ""),
            });
            if c.mode() == Mode::Programmer {
                v["value"] = c.word_value().map_or(Value::Null, |w| word_json(w, c.word_size()));
            } else {
                let ans = c.ans();
                if let Some(f) = fraction_text(ans, &Style::PLAIN) {
                    v["fraction"] = json!(f);
                }
                v["exact"] = json!(ans.is_exact());
            }
            Ok(v)
        }
        "convert" => {
            let value = arg_f64(args, "value")?;
            let (from, to) = (arg_str(args, "from")?, arg_str(args, "to")?);
            let (cf, uf) = find_unit(from).ok_or_else(|| format!("there is no unit called {from}"))?;
            let (ct, ut) = find_unit(to).ok_or_else(|| format!("there is no unit called {to}"))?;
            if cf != ct {
                return Err(format!(
                    "{} measures {} and {} measures {}: they do not convert",
                    CATEGORIES[cf].units[uf].name,
                    CATEGORIES[cf].name.to_lowercase(),
                    CATEGORIES[ct].units[ut].name,
                    CATEGORIES[ct].name.to_lowercase()
                ));
            }
            // The number as written: exact where it is (0.1 is a tenth, not its float).
            let written = args.get("value").map(|v| match v {
                Value::String(s) => s.trim().to_string(),
                other => other.to_string(),
            });
            let input = written
                .filter(|w| calculator_core::format::parse_decimal(w.trim_start_matches('-')).is_some())
                .unwrap_or_else(|| format!("{value}"));
            c.set_mode(Mode::Converter);
            c.converter.set_category(cf);
            c.converter.set_from(uf);
            c.converter.set_to(ut);
            c.converter.set_input(&input.replace('-', "\u{2212}"));
            let (from, to) = (c.converter.from_unit(), c.converter.to_unit());
            let result = c.converter.result().map_err(|e| e.to_string())?;
            Ok(json!({ "value": input, "from": from.name, "to": to.name, "result": plain(result) }))
        }
        "set_mode" => {
            let m = arg_str(args, "mode")?;
            let mode =
                Mode::ALL.into_iter().find(|x| mode_key(*x) == m).ok_or_else(|| format!("there is no {m} mode"))?;
            c.set_mode(mode);
            Ok(json!({ "mode": mode_key(mode) }))
        }
        "set_angle_unit" => {
            let u = arg_str(args, "unit")?;
            let unit = [AngleUnit::Degrees, AngleUnit::Radians, AngleUnit::Gradians]
                .into_iter()
                .find(|a| a.name() == u)
                .ok_or_else(|| format!("there is no angle unit called {u}"))?;
            c.angle = unit;
            Ok(json!({ "angle_unit": unit.name() }))
        }
        "set_programmer" => {
            if let Some(b) = arg_opt_str(args, "base") {
                let base = base_arg(b).ok_or_else(|| format!("there is no base called {b}"))?;
                c.set_base(base);
            }
            if let Some(s) = arg_opt_str(args, "size") {
                let size = WordSize::ALL
                    .into_iter()
                    .find(|w| w.label().eq_ignore_ascii_case(s))
                    .ok_or_else(|| format!("there is no size called {s}"))?;
                c.set_word_size(size);
            }
            c.set_mode(Mode::Programmer);
            Ok(json!({ "base": c.base().label(), "size": c.word_size().label() }))
        }
        "memory" => {
            let op = arg_str(args, "operation")?;
            // The memory is the calculator's, not the converter's.
            if c.mode() == Mode::Converter {
                c.set_mode(Mode::Standard);
            }
            if let Ok(x) = arg_f64(args, "value") {
                // The number the agent gave, in the display for the memory to take.
                let text = match args.get("value") {
                    Some(Value::String(s)) => s.trim().to_string(),
                    _ => format!("{x}"),
                };
                c.set_expression(&text.replace('-', "\u{2212}"));
            }
            let key = match op {
                "store" => Key::MemoryStore,
                "add" => Key::MemoryAdd,
                "subtract" => Key::MemorySubtract,
                "recall" => Key::MemoryRecall,
                "clear" => Key::MemoryClear,
                other => return Err(format!("the memory cannot {other}")),
            };
            if key == Key::MemoryRecall && c.memory().is_none() {
                return Err("there is nothing in memory".into());
            }
            c.press(key);
            Ok(json!({ "memory": c.memory().map(plain) }))
        }
        "clear" => {
            c.press(Key::Clear);
            Ok(json!({ "cleared": true }))
        }
        "settings" => {
            if let Some(g) = arg_bool(args, "digit_grouping") {
                c.style.grouping = g;
            }
            if let Some(s) = arg_bool(args, "scientific_notation") {
                c.style.scientific = s;
            }
            Ok(json!({ "digit_grouping": c.style.grouping, "scientific_notation": c.style.scientific }))
        }
        "clear_history" => {
            let n = c.history().len();
            c.clear_history();
            Ok(json!({ "forgotten": n }))
        }
        other => Err(format!("the Calculator has no action called {other}")),
    }
}

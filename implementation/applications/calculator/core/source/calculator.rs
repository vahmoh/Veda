// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The calculator: what each key does in each mode, what the display shows, the history and
//! the memory.
//!
//! Typing builds an expression, which is worked out as it is typed (the preview) and when `=`
//! is pressed. After `=` the result stays in the expression for what comes next: an operator
//! carries on from it (`=`, then `+ 5`), a number starts afresh, and `=` again repeats the last
//! operation (`2 + 3 = = =` counts 5, 8, 11, 14). A function key applies to the number just
//! typed (`9 √` is √9, `30 sin` is sin(30)), as on most calculators, or, with none there,
//! begins a call for what follows.
//!
//! Standard and Scientific modes share their expression; Programmer mode and the converter
//! have their own, so that switching modes loses nothing.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::entry::{ANS_MARK, Entry, Kind};
use crate::eval::{Env, WordSize, evaluate, evaluate_word};
use crate::format::{Style, exact_text, format};
use crate::lexer::{Func, tokenize};
use crate::number::{AngleUnit, CalcError, Number, Syntax};
use crate::parser::{BinOp, Expr, Node, open_parens, parse};
use crate::units::{CATEGORIES, Category, Unit, convert};

/// The history's length.
pub const HISTORY_MAX: usize = 100;

/// The calculator's modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Standard,
    Scientific,
    Programmer,
    Converter,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Standard, Mode::Scientific, Mode::Programmer, Mode::Converter];

    pub fn name(self) -> &'static str {
        match self {
            Mode::Standard => "Standard",
            Mode::Scientific => "Scientific",
            Mode::Programmer => "Programmer",
            Mode::Converter => "Converter",
        }
    }

    /// Whether the mode works out decimal expressions (Standard and Scientific).
    pub fn is_decimal(self) -> bool {
        matches!(self, Mode::Standard | Mode::Scientific)
    }
}

/// Programmer mode's bases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Base {
    Hex,
    #[default]
    Dec,
    Oct,
    Bin,
}

impl Base {
    pub const ALL: [Base; 4] = [Base::Hex, Base::Dec, Base::Oct, Base::Bin];

    pub fn radix(self) -> u32 {
        match self {
            Base::Hex => 16,
            Base::Dec => 10,
            Base::Oct => 8,
            Base::Bin => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Base::Hex => "HEX",
            Base::Dec => "DEC",
            Base::Oct => "OCT",
            Base::Bin => "BIN",
        }
    }

    /// The digits shown together, between spaces.
    fn group(self) -> usize {
        match self {
            Base::Hex | Base::Bin => 4,
            Base::Dec | Base::Oct => 3,
        }
    }
}

/// `v` (a whole number of `size`) written in `base`: signed in decimal, its bits otherwise
/// (two's complement); grouped (`FFFF FFFF`, `1,024`) if asked.
pub fn format_word(v: i64, base: Base, size: WordSize, grouping: bool) -> String {
    let digits = match base {
        Base::Dec => format!("{}", v.unsigned_abs()),
        Base::Hex => format!("{:X}", size.unsigned(v)),
        Base::Oct => format!("{:o}", size.unsigned(v)),
        Base::Bin => format!("{:b}", size.unsigned(v)),
    };
    let mut out = String::new();
    if base == Base::Dec && v < 0 {
        out.push('\u{2212}');
    }
    let separator = if base == Base::Dec { ',' } else { ' ' };
    let n = base.group();
    for (i, c) in digits.chars().enumerate() {
        if grouping && i > 0 && (digits.len() - i).is_multiple_of(n) {
            out.push(separator);
        }
        out.push(c);
    }
    out
}

/// A key: of the keypads, or what a typed character means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A digit, 0 to 15 (hexadecimal digits in Programmer mode).
    Digit(u8),
    Point,
    /// `E`: the exponent of scientific notation.
    Exponent,
    Add,
    Subtract,
    Multiply,
    Divide,
    Power,
    Modulo,
    Open,
    Close,
    /// ±
    Negate,
    Percent,
    Square,
    Cube,
    Reciprocal,
    Factorial,
    SquareRoot,
    CubeRoot,
    /// ʸ√x: a root of any degree.
    Root,
    /// 10ˣ
    PowerOfTen,
    /// eˣ
    Exp,
    Function(Func),
    Pi,
    Euler,
    Ans,
    And,
    Or,
    Xor,
    Nand,
    Nor,
    Not,
    ShiftLeft,
    ShiftRight,
    RotateLeft,
    RotateRight,
    Equals,
    /// C: clears the expression.
    Clear,
    /// CE: clears the number being entered.
    ClearEntry,
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    MemoryClear,
    MemoryRecall,
    MemoryAdd,
    MemorySubtract,
    MemoryStore,
}

/// A calculation done: what was worked out and its result.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryItem {
    /// The expression as it was shown.
    pub expression: String,
    pub result: Number,
    /// For Programmer mode's: the base and size it was worked out in.
    pub programmer: Option<(Base, WordSize)>,
}

/// Where an expression is.
#[derive(Debug, Clone, PartialEq)]
enum State {
    Editing,
    /// `=` worked the expression out (shown as it was), and the entry holds the result.
    Result {
        expression: String,
    },
    /// `=` found the expression wanting: why, and where in it (a byte offset), if known.
    Error {
        message: String,
        at: Option<usize>,
    },
}

/// An expression with its state: Standard and Scientific modes' and Programmer mode's.
#[derive(Debug, Clone)]
struct Session {
    entry: Entry,
    state: State,
    /// The last operation `=` did, which `=` again repeats: the operator and its right operand.
    repeat: Option<(BinOp, Number)>,
}

impl Session {
    fn new(programmer: bool) -> Session {
        Session { entry: Entry::new(programmer), state: State::Editing, repeat: None }
    }
}

/// The unit converter: a category, the units converted from and to, and the value typed.
#[derive(Debug, Clone)]
pub struct Converter {
    pub category: usize,
    pub from: usize,
    pub to: usize,
    entry: Entry,
}

impl Converter {
    fn new() -> Converter {
        let mut c = Converter { category: 0, from: 0, to: 0, entry: Entry::new(false) };
        c.set_category(0);
        c
    }

    pub fn category(&self) -> &'static Category {
        &CATEGORIES[self.category.min(CATEGORIES.len() - 1)]
    }

    pub fn from_unit(&self) -> &'static Unit {
        let units = self.category().units;
        &units[self.from.min(units.len() - 1)]
    }

    pub fn to_unit(&self) -> &'static Unit {
        let units = self.category().units;
        &units[self.to.min(units.len() - 1)]
    }

    /// Shows a category, with its usual pair of units.
    pub fn set_category(&mut self, category: usize) {
        self.category = category.min(CATEGORIES.len() - 1);
        (self.from, self.to) = self.category().default;
    }

    /// Converts from a unit (an index in the category).
    pub fn set_from(&mut self, unit: usize) {
        self.from = unit.min(self.category().units.len() - 1);
    }

    /// Converts to a unit (an index in the category).
    pub fn set_to(&mut self, unit: usize) {
        self.to = unit.min(self.category().units.len() - 1);
    }

    /// The text typed.
    pub fn input(&self) -> &str {
        self.entry.text()
    }

    /// Replaces the value typed.
    pub fn set_input(&mut self, text: &str) {
        self.entry.set(text);
    }

    /// The value typed, worked out (nothing typed is zero).
    pub fn value(&self) -> Result<Number, CalcError> {
        if self.entry.is_empty() {
            return Ok(Number::ZERO);
        }
        let tokens = tokenize(self.entry.text(), None)?;
        evaluate(&parse(&tokens, self.entry.text().len())?, &Env::default())
    }

    /// The value converted.
    pub fn result(&self) -> Result<Number, CalcError> {
        convert(self.value()?, self.from_unit(), self.to_unit())
    }

    /// Swaps the units, the converted value becoming the one typed.
    pub fn swap(&mut self) {
        if let Ok(r) = self.result() {
            let text = exact_text(r);
            self.entry.set(text.trim_start_matches('(').trim_end_matches(')'));
        }
        core::mem::swap(&mut self.from, &mut self.to);
    }
}

/// What the display shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Screen {
    /// The line above: the expression `=` worked out ("12 × 7 =").
    pub above: String,
    /// The main line: the expression being typed, or the result.
    pub main: String,
    /// Where the caret is in `main` (a byte offset), when it is not at the end.
    pub caret: Option<usize>,
    /// How many closing parentheses the expression leaves open, to show faintly after it.
    pub ghost: usize,
    /// The main line is a result.
    pub is_result: bool,
    /// The line below: the result so far, while typing.
    pub preview: Option<String>,
    /// What `=` found wrong.
    pub error: Option<String>,
    /// Where in `main` the error is (a byte offset), if known.
    pub error_at: Option<usize>,
}

/// The calculator.
#[derive(Debug, Clone)]
pub struct Calculator {
    mode: Mode,
    pub angle: AngleUnit,
    /// How results are written: grouping, and scientific notation always (F-E).
    pub style: Style,
    /// The second functions of the scientific keys (`sin⁻¹`, `x³`, ...).
    pub second: bool,
    /// The hyperbolic functions on the scientific keys.
    pub hyperbolic: bool,
    decimal: Session,
    programmer: Session,
    base: Base,
    word: WordSize,
    pub converter: Converter,
    /// The last result, of Standard and Scientific modes.
    ans: Number,
    /// The last result of Programmer mode.
    ans_word: i64,
    memory: Option<Number>,
    history: Vec<HistoryItem>,
}

impl Default for Calculator {
    fn default() -> Calculator {
        Calculator::new()
    }
}

impl Calculator {
    pub fn new() -> Calculator {
        Calculator {
            mode: Mode::Standard,
            angle: AngleUnit::Degrees,
            style: Style::default(),
            second: false,
            hyperbolic: false,
            decimal: Session::new(false),
            programmer: Session::new(true),
            base: Base::Dec,
            word: WordSize::Qword,
            converter: Converter::new(),
            ans: Number::ZERO,
            ans_word: 0,
            memory: None,
            history: Vec::new(),
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    pub fn base(&self) -> Base {
        self.base
    }

    pub fn word_size(&self) -> WordSize {
        self.word
    }

    pub fn memory(&self) -> Option<Number> {
        self.memory
    }

    /// The calculations done, the oldest first.
    pub fn history(&self) -> &[HistoryItem] {
        &self.history
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// The last result (Standard and Scientific modes').
    pub fn ans(&self) -> Number {
        self.ans
    }

    fn session(&mut self) -> &mut Session {
        if self.mode == Mode::Programmer { &mut self.programmer } else { &mut self.decimal }
    }

    fn session_ref(&self) -> &Session {
        if self.mode == Mode::Programmer { &self.programmer } else { &self.decimal }
    }

    /// The expression's text (with the previous result as [`ANS_MARK`]).
    pub fn expression(&self) -> &str {
        match self.mode {
            Mode::Converter => self.converter.entry.text(),
            _ => self.session_ref().entry.text(),
        }
    }

    /// The expression as shown: the previous result written out.
    pub fn shown_expression(&self) -> String {
        self.session_ref().entry.shown(&self.ans_text()).0
    }

    /// Replaces the expression (the caret at its end), its ASCII operators written as the
    /// keys write them (`*` as `×`).
    pub fn set_expression(&mut self, text: &str) {
        let text = signs(text);
        match self.mode {
            Mode::Converter => self.converter.entry.set(&text),
            _ => {
                let s = self.session();
                s.entry.set(&text);
                s.state = State::Editing;
            }
        }
    }

    /// Puts the caret at byte `at` of the expression as shown.
    pub fn set_caret(&mut self, at: usize) {
        let ans = self.ans_text();
        let s = self.session();
        // Byte `at` of the shown text, in the text itself.
        let text = s.entry.text().to_string();
        let mut shown = 0;
        let mut target = text.len();
        for (i, c) in text.char_indices() {
            if shown >= at {
                target = i;
                break;
            }
            shown += if c == ANS_MARK { ans.len() } else { c.len_utf8() };
        }
        s.entry.set_cursor(target);
        if matches!(s.state, State::Result { .. } | State::Error { .. }) {
            s.state = State::Editing;
        }
    }

    pub fn set_base(&mut self, base: Base) {
        if base == self.base {
            return;
        }
        // The numbers typed are written again in the new base.
        let (from, to) = (self.base.radix(), base.radix());
        let text = self.programmer.entry.text().to_string();
        let rewritten = match tokenize(&text, Some(from)) {
            Ok(tokens) => {
                let mut out = String::new();
                let mut last = 0;
                for t in &tokens {
                    if let crate::lexer::Tok::Num(n) = t.tok {
                        out.push_str(&text[last..t.start]);
                        let v = n.as_integer().unwrap_or(0) as u64;
                        out.push_str(&match to {
                            16 => format!("{v:X}"),
                            8 => format!("{v:o}"),
                            2 => format!("{v:b}"),
                            _ => format!("{v}"),
                        });
                        last = t.end;
                    }
                }
                out.push_str(&text[last..]);
                out
            }
            Err(_) => String::new(),
        };
        self.programmer.entry.set(&rewritten);
        self.base = base;
    }

    pub fn set_word_size(&mut self, size: WordSize) {
        self.word = size;
        self.ans_word = size.wrap(self.ans_word as i128);
    }

    /// The text the previous result shows as.
    fn ans_text(&self) -> String {
        format(self.ans, &Style { grouping: false, ..self.style })
    }

    /// Presses a key.
    pub fn press(&mut self, key: Key) {
        match self.mode {
            Mode::Converter => self.press_converter(key),
            _ => self.press_expression(key),
        }
    }

    /// Types `text`: digits and operators as their keys, names and anything else as text.
    pub fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let programmer = self.mode == Mode::Programmer;
            // A letter that is a digit of the base is that digit; others are text (`AND`).
            let digit = c.to_digit(if programmer { self.base.radix() } else { 10 });
            let key = match c {
                '0'..='9' => Some(Key::Digit(c as u8 - b'0')),
                'a'..='f' | 'A'..='F' if programmer && digit.is_some() => Some(Key::Digit(digit.unwrap_or(0) as u8)),
                '.' if !programmer => Some(Key::Point),
                '.' => continue,
                '+' => Some(Key::Add),
                '-' | '\u{2212}' => Some(Key::Subtract),
                '*' | '×' => Some(Key::Multiply),
                '/' | '÷' => Some(Key::Divide),
                '^' => Some(Key::Power),
                '%' if !programmer => Some(Key::Percent),
                '!' if !programmer => Some(Key::Factorial),
                '(' => Some(Key::Open),
                ')' => Some(Key::Close),
                '=' => Some(Key::Equals),
                _ => None,
            };
            match key {
                Some(k) => self.press(k),
                None if c.is_control() => {}
                None => {
                    let mut buf = [0u8; 4];
                    self.insert_raw(c.encode_utf8(&mut buf));
                }
            }
        }
    }

    /// Pastes text: thousands separators in a number are dropped, the rest typed.
    pub fn paste(&mut self, text: &str) {
        let text: String = text.trim().chars().take(1000).collect();
        let is_grouped_number = !text.is_empty()
            && text.chars().all(|c| c.is_ascii_digit() || matches!(c, ',' | '.' | '-' | '\u{2212}'))
            && text.contains(',');
        let text = if is_grouped_number { text.replace(',', "") } else { text };
        self.type_text(&text);
    }

    /// Inserts text that is not a key's, as typing it would.
    fn insert_raw(&mut self, s: &str) {
        if self.mode == Mode::Converter {
            self.converter.entry.insert(s);
            return;
        }
        // Letters and digits begin something new after a result; signs carry it on.
        let begins = s.chars().next().is_some_and(|c| c.is_alphanumeric() || c == 'π' || c == '√');
        self.begin(!begins);
        self.session().entry.insert(s);
    }

    /// Readies the expression for input: after a result or an error, input that `continues`
    /// carries on from what is there; other input starts afresh after a result.
    fn begin(&mut self, continues: bool) {
        let s = self.session();
        match s.state {
            State::Result { .. } => {
                if !continues {
                    s.entry.clear();
                }
                s.state = State::Editing;
            }
            State::Error { .. } => s.state = State::Editing,
            State::Editing => {}
        }
    }

    fn insert_operator(&mut self, op: &str) {
        self.begin(true);
        let s = self.session();
        let e = &mut s.entry;
        let minus = op == "\u{2212}";
        if e.is_empty() {
            // A minus begins a negative number; other operators work on zero.
            e.insert(if minus { "" } else { "0" });
            e.insert(op);
            return;
        }
        if let Some(range) = e.operators_before() {
            let before = e.text()[range.clone()].trim();
            // `5 ×` and `−`: the minus is the next number's sign. Otherwise the new operator
            // replaces what is there (`5 +` and `×` makes `5 ×`; `5 × −` and `+` makes `5 +`).
            let takes_sign = !before.ends_with(['\u{2212}', '-', '+']);
            if minus && takes_sign {
                e.insert(op);
            } else if !(minus && before.ends_with('\u{2212}')) {
                e.replace(range, op);
            }
            return;
        }
        if e.segments().iter().rev().find(|s| s.range.end <= e.cursor()).is_some_and(|s| s.kind == Kind::Open) && !minus
        {
            return;
        }
        e.insert(op);
    }

    fn press_expression(&mut self, key: Key) {
        let programmer = self.mode == Mode::Programmer;
        match key {
            Key::Digit(d) => {
                let radix = if programmer { self.base.radix() } else { 10 };
                if (d as u32) < radix {
                    self.begin(false);
                    let c = char::from_digit(d as u32, radix).map_or('0', |c| c.to_ascii_uppercase());
                    let mut buf = [0u8; 4];
                    self.session().entry.insert(c.encode_utf8(&mut buf));
                }
            }
            Key::Point if !programmer => {
                self.begin(false);
                let e = &mut self.session().entry;
                match e.number_at_cursor() {
                    Some(n) if n.contains(['.', 'E', 'e']) => {}
                    Some(_) => e.insert("."),
                    None => e.insert("0."),
                }
            }
            Key::Exponent if !programmer => {
                self.begin(true);
                let e = &mut self.session().entry;
                if e.number_at_cursor().is_some_and(|n| !n.contains(['E', 'e'])) && e.at_end() {
                    e.insert("E");
                }
            }
            Key::Add => self.insert_operator("+"),
            Key::Subtract => self.insert_operator("\u{2212}"),
            Key::Multiply => self.insert_operator("×"),
            Key::Divide => self.insert_operator("÷"),
            Key::Power => self.insert_operator("^"),
            Key::Modulo => self.insert_operator(" mod "),
            Key::And if programmer => self.insert_operator(" AND "),
            Key::Or if programmer => self.insert_operator(" OR "),
            Key::Xor if programmer => self.insert_operator(" XOR "),
            Key::Nand if programmer => self.insert_operator(" NAND "),
            Key::Nor if programmer => self.insert_operator(" NOR "),
            Key::ShiftLeft if programmer => self.insert_operator(" << "),
            Key::ShiftRight if programmer => self.insert_operator(" >> "),
            Key::RotateLeft if programmer => self.insert_operator(" ROL "),
            Key::RotateRight if programmer => self.insert_operator(" ROR "),
            Key::Not if programmer => {
                self.begin(true);
                self.session().entry.apply("NOT ");
            }
            Key::Open => {
                self.begin(false);
                self.session().entry.insert("(");
            }
            Key::Close => {
                self.begin(true);
                let e = &mut self.session().entry;
                if e.open_parens_before() > 0 && e.operand_before().is_some() {
                    e.insert(")");
                }
            }
            Key::Negate => {
                self.begin(true);
                self.session().entry.toggle_sign();
            }
            Key::Percent if !programmer => self.postfix("%"),
            Key::Square => self.postfix("²"),
            Key::Cube => self.postfix("³"),
            Key::Reciprocal if !programmer => self.postfix("\u{207B}\u{00B9}"),
            Key::Factorial if !programmer => self.postfix("!"),
            Key::SquareRoot if !programmer => self.apply("√"),
            Key::CubeRoot if !programmer => self.apply("³√"),
            Key::PowerOfTen if !programmer => self.apply("10^("),
            Key::Exp if !programmer => self.apply("e^("),
            Key::Function(f) if !programmer => {
                let name = format!("{}(", f.name());
                self.apply(&name);
            }
            Key::Root if !programmer => {
                self.begin(true);
                let e = &mut self.session().entry;
                match e.operand_before().filter(|op| op.end == e.cursor()) {
                    Some(op) => {
                        e.insert(", ");
                        let at = e.cursor();
                        e.set_cursor(op.start);
                        e.insert("root(");
                        e.set_cursor(at + "root(".len());
                    }
                    None => e.insert("root("),
                }
            }
            Key::Pi if !programmer => {
                self.begin(false);
                self.session().entry.insert("π");
            }
            Key::Euler if !programmer => {
                self.begin(false);
                self.session().entry.insert("e");
            }
            Key::Ans => {
                self.begin(false);
                self.session().entry.insert("Ans");
            }
            Key::Equals => self.equals(),
            Key::Clear => {
                let s = self.session();
                s.entry.clear();
                s.state = State::Editing;
                s.repeat = None;
            }
            Key::ClearEntry => {
                let s = self.session();
                if s.state != State::Editing {
                    s.entry.clear();
                    s.state = State::Editing;
                } else if let Some(op) = s.entry.operand_before().filter(|op| op.end == s.entry.cursor()) {
                    s.entry.replace(op, "");
                }
            }
            Key::Backspace => {
                self.begin(true);
                self.session().entry.backspace();
            }
            Key::Delete => {
                self.begin(true);
                self.session().entry.delete();
            }
            Key::Left => {
                self.begin(true);
                self.session().entry.left();
            }
            Key::Right => {
                self.begin(true);
                self.session().entry.right();
            }
            Key::Home => {
                self.begin(true);
                self.session().entry.home();
            }
            Key::End => {
                self.begin(true);
                self.session().entry.end();
            }
            Key::MemoryClear => self.memory = None,
            Key::MemoryStore => {
                if let Some(v) = self.current_value() {
                    self.memory = Some(v);
                }
            }
            Key::MemoryAdd | Key::MemorySubtract => {
                if let Some(v) = self.current_value() {
                    let m = self.memory.unwrap_or(Number::ZERO);
                    let sum = if key == Key::MemoryAdd { m.add(v) } else { m.sub(v) };
                    if let Ok(sum) = sum {
                        self.memory = Some(if programmer { self.word_number(sum) } else { sum });
                    }
                }
            }
            Key::MemoryRecall => {
                if let Some(m) = self.memory {
                    self.recall(m);
                }
            }
            // Keys of another mode.
            _ => {}
        }
    }

    /// A postfix sign (`²`, `!`) after the operand before the caret; nothing without one.
    fn postfix(&mut self, sign: &str) {
        self.begin(true);
        let e = &mut self.session().entry;
        if e.operand_before().is_some_and(|op| op.end == e.cursor()) {
            e.insert(sign);
        }
    }

    /// A function on the operand before the caret, or its beginning for what follows.
    fn apply(&mut self, name: &str) {
        self.begin(true);
        self.session().entry.apply(name);
    }

    /// The value of Programmer mode's size closest to `n` (its whole part, cut to the size).
    fn word_number(&self, n: Number) -> Number {
        let whole = n.to_f64() as i128;
        Number::int(self.word.wrap(n.as_integer().unwrap_or(whole)) as i128)
    }

    /// Puts a value into the expression: after a result, in its place; otherwise at the caret.
    fn recall(&mut self, value: Number) {
        let text = match self.mode {
            Mode::Programmer => {
                let v = self.word_number(value).as_integer().unwrap_or(0) as i64;
                format_word(v, self.base, self.word, false)
            }
            _ => exact_text(value),
        };
        self.begin(false);
        self.session().entry.insert(&text);
    }

    /// Shows a calculation of the history again: its result, as `=` left it.
    pub fn recall_history(&mut self, index: usize) {
        let Some(item) = self.history.get(index).cloned() else { return };
        match item.programmer {
            Some(_) if self.mode != Mode::Programmer => self.recall(item.result),
            Some(_) => {
                let v = self.word_number(item.result).as_integer().unwrap_or(0) as i64;
                self.ans_word = v;
                let text = format_word(v, self.base, self.word, false);
                let s = &mut self.programmer;
                s.entry.set(&text);
                s.state = State::Result { expression: item.expression };
            }
            None if self.mode == Mode::Programmer => self.recall(item.result),
            None => {
                self.ans = item.result;
                let text = self.result_text(item.result);
                let s = &mut self.decimal;
                s.entry.set(&text);
                s.state = State::Result { expression: item.expression };
                s.repeat = None;
            }
        }
    }

    /// The text a result is carried on as after `=`: its digits where they say it exactly and
    /// briefly (`84`, `0.125`), so that it can be edited; the previous result itself otherwise.
    fn result_text(&self, n: Number) -> String {
        if let Some(text) =
            Some(exact_text(n)).filter(|t| n.rational().is_some() && t.len() <= 24 && !t.contains(['(', 'E']))
        {
            return text;
        }
        String::from(ANS_MARK)
    }

    /// The value the display stands for: the result after `=`, otherwise what the expression
    /// works out to (if it does).
    pub fn current_value(&self) -> Option<Number> {
        match self.mode {
            Mode::Converter => self.converter.result().ok(),
            Mode::Programmer => self.evaluate_word().ok().map(|v| Number::int(v as i128)),
            _ => self.evaluate_decimal().ok(),
        }
    }

    fn parse(&self, text: &str) -> Result<Node, CalcError> {
        let radix = (self.mode == Mode::Programmer).then(|| self.base.radix());
        let tokens = tokenize(text, radix)?;
        parse(&tokens, text.len())
    }

    fn env(&self) -> Env {
        Env { angle: self.angle, ans: self.ans }
    }

    fn evaluate_decimal(&self) -> Result<Number, CalcError> {
        let node = self.parse(self.decimal.entry.text())?;
        evaluate(&node, &self.env())
    }

    fn evaluate_word(&self) -> Result<i64, CalcError> {
        let node = self.parse(self.programmer.entry.text())?;
        evaluate_word(&node, self.word, self.ans_word)
    }

    /// `=`: works the expression out.
    fn equals(&mut self) {
        let shown = self.shown_expression();
        let state = self.session_ref().state.clone();
        if let State::Result { .. } = state {
            self.repeat_last();
            return;
        }
        if self.session_ref().entry.is_empty() {
            return;
        }
        let text = self.session_ref().entry.text().to_string();
        let node = self.parse(&text);
        let outcome = match self.mode {
            Mode::Programmer => node.and_then(|n| {
                evaluate_word(&n, self.word, self.ans_word).map(|v| (Number::int(v as i128), repeatable(&n, None)))
            }),
            _ => node.and_then(|n| {
                let env = self.env();
                evaluate(&n, &env).map(|v| (v, repeatable(&n, Some(&env))))
            }),
        };
        let closing = ")".repeat(
            tokenize(&text, (self.mode == Mode::Programmer).then(|| self.base.radix())).map_or(0, |t| open_parens(&t)),
        );
        match outcome {
            Ok((value, repeat)) => {
                let expression = format!("{shown}{closing}");
                self.finish(expression, value, repeat);
            }
            Err(e) => {
                let at = match e {
                    CalcError::Syntax(_, at) => Some(at),
                    _ => None,
                };
                // Byte `at` of the text, in the text as shown.
                let ans_len = self.ans_text().len();
                let at = at.map(|at| {
                    text[..at.min(text.len())].chars().map(|c| if c == ANS_MARK { ans_len } else { c.len_utf8() }).sum()
                });
                self.session().state = State::Error { message: e.to_string(), at };
            }
        }
    }

    /// Records a result: in the history, as the previous result, and in the expression.
    fn finish(&mut self, expression: String, value: Number, repeat: Option<(BinOp, Number)>) {
        let programmer = self.mode == Mode::Programmer;
        self.history.push(HistoryItem {
            expression: expression.clone(),
            result: value,
            programmer: programmer.then_some((self.base, self.word)),
        });
        if self.history.len() > HISTORY_MAX {
            self.history.remove(0);
        }
        let text = if programmer {
            let v = value.as_integer().unwrap_or(0) as i64;
            self.ans_word = v;
            format_word(v, self.base, self.word, false)
        } else {
            self.ans = value;
            self.result_text(value)
        };
        let s = self.session();
        s.entry.set(&text);
        s.state = State::Result { expression };
        s.repeat = repeat;
    }

    /// `=` again: the last operation once more, on its result.
    fn repeat_last(&mut self) {
        let Some((op, operand)) = self.session_ref().repeat else { return };
        let programmer = self.mode == Mode::Programmer;
        let previous = if programmer { Number::int(self.ans_word as i128) } else { self.ans };
        let result = if programmer {
            let x = self.ans_word;
            let y = operand.as_integer().unwrap_or(0) as i64;
            let node = |n: i64| Node { expr: Expr::Num(Number::int(n as i128)), start: 0, end: 0 };
            let tree = Node {
                expr: Expr::Bin(op, alloc::boxed::Box::new(node(x)), alloc::boxed::Box::new(node(y))),
                start: 0,
                end: 0,
            };
            evaluate_word(&tree, self.word, x).map(|v| Number::int(v as i128))
        } else {
            match op {
                BinOp::Add => previous.add(operand),
                BinOp::Sub => previous.sub(operand),
                BinOp::Mul => previous.mul(operand),
                BinOp::Div => previous.div(operand),
                BinOp::Pow => previous.pow(operand),
                _ => return,
            }
        };
        let show = |n: Number| {
            if programmer {
                format_word(n.as_integer().unwrap_or(0) as i64, self.base, self.word, false)
            } else {
                format(n, &Style { grouping: false, ..self.style })
            }
        };
        let sign = match op {
            BinOp::Add => "+",
            BinOp::Sub => "\u{2212}",
            BinOp::Mul => "×",
            BinOp::Div => "÷",
            BinOp::Pow => "^",
            _ => "?",
        };
        match result {
            Ok(value) => {
                let expression = format!("{}{sign}{}", show(previous), show(operand));
                self.finish(expression, value, Some((op, operand)));
            }
            Err(e) => self.session().state = State::Error { message: e.to_string(), at: None },
        }
    }

    /// What the display shows.
    pub fn screen(&self) -> Screen {
        match self.mode {
            Mode::Converter => self.converter_screen(),
            _ => self.expression_screen(),
        }
    }

    fn expression_screen(&self) -> Screen {
        let s = self.session_ref();
        let programmer = self.mode == Mode::Programmer;
        let ans = self.ans_text();
        let (shown, caret) = s.entry.shown(&ans);
        let mut screen = Screen {
            caret: (caret < shown.len()).then_some(caret),
            ghost: tokenize(s.entry.text(), programmer.then(|| self.base.radix())).map_or(0, |t| open_parens(&t)),
            ..Screen::default()
        };
        match &s.state {
            State::Result { expression } => {
                screen.above = format!("{expression} =");
                screen.is_result = true;
                screen.ghost = 0;
                screen.caret = None;
                screen.main = if programmer {
                    format_word(self.ans_word, self.base, self.word, self.style.grouping)
                } else {
                    format(self.ans, &self.style)
                };
            }
            State::Error { message, at } => {
                screen.main = shown;
                screen.error = Some(message.clone());
                screen.error_at = *at;
            }
            State::Editing => {
                screen.preview = self.preview(&shown);
                screen.main = shown;
            }
        }
        screen
    }

    /// The result so far, while typing: shown when the expression works out to something
    /// other than what it says (a lone number is no news).
    fn preview(&self, shown: &str) -> Option<String> {
        // A lone number is no news; a lone name (π) is.
        let segs = self.session_ref().entry.segments();
        let mut units = segs.iter().filter(|s| s.kind != Kind::Space);
        let worked = match (units.next(), units.next()) {
            (Some(only), None) => only.kind != Kind::Number,
            (first, _) => first.is_some(),
        };
        if !worked {
            return None;
        }
        let text = match self.mode {
            Mode::Programmer => format_word(self.evaluate_word().ok()?, self.base, self.word, self.style.grouping),
            _ => format(self.evaluate_decimal().ok()?, &self.style),
        };
        (text != shown).then_some(text)
    }

    fn converter_screen(&self) -> Screen {
        let c = &self.converter;
        let main = if c.entry.is_empty() { String::from("0") } else { c.entry.text().to_string() };
        let caret = (!c.entry.at_end()).then_some(c.entry.cursor());
        let (preview, error) = match c.result() {
            Ok(r) => (Some(format(r, &self.style)), None),
            Err(e) if c.entry.is_empty() => (None, Some(e.to_string())),
            Err(CalcError::Syntax(..)) => (None, None),
            Err(e) => (None, Some(e.to_string())),
        };
        Screen { main, caret, preview, error, ..Screen::default() }
    }

    fn press_converter(&mut self, key: Key) {
        let e = &mut self.converter.entry;
        match key {
            Key::Digit(d) if d < 10 => {
                let mut buf = [0u8; 4];
                e.insert(char::from(b'0' + d).encode_utf8(&mut buf));
            }
            Key::Point => match e.number_at_cursor() {
                Some(n) if n.contains('.') => {}
                Some(_) => e.insert("."),
                None => e.insert("0."),
            },
            Key::Negate => e.toggle_sign(),
            Key::Add => e.insert("+"),
            Key::Subtract => e.insert("\u{2212}"),
            Key::Multiply => e.insert("×"),
            Key::Divide => e.insert("÷"),
            Key::Open => e.insert("("),
            Key::Close => e.insert(")"),
            Key::Backspace => {
                e.backspace();
            }
            Key::Delete => {
                e.delete();
            }
            Key::Clear | Key::ClearEntry => e.clear(),
            Key::Left => e.left(),
            Key::Right => e.right(),
            Key::Home => e.home(),
            Key::End => e.end(),
            Key::Equals => {
                if let Ok(v) = self.converter.value() {
                    let text = exact_text(v);
                    self.converter.entry.set(text.trim_start_matches('(').trim_end_matches(')'));
                }
            }
            _ => {}
        }
    }

    /// Programmer mode's value: the result after `=`, otherwise what the expression works out
    /// to so far (`None` if it does not).
    pub fn word_value(&self) -> Option<i64> {
        match self.programmer.state {
            State::Result { .. } => Some(self.ans_word),
            _ if self.programmer.entry.is_empty() => Some(0),
            _ => self.evaluate_word().ok(),
        }
    }

    /// Flips bit `bit` of Programmer mode's value (the bit keypad): the value becomes the
    /// expression.
    pub fn toggle_bit(&mut self, bit: u32) {
        if bit >= self.word.bits() {
            return;
        }
        let v = self.word_value().unwrap_or(0);
        let flipped = self.word.wrap((v ^ (1i64 << bit)) as i128);
        let text = format_word(flipped, self.base, self.word, false);
        let s = &mut self.programmer;
        s.entry.set(&text);
        s.state = State::Editing;
    }

    /// Restores what [`crate::store`] read.
    pub(crate) fn restore(&mut self, saved: crate::store::Saved) {
        self.mode = saved.mode;
        self.angle = saved.angle;
        self.style.grouping = saved.grouping;
        self.style.scientific = saved.scientific;
        self.base = saved.base;
        self.word = saved.word;
        self.converter.set_category(saved.category);
        if let Some((from, to)) = saved.units {
            self.converter.set_from(from);
            self.converter.set_to(to);
        }
        self.memory = saved.memory;
        self.history = saved.history;
        if let Some(last) = self.history.iter().rev().find(|h| h.programmer.is_none()) {
            self.ans = last.result;
        }
    }

    /// What [`crate::store`] keeps.
    pub(crate) fn saved(&self) -> crate::store::Saved {
        crate::store::Saved {
            mode: self.mode,
            angle: self.angle,
            grouping: self.style.grouping,
            scientific: self.style.scientific,
            base: self.base,
            word: self.word,
            category: self.converter.category,
            units: Some((self.converter.from, self.converter.to)),
            memory: self.memory,
            history: self.history.clone(),
        }
    }

    /// Whether the expression's text has a syntax error at its end only: it is unfinished
    /// rather than wrong (the display does not mark it).
    pub fn is_unfinished(&self) -> bool {
        let text = self.session_ref().entry.text();
        matches!(self.parse(text), Err(CalcError::Syntax(Syntax::MissingOperand | Syntax::Empty, at)) if at >= text.len())
    }
}

/// `text` with ASCII operators and names written as the keys write them (each reads the same):
/// `*` as `×`, `/` as `÷`, `-` as `−`, `pi` as `π`, `sqrt` as `√`.
pub fn signs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c.is_ascii_alphabetic() {
            let len = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
            let word = &rest[..len];
            out.push_str(match word.to_ascii_lowercase().as_str() {
                "pi" => "π",
                "sqrt" => "√",
                _ => word,
            });
            rest = &rest[len..];
            continue;
        }
        out.push(match c {
            '*' => '×',
            '/' => '÷',
            '-' => '\u{2212}',
            c => c,
        });
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// The operation `=` again repeats, for an expression that is one: its operator and its right
/// operand worked out (`2 + 3` repeats `+ 3`).
fn repeatable(n: &Node, env: Option<&Env>) -> Option<(BinOp, Number)> {
    let Expr::Bin(op, _, rhs) = &n.expr else { return None };
    if !matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow) {
        return None;
    }
    let value = match env {
        Some(env) => evaluate(rhs, env).ok()?,
        None => Number::int(evaluate_word(rhs, WordSize::Qword, 0).ok()? as i128),
    };
    Some((*op, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Presses keys written as text: digits, operators and `=` as typed, names in braces
    /// (`{sqrt}`).
    fn keys(c: &mut Calculator, script: &str) {
        let mut rest = script;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('{') {
                let end = after.find('}').unwrap();
                let key = match &after[..end] {
                    "sqrt" => Key::SquareRoot,
                    "cbrt" => Key::CubeRoot,
                    "sq" => Key::Square,
                    "inv" => Key::Reciprocal,
                    "neg" => Key::Negate,
                    "sin" => Key::Function(Func::Sin),
                    "back" => Key::Backspace,
                    "ce" => Key::ClearEntry,
                    "c" => Key::Clear,
                    "pi" => Key::Pi,
                    "ms" => Key::MemoryStore,
                    "mr" => Key::MemoryRecall,
                    "m+" => Key::MemoryAdd,
                    "root" => Key::Root,
                    "left" => Key::Left,
                    "exp" => Key::Exponent,
                    "and" => Key::And,
                    "not" => Key::Not,
                    "shl" => Key::ShiftLeft,
                    other => panic!("no key {other}"),
                };
                c.press(key);
                rest = &after[end + 1..];
            } else {
                let ch = rest.chars().next().unwrap();
                c.type_text(&rest[..ch.len_utf8()]);
                rest = &rest[ch.len_utf8()..];
            }
        }
    }

    fn run(script: &str) -> Screen {
        let mut c = Calculator::new();
        keys(&mut c, script);
        c.screen()
    }

    #[test]
    fn typing_and_working_out() {
        let s = run("2+3*4");
        assert_eq!((s.main.as_str(), s.preview.as_deref()), ("2+3×4", Some("14")));
        let s = run("2+3*4=");
        assert_eq!((s.above.as_str(), s.main.as_str(), s.is_result), ("2+3×4 =", "14", true));
        assert_eq!(run("0.1+0.2=").main, "0.3");
        assert_eq!(run("1/3*3=").main, "1");
        assert_eq!(run("(1+2").ghost, 1);
        assert_eq!(run("(1+2=").above, "(1+2) =");
        assert_eq!(run("12").preview, None, "a lone number is no news");
        assert_eq!(run("1/0=").error.as_deref(), Some("Can't divide by zero"));
        assert_eq!(run("2+=").error.as_deref(), Some("Missing a number"));
    }

    #[test]
    fn after_a_result() {
        assert_eq!(run("2+3=*2=").main, "10", "an operator carries the result on");
        assert_eq!(run("2+3=7").main, "7", "a number starts afresh");
        assert_eq!(run("2+3===").main, "11", "= repeats the last operation");
        assert_eq!(run("10/4==").main, "0.625");
        assert_eq!(run("1/3=*3=").main, "1", "the result carried on exactly");
        let s = run("1/3=+");
        assert_eq!(s.main, "0.3333333333333333+");
        assert_eq!(run("84={back}").main, "8", "a result is edited as digits");
        assert_eq!(run("9={sqrt}=").main, "3");
        assert_eq!(run("2+3={neg}").main, "\u{2212}5");
    }

    #[test]
    fn keys_that_apply_to_the_number() {
        assert_eq!(run("9{sqrt}").main, "√9");
        assert_eq!(run("9{sqrt}=").main, "3");
        assert_eq!(run("30{sin}=").main, "0.5");
        assert_eq!(run("{sin}30=").main, "0.5");
        assert_eq!(run("5{sq}=").main, "25");
        assert_eq!(run("4{inv}=").main, "0.25");
        assert_eq!(run("{sq}").main, "", "nothing to square");
        assert_eq!(run("8{root}3=").main, "2");
        assert_eq!(run("27{cbrt}=").main, "3");
        assert_eq!(run("5{neg}").main, "\u{2212}5");
        assert_eq!(run("5-3{neg}").main, "5\u{2212}(\u{2212}3)");
        assert_eq!(run("2{pi}=").main, "6.28318530717959", "π's multiples are shown as floats are");
        assert_eq!(run("1.5{exp}3=").main, "1,500");
    }

    #[test]
    fn operators_replace_one_another() {
        assert_eq!(run("5+*").main, "5×");
        assert_eq!(run("5*-").main, "5×\u{2212}");
        assert_eq!(run("5*-+").main, "5+");
        assert_eq!(run("+").main, "0+");
        assert_eq!(run("-5").main, "\u{2212}5");
        assert_eq!(run("(*").main, "(");
        assert_eq!(run("50+10%=").main, "55");
    }

    #[test]
    fn points_and_clearing() {
        assert_eq!(run("1.2.3").main, "1.23");
        assert_eq!(run(".5").main, "0.5");
        assert_eq!(run("2+.").main, "2+0.");
        assert_eq!(run("12+34{ce}").main, "12+");
        assert_eq!(run("12+34{c}").main, "");
        assert_eq!(run("12+34{back}{back}{back}").main, "12");
        assert_eq!(run("1)").main, "1", "nothing to close");
        let s = run("12+34{left}{left}5");
        assert_eq!((s.main.as_str(), s.caret), ("12+534", Some(4)));
    }

    #[test]
    fn memory() {
        let mut c = Calculator::new();
        keys(&mut c, "2+3={ms}{c}10*{mr}=");
        assert_eq!(c.screen().main, "50");
        keys(&mut c, "{m+}");
        assert_eq!(c.memory(), Some(Number::int(55)));
        keys(&mut c, "{c}1/3={ms}{c}{mr}*3=");
        assert_eq!(c.screen().main, "1", "memory keeps fractions exactly");
    }

    #[test]
    fn history_recalls_results() {
        let mut c = Calculator::new();
        keys(&mut c, "2+3=4*5=");
        assert_eq!(c.history().len(), 2);
        assert_eq!(c.history()[0].expression, "2+3");
        c.recall_history(0);
        assert_eq!(c.screen().main, "5");
        keys(&mut c, "+1=");
        assert_eq!(c.screen().main, "6");
        for _ in 0..HISTORY_MAX + 5 {
            keys(&mut c, "1+1=");
        }
        assert_eq!(c.history().len(), HISTORY_MAX);
    }

    #[test]
    fn scientific_settings() {
        let mut c = Calculator::new();
        c.set_mode(Mode::Scientific);
        c.angle = AngleUnit::Radians;
        keys(&mut c, "{pi}{sin}=");
        assert_eq!(c.screen().main, "0");
        c.style.scientific = true;
        keys(&mut c, "{c}1234=");
        assert_eq!(c.screen().main, "1.234E+3");
    }

    #[test]
    fn programmer_mode() {
        let mut c = Calculator::new();
        c.set_mode(Mode::Programmer);
        c.set_base(Base::Hex);
        keys(&mut c, "FF{and}0F=");
        assert_eq!(c.screen().main, "F");
        assert_eq!(c.word_value(), Some(15));
        keys(&mut c, "{c}1{shl}4=");
        assert_eq!(c.screen().main, "10");
        c.set_base(Base::Dec);
        assert_eq!(c.expression(), "16");
        keys(&mut c, "{c}{not}0=");
        assert_eq!(c.screen().main, "\u{2212}1");
        c.set_base(Base::Hex);
        assert_eq!(c.screen().main, "FFFF FFFF FFFF FFFF");
        c.set_word_size(WordSize::Byte);
        keys(&mut c, "{c}7F+1=");
        assert_eq!(c.screen().main, "80");
        keys(&mut c, "{c}9");
        keys(&mut c, "G");
        assert_eq!(c.expression(), "9G", "not a digit: typed as text, an error at =");
        c.set_base(Base::Dec);
        keys(&mut c, "{c}12 and 10=");
        assert_eq!(c.screen().main, "8", "letters that are not digits of the base are words");
        c.set_base(Base::Hex);
        keys(&mut c, "{c}");
        c.set_base(Base::Bin);
        keys(&mut c, "1012");
        assert_eq!(c.expression(), "101", "digits beyond the base are refused");
        c.toggle_bit(1);
        assert_eq!(c.word_value(), Some(0b111));
        assert_eq!(format_word(-1, Base::Bin, WordSize::Byte, true), "1111 1111");
        assert_eq!(format_word(1_234_567, Base::Dec, WordSize::Qword, true), "1,234,567");
        assert_eq!(format_word(0x1F, Base::Oct, WordSize::Qword, false), "37");
    }

    #[test]
    fn the_converter() {
        let mut c = Calculator::new();
        c.set_mode(Mode::Converter);
        c.converter.set_category(4); // Temperature: °C to °F.
        keys(&mut c, "{c}100");
        assert_eq!(c.screen().preview.as_deref(), Some("212"));
        c.converter.swap();
        assert_eq!(c.converter.input(), "212");
        assert_eq!(c.screen().preview.as_deref(), Some("100"));
        keys(&mut c, "{c}");
        assert_eq!(c.screen().main, "0");
        assert_eq!(c.screen().preview.as_deref(), Some("\u{2212}17.77777777777778"));
    }

    #[test]
    fn pasting() {
        let mut c = Calculator::new();
        c.paste("1,234.5 * 2");
        assert_eq!(c.expression(), "1,234.5 × 2", "commas between operators are left alone");
        c.press(Key::Clear);
        c.paste("1,234.5");
        assert_eq!(c.expression(), "1234.5");
        c.press(Key::Clear);
        c.paste("sqrt(16) + pi");
        c.press(Key::Equals);
        assert_eq!(c.screen().main, "7.14159265358979");
    }

    #[test]
    fn expressions_set_whole_are_written_with_the_keys_signs() {
        let mut c = Calculator::new();
        c.set_expression("12*(3+4)/2-1e-3");
        assert_eq!(c.expression(), "12×(3+4)÷2\u{2212}1e\u{2212}3");
        c.press(Key::Equals);
        assert_eq!(c.screen().main, "41.999");
        c.set_expression("2*PI + sqrt(16) + spin");
        assert_eq!(c.expression(), "2×π + √(16) + spin", "only whole names");
    }

    #[test]
    fn errors_point_at_their_place() {
        let s = run("2+(3*)=");
        assert_eq!(s.error.as_deref(), Some("Missing a number"));
        assert_eq!(s.error_at, Some("2+(3×".len()));
        let mut c = Calculator::new();
        keys(&mut c, "2+");
        assert!(c.is_unfinished());
    }
}

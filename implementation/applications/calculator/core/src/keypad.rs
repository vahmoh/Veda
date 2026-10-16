// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The keypads: which keys each mode has, and where.
//!
//! Each mode's keys fill a grid (a key may span rows), which [`place`] lays out in whatever
//! space the window gives, so that the keypad grows with it. Standard mode's is the familiar
//! one; Scientific mode's adds the functions (`2nd` shows their inverses, `hyp` the hyperbolic
//! ones); Programmer mode's has the hexadecimal digits and the bitwise operators (`2nd` shows
//! `NAND`, `NOR` and the rotations); the converter's has only what a number needs.

use alloc::vec::Vec;

use crate::calculator::{Base, Key, Mode};
use crate::lexer::Func;

/// What a key looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// A digit: the lightest keys.
    Digit,
    /// A function: darker.
    Function,
    /// An operator: tinted with the accent colour.
    Operator,
    /// `=`: the accent colour itself.
    Equals,
}

/// What a key shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    Text(&'static str),
    /// The backspace glyph.
    Backspace,
    /// Two arrows, one each way.
    Swap,
}

/// What pressing a key does: a calculator key, or one of the keypad's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Key(Key),
    /// Shows the second functions (`2nd`).
    Second,
    /// Shows the hyperbolic functions (`hyp`).
    Hyperbolic,
    /// Swaps the converter's units.
    Swap,
}

/// A key of a keypad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pad {
    pub label: Label,
    pub action: Action,
    pub look: Look,
    /// What it does, and its key on the keyboard, shown when the pointer rests on it.
    pub tip: &'static str,
    /// Its column and row, and how many rows it spans.
    pub col: u8,
    pub row: u8,
    pub rows: u8,
    /// It can be pressed (a digit beyond the base cannot).
    pub enabled: bool,
    /// It is a toggle that is on (`2nd`).
    pub on: bool,
}

/// A keypad: its grid's size and its keys.
pub struct Layout {
    pub cols: u8,
    pub rows: u8,
    pub pads: Vec<Pad>,
}

/// What the keypads depend on besides the mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub second: bool,
    pub hyperbolic: bool,
    pub base: Base,
}

struct Builder {
    pads: Vec<Pad>,
}

impl Builder {
    fn put(&mut self, col: u8, row: u8, label: Label, action: Action, look: Look, tip: &'static str) -> &mut Pad {
        self.pads.push(Pad { label, action, look, tip, col, row, rows: 1, enabled: true, on: false });
        self.pads.last_mut().unwrap_or_else(|| unreachable!("just pushed"))
    }

    fn text(&mut self, col: u8, row: u8, label: &'static str, key: Key, look: Look, tip: &'static str) -> &mut Pad {
        self.put(col, row, Label::Text(label), Action::Key(key), look, tip)
    }

    fn digit(&mut self, col: u8, row: u8, d: u8) -> &mut Pad {
        const DIGITS: [&str; 16] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "A", "B", "C", "D", "E", "F"];
        self.text(col, row, DIGITS[d as usize], Key::Digit(d), Look::Digit, "")
    }

    /// The digits 1 to 9 in a block whose top left is (`col`, `row`), and the rest of a
    /// number's keys below: ±, 0 and the point.
    fn number_block(&mut self, col: u8, row: u8) {
        for (i, d) in [7, 8, 9, 4, 5, 6, 1, 2, 3].into_iter().enumerate() {
            self.digit(col + (i % 3) as u8, row + (i / 3) as u8, d);
        }
        self.text(col, row + 3, "±", Key::Negate, Look::Digit, "Change the sign (F9)");
        self.digit(col + 1, row + 3, 0);
        self.text(col + 2, row + 3, ".", Key::Point, Look::Digit, "Decimal point (.)");
    }

    fn operators(&mut self, col: u8, row: u8) {
        self.text(col, row, "÷", Key::Divide, Look::Operator, "Divide (/)");
        self.text(col, row + 1, "×", Key::Multiply, Look::Operator, "Multiply (*)");
        self.text(col, row + 2, "−", Key::Subtract, Look::Operator, "Subtract (-)");
        self.text(col, row + 3, "+", Key::Add, Look::Operator, "Add (+)");
        self.text(col, row + 4, "=", Key::Equals, Look::Equals, "Equals (Enter)");
    }

    fn backspace(&mut self, col: u8, row: u8) {
        self.put(col, row, Label::Backspace, Action::Key(Key::Backspace), Look::Function, "Delete (Backspace)");
    }
}

/// The keypad of `mode`.
pub fn layout(mode: Mode, o: Options) -> Layout {
    let mut b = Builder { pads: Vec::new() };
    let (cols, rows) = match mode {
        Mode::Standard => {
            b.text(0, 0, "%", Key::Percent, Look::Function, "Percent (%)");
            b.text(1, 0, "CE", Key::ClearEntry, Look::Function, "Clear the number (Delete)");
            b.text(2, 0, "C", Key::Clear, Look::Function, "Clear (Esc)");
            b.backspace(3, 0);
            b.text(0, 1, "⅟x", Key::Reciprocal, Look::Function, "Reciprocal");
            b.text(1, 1, "x²", Key::Square, Look::Function, "Square");
            b.text(2, 1, "√x", Key::SquareRoot, Look::Function, "Square root (@)");
            b.number_block(0, 2);
            b.operators(3, 1);
            (4, 6)
        }
        Mode::Scientific => {
            let (s, h) = (o.second, o.hyperbolic);
            let trig = |plain: Func, inverse: Func, hyp: Func, hyp_inverse: Func| match (s, h) {
                (false, false) => plain,
                (true, false) => inverse,
                (false, true) => hyp,
                (true, true) => hyp_inverse,
            };
            let second = b.put(0, 0, Label::Text("2nd"), Action::Second, Look::Function, "Second functions");
            second.on = s;
            let hyp = b.put(1, 0, Label::Text("hyp"), Action::Hyperbolic, Look::Function, "Hyperbolic functions");
            hyp.on = h;
            let funcs = [
                trig(Func::Sin, Func::Asin, Func::Sinh, Func::Asinh),
                trig(Func::Cos, Func::Acos, Func::Cosh, Func::Acosh),
                trig(Func::Tan, Func::Atan, Func::Tanh, Func::Atanh),
            ];
            for (i, f) in funcs.into_iter().enumerate() {
                b.text(2 + i as u8, 0, f.name(), Key::Function(f), Look::Function, "");
            }
            if s {
                b.text(0, 1, "x³", Key::Cube, Look::Function, "Cube");
                b.text(1, 1, "ʸ√x", Key::Root, Look::Function, "Root of any degree");
                b.text(2, 1, "³√x", Key::CubeRoot, Look::Function, "Cube root");
                b.text(3, 1, "eˣ", Key::Exp, Look::Function, "e to the power");
                b.text(4, 1, "10ˣ", Key::PowerOfTen, Look::Function, "10 to the power");
            } else {
                b.text(0, 1, "x²", Key::Square, Look::Function, "Square");
                b.text(1, 1, "xʸ", Key::Power, Look::Function, "Power (^)");
                b.text(2, 1, "√x", Key::SquareRoot, Look::Function, "Square root (@)");
                b.text(3, 1, "ln", Key::Function(Func::Ln), Look::Function, "Natural logarithm");
                b.text(4, 1, "log", Key::Function(Func::Log), Look::Function, "Logarithm to base 10");
            }
            b.text(0, 2, "⅟x", Key::Reciprocal, Look::Function, "Reciprocal");
            b.text(1, 2, "|x|", Key::Function(Func::Abs), Look::Function, "Absolute value");
            b.text(2, 2, "(", Key::Open, Look::Function, "Open parenthesis (()");
            b.text(3, 2, ")", Key::Close, Look::Function, "Close parenthesis ())");
            b.text(4, 2, "n!", Key::Factorial, Look::Function, "Factorial (!)");
            b.text(0, 3, "π", Key::Pi, Look::Function, "Pi");
            b.text(1, 3, "C", Key::Clear, Look::Function, "Clear (Esc)");
            b.backspace(2, 3);
            b.text(3, 3, "%", Key::Percent, Look::Function, "Percent (%)");
            b.text(0, 4, "e", Key::Euler, Look::Function, "Euler's number");
            b.text(0, 5, "EE", Key::Exponent, Look::Function, "Times ten to the power (E)");
            b.text(0, 6, "mod", Key::Modulo, Look::Function, "Remainder");
            b.text(0, 7, "Ans", Key::Ans, Look::Function, "The last result");
            b.number_block(1, 4);
            b.operators(4, 3);
            (5, 8)
        }
        Mode::Programmer => {
            let s = o.second;
            let second = b.put(0, 0, Label::Text("2nd"), Action::Second, Look::Function, "More operators");
            second.on = s;
            let pairs = [("AND", Key::And, "NAND", Key::Nand), ("OR", Key::Or, "NOR", Key::Nor)];
            for (i, (a, ka, n, kn)) in pairs.into_iter().enumerate() {
                let (label, key) = if s { (n, kn) } else { (a, ka) };
                b.text(1 + i as u8, 0, label, key, Look::Function, "");
            }
            b.text(3, 0, "XOR", Key::Xor, Look::Function, "");
            b.text(4, 0, "NOT", Key::Not, Look::Function, "");
            let (shl, shl_key, shl_tip, shr, shr_key, shr_tip) = if s {
                ("ROL", Key::RotateLeft, "Rotate left", "ROR", Key::RotateRight, "Rotate right")
            } else {
                ("<<", Key::ShiftLeft, "Shift left", ">>", Key::ShiftRight, "Shift right, keeping the sign")
            };
            b.text(1, 1, shl, shl_key, Look::Function, shl_tip);
            b.text(2, 1, shr, shr_key, Look::Function, shr_tip);
            b.text(3, 1, "C", Key::Clear, Look::Function, "Clear (Esc)");
            b.backspace(4, 1);
            b.text(1, 2, "(", Key::Open, Look::Function, "Open parenthesis (()");
            b.text(2, 2, ")", Key::Close, Look::Function, "Close parenthesis ())");
            b.text(3, 2, "mod", Key::Modulo, Look::Function, "Remainder");
            for d in 10..16u8 {
                let p = b.digit(0, d - 9, d);
                p.enabled = o.base == Base::Hex;
            }
            for (i, d) in [7, 8, 9, 4, 5, 6, 1, 2, 3].into_iter().enumerate() {
                let p = b.digit(1 + (i % 3) as u8, 3 + (i / 3) as u8, d);
                p.enabled = (d as u32) < o.base.radix();
            }
            b.text(1, 6, "±", Key::Negate, Look::Digit, "Change the sign (F9)");
            b.digit(2, 6, 0);
            b.text(3, 6, "CE", Key::ClearEntry, Look::Function, "Clear the number (Delete)");
            b.text(4, 2, "÷", Key::Divide, Look::Operator, "Divide (/)");
            b.text(4, 3, "×", Key::Multiply, Look::Operator, "Multiply (*)");
            b.text(4, 4, "−", Key::Subtract, Look::Operator, "Subtract (-)");
            b.text(4, 5, "+", Key::Add, Look::Operator, "Add (+)");
            b.text(4, 6, "=", Key::Equals, Look::Equals, "Equals (Enter)");
            (5, 7)
        }
        Mode::Converter => {
            for (i, d) in [7, 8, 9, 4, 5, 6, 1, 2, 3].into_iter().enumerate() {
                b.digit((i % 3) as u8, (i / 3) as u8, d);
            }
            b.text(0, 3, "±", Key::Negate, Look::Digit, "Change the sign (F9)");
            b.digit(1, 3, 0);
            b.text(2, 3, ".", Key::Point, Look::Digit, "Decimal point (.)");
            b.text(3, 0, "CE", Key::ClearEntry, Look::Function, "Clear (Esc)");
            b.backspace(3, 1);
            let swap = b.put(3, 2, Label::Swap, Action::Swap, Look::Operator, "Swap the units");
            swap.rows = 2;
            (4, 4)
        }
    };
    Layout { cols, rows, pads: b.pads }
}

/// A rectangle: left, top, width and height, in pixels.
pub type Cell = (i32, i32, i32, i32);

/// Where each key of `layout` goes in `area`, with `gap` pixels between keys.
pub fn place(layout: &Layout, area: Cell, gap: i32) -> Vec<Cell> {
    let (ax, ay, aw, ah) = area;
    let (cols, rows) = (layout.cols as i32, layout.rows as i32);
    let cell_w = (aw - gap * (cols - 1)) as f32 / cols as f32;
    let cell_h = (ah - gap * (rows - 1)) as f32 / rows as f32;
    // Edges rounded from the grid's fractions, so that keys tile without drifting.
    let x = |c: i32| ax + libm::roundf(c as f32 * (cell_w + gap as f32)) as i32;
    let y = |r: i32| ay + libm::roundf(r as f32 * (cell_h + gap as f32)) as i32;
    layout
        .pads
        .iter()
        .map(|p| {
            let (c, r) = (p.col as i32, p.row as i32);
            let right = x(c + 1) - gap;
            let bottom = y(r + p.rows as i32) - gap;
            (x(c), y(r), right - x(c), bottom - y(r))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypads_tile_their_grids() {
        for mode in Mode::ALL {
            for (second, hyperbolic) in [(false, false), (true, true)] {
                for base in Base::ALL {
                    let l = layout(mode, Options { second, hyperbolic, base });
                    let mut taken = alloc::vec![false; l.cols as usize * l.rows as usize];
                    for p in &l.pads {
                        assert!(p.col < l.cols && p.row + p.rows <= l.rows, "{mode:?}: {:?}", p.label);
                        for r in p.row..p.row + p.rows {
                            let cell = &mut taken[r as usize * l.cols as usize + p.col as usize];
                            assert!(!*cell, "{mode:?}: two keys at {},{}", p.col, r);
                            *cell = true;
                        }
                    }
                    assert!(taken.iter().all(|t| *t), "{mode:?}: a gap in the keypad");
                }
            }
        }
    }

    #[test]
    fn keys_fill_their_area() {
        let l = layout(Mode::Standard, Options { second: false, hyperbolic: false, base: Base::Dec });
        let rects = place(&l, (10, 20, 403, 301), 4);
        let right = rects.iter().map(|r| r.0 + r.2).max().unwrap();
        let bottom = rects.iter().map(|r| r.1 + r.3).max().unwrap();
        assert_eq!((right, bottom), (10 + 403, 20 + 301));
        assert!(rects.iter().all(|r| r.2 >= 90 && r.3 >= 45));
        // Neighbours are a gap apart.
        assert_eq!(rects[1].0 - (rects[0].0 + rects[0].2), 4);
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `calculator-core` — what the Calculator works out and decides, apart from what it draws,
//! so that it can be tested on the host.
//!
//! * [`number`]: numbers exact where they can be (fractions, multiples of π) and floats where
//!   they must be, and their arithmetic and functions.
//! * [`format`]: numbers as people read and write them.
//! * [`lexer`], [`parser`] and [`eval`]: expressions, from their text to their value, as
//!   numbers or as Programmer mode's whole numbers of a word's size.
//! * [`entry`]: the expression being typed, and the edits the keys make.
//! * [`units`]: units and their conversions.
//! * [`keypad`]: which keys each mode has, and where.
//! * [`calculator`]: what each key does in each mode, what the display shows, the history and
//!   the memory.
//! * [`store`]: what the Calculator remembers between runs.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod calculator;
pub mod entry;
pub mod eval;
pub mod format;
pub mod keypad;
pub mod lexer;
pub mod number;
pub mod parser;
pub mod store;
pub mod units;

pub use calculator::{Base, Calculator, Converter, HistoryItem, Key, Mode, Screen, format_word};
pub use eval::WordSize;
pub use format::Style;
pub use lexer::Func;
pub use number::{AngleUnit, CalcError, Number};

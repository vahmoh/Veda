// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The terminal's text buffer.
//!
//! Output is kept as logical lines of styled [`Cell`]s. Lines are
//! soft-wrapped into visual rows when drawn, so resizing the window re-flows
//! the text. [`Screen::write`] understands newlines, tabs and the ANSI
//! "select graphic rendition" escape sequences (`ESC [ ... m`) that set
//! colours and attributes, and the carriage return, backspace and "erase in
//! line" (`ESC [ K`) with which programs redraw the line they are on
//! (progress bars); other escape sequences (window titles, hyperlinks,
//! cursor movement, ...) are recognised and left out, so that none of their
//! bytes show. The `vte` crate (Alacritty's parser of the DEC/ECMA-48
//! state machine) reads the sequences. [`Screen::write_styled`] writes
//! plain text in a given style.
//!
//! What is kept is bounded whatever programs write: at most [`MAX_LINES`]
//! lines and [`MAX_CELLS`] cells (the oldest go first), a line breaking
//! after [`MAX_LINE_CELLS`].

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use vte::{Params, Parser, Perform};

/// Maximum number of lines kept in the scrollback.
pub const MAX_LINES: usize = 10_000;

/// The longest line kept: a program writing on without a line break has
/// its line broken there, so that one line cannot grow without bound.
pub const MAX_LINE_CELLS: usize = 8192;

/// The most cells the scrollback keeps in all (about 12 MiB): with long
/// lines, fewer than [`MAX_LINES`] lines are kept.
pub const MAX_CELLS: usize = 1 << 20;

/// Tab stops are every `TAB_WIDTH` columns.
const TAB_WIDTH: usize = 8;

/// Indices into the 16-colour palette (the classic ANSI order).
pub mod color {
    pub const BLACK: u8 = 0;
    pub const RED: u8 = 1;
    pub const GREEN: u8 = 2;
    pub const YELLOW: u8 = 3;
    pub const BLUE: u8 = 4;
    pub const MAGENTA: u8 = 5;
    pub const CYAN: u8 = 6;
    pub const WHITE: u8 = 7;
    pub const GREY: u8 = 8;
    pub const BRIGHT_RED: u8 = 9;
    pub const BRIGHT_GREEN: u8 = 10;
    pub const BRIGHT_YELLOW: u8 = 11;
    pub const BRIGHT_BLUE: u8 = 12;
    pub const BRIGHT_MAGENTA: u8 = 13;
    pub const BRIGHT_CYAN: u8 = 14;
    pub const BRIGHT_WHITE: u8 = 15;
    /// The terminal's default foreground or background.
    pub const DEFAULT: u8 = 255;
}

/// How a cell is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub fg: u8,
    pub bg: u8,
    pub bold: bool,
    pub dim: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl Style {
    /// Default colours, no attributes.
    pub const PLAIN: Style =
        Style { fg: color::DEFAULT, bg: color::DEFAULT, bold: false, dim: false, underline: false, inverse: false };

    /// Plain style with foreground colour `fg`.
    pub const fn fg(fg: u8) -> Style {
        Style { fg, ..Style::PLAIN }
    }

    /// The same style in bold.
    pub const fn bold(self) -> Style {
        Style { bold: true, ..self }
    }

    /// Bold text in the default colour.
    pub const BOLD: Style = Style::PLAIN.bold();
    /// Faint grey text (secondary information).
    pub const DIM: Style = Style::fg(color::GREY);
    /// Error messages.
    pub const ERROR: Style = Style::fg(color::BRIGHT_RED);
}

/// One character cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
}

/// A position in the buffer: an absolute line number and a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub line: u64,
    pub col: usize,
}

/// The scrollback buffer.
pub struct Screen {
    /// Completed lines.
    lines: VecDeque<Vec<Cell>>,
    /// Absolute number of `lines[0]` (grows as old lines are dropped).
    base: u64,
    /// The cells of `lines`, in all.
    cells: usize,
    /// The line currently being written (not yet terminated).
    partial: Vec<Cell>,
    /// Where the next character goes in `partial`.
    col: usize,
    /// Current style of [`Screen::write`].
    style: Style,
    /// The escape sequence being read.
    parser: Parser,
    /// Incremented on every change (for redraw decisions and caches).
    pub generation: u64,
}

impl Default for Screen {
    fn default() -> Self {
        Screen::new()
    }
}

impl Screen {
    pub fn new() -> Screen {
        Screen {
            lines: VecDeque::new(),
            base: 0,
            cells: 0,
            partial: Vec::new(),
            col: 0,
            style: Style::PLAIN,
            parser: Parser::new(),
            generation: 0,
        }
    }

    /// Absolute number of the first stored line.
    pub fn base(&self) -> u64 {
        self.base
    }

    /// Absolute number one past the last completed line, i.e. the number of
    /// the line that is being written (or the input line).
    pub fn end(&self) -> u64 {
        self.base + self.lines.len() as u64
    }

    /// The completed line with absolute number `abs`.
    pub fn line(&self, abs: u64) -> Option<&[Cell]> {
        let i = abs.checked_sub(self.base)?;
        self.lines.get(i as usize).map(|l| l.as_slice())
    }

    /// Forgets everything (the `clear` command).
    pub fn clear(&mut self) {
        self.base += self.lines.len() as u64;
        self.lines.clear();
        self.cells = 0;
        self.partial.clear();
        self.col = 0;
        self.style = Style::PLAIN;
        self.parser = Parser::new();
        self.generation += 1;
    }

    fn push_line(&mut self, line: Vec<Cell>) {
        self.cells += line.len();
        self.lines.push_back(line);
        while self.lines.len() > MAX_LINES || (self.cells > MAX_CELLS && self.lines.len() > 1) {
            if let Some(old) = self.lines.pop_front() {
                self.cells -= old.len();
            }
            self.base += 1;
        }
    }

    /// Ends the current line.
    pub fn newline(&mut self) {
        self.col = 0;
        let line = core::mem::take(&mut self.partial);
        self.push_line(line);
        self.generation += 1;
    }

    /// Ends the current line if anything was written to it.
    pub fn finish_line(&mut self) {
        if !self.partial.is_empty() {
            self.newline();
        }
        self.style = Style::PLAIN;
        self.parser = Parser::new();
    }

    /// Appends a completed line of cells (e.g. the echoed prompt).
    pub fn push_cells(&mut self, cells: Vec<Cell>) {
        self.finish_line();
        self.push_line(cells);
        self.generation += 1;
    }

    /// The line being written, not yet ended (a prompt waiting for input).
    pub fn partial(&self) -> &[Cell] {
        &self.partial
    }

    /// Writes `c` at the cursor, over what is there; past
    /// [`MAX_LINE_CELLS`], on a new line.
    fn place(&mut self, c: Cell) {
        if self.col >= MAX_LINE_CELLS {
            self.newline();
        }
        if self.col < self.partial.len() {
            self.partial[self.col] = c;
        } else {
            self.partial.resize(self.col, Cell { ch: ' ', style: Style::PLAIN });
            self.partial.push(c);
        }
        self.col += 1;
    }

    fn put(&mut self, c: char, style: Style) {
        match c {
            '\n' => self.newline(),
            '\r' => self.col = 0,
            '\x08' => self.col = self.col.saturating_sub(1),
            '\t' => {
                let n = TAB_WIDTH - self.col % TAB_WIDTH;
                for _ in 0..n {
                    self.place(Cell { ch: ' ', style });
                }
            }
            c if c.is_control() => {}
            c => self.place(Cell { ch: c, style }),
        }
    }

    /// `ESC [ n K`: erases the line after the cursor (0), before it (1) or
    /// all of it (2).
    fn erase_in_line(&mut self, mode: u16) {
        match mode {
            0 => self.partial.truncate(self.col),
            1 => {
                let end = (self.col + 1).min(self.partial.len());
                for c in &mut self.partial[..end] {
                    *c = Cell { ch: ' ', style: Style::PLAIN };
                }
            }
            2 => self.partial.clear(),
            _ => {}
        }
    }

    /// Writes plain text in `style` (escape sequences are not interpreted).
    pub fn write_styled(&mut self, s: &str, style: Style) {
        for c in s.chars() {
            if c != '\x1b' {
                self.put(c, style);
            }
        }
        self.generation += 1;
    }

    /// Writes text, interpreting `ESC [ ... m` colour sequences and
    /// `ESC [ K`; other escape sequences are recognised and left out.
    pub fn write(&mut self, s: &str) {
        // The parser is taken out while it drives the screen.
        let mut parser = core::mem::take(&mut self.parser);
        parser.advance(self, s.as_bytes());
        self.parser = parser;
        self.generation += 1;
    }

    /// The text between two positions (inclusive start, exclusive end),
    /// including `live`, the cells of the input line at [`Screen::end`].
    /// Trailing blanks of each line are dropped; lines are joined with `\n`.
    pub fn text_between(&self, start: Pos, end: Pos, live: &[Cell]) -> String {
        let (start, end) = if start <= end { (start, end) } else { (end, start) };
        let mut out = String::new();
        let first = start.line.max(self.base);
        let mut abs = first;
        while abs <= end.line {
            let cells: &[Cell] = if abs == self.end() { live } else { self.line(abs).unwrap_or(&[]) };
            let c0 = if abs == start.line { start.col.min(cells.len()) } else { 0 };
            let c1 = if abs == end.line { end.col.min(cells.len()) } else { cells.len() };
            let mut s: String = cells[c0..c1.max(c0)].iter().map(|c| c.ch).collect();
            if abs != end.line {
                let trimmed = s.trim_end().len();
                s.truncate(trimmed);
                out.push_str(&s);
                out.push('\n');
            } else {
                out.push_str(&s);
            }
            if abs >= self.end() {
                break;
            }
            abs += 1;
        }
        out
    }
}

/// What the parser recognises, applied to the screen.
impl Perform for Screen {
    fn print(&mut self, c: char) {
        let style = self.style;
        self.put(c, style);
    }

    fn execute(&mut self, byte: u8) {
        if matches!(byte, b'\n' | b'\r' | b'\x08' | b'\t') {
            let style = self.style;
            self.put(byte as char, style);
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        match action {
            'm' => self.style = apply_sgr(self.style, params),
            'K' => self.erase_in_line(params.iter().next().and_then(|p| p.first().copied()).unwrap_or(0)),
            _ => {}
        }
    }
}

/// Applies SGR parameters (e.g. `1;31`) to a style.
fn apply_sgr(mut s: Style, params: &Params) -> Style {
    if params.is_empty() {
        return Style::PLAIN;
    }
    let mut it = params.iter();
    while let Some(p) = it.next() {
        match *p {
            [0] => s = Style::PLAIN,
            [1] => s.bold = true,
            [2] => s.dim = true,
            // `4:0` is no underline; `4:3` and the like are underline styles.
            [4, 0] => s.underline = false,
            [4, ..] => s.underline = true,
            [7] => s.inverse = true,
            [22] => {
                s.bold = false;
                s.dim = false;
            }
            [24] => s.underline = false,
            [27] => s.inverse = false,
            [n @ 30..=37] => s.fg = (n - 30) as u8,
            [39] => s.fg = color::DEFAULT,
            [n @ 40..=47] => s.bg = (n - 40) as u8,
            [49] => s.bg = color::DEFAULT,
            [n @ 90..=97] => s.fg = (n - 90 + 8) as u8,
            [n @ 100..=107] => s.bg = (n - 100 + 8) as u8,
            // Foreground, background and underline (not drawn) colours.
            [n @ (38 | 48 | 58), ref sub @ ..] => match (n, extended_colour(sub, &mut it)) {
                (38, Some(c)) => s.fg = c,
                (48, Some(c)) => s.bg = c,
                _ => {}
            },
            _ => {}
        }
    }
    s
}

/// The palette colour of an extended colour, from its parameter's
/// sub-parameters (`38:5:N`, `38:2:S:R:G:B` with a colour space `S`, often
/// left empty, or `38:2:R:G:B`) or, written with semicolons, from the
/// parameters that follow (`38;5;N`, `38;2;R;G;B`), which it consumes.
/// Colours beyond the palette's 16 take the nearest of them.
fn extended_colour<'a>(sub: &[u16], rest: &mut impl Iterator<Item = &'a [u16]>) -> Option<u8> {
    let mut next = || rest.next().and_then(|p| p.first().copied());
    match *sub {
        [] => match next()? {
            5 => next().map(palette_256),
            2 => {
                let (r, g, b) = (next()?, next()?, next()?);
                Some(approx_rgb(r, g, b))
            }
            _ => None,
        },
        [5, i, ..] => Some(palette_256(i)),
        [2, _, r, g, b, ..] | [2, r, g, b] => Some(approx_rgb(r, g, b)),
        _ => None,
    }
}

/// The palette colour of 256-colour index `i` (the first 16 exactly).
fn palette_256(i: u16) -> u8 {
    if i < 16 { i as u8 } else { approx_256(i.min(255) as u32) }
}

/// The palette colour nearest a true colour, through the 256-colour cube.
fn approx_rgb(r: u16, g: u16, b: u16) -> u8 {
    // The cube's levels are 0, 95, 135, 175, 215 and 255.
    let level = |v: u16| match v.min(255) {
        0..48 => 0,
        48..115 => 1,
        v => ((v - 35) / 40) as u32,
    };
    approx_256(16 + 36 * level(r) + 6 * level(g) + level(b))
}

/// Maps a colour of the xterm 256-colour cube onto the 16-colour palette.
fn approx_256(c: u32) -> u8 {
    if c >= 232 {
        return if c < 244 { color::GREY } else { color::WHITE };
    }
    let c = c - 16;
    let (r, g, b) = (c / 36, (c / 6) % 6, c % 6);
    let bright = r.max(g).max(b) >= 4;
    let bit = |v: u32| (v >= 2) as u8;
    let idx = bit(r) | bit(g) << 1 | bit(b) << 2;
    if bright { idx + 8 } else { idx }
}

/// Characters that end a "word" for double-click selection.
pub fn is_word_char(c: char) -> bool {
    !c.is_whitespace()
        && !matches!(c, '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '|' | ';' | ',' | '`')
}

/// The word around `col` in `cells`: `(start, end)` column range.
pub fn word_at(cells: &[Cell], col: usize) -> (usize, usize) {
    if cells.is_empty() {
        return (0, 0);
    }
    let col = col.min(cells.len() - 1);
    if !is_word_char(cells[col].ch) {
        return (col, col + 1);
    }
    let mut a = col;
    while a > 0 && is_word_char(cells[a - 1].ch) {
        a -= 1;
    }
    let mut b = col;
    while b < cells.len() && is_word_char(cells[b].ch) {
        b += 1;
    }
    (a, b)
}

/// Converts plain text to cells in one style.
pub fn cells(text: &str, style: Style) -> Vec<Cell> {
    text.chars().filter(|c| !c.is_control()).map(|ch| Cell { ch, style }).collect()
}

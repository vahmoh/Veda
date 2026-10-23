// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The expression being typed: its text, where the caret is, and the edits the keys make.
//!
//! The text is split into *segments* that editing treats as units, whether or not the text is
//! a well-formed expression yet: a number (whose digits are edited one by one), a name (`π`,
//! `Ans`, or a function with its opening parenthesis, `sin(`), an operator, a parenthesis, a
//! sign before or after a number (`√`, `²`, `!`), or spaces. Backspace removes a digit or a
//! whole unit, and the arrow keys move from unit to unit.
//!
//! The previous result, carried on after `=`, is the character [`ANS_MARK`] in the text: it
//! reads as the result itself, exactly, and is shown as its digits.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

pub use crate::lexer::ANS_MARK;
use crate::lexer::{self, Word, decimal_len};

/// What a segment of the text is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Number,
    /// A name: a constant, the previous result, a word operator (`mod`, `AND`), or a function
    /// with its opening parenthesis.
    Name,
    /// `+`, `−`, `×`, `÷`, `^`, `<<`, `,` ...
    Operator,
    Open,
    Close,
    /// After a number: `²`, `³`, `⁻¹`, `!`, `%`.
    Postfix,
    /// Before a number: `√`, `³√`.
    Prefix,
    Space,
    /// Anything else (a character that has no place in an expression).
    Other,
}

/// A unit of the text: its kind and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub kind: Kind,
    pub range: Range<usize>,
}

/// The superscript minus and one of `⁻¹`.
const INVERSE: &str = "\u{207B}\u{00B9}";

/// Splits `text` into segments (`programmer`: in Programmer mode, where numbers have letters).
pub fn segments(text: &str, programmer: bool) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let c = rest.chars().next().unwrap_or(' ');
        let (kind, len) = if c.is_whitespace() {
            (Kind::Space, rest.chars().take_while(|c| c.is_whitespace()).map(char::len_utf8).sum())
        } else if programmer && c.is_ascii_alphanumeric() {
            let len = rest.bytes().take_while(u8::is_ascii_alphanumeric).count();
            let kind = match lexer::word(&rest[..len]) {
                Some(Word::Func(_) | Word::Pi | Word::E) | None => Kind::Number,
                Some(_) => Kind::Name,
            };
            (kind, len)
        } else if c.is_ascii_digit() || c == '.' {
            (Kind::Number, decimal_len(rest).max(1))
        } else if c.is_ascii_alphabetic() {
            let mut len = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
            let word = lexer::word(&rest[..len]);
            for suffix in ["₂", "₁₀", INVERSE] {
                if rest[len..].starts_with(suffix) {
                    len += suffix.len();
                    break;
                }
            }
            if matches!(word, Some(Word::Func(_))) && rest[len..].starts_with('(') {
                len += 1;
            }
            (Kind::Name, len)
        } else if c == ANS_MARK || c == 'π' {
            (Kind::Name, c.len_utf8())
        } else if rest.starts_with("³√") {
            (Kind::Prefix, "³√".len())
        } else if rest.starts_with(INVERSE) {
            (Kind::Postfix, INVERSE.len())
        } else if rest.starts_with("<<") || rest.starts_with(">>") {
            (Kind::Operator, 2)
        } else {
            let kind = match c {
                '√' | '∛' => Kind::Prefix,
                '²' | '³' | '!' | '%' => Kind::Postfix,
                '(' => Kind::Open,
                ')' => Kind::Close,
                '+' | '-' | '\u{2212}' | '*' | '×' | '·' | '/' | '÷' | '^' | ',' | '&' | '|' | '~' => Kind::Operator,
                _ => Kind::Other,
            };
            (kind, c.len_utf8())
        };
        out.push(Segment { kind, range: i..i + len });
        i += len;
    }
    out
}

/// Whether a segment ends an operand: a number, a name that is not a function's opening, a
/// closing parenthesis, or a postfix sign.
fn ends_operand(text: &str, s: &Segment) -> bool {
    match s.kind {
        Kind::Number | Kind::Close | Kind::Postfix => true,
        Kind::Name => !text[s.range.clone()].ends_with('(') && !is_word_operator(&text[s.range.clone()]),
        _ => false,
    }
}

/// Whether a name is an operator (`mod`, `AND`, `NOT`).
fn is_word_operator(name: &str) -> bool {
    matches!(
        lexer::word(name),
        Some(Word::Mod | Word::And | Word::Or | Word::Xor | Word::Nand | Word::Nor | Word::Not | Word::Rol | Word::Ror)
    )
}

/// The expression being typed and its caret (a byte offset at a character's start).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    text: String,
    cursor: usize,
    /// Programmer mode: numbers have letters.
    programmer: bool,
}

impl Entry {
    /// An empty entry (`programmer`: for Programmer mode).
    pub fn new(programmer: bool) -> Entry {
        Entry { text: String::new(), cursor: 0, programmer }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Whether the caret is at the end.
    pub fn at_end(&self) -> bool {
        self.cursor == self.text.len()
    }

    /// Replaces the text, the caret at its end.
    pub fn set(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.cursor = self.text.len();
    }

    pub fn clear(&mut self) {
        self.set("");
    }

    /// Puts the caret at `at` (moved back to a character's start, and within the text).
    pub fn set_cursor(&mut self, at: usize) {
        let mut at = at.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        self.cursor = at;
    }

    pub fn segments(&self) -> Vec<Segment> {
        segments(&self.text, self.programmer)
    }

    /// Inserts `s` at the caret, and puts the caret after it.
    pub fn insert(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// The segment the character before the caret is in.
    fn before(&self) -> Option<Segment> {
        self.segments().into_iter().find(|s| s.range.start < self.cursor && self.cursor <= s.range.end)
    }

    /// The segment the character after the caret is in.
    fn after(&self) -> Option<Segment> {
        self.segments().into_iter().find(|s| s.range.start <= self.cursor && self.cursor < s.range.end)
    }

    /// Removes what is before the caret: a digit of a number, or a whole unit (a name with its
    /// parenthesis, an operator with the spaces around it). Returns whether anything went.
    pub fn backspace(&mut self) -> bool {
        let Some(s) = self.before() else { return false };
        let range = if s.kind == Kind::Number {
            let ch = self.text[..self.cursor].chars().next_back().map_or(1, char::len_utf8);
            self.cursor - ch..self.cursor
        } else {
            self.unit_around(s)
        };
        self.text.replace_range(range.clone(), "");
        self.cursor = range.start;
        true
    }

    /// Removes what is after the caret, as [`Entry::backspace`] does before it.
    pub fn delete(&mut self) -> bool {
        let Some(s) = self.after() else { return false };
        let range = if s.kind == Kind::Number {
            let ch = self.text[self.cursor..].chars().next().map_or(1, char::len_utf8);
            self.cursor..self.cursor + ch
        } else {
            self.unit_around(s)
        };
        self.text.replace_range(range.clone(), "");
        self.cursor = range.start;
        true
    }

    /// A segment that is not a number, with the spaces around it (spaces alone with what they
    /// space out): what one press removes.
    fn unit_around(&self, s: Segment) -> Range<usize> {
        let segs = self.segments();
        let i = segs.iter().position(|x| x.range == s.range).unwrap_or(0);
        let (mut first, mut last) = (i, i);
        if segs[i].kind == Kind::Space {
            // The spaces after a word go with it.
            if i > 0 && segs[i - 1].kind != Kind::Number && s.range.end <= self.cursor {
                first = i - 1;
            } else if i + 1 < segs.len() && segs[i + 1].kind != Kind::Number && s.range.start >= self.cursor {
                last = i + 1;
            } else {
                return s.range;
            }
        }
        if first > 0 && segs[first - 1].kind == Kind::Space && segs[first].kind == Kind::Name {
            first -= 1;
        }
        if last + 1 < segs.len() && segs[last + 1].kind == Kind::Space && segs[last].kind == Kind::Name {
            last += 1;
        }
        segs[first].range.start..segs[last].range.end
    }

    /// Moves the caret one unit left (a character in a number, a whole name or operator),
    /// past the spaces around it.
    pub fn left(&mut self) {
        self.skip_spaces(false);
        if let Some(s) = self.before() {
            self.cursor = if s.kind == Kind::Number {
                self.cursor - self.text[..self.cursor].chars().next_back().map_or(1, char::len_utf8)
            } else {
                s.range.start
            };
        }
        self.skip_spaces(false);
    }

    /// Moves the caret one unit right.
    pub fn right(&mut self) {
        self.skip_spaces(true);
        if let Some(s) = self.after() {
            self.cursor = if s.kind == Kind::Number {
                self.cursor + self.text[self.cursor..].chars().next().map_or(1, char::len_utf8)
            } else {
                s.range.end
            };
        }
        self.skip_spaces(true);
    }

    /// Moves the caret past the spaces on one side of it.
    fn skip_spaces(&mut self, forward: bool) {
        if forward {
            if let Some(s) = self.after().filter(|s| s.kind == Kind::Space) {
                self.cursor = s.range.end;
            }
        } else if let Some(s) = self.before().filter(|s| s.kind == Kind::Space) {
            self.cursor = s.range.start;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// The operand that ends at the caret (spaces before the caret aside): a number or name
    /// with the postfix signs after it, or a parenthesised group with the function or prefix
    /// sign before it. `None` if what is before the caret does not end an operand.
    pub fn operand_before(&self) -> Option<Range<usize>> {
        let segs = self.segments();
        let mut i = segs.iter().rposition(|s| s.range.end <= self.cursor)?;
        while segs[i].kind == Kind::Space {
            i = i.checked_sub(1)?;
        }
        if !ends_operand(&self.text, &segs[i]) {
            return None;
        }
        let end = segs[i].range.end;
        // Postfix signs belong to what they follow.
        while segs[i].kind == Kind::Postfix {
            i = i.checked_sub(1)?;
        }
        let mut start = segs[i].range.start;
        match segs[i].kind {
            Kind::Close => {
                // Back to the parenthesis it closes, or the function call that opened it.
                let mut depth = 0usize;
                loop {
                    let s = &segs[i];
                    let opens =
                        s.kind == Kind::Open || (s.kind == Kind::Name && self.text[s.range.clone()].ends_with('('));
                    if s.kind == Kind::Close {
                        depth += 1;
                    } else if opens {
                        depth -= 1;
                        if depth == 0 {
                            start = s.range.start;
                            break;
                        }
                    }
                    i = i.checked_sub(1)?;
                }
            }
            Kind::Number | Kind::Name => {}
            _ => return None,
        }
        // A prefix sign before the operand goes with it (`√9`).
        if i > 0 && segs[i - 1].kind == Kind::Prefix {
            start = segs[i - 1].range.start;
        }
        Some(start..end)
    }

    /// Whether a minus at `at` (the start of an operand) is a sign rather than a subtraction:
    /// nothing that ends an operand comes before it.
    fn is_sign(&self, at: usize) -> bool {
        let segs = self.segments();
        let Some(i) = segs.iter().position(|s| s.range.start == at) else { return false };
        if !matches!(&self.text[segs[i].range.clone()], "-" | "\u{2212}") {
            return false;
        }
        !segs[..i].iter().rev().find(|s| s.kind != Kind::Space).is_some_and(|s| ends_operand(&self.text, s))
    }

    /// Changes the sign of the operand before the caret (`3` → `−3` → `3`; after `+` or `−`, in
    /// parentheses: `5 − 3` → `5 − (−3)` → `5 − 3`); with no operand there, starts one with a
    /// minus.
    pub fn toggle_sign(&mut self) {
        const MINUS: &str = "\u{2212}";
        let Some(op) = self.operand_before() else {
            self.insert(MINUS);
            return;
        };
        let operand = &self.text[op.clone()];
        // `(−x)`, a group that is only a sign and its operand: back to `x`.
        if operand.starts_with("(\u{2212}") && operand.ends_with(')') && self.group_closes_at_end(&op) {
            let tail = op.end - 1;
            self.text.replace_range(tail..op.end, "");
            self.text.replace_range(op.start..op.start + 1 + MINUS.len(), "");
            self.cursor = tail - 1 - MINUS.len();
            return;
        }
        // `−x`: back to `x`.
        if let Some(c) = self.text[..op.start].chars().next_back()
            && (c == '\u{2212}' || c == '-')
            && self.is_sign(op.start - c.len_utf8())
        {
            let at = op.start - c.len_utf8();
            self.text.replace_range(at..op.start, "");
            self.cursor = op.end - c.len_utf8();
            return;
        }
        // After an addition or subtraction, in parentheses, so that `5 − −3` reads as it means.
        if self.text[..op.start].trim_end().ends_with(['+', '-', '\u{2212}']) {
            self.text.insert(op.end, ')');
            self.text.insert_str(op.start, "(\u{2212}");
            self.cursor = op.end + 1 + MINUS.len() + 1;
        } else {
            self.text.insert_str(op.start, MINUS);
            self.cursor = op.end + MINUS.len();
        }
    }

    /// Whether the operand `op`, which starts with a parenthesis, is one group: the parenthesis
    /// it starts with closes at its end.
    fn group_closes_at_end(&self, op: &Range<usize>) -> bool {
        let mut depth = 0usize;
        for (i, c) in self.text[op.clone()].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return op.start + i + 1 == op.end;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Applies a function to the operand before the caret (`9` → `√9`, `30` → `sin(30)`), or,
    /// with none there, inserts its name for what is typed next. `name` is a prefix sign (`√`)
    /// or a function's name with its parenthesis (`sin(`).
    pub fn apply(&mut self, name: &str) {
        let Some(op) = self.operand_before().filter(|op| op.end == self.cursor) else {
            self.insert(name);
            return;
        };
        if name.ends_with('(') {
            self.text.insert(op.end, ')');
            self.text.insert_str(op.start, name);
            self.cursor = op.end + name.len() + 1;
        } else {
            self.text.insert_str(op.start, name);
            self.cursor = op.end + name.len();
        }
    }

    /// The number the caret is in or just after, if any (to refuse a second point in it).
    pub fn number_at_cursor(&self) -> Option<&str> {
        let s = self.before().filter(|s| s.kind == Kind::Number)?;
        Some(&self.text[s.range])
    }

    /// The operators just before the caret, with the spaces around them, as one range (`5 × −`
    /// has `×−`): what a new operator replaces.
    pub fn operators_before(&self) -> Option<Range<usize>> {
        let segs = self.segments();
        let mut start = None;
        for s in segs.iter().rev().filter(|s| s.range.end <= self.cursor) {
            let is_op =
                s.kind == Kind::Operator || (s.kind == Kind::Name && is_word_operator(&self.text[s.range.clone()]));
            if is_op || (s.kind == Kind::Space && start.is_some()) {
                start = Some(s.range.start);
            } else if s.kind != Kind::Space {
                break;
            }
        }
        start.map(|s| s..self.cursor)
    }

    /// Replaces `range` with `with`, the caret after it.
    pub fn replace(&mut self, range: Range<usize>, with: &str) {
        self.text.replace_range(range.clone(), with);
        self.cursor = range.start + with.len();
    }

    /// How many parentheses are open before the caret and not closed.
    pub fn open_parens_before(&self) -> usize {
        let mut depth = 0usize;
        for s in segments(&self.text[..self.cursor], self.programmer) {
            let t = &self.text[s.range.clone()];
            if s.kind == Kind::Open || (s.kind == Kind::Name && t.ends_with('(')) {
                depth += 1;
            } else if s.kind == Kind::Close {
                depth = depth.saturating_sub(1);
            }
        }
        depth
    }

    /// The text as shown, [`ANS_MARK`] replaced by `ans`, and where the caret is in it.
    pub fn shown(&self, ans: &str) -> (String, usize) {
        let mut out = String::with_capacity(self.text.len());
        let mut cursor = self.cursor;
        for (i, c) in self.text.char_indices() {
            if c == ANS_MARK {
                out.push_str(ans);
                if i < self.cursor {
                    cursor = cursor + ans.len() - c.len_utf8();
                }
            } else {
                out.push(c);
            }
        }
        (out, cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str) -> Entry {
        let mut e = Entry::new(false);
        e.set(text);
        e
    }

    fn kinds(text: &str) -> Vec<(Kind, &str)> {
        segments(text, false).into_iter().map(|s| (s.kind, &text[s.range])).collect()
    }

    #[test]
    fn text_splits_into_units() {
        use Kind::*;
        assert_eq!(
            kinds("12.5×sin(30)+√2²"),
            [
                (Number, "12.5"),
                (Operator, "×"),
                (Name, "sin("),
                (Number, "30"),
                (Close, ")"),
                (Operator, "+"),
                (Prefix, "√"),
                (Number, "2"),
                (Postfix, "²")
            ]
        );
        assert_eq!(kinds("sin⁻¹(1)")[0], (Name, "sin⁻¹("));
        assert_eq!(kinds("log₂(8)")[0], (Name, "log₂("));
        assert_eq!(kinds("2 mod 3"), [(Number, "2"), (Space, " "), (Name, "mod"), (Space, " "), (Number, "3")]);
        assert_eq!(kinds("1E+5π"), [(Number, "1E+5"), (Name, "π")]);
        let hex = |t: &'static str| segments(t, true).into_iter().map(|s| (s.kind, &t[s.range])).collect::<Vec<_>>();
        assert_eq!(hex("FF AND 1"), [(Number, "FF"), (Space, " "), (Name, "AND"), (Space, " "), (Number, "1")]);
    }

    #[test]
    fn backspace_removes_digits_and_whole_names() {
        let mut e = entry("12×sin(30");
        e.backspace();
        assert_eq!(e.text(), "12×sin(3");
        e.backspace();
        e.backspace();
        assert_eq!(e.text(), "12×", "the function and its parenthesis go together");
        e.backspace();
        assert_eq!(e.text(), "12");
        let mut e = entry("FF AND ");
        e.programmer = true;
        e.backspace();
        assert_eq!(e.text(), "FF", "a word with its spaces");
        let mut e = entry("");
        assert!(!e.backspace());
        let mut e = entry("5³√8");
        e.set_cursor(5);
        assert_eq!(e.cursor(), 1 + "³".len(), "back to a character's start");
        e.set_cursor(1 + "³√".len());
        e.backspace();
        assert_eq!(e.text(), "58", "³√ is one unit");
    }

    #[test]
    fn the_caret_moves_by_units() {
        let mut e = entry("12×sin(30)");
        e.left();
        assert_eq!(e.cursor(), "12×sin(30".len());
        e.left();
        e.left();
        e.left();
        assert_eq!(e.cursor(), "12×".len());
        e.left();
        assert_eq!(e.cursor(), 2);
        e.insert("5");
        assert_eq!(e.text(), "125×sin(30)");
        e.right();
        assert_eq!(e.cursor(), "125×".len());
        e.home();
        e.delete();
        assert_eq!(e.text(), "25×sin(30)");
        e.end();
        assert!(e.at_end());
        let mut e = entry("2 mod 3");
        e.set_cursor(6);
        e.left();
        assert_eq!(e.cursor(), 1, "past the word and its spaces");
        e.right();
        assert_eq!(e.cursor(), 6);
    }

    #[test]
    fn operands_before_the_caret() {
        let op = |t: &str| entry(t).operand_before().map(|r| String::from(&t[r]));
        assert_eq!(op("2+35").as_deref(), Some("35"));
        assert_eq!(op("2×(3+4)").as_deref(), Some("(3+4)"));
        assert_eq!(op("1+sin(30)²").as_deref(), Some("sin(30)²"));
        assert_eq!(op("1+√9").as_deref(), Some("√9"));
        assert_eq!(op("4!").as_deref(), Some("4!"));
        assert_eq!(op("2π").as_deref(), Some("π"));
        assert_eq!(op("2+").as_deref(), None);
        assert_eq!(op("sin(").as_deref(), None);
        assert_eq!(op("").as_deref(), None);
        assert_eq!(op("((1)+2)").as_deref(), Some("((1)+2)"));
    }

    #[test]
    fn signs_toggle() {
        let toggle = |t: &str| {
            let mut e = entry(t);
            e.toggle_sign();
            (String::from(e.text()), e.cursor())
        };
        assert_eq!(toggle("3").0, "\u{2212}3");
        assert_eq!(toggle("\u{2212}3").0, "3");
        assert_eq!(toggle("5×3").0, "5×\u{2212}3");
        assert_eq!(toggle("5×\u{2212}3").0, "5×3");
        assert_eq!(toggle("5\u{2212}3").0, "5\u{2212}(\u{2212}3)");
        assert_eq!(toggle("5\u{2212}(\u{2212}3)").0, "5\u{2212}3");
        assert_eq!(toggle("2+(1+2)").0, "2+(\u{2212}(1+2))");
        assert_eq!(toggle("2+").0, "2+\u{2212}");
        let (text, cursor) = toggle("12");
        assert_eq!(cursor, text.len());
        // A subtraction's minus is not a sign.
        assert_eq!(toggle("8\u{2212}2").0, "8\u{2212}(\u{2212}2)");
    }

    #[test]
    fn functions_apply_to_the_number_before_them() {
        let apply = |t: &str, f: &str| {
            let mut e = entry(t);
            e.apply(f);
            (String::from(e.text()), e.cursor())
        };
        assert_eq!(apply("9", "√"), ("√9".into(), "√9".len()));
        assert_eq!(apply("2+30", "sin("), ("2+sin(30)".into(), "2+sin(30)".len()));
        assert_eq!(apply("2+(1+3)", "√"), ("2+√(1+3)".into(), "2+√(1+3)".len()));
        assert_eq!(apply("2+", "sin("), ("2+sin(".into(), "2+sin(".len()));
        assert_eq!(apply("", "√"), ("√".into(), "√".len()));
    }

    #[test]
    fn helpers() {
        let mut e = entry("1.5+2");
        assert_eq!(e.number_at_cursor(), Some("2"));
        e.set_cursor(2);
        assert_eq!(e.number_at_cursor(), Some("1.5"));
        assert_eq!(entry("2 + ").operators_before(), Some(1..4));
        assert_eq!(entry("2 mod").operators_before(), Some(1..5));
        assert_eq!(entry("5×\u{2212}").operators_before(), Some(1..1 + "×\u{2212}".len()));
        assert_eq!(entry("2").operators_before(), None);
        assert_eq!(entry("sin(2×(3").open_parens_before(), 2);
        let mut e = entry("\u{E000}+1");
        e.set_cursor(ANS_MARK.len_utf8());
        assert_eq!(e.shown("0.5"), ("0.5+1".into(), 3));
        assert_eq!(kinds("\u{E000}2")[0].0, Kind::Name);
    }
}

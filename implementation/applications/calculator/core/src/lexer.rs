// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The words of an expression: numbers, names, operators and parentheses, each with where it is
//! in the text.
//!
//! The keys write the calculator's signs (`×`, `÷`, `−`, `√`, `π`, `²`, `⁻¹`), and the
//! keyboard and pasted text the plain ones (`*`, `/`, `-`, `sqrt`, `pi`, `^2`): both read the
//! same. Names are read whatever their case. In Programmer mode numbers are written in the
//! current base, so `FF` is a number in hexadecimal; every word of that mode (`AND`, `ROL`)
//! has a letter that is not a hexadecimal digit, so none is taken for a number.

use alloc::vec::Vec;

use crate::format::parse_decimal;
use crate::number::{CalcError, Number, Syntax};

/// A function, called with its arguments in parentheses (or on the number after it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Func {
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Asinh,
    Acosh,
    Atanh,
    /// The natural logarithm.
    Ln,
    /// The logarithm to base 10, or to the base its second argument gives.
    Log,
    Log2,
    Exp,
    Sqrt,
    Cbrt,
    /// `root(x, n)`: the `n`-th root of `x`.
    Root,
    Abs,
    Floor,
    Ceil,
    Round,
    Gcd,
    Lcm,
    /// `nCr(n, k)`: combinations.
    NCr,
    /// `nPr(n, k)`: permutations.
    NPr,
}

impl Func {
    /// How the keys write the function's name.
    pub fn name(self) -> &'static str {
        match self {
            Func::Sin => "sin",
            Func::Cos => "cos",
            Func::Tan => "tan",
            Func::Asin => "sin⁻¹",
            Func::Acos => "cos⁻¹",
            Func::Atan => "tan⁻¹",
            Func::Sinh => "sinh",
            Func::Cosh => "cosh",
            Func::Tanh => "tanh",
            Func::Asinh => "sinh⁻¹",
            Func::Acosh => "cosh⁻¹",
            Func::Atanh => "tanh⁻¹",
            Func::Ln => "ln",
            Func::Log => "log",
            Func::Log2 => "log₂",
            Func::Exp => "exp",
            Func::Sqrt => "sqrt",
            Func::Cbrt => "cbrt",
            Func::Root => "root",
            Func::Abs => "abs",
            Func::Floor => "floor",
            Func::Ceil => "ceil",
            Func::Round => "round",
            Func::Gcd => "gcd",
            Func::Lcm => "lcm",
            Func::NCr => "nCr",
            Func::NPr => "nPr",
        }
    }

    /// The function undoing this one, as `sin` and `sin⁻¹`: the inverse a name followed by
    /// `⁻¹` reads as.
    fn inverse(self) -> Option<Func> {
        Some(match self {
            Func::Sin => Func::Asin,
            Func::Cos => Func::Acos,
            Func::Tan => Func::Atan,
            Func::Sinh => Func::Asinh,
            Func::Cosh => Func::Acosh,
            Func::Tanh => Func::Atanh,
            _ => return None,
        })
    }
}

/// A word with a meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word {
    Func(Func),
    /// π.
    Pi,
    /// Euler's number.
    E,
    /// The last result.
    Ans,
    /// The remainder operator (`7 mod 3`).
    Mod,
    And,
    Or,
    Xor,
    Nand,
    Nor,
    Not,
    /// Rotations (Programmer mode).
    Rol,
    Ror,
}

/// What a token is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tok {
    Num(Number),
    Word(Word),
    Plus,
    Minus,
    Times,
    Divide,
    Power,
    Percent,
    Bang,
    /// `²`
    Square,
    /// `³`
    Cube,
    /// `⁻¹`, the reciprocal.
    Inverse,
    /// `√`, before what it is the root of.
    Sqrt,
    /// `³√` (or `∛`).
    Cbrt,
    Open,
    Close,
    Comma,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
}

/// A token and where it is: its first byte and the one after it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub start: usize,
    pub end: usize,
}

/// The names a word may be, lower case.
const NAMES: &[(&str, Word)] = &[
    ("sin", Word::Func(Func::Sin)),
    ("cos", Word::Func(Func::Cos)),
    ("tan", Word::Func(Func::Tan)),
    ("asin", Word::Func(Func::Asin)),
    ("acos", Word::Func(Func::Acos)),
    ("atan", Word::Func(Func::Atan)),
    ("arcsin", Word::Func(Func::Asin)),
    ("arccos", Word::Func(Func::Acos)),
    ("arctan", Word::Func(Func::Atan)),
    ("sinh", Word::Func(Func::Sinh)),
    ("cosh", Word::Func(Func::Cosh)),
    ("tanh", Word::Func(Func::Tanh)),
    ("asinh", Word::Func(Func::Asinh)),
    ("acosh", Word::Func(Func::Acosh)),
    ("atanh", Word::Func(Func::Atanh)),
    ("ln", Word::Func(Func::Ln)),
    ("log", Word::Func(Func::Log)),
    ("exp", Word::Func(Func::Exp)),
    ("sqrt", Word::Func(Func::Sqrt)),
    ("cbrt", Word::Func(Func::Cbrt)),
    ("root", Word::Func(Func::Root)),
    ("abs", Word::Func(Func::Abs)),
    ("floor", Word::Func(Func::Floor)),
    ("ceil", Word::Func(Func::Ceil)),
    ("round", Word::Func(Func::Round)),
    ("gcd", Word::Func(Func::Gcd)),
    ("lcm", Word::Func(Func::Lcm)),
    ("ncr", Word::Func(Func::NCr)),
    ("npr", Word::Func(Func::NPr)),
    ("pi", Word::Pi),
    ("e", Word::E),
    ("ans", Word::Ans),
    ("mod", Word::Mod),
    ("and", Word::And),
    ("or", Word::Or),
    ("xor", Word::Xor),
    ("nand", Word::Nand),
    ("nor", Word::Nor),
    ("not", Word::Not),
    ("rol", Word::Rol),
    ("ror", Word::Ror),
];

/// The word `name` is, whatever its case.
pub fn word(name: &str) -> Option<Word> {
    NAMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|&(_, w)| w)
}

/// The superscript minus and one of `⁻¹`.
const INVERSE: &str = "\u{207B}\u{00B9}";

/// The previous result in an expression, carried on after `=` (see `entry`): a character of
/// Unicode's private use area, which no keyboard types.
pub const ANS_MARK: char = '\u{E000}';

/// Reads `text` into tokens; `radix` is Programmer mode's base (`None` in the other modes).
pub fn tokenize(text: &str, radix: Option<u32>) -> Result<Vec<Token>, CalcError> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some(&(start, c)) = chars.peek() {
        let rest = &text[start..];
        let single = |tok: Tok| Token { tok, start, end: start + c.len_utf8() };
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if let Some(radix) = radix
            && c.is_ascii_alphanumeric()
        {
            let len = rest.bytes().take_while(u8::is_ascii_alphanumeric).count();
            let w = &rest[..len];
            let tok = match word(w) {
                Some(Word::Func(_) | Word::Pi | Word::E) | None => Tok::Num(parse_radix(w, radix, start)?),
                Some(w) => Tok::Word(w),
            };
            tokens.push(Token { tok, start, end: start + len });
            skip(&mut chars, start + len);
            continue;
        }
        if radix.is_some() && c == '.' {
            return Err(CalcError::NotWhole);
        }
        if c.is_ascii_digit() || c == '.' {
            let len = decimal_len(rest);
            let n = parse_decimal(&rest[..len]).ok_or(CalcError::Syntax(Syntax::BadNumber, start))?;
            tokens.push(Token { tok: Tok::Num(n), start, end: start + len });
            skip(&mut chars, start + len);
            continue;
        }
        if c.is_ascii_alphabetic() {
            let len = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
            let mut end = start + len;
            let name = &rest[..len];
            // log₂ and log₁₀, and log2( and log10( as they are typed.
            let mut w = word(name);
            if name.eq_ignore_ascii_case("log") {
                for (suffix, base) in [("₂", Func::Log2), ("₁₀", Func::Log), ("2(", Func::Log2), ("10(", Func::Log)]
                {
                    if text[end..].starts_with(suffix) {
                        w = Some(Word::Func(base));
                        end += suffix.trim_end_matches('(').len();
                        break;
                    }
                }
            }
            let mut w = w.ok_or(CalcError::Syntax(Syntax::UnknownName, start))?;
            // sin⁻¹: the inverse function.
            if let Word::Func(f) = w
                && text[end..].starts_with(INVERSE)
                && let Some(inverse) = f.inverse()
            {
                w = Word::Func(inverse);
                end += INVERSE.len();
            }
            tokens.push(Token { tok: Tok::Word(w), start, end });
            skip(&mut chars, end);
            continue;
        }
        let tok = match c {
            '+' => Tok::Plus,
            '-' | '\u{2212}' | '\u{2013}' => Tok::Minus,
            '*' | '×' | '·' | '\u{2219}' => Tok::Times,
            '/' | '÷' | '\u{2215}' => Tok::Divide,
            '^' => Tok::Power,
            '%' => Tok::Percent,
            '!' => Tok::Bang,
            '²' => Tok::Square,
            '(' | '[' => Tok::Open,
            ')' | ']' => Tok::Close,
            ',' | ';' => Tok::Comma,
            '√' => Tok::Sqrt,
            '∛' => Tok::Cbrt,
            'π' => Tok::Word(Word::Pi),
            ANS_MARK => Tok::Word(Word::Ans),
            '&' => Tok::Word(Word::And),
            '|' => Tok::Word(Word::Or),
            '~' => Tok::Word(Word::Not),
            '³' if rest[c.len_utf8()..].starts_with('√') => {
                tokens.push(Token { tok: Tok::Cbrt, start, end: start + c.len_utf8() + '√'.len_utf8() });
                skip(&mut chars, start + c.len_utf8() + '√'.len_utf8());
                continue;
            }
            '³' => Tok::Cube,
            '\u{207B}' if rest.starts_with(INVERSE) => {
                tokens.push(Token { tok: Tok::Inverse, start, end: start + INVERSE.len() });
                skip(&mut chars, start + INVERSE.len());
                continue;
            }
            '<' | '>' if rest[1..].starts_with(c) => {
                tokens.push(Token { tok: if c == '<' { Tok::Shl } else { Tok::Shr }, start, end: start + 2 });
                skip(&mut chars, start + 2);
                continue;
            }
            _ => return Err(CalcError::Syntax(Syntax::Unexpected, start)),
        };
        tokens.push(single(tok));
        chars.next();
    }
    Ok(tokens)
}

/// Advances `chars` to byte `to`.
fn skip(chars: &mut core::iter::Peekable<core::str::CharIndices<'_>>, to: usize) {
    while chars.peek().is_some_and(|&(i, _)| i < to) {
        chars.next();
    }
}

/// The length of the decimal literal `text` starts with: digits and a point, then an exponent
/// (`E`, a sign and digits) only if one follows, so that `2e` is two times e.
pub fn decimal_len(text: &str) -> usize {
    let b = text.as_bytes();
    let mut i = b.iter().take_while(|c| c.is_ascii_digit() || **c == b'.').count();
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if text[j..].starts_with(['+', '-']) {
            j += 1;
        } else if text[j..].starts_with('\u{2212}') {
            j += '\u{2212}'.len_utf8();
        }
        let digits = b[j..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 {
            i = j + digits;
        }
    }
    i
}

/// A whole number written in `radix` (a word of letters and digits), or why it is not one.
fn parse_radix(word: &str, radix: u32, start: usize) -> Result<Number, CalcError> {
    let mut value: u128 = 0;
    for (i, c) in word.char_indices() {
        let Some(d) = c.to_digit(radix) else {
            let what = if c.is_ascii_hexdigit() { Syntax::BadDigit } else { Syntax::UnknownName };
            return Err(CalcError::Syntax(what, start + i));
        };
        value = value.checked_mul(radix as u128).and_then(|v| v.checked_add(d as u128)).ok_or(CalcError::Overflow)?;
    }
    if value > u64::MAX as u128 {
        return Err(CalcError::Overflow);
    }
    Ok(Number::int(value as i128))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(text: &str) -> Vec<Tok> {
        tokenize(text, None).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn signs_and_their_ascii_spellings_read_alike() {
        let n = |x| Tok::Num(Number::int(x));
        assert_eq!(toks("2×3÷4−1"), [n(2), Tok::Times, n(3), Tok::Divide, n(4), Tok::Minus, n(1)]);
        assert_eq!(toks("2*3/4-1"), toks("2×3÷4−1"));
        assert_eq!(toks("√9 + π"), [Tok::Sqrt, n(9), Tok::Plus, Tok::Word(Word::Pi)]);
        assert_eq!(
            toks("SQRT(9)+PI"),
            [Tok::Word(Word::Func(Func::Sqrt)), Tok::Open, n(9), Tok::Close, Tok::Plus, Tok::Word(Word::Pi)]
        );
        assert_eq!(toks("5²³√8"), [n(5), Tok::Square, Tok::Cbrt, n(8)]);
        assert_eq!(toks("2³"), [n(2), Tok::Cube]);
        assert_eq!(toks("4⁻¹"), [n(4), Tok::Inverse]);
        assert_eq!(toks("1<<2>>3"), [n(1), Tok::Shl, n(2), Tok::Shr, n(3)]);
    }

    #[test]
    fn names() {
        let f = |f| Tok::Word(Word::Func(f));
        assert_eq!(toks("sin⁻¹(1)")[0], f(Func::Asin));
        assert_eq!(toks("tanh⁻¹ 0")[0], f(Func::Atanh));
        assert_eq!(toks("log₂(8)")[0], f(Func::Log2));
        assert_eq!(toks("log2(8)")[..2], [f(Func::Log2), Tok::Open]);
        assert_eq!(toks("log10(8)")[..2], [f(Func::Log), Tok::Open]);
        assert_eq!(toks("log 2")[0], f(Func::Log), "log of two");
        assert_eq!(toks("Ans mod 2")[..2], [Tok::Word(Word::Ans), Tok::Word(Word::Mod)]);
        assert_eq!(tokenize("2 + foo", None), Err(CalcError::Syntax(Syntax::UnknownName, 4)));
        assert_eq!(tokenize("2 # 3", None), Err(CalcError::Syntax(Syntax::Unexpected, 2)));
    }

    #[test]
    fn exponents_and_e() {
        let n = |x| Tok::Num(x);
        assert_eq!(toks("2e3"), [n(Number::int(2000))]);
        assert_eq!(toks("2E−3"), [n(Number::fraction(2, 1000).unwrap())]);
        assert_eq!(toks("2e"), [n(Number::int(2)), Tok::Word(Word::E)]);
        assert_eq!(toks("2e+"), [n(Number::int(2)), Tok::Word(Word::E), Tok::Plus]);
        assert_eq!(tokenize("1.2.3", None), Err(CalcError::Syntax(Syntax::BadNumber, 0)));
        let spans: Vec<(usize, usize)> = tokenize("12 + 3.5", None).unwrap().iter().map(|t| (t.start, t.end)).collect();
        assert_eq!(spans, [(0, 2), (3, 4), (5, 8)]);
    }

    #[test]
    fn numbers_in_a_base() {
        let hex = |t: &str| tokenize(t, Some(16)).map(|v| v.into_iter().map(|t| t.tok).collect::<Vec<_>>());
        let n = |x| Tok::Num(Number::int(x));
        assert_eq!(hex("FF AND 0f"), Ok(alloc::vec![n(255), Tok::Word(Word::And), n(15)]));
        assert_eq!(hex("add + dec"), Ok(alloc::vec![n(0xADD), Tok::Plus, n(0xDEC)]));
        assert_eq!(hex("ans xor 1"), Ok(alloc::vec![Tok::Word(Word::Ans), Tok::Word(Word::Xor), n(1)]));
        assert_eq!(tokenize("102", Some(2)), Err(CalcError::Syntax(Syntax::BadDigit, 2)));
        assert_eq!(tokenize("1 + zz", Some(16)), Err(CalcError::Syntax(Syntax::UnknownName, 4)));
        assert_eq!(tokenize("FFFFFFFFFFFFFFFF", Some(16)).map(|t| t[0].tok), Ok(n(u64::MAX as i128)));
        assert_eq!(tokenize("1FFFFFFFFFFFFFFFF", Some(16)), Err(CalcError::Overflow));
        assert_eq!(tokenize("777", Some(8)).map(|t| t[0].tok), Ok(n(0o777)));
    }
}

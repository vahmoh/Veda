// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Expressions as trees, by precedence climbing (Pratt parsing).
//!
//! From the loosest binding to the tightest:
//!
//! | Operators | Notes |
//! |-----------|-------|
//! | `OR`, `NOR` | Programmer mode |
//! | `XOR` | |
//! | `AND`, `NAND` | |
//! | `<<`, `>>`, `ROL`, `ROR` | |
//! | `+`, `−` | |
//! | `×`, `÷`, `mod`, and side by side (`2π`, `3(4 + 5)`) | so `1 ÷ 2π` is half of π |
//! | prefix `−`, `+`, `NOT` | so `−2²` is −4 |
//! | `^`, `√`, `³√` and functions without parentheses (`sin 30`) | `^` from the right: `2^3^2` is 2^9 |
//! | `!`, `%`, `²`, `³`, `⁻¹` after a number | |
//!
//! Parentheses left open at the end are closed there, as calculators do: `sin(30` is `sin(30)`.
//! Two numbers side by side (`2 3`) are an error, not a product.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::lexer::{Func, Tok, Token, Word};
use crate::number::{CalcError, Number, Syntax};

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Mod,
    And,
    Or,
    Xor,
    Nand,
    Nor,
    Shl,
    Shr,
    Rol,
    Ror,
}

/// An operator after its operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostOp {
    Factorial,
    Percent,
    Square,
    Cube,
    Reciprocal,
}

/// An expression's tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(Number),
    /// The last result.
    Ans,
    Neg(Box<Node>),
    /// Bitwise complement (Programmer mode).
    Not(Box<Node>),
    Bin(BinOp, Box<Node>, Box<Node>),
    Post(PostOp, Box<Node>),
    Call(Func, Vec<Node>),
}

/// An expression and where it is in the text (its first byte and the one after it).
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub expr: Expr,
    pub start: usize,
    pub end: usize,
}

const OR: u8 = 2;
const XOR: u8 = 4;
const AND: u8 = 6;
const SHIFT: u8 = 8;
const ADD: u8 = 10;
const MUL: u8 = 20;
const PREFIX: u8 = 25;
const POW: u8 = 30;
const POSTFIX: u8 = 40;

/// The binary operator a token is, with how tightly it binds on its left and on its right
/// (equal for the right-associative `^`).
fn infix(tok: &Tok) -> Option<(BinOp, u8, u8)> {
    let left = |op, bp| Some((op, bp, bp + 1));
    match tok {
        Tok::Plus => left(BinOp::Add, ADD),
        Tok::Minus => left(BinOp::Sub, ADD),
        Tok::Times => left(BinOp::Mul, MUL),
        Tok::Divide => left(BinOp::Div, MUL),
        Tok::Word(Word::Mod) => left(BinOp::Mod, MUL),
        Tok::Power => Some((BinOp::Pow, POW, POW)),
        Tok::Shl => left(BinOp::Shl, SHIFT),
        Tok::Shr => left(BinOp::Shr, SHIFT),
        Tok::Word(Word::Rol) => left(BinOp::Rol, SHIFT),
        Tok::Word(Word::Ror) => left(BinOp::Ror, SHIFT),
        Tok::Word(Word::And) => left(BinOp::And, AND),
        Tok::Word(Word::Nand) => left(BinOp::Nand, AND),
        Tok::Word(Word::Xor) => left(BinOp::Xor, XOR),
        Tok::Word(Word::Or) => left(BinOp::Or, OR),
        Tok::Word(Word::Nor) => left(BinOp::Nor, OR),
        _ => None,
    }
}

/// The postfix operator a token is.
fn postfix(tok: &Tok) -> Option<PostOp> {
    Some(match tok {
        Tok::Bang => PostOp::Factorial,
        Tok::Percent => PostOp::Percent,
        Tok::Square => PostOp::Square,
        Tok::Cube => PostOp::Cube,
        Tok::Inverse => PostOp::Reciprocal,
        _ => return None,
    })
}

/// Whether a token begins an operand, which side by side with one before it multiplies.
fn begins_operand(tok: &Tok) -> bool {
    matches!(
        tok,
        Tok::Num(_) | Tok::Open | Tok::Sqrt | Tok::Cbrt | Tok::Word(Word::Func(_) | Word::Pi | Word::E | Word::Ans)
    )
}

struct Parser<'t> {
    tokens: &'t [Token],
    pos: usize,
    /// Where the text ends (for errors at its end).
    end: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).copied();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    /// An expression whose operators bind at least `min` tightly.
    fn expr(&mut self, min: u8) -> Result<Node, CalcError> {
        let mut lhs = self.prefix()?;
        while let Some(t) = self.peek().copied() {
            if let Some(op) = postfix(&t.tok) {
                if POSTFIX < min {
                    break;
                }
                self.pos += 1;
                lhs = Node { start: lhs.start, end: t.end, expr: Expr::Post(op, Box::new(lhs)) };
            } else if let Some((op, l, r)) = infix(&t.tok) {
                if l < min {
                    break;
                }
                self.pos += 1;
                let rhs = self.expr(r)?;
                lhs = Node { start: lhs.start, end: rhs.end, expr: Expr::Bin(op, Box::new(lhs), Box::new(rhs)) };
            } else if begins_operand(&t.tok) {
                if MUL < min {
                    break;
                }
                // Two numbers side by side are a typing mistake, not a product.
                if matches!(lhs.expr, Expr::Num(_)) && matches!(t.tok, Tok::Num(_)) {
                    return Err(CalcError::Syntax(Syntax::Unexpected, t.start));
                }
                let rhs = self.expr(MUL + 1)?;
                lhs =
                    Node { start: lhs.start, end: rhs.end, expr: Expr::Bin(BinOp::Mul, Box::new(lhs), Box::new(rhs)) };
            } else {
                break;
            }
        }
        Ok(lhs)
    }

    /// What an expression starts with: a number, a name, a prefix operator or a parenthesis.
    fn prefix(&mut self) -> Result<Node, CalcError> {
        let Some(t) = self.next() else {
            return Err(CalcError::Syntax(Syntax::MissingOperand, self.end));
        };
        let node = |expr, end| Node { expr, start: t.start, end };
        Ok(match t.tok {
            Tok::Num(n) => node(Expr::Num(n), t.end),
            Tok::Word(Word::Pi) => node(Expr::Num(Number::PI), t.end),
            Tok::Word(Word::E) => node(Expr::Num(Number::E), t.end),
            Tok::Word(Word::Ans) => node(Expr::Ans, t.end),
            Tok::Word(Word::Func(f)) => {
                let args = if self.peek().is_some_and(|n| n.tok == Tok::Open) {
                    self.pos += 1;
                    self.arguments()?
                } else {
                    alloc::vec![self.expr(POW)?]
                };
                let end = self.tokens[..self.pos].last().map_or(t.end, |l| l.end);
                node(Expr::Call(f, args), end)
            }
            Tok::Sqrt | Tok::Cbrt => {
                let arg = self.expr(POW)?;
                let f = if t.tok == Tok::Sqrt { Func::Sqrt } else { Func::Cbrt };
                let end = arg.end;
                node(Expr::Call(f, alloc::vec![arg]), end)
            }
            Tok::Minus => {
                let arg = self.expr(PREFIX)?;
                let end = arg.end;
                node(Expr::Neg(Box::new(arg)), end)
            }
            Tok::Plus => self.expr(PREFIX)?,
            Tok::Word(Word::Not) => {
                let arg = self.expr(PREFIX)?;
                let end = arg.end;
                node(Expr::Not(Box::new(arg)), end)
            }
            Tok::Open => {
                let mut inner = self.expr(0)?;
                inner.start = t.start;
                match self.peek() {
                    Some(c) if c.tok == Tok::Close => {
                        inner.end = c.end;
                        self.pos += 1;
                    }
                    // Closed at the end of the text.
                    None => inner.end = self.end,
                    Some(other) => return Err(CalcError::Syntax(Syntax::Unexpected, other.start)),
                }
                inner
            }
            Tok::Close => return Err(CalcError::Syntax(Syntax::UnmatchedParen, t.start)),
            _ => return Err(CalcError::Syntax(Syntax::MissingOperand, t.start)),
        })
    }

    /// A call's arguments after its `(`, up to its `)` or the end of the text.
    fn arguments(&mut self) -> Result<Vec<Node>, CalcError> {
        let mut args = Vec::new();
        loop {
            args.push(self.expr(0)?);
            match self.next() {
                None => return Ok(args),
                Some(t) if t.tok == Tok::Close => return Ok(args),
                Some(t) if t.tok == Tok::Comma => continue,
                Some(t) => return Err(CalcError::Syntax(Syntax::Unexpected, t.start)),
            }
        }
    }
}

/// Parses the tokens of a text `len` bytes long.
pub fn parse(tokens: &[Token], len: usize) -> Result<Node, CalcError> {
    if tokens.is_empty() {
        return Err(CalcError::Syntax(Syntax::Empty, 0));
    }
    let mut p = Parser { tokens, pos: 0, end: len };
    let node = p.expr(0)?;
    match p.peek() {
        None => Ok(node),
        Some(t) if t.tok == Tok::Close => Err(CalcError::Syntax(Syntax::UnmatchedParen, t.start)),
        Some(t) => Err(CalcError::Syntax(Syntax::Unexpected, t.start)),
    }
}

/// How many parentheses the tokens leave open (the closing ones a display shows faintly).
pub fn open_parens(tokens: &[Token]) -> usize {
    let mut depth = 0usize;
    for t in tokens {
        match t.tok {
            Tok::Open => depth += 1,
            Tok::Close => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use alloc::format;
    use alloc::string::String;

    /// The tree as a string of nested calls, for comparing.
    fn tree(text: &str) -> String {
        fn show(n: &Node) -> String {
            match &n.expr {
                Expr::Num(x) => format!("{}", x.to_f64()),
                Expr::Ans => "ans".into(),
                Expr::Neg(a) => format!("neg({})", show(a)),
                Expr::Not(a) => format!("not({})", show(a)),
                Expr::Bin(op, a, b) => format!("{op:?}({}, {})", show(a), show(b)),
                Expr::Post(op, a) => format!("{op:?}({})", show(a)),
                Expr::Call(f, args) => {
                    let args: alloc::vec::Vec<String> = args.iter().map(show).collect();
                    format!("{f:?}({})", args.join(", "))
                }
            }
        }
        match tokenize(text, None).and_then(|t| parse(&t, text.len())) {
            Ok(n) => show(&n),
            Err(e) => format!("error {e:?}"),
        }
    }

    #[test]
    fn precedence_and_associativity() {
        assert_eq!(tree("2+3×4"), "Add(2, Mul(3, 4))");
        assert_eq!(tree("2^3^2"), "Pow(2, Pow(3, 2))");
        assert_eq!(tree("−2^2"), "neg(Pow(2, 2))");
        assert_eq!(tree("−2×3"), "Mul(neg(2), 3)");
        assert_eq!(tree("8÷4÷2"), "Div(Div(8, 4), 2)");
        assert_eq!(tree("2^−3"), "Pow(2, neg(3))");
        assert_eq!(tree("−3!"), "neg(Factorial(3))");
        assert_eq!(tree("2^3!"), "Pow(2, Factorial(3))");
        assert_eq!(tree("50+10%"), "Add(50, Percent(10))");
        assert_eq!(tree("7 mod 3 + 1"), "Add(Mod(7, 3), 1)");
    }

    #[test]
    fn side_by_side_multiplies() {
        assert_eq!(tree("2π"), format!("Mul(2, {})", core::f64::consts::PI));
        assert_eq!(tree("3(4+5)"), "Mul(3, Add(4, 5))");
        assert_eq!(tree("(1)(2)"), "Mul(1, 2)");
        assert_eq!(tree("1÷2π"), format!("Mul(Div(1, 2), {})", core::f64::consts::PI));
        assert_eq!(tree("2√9"), "Mul(2, Sqrt(9))");
        assert_eq!(tree("2 sin(30)"), "Mul(2, Sin(30))");
        assert_eq!(tree("2Ans"), "Mul(2, ans)");
        assert!(tree("2 3").starts_with("error"));
    }

    #[test]
    fn functions_and_roots() {
        assert_eq!(tree("sin(30)"), "Sin(30)");
        assert_eq!(tree("sin 30 + 1"), "Add(Sin(30), 1)");
        assert_eq!(tree("sin(30)²"), "Square(Sin(30))");
        assert_eq!(tree("√4^2"), "Sqrt(Pow(4, 2))");
        assert_eq!(tree("√2×3"), "Mul(Sqrt(2), 3)");
        assert_eq!(tree("³√8"), "Cbrt(8)");
        assert_eq!(tree("nCr(5, 2)"), "NCr(5, 2)");
        assert_eq!(tree("log(8, 2)"), "Log(8, 2)");
    }

    #[test]
    fn parentheses_close_at_the_end() {
        assert_eq!(tree("sin(30"), "Sin(30)");
        assert_eq!(tree("(1+2)×(3"), "Mul(Add(1, 2), 3)");
        assert_eq!(tree("2×(3+(4"), "Mul(2, Add(3, 4))");
        let tokens = tokenize("2×(3+(4", None).unwrap();
        assert_eq!(open_parens(&tokens), 2);
    }

    #[test]
    fn errors_say_where() {
        let err = |t: &str| tokenize(t, None).and_then(|tokens| parse(&tokens, t.len())).err();
        assert_eq!(err(""), Some(CalcError::Syntax(Syntax::Empty, 0)));
        assert_eq!(err("2+"), Some(CalcError::Syntax(Syntax::MissingOperand, 2)));
        assert_eq!(err("2+)"), Some(CalcError::Syntax(Syntax::UnmatchedParen, 2)));
        assert_eq!(err("2)"), Some(CalcError::Syntax(Syntax::UnmatchedParen, 1)));
        assert_eq!(err("×3"), Some(CalcError::Syntax(Syntax::MissingOperand, 0)));
        assert_eq!(err("(2,3)"), Some(CalcError::Syntax(Syntax::Unexpected, 2)));
    }

    #[test]
    fn no_text_makes_it_panic() {
        // Every sequence of up to four of these pieces, and pseudo-random longer ones: each
        // parses or fails, and nothing in a tree reaches outside its text.
        const PIECES: [&str; 16] =
            ["1", "2.5", "+", "−", "×", "^", "(", ")", "sin(", "√", "!", "%", ",", "π", "mod", "²"];
        let check = |text: &str| {
            if let Ok(tokens) = tokenize(text, None)
                && let Ok(node) = parse(&tokens, text.len())
            {
                assert!(node.start <= node.end && node.end <= text.len(), "{text}");
            }
        };
        let mut text = String::new();
        for n in 0..PIECES.len().pow(4) {
            text.clear();
            let mut k = n;
            for _ in 0..4 {
                text.push_str(PIECES[k % PIECES.len()]);
                k /= PIECES.len();
            }
            check(&text);
        }
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..20_000 {
            text.clear();
            for _ in 0..(seed % 24) {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                text.push_str(PIECES[(seed % PIECES.len() as u64) as usize]);
            }
            check(&text);
        }
    }

    #[test]
    fn spans_cover_their_text() {
        let text = "12 + sin(30)";
        let node = parse(&tokenize(text, None).unwrap(), text.len()).unwrap();
        assert_eq!((node.start, node.end), (0, text.len()));
        let Expr::Bin(_, _, rhs) = node.expr else { panic!() };
        assert_eq!(&text[rhs.start..rhs.end], "sin(30)");
    }
}

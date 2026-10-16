// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Working expressions out: as numbers ([`evaluate`]), or as Programmer mode's whole numbers of
//! a word's size ([`evaluate_word`]).
//!
//! A percentage is a hundredth (`10%` is 0.1), except after `+` or `−`, where it is that share
//! of what it is added to or taken from, as on every calculator: `50 + 10%` is 55, and
//! `80 − 25%` is 60.

use alloc::vec::Vec;

use crate::lexer::Func;
use crate::number::{self, AngleUnit, CalcError, Number, Trig};
use crate::parser::{BinOp, Expr, Node, PostOp};

/// What an expression is worked out with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Env {
    pub angle: AngleUnit,
    /// The last result (`Ans`), zero before there is one.
    pub ans: Number,
}

impl Default for Env {
    fn default() -> Env {
        Env { angle: AngleUnit::Degrees, ans: Number::ZERO }
    }
}

/// Works out `node` as a number.
pub fn evaluate(node: &Node, env: &Env) -> Result<Number, CalcError> {
    match &node.expr {
        Expr::Num(n) => Ok(*n),
        Expr::Ans => Ok(env.ans),
        Expr::Neg(a) => Ok(evaluate(a, env)?.neg()),
        Expr::Not(_) => Err(CalcError::NotInMode),
        Expr::Post(op, a) => {
            let x = evaluate(a, env)?;
            match op {
                PostOp::Factorial => x.factorial(),
                PostOp::Percent => x.div(Number::int(100)),
                PostOp::Square => x.powi(2),
                PostOp::Cube => x.powi(3),
                PostOp::Reciprocal => Number::ONE.div(x),
            }
        }
        Expr::Bin(op, a, b) => {
            let x = evaluate(a, env)?;
            // `x ± y%` is x ± y% of x.
            if let (BinOp::Add | BinOp::Sub, Expr::Post(PostOp::Percent, p)) = (op, &b.expr) {
                let share = x.mul(evaluate(p, env)?)?.div(Number::int(100))?;
                return if *op == BinOp::Add { x.add(share) } else { x.sub(share) };
            }
            let y = evaluate(b, env)?;
            match op {
                BinOp::Add => x.add(y),
                BinOp::Sub => x.sub(y),
                BinOp::Mul => x.mul(y),
                BinOp::Div => x.div(y),
                BinOp::Pow => x.pow(y),
                BinOp::Mod => x.modulo(y),
                _ => Err(CalcError::NotInMode),
            }
        }
        Expr::Call(f, args) => {
            let values = args.iter().map(|a| evaluate(a, env)).collect::<Result<Vec<_>, _>>()?;
            call(*f, &values, env.angle)
        }
    }
}

/// Calls `f` with `args`.
fn call(f: Func, args: &[Number], angle: AngleUnit) -> Result<Number, CalcError> {
    let one = |args: &[Number]| match args {
        [x] => Ok(*x),
        _ => Err(CalcError::Arguments(f.name(), "one number")),
    };
    let two = |args: &[Number]| match args {
        [x, y] => Ok((*x, *y)),
        _ => Err(CalcError::Arguments(f.name(), "two numbers")),
    };
    match f {
        Func::Sin => one(args)?.trig(Trig::Sin, angle),
        Func::Cos => one(args)?.trig(Trig::Cos, angle),
        Func::Tan => one(args)?.trig(Trig::Tan, angle),
        Func::Asin => one(args)?.inverse_trig(Trig::Sin, angle),
        Func::Acos => one(args)?.inverse_trig(Trig::Cos, angle),
        Func::Atan => one(args)?.inverse_trig(Trig::Tan, angle),
        Func::Sinh | Func::Cosh | Func::Tanh | Func::Asinh | Func::Acosh | Func::Atanh => hyperbolic(f, one(args)?),
        Func::Ln => one(args)?.ln(),
        Func::Log => match args {
            [x] => x.log(Number::int(10)),
            [x, base] => x.log(*base),
            _ => Err(CalcError::Arguments("log", "one or two numbers")),
        },
        Func::Log2 => one(args)?.log(Number::int(2)),
        Func::Exp => one(args)?.exp(),
        Func::Sqrt => one(args)?.sqrt(),
        Func::Cbrt => one(args)?.root(3),
        Func::Root => {
            let (x, n) = two(args)?;
            match n.as_integer() {
                Some(n) => x.root(n),
                None if n.is_zero() => Err(CalcError::Domain),
                None => x.pow(Number::ONE.div(n)?),
            }
        }
        Func::Abs => Ok(one(args)?.abs()),
        Func::Floor => one(args)?.floor(),
        Func::Ceil => one(args)?.ceil(),
        Func::Round => one(args)?.round(),
        Func::Gcd => {
            let (x, y) = two(args)?;
            number::gcd(x, y)
        }
        Func::Lcm => {
            let (x, y) = two(args)?;
            number::lcm(x, y)
        }
        Func::NCr => {
            let (n, k) = two(args)?;
            number::choose(n, k, false)
        }
        Func::NPr => {
            let (n, k) = two(args)?;
            number::choose(n, k, true)
        }
    }
}

/// The hyperbolic functions and their inverses: exact at zero (and `acosh 1`), floats elsewhere.
fn hyperbolic(f: Func, x: Number) -> Result<Number, CalcError> {
    let v = x.to_f64();
    match f {
        Func::Sinh | Func::Tanh | Func::Asinh | Func::Atanh if x.is_zero() => Ok(Number::ZERO),
        Func::Cosh if x.is_zero() => Ok(Number::ONE),
        Func::Acosh if x == Number::ONE => Ok(Number::ZERO),
        Func::Sinh => number::approx(libm::sinh(v)),
        Func::Cosh => number::approx(libm::cosh(v)),
        Func::Tanh => number::approx(libm::tanh(v)),
        Func::Asinh => number::approx(libm::asinh(v)),
        Func::Acosh if v < 1.0 => Err(CalcError::Domain),
        Func::Acosh => number::approx(libm::acosh(v)),
        Func::Atanh if v.abs() >= 1.0 => Err(CalcError::Domain),
        Func::Atanh => number::approx(libm::atanh(v)),
        _ => Err(CalcError::Domain),
    }
}

/// The size of Programmer mode's whole numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WordSize {
    Byte,
    Word,
    Dword,
    #[default]
    Qword,
}

impl WordSize {
    pub const ALL: [WordSize; 4] = [WordSize::Qword, WordSize::Dword, WordSize::Word, WordSize::Byte];

    pub fn bits(self) -> u32 {
        match self {
            WordSize::Byte => 8,
            WordSize::Word => 16,
            WordSize::Dword => 32,
            WordSize::Qword => 64,
        }
    }

    /// The size's name, as its key shows it.
    pub fn label(self) -> &'static str {
        match self {
            WordSize::Byte => "BYTE",
            WordSize::Word => "WORD",
            WordSize::Dword => "DWORD",
            WordSize::Qword => "QWORD",
        }
    }

    /// The next smaller size, round to the largest, as the key cycles through them.
    pub fn next(self) -> WordSize {
        match self {
            WordSize::Qword => WordSize::Dword,
            WordSize::Dword => WordSize::Word,
            WordSize::Word => WordSize::Byte,
            WordSize::Byte => WordSize::Qword,
        }
    }

    /// `v` cut to this size and read as a signed number (two's complement).
    pub fn wrap(self, v: i128) -> i64 {
        let shift = 128 - self.bits();
        ((v << shift) >> shift) as i64
    }

    /// The bits of `v` in this size, as an unsigned number.
    pub fn unsigned(self, v: i64) -> u64 {
        (v as u64) & (u64::MAX >> (64 - self.bits()))
    }
}

/// Works out `node` as a whole number of `size` (overflows wrap round, as in a processor's
/// registers); `ans` is the last result.
pub fn evaluate_word(node: &Node, size: WordSize, ans: i64) -> Result<i64, CalcError> {
    let bits = size.bits();
    let wrap = |v: i128| size.wrap(v);
    Ok(match &node.expr {
        Expr::Num(n) => wrap(n.as_integer().ok_or(CalcError::NotWhole)?),
        Expr::Ans => wrap(ans as i128),
        Expr::Neg(a) => wrap(-(evaluate_word(a, size, ans)? as i128)),
        Expr::Not(a) => wrap(!(evaluate_word(a, size, ans)? as i128)),
        Expr::Post(op, a) => {
            let x = evaluate_word(a, size, ans)? as i128;
            match op {
                PostOp::Square => wrap(x.wrapping_mul(x)),
                PostOp::Cube => wrap(wrap(x.wrapping_mul(x)) as i128 * x),
                PostOp::Reciprocal | PostOp::Percent => return Err(CalcError::NotWhole),
                PostOp::Factorial => return Err(CalcError::NotInMode),
            }
        }
        Expr::Bin(op, a, b) => {
            let x = evaluate_word(a, size, ans)?;
            let y = evaluate_word(b, size, ans)?;
            let (xi, yi) = (x as i128, y as i128);
            let ux = size.unsigned(x);
            // A shift or rotation by a count beyond the size, or negative, by its bits mod the size.
            let count = (size.unsigned(y) % bits as u64) as u32;
            match op {
                BinOp::Add => wrap(xi + yi),
                BinOp::Sub => wrap(xi - yi),
                BinOp::Mul => wrap(xi * yi),
                BinOp::Div if y == 0 => return Err(CalcError::DivisionByZero),
                BinOp::Div => wrap(xi / yi),
                BinOp::Mod if y == 0 => return Err(CalcError::DivisionByZero),
                // The remainder with the sign of the dividend, as processors divide.
                BinOp::Mod => wrap(xi % yi),
                BinOp::Pow if y < 0 => return Err(CalcError::NotWhole),
                BinOp::Pow => {
                    let mut acc: i64 = 1;
                    let (mut base, mut e) = (x, y as u64);
                    while e > 0 {
                        if e & 1 == 1 {
                            acc = wrap(acc as i128 * base as i128);
                        }
                        base = wrap(base as i128 * base as i128);
                        e >>= 1;
                    }
                    acc
                }
                BinOp::And => wrap(xi & yi),
                BinOp::Or => wrap(xi | yi),
                BinOp::Xor => wrap(xi ^ yi),
                BinOp::Nand => wrap(!(xi & yi)),
                BinOp::Nor => wrap(!(xi | yi)),
                // Left shifts fill with zeros; right shifts keep the sign (arithmetic). Shifts
                // by the whole size or more empty the number (or fill it with its sign).
                BinOp::Shl if y < 0 || y as u64 >= bits as u64 => 0,
                BinOp::Shl => wrap(xi << y),
                BinOp::Shr if y < 0 || y as u64 >= bits as u64 => {
                    if x < 0 {
                        -1
                    } else {
                        0
                    }
                }
                BinOp::Shr => x >> y,
                BinOp::Rol => wrap(rotate(ux, count, bits, true) as i128),
                BinOp::Ror => wrap(rotate(ux, count, bits, false) as i128),
            }
        }
        Expr::Call(..) => return Err(CalcError::NotInMode),
    })
}

/// `v` (of `bits` bits) rotated by `count` bits, left or right.
fn rotate(v: u64, count: u32, bits: u32, left: bool) -> u64 {
    if count == 0 {
        return v;
    }
    let mask = u64::MAX >> (64 - bits);
    let count = if left { count } else { bits - count };
    ((v << count) | (v >> (bits - count))) & mask
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::parse;

    fn eval_with(text: &str, env: &Env) -> Result<Number, CalcError> {
        let tokens = tokenize(text, None)?;
        evaluate(&parse(&tokens, text.len())?, env)
    }

    fn eval(text: &str) -> Result<Number, CalcError> {
        eval_with(text, &Env::default())
    }

    fn q(n: i128, d: i128) -> Number {
        Number::fraction(n, d).unwrap()
    }

    fn near(text: &str, x: f64) {
        let v = eval(text).unwrap().to_f64();
        assert!((v - x).abs() <= 1e-12 * x.abs().max(1.0), "{text} = {v}, not {x}");
    }

    #[test]
    fn arithmetic() {
        assert_eq!(eval("0.1+0.2").unwrap(), q(3, 10));
        assert_eq!(eval("2+3×4").unwrap(), Number::int(14));
        assert_eq!(eval("(2+3)×4").unwrap(), Number::int(20));
        assert_eq!(eval("1÷3×3").unwrap(), Number::ONE);
        assert_eq!(eval("2^10").unwrap(), Number::int(1024));
        assert_eq!(eval("−2²").unwrap(), Number::int(-4));
        assert_eq!(eval("(−2)²").unwrap(), Number::int(4));
        assert_eq!(eval("5!").unwrap(), Number::int(120));
        assert_eq!(eval("4⁻¹").unwrap(), q(1, 4));
        assert_eq!(eval("7 mod 3").unwrap(), Number::int(1));
        assert_eq!(eval("1÷0"), Err(CalcError::DivisionByZero));
        assert_eq!(eval("√−4"), Err(CalcError::Domain));
    }

    #[test]
    fn percentages_as_calculators_have_them() {
        assert_eq!(eval("50+10%").unwrap(), Number::int(55));
        assert_eq!(eval("80−25%").unwrap(), Number::int(60));
        assert_eq!(eval("200×10%").unwrap(), Number::int(20));
        assert_eq!(eval("10%").unwrap(), q(1, 10));
        assert_eq!(eval("50÷10%").unwrap(), Number::int(500));
    }

    #[test]
    fn functions() {
        assert_eq!(eval("sin(30)").unwrap(), q(1, 2));
        assert_eq!(eval("sin⁻¹(0.5)").unwrap(), Number::int(30));
        let rad = Env { angle: AngleUnit::Radians, ..Env::default() };
        assert_eq!(eval_with("sin(π)", &rad).unwrap(), Number::ZERO);
        assert_eq!(eval_with("cos(2π÷3)", &rad).unwrap(), q(-1, 2));
        assert_eq!(eval("log(1000)").unwrap(), Number::int(3));
        assert_eq!(eval("log(8, 2)").unwrap(), Number::int(3));
        assert_eq!(eval("log₂(1024)").unwrap(), Number::int(10));
        assert_eq!(eval("³√27").unwrap(), Number::int(3));
        assert_eq!(eval("root(32, 5)").unwrap(), Number::int(2));
        assert_eq!(eval("abs(−3.5)").unwrap(), q(7, 2));
        assert_eq!(eval("nCr(5, 2)").unwrap(), Number::int(10));
        assert_eq!(eval("gcd(12, 18)").unwrap(), Number::int(6));
        assert_eq!(eval("floor(−2.5)").unwrap(), Number::int(-3));
        assert_eq!(eval("round(2.5)").unwrap(), Number::int(3));
        assert_eq!(eval("cosh(0)").unwrap(), Number::ONE);
        near("ln(e)", 1.0);
        near("e^2", libm::exp(2.0));
        near("√2", core::f64::consts::SQRT_2);
        near("sinh(1)", libm::sinh(1.0));
        assert_eq!(eval("atanh(1)"), Err(CalcError::Domain));
        assert_eq!(eval("sin(1, 2)"), Err(CalcError::Arguments("sin", "one number")));
        assert_eq!(eval("gcd(4)"), Err(CalcError::Arguments("gcd", "two numbers")));
    }

    #[test]
    fn ans_is_the_last_result() {
        let env = Env { ans: q(1, 3), ..Env::default() };
        assert_eq!(eval_with("Ans×3", &env).unwrap(), Number::ONE);
        assert_eq!(eval_with("2Ans", &env).unwrap(), q(2, 3));
        assert_eq!(eval("Ans+1").unwrap(), Number::ONE);
    }

    #[test]
    fn bitwise_operators_belong_to_programmer_mode() {
        assert_eq!(eval("5 and 3"), Err(CalcError::NotInMode));
    }

    fn word(text: &str, size: WordSize) -> Result<i64, CalcError> {
        let tokens = tokenize(text, Some(16))?;
        evaluate_word(&parse(&tokens, text.len())?, size, 0)
    }

    #[test]
    fn whole_numbers_of_a_size() {
        use WordSize::*;
        assert_eq!(word("FF + 1", Byte), Ok(0));
        assert_eq!(word("7F + 1", Byte), Ok(-128));
        assert_eq!(word("FF", Byte), Ok(-1));
        assert_eq!(word("FF", Word), Ok(255));
        assert_eq!(word("F0 AND 3C", Qword), Ok(0x30));
        assert_eq!(word("F0 OR 0F", Byte), Ok(-1));
        assert_eq!(word("F0 XOR FF", Qword), Ok(0x0F));
        assert_eq!(word("NOT 0", Dword), Ok(-1));
        assert_eq!(word("F NAND 3", Byte), Ok(!3));
        assert_eq!(word("1 << 4", Qword), Ok(16));
        assert_eq!(word("1 << 40", Dword), Ok(0));
        assert_eq!(word("80 >> 3", Byte), Ok(-16), "arithmetic: the sign stays");
        assert_eq!(word("81 ROL 1", Byte), Ok(3));
        assert_eq!(word("81 ROR 1", Byte), Ok(-64));
        assert_eq!(word("A ÷ 3", Qword), Ok(3));
        assert_eq!(word("−A mod 3", Qword), Ok(-1));
        assert_eq!(word("A ÷ 0", Qword), Err(CalcError::DivisionByZero));
        assert_eq!(word("2^A", Word), Ok(1024));
        assert_eq!(word("2^10", Word), Ok(0), "2^16 overflows a word");
        assert_eq!(word("10²", Qword), Ok(256));
        assert!(word("sin 1", Qword).is_err());
        assert_eq!(Qword.wrap(i64::MAX as i128 + 1), i64::MIN);
        assert_eq!(Byte.unsigned(-1), 255);
        assert_eq!(Qword.unsigned(-1), u64::MAX);
    }

    #[test]
    fn rotations() {
        assert_eq!(rotate(0b1000_0001, 1, 8, true), 0b0000_0011);
        assert_eq!(rotate(0b1000_0001, 1, 8, false), 0b1100_0000);
        assert_eq!(rotate(1, 63, 64, true), 1 << 63);
        assert_eq!(rotate(0xAB, 0, 8, true), 0xAB);
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Numbers as the Calculator keeps them: exact where it can be, approximate where it must.
//!
//! * [`Number::Exact`] is a fraction of two 128-bit integers (num-rational's [`Ratio`]). Sums,
//!   differences, products, quotients and whole powers of fractions are fractions, so 0.1 + 0.2
//!   is 3/10, exactly 0.3, and 1/3 × 3 is exactly 1.
//! * [`Number::Pi`] is a fraction times π, so that the sine of π is 0, the cosine of π/3 is 1/2
//!   and the arcsine of 1/2 is π/6, as they are.
//! * [`Number::Approx`] is a 64-bit float, good to 15 significant digits: most roots,
//!   logarithms and sines, and fractions too large for 128 bits.
//!
//! Exact arithmetic never overflows silently: an operation whose exact result does not fit (a
//! numerator beyond 128 bits, a denominator beyond [`MAX_DENOMINATOR`]) gives the float instead.
//! Every operation returns a [`Number`] or a [`CalcError`]; none panics, whatever it is given.

use core::cmp::Ordering;
use core::fmt;

use num_integer::{Integer, Roots};
use num_rational::Ratio;
use num_traits::{CheckedAdd, CheckedDiv, CheckedMul, CheckedSub, One, Signed, Zero};

/// An exact fraction.
pub type Rational = Ratio<i128>;

/// The largest denominator an exact number keeps: decimal digits are found by long division,
/// whose remainders times ten must fit in 128 bits.
pub const MAX_DENOMINATOR: i128 = 1_000_000_000_000_000_000_000_000_000_000_000_000;

/// The largest whole power computed exactly (beyond it, the float).
const MAX_EXACT_EXPONENT: i128 = 4096;
/// The largest root taken exactly (the 64th root of a fraction).
const MAX_EXACT_ROOT: i128 = 64;
/// The largest factorial computed exactly (33! < 2^127 < 34!).
const MAX_EXACT_FACTORIAL: i128 = 33;
/// The largest factorial a float holds (171! overflows).
const MAX_FACTORIAL: i128 = 170;

/// Why a calculation has no result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalcError {
    /// The expression is not well formed: what is wrong, and where (a byte offset in its text).
    Syntax(Syntax, usize),
    /// A division (or remainder) by zero.
    DivisionByZero,
    /// An argument outside the function's domain: the square root of a negative number, the
    /// logarithm of zero, the arcsine of 2.
    Domain,
    /// A result that is not defined: the tangent of 90°.
    Undefined,
    /// A result too large for the calculator.
    Overflow,
    /// A function that takes whole numbers was given another (the factorial of −1, gcd of 2.5).
    NotInteger,
    /// A function called with the wrong number of arguments: its name and how many it takes.
    Arguments(&'static str, &'static str),
    /// Programmer mode works with whole numbers only.
    NotWhole,
    /// Something only one mode has: a function in Programmer mode, a bitwise operator in another.
    NotInMode,
}

/// What is wrong with an expression's form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// There is nothing to calculate.
    Empty,
    /// An operator lacks its number ("2 +").
    MissingOperand,
    /// A closing parenthesis with no opening one.
    UnmatchedParen,
    /// Something where it cannot be ("2 )", "× 3").
    Unexpected,
    /// A word the calculator does not know.
    UnknownName,
    /// A malformed number ("1.2.3", "1E").
    BadNumber,
    /// A digit the current base has not (9 in binary).
    BadDigit,
}

impl fmt::Display for CalcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CalcError::Syntax(what, _) => f.write_str(match what {
                Syntax::Empty => "Enter a calculation",
                Syntax::MissingOperand => "Missing a number",
                Syntax::UnmatchedParen => "Unmatched parenthesis",
                Syntax::Unexpected => "Invalid expression",
                Syntax::UnknownName => "Unknown function or name",
                Syntax::BadNumber => "Invalid number",
                Syntax::BadDigit => "Not a digit in this base",
            }),
            CalcError::DivisionByZero => f.write_str("Can't divide by zero"),
            CalcError::Domain => f.write_str("Invalid input"),
            CalcError::Undefined => f.write_str("Result is undefined"),
            CalcError::Overflow => f.write_str("Overflow"),
            CalcError::NotInteger => f.write_str("Needs a whole number"),
            CalcError::Arguments(name, takes) => write!(f, "{name} takes {takes}"),
            CalcError::NotWhole => f.write_str("Programmer mode uses whole numbers"),
            CalcError::NotInMode => f.write_str("Not available in this mode"),
        }
    }
}

/// How angles are measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AngleUnit {
    #[default]
    Degrees,
    Radians,
    Gradians,
}

impl AngleUnit {
    /// The unit's short name, as its key shows it ("DEG").
    pub fn label(self) -> &'static str {
        match self {
            AngleUnit::Degrees => "DEG",
            AngleUnit::Radians => "RAD",
            AngleUnit::Gradians => "GRAD",
        }
    }

    /// The unit's name ("degrees").
    pub fn name(self) -> &'static str {
        match self {
            AngleUnit::Degrees => "degrees",
            AngleUnit::Radians => "radians",
            AngleUnit::Gradians => "gradians",
        }
    }

    /// The next unit, as the key cycles through them.
    pub fn next(self) -> AngleUnit {
        match self {
            AngleUnit::Degrees => AngleUnit::Radians,
            AngleUnit::Radians => AngleUnit::Gradians,
            AngleUnit::Gradians => AngleUnit::Degrees,
        }
    }

    /// A whole turn in this unit, for units that measure turns in fractions (`None` for radians).
    fn turn(self) -> Option<i128> {
        match self {
            AngleUnit::Degrees => Some(360),
            AngleUnit::Gradians => Some(400),
            AngleUnit::Radians => None,
        }
    }
}

/// A trigonometric function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trig {
    Sin,
    Cos,
    Tan,
}

/// A number: exact, an exact multiple of π, or approximate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Number {
    /// Exactly this fraction (reduced, its denominator positive and at most [`MAX_DENOMINATOR`]).
    Exact(Rational),
    /// Exactly this fraction times π (kept as [`Number::Exact`]'s are).
    Pi(Rational),
    /// About this much (finite, never −0).
    Approx(f64),
}

/// A fraction that may be kept exactly: reduced (as [`Ratio`]'s always are), not too fine, and
/// whose numerator negates.
fn keepable(r: Rational) -> Option<Rational> {
    (*r.denom() <= MAX_DENOMINATOR && *r.numer() != i128::MIN).then_some(r)
}

/// The float nearest a fraction.
fn ratio_f64(r: &Rational) -> f64 {
    *r.numer() as f64 / *r.denom() as f64
}

/// `n / d` reduced, its denominator positive; `None` for a zero denominator, or where reducing
/// would negate `i128::MIN`.
fn ratio(n: i128, d: i128) -> Option<Rational> {
    if d == 0 {
        return None;
    }
    // In unsigned arithmetic, where `i128::MIN`'s magnitude has a value.
    let g = i128::try_from(n.unsigned_abs().gcd(&d.unsigned_abs())).ok()?;
    let (n, d) = (n / g, d / g);
    if d < 0 { Some(Ratio::new_raw(n.checked_neg()?, d.checked_neg()?)) } else { Some(Ratio::new_raw(n, d)) }
}

/// The largest whole number at most `r` (num-rational's `floor` overflows near `i128::MIN`).
fn floor_ratio(r: Rational) -> Rational {
    Ratio::from_integer(Integer::div_floor(r.numer(), r.denom()))
}

/// The smallest whole number at least `r`, for a kept fraction (whose numerator negates).
fn ceil_ratio(r: Rational) -> Rational {
    Ratio::from_integer(-Integer::div_floor(&-r.numer(), r.denom()))
}

/// The whole number nearest a kept fraction, halves away from zero.
fn round_ratio(r: Rational) -> Option<Rational> {
    let half = Ratio::new_raw(1, 2);
    if r.is_negative() {
        Some(-floor_ratio((-r).checked_add(&half)?))
    } else {
        Some(floor_ratio(r.checked_add(&half)?))
    }
}

/// The result of a float computation: a number, or why there is none.
pub fn approx(x: f64) -> Result<Number, CalcError> {
    if x.is_nan() {
        Err(CalcError::Domain)
    } else if x.is_infinite() {
        Err(CalcError::Overflow)
    } else if x == 0.0 {
        // Never −0.
        Ok(Number::Approx(0.0))
    } else {
        Ok(Number::Approx(x))
    }
}

// Arithmetic returns a `Result` (an operation may have no result), so it cannot be the
// operator traits'.
#[allow(clippy::should_implement_trait)]
impl Number {
    pub const ZERO: Number = Number::Exact(Ratio::new_raw(0, 1));
    pub const ONE: Number = Number::Exact(Ratio::new_raw(1, 1));
    /// π, exactly.
    pub const PI: Number = Number::Pi(Ratio::new_raw(1, 1));
    /// Euler's number, as a float.
    pub const E: Number = Number::Approx(core::f64::consts::E);

    /// The whole number `n` (the float for `i128::MIN`).
    pub fn int(n: i128) -> Number {
        Number::exact(Ratio::from_integer(n))
    }

    /// `n / d`, exactly where it can be; `None` if `d` is zero.
    pub fn fraction(n: i128, d: i128) -> Option<Number> {
        if d == 0 {
            return None;
        }
        Some(Number::exact_or(ratio(n, d), || n as f64 / d as f64))
    }

    /// The fraction `r` exactly, or its float if it cannot be kept.
    pub fn exact(r: Rational) -> Number {
        match keepable(r) {
            Some(r) => Number::Exact(r),
            None => Number::Approx(ratio_f64(&r)),
        }
    }

    /// The fraction an exact operation gave, or the float `fallback` gives if it overflowed or
    /// cannot be kept.
    fn exact_or(r: Option<Rational>, fallback: impl FnOnce() -> f64) -> Number {
        match r.and_then(keepable) {
            Some(r) => Number::Exact(r),
            None => Number::Approx(fallback()),
        }
    }

    /// As [`Number::exact_or`], for multiples of π.
    fn pi_or(r: Option<Rational>, fallback: impl FnOnce() -> f64) -> Number {
        match r.and_then(keepable) {
            Some(r) if r.is_zero() => Number::ZERO,
            Some(r) => Number::Pi(r),
            None => Number::Approx(fallback()),
        }
    }

    /// The float `x` (`Domain` for NaN, `Overflow` for infinities).
    pub fn from_f64(x: f64) -> Result<Number, CalcError> {
        approx(x)
    }

    /// The nearest float.
    pub fn to_f64(self) -> f64 {
        match self {
            Number::Exact(r) => ratio_f64(&r),
            Number::Pi(r) => ratio_f64(&r) * core::f64::consts::PI,
            Number::Approx(x) => x,
        }
    }

    /// Whether the number is known exactly.
    pub fn is_exact(self) -> bool {
        !matches!(self, Number::Approx(_))
    }

    /// The fraction, if the number is exact and rational.
    pub fn rational(self) -> Option<Rational> {
        match self {
            Number::Exact(r) => Some(r),
            _ => None,
        }
    }

    /// The whole number, if the number is exactly one.
    pub fn as_integer(self) -> Option<i128> {
        match self {
            Number::Exact(r) if r.is_integer() => Some(*r.numer()),
            _ => None,
        }
    }

    /// The whole number nearest the number if it is one (exactly, or a float without a fraction
    /// and within 2^53, where floats count exactly).
    pub fn whole(self) -> Option<i128> {
        match self {
            Number::Approx(x) if libm::trunc(x) == x && libm::fabs(x) <= 9_007_199_254_740_992.0 => Some(x as i128),
            n => n.as_integer(),
        }
    }

    pub fn is_zero(self) -> bool {
        match self {
            Number::Exact(r) | Number::Pi(r) => r.is_zero(),
            Number::Approx(x) => x == 0.0,
        }
    }

    pub fn is_negative(self) -> bool {
        match self {
            Number::Exact(r) | Number::Pi(r) => r.is_negative(),
            Number::Approx(x) => x < 0.0,
        }
    }

    /// −self.
    pub fn neg(self) -> Number {
        match self {
            // Kept fractions never hold `i128::MIN`, so they negate.
            Number::Exact(r) => Number::Exact(-r),
            Number::Pi(r) => Number::Pi(-r),
            Number::Approx(x) => Number::Approx(if x == 0.0 { 0.0 } else { -x }),
        }
    }

    /// |self|.
    pub fn abs(self) -> Number {
        if self.is_negative() { self.neg() } else { self }
    }

    pub fn add(self, other: Number) -> Result<Number, CalcError> {
        use Number::*;
        Ok(match (self, other) {
            (Exact(a), Exact(b)) => Number::exact_or(a.checked_add(&b), || ratio_f64(&a) + ratio_f64(&b)),
            (Pi(a), Pi(b)) => Number::pi_or(a.checked_add(&b), || self.to_f64() + other.to_f64()),
            (Exact(z), n @ Pi(_)) | (n @ Pi(_), Exact(z)) if z.is_zero() => n,
            _ => return approx(self.to_f64() + other.to_f64()),
        })
    }

    pub fn sub(self, other: Number) -> Result<Number, CalcError> {
        self.add(other.neg())
    }

    pub fn mul(self, other: Number) -> Result<Number, CalcError> {
        use Number::*;
        Ok(match (self, other) {
            (Exact(a), Exact(b)) => Number::exact_or(a.checked_mul(&b), || ratio_f64(&a) * ratio_f64(&b)),
            (Exact(a), Pi(b)) | (Pi(b), Exact(a)) => {
                Number::pi_or(a.checked_mul(&b), || self.to_f64() * other.to_f64())
            }
            _ => return approx(self.to_f64() * other.to_f64()),
        })
    }

    pub fn div(self, other: Number) -> Result<Number, CalcError> {
        use Number::*;
        if other.is_zero() {
            return Err(CalcError::DivisionByZero);
        }
        Ok(match (self, other) {
            (Exact(a), Exact(b)) | (Pi(a), Pi(b)) => {
                Number::exact_or(a.checked_div(&b), || self.to_f64() / other.to_f64())
            }
            (Pi(a), Exact(b)) => Number::pi_or(a.checked_div(&b), || self.to_f64() / other.to_f64()),
            _ => return approx(self.to_f64() / other.to_f64()),
        })
    }

    /// The remainder of `self / other` with the sign of `other` (floored, as mathematicians
    /// define `mod`): 7 mod 3 = 1, −7 mod 3 = 2, 7 mod −3 = −2.
    pub fn modulo(self, other: Number) -> Result<Number, CalcError> {
        if other.is_zero() {
            return Err(CalcError::DivisionByZero);
        }
        if let (Number::Exact(a), Number::Exact(b)) = (self, other)
            && let Some(q) = a.checked_div(&b)
            && let Some(m) = b.checked_mul(&floor_ratio(q)).and_then(|bq| a.checked_sub(&bq))
            && let Some(m) = keepable(m)
        {
            return Ok(Number::Exact(m));
        }
        let (a, b) = (self.to_f64(), other.to_f64());
        let m = a - b * libm::floor(a / b);
        // A remainder rounded up to the divisor is none.
        approx(if m == b { 0.0 } else { m })
    }

    /// `self` to the power `exp`: exact for fractions to whole powers, and to fractional powers
    /// where the root is exact (8^(1/3) = 2, (−8)^(1/3) = −2); otherwise the float.
    pub fn pow(self, exp: Number) -> Result<Number, CalcError> {
        if let Some(n) = exp.as_integer() {
            return self.powi(n);
        }
        if let (Number::Exact(base), Number::Exact(e)) = (self, exp) {
            // A fractional power p/q: the q-th root, to the p-th power.
            let (p, q) = (*e.numer(), *e.denom());
            if base.is_negative() && q % 2 == 0 {
                return Err(CalcError::Domain);
            }
            if let Some(root) = exact_root(base, q) {
                return Number::Exact(root).powi(p);
            }
            let magnitude = libm::pow(libm::fabs(ratio_f64(&base)), ratio_f64(&e));
            // An odd root of a negative number is negative, and so is its odd power.
            let negative = base.is_negative() && p % 2 != 0;
            return approx(if negative { -magnitude } else { magnitude });
        }
        let (b, x) = (self.to_f64(), exp.to_f64());
        if b == core::f64::consts::E {
            return approx(libm::exp(x));
        }
        if b < 0.0 {
            // A negative base takes only whole powers (above), or odd roots of exact powers.
            return Err(CalcError::Domain);
        }
        if b == 0.0 && x < 0.0 {
            return Err(CalcError::DivisionByZero);
        }
        approx(libm::pow(b, x))
    }

    /// `self` to the whole power `n`.
    pub fn powi(self, n: i128) -> Result<Number, CalcError> {
        if n == 0 {
            // 0^0 is 1, as most calculators and the IEEE functions have it.
            return Ok(Number::ONE);
        }
        if self.is_zero() && n < 0 {
            return Err(CalcError::DivisionByZero);
        }
        if self == Number::ONE {
            return Ok(Number::ONE);
        }
        if self == Number::int(-1) {
            return Ok(Number::int(if n % 2 == 0 { 1 } else { -1 }));
        }
        match self {
            Number::Exact(r) if n.unsigned_abs() <= MAX_EXACT_EXPONENT as u128 => {
                let mut result = Some(Ratio::one());
                let (mut base, mut e) = (Some(r), n.unsigned_abs());
                // Squaring until it overflows; a result that overflows is the float's.
                while e > 0 {
                    if e & 1 == 1 {
                        result = result.zip(base).and_then(|(acc, b)| acc.checked_mul(&b)).and_then(keepable);
                    }
                    e >>= 1;
                    if e > 0 {
                        base = base.and_then(|b| b.checked_mul(&b)).and_then(keepable);
                    }
                }
                let result = if n < 0 { result.and_then(|r| Ratio::one().checked_div(&r)) } else { result };
                match result.and_then(keepable) {
                    Some(r) => Ok(Number::Exact(r)),
                    None => approx(libm::pow(ratio_f64(&r), n as f64)),
                }
            }
            Number::Pi(_) if n == 1 => Ok(self),
            _ => approx(libm::pow(self.to_f64(), n as f64)),
        }
    }

    /// The square root (exact for squares of fractions).
    pub fn sqrt(self) -> Result<Number, CalcError> {
        if self.is_negative() {
            return Err(CalcError::Domain);
        }
        self.root(2)
    }

    /// The real `n`-th root: of a negative number only for odd `n`.
    pub fn root(self, n: i128) -> Result<Number, CalcError> {
        if n == 0 {
            return Err(CalcError::Domain);
        }
        if n < 0 {
            return Number::ONE.div(self.root(-n)?);
        }
        if self.is_negative() && n % 2 == 0 {
            return Err(CalcError::Domain);
        }
        if let Number::Exact(r) = self
            && let Some(root) = exact_root(r, n)
        {
            return Ok(Number::Exact(root));
        }
        let x = self.to_f64();
        let magnitude = match n {
            2 => libm::sqrt(libm::fabs(x)),
            3 => libm::cbrt(libm::fabs(x)),
            _ => libm::pow(libm::fabs(x), 1.0 / n as f64),
        };
        approx(if x < 0.0 { -magnitude } else { magnitude })
    }

    /// `self!`: exact up to 33!, a float up to 170!, the gamma function's Γ(x + 1) between
    /// whole numbers. Negative whole numbers have none.
    pub fn factorial(self) -> Result<Number, CalcError> {
        if let Some(n) = self.whole() {
            if n < 0 {
                return Err(CalcError::Domain);
            }
            if n > MAX_FACTORIAL {
                return Err(CalcError::Overflow);
            }
            if n <= MAX_EXACT_FACTORIAL {
                return Ok(Number::int((1..=n).product()));
            }
            return approx((1..=n).fold(1.0, |acc, k| acc * k as f64));
        }
        let x = self.to_f64();
        if x < 0.0 && libm::trunc(x) == x {
            return Err(CalcError::Domain);
        }
        approx(libm::tgamma(x + 1.0))
    }

    /// The number rounded down to a whole number.
    pub fn floor(self) -> Result<Number, CalcError> {
        match self {
            Number::Exact(r) => Ok(Number::exact(floor_ratio(r))),
            _ => approx(libm::floor(self.to_f64())),
        }
    }

    /// The number rounded up to a whole number.
    pub fn ceil(self) -> Result<Number, CalcError> {
        match self {
            Number::Exact(r) => Ok(Number::exact(ceil_ratio(r))),
            _ => approx(libm::ceil(self.to_f64())),
        }
    }

    /// The number rounded to the nearest whole number, halves away from zero.
    pub fn round(self) -> Result<Number, CalcError> {
        match self {
            Number::Exact(r) => match round_ratio(r) {
                Some(r) => Ok(Number::exact(r)),
                None => approx(libm::round(ratio_f64(&r))),
            },
            _ => approx(libm::round(self.to_f64())),
        }
    }

    /// The natural logarithm.
    pub fn ln(self) -> Result<Number, CalcError> {
        if !self.is_positive() {
            return Err(CalcError::Domain);
        }
        if self == Number::ONE {
            return Ok(Number::ZERO);
        }
        approx(libm::log(self.to_f64()))
    }

    /// The logarithm to base `b` (exact where `self` is a whole power of a whole base: log 1000
    /// = 3, log₂ 1/8 = −3).
    pub fn log(self, b: Number) -> Result<Number, CalcError> {
        if !self.is_positive() || !b.is_positive() || b == Number::ONE {
            return Err(CalcError::Domain);
        }
        if let (Number::Exact(x), Some(base)) = (self, b.as_integer())
            && let Some(k) = exact_log(x, base)
        {
            return Ok(Number::int(k));
        }
        let x = self.to_f64();
        approx(match b.as_integer() {
            Some(10) => libm::log10(x),
            Some(2) => libm::log2(x),
            _ => libm::log(x) / libm::log(b.to_f64()),
        })
    }

    pub fn is_positive(self) -> bool {
        !self.is_negative() && !self.is_zero()
    }

    /// The exponential, e^self.
    pub fn exp(self) -> Result<Number, CalcError> {
        if self.is_zero() {
            return Ok(Number::ONE);
        }
        approx(libm::exp(self.to_f64()))
    }

    /// The angle in turns, exactly, where it is a fraction of a turn: degrees and gradians that
    /// are fractions, and multiples of π in radians.
    fn turns(self, unit: AngleUnit) -> Option<Rational> {
        match (self, unit.turn()) {
            (Number::Exact(r), Some(turn)) => r.checked_div(&Ratio::from_integer(turn)),
            (Number::Exact(r), None) if r.is_zero() => Some(r),
            (Number::Pi(r), None) => r.checked_div(&Ratio::from_integer(2)),
            _ => None,
        }
    }

    /// The angle in radians, as a float.
    fn radians(self, unit: AngleUnit) -> f64 {
        let x = self.to_f64();
        match unit {
            AngleUnit::Degrees => x * (core::f64::consts::PI / 180.0),
            AngleUnit::Gradians => x * (core::f64::consts::PI / 200.0),
            AngleUnit::Radians => x,
        }
    }

    /// The sine, cosine or tangent of the angle `self` measured in `unit`: exact at the angles
    /// whose values are fractions (sin 30° = 1/2, cos π = −1, tan 45° = 1), undefined where the
    /// tangent is (90°), and computed from the angle within one turn where it is a fraction of
    /// one, so that sin(10^20 °) is as accurate as sin(40°).
    pub fn trig(self, f: Trig, unit: AngleUnit) -> Result<Number, CalcError> {
        // The angle within one turn, from 0 to 1.
        let Some(t) = self.turns(unit).and_then(|turns| turns.checked_sub(&floor_ratio(turns))) else {
            let x = self.radians(unit);
            return approx(match f {
                Trig::Sin => libm::sin(x),
                Trig::Cos => libm::cos(x),
                Trig::Tan => libm::tan(x),
            });
        };
        // The angle in twelfths and eighths of a turn, where it is a whole number of them.
        let parts = |n: i128| t.checked_mul(&Ratio::from_integer(n)).filter(|p| p.is_integer()).map(|p| *p.numer());
        let (twelfths, eighths) = (parts(12), parts(8));
        // Sines of every twelfth of a turn whose sine is a fraction (0°, 30°, 90°, ...).
        let sine_at = |k: i128| -> Option<Rational> {
            match k.rem_euclid(12) {
                0 | 6 => Some(Ratio::zero()),
                1 | 5 => Some(Ratio::new_raw(1, 2)),
                3 => Some(Ratio::one()),
                7 | 11 => Some(Ratio::new_raw(-1, 2)),
                9 => Some(-Ratio::one()),
                _ => None,
            }
        };
        let exact = match f {
            Trig::Sin => twelfths.and_then(sine_at),
            Trig::Cos => twelfths.and_then(|k| sine_at(k + 3)),
            Trig::Tan => match eighths.map(|k| k.rem_euclid(8)) {
                Some(0 | 4) => Some(Ratio::zero()),
                Some(1 | 5) => Some(Ratio::one()),
                Some(3 | 7) => Some(-Ratio::one()),
                Some(_) => return Err(CalcError::Undefined),
                None => None,
            },
        };
        if let Some(v) = exact {
            return Ok(Number::Exact(v));
        }
        // Within the first half turn, about zero, where the float's angle is most accurate.
        let half = if t > Ratio::new_raw(1, 2) { t - Ratio::one() } else { t };
        let x = ratio_f64(&half) * core::f64::consts::TAU;
        approx(match f {
            Trig::Sin => libm::sin(x),
            Trig::Cos => libm::cos(x),
            Trig::Tan => libm::tan(x),
        })
    }

    /// The angle (in `unit`) whose sine, cosine or tangent is `self`: exact for the values whose
    /// angles are (asin ½ = 30°, or π/6 in radians).
    pub fn inverse_trig(self, f: Trig, unit: AngleUnit) -> Result<Number, CalcError> {
        let x = self.to_f64();
        if f != Trig::Tan && !(-1.0..=1.0).contains(&x) {
            return Err(CalcError::Domain);
        }
        // The angle in turns, for the values whose angles are fractions of one: the sines (and
        // so the cosines, a quarter turn on) that are fractions, and tan's 0 and ±1.
        let turns = match (f, self.rational()) {
            (Trig::Sin, Some(r)) => sine_twelfths(r).map(|k| Ratio::new(k, 12)),
            (Trig::Cos, Some(r)) => sine_twelfths(r).map(|k| Ratio::new(3 - k, 12)),
            (Trig::Tan, Some(r)) if r.is_zero() => Some(Ratio::zero()),
            (Trig::Tan, Some(r)) if r.is_one() => Some(Ratio::new_raw(1, 8)),
            (Trig::Tan, Some(r)) if r == -Ratio::one() => Some(Ratio::new_raw(-1, 8)),
            _ => None,
        };
        if let Some(t) = turns {
            return Ok(match unit.turn() {
                Some(turn) => Number::exact(t * Ratio::from_integer(turn)),
                None if t.is_zero() => Number::ZERO,
                None => Number::Pi(t * Ratio::from_integer(2)),
            });
        }
        let radians = match f {
            Trig::Sin => libm::asin(x),
            Trig::Cos => libm::acos(x),
            Trig::Tan => libm::atan(x),
        };
        approx(match unit {
            AngleUnit::Degrees => radians * (180.0 / core::f64::consts::PI),
            AngleUnit::Gradians => radians * (200.0 / core::f64::consts::PI),
            AngleUnit::Radians => radians,
        })
    }

    /// Compares two numbers (floats compared as floats).
    pub fn compare(self, other: Number) -> Ordering {
        match (self, other) {
            (Number::Exact(a), Number::Exact(b)) | (Number::Pi(a), Number::Pi(b)) => a.cmp(&b),
            _ => self.to_f64().partial_cmp(&other.to_f64()).unwrap_or(Ordering::Equal),
        }
    }
}

/// The twelfths of a turn (from −3 to 3) whose sine is the fraction `r`, if it is one of the
/// sines that are fractions.
fn sine_twelfths(r: Rational) -> Option<i128> {
    let half = Ratio::new_raw(1, 2);
    if r.is_zero() {
        Some(0)
    } else if r == half {
        Some(1)
    } else if r == -half {
        Some(-1)
    } else if r.is_one() {
        Some(3)
    } else if r == -Ratio::one() {
        Some(-3)
    } else {
        None
    }
}

/// The exact `n`-th root of `r` (`n` from 1 to [`MAX_EXACT_ROOT`]), if its numerator and
/// denominator are both `n`-th powers; odd roots of negative fractions are negative.
fn exact_root(r: Rational, n: i128) -> Option<Rational> {
    if !(1..=MAX_EXACT_ROOT).contains(&n) {
        return None;
    }
    if r.is_negative() && n % 2 == 0 {
        return None;
    }
    let n = n as u32;
    let root = |v: i128| -> Option<i128> {
        let m = v.unsigned_abs();
        let k = m.nth_root(n);
        (k.checked_pow(n) == Some(m)).then_some(k as i128)
    };
    let num = root(*r.numer())?;
    let den = root(*r.denom())?;
    let num = if r.is_negative() { -num } else { num };
    keepable(Ratio::new(num, den))
}

/// The whole `k` with `base^k = x`, if there is one (`base` at least 2).
fn exact_log(x: Rational, base: i128) -> Option<i128> {
    if base < 2 || !x.is_positive() {
        return None;
    }
    // Powers of the base: whole numbers, or one over them.
    let (value, sign) = if x.denom().is_one() {
        (*x.numer(), 1)
    } else if x.numer().is_one() {
        (*x.denom(), -1)
    } else {
        return None;
    };
    let mut k = 0;
    let mut v = value;
    while v > 1 {
        let (q, r) = v.div_rem(&base);
        if r != 0 {
            return None;
        }
        v = q;
        k += 1;
    }
    Some(sign * k)
}

/// The greatest common divisor of two whole numbers.
pub fn gcd(a: Number, b: Number) -> Result<Number, CalcError> {
    let (Some(a), Some(b)) = (a.whole(), b.whole()) else { return Err(CalcError::NotInteger) };
    let g = a.unsigned_abs().gcd(&b.unsigned_abs());
    Ok(i128::try_from(g).map_or(Number::Approx(g as f64), Number::int))
}

/// The least common multiple of two whole numbers.
pub fn lcm(a: Number, b: Number) -> Result<Number, CalcError> {
    let (Some(x), Some(y)) = (a.whole(), b.whole()) else { return Err(CalcError::NotInteger) };
    if x == 0 || y == 0 {
        return Ok(Number::ZERO);
    }
    let g = x.gcd(&y);
    match (x / g).checked_mul(y).and_then(i128::checked_abs) {
        Some(l) => Ok(Number::int(l)),
        None => approx((x as f64 / g as f64 * y as f64).abs()),
    }
}

/// The number of ways to choose `k` things from `n` (`exact` permutations, in order, if
/// `ordered`): nCr and nPr.
pub fn choose(n: Number, k: Number, ordered: bool) -> Result<Number, CalcError> {
    let (Some(n), Some(k)) = (n.whole(), k.whole()) else { return Err(CalcError::NotInteger) };
    if n < 0 || k < 0 {
        return Err(CalcError::Domain);
    }
    if k > n {
        return Ok(Number::ZERO);
    }
    let k = if ordered { k } else { k.min(n - k) };
    let mut exact = Some(1i128);
    let mut float = 1.0f64;
    for i in 0..k {
        let factor = n - i;
        float *= factor as f64;
        exact = exact.and_then(|acc| acc.checked_mul(factor));
        if !ordered {
            // Each step's product of i + 1 consecutive numbers is divisible by (i + 1)!.
            float /= (i + 1) as f64;
            exact = exact.map(|acc| acc / (i + 1));
        }
        if float.is_infinite() {
            return Err(CalcError::Overflow);
        }
    }
    match exact {
        Some(v) => Ok(Number::int(v)),
        None => approx(float),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(n: i128, d: i128) -> Number {
        Number::fraction(n, d).unwrap()
    }

    fn approx_eq(n: Number, x: f64) -> bool {
        let v = n.to_f64();
        (v - x).abs() <= 1e-12 * x.abs().max(1.0)
    }

    #[test]
    fn fractions_stay_exact() {
        let tenth = q(1, 10);
        assert_eq!(tenth.add(q(2, 10)).unwrap(), q(3, 10));
        assert_eq!(q(1, 3).mul(Number::int(3)).unwrap(), Number::ONE);
        assert_eq!(Number::int(7).div(Number::int(2)).unwrap(), q(7, 2));
        assert_eq!(q(1, 2).sub(q(1, 2)).unwrap(), Number::ZERO);
        assert_eq!(Number::int(1).div(Number::ZERO), Err(CalcError::DivisionByZero));
        // Too large for 128 bits: the float, not a wrong fraction.
        let big = Number::int(i128::MAX / 2);
        let sum = big.add(big).unwrap().add(big).unwrap();
        assert!(!sum.is_exact() && approx_eq(sum, 1.5 * i128::MAX as f64));
        // Denominators too fine for long division become floats.
        let tiny = Number::ONE.div(Number::int(10).powi(37).unwrap()).unwrap();
        assert!(!tiny.is_exact() && approx_eq(tiny, 1e-37));
        assert!(Number::int(i128::MIN).to_f64() < 0.0);
    }

    #[test]
    fn multiples_of_pi_stay_exact() {
        let two_pi = Number::PI.mul(Number::int(2)).unwrap();
        assert_eq!(two_pi, Number::Pi(Ratio::from_integer(2)));
        assert_eq!(two_pi.div(Number::PI).unwrap(), Number::int(2));
        assert_eq!(Number::PI.sub(Number::PI).unwrap(), Number::ZERO);
        assert!(approx_eq(Number::PI.add(Number::ONE).unwrap(), core::f64::consts::PI + 1.0));
        assert!(!Number::PI.mul(Number::PI).unwrap().is_exact());
    }

    #[test]
    fn powers_and_roots() {
        assert_eq!(Number::int(2).powi(10).unwrap(), Number::int(1024));
        assert_eq!(Number::int(2).powi(-2).unwrap(), q(1, 4));
        assert_eq!(q(2, 3).powi(3).unwrap(), q(8, 27));
        assert_eq!(Number::ZERO.powi(0).unwrap(), Number::ONE);
        assert_eq!(Number::ZERO.powi(-1), Err(CalcError::DivisionByZero));
        assert_eq!(Number::int(8).pow(q(1, 3)).unwrap(), Number::int(2));
        assert_eq!(Number::int(-8).pow(q(1, 3)).unwrap(), Number::int(-2));
        assert_eq!(Number::int(-8).pow(q(2, 3)).unwrap(), Number::int(4));
        assert_eq!(Number::int(-4).pow(q(1, 2)), Err(CalcError::Domain));
        assert_eq!(q(9, 4).sqrt().unwrap(), q(3, 2));
        assert_eq!(Number::int(-1).sqrt(), Err(CalcError::Domain));
        assert!(approx_eq(Number::int(2).sqrt().unwrap(), core::f64::consts::SQRT_2));
        assert_eq!(Number::int(-27).root(3).unwrap(), Number::int(-3));
        assert!(approx_eq(Number::int(-2).pow(q(1, 3)).unwrap(), -libm::cbrt(2.0)));
        assert!(approx_eq(Number::int(2).pow(Number::Approx(0.5)).unwrap(), core::f64::consts::SQRT_2));
        assert!(approx_eq(Number::E.pow(Number::int(2)).unwrap(), libm::exp(2.0)));
        // Large powers become floats, then overflow.
        assert!(!Number::int(10).powi(40).unwrap().is_exact());
        assert_eq!(Number::int(10).powi(400), Err(CalcError::Overflow));
        assert_eq!(Number::int(2).powi(i128::MAX).err(), Some(CalcError::Overflow));
        assert_eq!(Number::ONE.powi(i128::MIN).unwrap(), Number::ONE);
    }

    #[test]
    fn factorials() {
        assert_eq!(Number::int(0).factorial().unwrap(), Number::ONE);
        assert_eq!(Number::int(5).factorial().unwrap(), Number::int(120));
        assert_eq!(
            Number::int(33).factorial().unwrap(),
            Number::int(8_683_317_618_811_886_495_518_194_401_280_000_000)
        );
        assert!(approx_eq(Number::int(34).factorial().unwrap(), 2.952_327_990_396_041_4e38));
        assert!(Number::int(170).factorial().is_ok());
        assert_eq!(Number::int(171).factorial(), Err(CalcError::Overflow));
        assert_eq!(Number::int(-1).factorial(), Err(CalcError::Domain));
        assert!(approx_eq(q(1, 2).factorial().unwrap(), 0.886_226_925_452_758));
    }

    #[test]
    fn logarithms() {
        assert_eq!(Number::int(1000).log(Number::int(10)).unwrap(), Number::int(3));
        assert_eq!(q(1, 1000).log(Number::int(10)).unwrap(), Number::int(-3));
        assert_eq!(q(1, 8).log(Number::int(2)).unwrap(), Number::int(-3));
        assert_eq!(Number::ONE.ln().unwrap(), Number::ZERO);
        assert!(approx_eq(Number::int(2).log(Number::int(10)).unwrap(), libm::log10(2.0)));
        assert!(approx_eq(Number::E.ln().unwrap(), 1.0));
        assert_eq!(Number::ZERO.ln(), Err(CalcError::Domain));
        assert_eq!(Number::int(-5).log(Number::int(10)), Err(CalcError::Domain));
        assert_eq!(Number::int(5).log(Number::ONE), Err(CalcError::Domain));
    }

    #[test]
    fn exact_angles() {
        use AngleUnit::*;
        let sin = |x: Number, u| x.trig(Trig::Sin, u).unwrap();
        assert_eq!(sin(Number::int(30), Degrees), q(1, 2));
        assert_eq!(sin(Number::int(180), Degrees), Number::ZERO);
        assert_eq!(sin(Number::int(-90), Degrees), Number::int(-1));
        assert_eq!(sin(Number::int(3630), Degrees), q(1, 2), "ten turns and 30°");
        assert_eq!(sin(Number::PI, Radians), Number::ZERO);
        assert_eq!(sin(q(1, 6).mul(Number::PI).unwrap(), Radians), q(1, 2));
        assert_eq!(Number::int(100).trig(Trig::Sin, Gradians).unwrap(), Number::ONE);
        assert_eq!(Number::int(60).trig(Trig::Cos, Degrees).unwrap(), q(1, 2));
        assert_eq!(Number::PI.trig(Trig::Cos, Radians).unwrap(), Number::int(-1));
        assert_eq!(Number::int(45).trig(Trig::Tan, Degrees).unwrap(), Number::ONE);
        assert_eq!(Number::int(135).trig(Trig::Tan, Degrees).unwrap(), Number::int(-1));
        assert_eq!(Number::int(90).trig(Trig::Tan, Degrees), Err(CalcError::Undefined));
        assert_eq!(Number::int(-270).trig(Trig::Tan, Degrees), Err(CalcError::Undefined));
        // Other angles are floats, computed within a turn.
        assert!(approx_eq(sin(Number::int(60), Degrees), 3f64.sqrt() / 2.0));
        let far = Number::int(360 * 10i128.pow(17) + 40);
        assert!(approx_eq(sin(far, Degrees), libm::sin(40f64.to_radians())), "within a turn, however far");
        assert!(approx_eq(sin(Number::ONE, Radians), libm::sin(1.0)));
        assert!(approx_eq(Number::Approx(0.5).trig(Trig::Cos, Radians).unwrap(), libm::cos(0.5)));
    }

    #[test]
    fn exact_inverse_angles() {
        use AngleUnit::*;
        assert_eq!(q(1, 2).inverse_trig(Trig::Sin, Degrees).unwrap(), Number::int(30));
        assert_eq!(q(1, 2).inverse_trig(Trig::Sin, Radians).unwrap(), Number::Pi(Ratio::new(1, 6)));
        assert_eq!(q(-1, 2).inverse_trig(Trig::Cos, Degrees).unwrap(), Number::int(120));
        assert_eq!(Number::ONE.inverse_trig(Trig::Cos, Gradians).unwrap(), Number::ZERO);
        assert_eq!(Number::ONE.inverse_trig(Trig::Tan, Degrees).unwrap(), Number::int(45));
        assert_eq!(Number::int(-1).inverse_trig(Trig::Tan, Radians).unwrap(), Number::Pi(Ratio::new(-1, 4)));
        assert_eq!(Number::int(2).inverse_trig(Trig::Sin, Degrees), Err(CalcError::Domain));
        assert!(approx_eq(Number::int(2).inverse_trig(Trig::Tan, Radians).unwrap(), libm::atan(2.0)));
        // Round trips.
        let a = q(1, 2).inverse_trig(Trig::Sin, Radians).unwrap();
        assert_eq!(a.trig(Trig::Sin, Radians).unwrap(), q(1, 2));
    }

    #[test]
    fn remainders_floors_and_choices() {
        assert_eq!(Number::int(7).modulo(Number::int(3)).unwrap(), Number::int(1));
        assert_eq!(Number::int(-7).modulo(Number::int(3)).unwrap(), Number::int(2));
        assert_eq!(Number::int(7).modulo(Number::int(-3)).unwrap(), Number::int(-2));
        assert_eq!(q(7, 2).modulo(Number::int(1)).unwrap(), q(1, 2));
        assert_eq!(Number::int(5).modulo(Number::ZERO), Err(CalcError::DivisionByZero));
        assert!(approx_eq(Number::Approx(5.5).modulo(Number::int(2)).unwrap(), 1.5));
        assert_eq!(q(7, 2).floor().unwrap(), Number::int(3));
        assert_eq!(q(-7, 2).ceil().unwrap(), Number::int(-3));
        assert_eq!(q(5, 2).round().unwrap(), Number::int(3));
        assert_eq!(q(-5, 2).round().unwrap(), Number::int(-3));
        assert_eq!(choose(Number::int(5), Number::int(2), false).unwrap(), Number::int(10));
        assert_eq!(choose(Number::int(5), Number::int(2), true).unwrap(), Number::int(20));
        assert_eq!(choose(Number::int(60), Number::int(30), false).unwrap(), Number::int(118_264_581_564_861_424));
        assert_eq!(choose(Number::int(3), Number::int(5), false).unwrap(), Number::ZERO);
        assert_eq!(choose(q(1, 2), Number::int(1), false), Err(CalcError::NotInteger));
        assert_eq!(gcd(Number::int(12), Number::int(18)).unwrap(), Number::int(6));
        assert_eq!(lcm(Number::int(4), Number::int(6)).unwrap(), Number::int(12));
        assert_eq!(lcm(Number::int(-4), Number::int(6)).unwrap(), Number::int(12));
    }

    #[test]
    fn floats_never_give_nan_or_negative_zero() {
        assert_eq!(approx(f64::NAN), Err(CalcError::Domain));
        assert_eq!(approx(f64::INFINITY), Err(CalcError::Overflow));
        assert_eq!(approx(-0.0).unwrap(), Number::Approx(0.0));
        assert!(Number::Approx(0.0).neg().to_f64().is_sign_positive());
        assert_eq!(Number::Approx(1e308).mul(Number::int(10)), Err(CalcError::Overflow));
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Numbers as people read and write them.
//!
//! [`format`] writes a [`Number`] in decimal, rounded to a number of significant digits (half
//! away from zero), in plain or scientific notation (`1.5E+20`), with thousands separators if
//! asked. Fractions are expanded by long division, so every digit shown is right; floats are
//! shown to 15 significant digits, which hides the binary rounding of their last digits
//! (0.1 × 3 is 0.30000000000000004 as a float, and shows 0.3).
//!
//! [`parse_decimal`] reads the literals of expressions (`12.5`, `.5`, `6.02E23`) as exact
//! fractions, and [`exact_text`] writes a number back as text that reads as the same number.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use num_rational::Ratio;
use num_traits::Signed;

use crate::number::{MAX_DENOMINATOR, Number, Rational};

/// The significant digits a float is shown with: as many as a float always keeps.
pub const FLOAT_DIGITS: usize = 15;
/// Exact whole numbers below 10^21 are shown in full, however many digits they have.
const PLAIN_INTEGER_DIGITS: usize = 21;
/// Numbers from 10^-7 are shown in plain notation (`0.0000001`), smaller ones in scientific.
const PLAIN_MIN_EXPONENT: i32 = -7;

/// How [`format`] writes numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    /// The most significant digits of an exact number (floats have at most [`FLOAT_DIGITS`]).
    pub digits: usize,
    /// Thousands separators in the whole part (`1,234,567.5`).
    pub grouping: bool,
    /// Always scientific notation (`1.2345E+3`).
    pub scientific: bool,
    /// ASCII signs (`-`), for text other programs read; otherwise the minus sign (`−`).
    pub ascii: bool,
}

impl Default for Style {
    fn default() -> Style {
        Style { digits: 16, grouping: true, scientific: false, ascii: false }
    }
}

impl Style {
    /// Plain ASCII without separators: for the clipboard and the voice agent.
    pub const PLAIN: Style = Style { digits: 16, grouping: false, scientific: false, ascii: true };

    fn minus(&self) -> char {
        if self.ascii { '-' } else { '\u{2212}' }
    }
}

/// A number's significant digits: `0.d₁d₂…dₙ × 10^(exponent + 1)`, or `d₁.d₂…dₙ × 10^exponent`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Digits {
    negative: bool,
    /// From the first that is not zero (empty for zero).
    digits: Vec<u8>,
    /// The power of ten of the first digit.
    exponent: i32,
}

impl Digits {
    /// Rounds to `count` significant digits, halves away from zero, and drops trailing zeros.
    fn round(mut self, count: usize) -> Digits {
        if self.digits.len() > count {
            let up = self.digits[count] >= 5;
            self.digits.truncate(count);
            if up {
                let mut i = count;
                loop {
                    if i == 0 {
                        // 9.99… rounded up to 10.
                        self.digits.insert(0, 1);
                        self.digits.truncate(count);
                        self.exponent += 1;
                        break;
                    }
                    i -= 1;
                    if self.digits[i] == 9 {
                        self.digits[i] = 0;
                    } else {
                        self.digits[i] += 1;
                        break;
                    }
                }
            }
        }
        while self.digits.last() == Some(&0) {
            self.digits.pop();
        }
        if self.digits.is_empty() {
            self.negative = false;
            self.exponent = 0;
        }
        self
    }
}

/// The decimal digits of a fraction, by long division: at least `count + 1` significant ones
/// where it has them (one more than shown, to round by).
fn fraction_digits(r: &Rational, count: usize) -> Digits {
    let negative = r.is_negative();
    let (n, d) = (r.numer().unsigned_abs(), r.denom().unsigned_abs());
    if n == 0 {
        return Digits { negative: false, digits: Vec::new(), exponent: 0 };
    }
    let (whole, mut rem) = (n / d, n % d);
    let mut digits = Vec::new();
    let exponent;
    if whole > 0 {
        let text = format!("{whole}");
        exponent = text.len() as i32 - 1;
        digits.extend(text.bytes().map(|b| b - b'0'));
    } else {
        // Zeros after the point, then the first digit that is not one. Denominators are at most
        // 10^36, so remainders times ten stay within 128 bits and there are at most 36 zeros.
        let mut e = -1;
        loop {
            rem *= 10;
            let digit = (rem / d) as u8;
            rem %= d;
            if digit != 0 {
                digits.push(digit);
                break;
            }
            e -= 1;
        }
        exponent = e;
    }
    while digits.len() <= count && rem != 0 {
        rem *= 10;
        digits.push((rem / d) as u8);
        rem %= d;
    }
    Digits { negative, digits, exponent }
}

/// The decimal digits of a float, correctly rounded to `count` significant ones by the core
/// library's formatting.
fn float_digits(x: f64, count: usize) -> Digits {
    if x == 0.0 {
        return Digits { negative: false, digits: Vec::new(), exponent: 0 };
    }
    let text = format!("{:.*e}", count.max(1) - 1, x.abs());
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    Digits {
        negative: x < 0.0,
        digits: mantissa.bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').collect(),
        exponent: exponent.parse().unwrap_or(0),
    }
}

/// The fewest digits that read back as exactly the float `x` (the core library's shortest
/// round trip).
fn shortest_digits(x: f64) -> Digits {
    if x == 0.0 {
        return Digits { negative: false, digits: Vec::new(), exponent: 0 };
    }
    let text = format!("{:e}", x.abs());
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    Digits {
        negative: x < 0.0,
        digits: mantissa.bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').collect(),
        exponent: exponent.parse().unwrap_or(0),
    }
}

/// The digits of `n` to show: exact numbers to `style.digits`, others to [`FLOAT_DIGITS`].
fn digits_of(n: Number, style: &Style) -> Digits {
    match n {
        Number::Exact(r) => fraction_digits(&r, style.digits).round(style.digits),
        _ => {
            let count = style.digits.min(FLOAT_DIGITS);
            float_digits(n.to_f64(), count).round(count)
        }
    }
}

/// Inserts thousands separators into a run of digits.
fn group(digits: &str, grouping: bool) -> String {
    if !grouping || digits.len() <= 3 {
        return digits.into();
    }
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Writes `n` in decimal, as `style` says.
pub fn format(n: Number, style: &Style) -> String {
    // Exact whole numbers below 10^21 in full: 20! is 2432902008176640000.
    if let Some(i) = n.as_integer()
        && !style.scientific
    {
        let text = format!("{}", i.unsigned_abs());
        if text.len() <= PLAIN_INTEGER_DIGITS {
            let mut out = String::new();
            if i < 0 {
                out.push(style.minus());
            }
            out.push_str(&group(&text, style.grouping));
            return out;
        }
    }
    layout(&digits_of(n, style), style)
}

/// Lays digits out in plain or scientific notation.
fn layout(d: &Digits, style: &Style) -> String {
    let mut out = String::new();
    if d.digits.is_empty() {
        out.push('0');
        return out;
    }
    if d.negative {
        out.push(style.minus());
    }
    let text: String = d.digits.iter().map(|&b| char::from(b'0' + b)).collect();
    let count = d.digits.len() as i32;
    let plain_max = style.digits.min(PLAIN_INTEGER_DIGITS) as i32;
    if !style.scientific && d.exponent >= PLAIN_MIN_EXPONENT && d.exponent < plain_max {
        if d.exponent < 0 {
            out.push_str("0.");
            for _ in 0..(-d.exponent - 1) {
                out.push('0');
            }
            out.push_str(&text);
        } else {
            let whole_len = d.exponent as usize + 1;
            let (whole, fraction) = if count as usize > whole_len {
                (String::from(&text[..whole_len]), &text[whole_len..])
            } else {
                let mut w = text.clone();
                for _ in count..d.exponent + 1 {
                    w.push('0');
                }
                (w, "")
            };
            out.push_str(&group(&whole, style.grouping));
            if !fraction.is_empty() {
                out.push('.');
                out.push_str(fraction);
            }
        }
        return out;
    }
    out.push_str(&text[..1]);
    if text.len() > 1 {
        out.push('.');
        out.push_str(&text[1..]);
    }
    out.push('E');
    out.push(if d.exponent < 0 { style.minus() } else { '+' });
    out.push_str(&format!("{}", d.exponent.unsigned_abs()));
    out
}

/// A fraction as `n/d` (`−3/4`), for an exact number that is not whole; `None` otherwise, or
/// for fractions too fine to be worth reading (denominators beyond a million).
pub fn fraction_text(n: Number, style: &Style) -> Option<String> {
    let r = n.rational()?;
    if r.is_integer() || *r.denom() > 1_000_000 {
        return None;
    }
    let mut out = String::new();
    if r.is_negative() {
        out.push(style.minus());
    }
    out.push_str(&group(&format!("{}", r.numer().unsigned_abs()), style.grouping));
    out.push('/');
    out.push_str(&group(&format!("{}", r.denom()), style.grouping));
    Some(out)
}

/// Text that reads as exactly `n` in an expression (with the calculator's signs): a decimal
/// where one is exact (`0.125`), a quotient where it is not (`(1÷3)`), a multiple of π
/// (`(π÷6)`), and floats with the fewest digits that give them back exactly.
pub fn exact_text(n: Number) -> String {
    let minus = '\u{2212}';
    match n {
        Number::Exact(r) => {
            if let Some(decimal) = terminating_decimal(&r) {
                return decimal;
            }
            let sign = if r.is_negative() { "\u{2212}" } else { "" };
            format!("({sign}{}÷{})", r.numer().unsigned_abs(), r.denom())
        }
        Number::Pi(r) => {
            let sign = if r.is_negative() { "\u{2212}" } else { "" };
            let k = r.numer().unsigned_abs();
            let times = if k == 1 { String::from("π") } else { format!("{k}π") };
            if r.is_integer() { format!("({sign}{times})") } else { format!("({sign}{times}÷{})", r.denom()) }
        }
        Number::Approx(x) => {
            let d = shortest_digits(x);
            let style = Style { digits: 17, grouping: false, scientific: false, ascii: false };
            let mut text = layout(&d, &style);
            if text.starts_with(minus) {
                // A leading minus negates what follows only up to the next operator: keep the
                // number whole.
                text = format!("({text})");
            }
            text
        }
    }
}

/// The fraction as a decimal, if its expansion ends within 40 significant digits (its
/// denominator has no prime factors but 2 and 5, and not too many of them); negative ones in
/// parentheses, so that what they are inserted after does not take their sign for a minus.
fn terminating_decimal(r: &Rational) -> Option<String> {
    const MOST: usize = 40;
    let digits = fraction_digits(r, MOST);
    // Long division stopped short of its end: the expansion is longer, or does not end.
    if digits.digits.len() > MOST {
        return None;
    }
    let style = Style { digits: MOST, grouping: false, scientific: false, ascii: false };
    let text = layout(&digits.round(MOST), &style);
    Some(if r.is_negative() { format!("({text})") } else { text })
}

/// Reads a decimal literal (`12.5`, `.5`, `5.`, `6.02E23`, `1e−3`) exactly, as a fraction where
/// it fits, as a float where it does not; `None` if it is not one.
pub fn parse_decimal(text: &str) -> Option<Number> {
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(i) => {
            let e = text[i + 1..].replace('\u{2212}', "-");
            let e = e.strip_prefix('+').unwrap_or(&e);
            (&text[..i], Some(e.parse::<i32>().ok()?))
        }
        None => (text, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let exponent = exponent.unwrap_or(0) as i64 - fraction.len() as i64;
    let digits = || whole.bytes().chain(fraction.bytes()).map(|b| (b - b'0') as i128);
    let numerator = digits().try_fold(0i128, |acc, d| acc.checked_mul(10)?.checked_add(d));
    let exact = numerator.and_then(|n| {
        let scale = 10i128.checked_pow(exponent.unsigned_abs().try_into().ok()?)?;
        if exponent >= 0 {
            Some(Ratio::from_integer(n.checked_mul(scale)?))
        } else if scale <= MAX_DENOMINATOR {
            Some(Ratio::new(n, scale))
        } else {
            None
        }
    });
    match exact {
        Some(r) => Some(Number::exact(r)),
        None => {
            // Too many digits, or too large or small, for a fraction: the float nearest all the
            // digits, by the core library's correctly rounded parsing.
            let all: String = whole.chars().chain(fraction.chars()).collect();
            let x: f64 = format!("{all}e{exponent}").parse().ok()?;
            x.is_finite().then_some(Number::Approx(x))
        }
    }
}

/// Whether `n` is zero for display: no digit would be shown.
pub fn shows_zero(n: Number) -> bool {
    n.is_zero() || digits_of(n, &Style::default()).digits.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(n: i128, d: i128) -> Number {
        Number::fraction(n, d).unwrap()
    }

    fn show(n: Number) -> String {
        format(n, &Style::default())
    }

    #[test]
    fn fractions_in_decimal() {
        assert_eq!(show(q(3, 10)), "0.3");
        assert_eq!(show(q(1, 3)), "0.3333333333333333");
        assert_eq!(show(q(2, 3)), "0.6666666666666667");
        assert_eq!(show(q(-7, 2)), "\u{2212}3.5");
        assert_eq!(show(q(1, 8)), "0.125");
        assert_eq!(show(Number::int(1_234_567)), "1,234,567");
        assert_eq!(show(q(1_234_567_891, 1000)), "1,234,567.891");
        assert_eq!(show(Number::int(0)), "0");
        assert_eq!(show(q(1, 10_000_000)), "0.0000001");
        assert_eq!(show(q(1, 100_000_000)), "1E\u{2212}8");
        assert_eq!(show(q(999_999_999_999_999_999, 1_000_000_000_000_000_000)), "1");
        assert_eq!(show(q(99_999_999_999_999_999, 10)), "1E+16", "rounded beyond the digits shown");
    }

    #[test]
    fn whole_numbers_in_full_up_to_twenty_one_digits() {
        assert_eq!(show(Number::int(2_432_902_008_176_640_000)), "2,432,902,008,176,640,000");
        let style = Style { grouping: false, ..Style::default() };
        assert_eq!(format(Number::int(10i128.pow(20)), &style), "100000000000000000000");
        assert_eq!(format(Number::int(10i128.pow(21)), &style), "1E+21");
        assert_eq!(format(Number::int(2i128.pow(100)), &style), "1.267650600228229E+30");
        assert_eq!(format(Number::int(-(10i128.pow(30))), &Style::PLAIN), "-1E+30");
    }

    #[test]
    fn floats_hide_their_binary_rounding() {
        assert_eq!(show(Number::Approx(0.1 + 0.2)), "0.3");
        assert_eq!(show(Number::Approx(1.1 * 1.1)), "1.21");
        assert_eq!(show(Number::Approx(core::f64::consts::SQRT_2)), "1.4142135623731");
        assert_eq!(show(Number::Approx(core::f64::consts::PI)), "3.14159265358979");
        assert_eq!(show(Number::PI), "3.14159265358979");
        assert_eq!(show(Number::Approx(1.5e300)), "1.5E+300");
        assert_eq!(show(Number::Approx(-2.5e-12)), "\u{2212}2.5E\u{2212}12");
        assert_eq!(show(Number::Approx(123_456_789_012_345.6)), "123,456,789,012,346");
        assert_eq!(show(Number::Approx(9_999_999_999_999_998.0)), "1E+16");
    }

    #[test]
    fn scientific_on_request() {
        let sci = Style { scientific: true, ..Style::default() };
        assert_eq!(format(Number::int(1234), &sci), "1.234E+3");
        assert_eq!(format(q(1, 4), &sci), "2.5E\u{2212}1");
        assert_eq!(format(Number::int(0), &sci), "0");
    }

    #[test]
    fn rounding_carries_and_halves_go_up() {
        let three = Style { digits: 3, ..Style::default() };
        assert_eq!(format(q(9995, 10_000), &three), "1");
        assert_eq!(format(q(1235, 10_000), &three), "0.124");
        assert_eq!(format(q(-1235, 10_000), &three), "\u{2212}0.124");
        assert_eq!(format(q(1, 3), &three), "0.333");
        assert_eq!(format(q(99_950, 1), &three), "99,950", "whole numbers in full");
        assert_eq!(format(q(999_951, 10), &three), "1E+5");
    }

    #[test]
    fn fractions_as_fractions() {
        assert_eq!(fraction_text(q(3, 4), &Style::PLAIN).as_deref(), Some("3/4"));
        assert_eq!(fraction_text(q(-10_001, 3), &Style::default()).as_deref(), Some("\u{2212}10,001/3"));
        assert_eq!(fraction_text(Number::int(4), &Style::PLAIN), None);
        assert_eq!(fraction_text(Number::PI, &Style::PLAIN), None);
        assert_eq!(fraction_text(q(1, 10_000_000), &Style::PLAIN), None);
    }

    #[test]
    fn literals_read_exactly() {
        assert_eq!(parse_decimal("12.5"), Some(q(25, 2)));
        assert_eq!(parse_decimal(".5"), Some(q(1, 2)));
        assert_eq!(parse_decimal("5."), Some(Number::int(5)));
        assert_eq!(parse_decimal("6.02E23"), Some(Number::int(602_000_000_000_000_000_000_000)));
        assert_eq!(parse_decimal("1e\u{2212}3"), Some(q(1, 1000)));
        assert_eq!(parse_decimal("1E+2"), Some(Number::int(100)));
        assert_eq!(parse_decimal("0.1"), Some(q(1, 10)));
        // Too many digits for a fraction: the float.
        let long = parse_decimal("3.14159265358979323846264338327950288419716939937510").unwrap();
        assert!(!long.is_exact() && (long.to_f64() - core::f64::consts::PI).abs() < 1e-15);
        assert!(!parse_decimal("1E300").unwrap().is_exact());
        assert_eq!(parse_decimal("1E400"), None, "beyond the floats");
        for bad in ["", ".", "1.2.3", "1E", "E5", "1E+", "12a", "-1"] {
            assert_eq!(parse_decimal(bad), None, "{bad}");
        }
    }

    #[test]
    fn text_that_reads_back_the_same() {
        assert_eq!(exact_text(q(1, 8)), "0.125");
        assert_eq!(exact_text(Number::int(42)), "42");
        assert_eq!(exact_text(q(-5, 2)), "(\u{2212}2.5)");
        assert_eq!(exact_text(q(1, 3)), "(1÷3)");
        assert_eq!(exact_text(q(-1, 3)), "(\u{2212}1÷3)");
        assert_eq!(exact_text(Number::PI), "(π)");
        assert_eq!(exact_text(q(1, 6).mul(Number::PI).unwrap()), "(π÷6)");
        assert_eq!(exact_text(Number::int(-2).mul(Number::PI).unwrap()), "(\u{2212}2π)");
        assert_eq!(exact_text(Number::Approx(core::f64::consts::SQRT_2)), "1.4142135623730951");
        assert_eq!(exact_text(Number::Approx(-1.5e-20)), "(\u{2212}1.5E\u{2212}20)");
        assert_eq!(exact_text(q(1, 10i128.pow(12))), "1E\u{2212}12");
    }
}

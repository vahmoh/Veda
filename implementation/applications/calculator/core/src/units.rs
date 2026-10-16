// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Units and their conversions.
//!
//! Each [`Category`] measures in a base unit (metres, kilograms, kelvins) and gives every unit
//! of its as how many base units one of it is: exact fractions where the definitions are (an
//! inch is 0.0254 m exactly, a pound 0.45359237 kg), so that conversions are exact too.
//! Temperatures also have an offset (a degree Celsius is a kelvin, 0 °C is 273.15 K), and
//! angles are measured in radians, a degree being exactly π/180 of one.

use alloc::string::String;

use crate::format::parse_decimal;
use crate::number::{CalcError, Number};

/// A unit: its name and symbol, and what one of it is in its category's base unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unit {
    /// Its name, plural ("Kilometres").
    pub name: &'static str,
    /// Its symbol ("km").
    pub symbol: &'static str,
    /// Base units in one: a decimal, or a quotient of two ("5/18").
    factor: &'static str,
    /// The factor is times π.
    pi: bool,
    /// Base units added after scaling (temperatures'): base = value × factor + offset.
    offset: &'static str,
}

/// A kind of quantity and its units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Category {
    pub name: &'static str,
    pub units: &'static [Unit],
    /// The units converted from and to at first.
    pub default: (usize, usize),
}

const fn unit(name: &'static str, symbol: &'static str, factor: &'static str) -> Unit {
    Unit { name, symbol, factor, pi: false, offset: "0" }
}

const fn angle(name: &'static str, symbol: &'static str, factor: &'static str) -> Unit {
    Unit { name, symbol, factor, pi: true, offset: "0" }
}

const fn affine(name: &'static str, symbol: &'static str, factor: &'static str, offset: &'static str) -> Unit {
    Unit { name, symbol, factor, pi: false, offset }
}

/// Every category, in the order the converter lists them.
pub const CATEGORIES: &[Category] = &[
    Category {
        name: "Length",
        units: &[
            unit("Nanometres", "nm", "0.000000001"),
            unit("Micrometres", "µm", "0.000001"),
            unit("Millimetres", "mm", "0.001"),
            unit("Centimetres", "cm", "0.01"),
            unit("Metres", "m", "1"),
            unit("Kilometres", "km", "1000"),
            unit("Inches", "in", "0.0254"),
            unit("Feet", "ft", "0.3048"),
            unit("Yards", "yd", "0.9144"),
            unit("Miles", "mi", "1609.344"),
            unit("Nautical miles", "nmi", "1852"),
        ],
        default: (4, 7),
    },
    Category {
        name: "Area",
        units: &[
            unit("Square millimetres", "mm²", "0.000001"),
            unit("Square centimetres", "cm²", "0.0001"),
            unit("Square metres", "m²", "1"),
            unit("Hectares", "ha", "10000"),
            unit("Square kilometres", "km²", "1000000"),
            unit("Square inches", "in²", "0.00064516"),
            unit("Square feet", "ft²", "0.09290304"),
            unit("Square yards", "yd²", "0.83612736"),
            unit("Acres", "ac", "4046.8564224"),
            unit("Square miles", "mi²", "2589988.110336"),
        ],
        default: (2, 6),
    },
    Category {
        name: "Volume",
        units: &[
            unit("Millilitres", "mL", "0.001"),
            unit("Litres", "L", "1"),
            unit("Cubic metres", "m³", "1000"),
            unit("Teaspoons (US)", "tsp", "0.00492892159375"),
            unit("Tablespoons (US)", "tbsp", "0.01478676478125"),
            unit("Fluid ounces (US)", "fl oz", "0.0295735295625"),
            unit("Cups (US)", "cup", "0.2365882365"),
            unit("Pints (US)", "pt", "0.473176473"),
            unit("Quarts (US)", "qt", "0.946352946"),
            unit("Gallons (US)", "gal", "3.785411784"),
            unit("Gallons (UK)", "gal (UK)", "4.54609"),
            unit("Cubic inches", "in³", "0.016387064"),
            unit("Cubic feet", "ft³", "28.316846592"),
        ],
        default: (1, 9),
    },
    Category {
        name: "Mass",
        units: &[
            unit("Milligrams", "mg", "0.000001"),
            unit("Grams", "g", "0.001"),
            unit("Kilograms", "kg", "1"),
            unit("Tonnes", "t", "1000"),
            unit("Ounces", "oz", "0.028349523125"),
            unit("Pounds", "lb", "0.45359237"),
            unit("Stones", "st", "6.35029318"),
            unit("Short tons (US)", "ton", "907.18474"),
            unit("Long tons (UK)", "ton (UK)", "1016.0469088"),
        ],
        default: (2, 5),
    },
    Category {
        name: "Temperature",
        units: &[
            affine("Celsius", "°C", "1", "273.15"),
            affine("Fahrenheit", "°F", "5/9", "45967/180"),
            affine("Kelvin", "K", "1", "0"),
            affine("Rankine", "°R", "5/9", "0"),
        ],
        default: (0, 1),
    },
    Category {
        name: "Speed",
        units: &[
            unit("Metres per second", "m/s", "1"),
            unit("Kilometres per hour", "km/h", "5/18"),
            unit("Miles per hour", "mph", "0.44704"),
            unit("Knots", "kn", "463/900"),
            unit("Feet per second", "ft/s", "0.3048"),
        ],
        default: (1, 2),
    },
    Category {
        name: "Time",
        units: &[
            unit("Microseconds", "µs", "0.000001"),
            unit("Milliseconds", "ms", "0.001"),
            unit("Seconds", "s", "1"),
            unit("Minutes", "min", "60"),
            unit("Hours", "h", "3600"),
            unit("Days", "d", "86400"),
            unit("Weeks", "wk", "604800"),
            // The Gregorian calendar's year, on average: 365.2425 days.
            unit("Years", "yr", "31556952"),
        ],
        default: (4, 3),
    },
    Category {
        name: "Data",
        units: &[
            unit("Bits", "bit", "1/8"),
            unit("Bytes", "B", "1"),
            unit("Kilobits", "kb", "125"),
            unit("Kilobytes", "kB", "1000"),
            unit("Kibibytes", "KiB", "1024"),
            unit("Megabits", "Mb", "125000"),
            unit("Megabytes", "MB", "1000000"),
            unit("Mebibytes", "MiB", "1048576"),
            unit("Gigabits", "Gb", "125000000"),
            unit("Gigabytes", "GB", "1000000000"),
            unit("Gibibytes", "GiB", "1073741824"),
            unit("Terabytes", "TB", "1000000000000"),
            unit("Tebibytes", "TiB", "1099511627776"),
        ],
        default: (9, 10),
    },
    Category {
        name: "Energy",
        units: &[
            unit("Electronvolts", "eV", "1.602176634E-19"),
            unit("Joules", "J", "1"),
            unit("Kilojoules", "kJ", "1000"),
            unit("Calories", "cal", "4.184"),
            unit("Kilocalories", "kcal", "4184"),
            unit("Watt-hours", "Wh", "3600"),
            unit("Kilowatt-hours", "kWh", "3600000"),
            unit("British thermal units", "BTU", "1055.05585262"),
        ],
        default: (4, 2),
    },
    Category {
        name: "Power",
        units: &[
            unit("Watts", "W", "1"),
            unit("Kilowatts", "kW", "1000"),
            unit("Megawatts", "MW", "1000000"),
            unit("Horsepower", "hp", "745.69987158227022"),
            unit("Metric horsepower", "PS", "735.49875"),
            unit("BTUs per hour", "BTU/h", "1055.05585262/3600"),
        ],
        default: (1, 3),
    },
    Category {
        name: "Pressure",
        units: &[
            unit("Pascals", "Pa", "1"),
            unit("Kilopascals", "kPa", "1000"),
            unit("Bars", "bar", "100000"),
            unit("Atmospheres", "atm", "101325"),
            // A pound-force (0.45359237 kg × 9.80665 m/s²) on a square inch.
            unit("Pounds per square inch", "psi", "4.4482216152605/0.00064516"),
            unit("Millimetres of mercury", "mmHg", "101325/760"),
        ],
        default: (2, 4),
    },
    Category {
        name: "Angle",
        units: &[
            angle("Degrees", "°", "1/180"),
            unit("Radians", "rad", "1"),
            angle("Gradians", "grad", "1/200"),
            angle("Turns", "tr", "2"),
            angle("Arcminutes", "′", "1/10800"),
            angle("Arcseconds", "″", "1/648000"),
        ],
        default: (0, 1),
    },
];

/// A decimal or a quotient of two decimals, as the tables write factors.
fn quantity(text: &str) -> Number {
    let parsed = match text.split_once('/') {
        Some((n, d)) => parse_decimal(n).zip(parse_decimal(d)).and_then(|(n, d)| n.div(d).ok()),
        None => parse_decimal(text),
    };
    // The tables are this module's own, and every entry reads (a test checks).
    parsed.unwrap_or(Number::ONE)
}

impl Unit {
    /// Base units in one of this unit.
    pub fn factor(&self) -> Number {
        let f = quantity(self.factor);
        if self.pi { f.mul(Number::PI).unwrap_or(f) } else { f }
    }

    fn offset(&self) -> Number {
        quantity(self.offset)
    }

    /// `value` of this unit in the category's base unit.
    pub fn to_base(&self, value: Number) -> Result<Number, CalcError> {
        value.mul(self.factor())?.add(self.offset())
    }

    /// `value` in the category's base unit, in this unit.
    pub fn from_base(&self, value: Number) -> Result<Number, CalcError> {
        value.sub(self.offset())?.div(self.factor())
    }
}

/// `value` in `from`, in `to`.
pub fn convert(value: Number, from: &Unit, to: &Unit) -> Result<Number, CalcError> {
    to.from_base(from.to_base(value)?)
}

/// A unit's name as names are compared: lower case, spelt the British way ("metre", "litre"),
/// without a qualifier in parentheses ("(US)").
fn normalize(name: &str) -> String {
    let lower = name.to_lowercase().replace("meter", "metre").replace("liter", "litre");
    let bare = match lower.find(" (") {
        Some(i) => &lower[..i],
        None => &lower,
    };
    String::from(bare.trim())
}

/// A unit's name in the singular: its first word's ("feet" → "foot", "inches" → "inch",
/// "metres per second" → "metre per second").
fn singular(name: &str) -> String {
    let (first, rest) = match name.find(' ') {
        Some(i) => name.split_at(i),
        None => (name, ""),
    };
    let one = if first == "feet" {
        "foot"
    } else if ["ches", "shes", "sses"].iter().any(|e| first.ends_with(e)) {
        &first[..first.len() - 2]
    } else {
        first.strip_suffix('s').unwrap_or(first)
    };
    let mut out = String::from(one);
    out.push_str(rest);
    out
}

/// The category and unit (their indices) that `name` names: a symbol ("km", exactly, then
/// whatever its case), or a name, plural or not, spelt either way ("kilometer", "Kilometres").
pub fn find_unit(name: &str) -> Option<(usize, usize)> {
    let wanted = name.trim();
    let normal = normalize(wanted);
    let by: [&dyn Fn(&Unit) -> bool; 3] = [&|u| u.symbol == wanted, &|u| u.symbol.eq_ignore_ascii_case(wanted), &|u| {
        let n = normalize(u.name);
        n == normal || singular(&n) == normal
    }];
    by.iter().find_map(|matches| {
        CATEGORIES.iter().enumerate().find_map(|(c, cat)| cat.units.iter().position(matches).map(|u| (c, u)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(value: i128, from: &str, to: &str) -> Number {
        let (cf, f) = find_unit(from).unwrap();
        let (ct, t) = find_unit(to).unwrap();
        assert_eq!(cf, ct, "{from} and {to}");
        convert(Number::int(value), &CATEGORIES[cf].units[f], &CATEGORIES[ct].units[t]).unwrap()
    }

    fn q(n: i128, d: i128) -> Number {
        Number::fraction(n, d).unwrap()
    }

    #[test]
    fn every_factor_reads() {
        for cat in CATEGORIES {
            assert!(cat.default.0 < cat.units.len() && cat.default.1 < cat.units.len(), "{}", cat.name);
            for u in cat.units {
                for text in [u.factor, u.offset] {
                    let parsed = match text.split_once('/') {
                        Some((n, d)) => parse_decimal(n).is_some() && parse_decimal(d).is_some(),
                        None => parse_decimal(text).is_some(),
                    };
                    assert!(parsed, "{} {}", u.name, text);
                }
                assert!(!u.factor().is_zero(), "{}", u.name);
            }
        }
    }

    #[test]
    fn exact_conversions() {
        assert_eq!(conv(1, "in", "cm"), q(254, 100));
        assert_eq!(conv(1, "mile", "km"), q(1_609_344, 1_000_000));
        assert_eq!(conv(100, "°C", "°F"), Number::int(212));
        assert_eq!(conv(-40, "°F", "°C"), Number::int(-40));
        assert_eq!(conv(0, "K", "°C"), q(-27315, 100));
        assert_eq!(conv(36, "km/h", "m/s"), Number::int(10));
        assert_eq!(conv(1, "GiB", "MiB"), Number::int(1024));
        assert_eq!(conv(8, "bit", "B"), Number::ONE);
        assert_eq!(conv(1, "kWh", "kJ"), Number::int(3600));
        assert_eq!(conv(180, "°", "grad"), Number::int(200));
        assert_eq!(conv(180, "°", "rad"), Number::PI);
        assert_eq!(conv(1, "atm", "Pa"), Number::int(101_325));
        assert!((conv(1, "psi", "Pa").to_f64() - 6_894.757_293_168_36).abs() < 1e-6);
        assert!((conv(1, "rad", "°").to_f64() - 57.295_779_513_082_32).abs() < 1e-12);
    }

    #[test]
    fn units_by_their_names() {
        let unit = |n: &str| find_unit(n).map(|(c, u)| CATEGORIES[c].units[u].name);
        assert_eq!(unit("km"), Some("Kilometres"));
        assert_eq!(unit("kilometer"), Some("Kilometres"));
        assert_eq!(unit("Kilometres"), Some("Kilometres"));
        assert_eq!(unit("m"), Some("Metres"));
        assert_eq!(unit("inch"), Some("Inches"));
        assert_eq!(unit("pounds"), Some("Pounds"));
        assert_eq!(unit("fahrenheit"), Some("Fahrenheit"));
        assert_eq!(unit("liter"), Some("Litres"));
        assert_eq!(unit("MB"), Some("Megabytes"));
        assert_eq!(unit("Mb"), Some("Megabits"));
        assert_eq!(unit("parsecs"), None);
    }
}

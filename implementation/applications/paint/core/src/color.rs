// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Colours as people choose and name them: hue, saturation and value; `#RRGGBB`; names
//! ("dark blue"); the palette; and the arithmetic of premultiplied pixels the tools share.

use alloc::format;
use alloc::string::String;

use gfx::Color;

/// A colour's hue (0 to 360 degrees), saturation and value (0 to 1 each).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hsv {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

impl Hsv {
    /// The colour of a straight-alpha `Color`'s red, green and blue.
    pub fn of(c: Color) -> Hsv {
        let (r, g, b) = (c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let d = max - min;
        let h = if d == 0.0 {
            0.0
        } else if max == r {
            60.0 * libm::fmodf((g - b) / d + 6.0, 6.0)
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        Hsv { h, s: if max == 0.0 { 0.0 } else { d / max }, v: max }
    }

    /// The colour, with alpha `a`.
    pub fn to_color(self, a: u8) -> Color {
        let h = libm::fmodf(self.h.clamp(0.0, 360.0), 360.0) / 60.0;
        let (s, v) = (self.s.clamp(0.0, 1.0), self.v.clamp(0.0, 1.0));
        let c = v * s;
        let x = c * (1.0 - libm::fabsf(libm::fmodf(h, 2.0) - 1.0));
        let (r, g, b) = match h as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = v - c;
        let byte = |f: f32| libm::roundf((f + m) * 255.0).clamp(0.0, 255.0) as u8;
        Color::rgba(byte(r), byte(g), byte(b), a)
    }
}

/// `#RRGGBB`, with `AA` after it if the colour is not opaque.
pub fn hex(c: Color) -> String {
    if c.a() == 255 {
        format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
    } else {
        format!("#{:02X}{:02X}{:02X}{:02X}", c.r(), c.g(), c.b(), c.a())
    }
}

/// Colours by name, as people say them (CSS's names, and a few more).
pub const NAMED: &[(&str, u32)] = &[
    ("black", 0x000000),
    ("white", 0xFFFFFF),
    ("gray", 0x808080),
    ("grey", 0x808080),
    ("light gray", 0xD3D3D3),
    ("light grey", 0xD3D3D3),
    ("dark gray", 0x404040),
    ("dark grey", 0x404040),
    ("silver", 0xC0C0C0),
    ("red", 0xFF0000),
    ("dark red", 0x8B0000),
    ("crimson", 0xDC143C),
    ("maroon", 0x800000),
    ("pink", 0xFFC0CB),
    ("hot pink", 0xFF69B4),
    ("rose", 0xFF007F),
    ("orange", 0xFFA500),
    ("dark orange", 0xFF8C00),
    ("coral", 0xFF7F50),
    ("salmon", 0xFA8072),
    ("peach", 0xFFDAB9),
    ("yellow", 0xFFFF00),
    ("gold", 0xFFD700),
    ("light yellow", 0xFFFFE0),
    ("beige", 0xF5F5DC),
    ("tan", 0xD2B48C),
    ("brown", 0x8B4513),
    ("chocolate", 0xD2691E),
    ("olive", 0x808000),
    ("lime", 0x00FF00),
    ("green", 0x008000),
    ("light green", 0x90EE90),
    ("dark green", 0x006400),
    ("forest green", 0x228B22),
    ("mint", 0x98FF98),
    ("teal", 0x008080),
    ("turquoise", 0x40E0D0),
    ("cyan", 0x00FFFF),
    ("aqua", 0x00FFFF),
    ("sky blue", 0x87CEEB),
    ("light blue", 0xADD8E6),
    ("blue", 0x0000FF),
    ("royal blue", 0x4169E1),
    ("dark blue", 0x00008B),
    ("navy", 0x000080),
    ("indigo", 0x4B0082),
    ("purple", 0x800080),
    ("violet", 0xEE82EE),
    ("lavender", 0xE6E6FA),
    ("magenta", 0xFF00FF),
    ("fuchsia", 0xFF00FF),
];

/// Reads a colour: a name ("dark blue", "transparent"), `#RGB`, `#RRGGBB`, `#RRGGBBAA` (with or
/// without the `#`), or `rgb(r, g, b)`.
pub fn parse(text: &str) -> Option<Color> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    let lower = lower.replace(['-', '_'], " ");
    if lower == "transparent" || lower == "none" {
        return Some(Color::TRANSPARENT);
    }
    if let Some(&(_, rgb)) = NAMED.iter().find(|(n, _)| *n == lower || n.replace(' ', "") == lower.replace(' ', "")) {
        return Some(Color::hex(rgb));
    }
    if let Some(args) =
        lower.strip_prefix("rgb(").or_else(|| lower.strip_prefix("rgba(")).and_then(|r| r.strip_suffix(')'))
    {
        let v: alloc::vec::Vec<u32> = args.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        return match v[..] {
            [r, g, b] if r < 256 && g < 256 && b < 256 => Some(Color::rgb(r as u8, g as u8, b as u8)),
            [r, g, b, a] if r < 256 && g < 256 && b < 256 && a < 256 => {
                Some(Color::rgba(r as u8, g as u8, b as u8, a as u8))
            }
            _ => None,
        };
    }
    let digits = t.strip_prefix('#').unwrap_or(t);
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(digits, 16).ok()?;
    match digits.len() {
        3 => {
            let (r, g, b) = ((v >> 8) & 0xF, (v >> 4) & 0xF, v & 0xF);
            Some(Color::rgb((r * 17) as u8, (g * 17) as u8, (b * 17) as u8))
        }
        6 => Some(Color::hex(v)),
        8 => Some(Color::rgba((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8)),
        _ => None,
    }
}

/// The name of the named colour nearest `c` (for saying which colour is chosen): "dark blue",
/// "transparent".
pub fn name(c: Color) -> &'static str {
    if c.a() < 16 {
        return "transparent";
    }
    let dist = |rgb: u32| {
        let o = Color::hex(rgb);
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        // Weighted for how the eye tells colours apart (red and blue less than green).
        2 * d(c.r(), o.r()) + 4 * d(c.g(), o.g()) + 3 * d(c.b(), o.b())
    };
    NAMED.iter().min_by_key(|(_, rgb)| dist(*rgb)).map_or("black", |(n, _)| n)
}

/// The palette: rows of colours to click, from neutrals to the hues, light to dark.
pub const PALETTE: [u32; 28] = [
    0x000000, 0x464646, 0x7F7F7F, 0xC3C3C3, 0xFFFFFF, 0x880015, 0xED1C24, 0xFF7F27, 0xFFC90E, 0xFFF200, 0xB5E61D,
    0x22B14C, 0x00A2E8, 0x3F48CC, 0x99D9EA, 0xA349A4, 0xC8BFE7, 0xFFAEC9, 0xB97A57, 0xEFE4B0, 0x7092BE, 0x0B5394,
    0x2E7D32, 0x00BFA5, 0xFF4081, 0x7C4DFF, 0xFFD54F, 0x6D4C41,
];

/// A straight-alpha colour's premultiplied pixel, scaled by `opacity` (0 to 255).
pub fn premultiplied(c: Color, opacity: u8) -> u32 {
    gfx::color::scale(c.premul(), opacity as u32)
}

/// A premultiplied pixel with straight alpha again (for files, and colour pickers).
pub fn unpremultiply(px: u32) -> u32 {
    let a = px >> 24;
    match a {
        0 => 0,
        255 => px,
        _ => {
            let channel = |s: u32| (((px >> s) & 0xFF) * 255 + a / 2) / a;
            a << 24 | channel(16).min(255) << 16 | channel(8).min(255) << 8 | channel(0).min(255)
        }
    }
}

/// The largest difference between two pixels' channels (alpha too).
pub fn distance(a: u32, b: u32) -> u32 {
    [0, 8, 16, 24].iter().map(|s| ((a >> s) & 0xFF).abs_diff((b >> s) & 0xFF)).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trips() {
        for rgb in [0xFF0000, 0x00FF00, 0x0000FF, 0x123456, 0xFFFFFF, 0x000000, 0x808080, 0xFF8C00] {
            let c = Color::hex(rgb);
            assert_eq!(Hsv::of(c).to_color(255), c, "{rgb:06x}");
        }
        let h = Hsv::of(Color::hex(0x00FF00));
        assert_eq!((h.h, h.s, h.v), (120.0, 1.0, 1.0));
        assert_eq!(Hsv { h: 240.0, s: 1.0, v: 0.5 }.to_color(128), Color::rgba(0, 0, 128, 128));
        assert_eq!(Hsv { h: 360.0, s: 1.0, v: 1.0 }.to_color(255), Color::hex(0xFF0000));
    }

    #[test]
    fn colours_by_name_and_number() {
        assert_eq!(parse("red"), Some(Color::hex(0xFF0000)));
        assert_eq!(parse("Dark Blue"), Some(Color::hex(0x00008B)));
        assert_eq!(parse("darkblue"), Some(Color::hex(0x00008B)));
        assert_eq!(parse("sky-blue"), Some(Color::hex(0x87CEEB)));
        assert_eq!(parse("#FFF"), Some(Color::WHITE));
        assert_eq!(parse("1e90ff"), Some(Color::hex(0x1E90FF)));
        assert_eq!(parse("#11223380"), Some(Color::rgba(0x11, 0x22, 0x33, 0x80)));
        assert_eq!(parse("rgb(10, 20, 30)"), Some(Color::rgb(10, 20, 30)));
        assert_eq!(parse("transparent"), Some(Color::TRANSPARENT));
        assert_eq!(parse("rgb(300, 0, 0)"), None);
        assert_eq!(parse("plaid"), None);
        assert_eq!(parse("#12345"), None);
        assert_eq!(hex(Color::hex(0x1E90FF)), "#1E90FF");
        assert_eq!(hex(Color::rgba(1, 2, 3, 4)), "#01020304");
        assert_eq!(name(Color::hex(0xFE0101)), "red");
        assert_eq!(name(Color::hex(0x05008A)), "dark blue");
        assert_eq!(name(Color::TRANSPARENT), "transparent");
    }

    #[test]
    fn premultiplied_pixels() {
        assert_eq!(premultiplied(Color::hex(0xFF0000), 255), 0xFFFF_0000);
        assert_eq!(premultiplied(Color::hex(0xFF0000), 128), 0x8080_0000);
        for px in [0xFF12_3456, 0x8040_2010, 0x0000_0000, 0x0101_0101] {
            assert_eq!(gfx::color::premultiply(unpremultiply(px)), px, "{px:08x}");
        }
        assert_eq!(unpremultiply(0x8080_0000), 0x80FF_0000);
        assert_eq!(distance(0xFF10_2030, 0xFF10_2035), 5);
        assert_eq!(distance(0x0000_0000, 0xFF00_0000), 255);
    }
}

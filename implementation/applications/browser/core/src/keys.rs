// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The keys web pages get: for each of Veda's key events (a Linux input
//! code, the text the keyboard layout made of it, the modifiers held), the
//! XKB key code that names the key where it is (a page's `code`: Linux's
//! code plus 8) and the X11 key symbol that says what it means (its `key`
//! and the text it types).
//!
//! A key that types something means that: the symbol of its first
//! character (Latin-1's are the character's own number, others are
//! `0x0100_0000` past theirs). A key that types nothing has a symbol of its
//! own (Escape, the arrows, F1...). A key that typed nothing because a
//! modifier was held (Ctrl+C) means the character it has on a US keyboard,
//! which is what shortcuts name.

/// Veda's modifiers (`proto::display::modifiers`).
pub mod modifiers {
    pub const SHIFT: u32 = 1;
    pub const CTRL: u32 = 2;
    pub const ALT: u32 = 4;
    pub const SUPER: u32 = 8;
    pub const CAPS_LOCK: u32 = 16;
}

/// A key as a web page gets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    /// XKB's code: where the key is.
    pub keycode: u32,
    /// X11's symbol: what it means.
    pub keysym: u32,
}

/// X11's symbol for nothing.
pub const VOID: u32 = 0xff_ffff;

/// The key of `code`, which typed `text`, with `modifiers` held.
pub fn key(code: u16, text: &str, modifiers: u32) -> Key {
    let keycode = u32::from(code) + 8;
    let shift = modifiers & modifiers::SHIFT != 0;
    let typed = text.chars().next().filter(|c| !c.is_control());
    let keysym = keypad(code, typed)
        .or_else(|| own(code, shift))
        .or_else(|| typed.map(of_char))
        .or_else(|| us_layout(code, shift).map(of_char))
        .unwrap_or(VOID);
    Key { keycode, keysym }
}

/// The symbol of `c`.
pub fn of_char(c: char) -> u32 {
    let n = c as u32;
    if (0x20..=0x7e).contains(&n) || (0xa0..=0xff).contains(&n) { n } else { 0x0100_0000 + n }
}

/// The symbols of keys that type nothing.
fn own(code: u16, shift: bool) -> Option<u32> {
    Some(match code {
        1 => 0xff1b,                                 // Escape
        14 => 0xff08,                                // BackSpace
        15 if shift => 0xfe20,                       // ISO_Left_Tab
        15 => 0xff09,                                // Tab
        28 => 0xff0d,                                // Return
        29 => 0xffe3,                                // Control_L
        97 => 0xffe4,                                // Control_R
        42 => 0xffe1,                                // Shift_L
        54 => 0xffe2,                                // Shift_R
        56 => 0xffe9,                                // Alt_L
        100 => 0xffea,                               // Alt_R
        58 => 0xffe5,                                // Caps_Lock
        125 => 0xffeb,                               // Super_L
        126 => 0xffec,                               // Super_R
        127 => 0xff67,                               // Menu
        59..=68 => 0xffbe + u32::from(code - 59),    // F1 to F10
        87 => 0xffc8,                                // F11
        88 => 0xffc9,                                // F12
        183..=194 => 0xffca + u32::from(code - 183), // F13 to F24
        69 => 0xff7f,                                // Num_Lock
        70 => 0xff14,                                // Scroll_Lock
        99 => 0xff61,                                // Print
        119 => 0xff13,                               // Pause
        102 => 0xff50,                               // Home
        103 => 0xff52,                               // Up
        104 => 0xff55,                               // Prior
        105 => 0xff51,                               // Left
        106 => 0xff53,                               // Right
        107 => 0xff57,                               // End
        108 => 0xff54,                               // Down
        109 => 0xff56,                               // Next
        110 => 0xff63,                               // Insert
        111 => 0xffff,                               // Delete
        113 => 0x1008_ff12,                          // XF86AudioMute
        114 => 0x1008_ff11,                          // XF86AudioLowerVolume
        115 => 0x1008_ff13,                          // XF86AudioRaiseVolume
        163 => 0x1008_ff17,                          // XF86AudioNext
        164 => 0x1008_ff14,                          // XF86AudioPlay
        165 => 0x1008_ff16,                          // XF86AudioPrev
        166 => 0x1008_ff15,                          // XF86AudioStop
        158 => 0x1008_ff26,                          // XF86Back
        159 => 0x1008_ff27,                          // XF86Forward
        172 => 0x1008_ff18,                          // XF86HomePage
        173 => 0x1008_ff73,                          // XF86Reload
        217 => 0x1008_ff1b,                          // XF86Search
        _ => return None,
    })
}

/// The keypad's keys: digits while they type them (Num Lock), else where
/// they move.
fn keypad(code: u16, typed: Option<char>) -> Option<u32> {
    let digit = |d: u32| typed.is_some_and(|c| c.is_ascii_digit()).then_some(0xffb0 + d);
    Some(match code {
        71 => digit(7).unwrap_or(0xff95), // KP_7, KP_Home
        72 => digit(8).unwrap_or(0xff97), // KP_8, KP_Up
        73 => digit(9).unwrap_or(0xff9a), // KP_9, KP_Prior
        75 => digit(4).unwrap_or(0xff96), // KP_4, KP_Left
        76 => digit(5).unwrap_or(0xff9d), // KP_5, KP_Begin
        77 => digit(6).unwrap_or(0xff98), // KP_6, KP_Right
        79 => digit(1).unwrap_or(0xff9c), // KP_1, KP_End
        80 => digit(2).unwrap_or(0xff99), // KP_2, KP_Down
        81 => digit(3).unwrap_or(0xff9b), // KP_3, KP_Next
        82 => digit(0).unwrap_or(0xff9e), // KP_0, KP_Insert
        83 => {
            if typed.is_some() {
                0xffae // KP_Decimal
            } else {
                0xff9f // KP_Delete
            }
        }
        55 => 0xffaa,  // KP_Multiply
        74 => 0xffad,  // KP_Subtract
        78 => 0xffab,  // KP_Add
        96 => 0xff8d,  // KP_Enter
        98 => 0xffaf,  // KP_Divide
        117 => 0xffbd, // KP_Equal
        _ => return None,
    })
}

/// What `code` types on a US keyboard, with Shift or without.
fn us_layout(code: u16, shift: bool) -> Option<char> {
    const ROWS: [(u16, &str, &str); 4] = [
        (2, "1234567890-=", "!@#$%^&*()_+"),
        (16, "qwertyuiop[]", "QWERTYUIOP{}"),
        (30, "asdfghjkl;'`", "ASDFGHJKL:\"~"),
        (44, "zxcvbnm,./", "ZXCVBNM<>?"),
    ];
    match code {
        43 => return Some(if shift { '|' } else { '\\' }),
        57 => return Some(' '),
        _ => {}
    }
    ROWS.iter().find_map(|&(first, plain, shifted)| {
        let i = usize::from(code.checked_sub(first)?);
        let row = if shift { shifted } else { plain };
        row.chars().nth(i)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_that_type_mean_what_they_type() {
        assert_eq!(key(30, "a", 0), Key { keycode: 38, keysym: 'a' as u32 });
        assert_eq!(key(30, "A", modifiers::SHIFT).keysym, 'A' as u32);
        assert_eq!(key(57, " ", 0).keysym, 0x20);
        // Another layout's character, and one beyond Latin-1.
        assert_eq!(key(26, "ü", 0).keysym, 0xfc);
        assert_eq!(key(18, "е", 0).keysym, 0x0100_0435);
        assert_eq!(key(18, "€", 0).keysym, 0x0100_20ac);
    }

    #[test]
    fn keys_that_type_nothing_have_their_own() {
        assert_eq!(key(1, "\u{1b}", 0).keysym, 0xff1b);
        assert_eq!(key(28, "\r", 0).keysym, 0xff0d);
        assert_eq!(key(14, "", 0).keysym, 0xff08);
        assert_eq!(key(15, "\t", 0).keysym, 0xff09);
        assert_eq!(key(15, "", modifiers::SHIFT).keysym, 0xfe20);
        assert_eq!(key(105, "", 0).keysym, 0xff51);
        assert_eq!(key(59, "", 0).keysym, 0xffbe);
        assert_eq!(key(88, "", 0).keysym, 0xffc9);
        assert_eq!(key(42, "", 0).keysym, 0xffe1);
        assert_eq!(key(1, "", 0).keycode, 9, "Escape is XKB's 9");
    }

    #[test]
    fn shortcuts_mean_the_us_keyboard() {
        assert_eq!(key(46, "", modifiers::CTRL).keysym, 'c' as u32);
        assert_eq!(key(46, "", modifiers::CTRL | modifiers::SHIFT).keysym, 'C' as u32);
        assert_eq!(key(2, "", modifiers::CTRL | modifiers::SHIFT).keysym, '!' as u32);
        assert_eq!(key(53, "", modifiers::CTRL).keysym, '/' as u32);
        // A control character typed counts as nothing typed.
        assert_eq!(key(30, "\u{1}", modifiers::CTRL).keysym, 'a' as u32);
    }

    #[test]
    fn the_keypad_types_digits_or_moves() {
        assert_eq!(key(71, "7", 0).keysym, 0xffb7);
        assert_eq!(key(71, "", 0).keysym, 0xff95);
        assert_eq!(key(83, ".", 0).keysym, 0xffae);
        assert_eq!(key(83, "", 0).keysym, 0xff9f);
        assert_eq!(key(96, "\r", 0).keysym, 0xff8d);
        assert_eq!(key(55, "*", 0).keysym, 0xffaa);
        assert_eq!(key(999, "", 0).keysym, VOID);
    }
}

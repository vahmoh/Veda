// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What programs write, as the Terminal shows it.

use alloc::format;
use alloc::string::String;

use crate::screen::{Screen, Style, color};

fn written(chunks: &[&str]) -> Screen {
    let mut s = Screen::new();
    for c in chunks {
        s.write(c);
    }
    s
}

/// The text of the line being written.
fn text(s: &Screen) -> String {
    s.partial().iter().map(|c| c.ch).collect()
}

/// The text of completed line `n` (0 is the first).
fn line(s: &Screen, n: u64) -> String {
    s.line(s.base() + n).unwrap().iter().map(|c| c.ch).collect()
}

/// The style of the first `ch` on the line being written.
fn style_of(s: &Screen, ch: char) -> Style {
    s.partial().iter().find(|c| c.ch == ch).unwrap().style
}

#[test]
fn what_compilers_and_tools_write() {
    // GCC: a bold location, a bold red "error:", resets after each.
    let s = written(&["\x1b[01m\x1b[Khello.c:3:5:\x1b[m\x1b[K \x1b[01;31m\x1b[Kerror: \x1b[m\x1b[Kexpected ';'\n"]);
    assert_eq!(line(&s, 0), "hello.c:3:5: error: expected ';'");
    let cells = s.line(s.base()).unwrap();
    assert_eq!(cells[0].style, Style::BOLD);
    assert_eq!(cells[13].style, Style::fg(color::RED).bold());
    assert_eq!(cells[20].style, Style::PLAIN);
    // GCC's hyperlinks (OSC 8), ended by BEL or by ESC \: only the text.
    let s = written(&[
        "[\x1b]8;;https://gcc.gnu.org/onlinedocs/gcc/Warning-Options.html#index-Wunused\x07-Wunused\x1b]8;;\x07]\n",
        "\x1b]8;;https://example.org/\x1b\\link\x1b]8;;\x1b\\ done\n",
    ]);
    assert_eq!((line(&s, 0).as_str(), line(&s, 1).as_str()), ("[-Wunused]", "link done"));
    // Cargo redraws its progress line: back to its start, the rest erased.
    let s =
        written(&["\x1b[1m\x1b[36m    Building\x1b[0m [=====>   ] 3/7: foo\r\x1b[K\x1b[1m\x1b[92m    Finished\x1b[0m"]);
    assert_eq!(text(&s), "    Finished");
    assert_eq!(style_of(&s, 'F'), Style::fg(color::BRIGHT_GREEN).bold());
    // ls: bold blue directories; rustc: 256-colour bright red.
    let s = written(&["\x1b[0m\x1b[01;34mdir\x1b[0m  \x1b[38;5;9merror"]);
    assert_eq!(style_of(&s, 'd'), Style::fg(color::BLUE).bold());
    assert_eq!(style_of(&s, 'e'), Style::fg(color::BRIGHT_RED));
    // Character sets and saved cursors are recognised and left out.
    assert_eq!(text(&written(&["\x1b(Bplain\x1b7\x1b8 text\x1bc"])), "plain text");
}

#[test]
fn true_colour() {
    // With semicolons, and with colons (an empty colour space, none, or one).
    for seq in ["\x1b[38;2;255;0;0mX", "\x1b[38:2::255:0:0mX", "\x1b[38:2:255:0:0mX", "\x1b[38:2:0:255:0:0mX"] {
        assert_eq!(style_of(&written(&[seq]), 'X'), Style::fg(color::BRIGHT_RED), "{seq:?}");
    }
    // The parameters after it still count (a misread colour once reset
    // the style and turned on dim).
    let x = style_of(&written(&["\x1b[38;2;0;0;0;1;4mX"]), 'X');
    assert_eq!((x.fg, x.bold, x.underline, x.dim), (color::BLACK, true, true, false));
    assert_eq!(style_of(&written(&["\x1b[48;2;0;0;170mX"]), 'X').bg, color::BLUE);
    assert_eq!(style_of(&written(&["\x1b[48:2::200:200:200mX"]), 'X').bg, color::BRIGHT_WHITE);
    // Cut short: nothing changes.
    assert_eq!(style_of(&written(&["\x1b[38;2;255mX"]), 'X'), Style::PLAIN);
}

#[test]
fn palette_colours() {
    let cases = [
        ("\x1b[31mX", color::RED),
        ("\x1b[91mX", color::BRIGHT_RED),
        ("\x1b[38;5;196mX", color::BRIGHT_RED),
        ("\x1b[38:5:196mX", color::BRIGHT_RED),
        ("\x1b[38;5;3mX", color::YELLOW),
        ("\x1b[38;5;232mX", color::GREY),
        ("\x1b[38;5;255mX", color::WHITE),
        ("\x1b[38;5;300mX", color::WHITE),
    ];
    for (seq, fg) in cases {
        assert_eq!(style_of(&written(&[seq]), 'X').fg, fg, "{seq:?}");
    }
    let s = written(&["\x1b[42;97mX\x1b[49;39mY"]);
    assert_eq!((style_of(&s, 'X').bg, style_of(&s, 'X').fg), (color::GREEN, color::BRIGHT_WHITE));
    assert_eq!(style_of(&s, 'Y'), Style::PLAIN);
}

#[test]
fn attributes() {
    let s = written(&["\x1b[1;2;4;7mA\x1b[22mB\x1b[24;27mC\x1b[4:3mD\x1b[4:0mE\x1b[mF"]);
    let a = style_of(&s, 'A');
    assert!(a.bold && a.dim && a.underline && a.inverse);
    let b = style_of(&s, 'B');
    assert!(!b.bold && !b.dim && b.underline && b.inverse);
    assert_eq!(style_of(&s, 'C'), Style::PLAIN);
    // Underline styles (curly, here) underline; `4:0` is none.
    assert!(style_of(&s, 'D').underline);
    assert!(!style_of(&s, 'E').underline);
    assert_eq!(style_of(&s, 'F'), Style::PLAIN);
    // An underline colour is read and left out, with what follows it.
    let s = written(&["\x1b[58;2;255;0;0;1mX\x1b[58:5:9;3mY"]);
    assert_eq!(style_of(&s, 'X'), Style::BOLD);
    assert_eq!(style_of(&s, 'Y'), Style::BOLD);
}

#[test]
fn private_sequences_change_nothing() {
    // xterm's modifyOtherKeys (`CSI > 4 ; 2 m`) and DEC's selective erase.
    let s = written(&["\x1b[31m\x1b[>4;2mX\x1b[?2KY"]);
    assert_eq!(style_of(&s, 'X'), Style::fg(color::RED));
    assert_eq!(text(&s), "XY");
}

#[test]
fn sequences_across_writes() {
    let s = written(&["\x1b", "[3", "1mX", "\x1b]0;ti", "tle\x07Y\x1b[", "0mZ"]);
    assert_eq!(text(&s), "XYZ");
    assert_eq!(style_of(&s, 'X'), Style::fg(color::RED));
    assert_eq!(style_of(&s, 'Y'), Style::fg(color::RED));
    assert_eq!(style_of(&s, 'Z'), Style::PLAIN);
    // A character at a time, as a pipe may deliver it.
    let mut s = Screen::new();
    for c in "\x1b[1;32mok\x1b[0m done".chars() {
        let mut b = [0; 4];
        s.write(c.encode_utf8(&mut b));
    }
    assert_eq!(text(&s), "ok done");
    assert_eq!(style_of(&s, 'o'), Style::fg(color::GREEN).bold());
    assert_eq!(style_of(&s, 'd'), Style::PLAIN);
    // A line's end forgets a sequence left unfinished.
    let mut s = Screen::new();
    s.write("\x1b[3");
    s.finish_line();
    s.write("1mX");
    assert_eq!(text(&s), "1mX");
}

#[test]
fn cancelled_and_hostile_sequences() {
    // CAN and SUB end a sequence.
    let s = written(&["\x1b[31\x18X\x1b[32\x1aY"]);
    assert_eq!(text(&s), "XY");
    assert_eq!(style_of(&s, 'X'), Style::PLAIN);
    // Very many and very large parameters, and a very long hyperlink.
    let many = format!("\x1b[{}31mX", "1;".repeat(1000));
    assert_eq!(text(&written(&[&many])), "X");
    assert_eq!(text(&written(&["\x1b[99999999999999999999;38;5;99999999999mX\x1b[K"])), "X");
    let long = format!("\x1b]8;;{}\x07X", "u".repeat(100_000));
    assert_eq!(text(&written(&[&long])), "X");
    // Control characters other than the four the screen keeps are not
    // drawn, nor are C1 controls written as characters.
    assert_eq!(text(&written(&["bell\x07 nul\x00 del\x7f a\u{9b}31mb"])), "bell nul del a31mb");
}

#[test]
fn erase_in_line() {
    assert_eq!(text(&written(&["abcdef\r\x1b[Kxy"])), "xy");
    assert_eq!(text(&written(&["abcdef\rab\x1b[0K"])), "ab");
    assert_eq!(text(&written(&["abcdef\x08\x08\x08\x1b[1K"])), "    ef");
    assert_eq!(text(&written(&["abcdef\x1b[2K"])), "");
    assert_eq!(text(&written(&["abcdef\r\x1b[3K"])), "abcdef");
}

#[test]
fn tabs_returns_and_backspaces() {
    assert_eq!(text(&written(&["a\tb"])), "a       b");
    assert_eq!(text(&written(&["12345678\tx"])), "12345678        x");
    assert_eq!(text(&written(&["progress 10%\rprogress 20%\rdone        "])), "done        ");
    assert_eq!(text(&written(&["abc\x08\x08X"])), "aXc");
}

#[test]
fn unicode_text() {
    let s = written(&["h\u{e9}llo w\u{f6}rld \u{4e2d}\u{6587} \u{1f600}"]);
    assert_eq!(text(&s), "h\u{e9}llo w\u{f6}rld \u{4e2d}\u{6587} \u{1f600}");
    let s = written(&["\x1b[33m\u{e9}t\u{e9}\x1b[0m"]);
    assert_eq!(style_of(&s, '\u{e9}'), Style::fg(color::YELLOW));
}

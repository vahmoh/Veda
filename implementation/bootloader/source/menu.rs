// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The boot menu: what to do with a Veda that is not installed. `menu=` in
//! `BOOT.CFG` asks for it, as the live system's does.
//!
//! It comes before the loader does anything to the screen, on the firmware's
//! text console and keyboard, which every PC has and which take no font or
//! driver of the loader's. The loader then sets its own mode and clears the
//! screen (`blank`) as it does without a menu, whatever the menu left there.
//!
//! The first entry starts by itself once the seconds `menu=` gives have
//! passed without a key: a machine nobody is at, or whose keyboard the
//! firmware does not read, starts all the same.

use core::fmt::{self, Write as _};

use crate::Firmware;
use crate::uefi::{
    CARRIAGE_RETURN, InputKey, SCAN_DOWN, SCAN_UP, SimpleTextInput, SimpleTextOutput, TEXT_GREY, TEXT_INVERSE,
    TEXT_WHITE, is_error,
};

/// What the menu offers. Trying Veda goes on to start the system, which is
/// live as `BOOT.CFG`'s command line says; nothing installs it yet.
const TRY: usize = 0;
const INSTALL: usize = 1;
const ENTRIES: [&str; 2] = ["Try without installing", "Install Veda"];

/// The menu's size on the console, and where its lines are in it.
const WIDTH: usize = 40;
const HEIGHT: usize = 7;
const FIRST_ENTRY: usize = 2;
const HINT: usize = 5;
const COUNTDOWN: usize = 6;

/// How many times a second the keyboard is looked at.
const POLLS_PER_SECOND: u64 = 50;

/// The firmware's text console, with the menu in the middle of it.
struct Console<'a> {
    fw: &'a Firmware,
    input: *mut SimpleTextInput,
    output: *mut SimpleTextOutput,
    left: usize,
    top: usize,
}

impl Console<'_> {
    /// Prints `text` on line `line` of the menu from `column` on, in the
    /// colours `attribute`.
    fn put(&mut self, column: usize, line: usize, attribute: usize, text: fmt::Arguments) {
        // SAFETY: the firmware's console, valid while boot services are.
        unsafe {
            ((*self.output).set_attribute)(self.output, attribute);
            ((*self.output).set_cursor_position)(self.output, self.left + column, self.top + line);
        }
        let _ = self.write_fmt(text);
    }

    /// Shows the entries, `selected` highlighted.
    fn entries(&mut self, selected: usize) {
        for (i, entry) in ENTRIES.iter().enumerate() {
            let colours = if i == selected { TEXT_INVERSE } else { TEXT_GREY };
            self.put(0, FIRST_ENTRY + i, colours, format_args!(" {entry:<width$}", width = WIDTH - 1));
        }
    }

    /// The next key pressed, if there is one.
    fn key(&self) -> Option<InputKey> {
        let mut key = InputKey::default();
        // SAFETY: the firmware's console, and room for its key.
        let s = unsafe { ((*self.input).read_key_stroke)(self.input, &mut key) };
        (!is_error(s)).then_some(key)
    }
}

impl fmt::Write for Console<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.fw.print(s);
        Ok(())
    }
}

/// The size of the console `output`: its columns and rows.
fn console_size(output: *mut SimpleTextOutput) -> (usize, usize) {
    let (mut columns, mut rows) = (0, 0);
    // SAFETY: the firmware's console, and its current mode.
    let s = unsafe { ((*output).query_mode)(output, (*(*output).mode).mode as usize, &mut columns, &mut rows) };
    // What the specification has every console offer.
    if is_error(s) { (80, 25) } else { (columns, rows) }
}

/// Shows the menu until the system is to start, for `seconds` at most if no
/// key is pressed; for as long as it takes once one is.
pub fn show(fw: &Firmware, seconds: u32) {
    let (input, output) = (fw.st.con_in, fw.st.con_out);
    if input.is_null() || output.is_null() {
        crate::log!("menu: the firmware has no console: {} starts", ENTRIES[TRY]);
        return;
    }
    let (columns, rows) = console_size(output);
    let (left, top) = (columns.saturating_sub(WIDTH) / 2, rows.saturating_sub(HEIGHT) / 2);
    let mut console = Console { fw, input, output, left, top };
    // SAFETY: the firmware's console. A key still on its way from the
    // firmware's own menus is no answer to this one.
    unsafe {
        ((*input).reset)(input, false);
        ((*output).set_attribute)(output, TEXT_GREY);
        ((*output).clear_screen)(output);
        ((*output).enable_cursor)(output, false);
    }
    console.put(1, 0, TEXT_WHITE, format_args!("Veda"));
    console.put(1, HINT, TEXT_GREY, format_args!("Up and Down to choose, Enter to start"));
    let mut selected = TRY;
    console.entries(selected);
    crate::log!("menu: {} starts in {} s unless a key is pressed", ENTRIES[TRY], seconds);

    // The polls until the first entry starts by itself, as long as no key
    // has been pressed.
    let mut countdown = Some(seconds as u64 * POLLS_PER_SECOND);
    loop {
        if countdown == Some(0) {
            crate::log!("menu: no key pressed: {} starts", ENTRIES[TRY]);
            break;
        }
        if let Some(polls) = countdown
            && polls.is_multiple_of(POLLS_PER_SECOND)
        {
            console.put(1, COUNTDOWN, TEXT_GREY, format_args!("Starting in {} s ", polls / POLLS_PER_SECOND));
        }
        let Some(key) = console.key() else {
            countdown = countdown.map(|polls| polls - 1);
            // SAFETY: stalling is always safe during boot services.
            unsafe { (fw.bs.stall)(1_000_000 / POLLS_PER_SECOND as usize) };
            continue;
        };
        if countdown.take().is_some() {
            console.put(0, COUNTDOWN, TEXT_GREY, format_args!("{:width$}", "", width = WIDTH));
        }
        let moved = match key.scan_code {
            SCAN_UP => selected.saturating_sub(1),
            SCAN_DOWN => (selected + 1).min(ENTRIES.len() - 1),
            _ => selected,
        };
        if moved != selected {
            selected = moved;
            console.entries(selected);
            crate::log!("menu: {} selected", ENTRIES[selected]);
        } else if key.unicode_char == CARRIAGE_RETURN && selected == INSTALL {
            crate::log!("menu: {} chosen, which does nothing yet", ENTRIES[selected]);
        } else if key.unicode_char == CARRIAGE_RETURN {
            crate::log!("menu: {} chosen", ENTRIES[selected]);
            break;
        }
    }
    // SAFETY: the firmware's console. As it was, for the messages of a
    // start that fails.
    unsafe {
        ((*output).set_attribute)(output, TEXT_GREY);
        ((*output).clear_screen)(output);
    }
}

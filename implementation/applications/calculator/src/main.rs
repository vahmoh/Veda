// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Calculator: Standard, Scientific and Programmer modes and a unit converter, with exact
//! arithmetic, a live result as the expression is typed, a history and a memory.
//!
//! What it works out and decides (numbers, expressions, the keys' edits, the keypads, what it
//! remembers) is the host-tested `calculator-core` crate; this is the window around it: the
//! display ([`display`]), the keys ([`keypad`]), the history and memory panel, Programmer
//! mode's bases and bits, the converter, the keyboard, and [`agent`], which lets the voice
//! agent calculate and convert.
//!
//! Everything works from the keyboard: digits and operators as typed, Enter for `=`,
//! Backspace, Delete for CE, Esc for C, F9 to change a sign, the arrows to move in the
//! expression, Ctrl+C and Ctrl+V, Ctrl+H for the history and Alt+1 to Alt+4 for the modes.

mod agent;
mod display;
mod keypad;

use std::path::Path;

use calculator_core::keypad::{Action, Options};
use calculator_core::units::{CATEGORIES, convert};
use calculator_core::{Base, Calculator, Key, Mode, Number, Style, format_word, store};
use vgfx::{Align, Color, Rect};
use vproto::display::{Cursor, modifiers};
use vproto::input::keys;
use vui::{App, Font, Icon, MenuItem, Ui, WindowSpec};

use display::Drawn;

/// Where the Calculator keeps what it remembers.
const STATE_FILE: &str = "/home/user/.config/calculator/state";
/// The largest state file read (bytes): a full history is some 10 KB.
const STATE_MAX: usize = 256 * 1024;
/// How long after a change what is remembered is written (ns).
const SAVE_DELAY_NS: u64 = 3_000_000_000;
/// How long a toast shows (ns).
const TOAST_NS: u64 = 1_800_000_000;
/// The width from which the history shows beside the keypad.
const PANEL_FROM_WIDTH: i32 = 640;

/// The panel's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    History,
    Memory,
}

/// A dialog over the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modal {
    Shortcuts,
    About,
}

const SHORTCUTS: [(&str, &str); 17] = [
    ("0–9 . + − * / ^", "Digits and operators"),
    ("Enter or =", "Work out the expression"),
    ("Backspace", "Delete before the caret"),
    ("Delete", "Clear the number (CE)"),
    ("Esc", "Clear everything (C)"),
    ("F9", "Change the sign"),
    ("% !", "Percent, factorial"),
    ("@", "Square root"),
    ("( )", "Parentheses"),
    ("← → Home End", "Move in the expression"),
    ("sin, ln, pi, ...", "Type names of functions"),
    ("A–F", "Hexadecimal digits (Programmer)"),
    ("F5 F6 F7 F8", "Hexadecimal, decimal, octal, binary"),
    ("Ctrl+C / Ctrl+V", "Copy the result / paste"),
    ("Ctrl+H", "Show or hide the history"),
    ("Alt+1 … Alt+4", "Standard, Scientific, Programmer, Converter"),
    ("F1", "These shortcuts"),
];

/// What the Calculator remembers, kept in its file.
struct Store {
    /// What the file holds (so that nothing is written that is already there).
    written: String,
    /// When to write next, if something changed.
    due: Option<u64>,
}

impl Store {
    fn open() -> Store {
        Store { written: String::new(), due: None }
    }

    fn load(&mut self) -> Calculator {
        match apps_common::store::read(Path::new(STATE_FILE), STATE_MAX) {
            Ok(bytes) => {
                self.written = String::from_utf8_lossy(&bytes).into_owned();
                store::load(&self.written)
            }
            Err(_) => Calculator::new(),
        }
    }

    /// Writes the calculator's state if it changed.
    fn save(&mut self, calc: &Calculator) {
        self.due = None;
        let text = store::save(calc);
        if text == self.written {
            return;
        }
        match apps_common::store::write(Path::new(STATE_FILE), text.as_bytes()) {
            Ok(()) => self.written = text,
            Err(e) => eprintln!("could not save the state: {e}"),
        }
    }
}

struct CalcApp {
    calc: Calculator,
    store: Store,
    /// A key pressed on the keyboard, lit on screen until the time.
    flash: Option<(Action, u64)>,
    toast: Option<(String, u64)>,
    /// The history panel is shown (always beside the keypad in a wide window).
    panel: bool,
    tab: Tab,
    /// Programmer mode shows its bits instead of its keypad.
    bits: bool,
    modal: Option<Modal>,
    /// Where the display's large line was last drawn.
    drawn: Drawn,
}

impl CalcApp {
    fn new() -> CalcApp {
        let mut store = Store::open();
        let calc = store.load();
        CalcApp {
            calc,
            store,
            flash: None,
            toast: None,
            panel: false,
            tab: Tab::History,
            bits: false,
            modal: None,
            drawn: Drawn::default(),
        }
    }

    /// Something to remember changed: written a little later.
    fn changed(&mut self, now: u64) {
        self.store.due = Some(now + SAVE_DELAY_NS);
    }

    fn show_toast(&mut self, text: &str, now: u64) {
        self.toast = Some((text.into(), now + TOAST_NS));
    }

    /// Does what a keypad key does.
    fn act(&mut self, action: Action, now: u64) {
        match action {
            Action::Key(k) => self.calc.press(k),
            Action::Second => self.calc.second = !self.calc.second,
            Action::Hyperbolic => self.calc.hyperbolic = !self.calc.hyperbolic,
            Action::Swap => self.calc.converter.swap(),
        }
        self.changed(now);
    }

    /// Presses `key` as its key on the keyboard did: it lights up on screen.
    fn press_typed(&mut self, key: Key, now: u64) {
        self.calc.press(key);
        self.flash = Some((Action::Key(key), now + keypad::FLASH_NS));
        self.changed(now);
    }

    fn set_mode(&mut self, mode: Mode, now: u64) {
        self.calc.set_mode(mode);
        self.changed(now);
    }

    /// What Ctrl+C copies: the value shown, in plain text (in the base shown, in Programmer
    /// mode), or the expression if it has none.
    fn copy_text(&self) -> String {
        let c = &self.calc;
        match c.mode() {
            Mode::Programmer => match c.word_value() {
                Some(v) => format_word(v, c.base(), c.word_size(), false).replace('\u{2212}', "-"),
                None => c.shown_expression(),
            },
            _ => match c.current_value() {
                Some(v) => calculator_core::format::format(v, &Style::PLAIN),
                None => c.shown_expression(),
            },
        }
    }

    fn copy(&mut self, ui: &mut Ui) {
        let text = self.copy_text();
        let now = ui.now();
        match apps_common::clipboard::copy(ui, &text) {
            Ok(()) => self.show_toast("Copied", now),
            Err(e) => self.show_toast(&format!("Couldn't copy: {e}"), now),
        }
    }

    fn paste(&mut self, ui: &mut Ui) {
        let text = ui.clipboard();
        if !text.trim().is_empty() {
            self.calc.paste(&text);
            self.changed(ui.now());
        }
    }

    // ---- keyboard --------------------------------------------------------------

    fn keyboard(&mut self, ui: &mut Ui) {
        let now = ui.now();
        for k in ui.input.keys.clone() {
            let ctrl = k.modifiers & modifiers::CTRL != 0;
            let alt = k.modifiers & modifiers::ALT != 0;
            match k.code {
                keys::C if ctrl => self.copy(ui),
                keys::V if ctrl => self.paste(ui),
                keys::H if ctrl => self.panel = !self.panel,
                keys::KEY_1 | keys::KEY_2 | keys::KEY_3 | keys::KEY_4 if alt => {
                    self.set_mode(Mode::ALL[(k.code - keys::KEY_1) as usize], now);
                }
                keys::F1 => self.modal = Some(Modal::Shortcuts),
                keys::F5 | keys::F6 | keys::F7 | keys::F8 if self.calc.mode() == Mode::Programmer => {
                    self.calc.set_base(Base::ALL[(k.code - keys::F5) as usize]);
                    self.changed(now);
                }
                _ if ctrl || alt => {}
                keys::ENTER | keys::KPENTER => self.press_typed(Key::Equals, now),
                keys::BACKSPACE => self.press_typed(Key::Backspace, now),
                keys::ESC => self.press_typed(Key::Clear, now),
                keys::F9 => self.press_typed(Key::Negate, now),
                keys::DELETE => {
                    // Within the expression it deletes forwards; at its end it is CE.
                    let at_end = self.calc.screen().caret.is_none();
                    self.press_typed(if at_end { Key::ClearEntry } else { Key::Delete }, now);
                }
                keys::LEFT => self.calc.press(Key::Left),
                keys::RIGHT => self.calc.press(Key::Right),
                keys::HOME => self.calc.press(Key::Home),
                keys::END => self.calc.press(Key::End),
                _ => {
                    let Some(ch) = k.ch else { continue };
                    if ch == '@' && self.calc.mode().is_decimal() {
                        self.press_typed(Key::SquareRoot, now);
                        continue;
                    }
                    let mut buf = [0u8; 4];
                    self.calc.type_text(ch.encode_utf8(&mut buf));
                    if let Some(key) = typed_key(ch, self.calc.mode()) {
                        self.flash = Some((Action::Key(key), now + keypad::FLASH_NS));
                    }
                    self.changed(now);
                }
            }
        }
    }

    // ---- the window's parts ----------------------------------------------------

    fn header(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme().clone();
        let now = ui.now();
        let name = self.calc.mode().name();
        let w = ui.measure(name, Font::Bold, 17.0) as i32 + 44;
        let button = Rect::new(r.x + 8, r.y + 6, w, r.h - 12);
        let id = ui.id("mode-button");
        let resp = ui.interact(id, button);
        let hover = ui.animate(id, if resp.hovered { 1.0 } else { 0.0 }, 9.0);
        if hover > 0.0 {
            ui.canvas.fill_rounded_rect(button, t.radius, Color::rgba(255, 255, 255, (hover * 22.0) as u8));
        }
        ui.label(Rect::new(button.x + 12, button.y, w, button.h), name, Font::Bold, 17.0, t.text, Align::Left);
        ui.icon(Rect::new(button.right() - 26, button.y, 18, button.h), Icon::ChevronDown, 14.0, t.text_dim);
        if resp.hovered {
            ui.set_cursor(Cursor::Hand);
        }
        if resp.clicked {
            ui.open_context_menu("modes", button.x, button.bottom() + 4);
        }
        let items: Vec<MenuItem> = Mode::ALL
            .iter()
            .enumerate()
            .map(|(i, m)| MenuItem::new(m.name()).shortcut(&format!("Alt+{}", i + 1)).checked(*m == self.calc.mode()))
            .collect();
        if let Some(i) = ui.context_menu("modes", &items) {
            self.set_mode(Mode::ALL[i], now);
        }

        let more = Rect::new(r.right() - 44, r.y + 6, 36, r.h - 12);
        if ui.glyph_button(more, "More", |c, r, color| {
            let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
            for dx in [-6.0, 0.0, 6.0] {
                c.fill_circle(cx + dx, cy, 1.8, color);
            }
        }) {
            ui.open_context_menu("more", more.x - 150, more.bottom() + 4);
        }
        let menu = [
            MenuItem::new("Copy").shortcut("Ctrl+C"),
            MenuItem::new("Paste").shortcut("Ctrl+V"),
            MenuItem::separator(),
            MenuItem::new("Digit Grouping").checked(self.calc.style.grouping),
            MenuItem::separator(),
            MenuItem::new("Keyboard Shortcuts").shortcut("F1"),
            MenuItem::new("About Calculator"),
        ];
        match ui.context_menu("more", &menu) {
            Some(0) => self.copy(ui),
            Some(1) => self.paste(ui),
            Some(3) => {
                self.calc.style.grouping = !self.calc.style.grouping;
                self.changed(now);
            }
            Some(5) => self.modal = Some(Modal::Shortcuts),
            Some(6) => self.modal = Some(Modal::About),
            _ => {}
        }
        let history = Rect::new(more.x - 40, r.y + 6, 36, r.h - 12);
        if self.calc.mode() != Mode::Converter && ui.icon_button(history, Icon::History, "History (Ctrl+H)") {
            self.panel = !self.panel;
        }
    }

    /// The display, with what is clicked in it.
    fn display(&mut self, ui: &mut Ui, r: Rect) {
        let screen = self.calc.screen();
        self.drawn = display::draw(ui, r, &screen, self.calc.memory().is_some());
        let line = self.drawn.line;
        let resp = ui.interact(ui.id("display"), line);
        if resp.pressed {
            let at = display::index_at(ui, &self.drawn, ui.input.pointer.map_or(0, |p| p.0));
            if !screen.is_result {
                self.calc.set_caret(at);
            }
        }
        if resp.right_clicked
            && let Some((x, y)) = ui.input.pointer
        {
            ui.open_context_menu("display-menu", x, y);
        }
        match ui.context_menu(
            "display-menu",
            &[MenuItem::new("Copy").shortcut("Ctrl+C"), MenuItem::new("Paste").shortcut("Ctrl+V")],
        ) {
            Some(0) => self.copy(ui),
            Some(1) => self.paste(ui),
            _ => {}
        }
    }

    /// A small text button of a row (memory keys, settings); returns whether it was clicked.
    fn small_button(ui: &mut Ui, r: Rect, label: &str, tip: &str, enabled: bool, on: bool) -> bool {
        let t = ui.theme().clone();
        let id = ui.id(tip) ^ ((r.x as u64) << 20) ^ 0x5b7;
        let resp = if enabled { ui.interact(id, r) } else { vui::Response::default() };
        let hover = ui.animate(id, if resp.hovered { 1.0 } else { 0.0 }, 9.0);
        if on {
            ui.canvas.fill_rounded_rect(r, t.radius, t.accent.with_alpha(60));
        } else if hover > 0.0 {
            ui.canvas.fill_rounded_rect(r, t.radius, Color::rgba(255, 255, 255, (hover * 20.0) as u8));
        }
        let color = if !enabled {
            t.text_faint
        } else if on {
            t.text
        } else {
            t.text_dim
        };
        ui.label(r, label, Font::Bold, 13.0, color, Align::Center);
        if resp.hovered {
            ui.set_cursor(Cursor::Hand);
            ui.tooltip(id, r, tip);
        }
        resp.clicked
    }

    /// The row between the display and the keypad: the mode's settings on the left, the memory
    /// keys on the right.
    fn tool_row(&mut self, ui: &mut Ui, r: Rect) {
        let now = ui.now();
        let mut x = r.x;
        match self.calc.mode() {
            Mode::Scientific => {
                let angle = self.calc.angle;
                if Self::small_button(
                    ui,
                    Rect::new(x, r.y, 56, r.h),
                    angle.label(),
                    "Angles in degrees, radians or gradians",
                    true,
                    false,
                ) {
                    self.calc.angle = angle.next();
                    self.changed(now);
                }
                x += 60;
                let sci = self.calc.style.scientific;
                if Self::small_button(ui, Rect::new(x, r.y, 46, r.h), "F-E", "Scientific notation always", true, sci) {
                    self.calc.style.scientific = !sci;
                    self.changed(now);
                }
            }
            Mode::Programmer => {
                let size = self.calc.word_size();
                if Self::small_button(
                    ui,
                    Rect::new(x, r.y, 70, r.h),
                    size.label(),
                    "The size of the numbers",
                    true,
                    false,
                ) {
                    self.calc.set_word_size(size.next());
                    self.changed(now);
                }
                x += 74;
                let bits = self.bits;
                if Self::small_button(
                    ui,
                    Rect::new(x, r.y, 50, r.h),
                    "Bits",
                    "Show the bits to switch them on and off",
                    true,
                    bits,
                ) {
                    self.bits = !bits;
                }
            }
            _ => {}
        }
        let has = self.calc.memory().is_some();
        let memory = [
            ("MC", Key::MemoryClear, "Clear the memory", has),
            ("MR", Key::MemoryRecall, "Recall the memory", has),
            ("M+", Key::MemoryAdd, "Add to the memory", true),
            ("M−", Key::MemorySubtract, "Subtract from the memory", true),
            ("MS", Key::MemoryStore, "Store in the memory", true),
        ];
        let w = ((r.w - (x - r.x) - 12) / 5).clamp(30, 46);
        let mut mx = r.right() - w * 5;
        for (label, key, tip, enabled) in memory {
            if Self::small_button(ui, Rect::new(mx, r.y, w, r.h), label, tip, enabled, false) {
                self.calc.press(key);
                if key == Key::MemoryStore {
                    self.show_toast("Saved to memory", now);
                }
                self.changed(now);
            }
            mx += w;
        }
    }

    /// Programmer mode's value in every base; clicking one shows numbers in it.
    fn bases(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme().clone();
        let value = self.calc.word_value();
        let row_h = r.h / 4;
        for (i, base) in Base::ALL.into_iter().enumerate() {
            let row = Rect::new(r.x, r.y + i as i32 * row_h, r.w, row_h);
            let id = ui.id(base.label()) ^ 0xba5e;
            let resp = ui.interact(id, row);
            let current = base == self.calc.base();
            if current {
                ui.canvas.fill_rounded_rect(Rect::new(row.x + 8, row.y + 5, 3, row.h - 10), 1.5, t.accent);
            } else if resp.hovered {
                ui.canvas.fill_rounded_rect(row.inset(4, 1, 4, 1), t.radius, Color::rgba(255, 255, 255, 12));
            }
            let label_color = if current { t.text } else { t.text_dim };
            ui.label(Rect::new(row.x + 20, row.y, 50, row.h), base.label(), Font::Bold, 13.0, label_color, Align::Left);
            let digits = value.map_or(String::from("—"), |v| format_word(v, base, self.calc.word_size(), true));
            let font = ui.ctx.font(Font::Mono);
            let shown = ui.ctx.text.ellipsize(font, 14.0, &digits, (row.w - 96) as f32);
            ui.label(
                Rect::new(row.x + 76, row.y, row.w - 92, row.h),
                &shown,
                Font::Mono,
                14.0,
                label_color,
                Align::Left,
            );
            if resp.hovered {
                ui.set_cursor(Cursor::Hand);
            }
            if resp.clicked {
                self.calc.set_base(base);
                self.changed(ui.now());
            }
        }
    }

    /// Programmer mode's bits, to switch on and off: four rows of sixteen, the highest first.
    fn bit_grid(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme().clone();
        let value = self.calc.word_value().unwrap_or(0) as u64;
        let bits = self.calc.word_size().bits();
        let row_h = (r.h / 4).min(64);
        let top = r.y + (r.h - row_h * 4) / 2;
        let group_gap = 10;
        let cell_w = (r.w - group_gap * 3) / 16;
        for row in 0..4 {
            for col in 0..16 {
                let bit = 63 - (row * 16 + col) as u32;
                let x = r.x + col * cell_w + (col / 4) * group_gap;
                let cell = Rect::new(x, top + row * row_h, cell_w, row_h - 18);
                let on = value >> bit & 1 == 1;
                let enabled = bit < bits;
                let id = ui.id("bit") ^ bit as u64;
                let resp = if enabled { ui.interact(id, cell) } else { vui::Response::default() };
                if resp.hovered {
                    ui.canvas.fill_rounded_rect(cell.inset(1, 0, 1, 0), t.radius, Color::rgba(255, 255, 255, 18));
                    ui.set_cursor(Cursor::Hand);
                }
                let color = match (enabled, on) {
                    (false, _) => t.text_faint.fade(0.5),
                    (true, true) => t.accent_hover,
                    (true, false) => t.text_dim,
                };
                ui.label(cell, if on { "1" } else { "0" }, Font::MonoBold, 18.0, color, Align::Center);
                if col % 4 == 3 {
                    let label = format!("{bit}");
                    let under = Rect::new(cell.x - cell_w * 3, cell.bottom(), cell_w * 4, 16);
                    ui.label(under, &label, Font::Regular, 11.0, t.text_faint, Align::Right);
                }
                if resp.clicked {
                    self.calc.toggle_bit(bit);
                    self.changed(ui.now());
                }
            }
        }
    }

    fn keypad(&mut self, ui: &mut Ui, r: Rect) {
        let now = ui.now();
        let options = Options { second: self.calc.second, hyperbolic: self.calc.hyperbolic, base: self.calc.base() };
        let layout = calculator_core::keypad::layout(self.calc.mode(), options);
        let rects = keypad::place(&layout, r, 4);
        let flash = self.flash.filter(|&(_, until)| now < until).map(|(a, _)| a);
        let mut pressed = None;
        for (pad, rect) in layout.pads.iter().zip(rects) {
            if keypad::key(ui, rect, pad, flash == Some(pad.action)) {
                pressed = Some(pad.action);
            }
        }
        if let Some(action) = pressed {
            self.act(action, now);
        }
        if let Some((_, until)) = self.flash {
            if now < until {
                ui.repaint_at(until);
            } else {
                self.flash = None;
            }
        }
    }

    /// The converter: its category, the value typed and its unit, the value converted and
    /// its unit, and the factor between the units.
    fn converter(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme().clone();
        let now = ui.now();
        let pad = 18;
        let inner = r.inset(pad, 4, pad, 4);
        let c = &self.calc.converter;
        let category = c.category();
        let (from, to) = (c.from_unit(), c.to_unit());

        // The category.
        let cat_r = Rect::new(inner.x - 10, inner.y, ui.measure(category.name, Font::Bold, 15.0) as i32 + 40, 32);
        if Self::dropdown(ui, cat_r, "category", category.name, 15.0) {
            ui.open_context_menu("categories", cat_r.x, cat_r.bottom() + 2);
        }
        let items: Vec<MenuItem> = CATEGORIES
            .iter()
            .enumerate()
            .map(|(i, c)| MenuItem::new(c.name).checked(i == self.calc.converter.category))
            .collect();
        if let Some(i) = ui.context_menu("categories", &items) {
            self.calc.converter.set_category(i);
            self.changed(now);
        }

        let block_h = (inner.h - 32 - 26) / 2;
        let screen = self.calc.screen();
        // From: the value typed (with its caret).
        let from_r = Rect::new(inner.x, inner.y + 36, inner.w, block_h);
        let input = if screen.main.is_empty() { String::from("0") } else { screen.main.clone() };
        self.value_block(ui, from_r, &input, screen.caret, from, true);
        // To: the value converted.
        let to_r = Rect::new(inner.x, from_r.bottom(), inner.w, block_h);
        let result = match (&screen.preview, &screen.error) {
            (Some(p), _) => p.clone(),
            (None, Some(e)) => e.clone(),
            (None, None) => String::from("—"),
        };
        self.value_block(ui, to_r, &result, None, to, false);
        // The factor: what one of the first unit is in the second.
        if let Ok(one) = convert(Number::ONE, from, to) {
            let style = Style { digits: 6, grouping: true, scientific: false, ascii: false };
            let text = format!("1 {} = {} {}", from.symbol, calculator_core::format::format(one, &style), to.symbol);
            let line = Rect::new(inner.x, to_r.bottom(), inner.w, 24);
            ui.label(line, &text, Font::Regular, 13.0, t.text_dim, Align::Left);
        }
    }

    /// A value and its unit's menu, in the converter. `from`: the value typed (whose unit is
    /// the one converted from).
    fn value_block(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        value: &str,
        caret: Option<usize>,
        unit: &calculator_core::units::Unit,
        from: bool,
    ) {
        let t = ui.theme().clone();
        let now = ui.now();
        let (label, menu_id) = if from { ("From", "from-units") } else { ("To", "to-units") };
        ui.label(Rect::new(r.x, r.y, 100, 20), label, Font::Regular, 12.5, t.text_faint, Align::Left);
        let unit_w = (ui.measure(unit.name, Font::Regular, 14.0) as i32 + 36).min(r.w / 2);
        let unit_r = Rect::new(r.right() - unit_w + 8, r.y + 20, unit_w, 32);
        if Self::dropdown(ui, unit_r, menu_id, unit.name, 14.0) {
            ui.open_context_menu(menu_id, unit_r.x, unit_r.bottom() + 2);
        }
        let c = &self.calc.converter;
        let current = if from { c.from } else { c.to };
        let items: Vec<MenuItem> = c
            .category()
            .units
            .iter()
            .enumerate()
            .map(|(i, u)| MenuItem::new(u.name).shortcut(u.symbol).checked(i == current))
            .collect();
        if let Some(i) = ui.context_menu(menu_id, &items) {
            if from {
                self.calc.converter.set_from(i);
            } else {
                self.calc.converter.set_to(i);
            }
            self.changed(now);
        }
        let value_r = Rect::new(r.x, r.y + 20, r.w - unit_w - 8, r.h - 20);
        // As large as fits, down to a size still read at a glance; past that, cut short.
        let mut size = (value_r.h as f32 * 0.62).clamp(18.0, 38.0);
        while size > 18.0 && ui.measure(value, Font::Regular, size) > value_r.w as f32 {
            size -= 2.0;
        }
        let font = ui.ctx.font(Font::Regular);
        let shown = ui.ctx.text.ellipsize(font, size, value, value_r.w as f32);
        let color = if from { t.text } else { t.accent_hover };
        ui.label(value_r, &shown, Font::Regular, size, color, Align::Left);
        if let Some(c) = caret.filter(|_| shown == value) {
            let cx = value_r.x + ui.measure(&value[..c.min(value.len())], Font::Regular, size) as i32;
            let h = (size * 1.05) as i32;
            ui.canvas.fill_rect(Rect::new(cx, value_r.y + (value_r.h - h) / 2, 2, h), t.accent);
        }
    }

    /// A button that opens a menu: its label and a chevron. Returns whether it was clicked.
    fn dropdown(ui: &mut Ui, r: Rect, id_label: &str, label: &str, size: f32) -> bool {
        let t = ui.theme().clone();
        let id = ui.id(id_label) ^ 0xd409;
        let resp = ui.interact(id, r);
        let hover = ui.animate(id, if resp.hovered { 1.0 } else { 0.0 }, 9.0);
        ui.canvas.fill_rounded_rect(r, t.radius, Color::rgba(255, 255, 255, 10 + (hover * 16.0) as u8));
        ui.label(Rect::new(r.x + 10, r.y, r.w - 34, r.h), label, Font::Bold, size, t.text, Align::Left);
        ui.icon(Rect::new(r.right() - 26, r.y, 18, r.h), Icon::ChevronDown, 13.0, t.text_dim);
        if resp.hovered {
            ui.set_cursor(Cursor::Hand);
        }
        resp.clicked
    }

    /// The history and memory panel.
    fn panel(&mut self, ui: &mut Ui, r: Rect, overlay: bool) {
        let t = ui.theme().clone();
        let now = ui.now();
        if overlay {
            ui.canvas.draw_shadow(r.translate(0, 4), 12, 18, Color::rgba(0, 0, 0, 140));
        }
        ui.canvas.fill_rounded_rect(r, t.radius_large, t.surface);
        ui.canvas.stroke_rounded_rect(r, t.radius_large, 1.0, t.border);
        let mut tab = if self.tab == Tab::History { 0 } else { 1 };
        ui.tabs(Rect::new(r.x + 8, r.y + 4, r.w - 16, 40), &["History", "Memory"], &mut tab);
        self.tab = if tab == 0 { Tab::History } else { Tab::Memory };
        let body = Rect::new(r.x + 4, r.y + 48, r.w - 8, r.h - 48 - 48);
        let footer = Rect::new(r.x + 8, body.bottom() + 6, r.w - 16, 36);
        match self.tab {
            Tab::History => {
                let items: Vec<(String, String, usize)> = self
                    .calc
                    .history()
                    .iter()
                    .enumerate()
                    .rev()
                    .map(|(i, h)| {
                        let result = match h.programmer {
                            Some((base, size)) => {
                                format_word(h.result.as_integer().unwrap_or(0) as i64, base, size, true)
                                    + " "
                                    + base.label()
                            }
                            None => calculator_core::format::format(h.result, &self.calc.style),
                        };
                        (format!("{} =", h.expression), result, i)
                    })
                    .collect();
                if items.is_empty() {
                    ui.paragraph(
                        body.inset(16, 16, 16, 0),
                        "There's no history yet. Calculations you work out show here.",
                        t.font_size,
                        t.text_faint,
                    );
                } else {
                    let row_h = 62;
                    let mut recall = None;
                    ui.scroll_area(body, "history-list", items.len() as i32 * row_h, |ui, offset| {
                        for (n, (expression, result, index)) in items.iter().enumerate() {
                            let row = Rect::new(body.x, body.y + n as i32 * row_h - offset, body.w - 10, row_h);
                            if row.bottom() < body.y || row.y > body.bottom() {
                                continue;
                            }
                            let resp = ui.interact(ui.id("history-row") ^ *index as u64, row);
                            if resp.hovered {
                                ui.canvas.fill_rounded_rect(
                                    row.inset(4, 2, 4, 2),
                                    t.radius,
                                    Color::rgba(255, 255, 255, 14),
                                );
                                ui.set_cursor(Cursor::Hand);
                            }
                            let font = ui.ctx.font(Font::Regular);
                            let e = ui.ctx.text.ellipsize(font, 13.0, expression, (row.w - 28) as f32);
                            ui.label(
                                Rect::new(row.x + 12, row.y + 8, row.w - 24, 20),
                                &e,
                                Font::Regular,
                                13.0,
                                t.text_dim,
                                Align::Right,
                            );
                            let v = ui.ctx.text.ellipsize(font, 20.0, result, (row.w - 28) as f32);
                            ui.label(
                                Rect::new(row.x + 12, row.y + 28, row.w - 24, 28),
                                &v,
                                Font::Regular,
                                20.0,
                                t.text,
                                Align::Right,
                            );
                            if resp.clicked {
                                recall = Some(*index);
                            }
                        }
                    });
                    if let Some(i) = recall {
                        self.calc.recall_history(i);
                        if overlay {
                            self.panel = false;
                        }
                    }
                    let clear = Rect::new(footer.right() - 36, footer.y, 36, footer.h);
                    if ui.icon_button(clear, Icon::Trash, "Clear the history") {
                        self.calc.clear_history();
                        self.changed(now);
                    }
                }
            }
            Tab::Memory => match self.calc.memory() {
                None => {
                    ui.paragraph(
                        body.inset(16, 16, 16, 0),
                        "There's nothing in memory. MS stores what the display shows.",
                        t.font_size,
                        t.text_faint,
                    );
                }
                Some(m) => {
                    let text = calculator_core::format::format(m, &self.calc.style);
                    let font = ui.ctx.font(Font::Regular);
                    let v = ui.ctx.text.ellipsize(font, 26.0, &text, (body.w - 32) as f32);
                    ui.label(
                        Rect::new(body.x + 16, body.y + 8, body.w - 32, 40),
                        &v,
                        Font::Regular,
                        26.0,
                        t.text,
                        Align::Right,
                    );
                    let mut x = body.right() - 16 - 3 * 48;
                    for (label, key, tip) in [
                        ("MC", Key::MemoryClear, "Clear the memory"),
                        ("M+", Key::MemoryAdd, "Add to the memory"),
                        ("M−", Key::MemorySubtract, "Subtract from the memory"),
                    ] {
                        if Self::small_button(ui, Rect::new(x, body.y + 52, 44, 30), label, tip, true, false) {
                            self.calc.press(key);
                            self.changed(now);
                        }
                        x += 48;
                    }
                }
            },
        }
    }

    fn draw_toast(&mut self, ui: &mut Ui, r: Rect) {
        let Some((text, until)) = self.toast.clone() else { return };
        if ui.now() >= until {
            self.toast = None;
            return;
        }
        display::toast(ui, r, &text);
        ui.repaint_at(until);
    }

    fn modal(&mut self, ui: &mut Ui) {
        let Some(modal) = self.modal else { return };
        let done = match modal {
            Modal::About => ui
                .message_box(
                    "Calculator",
                    "Veda Calculator: exact fractions, scientific functions, programmer's bases and bits, and unit conversions.",
                    &["OK"],
                )
                .is_some(),
            Modal::Shortcuts => {
                let t = ui.theme().clone();
                let mut done = false;
                let h = 120 + SHORTCUTS.len() as i32 * 24;
                ui.modal(540, h, |ui, card| {
                    done = ui.input.key(keys::ESC) || ui.input.key(keys::ENTER);
                    let inner = card.inset(24, 18, 24, 18);
                    ui.heading(Rect::new(inner.x, inner.y, inner.w, 26), "Keyboard Shortcuts");
                    for (i, (k, what)) in SHORTCUTS.iter().enumerate() {
                        let y = inner.y + 40 + i as i32 * 24;
                        ui.label(Rect::new(inner.x, y, 190, 24), k, Font::Mono, 12.5, t.accent_hover, Align::Left);
                        ui.label(Rect::new(inner.x + 200, y, inner.w - 200, 24), what, Font::Regular, 13.0, t.text, Align::Left);
                    }
                    if ui.primary_button(Rect::new(inner.right() - 96, inner.bottom() - 34, 96, 34), "Close") {
                        done = true;
                    }
                });
                done
            }
        };
        if done {
            self.modal = None;
            ui.repaint();
        }
    }
}

/// The key a typed character presses, to light it up on screen.
fn typed_key(ch: char, mode: Mode) -> Option<Key> {
    let programmer = mode == Mode::Programmer;
    Some(match ch {
        '0'..='9' => Key::Digit(ch as u8 - b'0'),
        'a'..='f' | 'A'..='F' if programmer => Key::Digit(ch.to_ascii_lowercase() as u8 - b'a' + 10),
        '.' => Key::Point,
        '+' => Key::Add,
        '-' => Key::Subtract,
        '*' => Key::Multiply,
        '/' => Key::Divide,
        '^' => Key::Power,
        '%' => Key::Percent,
        '!' => Key::Factorial,
        '(' => Key::Open,
        ')' => Key::Close,
        '=' => Key::Equals,
        _ => return None,
    })
}

impl App for CalcApp {
    fn update(&mut self, ui: &mut Ui) {
        let now = ui.now();
        let (w, h) = (ui.width, ui.height);
        if self.modal.is_none() {
            self.keyboard(ui);
        }
        let mode = self.calc.mode();

        // The panel beside the keypad, in a wide window.
        let wide = w >= PANEL_FROM_WIDTH && mode != Mode::Converter;
        let panel_w = if wide { (w * 36 / 100).clamp(240, 340) } else { 0 };
        let main = Rect::new(0, 0, w - panel_w, h);

        let header = Rect::new(main.x, main.y, main.w, 48);
        self.header(ui, header);

        let margin = 10;
        let body =
            Rect::new(main.x + margin, header.bottom(), main.w - margin * 2, main.bottom() - header.bottom() - margin);
        match mode {
            Mode::Converter => {
                let view_h = (body.h * 44 / 100).clamp(200, 300);
                self.converter(ui, Rect::new(body.x, body.y, body.w, view_h));
                let pad = Rect::new(body.x, body.y + view_h + 6, body.w, body.h - view_h - 6);
                self.keypad(ui, pad);
            }
            _ => {
                let programmer = mode == Mode::Programmer;
                let display_h = (body.h * if programmer { 20 } else { 27 } / 100).clamp(112, 190);
                self.display(ui, Rect::new(body.x, body.y, body.w, display_h));
                let mut y = body.y + display_h;
                if programmer {
                    let bases_h = 104;
                    self.bases(ui, Rect::new(body.x, y, body.w, bases_h));
                    y += bases_h + 4;
                }
                let tools = Rect::new(body.x, y, body.w, 32);
                self.tool_row(ui, tools);
                let pad = Rect::new(body.x, tools.bottom() + 6, body.w, body.bottom() - tools.bottom() - 6);
                if programmer && self.bits {
                    self.bit_grid(ui, pad);
                } else {
                    self.keypad(ui, pad);
                }
            }
        }

        if wide {
            self.panel(ui, Rect::new(main.right(), 8, panel_w - 8, h - 16), false);
        } else if self.panel && mode != Mode::Converter {
            let r = Rect::new(8, header.bottom(), w - 16, h - header.bottom() - 8);
            // Clicking outside the panel closes it.
            if ui.input.pressed[0] && ui.input.pointer.is_some_and(|(x, y)| !r.contains(x, y) && !header.contains(x, y))
            {
                self.panel = false;
            } else {
                ui.cover(r);
                self.panel(ui, r, true);
            }
        }

        self.draw_toast(ui, main);
        self.modal(ui);
        ui.set_title("Calculator");

        // What is remembered, written a little after it changes.
        if let Some(due) = self.store.due {
            if now >= due {
                self.store.save(&self.calc);
            } else {
                ui.repaint_at(due);
            }
        }
    }

    fn close_requested(&mut self) -> bool {
        self.store.save(&self.calc);
        true
    }

    fn agent_info(&self) -> Option<vui::agent::AppAgentInfo> {
        Some(agent::info())
    }

    fn agent_state(&self) -> vui::agent::Value {
        agent::state(&self.calc)
    }

    fn agent_invoke(&mut self, action: &str, args: &vui::agent::Value) -> Result<vui::agent::Value, String> {
        let result = agent::invoke(&mut self.calc, action, args);
        self.store.due = Some(vrt::time::now_ns() + SAVE_DELAY_NS);
        if action == "set_mode" || action == "convert" {
            self.panel = false;
        }
        result
    }
}

fn main() {
    let app = CalcApp::new();
    println!("{} mode", app.calc.mode().name());
    let mut spec = WindowSpec::new("Calculator", 420, 660);
    spec.min_width = 320;
    spec.min_height = 500;
    spec.app_id = "calculator".into();
    std::process::exit(vui::run(spec, app));
}

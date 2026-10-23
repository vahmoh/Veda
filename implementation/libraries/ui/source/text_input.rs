// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Single-line text input.

use alloc::string::String;

use gfx::{Color, Rect};
use proto::display::Cursor;
use proto::input::keys;
use unicode_segmentation::GraphemeCursor;

use crate::theme::Font;
use crate::ui::{TextEditState, Ui};

/// What happened to a text input this frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct TextInputResponse {
    pub changed: bool,
    /// Enter was pressed.
    pub submitted: bool,
    pub focused: bool,
}

/// The largest character boundary of `s` at or before `i`.
fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The caret position one character before `i`: a grapheme cluster back (a letter with its
/// accents, an emoji with its modifiers), which is also what Backspace deletes.
fn prev_boundary(s: &str, i: usize) -> usize {
    GraphemeCursor::new(i, s.len(), true).prev_boundary(s, 0).ok().flatten().unwrap_or(0)
}

/// The caret position one character (grapheme cluster) after `i`.
fn next_boundary(s: &str, i: usize) -> usize {
    GraphemeCursor::new(i, s.len(), true).next_boundary(s, 0).ok().flatten().unwrap_or(s.len())
}

/// Whether the character before `i` is white space.
fn space_before(s: &str, i: usize) -> bool {
    s[..i].chars().next_back().is_some_and(char::is_whitespace)
}

/// Whether the character at `i` is white space.
fn space_at(s: &str, i: usize) -> bool {
    s[i..].chars().next().is_some_and(char::is_whitespace)
}

fn word_left(s: &str, mut i: usize) -> usize {
    while i > 0 && space_before(s, i) {
        i = prev_boundary(s, i);
    }
    while i > 0 && !space_before(s, i) {
        i = prev_boundary(s, i);
    }
    i
}

fn word_right(s: &str, mut i: usize) -> usize {
    while i < s.len() && space_at(s, i) {
        i = next_boundary(s, i);
    }
    while i < s.len() && !space_at(s, i) {
        i = next_boundary(s, i);
    }
    i
}

impl<'a> Ui<'a> {
    /// Moves keyboard focus to the text input `id_label`.
    pub fn focus_text_input(&mut self, id_label: &str) {
        let id = self.id(id_label) ^ 0x7e47;
        self.focus(id);
    }

    /// Takes the keyboard from the text input `id_label`, if it has it.
    pub fn blur_text_input(&mut self, id_label: &str) {
        let id = self.id(id_label) ^ 0x7e47;
        if self.state.focus == Some(id) {
            self.state.focus = None;
        }
    }

    /// Sets the selection of the text input `id_label` as byte offsets into
    /// its value (`anchor == cursor` places the caret), e.g. to select a
    /// file name without its extension.
    pub fn set_text_input_selection(&mut self, id_label: &str, anchor: usize, cursor: usize) {
        let id = self.id(id_label) ^ 0x7e47;
        let st = self.state.edits.entry(id).or_default();
        st.anchor = anchor;
        st.cursor = cursor;
    }

    /// A text box editing `value`. Clicking focuses it; typing, arrows,
    /// Home/End, Backspace/Delete, Shift-selection, Ctrl+A/C/X/V work.
    pub fn text_input(&mut self, r: Rect, id_label: &str, value: &mut String, placeholder: &str) -> TextInputResponse {
        self.edit_line(r, id_label, value, placeholder, false, true)
    }

    /// A text box for a password: shows a bullet per character unless
    /// `reveal`, and never copies or cuts its contents to the clipboard.
    pub fn password_input(
        &mut self,
        r: Rect,
        id_label: &str,
        value: &mut String,
        placeholder: &str,
        reveal: bool,
    ) -> TextInputResponse {
        self.edit_line(r, id_label, value, placeholder, !reveal, true)
    }

    /// A text box for a secret typed at the keyboard alone, such as the
    /// lock screen's password: a bullet per character, nothing copied or
    /// cut to the clipboard, and nothing pasted from it either (what was
    /// copied before the screen was locked is not for whoever is at it).
    pub fn secret_input(
        &mut self,
        r: Rect,
        id_label: &str,
        value: &mut String,
        placeholder: &str,
    ) -> TextInputResponse {
        self.edit_line(r, id_label, value, placeholder, true, false)
    }

    fn edit_line(
        &mut self,
        r: Rect,
        id_label: &str,
        value: &mut String,
        placeholder: &str,
        mask: bool,
        paste: bool,
    ) -> TextInputResponse {
        // With `mask`, what is measured and drawn is one bullet per
        // character; offsets are converted between the two strings.
        const BULLET: char = '\u{2022}';
        let shown = |v: &str| -> String { if mask { v.chars().map(|_| BULLET).collect() } else { String::from(v) } };
        let to_shown =
            |v: &str, i: usize| -> usize { if mask { v[..i].chars().count() * BULLET.len_utf8() } else { i } };
        let from_shown = |v: &str, j: usize| -> usize {
            if mask { v.char_indices().nth(j / BULLET.len_utf8()).map_or(v.len(), |(i, _)| i) } else { j }
        };
        let id = self.id(id_label) ^ 0x7e47;
        let t = self.ctx.theme.clone();
        let size = t.font_size;
        let font = self.ctx.font(Font::Regular);
        let resp = self.interact(id, r);
        let mut st = self.state.edits.get(&id).copied().unwrap_or_default();
        // The value may have changed outside: keep both ends on characters.
        st.cursor = floor_boundary(value, st.cursor);
        st.anchor = floor_boundary(value, st.anchor);
        let pad = 10;
        let text_x = r.x + pad;
        if resp.pressed {
            self.focus(id);
            if let Some((px, _)) = self.input.pointer {
                let disp = shown(value);
                let idx =
                    from_shown(value, self.ctx.text.index_for_x(font, size, &disp, (px - text_x) as f32 + st.scroll));
                st.cursor = idx;
                if !self.input.shift() {
                    st.anchor = idx;
                }
                if resp.double_clicked {
                    // Word boundaries would reveal where the spaces are.
                    st.anchor = if mask { 0 } else { word_left(value, idx) };
                    st.cursor = if mask { value.len() } else { word_right(value, idx) };
                }
            }
            st.blink_start = self.now();
        } else if resp.held
            && let Some((px, _)) = self.input.pointer
        {
            let disp = shown(value);
            st.cursor =
                from_shown(value, self.ctx.text.index_for_x(font, size, &disp, (px - text_x) as f32 + st.scroll));
        }
        let focused = self.focused(id);
        let mut out = TextInputResponse { focused, ..Default::default() };
        if focused {
            let shift = self.input.shift();
            let ctrl = self.input.ctrl();
            let sel = |st: &TextEditState| (st.cursor.min(st.anchor), st.cursor.max(st.anchor));
            for k in self.input.keys.clone() {
                let (s0, s1) = sel(&st);
                let mut moved = None;
                match k.code {
                    keys::LEFT => {
                        moved = Some(if ctrl {
                            word_left(value, st.cursor)
                        } else if s0 != s1 && !shift {
                            s0
                        } else {
                            prev_boundary(value, st.cursor)
                        })
                    }
                    keys::RIGHT => {
                        moved = Some(if ctrl {
                            word_right(value, st.cursor)
                        } else if s0 != s1 && !shift {
                            s1
                        } else {
                            next_boundary(value, st.cursor)
                        })
                    }
                    keys::HOME => moved = Some(0),
                    keys::END => moved = Some(value.len()),
                    keys::BACKSPACE => {
                        if s0 != s1 {
                            value.replace_range(s0..s1, "");
                            st.cursor = s0;
                        } else if st.cursor > 0 {
                            let p = if ctrl { word_left(value, st.cursor) } else { prev_boundary(value, st.cursor) };
                            value.replace_range(p..st.cursor, "");
                            st.cursor = p;
                        }
                        st.anchor = st.cursor;
                        out.changed = true;
                    }
                    keys::DELETE => {
                        if s0 != s1 {
                            value.replace_range(s0..s1, "");
                            st.cursor = s0;
                        } else if st.cursor < value.len() {
                            let n = next_boundary(value, st.cursor);
                            value.replace_range(st.cursor..n, "");
                        }
                        st.anchor = st.cursor;
                        out.changed = true;
                    }
                    keys::ENTER | keys::KPENTER => out.submitted = true,
                    keys::A if ctrl => {
                        st.anchor = 0;
                        st.cursor = value.len();
                    }
                    keys::C | keys::X if ctrl && !mask => {
                        if s0 != s1 {
                            let text = String::from(&value[s0..s1]);
                            self.set_clipboard(&text);
                            if k.code == keys::X {
                                value.replace_range(s0..s1, "");
                                st.cursor = s0;
                                st.anchor = s0;
                                out.changed = true;
                            }
                        }
                    }
                    keys::V if ctrl && paste => {
                        let clip = self.clipboard();
                        let clip: String = clip.chars().filter(|c| !c.is_control()).collect();
                        value.replace_range(s0..s1, &clip);
                        st.cursor = s0 + clip.len();
                        st.anchor = st.cursor;
                        out.changed = true;
                    }
                    _ => {}
                }
                if let Some(m) = moved {
                    st.cursor = m;
                    if !shift {
                        st.anchor = m;
                    }
                }
                st.blink_start = self.now();
            }
            if !self.input.text.is_empty() {
                let (s0, s1) = sel(&st);
                let typed: String = self.input.text.chars().filter(|c| *c != '\n' && *c != '\t').collect();
                if !typed.is_empty() {
                    value.replace_range(s0..s1, &typed);
                    st.cursor = s0 + typed.len();
                    st.anchor = st.cursor;
                    out.changed = true;
                    st.blink_start = self.now();
                }
            }
            if self.input.key(keys::ESC) {
                self.state.focus = None;
            }
        }

        // Keep the cursor visible, with no more of the box empty past the
        // text's end than the cursor needs (the text may have become
        // shorter).
        let inner_w = (r.w - 2 * pad) as f32;
        let disp = shown(value);
        let cx = self.ctx.text.x_for_index(font, size, &disp, to_shown(value, st.cursor));
        if cx - st.scroll > inner_w {
            st.scroll = cx - inner_w;
        } else if cx < st.scroll {
            st.scroll = cx;
        }
        let end = self.ctx.text.x_for_index(font, size, &disp, disp.len());
        st.scroll = st.scroll.min(end - inner_w).max(0.0);

        // Draw.
        let bg = if focused {
            t.bg
        } else if resp.hovered {
            t.control_hover
        } else {
            t.control
        };
        self.canvas.fill_rounded_rect(r, t.radius, bg);
        let border = if focused { t.accent } else { t.border };
        self.canvas.stroke_rounded_rect(r, t.radius, if focused { 1.5 } else { 1.0 }, border);
        let m = self.ctx.text.metrics(font, size);
        let baseline = r.y as f32 + (r.h as f32 - (m.ascent + m.descent)) / 2.0 + m.ascent;
        self.canvas.save();
        self.canvas.clip_to(r.inset(pad - 2, 2, pad - 2, 2));
        let (s0, s1) = (st.cursor.min(st.anchor), st.cursor.max(st.anchor));
        if focused && s0 != s1 {
            let x0 = self.ctx.text.x_for_index(font, size, &disp, to_shown(value, s0)) - st.scroll;
            let x1 = self.ctx.text.x_for_index(font, size, &disp, to_shown(value, s1)) - st.scroll;
            let sel_rect = Rect::new(text_x + x0 as i32, r.y + 5, (x1 - x0) as i32, r.h - 10);
            self.canvas.fill_rect(sel_rect, t.selection);
        }
        if value.is_empty() && !placeholder.is_empty() {
            self.ctx.text.draw(
                &mut self.canvas,
                font,
                size,
                text_x as f32,
                baseline.floor_px(),
                placeholder,
                t.text_faint,
            );
        } else {
            self.ctx.text.draw(
                &mut self.canvas,
                font,
                size,
                text_x as f32 - st.scroll,
                baseline.floor_px(),
                &disp,
                t.text,
            );
        }
        if focused {
            let phase = ((self.now() - st.blink_start) / 530_000_000) % 2;
            if phase == 0 {
                let x = text_x + (cx - st.scroll) as i32;
                self.canvas.fill_rect(Rect::new(x, r.y + 7, 2, r.h - 14), Color::hex(0xFFFFFF));
            }
            let next = st.blink_start + (((self.now() - st.blink_start) / 530_000_000) + 1) * 530_000_000;
            self.repaint_at(next);
        }
        self.canvas.restore();
        if resp.hovered {
            self.set_cursor(Cursor::Text);
        }
        self.state.edits.insert(id, st);
        out
    }
}

trait FloorPx {
    fn floor_px(self) -> f32;
}

impl FloorPx for f32 {
    fn floor_px(self) -> f32 {
        libm::floorf(self + 0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::SharedText;
    use crate::ui::tests::{context, frame};
    use crate::ui::{Input, UiState};

    const INTER: &[u8] = include_bytes!("../../../ports/fonts/Inter-Regular.otf");

    #[test]
    fn carets_move_over_whole_characters_and_words() {
        // e + combining acute: one character to the caret and to Backspace.
        let s = "ce\u{301}\u{a0}d";
        assert_eq!(next_boundary(s, 1), 4);
        assert_eq!(prev_boundary(s, 4), 1);
        assert_eq!(next_boundary(s, s.len()), s.len());
        assert_eq!(prev_boundary(s, 0), 0);
        // A no-break space separates words like any white space.
        assert_eq!(word_right(s, 0), 4);
        assert_eq!(word_left(s, s.len()), s.len() - 1);
    }

    #[test]
    fn text_made_shorter_from_outside_shows_from_its_start() {
        let (mut ctx, mut state, mut input) = (context(), UiState::default(), Input::default());
        ctx.text = SharedText::new(gfx::Text::new());
        ctx.text.add_font(INTER);
        let field = Rect::new(10, 10, 200, 30);
        // Typed past the box's end, the caret there: its text scrolls.
        let mut value = String::from("https://example.com/a/path/longer/than/the/box/is/wide");
        let id = frame(&mut ctx, &mut state, &mut input, |ui| {
            ui.set_text_input_selection("address", value.len(), value.len());
            ui.text_input(field, "address", &mut value, "");
            ui.id("address") ^ 0x7e47
        });
        assert!(state.edits[&id].scroll > 0.0);
        // Text that fits, put there by the program, caret and all: none of
        // it is scrolled out of sight.
        let mut value = String::from("example.com/b");
        frame(&mut ctx, &mut state, &mut input, |ui| ui.text_input(field, "address", &mut value, ""));
        assert_eq!(state.edits[&id].scroll, 0.0);
    }

    #[test]
    fn a_text_input_lets_go_of_the_keyboard_it_has() {
        let (mut ctx, mut state, mut input) = (context(), UiState::default(), Input::default());
        let (first, second) = (Rect::new(10, 10, 200, 30), Rect::new(10, 50, 200, 30));
        let (mut a, mut b) = (String::new(), String::new());
        // Another's keyboard stays where it is.
        let focused = frame(&mut ctx, &mut state, &mut input, |ui| {
            ui.focus_text_input("b");
            ui.blur_text_input("a");
            ui.text_input(first, "a", &mut a, "");
            ui.text_input(second, "b", &mut b, "").focused
        });
        assert!(focused);
        let focused = frame(&mut ctx, &mut state, &mut input, |ui| {
            ui.blur_text_input("b");
            ui.text_input(second, "b", &mut b, "").focused
        });
        assert!(!focused);
    }
}

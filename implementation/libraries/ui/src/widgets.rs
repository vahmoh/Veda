// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Standard widgets.

use alloc::string::String;

use vgfx::{Align, Canvas, Color, Rect};
use vproto::display::Cursor;
use vproto::input::keys;

use crate::icons::Icon;
use crate::theme::Font;
use crate::ui::{Id, Overlay, Ui};

/// Visual style of a button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// Accent-filled call to action.
    Primary,
    /// Neutral filled button.
    Secondary,
    /// No background until hovered (toolbars).
    Ghost,
    /// Red, for destructive actions.
    Danger,
}

/// Per-row information passed to list row painters.
#[derive(Debug, Clone, Copy)]
pub struct RowState {
    pub selected: bool,
    pub hovered: bool,
    pub focused: bool,
}

/// What happened in a list view this frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListResponse {
    /// Row clicked (or selected with the keyboard).
    pub clicked: Option<usize>,
    /// Row double-clicked or activated with Enter.
    pub activated: Option<usize>,
    /// Row right-clicked, with the pointer position.
    pub context: Option<(usize, i32, i32)>,
}

impl<'a> Ui<'a> {
    fn button_colors(&self, kind: ButtonKind, hover: f32, held: bool) -> (Color, Color) {
        let t = &self.ctx.theme;
        match kind {
            ButtonKind::Primary => {
                let base = if held { t.accent_pressed } else { t.accent.lerp(t.accent_hover, hover) };
                (base, t.on_accent)
            }
            ButtonKind::Secondary => {
                let base = if held { t.control_pressed } else { t.control.lerp(t.control_hover, hover) };
                (base, t.text)
            }
            ButtonKind::Ghost => {
                let a = if held { 18 } else { (hover * 26.0) as u8 };
                (Color::rgba(255, 255, 255, a), t.text)
            }
            ButtonKind::Danger => {
                let base = if held { t.danger.shade(-0.15) } else { t.danger.lerp(t.danger.shade(0.12), hover) };
                (base, Color::WHITE)
            }
        }
    }

    /// A button with an optional icon and label. Returns `true` when clicked.
    pub fn button_full(&mut self, r: Rect, icon: Option<Icon>, label: &str, kind: ButtonKind) -> bool {
        let id = self.id(label) ^ (r.x as u64) << 32 ^ r.y as u64;
        let resp = self.interact(id, r);
        let hover = self.animate(id, if resp.hovered { 1.0 } else { 0.0 }, 8.0);
        let (bg, fg) = self.button_colors(kind, hover, resp.held);
        let radius = self.ctx.theme.radius;
        if bg.a() > 0 {
            if kind == ButtonKind::Primary && !resp.held {
                let top = bg.shade(0.06);
                self.canvas.fill_rounded_rect_gradient(r, radius, top, bg.shade(-0.04));
            } else {
                self.canvas.fill_rounded_rect(r, radius, bg);
            }
        }
        if kind == ButtonKind::Secondary {
            let border = self.ctx.theme.border;
            self.canvas.stroke_rounded_rect(r, radius, 1.0, border);
        }
        if resp.hovered {
            self.set_cursor(Cursor::Hand);
        }
        let size = self.ctx.theme.font_size;
        match (icon, label.is_empty()) {
            (Some(icon), true) => icon.draw(&mut self.canvas, r, 18.0, fg),
            (Some(icon), false) => {
                let tw = self.measure(label, Font::Bold, size) as i32;
                let total = 18 + 8 + tw;
                let x = r.x + (r.w - total) / 2;
                icon.draw(&mut self.canvas, Rect::new(x, r.y, 18, r.h), 18.0, fg);
                self.label(Rect::new(x + 26, r.y, tw + 4, r.h), label, Font::Bold, size, fg, Align::Left);
            }
            (None, _) => self.label(r, label, Font::Bold, size, fg, Align::Center),
        }
        resp.clicked
    }

    /// A neutral button.
    pub fn button(&mut self, r: Rect, label: &str) -> bool {
        self.button_full(r, None, label, ButtonKind::Secondary)
    }

    /// An accent-coloured button.
    pub fn primary_button(&mut self, r: Rect, label: &str) -> bool {
        self.button_full(r, None, label, ButtonKind::Primary)
    }

    /// A borderless icon button with a tooltip.
    pub fn icon_button(&mut self, r: Rect, icon: Icon, tooltip: &str) -> bool {
        let size = (r.w.min(r.h) as f32 * 0.55).clamp(14.0, 22.0);
        self.glyph_button(r, tooltip, |c, r, color| icon.draw(c, r, size, color))
    }

    /// A borderless button with a tooltip, showing what `draw` draws in
    /// the button's rectangle and colour (a [`status`](crate::status)
    /// glyph, say).
    pub fn glyph_button(&mut self, r: Rect, tooltip: &str, draw: impl FnOnce(&mut Canvas, Rect, Color)) -> bool {
        let id = self.id(tooltip) ^ (r.x as u64) << 32 ^ r.y as u64 ^ 0x1c0;
        let resp = self.interact(id, r);
        let hover = self.animate(id, if resp.hovered { 1.0 } else { 0.0 }, 8.0);
        let (bg, fg) = self.button_colors(ButtonKind::Ghost, hover, resp.held);
        if bg.a() > 0 {
            let radius = self.ctx.theme.radius;
            self.canvas.fill_rounded_rect(r, radius, bg);
        }
        draw(&mut self.canvas, r, fg);
        if resp.hovered {
            self.set_cursor(Cursor::Hand);
            self.tooltip(id, r, tooltip);
        }
        resp.clicked
    }

    /// Shows `text` below `anchor` after the pointer rests on it briefly
    /// (call it while the pointer is over `anchor`: a frame without the
    /// call ends the rest).
    pub fn tooltip(&mut self, id: Id, anchor: Rect, text: &str) {
        if text.is_empty() {
            return;
        }
        let now = self.now();
        self.tooltip_asked = true;
        let since = match self.state.tooltip {
            Some((tid, t)) if tid == id => t,
            _ => {
                self.state.tooltip = Some((id, now));
                now
            }
        };
        if now.saturating_sub(since) < 600_000_000 {
            self.repaint_at(since + 600_000_000);
            return;
        }
        let size = self.ctx.theme.small_size;
        let w = self.measure(text, Font::Regular, size) as i32 + 18;
        let h = 26;
        let mut x = anchor.x + (anchor.w - w) / 2;
        x = x.clamp(4, (self.width - w - 4).max(4));
        let mut y = anchor.bottom() + 6;
        if y + h > self.height - 4 {
            y = anchor.y - h - 6;
        }
        self.overlays.push(Overlay::Tooltip { rect: Rect::new(x, y, w, h), text: String::from(text) });
    }

    /// A checkbox with a label. Returns `true` if toggled.
    pub fn checkbox(&mut self, r: Rect, label: &str, value: &mut bool) -> bool {
        let id = self.id(label) ^ 0xc4ec;
        let resp = self.interact(id, r);
        let t = self.ctx.theme.clone();
        let box_r = Rect::new(r.x, r.y + (r.h - 18) / 2, 18, 18);
        if *value {
            self.canvas.fill_rounded_rect(box_r, 4.0, t.accent);
            Icon::Check.draw(&mut self.canvas, box_r, 14.0, t.on_accent);
        } else {
            self.canvas.fill_rounded_rect(box_r, 4.0, if resp.hovered { t.control_hover } else { t.control });
            self.canvas.stroke_rounded_rect(box_r, 4.0, 1.0, t.border_strong);
        }
        self.label(Rect::new(r.x + 28, r.y, r.w - 28, r.h), label, Font::Regular, t.font_size, t.text, Align::Left);
        if resp.clicked {
            *value = !*value;
        }
        resp.clicked
    }

    /// An on/off switch. Returns `true` if toggled.
    pub fn toggle(&mut self, r: Rect, id_label: &str, value: &mut bool) -> bool {
        let id = self.id(id_label) ^ 0x7067;
        let track = Rect::new(r.x, r.y + (r.h - 22) / 2, 42, 22);
        let resp = self.interact(id, track);
        if resp.clicked {
            *value = !*value;
        }
        let k = self.animate(id, if *value { 1.0 } else { 0.0 }, 7.0);
        let t = self.ctx.theme.clone();
        let bg = t.control_hover.lerp(t.accent, k);
        self.canvas.fill_rounded_rect(track, 11.0, bg);
        let knob_x = track.x as f32 + 11.0 + k * 20.0;
        self.canvas.fill_circle(knob_x, track.y as f32 + 11.0, 8.0, Color::WHITE);
        if resp.hovered {
            self.set_cursor(Cursor::Hand);
        }
        resp.clicked
    }

    /// A horizontal slider. Returns `true` while the value changes.
    pub fn slider(&mut self, r: Rect, id_label: &str, value: &mut f32, min: f32, max: f32) -> bool {
        let id = self.id(id_label) ^ 0x511d;
        let resp = self.interact(id, r);
        let t = self.ctx.theme.clone();
        let mut changed = false;
        let span = (max - min).max(f32::EPSILON);
        if (resp.held || resp.pressed)
            && let Some((px, _)) = self.input.pointer
        {
            let f = ((px - r.x) as f32 / r.w.max(1) as f32).clamp(0.0, 1.0);
            let nv = min + f * span;
            if (nv - *value).abs() > f32::EPSILON {
                *value = nv;
                changed = true;
            }
        }
        if resp.hovered && self.input.scroll.1 != 0 {
            *value = (*value + self.input.scroll.1 as f32 * span * 0.05).clamp(min, max);
            changed = true;
        }
        let f = ((*value - min) / span).clamp(0.0, 1.0);
        let cy = r.y + r.h / 2;
        let track = Rect::new(r.x, cy - 2, r.w, 4);
        self.canvas.fill_rounded_rect(track, 2.0, t.control_hover);
        let filled = Rect::new(r.x, cy - 2, (r.w as f32 * f) as i32, 4);
        self.canvas.fill_rounded_rect(filled, 2.0, t.accent);
        let kx = r.x as f32 + r.w as f32 * f;
        let grow = self.animate(id, if resp.hovered || resp.held { 1.0 } else { 0.0 }, 8.0);
        self.canvas.fill_circle(kx, cy as f32, 7.0 + grow * 1.5, Color::WHITE);
        self.canvas.fill_circle(kx, cy as f32, 3.5, t.accent);
        if resp.hovered {
            self.set_cursor(Cursor::Hand);
        }
        changed
    }

    /// A progress bar (`fraction` in 0..=1).
    pub fn progress(&mut self, r: Rect, fraction: f32) {
        let t = self.ctx.theme.clone();
        let radius = r.h as f32 / 2.0;
        self.canvas.fill_rounded_rect(r, radius, t.control_hover);
        let w = (r.w as f32 * fraction.clamp(0.0, 1.0)) as i32;
        if w > 0 {
            self.canvas.save();
            self.canvas.clip_to(Rect::new(r.x, r.y, w, r.h));
            self.canvas.fill_rounded_rect(r, radius, t.accent);
            self.canvas.restore();
        }
    }

    pub fn separator(&mut self, x: i32, y: i32, w: i32) {
        let c = self.ctx.theme.border;
        self.canvas.fill_rect(Rect::new(x, y, w, 1), c);
    }

    /// A raised card background.
    pub fn card(&mut self, r: Rect) {
        let t = self.ctx.theme.clone();
        self.canvas.fill_rounded_rect(r, t.radius_large, t.surface);
        self.canvas.stroke_rounded_rect(r, t.radius_large, 1.0, t.border);
    }

    /// A row of tabs; returns `true` if the selection changed.
    pub fn tabs(&mut self, r: Rect, labels: &[&str], selected: &mut usize) -> bool {
        let t = self.ctx.theme.clone();
        let mut changed = false;
        let mut x = r.x;
        for (i, label) in labels.iter().enumerate() {
            let w = self.measure(label, Font::Bold, t.font_size) as i32 + 28;
            let tr = Rect::new(x, r.y, w, r.h);
            let id = self.id(label) ^ 0x7ab5 ^ i as u64;
            let resp = self.interact(id, tr);
            if resp.clicked && *selected != i {
                *selected = i;
                changed = true;
            }
            let active = *selected == i;
            if resp.hovered && !active {
                self.canvas.fill_rounded_rect(tr.inset(2, 4, 2, 4), t.radius, Color::rgba(255, 255, 255, 14));
            }
            let color = if active { t.text } else { t.text_dim };
            self.label(tr, label, Font::Bold, t.font_size, color, Align::Center);
            if active {
                self.canvas.fill_rounded_rect(Rect::new(tr.x + 12, tr.bottom() - 3, tr.w - 24, 3), 1.5, t.accent);
            }
            x += w + 4;
        }
        changed
    }

    /// A vertically scrolling region. `f` draws the content given the
    /// scroll offset (content y = viewport y - offset). Returns `f`'s value.
    pub fn scroll_area<R>(
        &mut self,
        r: Rect,
        id_label: &str,
        content_h: i32,
        f: impl FnOnce(&mut Ui<'a>, i32) -> R,
    ) -> R {
        let id = self.id(id_label) ^ 0x5c01;
        let max = (content_h - r.h).max(0) as f32;
        let mut offset = self.state.floats.get(&id).copied().unwrap_or(0.0).clamp(0.0, max);
        if self.hovered(r) && self.input.scroll.1 != 0 {
            offset = (offset - self.input.scroll.1 as f32 * 48.0).clamp(0.0, max);
        }
        // Scrollbar dragging.
        let bar_id = id ^ 0xba5;
        let track = Rect::new(r.right() - 10, r.y + 2, 8, r.h - 4);
        if max > 0.0 {
            let thumb_h = ((r.h as f32 / content_h as f32) * track.h as f32).max(24.0) as i32;
            let thumb_y = track.y + ((offset / max) * (track.h - thumb_h) as f32) as i32;
            let thumb = Rect::new(track.x, thumb_y, track.w, thumb_h);
            let resp = self.interact(bar_id, track);
            if resp.held
                && let Some((_, py)) = self.input.pointer
            {
                let f = ((py - track.y - thumb_h / 2) as f32 / (track.h - thumb_h).max(1) as f32).clamp(0.0, 1.0);
                offset = f * max;
            }
            self.state.floats.insert(id, offset);
            let content_r = r;
            self.canvas.save();
            self.canvas.clip_to(content_r);
            let was_blocked = self.blocked;
            if !self.hovered(content_r) {
                self.blocked = true;
            }
            let out = f(self, offset as i32);
            self.blocked = was_blocked;
            self.canvas.restore();
            let hover = self.animate(bar_id, if resp.hovered || resp.held || self.hovered(r) { 1.0 } else { 0.0 }, 6.0);
            let alpha = (60.0 + hover * 80.0) as u8;
            self.canvas.fill_rounded_rect(thumb.inset(2, 0, 1, 0), 3.0, Color::rgba(255, 255, 255, alpha));
            out
        } else {
            self.state.floats.insert(id, 0.0);
            self.canvas.save();
            self.canvas.clip_to(r);
            let was_blocked = self.blocked;
            if !self.hovered(r) {
                self.blocked = true;
            }
            let out = f(self, 0);
            self.blocked = was_blocked;
            self.canvas.restore();
            out
        }
    }

    /// Scrolls a scroll area so that `[y, y+h)` (content coordinates) is visible.
    pub fn scroll_into_view(&mut self, id_label: &str, viewport_h: i32, y: i32, h: i32) {
        let id = self.id(id_label) ^ 0x5c01;
        let off = self.state.floats.get(&id).copied().unwrap_or(0.0) as i32;
        let new = if y < off {
            y
        } else if y + h > off + viewport_h {
            y + h - viewport_h
        } else {
            off
        };
        self.state.floats.insert(id, new.max(0) as f32);
    }

    /// A virtualised list with selection, keyboard navigation and
    /// activation. `paint` draws one row.
    pub fn list(
        &mut self,
        r: Rect,
        id_label: &str,
        count: usize,
        row_h: i32,
        selected: &mut Option<usize>,
        mut paint: impl FnMut(&mut Ui<'a>, usize, Rect, RowState),
    ) -> ListResponse {
        let id = self.id(id_label) ^ 0x1157;
        let mut out = ListResponse::default();
        let focused = self.focused(id);
        // Keyboard navigation.
        if focused && count > 0 {
            let cur = selected.unwrap_or(0);
            let page = (r.h / row_h.max(1)).max(1) as usize;
            let mut next = None;
            for k in &self.input.keys {
                next = match k.code {
                    keys::DOWN => Some((selected.map_or(0, |s| s + 1)).min(count - 1)),
                    keys::UP => Some(cur.saturating_sub(1)),
                    keys::PAGEDOWN => Some((cur + page).min(count - 1)),
                    keys::PAGEUP => Some(cur.saturating_sub(page)),
                    keys::HOME => Some(0),
                    keys::END => Some(count - 1),
                    keys::ENTER | keys::KPENTER => {
                        if let Some(s) = *selected {
                            out.activated = Some(s);
                        }
                        next
                    }
                    _ => next,
                };
            }
            if let Some(n) = next {
                *selected = Some(n);
                out.clicked = Some(n);
                self.scroll_into_view(id_label, r.h, n as i32 * row_h, row_h);
            }
        }
        let content_h = count as i32 * row_h;
        let pressed_in = self.hovered(r) && (self.input.pressed[0] || self.input.pressed[1]);
        if pressed_in {
            self.focus(id);
        }
        self.scroll_area(r, id_label, content_h, |ui, offset| {
            let first = (offset / row_h.max(1)) as usize;
            let visible = (r.h / row_h.max(1) + 2) as usize;
            for i in first..(first + visible).min(count) {
                let rr = Rect::new(r.x, r.y + i as i32 * row_h - offset, r.w, row_h);
                let row_id = id ^ ((i as u64 + 1) << 20);
                let resp = ui.interact(row_id, rr);
                if resp.pressed {
                    *selected = Some(i);
                    out.clicked = Some(i);
                    if resp.double_clicked {
                        out.activated = Some(i);
                    }
                }
                if resp.right_clicked {
                    *selected = Some(i);
                    if let Some((px, py)) = ui.input.pointer {
                        out.context = Some((i, px, py));
                    }
                }
                let state = RowState { selected: *selected == Some(i), hovered: resp.hovered, focused };
                paint(ui, i, rr, state);
            }
        });
        out
    }

    /// Default row background for list painters.
    pub fn row_background(&mut self, r: Rect, s: RowState) {
        let t = self.ctx.theme.clone();
        let inner = r.inset(4, 1, 4, 1);
        if s.selected {
            self.canvas.fill_rounded_rect(
                inner,
                t.radius,
                if s.focused { t.selection } else { Color::rgba(255, 255, 255, 22) },
            );
        } else if s.hovered {
            self.canvas.fill_rounded_rect(inner, t.radius, Color::rgba(255, 255, 255, 12));
        }
    }

    /// Shows a modal dialog of the given size centred in the window. `f`
    /// draws its content inside the card rectangle and returns a value
    /// that is passed through. Everything outside is dimmed and inert: it
    /// sees neither the pointer nor the keyboard while the dialog is up, and
    /// what it put over itself (a tooltip, a menu) is not drawn.
    ///
    /// The dialog has the keyboard from the frame after it first appears
    /// (keys that came before it went to what was there): its content sees
    /// the keys and typed text in [`Ui::input`], and only its widgets can
    /// have the focus. Keys meant for the dialog (Esc, say) are read in `f`.
    pub fn modal<R>(&mut self, w: i32, h: i32, f: impl FnOnce(&mut Ui<'a>, Rect) -> R) -> R {
        if !self.state.modal {
            // The window's menus close as a dialog appears over it.
            self.state.open_menu = None;
            self.state.context_menu = None;
            self.state.menu_key = None;
        }
        self.overlays.clear();
        self.new_overlay_rects.clear();
        self.canvas.fill_rect(self.rect(), Color::rgba(0, 0, 0, 120));
        let card = self.rect().centered(w.min(self.width - 24), h.min(self.height - 24));
        self.canvas.draw_shadow(card.translate(0, 8), 12, 24, Color::rgba(0, 0, 0, 140));
        let t = self.ctx.theme.clone();
        self.canvas.fill_rounded_rect(card, t.radius_large, t.surface);
        self.canvas.stroke_rounded_rect(card, t.radius_large, 1.0, t.border_strong);
        self.modal_shown = true;
        let (was_blocked, was_input, was_in_modal) = (self.blocked, self.input, self.in_modal);
        self.blocked = self.input.pointer.is_some_and(|(x, y)| self.state.overlays.iter().any(|o| o.contains(x, y)));
        // Keys that came before the dialog appeared went to what was there,
        // and a menu open in it keeps the keyboard.
        self.input = if self.state.modal && !self.menu_open() { self.menu_input } else { self.quiet };
        self.in_modal = true;
        let out = f(self, card);
        self.in_modal = was_in_modal;
        self.input = was_input;
        self.blocked = was_blocked;
        out
    }

    /// A standard message box with buttons; returns the index of the button
    /// pressed this frame (Enter picks the first button, Esc the last).
    pub fn message_box(&mut self, title: &str, message: &str, buttons: &[&str]) -> Option<usize> {
        self.message_box_with(title, message, buttons, ButtonKind::Primary)
    }

    /// A message box whose first button destroys something for good
    /// (deleting files): that button is red. Returns as
    /// [`Ui::message_box`].
    pub fn destructive_box(&mut self, title: &str, message: &str, buttons: &[&str]) -> Option<usize> {
        self.message_box_with(title, message, buttons, ButtonKind::Danger)
    }

    fn message_box_with(&mut self, title: &str, message: &str, buttons: &[&str], first: ButtonKind) -> Option<usize> {
        let picked = self.modal(440, 190, |ui, card| {
            let inner = card.inset(24, 20, 24, 20);
            ui.heading(Rect::new(inner.x, inner.y, inner.w, 26), title);
            let (size, color) = (ui.ctx.theme.font_size, ui.ctx.theme.text_dim);
            ui.paragraph(Rect::new(inner.x, inner.y + 38, inner.w, 60), message, size, color);
            let mut picked = None;
            let mut x = inner.right();
            for (i, label) in buttons.iter().enumerate().rev() {
                let w = (ui.measure(label, Font::Bold, size) as i32 + 32).max(92);
                x -= w;
                let kind = if i == 0 { first } else { ButtonKind::Secondary };
                if ui.button_full(Rect::new(x, inner.bottom() - 36, w, 36), None, label, kind) {
                    picked = Some(i);
                }
                x -= 10;
            }
            if ui.input.key(keys::ENTER) {
                picked = Some(0);
            }
            if ui.input.key(keys::ESC) {
                picked = picked.or(Some(buttons.len().saturating_sub(1)));
            }
            picked
        });
        if picked.is_some() {
            // The box goes: the next frame is drawn without it.
            self.repaint();
        }
        picked
    }

    /// Draws an icon.
    pub fn icon(&mut self, r: Rect, icon: Icon, size: f32, color: Color) {
        icon.draw(&mut self.canvas, r, size, color);
    }

    /// A small rotating activity indicator.
    pub fn spinner(&mut self, cx: i32, cy: i32, radius: f32) {
        let t = (self.now() / 16_000_000) as f32 * 0.12;
        let accent = self.ctx.theme.accent;
        for i in 0..8 {
            let a = t + i as f32 * core::f32::consts::TAU / 8.0;
            let (s, c) = (libm::sinf(a), libm::cosf(a));
            let alpha = (255.0 * (i as f32 + 1.0) / 8.0) as u8;
            self.canvas.fill_circle(
                cx as f32 + c * radius,
                cy as f32 + s * radius,
                radius * 0.22,
                accent.with_alpha(alpha),
            );
        }
        let now = self.now();
        self.repaint_at(now + 50_000_000);
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use vproto::input::keys;

    use super::*;
    use crate::ui::tests::{context, frame, press};
    use crate::ui::{ANIMATION_FRAMES, Input, UiState};
    use crate::{Menu, MenuItem};

    const FIELD: Rect = Rect::new(10, 10, 200, 30);

    /// A focused text field, and a message box over it if `show_box`: what
    /// each answered.
    fn field_and_box(ui: &mut Ui, text: &mut String, show_box: bool) -> (crate::TextInputResponse, Option<usize>) {
        let field = ui.text_input(FIELD, "note", text, "");
        let answer = show_box.then(|| ui.message_box("Save changes?", "Unsaved.", &["Save", "Don't Save", "Cancel"]));
        (field, answer.flatten())
    }

    #[test]
    fn a_modal_dialog_has_the_keyboard() {
        let (mut ctx, mut state, mut input) = (context(), UiState::default(), Input::default());
        let mut text = String::from("draft");
        frame(&mut ctx, &mut state, &mut input, |ui| {
            ui.focus_text_input("note");
            ui.set_text_input_selection("note", 5, 5);
            field_and_box(ui, &mut text, false)
        });
        let mut step = |input: &mut Input, show_box: bool| {
            frame(&mut ctx, &mut state, input, |ui| field_and_box(ui, &mut text, show_box))
        };
        // Keys that came as the box appeared were the field's.
        press(&mut input, &[keys::ENTER], "a");
        let (field, answer) = step(&mut input, true);
        assert_eq!(answer, None);
        assert!(field.focused && field.changed && field.submitted);
        // Now the box has the keyboard: Enter is its, and the field, which
        // keeps its focus behind the box, sees neither it nor typed text.
        press(&mut input, &[keys::ENTER], "x");
        let (field, answer) = step(&mut input, true);
        assert_eq!(answer, Some(0));
        assert!(!field.focused && !field.changed && !field.submitted);
        // Esc picks the last button.
        press(&mut input, &[keys::ESC], "");
        assert_eq!(step(&mut input, true).1, Some(2));
        // Keys that come as the box goes are nobody's: the field gets them
        // in the frame after.
        press(&mut input, &[], "b");
        let (field, _) = step(&mut input, false);
        assert!(!field.focused && !field.changed);
        let (field, _) = step(&mut input, false);
        assert!(field.focused && field.changed);
        assert!(input.text.is_empty());
        drop(step);
        assert_eq!(text, "draftab");
    }

    #[test]
    fn animations_of_widgets_that_moved_away_are_dropped() {
        let (mut ctx, mut state, mut input) = (context(), UiState::default(), Input::default());
        input.pointer = Some((5, 5));
        // A button that moves a pixel every frame (a window being resized)
        // has a new id every frame.
        for x in 0..1000 {
            frame(&mut ctx, &mut state, &mut input, |ui| ui.button(Rect::new(x, 0, 80, 30), "OK"));
        }
        // Values unused for that many frames go every that many frames.
        assert!(state.animations.len() < 2 * ANIMATION_FRAMES as usize, "{}", state.animations.len());
        // One that stays keeps its value however long it rests.
        let mut hover = 0.0;
        for _ in 0..1000 {
            hover = frame(&mut ctx, &mut state, &mut input, |ui| ui.animate(7, 1.0, 8.0));
        }
        assert_eq!(hover, 1.0);
    }

    #[test]
    fn a_tooltip_waits_each_time_the_pointer_comes_back() {
        let (mut ctx, mut state, mut input) = (context(), UiState::default(), Input::default());
        let anchor = Rect::new(10, 10, 40, 30);
        let mut at = |input: &mut Input, now: u64, over: bool| {
            input.now = now;
            frame(&mut ctx, &mut state, input, |ui| {
                if over {
                    ui.tooltip(1, anchor, "Tip");
                }
                ui.overlays.len()
            })
        };
        assert_eq!(at(&mut input, 0, true), 0);
        assert_eq!(at(&mut input, 700_000_000, true), 1);
        // The pointer leaves and comes back: the tooltip waits again.
        assert_eq!(at(&mut input, 800_000_000, false), 0);
        assert_eq!(at(&mut input, 2_000_000_000, true), 0);
        assert_eq!(at(&mut input, 2_700_000_000, true), 1);
    }

    #[test]
    fn under_a_modal_dialog_the_menu_bar_is_inert() {
        let (mut ctx, mut state, mut input) = (context(), UiState::default(), Input::default());
        let menus = [Menu::new("File", alloc::vec![MenuItem::new("Open")])];
        let draw = |ui: &mut Ui| {
            ui.menu_bar(Rect::new(0, 0, 480, 32), &menus);
            ui.modal(200, 100, |_, _| ())
        };
        frame(&mut ctx, &mut state, &mut input, draw);
        // F10 opens no menu behind the dialog, nor does a click on a title.
        press(&mut input, &[keys::F10], "");
        frame(&mut ctx, &mut state, &mut input, draw);
        assert_eq!(state.open_menu, None);
        input.pointer = Some((20, 16));
        (input.pressed[0], input.down[0]) = (true, true);
        frame(&mut ctx, &mut state, &mut input, draw);
        assert_eq!(state.open_menu, None);
        // Focus is the dialog's content's alone.
        state.focus = Some(42);
        let (inside, outside) = frame(&mut ctx, &mut state, &mut input, |ui| {
            let outside = ui.focused(42);
            (ui.modal(200, 100, |ui, _| ui.focused(42)), outside)
        });
        assert!(inside && !outside);
    }
}

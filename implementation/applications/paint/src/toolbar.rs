// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The toolbar (Undo and Redo, the tools, the shapes, the colours and the palette) and, under
//! it, the options of the tool in hand.

use alloc::format;

use paint_core::fill::Reach;
use paint_core::settings::{CUSTOM, FontChoice, Settings, Tool, palette};
use paint_core::shapes::{Kind, Paint, Shape};
use vgfx::{Align, Color, Rect};
use vproto::display::Cursor;
use vui::{Font, Icon, Ui};

/// What the toolbar asks of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Undo,
    Redo,
    /// Choose the primary (or the secondary) colour in the colour dialog.
    EditPrimary,
    EditSecondary,
    /// The tool changed (what was under way ends).
    ToolChanged,
    /// A setting of the shape or the text under way changed: draw it again.
    Restyle,
    /// The selection's commands, from the options bar.
    Crop,
    RotateLeft,
    RotateRight,
    FlipHorizontal,
    FlipVertical,
    SelectAll,
}

/// The toolbar's height, and the options bar's.
pub const TOOLBAR_H: i32 = 60;
pub const OPTIONS_H: i32 = 40;

/// The icon of each tool.
pub fn icon(tool: Tool) -> Icon {
    match tool {
        Tool::Select => Icon::Select,
        Tool::Pencil => Icon::Edit,
        Tool::Brush => Icon::Brush,
        Tool::Airbrush => Icon::Spray,
        Tool::Eraser => Icon::Eraser,
        Tool::Fill => Icon::Fill,
        Tool::Picker => Icon::Eyedropper,
        Tool::Text => Icon::Text,
        Tool::Shape => Icon::Shapes,
    }
}

/// A square button that is on (the tool in hand) or off; returns whether it was clicked.
fn tool_button(ui: &mut Ui, r: Rect, icon: Icon, tip: &str, on: bool) -> bool {
    let t = ui.theme().clone();
    let id = ui.id(tip) ^ 0x7001;
    let resp = ui.interact(id, r);
    let hover = ui.animate(id, if resp.hovered { 1.0 } else { 0.0 }, 9.0);
    if on {
        ui.canvas.fill_rounded_rect(r, t.radius, t.accent.with_alpha(70));
        ui.canvas.stroke_rounded_rect(r, t.radius, 1.0, t.accent);
    } else if hover > 0.0 {
        ui.canvas.fill_rounded_rect(r, t.radius, Color::rgba(255, 255, 255, (hover * 22.0) as u8));
    }
    ui.icon(r, icon, 19.0, if on { t.text } else { t.text_dim.lerp(t.text, hover) });
    if resp.hovered {
        ui.set_cursor(Cursor::Hand);
        ui.tooltip(id, r, tip);
    }
    resp.clicked
}

fn separator(ui: &mut Ui, x: i32, r: Rect) {
    let c = ui.theme().border;
    ui.canvas.fill_rect(Rect::new(x, r.y + 10, 1, r.h - 20), c);
}

/// A colour's swatch: over a checkerboard where it is transparent.
pub fn swatch(ui: &mut Ui, r: Rect, c: Color, radius: f32) {
    if c.a() < 255 {
        ui.canvas.save();
        ui.canvas.clip_to(r);
        ui.canvas.fill_rect(r, Color::hex(0xF2F2F2));
        let s = (r.w / 4).max(3);
        for y in (r.y..r.bottom()).step_by(s as usize) {
            for x in (r.x..r.right()).step_by(s as usize) {
                if ((x - r.x) / s + (y - r.y) / s) % 2 == 1 {
                    ui.canvas.fill_rect(Rect::new(x, y, s, s), Color::hex(0xCCCCD2));
                }
            }
        }
        ui.canvas.restore();
    }
    ui.canvas.fill_rounded_rect(r, radius, c);
    ui.canvas.stroke_rounded_rect(r, radius, 1.0, Color::rgba(255, 255, 255, 60));
}

/// Draws the toolbar in `r`.
pub fn toolbar(ui: &mut Ui, r: Rect, s: &mut Settings, can_undo: bool, can_redo: bool) -> Option<Request> {
    let t = ui.theme().clone();
    ui.canvas.fill_rect(r, t.surface);
    ui.canvas.fill_rect(Rect::new(r.x, r.bottom() - 1, r.w, 1), t.border);
    let mut request = None;
    let mid = r.y + (r.h - 36) / 2;
    let mut x = r.x + 10;

    let undo = Rect::new(x, mid, 36, 36);
    if can_undo && ui.icon_button(undo, Icon::Undo, "Undo (Ctrl+Z)") {
        request = Some(Request::Undo);
    } else if !can_undo {
        ui.icon(undo, Icon::Undo, 19.0, t.text_faint);
    }
    x += 38;
    let redo = Rect::new(x, mid, 36, 36);
    if can_redo && ui.icon_button(redo, Icon::Redo, "Redo (Ctrl+Y)") {
        request = Some(Request::Redo);
    } else if !can_redo {
        ui.icon(redo, Icon::Redo, 19.0, t.text_faint);
    }
    x += 46;
    separator(ui, x - 5, r);

    for tool in Tool::ALL.into_iter().filter(|t| *t != Tool::Shape) {
        let tip = format!("{} ({})", tool.name(), tool.key());
        if tool_button(ui, Rect::new(x, mid, 36, 36), icon(tool), &tip, s.tool == tool) && s.tool != tool {
            s.tool = tool;
            request = Some(Request::ToolChanged);
        }
        x += 38;
    }
    x += 8;
    separator(ui, x - 5, r);

    // The shapes: two rows of six, each drawn as itself.
    let cell = 25;
    let top = r.y + (r.h - 2 * cell) / 2;
    for (i, kind) in Kind::ALL.into_iter().enumerate() {
        let c = Rect::new(x + (i as i32 % 6) * cell, top + (i as i32 / 6) * cell, cell - 1, cell - 1);
        let id = ui.id(kind.name()) ^ 0x5a9e;
        let resp = ui.interact(id, c);
        let on = s.tool == Tool::Shape && s.shape == kind;
        if on {
            ui.canvas.fill_rounded_rect(c, 4.0, t.accent.with_alpha(70));
        } else if resp.hovered {
            ui.canvas.fill_rounded_rect(c, 4.0, Color::rgba(255, 255, 255, 22));
        }
        let inset = 5.0;
        let shape = Shape {
            kind,
            a: (c.x as f32 + inset, c.y as f32 + inset + if kind.is_line() { c.h as f32 - 2.0 * inset } else { 0.0 }),
            b: (c.right() as f32 - inset, if kind.is_line() { c.y as f32 + inset } else { c.bottom() as f32 - inset }),
            paint: Paint::Outline,
            width: 1.4,
            outline: if on { t.text } else { t.text_dim },
            fill: Color::TRANSPARENT,
        };
        shape.draw(&mut ui.canvas);
        if resp.hovered {
            ui.set_cursor(Cursor::Hand);
            ui.tooltip(id, c, &format!("{} (O)", kind.name()));
        }
        if resp.clicked {
            let changed = s.tool != Tool::Shape;
            s.tool = Tool::Shape;
            s.shape = kind;
            request = Some(if changed { Request::ToolChanged } else { Request::Restyle });
        }
    }
    x += 6 * cell + 12;
    separator(ui, x - 6, r);

    // The two colours: the primary in front, the secondary behind it.
    let front = Rect::new(x + 2, r.y + 10, 28, 28);
    let back = Rect::new(x + 16, r.y + 22, 28, 28);
    swatch(ui, back, s.secondary, 5.0);
    swatch(ui, front, s.primary, 5.0);
    let back_only = Rect::new(back.x, front.bottom(), back.w, back.bottom() - front.bottom());
    let back_side = Rect::new(front.right(), back.y, back.right() - front.right(), back.h);
    let resp_front = ui.interact(ui.id("primary-swatch"), front);
    let resp_back = ui.interact(ui.id("secondary-swatch"), back_only.union(&back_side));
    if resp_front.hovered {
        ui.tooltip(ui.id("primary-swatch"), front, "Primary colour: the left button's");
        ui.set_cursor(Cursor::Hand);
    }
    if resp_back.hovered {
        ui.tooltip(ui.id("secondary-swatch"), back, "Secondary colour: the right button's");
        ui.set_cursor(Cursor::Hand);
    }
    if resp_front.clicked {
        request = Some(Request::EditPrimary);
    }
    if resp_back.clicked {
        request = Some(Request::EditSecondary);
    }
    let swap = Rect::new(x + 32, r.y + 6, 16, 16);
    if ui.icon_button(swap, Icon::Swap, "Swap the colours (X)") {
        core::mem::swap(&mut s.primary, &mut s.secondary);
        request = request.or(Some(Request::Restyle));
    }
    x += 58;

    // The palette, then the colours made, as many columns as fit.
    let size = 20;
    let step = size + 3;
    let room = (r.right() - x - 46) / step;
    let colors: alloc::vec::Vec<Color> = palette().collect();
    let columns = (colors.len() as i32 / 2).min(room.max(0));
    let custom_cols = ((room - columns - 1).max(0)).min(CUSTOM as i32 / 2);
    let mut cells: alloc::vec::Vec<(Rect, Option<Color>)> = alloc::vec::Vec::new();
    for row in 0..2 {
        for col in 0..columns {
            let c = colors[(row * colors.len() as i32 / 2 + col) as usize];
            cells.push((Rect::new(x + col * step, top + row * (size + 4), size, size), Some(c)));
        }
        for col in 0..custom_cols {
            let i = (row * custom_cols + col) as usize;
            let rect = Rect::new(x + (columns + col) * step + 8, top + row * (size + 4), size, size);
            cells.push((rect, s.custom.get(i).copied().flatten()));
        }
    }
    for (i, (cell, color)) in cells.into_iter().enumerate() {
        let id = ui.id("palette") ^ ((i as u64 + 1) << 8);
        let resp = ui.interact(id, cell);
        match color {
            Some(c) => swatch(ui, cell, c, 4.0),
            None => ui.canvas.stroke_rounded_rect(cell, 4.0, 1.0, t.border_strong),
        }
        if let Some(c) = color {
            if resp.hovered {
                ui.canvas.stroke_rounded_rect(cell.inflate(1), 5.0, 1.5, t.text);
                ui.set_cursor(Cursor::Hand);
                ui.tooltip(id, cell, &paint_core::color::hex(c));
            }
            if resp.clicked {
                s.primary = c;
                request = request.or(Some(Request::Restyle));
            } else if resp.right_clicked {
                s.secondary = c;
                request = request.or(Some(Request::Restyle));
            }
        }
    }
    let more = Rect::new((x + (columns + custom_cols) * step + 14).min(r.right() - 40), mid, 34, 36);
    if ui.icon_button(more, Icon::Palette, "More colours") {
        request = Some(Request::EditPrimary);
    }
    request
}

/// A labelled slider of a whole number from `min` to `max` with its value and `unit` after
/// it; returns the width it took.
fn slider(ui: &mut Ui, x: i32, r: Rect, label: &str, id: &str, value: &mut f32, min: f32, max: f32, unit: &str) -> i32 {
    let t = ui.theme().clone();
    let lw = ui.measure(label, Font::Regular, 13.0) as i32 + 8;
    ui.label(Rect::new(x, r.y, lw, r.h), label, Font::Regular, 13.0, t.text_dim, Align::Left);
    let sr = Rect::new(x + lw, r.y + 8, 120, r.h - 16);
    ui.slider(sr, id, value, min, max);
    *value = libm::roundf(*value);
    let text = format!("{}{unit}", *value as i32);
    ui.label(Rect::new(sr.right() + 10, r.y, 52, r.h), &text, Font::Bold, 13.0, t.text, Align::Left);
    lw + 120 + 10 + 52 + 14
}

/// Buttons of which one is chosen; returns the width they took, and the choice if it changed.
pub fn segmented(ui: &mut Ui, x: i32, r: Rect, id: &str, options: &[&str], chosen: usize) -> (i32, Option<usize>) {
    let t = ui.theme().clone();
    let widths: alloc::vec::Vec<i32> = options.iter().map(|o| ui.measure(o, Font::Regular, 13.0) as i32 + 20).collect();
    let total: i32 = widths.iter().sum::<i32>() + 4;
    let outer = Rect::new(x, r.y + 6, total, r.h - 12);
    ui.canvas.fill_rounded_rect(outer, t.radius, t.control);
    let mut cx = x + 2;
    let mut picked = None;
    for (i, (o, w)) in options.iter().zip(&widths).enumerate() {
        let cell = Rect::new(cx, outer.y + 2, *w, outer.h - 4);
        let iid = ui.id(id) ^ ((i as u64 + 1) << 12);
        let resp = ui.interact(iid, cell);
        if i == chosen {
            ui.canvas.fill_rounded_rect(cell, t.radius - 1.0, t.accent);
        } else if resp.hovered {
            ui.canvas.fill_rounded_rect(cell, t.radius - 1.0, t.control_hover);
        }
        let color = if i == chosen { t.on_accent } else { t.text_dim };
        ui.label(cell, o, Font::Regular, 13.0, color, Align::Center);
        if resp.hovered {
            ui.set_cursor(Cursor::Hand);
        }
        if resp.clicked && i != chosen {
            picked = Some(i);
        }
        cx += w;
    }
    (total + 14, picked)
}

/// A checkbox; returns the width it took and whether it was toggled.
fn check(ui: &mut Ui, x: i32, r: Rect, label: &str, value: &mut bool) -> (i32, bool) {
    let w = ui.measure(label, Font::Regular, 13.0) as i32 + 36;
    let toggled = ui.checkbox(Rect::new(x, r.y, w, r.h), label, value);
    (w + 10, toggled)
}

/// The options of the tool in hand, in `r`. `selection`: there is a selection (its commands
/// show).
pub fn options(ui: &mut Ui, r: Rect, s: &mut Settings, selection: bool) -> Option<Request> {
    let t = ui.theme().clone();
    ui.canvas.fill_rect(r, t.bg.lerp(t.surface, 0.5));
    ui.canvas.fill_rect(Rect::new(r.x, r.bottom() - 1, r.w, 1), t.border);
    let mut x = r.x + 14;
    let mut request = None;
    let pct = |v: u8| libm::roundf(v as f32 * 100.0 / 255.0);
    let byte = |p: f32| libm::roundf(p.clamp(1.0, 100.0) * 2.55) as u8;
    let hint = |ui: &mut Ui, x: i32, text: &str| {
        let t = ui.theme().clone();
        ui.label(Rect::new(x, r.y, r.right() - x - 10, r.h), text, Font::Regular, 13.0, t.text_faint, Align::Left);
    };
    match s.tool {
        Tool::Pencil => {
            x += slider(ui, x, r, "Size", "pencil-size", &mut s.pencil, 1.0, 32.0, " px");
            hint(ui, x, "Draws whole pixels. Shift and a click draw a line.");
        }
        Tool::Brush | Tool::Airbrush | Tool::Eraser => {
            let tip = match s.tool {
                Tool::Brush => &mut s.brush,
                Tool::Airbrush => &mut s.airbrush,
                _ => &mut s.eraser,
            };
            x += slider(ui, x, r, "Size", "tip-size", &mut tip.size, 1.0, 200.0, " px");
            if s.tool != Tool::Airbrush {
                let mut h = libm::roundf(tip.hardness * 100.0);
                x += slider(ui, x, r, "Hardness", "tip-hardness", &mut h, 0.0, 100.0, "%");
                tip.hardness = h / 100.0;
            }
            let mut o = pct(tip.opacity);
            let label = if s.tool == Tool::Airbrush { "Flow" } else { "Opacity" };
            x += slider(ui, x, r, label, "tip-opacity", &mut o, 1.0, 100.0, "%");
            tip.opacity = byte(o);
            if r.right() - x > 220 {
                hint(
                    ui,
                    x,
                    if s.tool == Tool::Eraser { "Erases to transparency." } else { "Right button: secondary colour." },
                );
            }
        }
        Tool::Fill => {
            let mut tol = pct(s.tolerance);
            x += slider(ui, x, r, "Tolerance", "fill-tolerance", &mut tol, 0.0, 100.0, "%");
            s.tolerance = libm::roundf(tol * 2.55) as u8;
            let chosen = if s.reach == Reach::Global { 1 } else { 0 };
            let (w, picked) = segmented(ui, x, r, "fill-reach", &["Contiguous", "Global"], chosen);
            if let Some(p) = picked {
                s.reach = if p == 1 { Reach::Global } else { Reach::Contiguous };
            }
            x += w;
            let (w, _) = check(ui, x, r, "Smooth edges", &mut s.soft_fill);
            x += w;
            hint(ui, x, "");
        }
        Tool::Picker => hint(ui, x, "Click to pick the primary colour, right-click the secondary."),
        Tool::Text => {
            let chosen = FontChoice::ALL.iter().position(|f| *f == s.font).unwrap_or(0);
            let names: alloc::vec::Vec<&str> = FontChoice::ALL.iter().map(|f| f.name()).collect();
            let (w, picked) = segmented(ui, x, r, "text-font", &names, chosen);
            if let Some(p) = picked {
                s.font = FontChoice::ALL[p];
                request = Some(Request::Restyle);
            }
            x += w;
            let before = s.text_size;
            x += slider(ui, x, r, "Size", "text-size", &mut s.text_size, 8.0, 200.0, " px");
            let (w, toggled) = check(ui, x, r, "Background", &mut s.text_background);
            x += w;
            if toggled || before != s.text_size {
                request = Some(Request::Restyle);
            }
            hint(ui, x, "Click to write. Esc or a click elsewhere places the text; Ctrl+Z removes it.");
        }
        Tool::Shape => {
            let chosen = match s.shape_paint {
                Paint::Outline => 0,
                Paint::Fill => 1,
                Paint::Both => 2,
            };
            let (w, picked) = segmented(ui, x, r, "shape-paint", &["Outline", "Fill", "Outline and fill"], chosen);
            if let Some(p) = picked {
                s.shape_paint = [Paint::Outline, Paint::Fill, Paint::Both][p];
                request = Some(Request::Restyle);
            }
            x += w;
            let before = s.shape_width;
            x += slider(ui, x, r, "Width", "shape-width", &mut s.shape_width, 1.0, 50.0, " px");
            if before != s.shape_width {
                request = Some(Request::Restyle);
            }
            hint(ui, x, "Shift: squares, circles, straight lines.");
        }
        Tool::Select => {
            let buttons: [(Icon, &str, Request); 6] = [
                (Icon::Crop, "Crop to the selection (Ctrl+Shift+X)", Request::Crop),
                (Icon::Rotate, "Rotate left", Request::RotateLeft),
                (Icon::RotateRight, "Rotate right", Request::RotateRight),
                (Icon::FlipHorizontal, "Flip horizontally", Request::FlipHorizontal),
                (Icon::FlipVertical, "Flip vertically", Request::FlipVertical),
                (Icon::Fullscreen, "Select all (Ctrl+A)", Request::SelectAll),
            ];
            for (icon, tip, req) in buttons {
                let b = Rect::new(x, r.y + 4, 32, r.h - 8);
                // Cropping needs a selection; the rest turn the picture without one.
                let enabled = selection || req != Request::Crop;
                if !enabled {
                    ui.icon(b, icon, 18.0, t.text_faint);
                } else if ui.icon_button(b, icon, tip) {
                    request = Some(req);
                }
                x += 34;
            }
            hint(ui, x + 10, "Drag to select. Drag the selection to move it, its handles to stretch it.");
        }
    }
    request
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Paint for the voice agent: what it shows (the picture, its size and file, the tool and the
//! colours, the selection, what can be undone) and what it does (drawing shapes, lines, text
//! and fills, colours, selections, crops, resizes, turns and effects, undoing, opening and
//! saving), through the same document operations as the mouse and the menus, so that the
//! window shows what the agent did as it does it.
//!
//! Coordinates are the picture's pixels from its top left corner. Colours are names ("dark
//! blue"), `#RRGGBB` or `transparent`.
//!
//! Drawing is routine (Undo takes it back), and so is saving where the file is new or holds
//! only what the agent saved; replacing a file (`save_as`) is sensitive, and throwing away
//! unsaved changes (`discard_changes`) destructive. Opening or starting a picture over unsaved
//! changes is refused: the agent saves them or discards them first.

use gfx::{Color, Rect};
use paint_core::Document;
use paint_core::codec::{self, format_of};
use paint_core::color;
use paint_core::effects::Effect;
use paint_core::fill::Reach;
use paint_core::settings::{FontChoice, Tool};
use paint_core::shapes::{Kind, Paint, Shape};
use paint_core::text::TextBox;
use paint_core::transform::{Anchor, Resample};
use ui::agent::{
    self, Action, AppAgentInfo, Risk, Value, arg_bool, arg_f64, arg_list, arg_opt_int, arg_opt_str, arg_path, arg_str,
    json, show_path,
};

use crate::{NEW_SIZE, PaintApp};

pub fn info() -> AppAgentInfo {
    agent::info(
        "Paint: draws and edits pictures (shapes, lines, text, fills, selections, crops, resizes, turns and effects) and opens and saves PNG, JPEG, BMP and QOI files. Coordinates are the picture's pixels from its top left; colours are names, #RRGGBB or transparent.",
        vec![
            Action::new(
                "new_picture",
                "Starts a new picture (refused over unsaved changes: save or discard them first)",
            )
            .param("width", "integer", "Width in pixels (960 by default)", false)
            .param("height", "integer", "Height in pixels (540 by default)", false)
            .param("background", "string", "Its colour, or transparent (white by default)", false)
            .build(),
            Action::new(
                "open_picture",
                "Opens a picture file (refused over unsaved changes: save or discard them first)",
            )
            .param("path", "string", "The file, such as ~/Pictures/Lighthouse.jpg", true)
            .build(),
            Action::new(
                "draw_shape",
                "Draws a shape between two corners (a line between its two ends): rectangle, rounded rectangle, \
                 ellipse (or circle), line, arrow, triangle, right triangle, diamond, pentagon, hexagon, star or heart",
            )
            .param("shape", "string", "The shape", true)
            .param("from_x", "number", "The first corner's x (a line's start)", true)
            .param("from_y", "number", "The first corner's y", true)
            .param("to_x", "number", "The opposite corner's x (a line's end)", true)
            .param("to_y", "number", "The opposite corner's y", true)
            .param("color", "string", "The outline's colour (the primary colour by default)", false)
            .param("fill", "string", "The inside's colour, if it is filled (not filled by default)", false)
            .param("line_width", "number", "The outline's width in pixels (3 by default)", false)
            .build(),
            Action::new("draw_line", "Draws a line straight from point to point, with the brush, pencil or airbrush")
                .param(
                    "points",
                    "array",
                    "The points in order, each \"x,y\", such as [\"10,20\", \"40,60\"]; many close points make a curve",
                    true,
                )
                .param("color", "string", "The colour (the primary colour by default)", false)
                .param("size", "number", "The tip's width in pixels (the tool's own by default)", false)
                .choice(
                    "tool",
                    "What draws it (the brush by default)",
                    false,
                    &["brush", "pencil", "airbrush", "eraser"],
                )
                .build(),
            Action::new("draw_text", "Writes text on the picture, its top left corner at a point")
                .param("text", "string", "The text, with \\n between lines", true)
                .param("x", "number", "Left edge", true)
                .param("y", "number", "Top edge", true)
                .param("size", "number", "Text size in pixels (32 by default)", false)
                .param("color", "string", "Its colour (the primary colour by default)", false)
                .choice("font", "The font (sans by default)", false, &["sans", "bold", "mono", "mono bold"])
                .param("width", "number", "The width it wraps at, in pixels (to the picture's edge by default)", false)
                .build(),
            Action::new("fill", "Fills the area of one colour around a point with a colour, as the paint bucket does")
                .param("x", "number", "A point in the area", true)
                .param("y", "number", "A point in the area", true)
                .param("color", "string", "The colour (the primary colour by default)", false)
                .param(
                    "tolerance",
                    "number",
                    "How different colours still count as the area's, 0 to 100 (12 by default)",
                    false,
                )
                .param("everywhere", "boolean", "Every pixel of that colour, not only the connected area", false)
                .build(),
            Action::new("set_colors", "Sets the primary colour (what the left button paints) and the secondary one")
                .param("primary", "string", "The primary colour", false)
                .param("secondary", "string", "The secondary colour", false)
                .build(),
            Action::new("set_tool", "Chooses the tool in hand, and its size")
                .choice(
                    "tool",
                    "The tool",
                    true,
                    &["select", "pencil", "brush", "airbrush", "eraser", "fill", "color picker", "text", "shapes"],
                )
                .param("size", "number", "Its size in pixels, where it has one", false)
                .param("shape", "string", "For shapes: which", false)
                .build(),
            Action::new("select", "Selects a rectangle of the picture, all of it, or nothing")
                .param("x", "number", "Left edge (with y, width and height)", false)
                .param("y", "number", "Top edge", false)
                .param("width", "number", "Width", false)
                .param("height", "number", "Height", false)
                .choice("what", "all or none instead of a rectangle", false, &["all", "none"])
                .build(),
            Action::new(
                "clear",
                "Clears the selection (or a rectangle given, or the whole picture) to a colour or to transparency",
            )
            .param("color", "string", "The colour (transparent by default)", false)
            .param("x", "number", "Left edge of a rectangle (with y, width and height)", false)
            .param("y", "number", "Top edge", false)
            .param("width", "number", "Width", false)
            .param("height", "number", "Height", false)
            .build(),
            Action::new("crop", "Crops the picture to a rectangle, or to the selection")
                .param("x", "number", "Left edge (the selection's if not given)", false)
                .param("y", "number", "Top edge", false)
                .param("width", "number", "Width", false)
                .param("height", "number", "Height", false)
                .build(),
            Action::new("resize", "Resizes the picture, by pixels or by a percentage")
                .param("width", "integer", "New width in pixels", false)
                .param("height", "integer", "New height in pixels (in proportion to the width if not given)", false)
                .param("percent", "number", "Instead: a percentage of the size", false)
                .param("pixelated", "boolean", "Whole pixels, for pixel art (smooth by default)", false)
                .build(),
            Action::new("canvas_size", "Changes the canvas's size, the picture kept where an anchor says")
                .param("width", "integer", "New width in pixels", true)
                .param("height", "integer", "New height in pixels", true)
                .choice(
                    "anchor",
                    "Where the picture stays (center by default)",
                    false,
                    &[
                        "top left",
                        "top",
                        "top right",
                        "left",
                        "center",
                        "right",
                        "bottom left",
                        "bottom",
                        "bottom right",
                    ],
                )
                .param("color", "string", "The new canvas's colour (the secondary colour by default)", false)
                .build(),
            Action::new("rotate", "Turns the selection, or the picture, clockwise")
                .choice("degrees", "How far", true, &["90", "180", "270"])
                .build(),
            Action::new("flip", "Mirrors the selection, or the picture")
                .choice("direction", "Which way", true, &["horizontal", "vertical"])
                .build(),
            Action::new("effect", "Applies an effect to the selection, or the whole picture")
                .choice(
                    "effect",
                    "The effect",
                    true,
                    &["invert", "black and white", "sepia", "blur", "sharpen", "brightness and contrast"],
                )
                .param("amount", "number", "Blur radius in pixels (4), or sharpening in percent (100)", false)
                .param("brightness", "number", "-100 to 100", false)
                .param("contrast", "number", "-100 to 100", false)
                .build(),
            Action::new("undo", "Undoes the latest changes")
                .param("times", "integer", "How many (1 by default)", false)
                .build(),
            Action::new("redo", "Redoes changes undone")
                .param("times", "integer", "How many (1 by default)", false)
                .build(),
            Action::new("zoom", "Changes how large the picture shows")
                .param("percent", "number", "The zoom in percent (100 is actual size)", false)
                .choice("to", "Or: fit the window, or actual size", false, &["fit", "actual"])
                .build(),
            Action::new("color_at", "Says the colour of the picture at a point")
                .param("x", "number", "x", true)
                .param("y", "number", "y", true)
                .build(),
            Action::new(
                "save",
                "Saves the picture to its file, if that is new or holds only what the agent saved; otherwise \
                 save_as with the same path does it, asking the user. A new picture needs a path for a new file \
                 (.png, .jpg, .bmp or .qoi).",
            )
            .param("path", "string", "Where to save a new picture (the file must not exist yet)", false)
            .build(),
            Action::new("save_as", "Saves the picture to a file, replacing what that file holds")
                .param("path", "string", "The file (.png, .jpg, .bmp or .qoi)", true)
                .risk(Risk::Sensitive)
                .build(),
            Action::new(
                "discard_changes",
                "Throws away the picture's unsaved changes: back to its file, or a blank picture if it has none",
            )
            .param("name", "string", "The picture's name, as Paint shows it (Untitled for a new one)", true)
            .risk(Risk::Destructive)
            .build(),
        ],
    )
}

/// A colour argument (`default` if not given).
fn color_arg(args: &Value, name: &str, default: Color) -> Result<Color, String> {
    match arg_opt_str(args, name) {
        None => Ok(default),
        Some(text) => color::parse(text).ok_or_else(|| format!("there is no colour called {text}")),
    }
}

/// An optional number argument.
fn opt_f32(args: &Value, name: &str) -> Result<Option<f32>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => arg_f64(args, name).map(|v| Some(v as f32)),
    }
}

/// A rectangle from `x`, `y`, `width` and `height`, if any of them is given (then all must be).
fn rect_arg(args: &Value) -> Result<Option<Rect>, String> {
    let parts = [opt_f32(args, "x")?, opt_f32(args, "y")?, opt_f32(args, "width")?, opt_f32(args, "height")?];
    match parts {
        [None, None, None, None] => Ok(None),
        [Some(x), Some(y), Some(w), Some(h)] if w >= 1.0 && h >= 1.0 => {
            Ok(Some(Rect::new(x as i32, y as i32, w as i32, h as i32)))
        }
        [Some(_), Some(_), Some(_), Some(_)] => Err("the width and height must be at least 1 pixel".into()),
        _ => Err("a rectangle needs x, y, width and height".into()),
    }
}

/// A rectangle as the agent reads it.
fn rect_json(r: Rect) -> Value {
    json!({ "x": r.x, "y": r.y, "width": r.w, "height": r.h })
}

/// A colour as the agent reads it: its name and number.
fn color_json(c: Color) -> Value {
    json!({ "name": color::name(c), "hex": color::hex(c) })
}

pub fn state(app: &PaintApp) -> Value {
    let d = &app.doc;
    let s = &app.settings;
    let pending = if d.shape().is_some() {
        Some("a shape not yet placed")
    } else if d.text().is_some() {
        Some("text being typed")
    } else if d.floating().is_some() {
        Some("a selection being moved")
    } else {
        None
    };
    let history: Vec<&str> = d.history().names().collect();
    let recent: Vec<&str> = history.iter().rev().take(5).rev().copied().collect();
    json!({
        "picture": {
            "name": app.title(),
            "file": d.path.as_deref().map(show_path),
            "width": d.width(),
            "height": d.height(),
            "unsaved_changes": d.is_modified(),
        },
        "tool": s.tool.name(),
        "tool_size": s.size(),
        "shape": (s.tool == Tool::Shape).then(|| s.shape.name()),
        "primary_color": color_json(s.primary),
        "secondary_color": color_json(s.secondary),
        "selection": d.selection().map(rect_json),
        "pending": pending,
        "zoom_percent": libm::roundf(app.canvas.view.zoom * 100.0) as i32,
        "recent_changes": recent,
        "can_redo": d.history().can_redo(),
        "busy": app.busy,
        "dialog_open": app.modal.is_some(),
    })
}

/// Refuses to replace a picture with unsaved changes.
fn no_unsaved(app: &PaintApp) -> Result<(), String> {
    if app.doc.is_modified() {
        return Err(format!(
            "{} has unsaved changes: save them (save or save_as) or discard them (discard_changes) first",
            app.title()
        ));
    }
    Ok(())
}

/// A path to save to, with an extension Paint writes.
fn save_path(args: &Value) -> Result<(String, image::Format), String> {
    let path = arg_path(args, "path")?;
    if path.starts_with("/system") {
        return Err("the system folder cannot be written".into());
    }
    let format =
        format_of(&path).ok_or_else(|| format!("{} needs .png, .jpg, .bmp or .qoi at its end", show_path(&path)))?;
    Ok((path, format))
}

/// Writes the picture to `path` now (the agent waits for its answer).
fn write(app: &mut PaintApp, path: &str, format: image::Format) -> Result<Value, String> {
    let (picture, version) = app.doc.snapshot();
    let bytes = codec::encode(&picture, format, 92).map_err(|e| e.to_string())?;
    app.fs.write(path, &bytes).map_err(|e| format!("{} could not be written: {e}", show_path(path)))?;
    app.doc.saved_version(version, path, format);
    app.agent_file = true;
    println!("the agent saved {path}");
    Ok(json!({ "saved": show_path(path), "format": format.name(), "bytes": bytes.len() }))
}

/// Opens `path` now (the agent waits for its answer).
fn open(app: &mut PaintApp, path: &str) -> Result<Value, String> {
    let bytes =
        app.fs.read_limited(path, 256 << 20).map_err(|e| format!("{} could not be read: {e}", show_path(path)))?;
    let o =
        codec::decode(&bytes, app.max_pixels).map_err(|e| format!("{} could not be opened: {e}", show_path(path)))?;
    let (w, h) = (o.picture.width, o.picture.height);
    app.doc = Document::from_picture(o.picture, Some(path.into()), o.format, app.budget);
    app.canvas.fit_next = true;
    app.agent_file = false;
    Ok(json!({ "opened": show_path(path), "width": w, "height": h, "damaged": o.damaged }))
}

/// The point `x`,`y` written as text ("10,20" or "10 20").
fn point(text: &str) -> Option<(f32, f32)> {
    let mut parts = text.split([',', ' ', ';']).filter(|p| !p.is_empty()).map(|p| p.trim().parse::<f32>().ok());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Some(x)), Some(Some(y)), None) if x.is_finite() && y.is_finite() => Some((x, y)),
        _ => None,
    }
}

pub fn invoke(app: &mut PaintApp, action: &str, args: &Value) -> Result<Value, String> {
    // What the mouse left under way is placed first, as clicking a menu would.
    if action != "undo" {
        app.doc.commit();
    }
    match action {
        "new_picture" => {
            no_unsaved(app)?;
            let w = arg_opt_int(args, "width")?.unwrap_or(NEW_SIZE.0 as i64);
            let h = arg_opt_int(args, "height")?.unwrap_or(NEW_SIZE.1 as i64);
            let max = paint_core::transform::MAX_SIDE as i64;
            if !(1..=max).contains(&w) || !(1..=max).contains(&h) {
                return Err(format!("a picture is 1 to {max} pixels a side"));
            }
            let background = color_arg(args, "background", Color::WHITE)?;
            app.new_picture(w as i32, h as i32, background);
            Ok(json!({ "width": w, "height": h, "background": color::name(background) }))
        }
        "open_picture" => {
            no_unsaved(app)?;
            let path = arg_path(args, "path")?;
            open(app, &path)
        }
        "draw_shape" => {
            let name = arg_str(args, "shape")?;
            let kind = Kind::named(name).ok_or_else(|| format!("Paint has no shape called {name}"))?;
            let a = (arg_f64(args, "from_x")? as f32, arg_f64(args, "from_y")? as f32);
            let b = (arg_f64(args, "to_x")? as f32, arg_f64(args, "to_y")? as f32);
            let outline = color_arg(args, "color", app.settings.primary)?;
            let fill = arg_opt_str(args, "fill")
                .map(|f| color::parse(f).ok_or_else(|| format!("there is no colour called {f}")))
                .transpose()?;
            let width = opt_f32(args, "line_width")?.unwrap_or(3.0).clamp(1.0, 100.0);
            let paint = match fill {
                Some(_) if kind.is_line() => Paint::Outline,
                Some(_) => Paint::Both,
                None => Paint::Outline,
            };
            let shape = Shape { kind, a, b, paint, width, outline, fill: fill.unwrap_or(Color::TRANSPARENT) };
            app.doc.draw_shape(shape);
            Ok(json!({ "drew": kind.name(), "bounds": rect_json(shape.bounds().intersect(&app.doc.picture().rect())) }))
        }
        "draw_line" => {
            let texts = arg_list(args, "points")?;
            let points: Vec<(f32, f32)> = texts
                .iter()
                .map(|t| point(t).ok_or_else(|| format!("\"{t}\" is not a point (x,y)")))
                .collect::<Result<_, _>>()?;
            let tool = match arg_opt_str(args, "tool").unwrap_or("brush") {
                "pencil" => Tool::Pencil,
                "airbrush" => Tool::Airbrush,
                "eraser" => Tool::Eraser,
                _ => Tool::Brush,
            };
            let color = color_arg(args, "color", app.settings.primary)?;
            let mut style = app.settings.stroke(tool, color).ok_or("that tool does not draw lines")?;
            if let Some(size) = opt_f32(args, "size")? {
                style.size = size.clamp(1.0, paint_core::brush::MAX_SIZE);
            }
            app.doc.stroke_path(style, &points);
            Ok(json!({ "drew": "line", "points": points.len() }))
        }
        "draw_text" => {
            let text = arg_str(args, "text")?;
            if text.trim().is_empty() {
                return Err("the text is empty".into());
            }
            let x = arg_f64(args, "x")? as i32;
            let y = arg_f64(args, "y")? as i32;
            let size = opt_f32(args, "size")?.unwrap_or(32.0).clamp(6.0, 400.0);
            let color = color_arg(args, "color", app.settings.primary)?;
            let font = match arg_opt_str(args, "font").unwrap_or("sans") {
                "bold" => FontChoice::SansBold,
                "mono" => FontChoice::Mono,
                "mono bold" => FontChoice::MonoBold,
                _ => FontChoice::Sans,
            };
            let width = opt_f32(args, "width")?.map_or(app.doc.width() - x, |w| w as i32).max(8);
            // The interface's fonts, as the text tool uses them: the process's text context.
            let (shared, roles) = ui::SharedText::process();
            let index = roles[match font {
                FontChoice::Sans => 0,
                FontChoice::SansBold => 1,
                FontChoice::Mono => 2,
                FontChoice::MonoBold => 3,
            }];
            let mut b = TextBox::new((x, y), width, size, index, color);
            b.insert(&text.replace("\\n", "\n"));
            let bounds = shared.with(|ctx| {
                app.doc.set_text(b.clone(), ctx);
                b.bounds(ctx)
            });
            app.doc.commit();
            Ok(json!({ "wrote": text, "bounds": rect_json(bounds) }))
        }
        "fill" => {
            let (x, y) = (arg_f64(args, "x")? as i32, arg_f64(args, "y")? as i32);
            let color = color_arg(args, "color", app.settings.primary)?;
            let tolerance = opt_f32(args, "tolerance")?.unwrap_or(12.0).clamp(0.0, 100.0);
            let reach = if arg_bool(args, "everywhere") == Some(true) { Reach::Global } else { Reach::Contiguous };
            if !app.doc.fill(x, y, color, 255, (tolerance * 2.55) as u8, reach, true) {
                return Err(format!(
                    "({x}, {y}) is not on the picture, which is {} by {}",
                    app.doc.width(),
                    app.doc.height()
                ));
            }
            Ok(json!({ "filled_at": [x, y], "color": color::name(color) }))
        }
        "set_colors" => {
            if let Some(p) = arg_opt_str(args, "primary") {
                app.settings.primary = color::parse(p).ok_or_else(|| format!("there is no colour called {p}"))?;
            }
            if let Some(s) = arg_opt_str(args, "secondary") {
                app.settings.secondary = color::parse(s).ok_or_else(|| format!("there is no colour called {s}"))?;
            }
            app.settings_changed(rt::time::now_ns());
            Ok(json!({ "primary": color_json(app.settings.primary), "secondary": color_json(app.settings.secondary) }))
        }
        "set_tool" => {
            let name = arg_str(args, "tool")?;
            let tool = Tool::named(name).ok_or_else(|| format!("Paint has no tool called {name}"))?;
            app.settings.tool = tool;
            if let Some(shape) = arg_opt_str(args, "shape") {
                app.settings.shape = Kind::named(shape).ok_or_else(|| format!("Paint has no shape called {shape}"))?;
                app.settings.tool = Tool::Shape;
            }
            if let Some(size) = opt_f32(args, "size")? {
                app.settings.set_size(size);
            }
            app.settings_changed(rt::time::now_ns());
            Ok(json!({ "tool": app.settings.tool.name(), "size": app.settings.size() }))
        }
        "select" => {
            match (arg_opt_str(args, "what"), rect_arg(args)?) {
                (Some("all"), _) => app.doc.select_all(),
                (Some("none"), _) => app.doc.deselect(),
                (_, Some(r)) => {
                    app.doc.select(r);
                    if app.doc.selection().is_none() {
                        return Err("that rectangle is off the picture".into());
                    }
                }
                _ => return Err("give a rectangle (x, y, width, height), or what: all or none".into()),
            }
            app.settings.tool = Tool::Select;
            Ok(json!({ "selection": app.doc.selection().map(rect_json) }))
        }
        "clear" => {
            let color = color_arg(args, "color", Color::TRANSPARENT)?;
            if let Some(r) = rect_arg(args)? {
                app.doc.select(r);
            }
            let area = app.doc.selection().unwrap_or(app.doc.picture().rect());
            app.doc.clear(color);
            Ok(json!({ "cleared": rect_json(area), "color": color::name(color) }))
        }
        "crop" => {
            let r = match rect_arg(args)? {
                Some(r) => r,
                None => app.doc.selection().ok_or("give the rectangle to crop to, or select one first")?,
            };
            if !app.doc.crop(r) {
                return Err("that rectangle is off the picture".into());
            }
            app.canvas.fit_next = true;
            Ok(json!({ "width": app.doc.width(), "height": app.doc.height() }))
        }
        "resize" => {
            let (w0, h0) = (app.doc.width() as f64, app.doc.height() as f64);
            let (w, h) = match (opt_f32(args, "percent")?, arg_opt_int(args, "width")?, arg_opt_int(args, "height")?) {
                (Some(p), _, _) if p > 0.0 => (libm::round(w0 * p as f64 / 100.0), libm::round(h0 * p as f64 / 100.0)),
                (_, Some(w), Some(h)) => (w as f64, h as f64),
                (_, Some(w), None) => (w as f64, libm::round(h0 * w as f64 / w0)),
                (_, None, Some(h)) => (libm::round(w0 * h as f64 / h0), h as f64),
                _ => return Err("give a width, a height or a percentage".into()),
            };
            let max = paint_core::transform::MAX_SIDE as f64;
            if !(1.0..=max).contains(&w) || !(1.0..=max).contains(&h) {
                return Err(format!("a picture is 1 to {max} pixels a side"));
            }
            let how = if arg_bool(args, "pixelated") == Some(true) { Resample::Pixelated } else { Resample::Smooth };
            app.doc.resize(w as i32, h as i32, how);
            app.canvas.fit_next = true;
            Ok(json!({ "width": app.doc.width(), "height": app.doc.height() }))
        }
        "canvas_size" => {
            let w = arg_opt_int(args, "width")?.ok_or("missing number argument 'width'")?;
            let h = arg_opt_int(args, "height")?.ok_or("missing number argument 'height'")?;
            let max = paint_core::transform::MAX_SIDE as i64;
            if !(1..=max).contains(&w) || !(1..=max).contains(&h) {
                return Err(format!("a canvas is 1 to {max} pixels a side"));
            }
            let anchor = match arg_opt_str(args, "anchor") {
                Some(a) => Anchor::named(a).ok_or_else(|| format!("there is no anchor called {a}"))?,
                None => Anchor::Center,
            };
            let fill = color_arg(args, "color", app.settings.secondary)?;
            app.doc.canvas_size(w as i32, h as i32, anchor, fill);
            app.canvas.fit_next = true;
            Ok(json!({ "width": w, "height": h }))
        }
        "rotate" => {
            let quarters = match arg_str(args, "degrees")?.trim() {
                "90" | "-270" => 1,
                "180" | "-180" => 2,
                "270" | "-90" => 3,
                other => return Err(format!("Paint turns by 90, 180 or 270 degrees, not {other}")),
            };
            let what = if app.doc.selection().is_some() { "selection" } else { "picture" };
            app.doc.rotate(quarters);
            Ok(json!({ "rotated": what, "width": app.doc.width(), "height": app.doc.height() }))
        }
        "flip" => {
            let horizontal = match arg_str(args, "direction")? {
                "horizontal" => true,
                "vertical" => false,
                other => return Err(format!("flip horizontal or vertical, not {other}")),
            };
            let what = if app.doc.selection().is_some() { "selection" } else { "picture" };
            app.doc.flip(horizontal);
            Ok(json!({ "flipped": what }))
        }
        "effect" => {
            let name = arg_str(args, "effect")?;
            let amount = opt_f32(args, "amount")?;
            let effect = match name {
                "invert" => Effect::Invert,
                "black and white" | "grayscale" | "greyscale" => Effect::Grayscale,
                "sepia" => Effect::Sepia,
                "blur" => Effect::Blur(amount.unwrap_or(4.0).clamp(1.0, 200.0) as i32),
                "sharpen" => Effect::Sharpen(amount.unwrap_or(100.0).clamp(0.0, 500.0) / 100.0),
                "brightness and contrast" => Effect::BrightnessContrast(
                    opt_f32(args, "brightness")?.unwrap_or(0.0).clamp(-100.0, 100.0) as i32,
                    opt_f32(args, "contrast")?.unwrap_or(0.0).clamp(-100.0, 100.0) as i32,
                ),
                other => return Err(format!("Paint has no effect called {other}")),
            };
            let area = app.doc.selection().map_or("the picture", |_| "the selection");
            app.doc.effect(effect);
            Ok(json!({ "applied": effect.name(), "to": area }))
        }
        "undo" | "redo" => {
            let times = arg_opt_int(args, "times")?.unwrap_or(1).clamp(1, 100);
            let mut done = Vec::new();
            for _ in 0..times {
                let step = if action == "undo" { app.doc.undo() } else { app.doc.redo() };
                match step {
                    Some(name) => done.push(name),
                    None => break,
                }
            }
            if done.is_empty() {
                return Err(format!("there is nothing to {action}"));
            }
            Ok(json!({ action: done }))
        }
        "zoom" => {
            match (arg_opt_str(args, "to"), opt_f32(args, "percent")?) {
                (Some("fit"), _) => app.canvas.fit(&app.doc),
                (Some("actual"), _) => app.canvas.zoom_to(1.0, &app.doc),
                (_, Some(p)) if p > 0.0 => app.canvas.zoom_to(p / 100.0, &app.doc),
                _ => return Err("give a percentage, or to: fit or actual".into()),
            }
            Ok(json!({ "zoom_percent": libm::roundf(app.canvas.view.zoom * 100.0) as i32 }))
        }
        "color_at" => {
            let (x, y) = (arg_f64(args, "x")? as i32, arg_f64(args, "y")? as i32);
            let c = app.doc.color_at(x, y).ok_or_else(|| format!("({x}, {y}) is not on the picture"))?;
            Ok(json!({ "x": x, "y": y, "color": color_json(c), "opacity_percent": c.a() as u32 * 100 / 255 }))
        }
        "save" => match (&app.doc.path.clone(), arg_opt_str(args, "path")) {
            (Some(path), None) => {
                let replaceable = app.agent_file || !app.fs.exists(path);
                if !replaceable {
                    return Err(format!(
                        "{} holds the user's picture: save_as with that path replaces it (asking the user)",
                        show_path(path)
                    ));
                }
                let format = format_of(path).ok_or("the picture's file has no extension Paint writes: use save_as")?;
                write(app, path, format)
            }
            (None, None) => Err("the picture has no file yet: save needs a path for a new file".into()),
            (_, Some(_)) => {
                let (path, format) = save_path(args)?;
                if app.fs.exists(&path) {
                    return Err(format!("{} already exists: save_as replaces it (asking the user)", show_path(&path)));
                }
                write(app, &path, format)
            }
        },
        "save_as" => {
            let (path, format) = save_path(args)?;
            write(app, &path, format)
        }
        "discard_changes" => {
            // The picture named, still: an approval may come minutes after it was asked for.
            let name = arg_str(args, "name")?;
            if !app.title().eq_ignore_ascii_case(name.trim()) {
                return Err(format!("Paint shows {}, not {name}", app.title()));
            }
            app.doc.cancel();
            match app.doc.path.clone().filter(|p| app.fs.exists(p)) {
                Some(path) => {
                    let mut v = open(app, &path)?;
                    v["discarded"] = json!(true);
                    Ok(v)
                }
                None => {
                    app.new_picture(NEW_SIZE.0, NEW_SIZE.1, Color::WHITE);
                    Ok(json!({ "discarded": true, "new_picture": true }))
                }
            }
        }
        other => Err(format!("Paint has no action called {other}")),
    }
}

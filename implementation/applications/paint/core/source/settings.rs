// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The tools, their settings, the colours, and what Paint remembers of them between runs, in
//! `~/.config/paint/settings`.
//!
//! The file is text, one setting a line (`brush 24 80 100`: size, hardness and opacity in
//! percent); lines that do not read are skipped, so a damaged file loses only what it damaged.

use alloc::format;
use alloc::string::String;

use gfx::Color;

use crate::brush::{self, Tip};
use crate::color::{self, PALETTE};
use crate::fill::Reach;
use crate::shapes::{Kind, Paint};

/// Paint's tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    Select,
    Pencil,
    #[default]
    Brush,
    Airbrush,
    Eraser,
    Fill,
    Picker,
    Text,
    Shape,
}

impl Tool {
    pub const ALL: [Tool; 9] = [
        Tool::Select,
        Tool::Pencil,
        Tool::Brush,
        Tool::Airbrush,
        Tool::Eraser,
        Tool::Fill,
        Tool::Picker,
        Tool::Text,
        Tool::Shape,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Pencil => "Pencil",
            Tool::Brush => "Brush",
            Tool::Airbrush => "Airbrush",
            Tool::Eraser => "Eraser",
            Tool::Fill => "Fill",
            Tool::Picker => "Colour picker",
            Tool::Text => "Text",
            Tool::Shape => "Shapes",
        }
    }

    /// Its key on the keyboard.
    pub fn key(self) -> char {
        match self {
            Tool::Select => 'S',
            Tool::Pencil => 'P',
            Tool::Brush => 'B',
            Tool::Airbrush => 'A',
            Tool::Eraser => 'E',
            Tool::Fill => 'F',
            Tool::Picker => 'K',
            Tool::Text => 'T',
            Tool::Shape => 'O',
        }
    }

    /// The tool called `name` ("brush", "paint bucket", "eyedropper", ...).
    pub fn named(name: &str) -> Option<Tool> {
        let n: String = name.chars().filter(|c| c.is_alphabetic()).collect::<String>().to_lowercase();
        Some(match n.as_str() {
            "select" | "selection" | "rectangleselect" => Tool::Select,
            "pencil" => Tool::Pencil,
            "brush" | "paintbrush" => Tool::Brush,
            "airbrush" | "spray" | "spraycan" => Tool::Airbrush,
            "eraser" | "rubber" => Tool::Eraser,
            "fill" | "bucket" | "paintbucket" | "fillbucket" => Tool::Fill,
            "picker" | "colourpicker" | "colorpicker" | "eyedropper" | "dropper" => Tool::Picker,
            "text" | "type" => Tool::Text,
            "shape" | "shapes" => Tool::Shape,
            _ => return None,
        })
    }

    fn id(self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::Pencil => "pencil",
            Tool::Brush => "brush",
            Tool::Airbrush => "airbrush",
            Tool::Eraser => "eraser",
            Tool::Fill => "fill",
            Tool::Picker => "picker",
            Tool::Text => "text",
            Tool::Shape => "shape",
        }
    }
}

/// A tip's size, hardness and opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TipSettings {
    /// Pixels.
    pub size: f32,
    /// 0 to 1.
    pub hardness: f32,
    /// 0 to 255.
    pub opacity: u8,
}

/// The fonts text can be in (the interface's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontChoice {
    #[default]
    Sans,
    SansBold,
    Mono,
    MonoBold,
}

impl FontChoice {
    pub const ALL: [FontChoice; 4] = [FontChoice::Sans, FontChoice::SansBold, FontChoice::Mono, FontChoice::MonoBold];

    pub fn name(self) -> &'static str {
        match self {
            FontChoice::Sans => "Sans",
            FontChoice::SansBold => "Bold",
            FontChoice::Mono => "Mono",
            FontChoice::MonoBold => "Mono Bold",
        }
    }
}

/// How many colours the custom row keeps.
pub const CUSTOM: usize = 14;

/// The tools' settings and the colours.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub tool: Tool,
    /// What the left button paints with, and the right one.
    pub primary: Color,
    pub secondary: Color,
    pub pencil: f32,
    pub brush: TipSettings,
    pub airbrush: TipSettings,
    pub eraser: TipSettings,
    pub shape: Kind,
    pub shape_paint: Paint,
    pub shape_width: f32,
    /// 0 to 255.
    pub tolerance: u8,
    pub reach: Reach,
    pub soft_fill: bool,
    pub font: FontChoice,
    pub text_size: f32,
    /// Text over the secondary colour.
    pub text_background: bool,
    /// Colours the user made, the latest first.
    pub custom: [Option<Color>; CUSTOM],
    /// The pixel grid at high zoom.
    pub grid: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            tool: Tool::Brush,
            primary: Color::BLACK,
            secondary: Color::WHITE,
            pencil: 1.0,
            brush: TipSettings { size: 8.0, hardness: 0.85, opacity: 255 },
            airbrush: TipSettings { size: 40.0, hardness: 0.0, opacity: 255 },
            eraser: TipSettings { size: 24.0, hardness: 0.9, opacity: 255 },
            shape: Kind::Rectangle,
            shape_paint: Paint::Outline,
            shape_width: 3.0,
            tolerance: 32,
            reach: Reach::Contiguous,
            soft_fill: true,
            font: FontChoice::Sans,
            text_size: 32.0,
            text_background: false,
            custom: [None; CUSTOM],
            grid: true,
        }
    }
}

impl Settings {
    /// The stroke style of a stroking tool, painting with `color`.
    pub fn stroke(&self, tool: Tool, color: Color) -> Option<brush::Style> {
        let (tip, t) = match tool {
            Tool::Pencil => (Tip::Pencil, TipSettings { size: self.pencil, hardness: 1.0, opacity: 255 }),
            Tool::Brush => (Tip::Brush, self.brush),
            Tool::Airbrush => (Tip::Airbrush, self.airbrush),
            Tool::Eraser => (Tip::Eraser, self.eraser),
            _ => return None,
        };
        Some(brush::Style { tip, size: t.size, hardness: t.hardness, opacity: t.opacity, color })
    }

    /// The size of the tool in hand (what `[` and `]` change), if it has one.
    pub fn size(&self) -> Option<f32> {
        Some(match self.tool {
            Tool::Pencil => self.pencil,
            Tool::Brush => self.brush.size,
            Tool::Airbrush => self.airbrush.size,
            Tool::Eraser => self.eraser.size,
            Tool::Shape => self.shape_width,
            Tool::Text => self.text_size,
            _ => return None,
        })
    }

    /// Sets the size of the tool in hand (within its range).
    pub fn set_size(&mut self, size: f32) {
        let s = size.max(1.0);
        match self.tool {
            Tool::Pencil => self.pencil = s.min(32.0),
            Tool::Brush => self.brush.size = s.min(brush::MAX_SIZE),
            Tool::Airbrush => self.airbrush.size = s.min(brush::MAX_SIZE),
            Tool::Eraser => self.eraser.size = s.min(brush::MAX_SIZE),
            Tool::Shape => self.shape_width = s.min(100.0),
            Tool::Text => self.text_size = s.clamp(6.0, 400.0),
            _ => {}
        }
    }

    /// The next size up or down from the tool's (`[` and `]`): a step that grows with it.
    pub fn step_size(&mut self, up: bool) {
        if let Some(s) = self.size() {
            let step = (s * 0.15).max(1.0);
            self.set_size(libm::roundf(if up { s + step } else { s - step }));
        }
    }

    /// Adds a colour to the custom row (first; once).
    pub fn add_custom(&mut self, c: Color) {
        let mut row = [None; CUSTOM];
        row[0] = Some(c);
        let mut i = 1;
        for old in self.custom.iter().flatten() {
            if *old != c && i < CUSTOM {
                row[i] = Some(*old);
                i += 1;
            }
        }
        self.custom = row;
    }

    /// The settings as the file's text.
    pub fn to_text(&self) -> String {
        let tip = |t: &TipSettings| {
            format!(
                "{} {} {}",
                libm::roundf(t.size) as i32,
                libm::roundf(t.hardness * 100.0) as i32,
                t.opacity as u32 * 100 / 255
            )
        };
        let mut out = String::from("paint 1\n");
        out.push_str(&format!("tool {}\n", self.tool.id()));
        out.push_str(&format!("primary {}\n", color::hex(self.primary)));
        out.push_str(&format!("secondary {}\n", color::hex(self.secondary)));
        out.push_str(&format!("pencil {}\n", libm::roundf(self.pencil) as i32));
        out.push_str(&format!("brush {}\n", tip(&self.brush)));
        out.push_str(&format!("airbrush {}\n", tip(&self.airbrush)));
        out.push_str(&format!("eraser {}\n", tip(&self.eraser)));
        let kind = Kind::ALL.iter().position(|k| *k == self.shape).unwrap_or(2);
        let paint = match self.shape_paint {
            Paint::Outline => 0,
            Paint::Fill => 1,
            Paint::Both => 2,
        };
        out.push_str(&format!("shape {kind} {paint} {}\n", libm::roundf(self.shape_width) as i32));
        let reach = if self.reach == Reach::Global { 1 } else { 0 };
        out.push_str(&format!("fill {} {reach} {}\n", self.tolerance, self.soft_fill as u8));
        let font = FontChoice::ALL.iter().position(|f| *f == self.font).unwrap_or(0);
        out.push_str(&format!("text {font} {} {}\n", libm::roundf(self.text_size) as i32, self.text_background as u8));
        out.push_str(&format!("grid {}\n", self.grid as u8));
        for c in self.custom.iter().flatten() {
            out.push_str(&format!("custom {}\n", color::hex(*c)));
        }
        out
    }

    /// The settings `text` (the file's) gives, the defaults for what it lacks.
    pub fn from_text(text: &str) -> Settings {
        let mut s = Settings::default();
        let mut customs = alloc::vec::Vec::new();
        for line in text.lines() {
            let Some((key, value)) = line.split_once(' ') else { continue };
            let nums: alloc::vec::Vec<f32> = value.split(' ').filter_map(|v| v.parse().ok()).collect();
            let tip = |n: &[f32], d: TipSettings| match *n {
                [size, hardness, opacity] => TipSettings {
                    size: size.clamp(1.0, brush::MAX_SIZE),
                    hardness: (hardness / 100.0).clamp(0.0, 1.0),
                    opacity: (opacity.clamp(1.0, 100.0) * 2.55) as u8,
                },
                _ => d,
            };
            match key {
                "tool" => s.tool = Tool::ALL.into_iter().find(|t| t.id() == value).unwrap_or(s.tool),
                "primary" => s.primary = color::parse(value).unwrap_or(s.primary),
                "secondary" => s.secondary = color::parse(value).unwrap_or(s.secondary),
                "pencil" => s.pencil = nums.first().map_or(s.pencil, |v| v.clamp(1.0, 32.0)),
                "brush" => s.brush = tip(&nums, s.brush),
                "airbrush" => s.airbrush = tip(&nums, s.airbrush),
                "eraser" => s.eraser = tip(&nums, s.eraser),
                "shape" => {
                    if let [kind, paint, width] = nums[..] {
                        s.shape = Kind::ALL.get(kind as usize).copied().unwrap_or(s.shape);
                        s.shape_paint =
                            [Paint::Outline, Paint::Fill, Paint::Both].get(paint as usize).copied().unwrap_or_default();
                        s.shape_width = width.clamp(1.0, 100.0);
                    }
                }
                "fill" => {
                    if let [tolerance, reach, soft] = nums[..] {
                        s.tolerance = tolerance.clamp(0.0, 255.0) as u8;
                        s.reach = if reach == 1.0 { Reach::Global } else { Reach::Contiguous };
                        s.soft_fill = soft == 1.0;
                    }
                }
                "text" => {
                    if let [font, size, background] = nums[..] {
                        s.font = FontChoice::ALL.get(font as usize).copied().unwrap_or_default();
                        s.text_size = size.clamp(6.0, 400.0);
                        s.text_background = background == 1.0;
                    }
                }
                "grid" => s.grid = value == "1",
                "custom" => {
                    if let Some(c) = color::parse(value)
                        && customs.len() < CUSTOM
                    {
                        customs.push(c);
                    }
                }
                _ => {}
            }
        }
        for (slot, c) in s.custom.iter_mut().zip(customs) {
            *slot = Some(c);
        }
        s
    }
}

/// The palette's colours.
pub fn palette() -> impl Iterator<Item = Color> {
    PALETTE.iter().map(|&rgb| Color::hex(rgb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let mut s =
            Settings { tool: Tool::Eraser, primary: Color::hex(0x123456), shape: Kind::Star, ..Settings::default() };
        s.brush.size = 42.0;
        s.reach = Reach::Global;
        s.font = FontChoice::Mono;
        s.add_custom(Color::hex(0xFF0000));
        s.add_custom(Color::rgba(0, 255, 0, 128));
        s.add_custom(Color::hex(0xFF0000));
        let back = Settings::from_text(&s.to_text());
        assert_eq!(back.to_text(), s.to_text());
        assert_eq!(back.tool, Tool::Eraser);
        assert_eq!(back.custom[0], Some(Color::hex(0xFF0000)), "the latest first, once");
        assert_eq!(back.custom[1], Some(Color::rgba(0, 255, 0, 128)));
        assert_eq!(back.custom[2], None);
        // A damaged file loses only what it damaged.
        let d = Settings::from_text("tool hammer\nbrush 12 x\nprimary #00FF00\npencil 900\n");
        assert_eq!(
            (d.tool, d.brush, d.primary, d.pencil),
            (Tool::Brush, Settings::default().brush, Color::hex(0x00FF00), 32.0)
        );
    }

    #[test]
    fn sizes_step_with_the_tool() {
        let mut s = Settings { tool: Tool::Brush, ..Settings::default() };
        s.set_size(100.0);
        s.step_size(true);
        assert_eq!(s.brush.size, 115.0);
        s.step_size(false);
        s.step_size(false);
        assert!(s.brush.size < 100.0);
        s.tool = Tool::Pencil;
        s.set_size(100.0);
        assert_eq!(s.pencil, 32.0);
        s.tool = Tool::Fill;
        assert_eq!(s.size(), None);
        assert_eq!(Tool::named("Paint bucket"), Some(Tool::Fill));
        assert_eq!(Tool::named("eye-dropper"), Some(Tool::Picker));
        let style = s.stroke(Tool::Brush, Color::WHITE).unwrap();
        assert_eq!((style.tip, style.size), (Tip::Brush, 83.0));
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Veda icon set: simple line icons designed on a 24x24 grid, drawn
//! as vector paths so they are crisp at any size.

use gfx::{Canvas, Color, FillRule, LineCap, LineJoin, Path, Rect, StrokeStyle, Transform};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    Folder,
    File,
    Document,
    Image,
    Music,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    Shuffle,
    Repeat,
    Volume,
    Mute,
    Search,
    Settings,
    Power,
    Restart,
    Close,
    Plus,
    Minus,
    Check,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    ChevronDown,
    Home,
    Terminal,
    Grid,
    Edit,
    Save,
    Open,
    Trash,
    /// The Trash with something in it: a page and a crumpled ball in an open
    /// bin.
    TrashFull,
    Info,
    Warning,
    Star,
    Heart,
    Gamepad,
    Monitor,
    Cpu,
    Clock,
    Calendar,
    Refresh,
    ZoomIn,
    ZoomOut,
    /// Rotate anticlockwise.
    Rotate,
    /// Rotate clockwise.
    RotateRight,
    Fullscreen,
    /// Leave full screen: the corners pointing inwards.
    FullscreenExit,
    /// A strip of film: videos.
    Video,
    List,
    User,
    Sun,
    Moon,
    Palette,
    Chart,
    Car,
    /// A police car's beacon, shining.
    Siren,
    Rocket,
    Chess,
    Cube,
    Lock,
    Up,
    Download,
    Copy,
    Cut,
    Paste,
    Undo,
    Redo,
    Wallpaper,
    Speaker,
    /// Wi-Fi with a strong signal (three arcs).
    Wifi,
    /// Wi-Fi, fair signal (two arcs).
    WifiFair,
    /// Wi-Fi, weak signal (one arc).
    WifiWeak,
    /// Wi-Fi, no usable signal (the dot only).
    WifiNone,
    /// Wi-Fi switched off or unavailable.
    WifiOff,
    /// A wired network connection.
    Network,
    /// A microphone (on, and switched off).
    Mic,
    MicOff,
    /// The voice agent: a circle in a ring.
    Agent,
    /// A pocket calculator: its display above a grid of keys.
    Calculator,
    /// A paintbrush: painting (Paint).
    Brush,
    /// An eraser block on a line.
    Eraser,
    /// A paint bucket pouring: filling an area.
    Fill,
    /// An eyedropper: picking a colour from a picture.
    Eyedropper,
    /// Two crossed corners: cropping.
    Crop,
    /// A capital T: writing text.
    Text,
    /// A dashed rectangle: selecting part of a picture.
    Select,
    /// A spray can: airbrushing.
    Spray,
    /// A square and a circle: shapes.
    Shapes,
    /// Two arrows, one each way: swapping two things.
    Swap,
    /// The key that deletes leftwards: an arrow-shaped key with a cross.
    Backspace,
    /// A clock turning back: what was done before.
    History,
    /// Two triangles mirrored across an upright line: mirroring left to right.
    FlipHorizontal,
    /// Two triangles mirrored across a level line: mirroring top to bottom.
    FlipVertical,
}

/// Stroke-based icon geometry on a 24x24 grid; `filled` parts are filled.
struct Builder {
    stroke: Path,
    fill: Path,
}

impl Builder {
    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        self.stroke.move_to(x0, y0);
        self.stroke.line_to(x1, y1);
    }
    fn poly(&mut self, pts: &[(f32, f32)], close: bool) {
        for (i, &(x, y)) in pts.iter().enumerate() {
            if i == 0 { self.stroke.move_to(x, y) } else { self.stroke.line_to(x, y) }
        }
        if close {
            self.stroke.close();
        }
    }
    fn fill_poly(&mut self, pts: &[(f32, f32)]) {
        for (i, &(x, y)) in pts.iter().enumerate() {
            if i == 0 { self.fill.move_to(x, y) } else { self.fill.line_to(x, y) }
        }
        self.fill.close();
    }
    fn rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        self.stroke.rounded_rect(x, y, w, h, [r; 4]);
    }
    fn circle(&mut self, cx: f32, cy: f32, r: f32) {
        self.stroke.circle(cx, cy, r);
    }
    /// An arc of its own: a path's arc would be joined to whatever was
    /// drawn before it by a straight line.
    fn arc(&mut self, cx: f32, cy: f32, r: f32, start_deg: f32, sweep_deg: f32) {
        let rad = core::f32::consts::PI / 180.0;
        let (s, c) = libm::sincosf(start_deg * rad);
        self.stroke.move_to(cx + r * c, cy + r * s);
        self.stroke.arc(cx, cy, r, start_deg * rad, sweep_deg * rad);
    }
    fn dot(&mut self, cx: f32, cy: f32, r: f32) {
        self.fill.circle(cx, cy, r);
    }
}

fn build(icon: Icon) -> Builder {
    let mut b = Builder { stroke: Path::new(), fill: Path::new() };
    match icon {
        Icon::Folder => b.poly(
            &[(3.0, 7.0), (3.0, 18.0), (21.0, 18.0), (21.0, 8.5), (12.0, 8.5), (10.0, 5.5), (3.0, 5.5), (3.0, 7.0)],
            true,
        ),
        Icon::File => {
            b.poly(&[(6.0, 3.0), (14.0, 3.0), (19.0, 8.0), (19.0, 21.0), (6.0, 21.0)], true);
            b.poly(&[(14.0, 3.0), (14.0, 8.0), (19.0, 8.0)], false);
        }
        Icon::Document => {
            b.poly(&[(6.0, 3.0), (14.0, 3.0), (19.0, 8.0), (19.0, 21.0), (6.0, 21.0)], true);
            b.poly(&[(14.0, 3.0), (14.0, 8.0), (19.0, 8.0)], false);
            b.line(9.0, 12.5, 16.0, 12.5);
            b.line(9.0, 16.0, 16.0, 16.0);
        }
        Icon::Image | Icon::Wallpaper => {
            b.rrect(3.0, 4.0, 18.0, 16.0, 2.5);
            b.dot(8.5, 9.0, 1.8);
            b.poly(&[(3.5, 18.0), (9.5, 12.0), (13.0, 15.5), (16.0, 12.5), (20.5, 17.0)], false);
        }
        Icon::Music => {
            b.poly(&[(9.0, 18.0), (9.0, 5.0), (20.0, 3.0), (20.0, 16.0)], false);
            b.circle(6.5, 18.0, 2.6);
            b.circle(17.5, 16.0, 2.6);
        }
        Icon::Play => b.fill_poly(&[(7.0, 4.5), (19.5, 12.0), (7.0, 19.5)]),
        Icon::Pause => {
            b.fill.rounded_rect(6.0, 4.5, 4.0, 15.0, [1.0; 4]);
            b.fill.rounded_rect(14.0, 4.5, 4.0, 15.0, [1.0; 4]);
        }
        Icon::Stop => b.fill.rounded_rect(5.5, 5.5, 13.0, 13.0, [2.0; 4]),
        Icon::Next => {
            b.fill_poly(&[(5.0, 5.0), (15.0, 12.0), (5.0, 19.0)]);
            b.fill.rounded_rect(16.0, 5.0, 3.0, 14.0, [1.0; 4]);
        }
        Icon::Previous => {
            b.fill_poly(&[(19.0, 5.0), (9.0, 12.0), (19.0, 19.0)]);
            b.fill.rounded_rect(5.0, 5.0, 3.0, 14.0, [1.0; 4]);
        }
        Icon::Shuffle => {
            b.poly(&[(3.0, 7.0), (7.0, 7.0), (15.0, 17.0), (20.0, 17.0)], false);
            b.poly(&[(3.0, 17.0), (7.0, 17.0), (15.0, 7.0), (20.0, 7.0)], false);
            b.poly(&[(17.5, 4.5), (20.5, 7.0), (17.5, 9.5)], false);
            b.poly(&[(17.5, 14.5), (20.5, 17.0), (17.5, 19.5)], false);
        }
        Icon::Repeat => {
            b.poly(&[(4.0, 11.0), (4.0, 8.0), (19.0, 8.0)], false);
            b.poly(&[(16.0, 5.0), (19.0, 8.0), (16.0, 11.0)], false);
            b.poly(&[(20.0, 13.0), (20.0, 16.0), (5.0, 16.0)], false);
            b.poly(&[(8.0, 19.0), (5.0, 16.0), (8.0, 13.0)], false);
        }
        Icon::Volume | Icon::Speaker => {
            b.poly(&[(3.0, 9.0), (7.0, 9.0), (12.0, 4.5), (12.0, 19.5), (7.0, 15.0), (3.0, 15.0)], true);
            b.arc(12.0, 12.0, 4.0, -45.0, 90.0);
            b.arc(12.0, 12.0, 8.0, -45.0, 90.0);
        }
        Icon::Mute => {
            b.poly(&[(3.0, 9.0), (7.0, 9.0), (12.0, 4.5), (12.0, 19.5), (7.0, 15.0), (3.0, 15.0)], true);
            b.line(16.0, 9.0, 22.0, 15.0);
            b.line(22.0, 9.0, 16.0, 15.0);
        }
        Icon::Search => {
            b.circle(10.5, 10.5, 6.5);
            b.line(15.5, 15.5, 20.5, 20.5);
        }
        Icon::Settings => {
            b.circle(12.0, 12.0, 3.0);
            // Gear: eight teeth around a ring.
            let rad = core::f32::consts::PI / 180.0;
            for i in 0..8 {
                let a = i as f32 * 45.0 * rad;
                let (s, c) = (libm::sinf(a), libm::cosf(a));
                b.line(12.0 + c * 6.5, 12.0 + s * 6.5, 12.0 + c * 9.0, 12.0 + s * 9.0);
            }
            b.circle(12.0, 12.0, 6.5);
        }
        Icon::Power => {
            b.line(12.0, 3.5, 12.0, 11.0);
            b.arc(12.0, 13.0, 7.5, -60.0, 300.0);
        }
        Icon::Restart | Icon::Refresh => {
            b.arc(12.0, 12.0, 7.5, -70.0, 300.0);
            b.poly(&[(14.5, 2.5), (14.6, 5.4), (17.6, 5.0)], false);
        }
        Icon::Close => {
            b.line(6.0, 6.0, 18.0, 18.0);
            b.line(18.0, 6.0, 6.0, 18.0);
        }
        Icon::Plus => {
            b.line(12.0, 5.0, 12.0, 19.0);
            b.line(5.0, 12.0, 19.0, 12.0);
        }
        Icon::Minus => b.line(5.0, 12.0, 19.0, 12.0),
        Icon::Check => b.poly(&[(5.0, 12.5), (10.0, 17.5), (19.5, 7.0)], false),
        Icon::ChevronLeft => b.poly(&[(15.0, 5.0), (8.0, 12.0), (15.0, 19.0)], false),
        Icon::ChevronRight => b.poly(&[(9.0, 5.0), (16.0, 12.0), (9.0, 19.0)], false),
        Icon::ChevronUp => b.poly(&[(5.0, 15.0), (12.0, 8.0), (19.0, 15.0)], false),
        Icon::ChevronDown => b.poly(&[(5.0, 9.0), (12.0, 16.0), (19.0, 9.0)], false),
        Icon::Up => {
            b.line(12.0, 19.0, 12.0, 5.0);
            b.poly(&[(6.0, 11.0), (12.0, 5.0), (18.0, 11.0)], false);
        }
        Icon::Download => {
            b.line(12.0, 4.0, 12.0, 15.0);
            b.poly(&[(7.0, 10.0), (12.0, 15.0), (17.0, 10.0)], false);
            b.poly(&[(4.0, 15.0), (4.0, 20.0), (20.0, 20.0), (20.0, 15.0)], false);
        }
        Icon::Home => {
            b.poly(&[(3.5, 11.0), (12.0, 3.5), (20.5, 11.0)], false);
            b.poly(&[(6.0, 9.0), (6.0, 20.0), (18.0, 20.0), (18.0, 9.0)], false);
            b.poly(&[(10.0, 20.0), (10.0, 14.0), (14.0, 14.0), (14.0, 20.0)], false);
        }
        Icon::Terminal => {
            b.rrect(2.5, 4.0, 19.0, 16.0, 2.5);
            b.poly(&[(6.5, 9.0), (9.5, 12.0), (6.5, 15.0)], false);
            b.line(11.5, 15.0, 16.5, 15.0);
        }
        Icon::Grid => {
            for (x, y) in [(4.0, 4.0), (13.5, 4.0), (4.0, 13.5), (13.5, 13.5)] {
                b.rrect(x, y, 6.5, 6.5, 1.5);
            }
        }
        Icon::Edit => {
            b.poly(&[(4.0, 20.0), (4.5, 16.0), (16.0, 4.5), (19.5, 8.0), (8.0, 19.5)], true);
            b.line(13.5, 7.0, 17.0, 10.5);
        }
        Icon::Save => {
            b.poly(&[(4.0, 4.0), (16.5, 4.0), (20.0, 7.5), (20.0, 20.0), (4.0, 20.0)], true);
            b.poly(&[(8.0, 4.0), (8.0, 9.0), (15.0, 9.0), (15.0, 4.0)], false);
            b.rrect(7.0, 13.0, 10.0, 7.0, 1.0);
        }
        Icon::Open => {
            b.poly(&[(3.0, 18.0), (3.0, 5.5), (9.5, 5.5), (11.5, 8.0), (18.0, 8.0), (18.0, 10.5)], false);
            b.poly(&[(3.0, 18.0), (6.0, 10.5), (21.5, 10.5), (18.5, 18.0)], true);
        }
        Icon::Trash => {
            b.line(4.0, 6.5, 20.0, 6.5);
            b.poly(&[(9.0, 6.5), (9.5, 3.5), (14.5, 3.5), (15.0, 6.5)], false);
            b.poly(&[(6.0, 6.5), (7.0, 20.5), (17.0, 20.5), (18.0, 6.5)], false);
            b.line(10.0, 10.5, 10.0, 16.5);
            b.line(14.0, 10.5, 14.0, 16.5);
        }
        Icon::TrashFull => {
            // The bin without its lid, a page and a crumpled ball (solid,
            // to tell them from the lid's handle) above the rim.
            b.line(4.0, 9.5, 20.0, 9.5);
            b.poly(&[(6.0, 9.5), (7.0, 20.5), (17.0, 20.5), (18.0, 9.5)], false);
            b.line(10.0, 13.0, 10.0, 17.0);
            b.line(14.0, 13.0, 14.0, 17.0);
            b.fill_poly(&[(6.6, 9.5), (7.6, 2.6), (12.9, 3.5), (12.3, 9.5)]);
            b.dot(16.2, 6.9, 2.7);
        }
        Icon::Info => {
            b.circle(12.0, 12.0, 9.0);
            b.line(12.0, 11.0, 12.0, 16.5);
            b.dot(12.0, 7.8, 1.2);
        }
        Icon::Warning => {
            b.poly(&[(12.0, 3.5), (21.5, 20.0), (2.5, 20.0)], true);
            b.line(12.0, 9.5, 12.0, 14.0);
            b.dot(12.0, 17.0, 1.1);
        }
        Icon::Star => b.poly(
            &[
                (12.0, 3.0),
                (14.7, 8.9),
                (21.0, 9.5),
                (16.2, 13.7),
                (17.6, 20.0),
                (12.0, 16.7),
                (6.4, 20.0),
                (7.8, 13.7),
                (3.0, 9.5),
                (9.3, 8.9),
            ],
            true,
        ),
        Icon::Heart => {
            b.stroke.move_to(12.0, 20.0);
            b.stroke.cubic_to(2.0, 13.0, 3.0, 4.5, 8.0, 4.5);
            b.stroke.cubic_to(10.0, 4.5, 11.5, 6.0, 12.0, 7.5);
            b.stroke.cubic_to(12.5, 6.0, 14.0, 4.5, 16.0, 4.5);
            b.stroke.cubic_to(21.0, 4.5, 22.0, 13.0, 12.0, 20.0);
            b.stroke.close();
        }
        Icon::Gamepad => {
            b.rrect(2.5, 7.0, 19.0, 11.0, 5.0);
            b.line(6.5, 12.5, 10.0, 12.5);
            b.line(8.25, 10.75, 8.25, 14.25);
            b.dot(15.0, 11.5, 1.2);
            b.dot(17.5, 13.5, 1.2);
        }
        Icon::Monitor => {
            b.rrect(2.5, 4.0, 19.0, 13.0, 2.0);
            b.line(9.0, 20.5, 15.0, 20.5);
            b.line(12.0, 17.0, 12.0, 20.5);
        }
        Icon::Cpu => {
            b.rrect(6.0, 6.0, 12.0, 12.0, 2.0);
            b.rrect(9.5, 9.5, 5.0, 5.0, 1.0);
            for t in [9.5, 14.5] {
                b.line(t, 2.5, t, 6.0);
                b.line(t, 18.0, t, 21.5);
                b.line(2.5, t, 6.0, t);
                b.line(18.0, t, 21.5, t);
            }
        }
        Icon::Clock => {
            b.circle(12.0, 12.0, 9.0);
            b.poly(&[(12.0, 7.0), (12.0, 12.0), (15.5, 14.0)], false);
        }
        Icon::Calendar => {
            b.rrect(3.5, 5.0, 17.0, 15.5, 2.0);
            b.line(3.5, 10.0, 20.5, 10.0);
            b.line(8.0, 3.0, 8.0, 7.0);
            b.line(16.0, 3.0, 16.0, 7.0);
        }
        Icon::ZoomIn | Icon::ZoomOut => {
            b.circle(10.5, 10.5, 6.5);
            b.line(15.5, 15.5, 20.5, 20.5);
            b.line(7.5, 10.5, 13.5, 10.5);
            if icon == Icon::ZoomIn {
                b.line(10.5, 7.5, 10.5, 13.5);
            }
        }
        Icon::Rotate => {
            b.arc(12.0, 13.0, 7.0, 200.0, 270.0);
            b.poly(&[(3.0, 6.5), (4.6, 11.0), (9.0, 9.4)], false);
        }
        Icon::RotateRight => {
            b.arc(12.0, 13.0, 7.0, -20.0, -270.0);
            b.poly(&[(21.0, 6.5), (19.4, 11.0), (15.0, 9.4)], false);
        }
        Icon::Fullscreen => {
            b.poly(&[(4.0, 9.0), (4.0, 4.0), (9.0, 4.0)], false);
            b.poly(&[(15.0, 4.0), (20.0, 4.0), (20.0, 9.0)], false);
            b.poly(&[(20.0, 15.0), (20.0, 20.0), (15.0, 20.0)], false);
            b.poly(&[(9.0, 20.0), (4.0, 20.0), (4.0, 15.0)], false);
        }
        Icon::FullscreenExit => {
            b.poly(&[(9.0, 4.0), (9.0, 9.0), (4.0, 9.0)], false);
            b.poly(&[(15.0, 4.0), (15.0, 9.0), (20.0, 9.0)], false);
            b.poly(&[(20.0, 15.0), (15.0, 15.0), (15.0, 20.0)], false);
            b.poly(&[(4.0, 15.0), (9.0, 15.0), (9.0, 20.0)], false);
        }
        Icon::Video => {
            b.rrect(3.5, 4.0, 17.0, 16.0, 2.5);
            b.line(8.0, 4.0, 8.0, 20.0);
            b.line(16.0, 4.0, 16.0, 20.0);
            // The sprocket holes along both edges.
            for y in [8.0, 12.0, 16.0] {
                b.line(3.5, y, 8.0, y);
                b.line(16.0, y, 20.5, y);
            }
        }
        Icon::List => {
            for y in [6.0, 12.0, 18.0] {
                b.dot(5.0, y, 1.2);
                b.line(9.0, y, 20.0, y);
            }
        }
        Icon::User => {
            b.circle(12.0, 8.0, 4.0);
            b.arc(12.0, 21.0, 7.5, 180.0, 180.0);
        }
        Icon::Sun => {
            b.circle(12.0, 12.0, 4.0);
            let rad = core::f32::consts::PI / 180.0;
            for i in 0..8 {
                let a = i as f32 * 45.0 * rad;
                let (s, c) = (libm::sinf(a), libm::cosf(a));
                b.line(12.0 + c * 7.0, 12.0 + s * 7.0, 12.0 + c * 9.5, 12.0 + s * 9.5);
            }
        }
        Icon::Moon => {
            b.stroke.move_to(19.5, 14.5);
            b.stroke.cubic_to(15.0, 17.0, 8.0, 13.0, 10.0, 4.0);
            b.stroke.cubic_to(4.0, 6.0, 3.0, 13.0, 6.5, 17.0);
            b.stroke.cubic_to(10.0, 21.0, 17.0, 20.0, 19.5, 14.5);
            b.stroke.close();
        }
        Icon::Palette => {
            b.circle(12.0, 12.0, 9.0);
            b.dot(8.0, 9.0, 1.4);
            b.dot(12.0, 7.0, 1.4);
            b.dot(16.0, 9.0, 1.4);
            b.dot(15.5, 14.5, 1.4);
        }
        Icon::Chart => {
            b.poly(&[(3.5, 3.5), (3.5, 20.5), (20.5, 20.5)], false);
            b.poly(&[(7.0, 16.0), (11.0, 11.0), (14.0, 13.5), (19.5, 6.5)], false);
        }
        Icon::Car => {
            b.poly(&[(3.0, 16.0), (3.0, 12.5), (5.5, 7.5), (18.5, 7.5), (21.0, 12.5), (21.0, 16.0)], true);
            b.line(3.5, 12.5, 20.5, 12.5);
            b.circle(7.5, 16.5, 2.0);
            b.circle(16.5, 16.5, 2.0);
        }
        Icon::Siren => {
            // The dome on its base, and its light.
            b.poly(
                &[
                    (6.5, 15.0),
                    (6.92, 12.9),
                    (8.11, 11.11),
                    (9.9, 9.92),
                    (12.0, 9.5),
                    (14.1, 9.92),
                    (15.89, 11.11),
                    (17.08, 12.9),
                    (17.5, 15.0),
                ],
                false,
            );
            b.poly(&[(4.5, 15.0), (19.5, 15.0), (19.5, 19.0), (4.5, 19.0)], true);
            b.line(12.0, 2.5, 12.0, 5.5);
            b.line(4.0, 6.0, 6.0, 8.0);
            b.line(20.0, 6.0, 18.0, 8.0);
        }
        Icon::Rocket => {
            b.stroke.move_to(12.0, 2.5);
            b.stroke.cubic_to(16.5, 6.0, 16.5, 12.0, 15.0, 16.0);
            b.stroke.line_to(9.0, 16.0);
            b.stroke.cubic_to(7.5, 12.0, 7.5, 6.0, 12.0, 2.5);
            b.stroke.close();
            b.circle(12.0, 9.0, 1.8);
            b.poly(&[(9.0, 13.0), (5.5, 16.0), (5.5, 19.5), (9.0, 16.0)], false);
            b.poly(&[(15.0, 13.0), (18.5, 16.0), (18.5, 19.5), (15.0, 16.0)], false);
            b.line(12.0, 18.0, 12.0, 21.5);
        }
        Icon::Chess => {
            // A knight.
            b.poly(
                &[
                    (7.0, 20.0),
                    (17.0, 20.0),
                    (16.0, 17.0),
                    (16.5, 9.0),
                    (13.0, 4.0),
                    (10.0, 4.5),
                    (5.5, 9.5),
                    (7.0, 11.5),
                    (11.0, 10.0),
                    (8.0, 17.0),
                ],
                true,
            );
            b.dot(11.5, 7.5, 0.9);
        }
        Icon::Cube => {
            b.poly(&[(12.0, 3.0), (20.0, 7.5), (20.0, 16.5), (12.0, 21.0), (4.0, 16.5), (4.0, 7.5)], true);
            b.poly(&[(4.0, 7.5), (12.0, 12.0), (20.0, 7.5)], false);
            b.line(12.0, 12.0, 12.0, 21.0);
        }
        Icon::Wifi | Icon::WifiFair | Icon::WifiWeak | Icon::WifiNone | Icon::WifiOff => {
            let arcs = match icon {
                Icon::WifiNone => 0,
                Icon::WifiWeak => 1,
                Icon::WifiFair => 2,
                _ => 3,
            };
            for r in [5.5, 10.0, 14.5].into_iter().take(arcs) {
                b.arc(12.0, 20.0, r, -135.0, 90.0);
            }
            b.dot(12.0, 20.0, 1.9);
            if icon == Icon::WifiOff {
                b.line(4.0, 4.5, 20.0, 20.5);
            }
        }
        Icon::Network => {
            // Three boxes joined: the mark beside network sockets.
            b.rrect(9.0, 3.0, 6.0, 5.0, 1.5);
            b.rrect(3.0, 16.0, 6.0, 5.0, 1.5);
            b.rrect(15.0, 16.0, 6.0, 5.0, 1.5);
            b.line(12.0, 8.0, 12.0, 12.0);
            b.poly(&[(6.0, 16.0), (6.0, 12.0), (18.0, 12.0), (18.0, 16.0)], false);
        }
        Icon::Mic | Icon::MicOff => {
            b.rrect(9.0, 3.0, 6.0, 11.0, 3.0);
            b.arc(12.0, 11.0, 6.0, 0.0, 180.0);
            b.line(12.0, 17.0, 12.0, 21.0);
            b.line(8.5, 21.0, 15.5, 21.0);
            if icon == Icon::MicOff {
                b.line(4.0, 4.0, 20.0, 20.0);
            }
        }
        Icon::Agent => {
            b.circle(12.0, 12.0, 8.5);
            b.circle(12.0, 12.0, 4.0);
        }
        Icon::Calculator => {
            b.rrect(5.0, 2.5, 14.0, 19.0, 2.5);
            b.rrect(8.0, 5.5, 8.0, 4.0, 1.0);
            for y in [13.25, 17.5] {
                for x in [8.75, 12.0, 15.25] {
                    b.dot(x, y, 1.15);
                }
            }
        }
        Icon::Brush => {
            // The handle, slanting up to the right, its ferrule, and the bristles curving down to
            // their tip at the bottom left.
            b.poly(&[(19.0, 2.6), (21.4, 5.0), (14.6, 11.8), (12.2, 9.4)], true);
            b.poly(&[(12.2, 9.4), (10.6, 11.0), (13.0, 13.4), (14.6, 11.8)], false);
            b.stroke.move_to(10.6, 11.0);
            b.stroke.cubic_to(7.0, 11.0, 6.5, 14.0, 6.0, 16.5);
            b.stroke.cubic_to(5.6, 18.5, 4.8, 19.8, 3.0, 21.0);
            b.stroke.cubic_to(8.5, 21.0, 12.8, 19.0, 13.0, 13.4);
        }
        Icon::Eraser => {
            b.poly(&[(13.5, 3.5), (20.5, 10.5), (11.0, 20.0), (7.0, 20.0), (3.5, 16.5)], true);
            b.line(8.0, 9.0, 15.0, 16.0);
            b.line(11.0, 20.0, 20.5, 20.0);
        }
        Icon::Fill => {
            // A bucket on its side, its open top to the left, a drop falling from its lip.
            b.poly(&[(10.5, 3.0), (19.0, 11.5), (12.0, 18.5), (3.5, 10.0)], true);
            b.line(3.5, 10.0, 19.0, 11.5);
            b.line(10.5, 3.0, 8.0, 0.5);
            b.stroke.move_to(20.5, 14.0);
            b.stroke.cubic_to(22.5, 17.0, 22.5, 19.5, 20.5, 19.5);
            b.stroke.cubic_to(18.5, 19.5, 18.5, 17.0, 20.5, 14.0);
            b.stroke.close();
        }
        Icon::Eyedropper => {
            // The bulb at the top right, the collar across, the tube down to the tip.
            b.stroke.move_to(13.0, 8.0);
            b.stroke.line_to(16.0, 5.0);
            b.stroke.cubic_to(17.5, 3.5, 19.5, 3.5, 20.5, 4.5);
            b.stroke.cubic_to(21.5, 5.5, 21.5, 7.5, 20.0, 9.0);
            b.stroke.line_to(17.0, 12.0);
            b.line(11.5, 6.5, 18.5, 13.5);
            b.poly(&[(15.5, 10.5), (7.0, 19.0), (4.0, 20.0), (5.0, 17.0), (13.5, 8.5)], false);
        }
        Icon::Crop => {
            b.poly(&[(6.5, 2.5), (6.5, 17.5), (21.5, 17.5)], false);
            b.poly(&[(2.5, 6.5), (17.5, 6.5), (17.5, 21.5)], false);
        }
        Icon::Text => {
            b.poly(&[(5.0, 7.5), (5.0, 4.0), (19.0, 4.0), (19.0, 7.5)], false);
            b.line(12.0, 4.0, 12.0, 20.0);
            b.line(9.0, 20.0, 15.0, 20.0);
        }
        Icon::Select => {
            // A dashed rectangle: its corners, and a dash in the middle of each side.
            for (x0, y0, x1, y1) in [(3.5, 7.0, 3.5, 3.5), (3.5, 3.5, 7.0, 3.5), (17.0, 3.5, 20.5, 3.5)] {
                b.line(x0, y0, x1, y1);
            }
            b.poly(&[(20.5, 3.5), (20.5, 7.0)], false);
            b.poly(&[(3.5, 17.0), (3.5, 20.5), (7.0, 20.5)], false);
            b.poly(&[(17.0, 20.5), (20.5, 20.5), (20.5, 17.0)], false);
            b.line(10.5, 3.5, 13.5, 3.5);
            b.line(10.5, 20.5, 13.5, 20.5);
            b.line(3.5, 10.5, 3.5, 13.5);
            b.line(20.5, 10.5, 20.5, 13.5);
        }
        Icon::Spray => {
            b.rrect(3.5, 10.0, 9.5, 11.5, 2.0);
            b.rrect(5.5, 6.5, 5.5, 3.5, 1.0);
            b.line(11.0, 8.25, 13.0, 8.25);
            for (x, y) in [(16.5, 8.25), (19.5, 5.5), (19.5, 11.0), (21.5, 8.25), (17.5, 3.0), (17.5, 13.5)] {
                b.dot(x, y, 1.1);
            }
        }
        Icon::Shapes => {
            b.rrect(3.0, 10.5, 10.5, 10.5, 1.5);
            b.circle(15.0, 9.0, 6.0);
        }
        Icon::Swap => {
            b.line(4.0, 8.0, 19.0, 8.0);
            b.poly(&[(15.5, 4.5), (19.0, 8.0), (15.5, 11.5)], false);
            b.line(20.0, 16.0, 5.0, 16.0);
            b.poly(&[(8.5, 12.5), (5.0, 16.0), (8.5, 19.5)], false);
        }
        Icon::Backspace => {
            b.poly(&[(8.5, 5.0), (20.5, 5.0), (20.5, 19.0), (8.5, 19.0), (2.5, 12.0)], true);
            b.line(11.0, 9.0, 17.0, 15.0);
            b.line(17.0, 9.0, 11.0, 15.0);
        }
        Icon::FlipHorizontal | Icon::FlipVertical => {
            // Drawn either side of an upright axis; turned a quarter for the vertical one.
            let turn = |(x, y): (f32, f32)| if icon == Icon::FlipVertical { (y, x) } else { (x, y) };
            for (y0, y1) in [(2.5, 5.0), (8.0, 10.5), (13.5, 16.0), (19.0, 21.5)] {
                let (p, q) = (turn((12.0, y0)), turn((12.0, y1)));
                b.line(p.0, p.1, q.0, q.1);
            }
            b.poly(&[(9.0, 5.0), (9.0, 19.0), (2.5, 19.0)].map(turn), true);
            b.fill_poly(&[(15.0, 5.0), (15.0, 19.0), (21.5, 19.0)].map(turn));
        }
        Icon::History => {
            // A clock face whose rim turns back into an arrow at the top left.
            b.arc(12.0, 12.0, 8.5, 225.0, 300.0);
            b.poly(&[(3.0, 4.5), (6.0, 6.0), (7.5, 2.8)], false);
            b.poly(&[(12.0, 7.5), (12.0, 12.0), (15.0, 14.0)], false);
        }
        Icon::Lock => {
            b.rrect(5.0, 11.0, 14.0, 10.0, 2.0);
            b.arc(12.0, 11.0, 4.5, 180.0, 180.0);
        }
        Icon::Copy => {
            b.rrect(8.5, 8.5, 12.0, 12.0, 2.0);
            b.poly(
                &[(15.5, 8.5), (15.5, 5.5), (13.5, 3.5), (5.5, 3.5), (3.5, 5.5), (3.5, 13.5), (5.5, 15.5), (8.5, 15.5)],
                false,
            );
        }
        Icon::Cut => {
            b.circle(6.5, 17.5, 2.8);
            b.circle(17.5, 17.5, 2.8);
            b.line(8.5, 15.5, 18.0, 4.0);
            b.line(15.5, 15.5, 6.0, 4.0);
        }
        Icon::Paste => {
            b.rrect(5.0, 4.5, 14.0, 17.0, 2.0);
            b.rrect(9.0, 2.5, 6.0, 4.0, 1.0);
        }
        Icon::Undo => {
            b.poly(&[(8.0, 5.0), (4.0, 9.0), (8.0, 13.0)], false);
            b.stroke.move_to(4.0, 9.0);
            b.stroke.line_to(14.5, 9.0);
            b.stroke.cubic_to(18.5, 9.0, 20.5, 11.5, 20.5, 14.0);
            b.stroke.cubic_to(20.5, 16.5, 18.5, 19.0, 14.5, 19.0);
            b.stroke.line_to(10.0, 19.0);
        }
        Icon::Redo => {
            b.poly(&[(16.0, 5.0), (20.0, 9.0), (16.0, 13.0)], false);
            b.stroke.move_to(20.0, 9.0);
            b.stroke.line_to(9.5, 9.0);
            b.stroke.cubic_to(5.5, 9.0, 3.5, 11.5, 3.5, 14.0);
            b.stroke.cubic_to(3.5, 16.5, 5.5, 19.0, 9.5, 19.0);
            b.stroke.line_to(14.0, 19.0);
        }
    }
    b
}

/// Draws the Veda logo filling the square `r`: OS1's white ring from
/// Spike Jonze's "Her", a quarter of its radius wide (the agent's ring).
pub fn draw_logo(c: &mut Canvas, r: Rect) {
    let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
    let outer = r.w.min(r.h) as f32 / 2.0;
    // A quarter of the middle radius wide: 2/9 of the outer one.
    let inner = outer * 7.0 / 9.0;
    let mut ring = Path::new();
    ring.circle(cx, cy, outer);
    ring.circle(cx, cy, inner);
    c.fill_path(&ring, Color::WHITE, FillRule::EvenOdd);
}

impl Icon {
    /// Draws the icon centred in `r` at `size` pixels with `color`.
    pub fn draw(self, c: &mut Canvas, r: Rect, size: f32, color: Color) {
        let b = build(self);
        let s = size / 24.0;
        let ox = r.x as f32 + (r.w as f32 - size) / 2.0;
        let oy = r.y as f32 + (r.h as f32 - size) / 2.0;
        let t = Transform::scale(s, s).then_translate(ox, oy);
        let width = (1.75 * s).max(1.1);
        let style = StrokeStyle::new(width / s).with_cap(LineCap::Round).with_join(LineJoin::Round);
        if !b.stroke.is_empty() {
            c.stroke_path_transformed(&b.stroke, &style, &t, color);
        }
        if !b.fill.is_empty() {
            c.fill_path_transformed(&b.fill, &t, color, FillRule::NonZero);
        }
    }

    /// Every icon (for galleries and tests).
    pub const ALL: [Icon; 96] = [
        Icon::Folder,
        Icon::File,
        Icon::Document,
        Icon::Image,
        Icon::Music,
        Icon::Play,
        Icon::Pause,
        Icon::Stop,
        Icon::Next,
        Icon::Previous,
        Icon::Shuffle,
        Icon::Repeat,
        Icon::Volume,
        Icon::Mute,
        Icon::Search,
        Icon::Settings,
        Icon::Power,
        Icon::Restart,
        Icon::Close,
        Icon::Plus,
        Icon::Minus,
        Icon::Check,
        Icon::ChevronLeft,
        Icon::ChevronRight,
        Icon::ChevronUp,
        Icon::ChevronDown,
        Icon::Home,
        Icon::Terminal,
        Icon::Grid,
        Icon::Edit,
        Icon::Save,
        Icon::Open,
        Icon::Trash,
        Icon::TrashFull,
        Icon::Info,
        Icon::Warning,
        Icon::Star,
        Icon::Heart,
        Icon::Gamepad,
        Icon::Monitor,
        Icon::Cpu,
        Icon::Clock,
        Icon::Calendar,
        Icon::Refresh,
        Icon::ZoomIn,
        Icon::ZoomOut,
        Icon::Rotate,
        Icon::RotateRight,
        Icon::Fullscreen,
        Icon::FullscreenExit,
        Icon::Video,
        Icon::List,
        Icon::User,
        Icon::Sun,
        Icon::Moon,
        Icon::Palette,
        Icon::Chart,
        Icon::Car,
        Icon::Siren,
        Icon::Rocket,
        Icon::Chess,
        Icon::Cube,
        Icon::Lock,
        Icon::Up,
        Icon::Download,
        Icon::Copy,
        Icon::Cut,
        Icon::Paste,
        Icon::Undo,
        Icon::Redo,
        Icon::Wallpaper,
        Icon::Speaker,
        Icon::Wifi,
        Icon::WifiFair,
        Icon::WifiWeak,
        Icon::WifiNone,
        Icon::WifiOff,
        Icon::Network,
        Icon::Mic,
        Icon::MicOff,
        Icon::Agent,
        Icon::Calculator,
        Icon::Brush,
        Icon::Eraser,
        Icon::Fill,
        Icon::Eyedropper,
        Icon::Crop,
        Icon::Text,
        Icon::Select,
        Icon::Spray,
        Icon::Shapes,
        Icon::Swap,
        Icon::Backspace,
        Icon::History,
        Icon::FlipHorizontal,
        Icon::FlipVertical,
    ];

    /// Looks an icon up by its manifest name (`apps/*.app` `icon=` field).
    pub fn by_name(name: &str) -> Option<Icon> {
        Some(match name {
            "folder" | "files" => Icon::Folder,
            "wifi" | "wireless" => Icon::Wifi,
            "network" | "ethernet" => Icon::Network,
            "agent" => Icon::Agent,
            "mic" | "microphone" => Icon::Mic,
            "file" => Icon::File,
            "trash" => Icon::Trash,
            "document" | "editor" | "text" => Icon::Document,
            "image" | "photos" => Icon::Image,
            "music" => Icon::Music,
            "video" | "videos" | "film" | "movie" => Icon::Video,
            "terminal" => Icon::Terminal,
            "settings" => Icon::Settings,
            "monitor" | "sysmon" | "taskmgr" => Icon::Chart,
            "gamepad" | "game" => Icon::Gamepad,
            "car" => Icon::Car,
            "siren" | "police" | "interceptor" => Icon::Siren,
            "rocket" => Icon::Rocket,
            "chess" => Icon::Chess,
            "cube" => Icon::Cube,
            "info" | "about" => Icon::Info,
            "clock" => Icon::Clock,
            "calendar" => Icon::Calendar,
            "palette" => Icon::Palette,
            "warning" => Icon::Warning,
            "calculator" | "calc" => Icon::Calculator,
            "brush" | "paint" => Icon::Brush,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use gfx::Bitmap;

    #[test]
    fn arcs_are_not_joined_to_what_comes_before() {
        // The power symbol at 96 pixels, 4 to a unit of its grid: its stem
        // and its arc are drawn, and nothing between the stem's foot and
        // where the arc starts.
        let mut b = Bitmap::new(96, 96);
        Icon::Power.draw(&mut Canvas::for_bitmap(&mut b), Rect::new(0, 0, 96, 96), 96.0, Color::WHITE);
        let alpha = |x: i32, y: i32| b.get(x, y) >> 24;
        assert_eq!(alpha(48, 24), 255, "the stem");
        assert_eq!(alpha(18, 52), 255, "the arc's left");
        assert_eq!(alpha(56, 35), 0, "between them");
    }

    #[test]
    fn every_icon_draws_on_its_grid() {
        // At 48 pixels in the middle of 64: something is drawn, nothing beyond the grid's square
        // but the half stroke and anti-aliasing at its edge.
        for icon in Icon::ALL {
            let mut b = Bitmap::new(64, 64);
            icon.draw(&mut Canvas::for_bitmap(&mut b), Rect::new(8, 8, 48, 48), 48.0, Color::WHITE);
            let drawn: std::vec::Vec<(i32, i32)> =
                (0..64).flat_map(|y| (0..64).map(move |x| (x, y))).filter(|&(x, y)| b.get(x, y) != 0).collect();
            assert!(drawn.len() > 40, "{icon:?} draws {} pixels", drawn.len());
            assert!(
                drawn.iter().all(|&(x, y)| (6..58).contains(&x) && (6..58).contains(&y)),
                "{icon:?} leaves its grid"
            );
        }
        assert_eq!(Icon::by_name("calculator"), Some(Icon::Calculator));
        assert_eq!(Icon::by_name("paint"), Some(Icon::Brush));
    }

    /// Every icon at 16, 24 and 48 pixels, white on the theme's background, for a person to look
    /// at: `cargo test -p ui icon_gallery -- --ignored` writes `target/visual/ui-icons.png`
    /// (from the workspace's root).
    #[test]
    #[ignore]
    fn icon_gallery() {
        const COLUMNS: i32 = 8;
        const CELL: (i32, i32) = (128, 64);
        let rows = (Icon::ALL.len() as i32 + COLUMNS - 1) / COLUMNS;
        let mut sheet = Bitmap::filled(COLUMNS * CELL.0, rows * CELL.1, crate::Theme::dark().bg.premul());
        let mut c = Canvas::for_bitmap(&mut sheet);
        for (i, icon) in Icon::ALL.iter().enumerate() {
            let (x, y) = ((i as i32 % COLUMNS) * CELL.0, (i as i32 / COLUMNS) * CELL.1);
            icon.draw(&mut c, Rect::new(x + 8, y + 24, 16, 16), 16.0, Color::WHITE);
            icon.draw(&mut c, Rect::new(x + 32, y + 20, 24, 24), 24.0, Color::WHITE);
            icon.draw(&mut c, Rect::new(x + 68, y + 8, 48, 48), 48.0, Color::WHITE);
        }
        drop(c);
        let image = image::Image::from_pixels(sheet.width as u32, sheet.height as u32, sheet.pixels).unwrap();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/visual");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ui-icons.png"), image::encode_png(&image).unwrap()).unwrap();
    }
}

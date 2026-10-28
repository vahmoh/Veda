// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Paint: draw and edit pictures — a pencil, a brush, an airbrush and an eraser, fills, shapes
//! and text, selections to move, stretch, turn and crop, resizing and effects, with unlimited
//! undo (within memory) and PNG, JPEG, BMP and QOI files.
//!
//! What it does to pictures is the host-tested `paint-core` crate; this is the window around
//! it: the menus, the toolbar and the tool's options ([`toolbar`]), the canvas ([`canvas`]),
//! the dialogs ([`dialogs`], [`colors`]), files opened and saved on a thread of their own
//! ([`files`]), and [`agent`], which lets the voice agent draw.

mod agent;
mod canvas;
mod colors;
mod dialogs;
mod files;
mod toolbar;

use std::path::Path;

use abi::RawHandle;
use gfx::{Align, Bitmap, Color, Rect};
use paint_core::Document;
use paint_core::codec::{self, format_of};
use paint_core::effects::Effect;
use paint_core::settings::{Settings, Tool};
use paint_core::view::MIN_ZOOM;
use proto::display::modifiers;
use proto::input::keys;
use ui::{App, FileDialog, FileDialogResult, Font, Icon, Menu, MenuItem, Ui, WindowSpec};

use canvas::Canvas;
use colors::{ColorDialog, Target};
use dialogs::{Adjust, AdjustKind, Background, CanvasSize, NewPicture, ResizePicture};
use files::{Done, Files};

const PICTURES: &str = "/home/user/Pictures";
const SETTINGS_FILE: &str = "/home/user/.config/paint/settings";
/// The largest settings file read (bytes).
const SETTINGS_MAX: usize = 64 * 1024;
/// How long after a change of the settings they are written (ns).
const SAVE_SETTINGS_NS: u64 = 2_000_000_000;
const TOAST_NS: u64 = 2_400_000_000;
/// A new picture's size.
const NEW_SIZE: (i32, i32) = (960, 540);
/// The extensions the Open dialog shows.
const OPENS: [&str; 7] = ["png", "jpg", "jpeg", "jfif", "bmp", "dib", "qoi"];

/// What to do once a question about unsaved changes is answered (and the picture saved, if
/// that was the answer).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Then {
    Nothing,
    New,
    Open,
    Quit,
}

/// A dialog over the window.
enum Modal {
    New(NewPicture),
    Open(FileDialog),
    SaveAs { dialog: FileDialog, then: Then },
    Unsaved(Then),
    Resize(ResizePicture),
    CanvasSize(CanvasSize),
    Adjust(Adjust),
    Color(ColorDialog),
    Message { title: String, text: String },
    Shortcuts,
    About,
}

const SHORTCUTS: [(&str, &str); 20] = [
    ("S P B A E", "Select, pencil, brush, airbrush, eraser"),
    ("F K T O", "Fill, colour picker, text, shapes"),
    ("[ and ]", "Smaller and larger tip"),
    ("X / D", "Swap the colours / black and white"),
    ("Right button", "Paint with the secondary colour"),
    ("Shift and a click", "A line from where the last stroke ended"),
    ("Shift while drawing", "Squares, circles, straight lines"),
    ("Ctrl+Z / Ctrl+Y", "Undo / redo"),
    ("Ctrl+A / Esc", "Select all / place and deselect"),
    ("Ctrl+C / X / V", "Copy, cut, paste the selection"),
    ("Ctrl and a drag", "Move a copy of the selection"),
    ("Delete", "Clear the selection"),
    ("Arrows", "Nudge the selection (Shift: 10 pixels)"),
    ("Ctrl+Shift+X", "Crop to the selection"),
    ("Ctrl+R / Ctrl+Shift+R", "Resize / canvas size"),
    ("Ctrl+Shift+I", "Invert colours"),
    ("Ctrl+= / Ctrl+-", "Zoom in / out (or Ctrl and the wheel)"),
    ("Ctrl+0 / Ctrl+1", "Fit the window / actual size"),
    ("Ctrl+G", "Pixel grid"),
    ("Ctrl+N / O / S", "New, open, save (Ctrl+Shift+S: save as)"),
];

/// What the memory allows: the history's budget (bytes) and the largest picture opened
/// (pixels), from the machine's memory.
fn limits() -> (usize, u64) {
    let total = runtime::object::system_info().map(|i| i.total_memory).unwrap_or(1 << 30);
    let budget = (total / 8).clamp(32 << 20, 512 << 20) as usize;
    let pixels = (total / 24).clamp(4 << 20, 64 << 20);
    (budget, pixels)
}

struct PaintApp {
    doc: Document,
    settings: Settings,
    canvas: Canvas,
    files: Option<Files>,
    /// Pictures the voice agent opens and saves while it waits (the window's own go through
    /// [`files`]).
    fs: ::files::Fs,
    modal: Option<Modal>,
    /// The picture copied (the window system's clipboard holds text only).
    clipboard: Option<Bitmap>,
    toast: Option<(String, u64)>,
    /// What the file worker is doing, for the status bar ("Saving Cat.png").
    busy: Option<String>,
    /// What follows the save under way.
    after_save: Then,
    quitting: bool,
    budget: usize,
    max_pixels: u64,
    /// The settings as last written, and when to write them next.
    settings_written: String,
    settings_due: Option<u64>,
    /// The picture's file holds what the voice agent saved (it may replace it again).
    pub agent_file: bool,
}

impl PaintApp {
    fn new(path: Option<&str>) -> PaintApp {
        let (budget, max_pixels) = limits();
        let settings_written = apps_common::store::read(Path::new(SETTINGS_FILE), SETTINGS_MAX)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let settings = Settings::from_text(&settings_written);
        let mut app = PaintApp {
            doc: Document::new(NEW_SIZE.0, NEW_SIZE.1, Color::WHITE, budget),
            settings,
            canvas: Canvas::new(),
            files: Files::start(),
            fs: ::files::Fs::connect(),
            modal: None,
            clipboard: None,
            toast: None,
            busy: None,
            after_save: Then::Nothing,
            quitting: false,
            budget,
            max_pixels,
            settings_written,
            settings_due: None,
            agent_file: false,
        };
        if let Some(p) = path {
            app.open_path(p);
        }
        app
    }

    fn show_toast(&mut self, text: &str, now: u64) {
        self.toast = Some((text.into(), now + TOAST_NS));
    }

    fn message(&mut self, title: &str, text: String) {
        self.modal = Some(Modal::Message { title: title.into(), text });
    }

    /// The settings changed: written a little later.
    fn settings_changed(&mut self, now: u64) {
        self.settings_due = Some(now + SAVE_SETTINGS_NS);
    }

    fn save_settings(&mut self) {
        self.settings_due = None;
        let text = self.settings.to_text();
        if text == self.settings_written {
            return;
        }
        match apps_common::store::write(Path::new(SETTINGS_FILE), text.as_bytes()) {
            Ok(()) => self.settings_written = text,
            Err(e) => eprintln!("could not save the settings: {e}"),
        }
    }

    /// The picture's name, as the title and dialogs say it.
    fn title(&self) -> String {
        self.doc.path.as_deref().map_or_else(|| String::from("Untitled"), |p| String::from(::files::path::file_name(p)))
    }

    fn dir(&self) -> String {
        self.doc.path.as_deref().map_or_else(|| String::from(PICTURES), |p| String::from(::files::path::parent(p)))
    }

    // ---- files ---------------------------------------------------------------------

    /// Asks about unsaved changes before `then`, or does it.
    fn guard(&mut self, then: Then) {
        if self.doc.is_modified() {
            self.modal = Some(Modal::Unsaved(then));
        } else {
            self.after(then);
        }
    }

    fn after(&mut self, then: Then) {
        match then {
            Then::Nothing => {}
            Then::New => self.modal = Some(Modal::New(NewPicture::new(NEW_SIZE.0, NEW_SIZE.1))),
            Then::Open => {
                let dialog = FileDialog::open("Open picture", &self.dir()).with_filter(&OPENS);
                self.modal = Some(Modal::Open(dialog));
            }
            Then::Quit => self.quitting = true,
        }
    }

    /// Opens `path` (on the worker).
    fn open_path(&mut self, path: &str) {
        match &self.files {
            Some(f) => {
                f.open(path, self.max_pixels);
                self.busy = Some(format!("Opening {}", ::files::path::file_name(path)));
            }
            None => self.message("Couldn't open the picture", "Paint could not start its file worker.".into()),
        }
    }

    /// A new picture.
    fn new_picture(&mut self, w: i32, h: i32, background: Color) {
        self.doc = Document::new(w, h, background, self.budget);
        self.canvas.fit_next = true;
        self.agent_file = false;
        println!("new picture {w}x{h}");
    }

    /// Saves to the picture's file, or asks where; then `then`.
    fn save(&mut self, then: Then) {
        match self.doc.path.clone() {
            Some(path) if !path.starts_with("/system/") && format_of(&path).is_some() => self.save_to(&path, then),
            _ => self.save_as(then),
        }
    }

    fn save_as(&mut self, then: Then) {
        let name = codec::file_name(self.doc.path.as_deref(), self.doc.format);
        let dir = if self.dir().starts_with("/system") { String::from(PICTURES) } else { self.dir() };
        let dialog = FileDialog::save("Save picture", &dir, &name);
        self.modal = Some(Modal::SaveAs { dialog, then });
    }

    /// Saves to `path` (in the format its extension says; PNG if it says none it knows).
    fn save_to(&mut self, path: &str, then: Then) {
        let (path, format) = match format_of(path) {
            Some(f) => (String::from(path), f),
            None => (format!("{path}.png"), image::Format::Png),
        };
        let Some(files) = &self.files else {
            return self.message("Couldn't save the picture", "Paint could not start its file worker.".into());
        };
        let (picture, version) = self.doc.snapshot();
        files.save(&path, picture, format, version);
        self.busy = Some(format!("Saving {}", ::files::path::file_name(&path)));
        self.after_save = then;
    }

    /// What the file worker finished.
    fn files_done(&mut self, now: u64) {
        let Some(files) = &self.files else { return };
        let (finished, busy) = (files.take(), files.busy());
        for done in finished {
            match done {
                Done::Opened { path, result } => match result {
                    Ok(o) => {
                        self.doc = Document::from_picture(o.picture, Some(path.clone()), o.format, self.budget);
                        self.canvas.fit_next = true;
                        self.agent_file = false;
                        if o.damaged {
                            self.show_toast("The file is damaged: what could be read is shown", now);
                        }
                    }
                    Err(e) => self
                        .message("Couldn't open the picture", format!("{}: {e}.", ::files::path::display_path(&path))),
                },
                Done::Saved { path, format, version, result } => match result {
                    Ok(_) => {
                        self.doc.saved_version(version, &path, format);
                        self.show_toast(&format!("Saved {}", ::files::path::file_name(&path)), now);
                        let then = core::mem::replace(&mut self.after_save, Then::Nothing);
                        self.after(then);
                    }
                    Err(e) => {
                        self.after_save = Then::Nothing;
                        self.message(
                            "Couldn't save the picture",
                            format!("{}: {e}.", ::files::path::display_path(&path)),
                        );
                    }
                },
            }
        }
        if !busy {
            self.busy = None;
        }
    }

    // ---- editing -------------------------------------------------------------------

    fn undo(&mut self, now: u64) {
        if let Some(name) = self.doc.undo() {
            self.show_toast(&format!("Undid {}", name.to_lowercase()), now);
        }
    }

    fn redo(&mut self, now: u64) {
        if let Some(name) = self.doc.redo() {
            self.show_toast(&format!("Redid {}", name.to_lowercase()), now);
        }
    }

    fn copy(&mut self, now: u64) -> bool {
        match self.doc.copy() {
            Some(piece) => {
                let msg = format!("Copied {} × {} pixels", piece.width, piece.height);
                self.clipboard = Some(piece);
                self.show_toast(&msg, now);
                true
            }
            None => false,
        }
    }

    fn cut(&mut self, now: u64) {
        if self.copy(now) {
            self.doc.delete_selection();
        }
    }

    /// Pastes the picture copied at the top left of what the window shows.
    fn paste(&mut self) {
        let Some(piece) = self.clipboard.clone() else { return };
        let (x, y) = self.canvas.view.to_picture(
            self.canvas.viewport,
            self.canvas.viewport.x as f32,
            self.canvas.viewport.y as f32,
        );
        let at = (libm::floorf(x.max(0.0)) as i32, libm::floorf(y.max(0.0)) as i32);
        let at =
            (at.0.min((self.doc.width() - piece.width).max(0)), at.1.min((self.doc.height() - piece.height).max(0)));
        self.settings.tool = Tool::Select;
        self.doc.paste(piece, at);
    }

    /// Moves the selection by (`dx`, `dy`) pixels (lifting it).
    fn nudge(&mut self, dx: i32, dy: i32) {
        if self.doc.lift()
            && let Some(r) = self.doc.selection()
        {
            self.doc.move_float(r.translate(dx, dy));
        }
    }

    fn tool_changed(&mut self, now: u64) {
        // A shape or text under way is placed; the selection stays for the selection tool.
        if self.doc.shape().is_some() || self.doc.text().is_some() {
            self.doc.commit();
        }
        if self.settings.tool != Tool::Select && self.doc.floating().is_some() {
            self.doc.commit();
        }
        self.settings_changed(now);
    }

    fn set_tool(&mut self, tool: Tool, now: u64) {
        if self.settings.tool != tool {
            self.settings.tool = tool;
            self.tool_changed(now);
        }
    }

    // ---- menus ---------------------------------------------------------------------

    fn menus(&self) -> [Menu; 6] {
        let h = self.doc.history();
        let undo = match h.undo_name() {
            Some(n) => format!("Undo {}", n.to_lowercase()),
            None => String::from("Undo"),
        };
        let redo = match h.redo_name() {
            Some(n) => format!("Redo {}", n.to_lowercase()),
            None => String::from("Redo"),
        };
        let sel = self.doc.selection().is_some();
        let live = self.doc.shape().is_some() || self.doc.text().is_some() || self.doc.floating().is_some();
        [
            Menu::new(
                "File",
                vec![
                    MenuItem::new("New...").shortcut("Ctrl+N"),
                    MenuItem::new("Open...").shortcut("Ctrl+O"),
                    MenuItem::separator(),
                    MenuItem::new("Save").shortcut("Ctrl+S"),
                    MenuItem::new("Save As...").shortcut("Ctrl+Shift+S"),
                    MenuItem::separator(),
                    MenuItem::new("Exit"),
                ],
            ),
            Menu::new(
                "Edit",
                vec![
                    MenuItem::new(&undo).shortcut("Ctrl+Z").enabled(h.can_undo() || live),
                    MenuItem::new(&redo).shortcut("Ctrl+Y").enabled(h.can_redo() && !live),
                    MenuItem::separator(),
                    MenuItem::new("Cut").shortcut("Ctrl+X").enabled(sel),
                    MenuItem::new("Copy").shortcut("Ctrl+C").enabled(sel),
                    MenuItem::new("Paste").shortcut("Ctrl+V").enabled(self.clipboard.is_some()),
                    MenuItem::new("Delete").shortcut("Del").enabled(sel),
                    MenuItem::separator(),
                    MenuItem::new("Select All").shortcut("Ctrl+A"),
                    MenuItem::new("Deselect").shortcut("Esc").enabled(sel),
                    MenuItem::new("Crop to Selection").shortcut("Ctrl+Shift+X").enabled(sel),
                ],
            ),
            Menu::new(
                "View",
                vec![
                    MenuItem::new("Zoom In").shortcut("Ctrl+="),
                    MenuItem::new("Zoom Out").shortcut("Ctrl+-"),
                    MenuItem::new("Fit to Window").shortcut("Ctrl+0"),
                    MenuItem::new("Actual Size").shortcut("Ctrl+1"),
                    MenuItem::separator(),
                    MenuItem::new("Pixel Grid").shortcut("Ctrl+G").checked(self.settings.grid),
                ],
            ),
            Menu::new(
                "Image",
                vec![
                    MenuItem::new("Resize...").shortcut("Ctrl+R"),
                    MenuItem::new("Canvas Size...").shortcut("Ctrl+Shift+R"),
                    MenuItem::separator(),
                    MenuItem::new("Rotate Right"),
                    MenuItem::new("Rotate Left"),
                    MenuItem::new("Rotate 180°"),
                    MenuItem::new("Flip Horizontal"),
                    MenuItem::new("Flip Vertical"),
                    MenuItem::separator(),
                    MenuItem::new("Clear to Secondary Colour"),
                ],
            ),
            Menu::new(
                "Effects",
                vec![
                    MenuItem::new("Invert Colours").shortcut("Ctrl+Shift+I"),
                    MenuItem::new("Black and White"),
                    MenuItem::new("Sepia"),
                    MenuItem::separator(),
                    MenuItem::new("Blur..."),
                    MenuItem::new("Sharpen..."),
                    MenuItem::new("Brightness and Contrast..."),
                ],
            ),
            Menu::new("Help", vec![MenuItem::new("Keyboard Shortcuts").shortcut("F1"), MenuItem::new("About Paint")]),
        ]
    }

    fn menu_action(&mut self, ui: &mut Ui, menu: usize, item: usize) {
        let now = ui.now();
        match (menu, item) {
            (0, 0) => self.guard(Then::New),
            (0, 1) => self.guard(Then::Open),
            (0, 3) => self.save(Then::Nothing),
            (0, 4) => self.save_as(Then::Nothing),
            (0, 6) => self.guard(Then::Quit),
            (1, 0) => self.undo(now),
            (1, 1) => self.redo(now),
            (1, 3) => self.cut(now),
            (1, 4) => {
                self.copy(now);
            }
            (1, 5) => self.paste(),
            (1, 6) => {
                self.doc.delete_selection();
            }
            (1, 8) => {
                self.set_tool(Tool::Select, now);
                self.doc.select_all();
            }
            (1, 9) => self.doc.deselect(),
            (1, 10) => {
                self.doc.crop_to_selection();
                self.canvas.fit_next = true;
            }
            (2, 0) => self.zoom(true),
            (2, 1) => self.zoom(false),
            (2, 2) => self.canvas.fit(&self.doc),
            (2, 3) => self.canvas.zoom_to(1.0, &self.doc),
            (2, 5) => {
                self.settings.grid = !self.settings.grid;
                self.settings_changed(now);
            }
            (3, 0) => self.modal = Some(Modal::Resize(ResizePicture::new(self.doc.width(), self.doc.height()))),
            (3, 1) => self.modal = Some(Modal::CanvasSize(CanvasSize::new(self.doc.width(), self.doc.height()))),
            (3, 3) => self.doc.rotate(1),
            (3, 4) => self.doc.rotate(-1),
            (3, 5) => self.doc.rotate(2),
            (3, 6) => self.doc.flip(true),
            (3, 7) => self.doc.flip(false),
            (3, 9) => self.doc.clear(self.settings.secondary),
            (4, 0) => self.doc.effect(Effect::Invert),
            (4, 1) => self.doc.effect(Effect::Grayscale),
            (4, 2) => self.doc.effect(Effect::Sepia),
            (4, 4) => self.modal = Some(Modal::Adjust(Adjust::new(AdjustKind::Blur))),
            (4, 5) => self.modal = Some(Modal::Adjust(Adjust::new(AdjustKind::Sharpen))),
            (4, 6) => self.modal = Some(Modal::Adjust(Adjust::new(AdjustKind::BrightnessContrast))),
            (5, 0) => self.modal = Some(Modal::Shortcuts),
            (5, 1) => self.modal = Some(Modal::About),
            _ => {}
        }
    }

    fn zoom(&mut self, inwards: bool) {
        let z = self.canvas.view.step(inwards);
        self.canvas.zoom_to(z, &self.doc);
    }

    // ---- keyboard ------------------------------------------------------------------

    /// Typing into the text not yet placed. Returns whether the key was the text's.
    fn text_key(&mut self, ui: &mut Ui, k: &ui::ui::KeyPress) -> bool {
        let Some(t) = self.doc.text() else { return false };
        let ctrl = k.modifiers & modifiers::CTRL != 0;
        let mut t = t.clone();
        match k.code {
            keys::ENTER | keys::KPENTER if ctrl => {
                self.doc.commit();
                return true;
            }
            keys::ESC => {
                // Esc places the text, as a click elsewhere does: Undo takes it away.
                self.doc.commit();
                return true;
            }
            keys::ENTER | keys::KPENTER => t.insert("\n"),
            keys::BACKSPACE => t.backspace(),
            keys::DELETE => t.delete(),
            keys::LEFT => t.left(),
            keys::RIGHT => t.right(),
            keys::HOME => t.home(),
            keys::END => t.end(),
            keys::V if ctrl => {
                let pasted = ui.clipboard();
                t.insert(&pasted);
            }
            _ if ctrl => return false,
            _ => match k.ch {
                Some(c) if c != '\t' => {
                    let mut buf = [0u8; 4];
                    t.insert(c.encode_utf8(&mut buf));
                }
                _ => return false,
            },
        }
        ui.ctx.text.with(|ctx| self.doc.set_text(t, ctx));
        true
    }

    fn keyboard(&mut self, ui: &mut Ui) {
        let now = ui.now();
        for k in ui.input.keys.clone() {
            if self.modal.is_some() {
                break;
            }
            if self.text_key(ui, &k) {
                continue;
            }
            let ctrl = k.modifiers & modifiers::CTRL != 0;
            let shift = k.modifiers & modifiers::SHIFT != 0;
            let alt = k.modifiers & modifiers::ALT != 0;
            if ctrl {
                match k.code {
                    keys::N => self.guard(Then::New),
                    keys::O => self.guard(Then::Open),
                    keys::S if shift => self.save_as(Then::Nothing),
                    keys::S => self.save(Then::Nothing),
                    keys::Z if shift => self.redo(now),
                    keys::Z => self.undo(now),
                    keys::Y => self.redo(now),
                    keys::A => {
                        self.set_tool(Tool::Select, now);
                        self.doc.select_all();
                    }
                    keys::D => self.doc.deselect(),
                    keys::C => {
                        self.copy(now);
                    }
                    keys::X if shift => {
                        self.doc.crop_to_selection();
                        self.canvas.fit_next = true;
                    }
                    keys::X => self.cut(now),
                    keys::V => self.paste(),
                    keys::R if shift => {
                        self.modal = Some(Modal::CanvasSize(CanvasSize::new(self.doc.width(), self.doc.height())))
                    }
                    keys::R => {
                        self.modal = Some(Modal::Resize(ResizePicture::new(self.doc.width(), self.doc.height())))
                    }
                    keys::I if shift => self.doc.effect(Effect::Invert),
                    keys::EQUAL | keys::KPPLUS => self.zoom(true),
                    keys::MINUS | keys::KPMINUS => self.zoom(false),
                    keys::KEY_0 | keys::KP0 => self.canvas.fit(&self.doc),
                    keys::KEY_1 | keys::KP1 => self.canvas.zoom_to(1.0, &self.doc),
                    keys::G => {
                        self.settings.grid = !self.settings.grid;
                        self.settings_changed(now);
                    }
                    _ => {}
                }
                continue;
            }
            if alt {
                continue;
            }
            let step = if shift { 10 } else { 1 };
            match k.code {
                keys::F1 => self.modal = Some(Modal::Shortcuts),
                // Esc places what is under way (Undo takes it back) and selects nothing.
                keys::ESC => self.doc.deselect(),
                keys::ENTER | keys::KPENTER => self.doc.commit(),
                keys::DELETE | keys::BACKSPACE => {
                    self.doc.delete_selection();
                }
                keys::LEFT if self.doc.selection().is_some() => self.nudge(-step, 0),
                keys::RIGHT if self.doc.selection().is_some() => self.nudge(step, 0),
                keys::UP if self.doc.selection().is_some() => self.nudge(0, -step),
                keys::DOWN if self.doc.selection().is_some() => self.nudge(0, step),
                _ => match k.ch.map(|c| c.to_ascii_uppercase()) {
                    Some('X') => {
                        core::mem::swap(&mut self.settings.primary, &mut self.settings.secondary);
                        canvas::Canvas::restyle(ui, &mut self.doc, &self.settings);
                        self.settings_changed(now);
                    }
                    Some('D') => {
                        (self.settings.primary, self.settings.secondary) = (Color::BLACK, Color::WHITE);
                        canvas::Canvas::restyle(ui, &mut self.doc, &self.settings);
                        self.settings_changed(now);
                    }
                    Some('[') | Some(']') => {
                        self.settings.step_size(k.ch == Some(']'));
                        canvas::Canvas::restyle(ui, &mut self.doc, &self.settings);
                        self.settings_changed(now);
                    }
                    Some(c) => {
                        if let Some(tool) = Tool::ALL.into_iter().find(|t| t.key() == c) {
                            self.set_tool(tool, now);
                        }
                    }
                    None => {}
                },
            }
        }
    }

    // ---- the window's parts --------------------------------------------------------

    fn status_bar(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme().clone();
        ui.canvas.fill_rect(r, t.surface);
        ui.canvas.fill_rect(Rect::new(r.x, r.y, r.w, 1), t.border);
        let size = t.small_size + 0.5;
        let mut x = r.x + 14;
        let mut item = |ui: &mut Ui, icon: Icon, text: &str, w: i32| {
            ui.icon(Rect::new(x, r.y, 16, r.h), icon, 14.0, t.text_faint);
            ui.label(Rect::new(x + 22, r.y, w, r.h), text, Font::Regular, size, t.text_dim, Align::Left);
            x += w + 30;
        };
        let pointer = self.canvas.pointer.filter(|&(px, py)| {
            px >= 0.0 && py >= 0.0 && (px as i32) < self.doc.width() && (py as i32) < self.doc.height()
        });
        let at = pointer.map_or(String::new(), |(px, py)| format!("{}, {} px", px as i32, py as i32));
        item(ui, Icon::Crop, &at, 110);
        let sel = self.doc.selection().map_or(String::new(), |s| format!("{} × {} px", s.w, s.h));
        item(ui, Icon::Select, &sel, 110);
        let dims = format!("{} × {} px", self.doc.width(), self.doc.height());
        item(ui, Icon::Image, &dims, 120);
        if let Some(busy) = self.busy.clone() {
            ui.spinner(x + 8, r.y + r.h / 2, 7.0);
            ui.label(
                Rect::new(x + 22, r.y, 260, r.h),
                &format!("{busy}…"),
                Font::Regular,
                size,
                t.text_dim,
                Align::Left,
            );
        }
        // The zoom: out, the level, in, fit.
        let mut zx = r.right() - 10;
        let fit = Rect::new(zx - 34, r.y + 2, 30, r.h - 4);
        if ui.icon_button(fit, Icon::Fullscreen, "Fit to window (Ctrl+0)") {
            self.canvas.fit(&self.doc);
        }
        zx = fit.x - 4;
        let zin = Rect::new(zx - 28, r.y + 2, 26, r.h - 4);
        if ui.icon_button(zin, Icon::ZoomIn, "Zoom in (Ctrl+=)") {
            self.zoom(true);
        }
        let pct = format!("{}%", libm::roundf(self.canvas.view.zoom * 100.0) as i32);
        let label = Rect::new(zin.x - 56, r.y, 52, r.h);
        ui.label(label, &pct, Font::Bold, size, t.text, Align::Center);
        let zout = Rect::new(label.x - 28, r.y + 2, 26, r.h - 4);
        if ui.icon_button(zout, Icon::ZoomOut, "Zoom out (Ctrl+-)") && self.canvas.view.zoom > MIN_ZOOM {
            self.zoom(false);
        }
    }

    fn draw_toast(&mut self, ui: &mut Ui, area: Rect) {
        let Some((text, until)) = self.toast.clone() else { return };
        if ui.now() >= until {
            self.toast = None;
            return;
        }
        let t = ui.theme().clone();
        let w = ui.measure(&text, Font::Regular, 13.0) as i32 + 48;
        let r = Rect::new(area.right() - w - 18, area.bottom() - 52, w, 36);
        ui.canvas.draw_shadow(r.translate(0, 4), 10, 12, Color::rgba(0, 0, 0, 110));
        ui.canvas.fill_rounded_rect(r, 10.0, Color::hex(0x2E2E36));
        ui.canvas.stroke_rounded_rect(r, 10.0, 1.0, t.border_strong);
        ui.icon(Rect::new(r.x + 10, r.y, 18, r.h), Icon::Check, 14.0, t.success);
        ui.label(Rect::new(r.x + 34, r.y, w - 40, r.h), &text, Font::Regular, 13.0, t.text, Align::Left);
        ui.repaint_at(until);
    }

    fn modal(&mut self, ui: &mut Ui) {
        let Some(mut modal) = self.modal.take() else { return };
        let now = ui.now();
        let keep = match &mut modal {
            Modal::New(d) => match d.show(ui) {
                Some(Some((w, h, bg))) => {
                    let color = match bg {
                        Background::White => Color::WHITE,
                        Background::Transparent => Color::TRANSPARENT,
                        Background::Secondary => self.settings.secondary,
                    };
                    self.new_picture(w, h, color);
                    false
                }
                Some(None) => false,
                None => true,
            },
            Modal::Open(dialog) => match dialog.show(ui) {
                Some(FileDialogResult::Chosen(path)) => {
                    self.open_path(&path);
                    false
                }
                Some(FileDialogResult::Cancelled) => false,
                None => true,
            },
            Modal::SaveAs { dialog, then } => match dialog.show(ui) {
                Some(FileDialogResult::Chosen(path)) => {
                    let then = core::mem::replace(then, Then::Nothing);
                    self.agent_file = false;
                    self.save_to(&path, then);
                    false
                }
                Some(FileDialogResult::Cancelled) => false,
                None => true,
            },
            Modal::Unsaved(then) => {
                let text = format!("Do you want to save the changes you made to {}?", self.title());
                match ui.message_box("Save changes?", &text, &["Save", "Don't Save", "Cancel"]) {
                    Some(0) => {
                        let then = core::mem::replace(then, Then::Nothing);
                        self.save(then);
                        false
                    }
                    Some(1) => {
                        let then = core::mem::replace(then, Then::Nothing);
                        self.after(then);
                        false
                    }
                    Some(_) => false,
                    None => true,
                }
            }
            Modal::Resize(d) => match d.show(ui) {
                Some(Some((w, h, how))) => {
                    self.doc.resize(w, h, how);
                    self.canvas.fit_next = true;
                    false
                }
                Some(None) => false,
                None => true,
            },
            Modal::CanvasSize(d) => match d.show(ui) {
                Some(Some((w, h, anchor, transparent))) => {
                    let fill = if transparent { Color::TRANSPARENT } else { self.settings.secondary };
                    self.doc.canvas_size(w, h, anchor, fill);
                    self.canvas.fit_next = true;
                    false
                }
                Some(None) => false,
                None => true,
            },
            Modal::Adjust(d) => d.show(ui, &mut self.doc).is_none(),
            Modal::Color(d) => match d.show(ui, &mut self.settings) {
                Some(Some(c)) => {
                    match d.target {
                        Target::Primary => self.settings.primary = c,
                        Target::Secondary => self.settings.secondary = c,
                    }
                    canvas::Canvas::restyle(ui, &mut self.doc, &self.settings);
                    self.settings_changed(now);
                    false
                }
                Some(None) => false,
                None => true,
            },
            Modal::Message { title, text } => ui.message_box(title, text, &["OK"]).is_none(),
            Modal::About => ui
                .message_box(
                    "Paint",
                    "Veda Paint: brushes, shapes, text, selections and effects, with unlimited undo, for PNG, JPEG, BMP and QOI pictures.",
                    &["OK"],
                )
                .is_none(),
            Modal::Shortcuts => {
                let t = ui.theme().clone();
                let mut done = false;
                let h = 110 + SHORTCUTS.len() as i32 * 22;
                ui.modal(600, h, |ui, card| {
                    done = ui.input.key(keys::ESC) || ui.input.key(keys::ENTER);
                    let inner = card.inset(24, 18, 24, 18);
                    ui.heading(Rect::new(inner.x, inner.y, inner.w, 26), "Keyboard Shortcuts");
                    for (i, (k, what)) in SHORTCUTS.iter().enumerate() {
                        let y = inner.y + 40 + i as i32 * 22;
                        ui.label(Rect::new(inner.x, y, 200, 22), k, Font::Mono, 12.5, t.accent_hover, Align::Left);
                        ui.label(Rect::new(inner.x + 210, y, inner.w - 210, 22), what, Font::Regular, 13.0, t.text, Align::Left);
                    }
                    if ui.primary_button(Rect::new(inner.right() - 96, inner.bottom() - 34, 96, 34), "Close") {
                        done = true;
                    }
                });
                !done
            }
        };
        if keep && self.modal.is_none() {
            self.modal = Some(modal);
        }
    }
}

impl App for PaintApp {
    fn update(&mut self, ui: &mut Ui) {
        let t = ui.theme().clone();
        let now = ui.now();
        let (w, h) = (ui.width, ui.height);
        self.files_done(now);
        if self.modal.is_none() {
            self.keyboard(ui);
        }

        let mb = Rect::new(0, 0, w, 32);
        ui.canvas.fill_rect(mb, t.surface);
        let menus = self.menus();
        let chosen = ui.menu_bar(mb, &menus);

        let bar = Rect::new(0, mb.bottom(), w, toolbar::TOOLBAR_H);
        let (can_undo, can_redo) =
            (self.doc.history().can_undo() || self.doc.shape().is_some(), self.doc.history().can_redo());
        let request = toolbar::toolbar(ui, bar, &mut self.settings, can_undo, can_redo);
        let opts = Rect::new(0, bar.bottom(), w, toolbar::OPTIONS_H);
        let option_request = toolbar::options(ui, opts, &mut self.settings, self.doc.selection().is_some());
        let status = Rect::new(0, h - 28, w, 28);
        let area = Rect::new(0, opts.bottom(), w, status.y - opts.bottom());
        if let Some(canvas::Event::Colors) = self.canvas.update(ui, area, &mut self.doc, &mut self.settings) {
            self.settings_changed(now);
        }
        self.status_bar(ui, status);
        self.draw_toast(ui, area);

        for request in [request, option_request].into_iter().flatten() {
            match request {
                toolbar::Request::Undo => self.undo(now),
                toolbar::Request::Redo => self.redo(now),
                toolbar::Request::EditPrimary => {
                    self.modal = Some(Modal::Color(ColorDialog::new(Target::Primary, self.settings.primary)))
                }
                toolbar::Request::EditSecondary => {
                    self.modal = Some(Modal::Color(ColorDialog::new(Target::Secondary, self.settings.secondary)))
                }
                toolbar::Request::ToolChanged => self.tool_changed(now),
                toolbar::Request::Restyle => {
                    canvas::Canvas::restyle(ui, &mut self.doc, &self.settings);
                    self.settings_changed(now);
                }
                toolbar::Request::Crop => {
                    self.doc.crop_to_selection();
                    self.canvas.fit_next = true;
                }
                toolbar::Request::RotateLeft => self.doc.rotate(-1),
                toolbar::Request::RotateRight => self.doc.rotate(1),
                toolbar::Request::FlipHorizontal => self.doc.flip(true),
                toolbar::Request::FlipVertical => self.doc.flip(false),
                toolbar::Request::SelectAll => self.doc.select_all(),
            }
        }
        if let Some((m, i)) = chosen {
            self.menu_action(ui, m, i);
        }
        self.modal(ui);

        let dot = if self.doc.is_modified() { "\u{2022} " } else { "" };
        ui.set_title(&format!("{dot}{} \u{2014} Paint", self.title()));
        if let Some(due) = self.settings_due {
            if now >= due {
                self.save_settings();
            } else {
                ui.repaint_at(due);
            }
        }
        if self.busy.is_some() {
            ui.repaint_at(now + 100_000_000);
        }
        if self.quitting && self.busy.is_none() {
            ui.close_window();
        }
    }

    fn wait_handles(&self) -> Vec<(RawHandle, u32)> {
        self.files.as_ref().map(|f| vec![(f.event_handle(), abi::signals::SIGNALED)]).unwrap_or_default()
    }

    fn close_requested(&mut self) -> bool {
        if self.quitting {
            self.save_settings();
            return self.busy.is_none();
        }
        if self.doc.is_modified() {
            if self.modal.is_none() {
                self.modal = Some(Modal::Unsaved(Then::Quit));
            }
            return false;
        }
        self.save_settings();
        true
    }

    fn agent_info(&self) -> Option<ui::agent::AppAgentInfo> {
        Some(agent::info())
    }

    fn agent_state(&self) -> ui::agent::Value {
        agent::state(self)
    }

    fn agent_invoke(&mut self, action: &str, args: &ui::agent::Value) -> Result<ui::agent::Value, String> {
        agent::invoke(self, action, args)
    }
}

/// The window's size on a screen of `w` by `h`: room for the toolbar, as much of the screen as
/// suits, and clear of the taskbar.
fn window_size(w: u32, h: u32) -> (u32, u32) {
    let width = (w * 7 / 10).max(1180).min(w.saturating_sub(100)).max(640);
    let height = (h * 3 / 4).max(680).min(h.saturating_sub(112)).max(460);
    (width, height)
}

fn main() {
    let path = std::env::args().nth(1);
    let app = PaintApp::new(path.as_deref());
    let screen = ui::window::connect().ok().and_then(|d| d.screen_info().ok());
    let (w, h) = screen.map_or((1180, 680), |s| window_size(s.width, s.height));
    let mut spec = WindowSpec::new("Paint", w, h);
    spec.min_width = 640;
    spec.min_height = 460;
    spec.app_id = "paint".into();
    std::process::exit(ui::run(spec, app));
}

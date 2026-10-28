// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! About Veda: the system's version, hardware and licences. Its licences
//! pages list the parts of the system with their licences, as
//! `/system/licences/README` has them, and show each part's texts. The
//! voice agent reads what the window shows and can show the licences.
//!
//! Usage: `about [licences]`; with `licences` (Settings' Licence texts
//! button) it opens on the licences.

use std::ops::Range;
use std::path::Path;

use apps_common::format::{cstr, uptime};
use apps_common::licences::{self, NOTICE, Part};
use apps_common::store;
use proto::input::keys;
use ui::agent::{self, Action, AppAgentInfo, Value, arg_bool, json};
use ui::{Align, App, Color, Font, Icon, Rect, Ui, WindowSpec};

/// The most of a licence text the window reads (linux-firmware's WHENCE,
/// the longest, has some 470 KB).
const TEXT_LIMIT: usize = 4 << 20;

/// The most of a part's files the agent is told of.
const AGENT_FILES: usize = 40;

/// The height of the row of buttons at the bottom, with the space above it.
const BUTTONS_H: i32 = 48;

/// What the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    /// The version, the hardware, and Veda's licence in a line.
    About,
    /// The parts of the system and their licences.
    Licences,
    /// A part: what it is, and the files of its texts.
    Part(usize),
    /// A text of the part open.
    Text(usize),
}

/// A licence text as the window shows it.
struct Text {
    path: String,
    body: String,
    /// The size of its letters, at which its lines fit `width` (as they
    /// were written, mostly 72 to 80 columns), and its lines, wrapped to it.
    size: f32,
    lines: Vec<Range<usize>>,
    width: i32,
}

/// The columns of a licence text's lines that fit the window, at most: those
/// of the longest, up to 80; a longer line wraps.
fn columns(text: &str) -> usize {
    text.lines()
        .map(|l| l.trim_end().chars().map(|c| if c == '\t' { 8 } else { 1 }).sum::<usize>())
        .max()
        .unwrap_or(0)
        .clamp(40, 80)
}

struct About {
    info: abi::SystemInfo,
    last_refresh: u64,
    page: Page,
    /// The parts, read when the licences are first shown.
    parts: Vec<Part>,
    /// The texts of the part open: their paths in its directory (one,
    /// empty, for a part whose texts are a file).
    files: Vec<String>,
    text: Option<Text>,
    part_row: Option<usize>,
    file_row: Option<usize>,
    /// The page the last frame showed: a page shown anew gives its list
    /// the keyboard.
    last_page: Page,
}

impl About {
    /// The memory line ("512 MiB free of 1024 MiB").
    fn memory(&self) -> String {
        format!("{} MiB free of {} MiB", self.info.free_memory >> 20, self.info.total_memory >> 20)
    }

    /// Shows the parts' licences, reading the list the first time.
    fn show_licences(&mut self) {
        if self.parts.is_empty() {
            if let Ok(text) = store::read(Path::new(licences::README), 1 << 20) {
                self.parts = licences::parse(&String::from_utf8_lossy(&text));
            }
            if self.parts.is_empty() {
                println!("no {}: the licences of Veda and its fonts alone", licences::README);
                self.parts = licences::fallback();
            }
            println!("licences: {} parts", self.parts.len());
        }
        self.page = Page::Licences;
        self.text = None;
    }

    /// Opens part `i`: the list of its texts, or the text of a part of one.
    fn open_part(&mut self, i: usize) {
        let Some(part) = self.parts.get(i) else { return };
        self.files = texts_of(&part.texts);
        self.file_row = None;
        println!("licences of {}: {} file(s) in {}", part.title(), self.files.len(), part.texts);
        if self.files.len() == 1 {
            self.open_text(i, 0);
        } else {
            self.page = Page::Part(i);
            self.text = None;
        }
    }

    /// Shows the text `file` of part `part`.
    fn open_text(&mut self, part: usize, file: usize) {
        let (Some(p), Some(rel)) = (self.parts.get(part), self.files.get(file)) else { return };
        let path = if rel.is_empty() { p.texts.clone() } else { format!("{}/{rel}", p.texts) };
        let body = match store::read(Path::new(&path), TEXT_LIMIT) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => format!("{path} could not be read: {e}"),
        };
        println!("showing {path} ({} bytes)", body.len());
        self.text = Some(Text { path, body, size: 0.0, lines: Vec::new(), width: 0 });
        self.page = Page::Text(part);
    }

    /// Goes back a page.
    fn back(&mut self) {
        self.page = match self.page {
            Page::Text(part) if self.files.len() > 1 => Page::Part(part),
            Page::Text(_) | Page::Part(_) => Page::Licences,
            Page::Licences | Page::About => Page::About,
        };
        if !matches!(self.page, Page::Text(_)) {
            self.text = None;
        }
    }

    /// The part named `name` (its name, its title, or its directory's).
    fn find_part(&self, name: &str) -> Option<usize> {
        let name = name.trim();
        let is = |s: &str| s.eq_ignore_ascii_case(name);
        self.parts
            .iter()
            .position(|p| is(&p.name) || is(&p.title()) || p.texts.rsplit('/').next().is_some_and(is))
            .or_else(|| {
                let name = name.to_lowercase();
                self.parts.iter().position(|p| p.name.to_lowercase().contains(&name))
            })
    }

    /// The parts, as the agent is told of them.
    fn parts_json(&self) -> Value {
        Value::Array(
            self.parts.iter().map(|p| json!({ "name": p.title(), "licence": p.licence, "what": p.what })).collect(),
        )
    }

    /// The version, the hardware and Veda's licence.
    fn about_page(&mut self, ui: &mut Ui, area: Rect) {
        let t = ui.theme().clone();
        ui::draw_logo(&mut ui.canvas, Rect::new(area.x, area.y + 4, 64, 64));
        ui.label(Rect::new(area.x + 88, area.y, 400, 40), "Veda", Font::Bold, t.title_size + 6.0, t.text, Align::Left);
        let version = cstr(&self.info.version);
        ui.label(
            Rect::new(area.x + 90, area.y + 40, 400, 24),
            &format!("{version} · microkernel edition"),
            Font::Regular,
            t.font_size,
            t.text_dim,
            Align::Left,
        );

        let card = Rect::new(area.x, area.y + 96, area.w, 168);
        ui.card(card);
        let rows: [(Icon, &str, String); 5] = [
            (Icon::Cpu, "Processor", cstr(&self.info.cpu_brand)),
            (Icon::Grid, "Cores", format!("{} online", self.info.cpu_count)),
            (Icon::Chart, "Memory", self.memory()),
            (Icon::Clock, "Uptime", uptime(self.info.uptime_ns)),
            (
                Icon::List,
                "Processes",
                format!("{} processes, {} threads", self.info.process_count, self.info.thread_count),
            ),
        ];
        for (i, (icon, label, value)) in rows.iter().enumerate() {
            let y = card.y + 12 + i as i32 * 30;
            ui.icon(Rect::new(card.x + 16, y, 22, 28), *icon, 18.0, t.accent);
            ui.label(Rect::new(card.x + 48, y, 110, 28), label, Font::Regular, t.font_size, t.text_dim, Align::Left);
            ui.label(Rect::new(card.x + 160, y, card.w - 176, 28), value, Font::Bold, t.font_size, t.text, Align::Left);
        }
        let line = "Veda is free software under the GNU General Public License, version 3, and comes with \
                    ABSOLUTELY NO WARRANTY. Its other parts keep their own licences.";
        ui.paragraph(Rect::new(area.x, card.bottom() + 18, area.w, 60), line, t.small_size + 1.0, t.text_dim);
    }

    /// The parts and their licences.
    fn licences_page(&mut self, ui: &mut Ui, area: Rect) {
        let t = ui.theme().clone();
        ui.heading(Rect::new(area.x, area.y, area.w, 32), "Licences");
        let used = ui.paragraph(Rect::new(area.x, area.y + 40, area.w, 90), NOTICE, t.small_size + 1.0, t.text_dim);
        let top = area.y + 40 + used + 12;
        let list = Rect::new(area.x, top, area.w, area.bottom() - BUTTONS_H - top);
        let pressed = ui.input.pressed[0];
        let parts = &self.parts;
        let mut row = self.part_row;
        let resp = ui.list(list, "parts", parts.len(), 52, &mut row, |ui, i, r, state| {
            ui.row_background(r, state);
            let part = &parts[i];
            let half = (r.w - 36) / 2;
            let title = fit(ui, &part.title(), Font::Bold, t.font_size, half);
            ui.label(Rect::new(r.x + 12, r.y + 5, half, 22), &title, Font::Bold, t.font_size, t.text, Align::Left);
            let licence = fit(ui, &part.licence, Font::Regular, t.small_size, half);
            let at = Rect::new(r.right() - 24 - half, r.y + 5, half, 22);
            ui.label(at, &licence, Font::Regular, t.small_size, t.accent, Align::Right);
            let what = fit(ui, &part.what, Font::Regular, t.small_size, r.w - 36);
            ui.label(
                Rect::new(r.x + 12, r.y + 27, r.w - 36, 20),
                &what,
                Font::Regular,
                t.small_size,
                t.text_dim,
                Align::Left,
            );
        });
        self.part_row = row;
        // A click opens a part, as Enter does; the arrow keys only choose.
        if let Some(i) = resp.activated.or(resp.clicked.filter(|_| pressed)) {
            self.open_part(i);
        }
    }

    /// Part `i`: what it is, and the files of its texts.
    fn part_page(&mut self, ui: &mut Ui, area: Rect, i: usize) {
        let t = ui.theme().clone();
        let Some(part) = self.parts.get(i).cloned() else { return };
        ui.heading(Rect::new(area.x, area.y, area.w, 32), &part.title());
        let licence = fit(ui, &part.licence, Font::Bold, t.font_size, area.w);
        ui.label(Rect::new(area.x, area.y + 34, area.w, 24), &licence, Font::Bold, t.font_size, t.accent, Align::Left);
        let mut y = area.y + 64;
        y += ui.paragraph(Rect::new(area.x, y, area.w, 72), &part.what, t.small_size + 1.0, t.text_dim) + 6;
        let place = fit(ui, &format!("Its texts are in {}:", part.texts), Font::Regular, t.small_size, area.w);
        ui.label(Rect::new(area.x, y, area.w, 20), &place, Font::Regular, t.small_size, t.text_faint, Align::Left);
        y += 26;
        let list = Rect::new(area.x, y, area.w, area.bottom() - BUTTONS_H - y);
        let pressed = ui.input.pressed[0];
        let files = &self.files;
        let mut row = self.file_row;
        let resp = ui.list(list, "files", files.len(), 32, &mut row, |ui, f, r, state| {
            ui.row_background(r, state);
            let name = fit(ui, &files[f], Font::Regular, t.font_size, r.w - 48);
            ui.icon(Rect::new(r.x + 10, r.y, 20, r.h), Icon::File, 16.0, t.text_dim);
            ui.label(Rect::new(r.x + 36, r.y, r.w - 48, r.h), &name, Font::Regular, t.font_size, t.text, Align::Left);
        });
        self.file_row = row;
        if let Some(f) = resp.activated.or(resp.clicked.filter(|_| pressed)) {
            self.open_text(i, f);
        }
    }

    /// The text open, of part `i`.
    fn text_page(&mut self, ui: &mut Ui, area: Rect, i: usize) {
        let t = ui.theme().clone();
        let part = self.parts.get(i).map(Part::title).unwrap_or_default();
        let Some(text) = self.text.as_mut() else { return };
        let name = text.path.rsplit('/').next().unwrap_or(&text.path).to_string();
        ui.heading(Rect::new(area.x, area.y, area.w, 32), &fit(ui, &name, Font::Bold, t.heading_size, area.w));
        let place = fit(ui, &format!("{part} · {}", text.path), Font::Regular, t.small_size, area.w);
        ui.label(
            Rect::new(area.x, area.y + 34, area.w, 22),
            &place,
            Font::Regular,
            t.small_size,
            t.text_dim,
            Align::Left,
        );
        // The text has the window's width, but for a margin of 16 pixels.
        let top = area.y + 66;
        let r = Rect::new(area.x - 16, top, area.w + 32, area.bottom() - BUTTONS_H - top);
        ui.canvas.fill_rounded_rect(r, t.radius, Color::hex(0x151519));
        ui.canvas.stroke_rounded_rect(r, t.radius, 1.0, t.border);
        let inner = r.inset(1, 1, 1, 1);
        let font = ui.ctx.font(Font::Mono);
        // Wrapped once for the width, in letters at which its lines fit
        // (none smaller than 9 pixels), then drawn a screenful at a time.
        let width = inner.w - 30;
        if text.width != width {
            let line = "0".repeat(columns(&text.body));
            text.size = t.small_size;
            while text.size > 9.0 && ui.ctx.text.measure(font, text.size, &line) > width as f32 {
                text.size -= 0.5;
            }
            text.lines = ui.ctx.text.wrap(font, text.size, &text.body, width as f32);
            text.width = width;
        }
        let metrics = ui.ctx.text.metrics(font, text.size);
        let line_h = metrics.line_height.ceil().max(1.0) as i32;
        // Its keys: a line, a page, the start or the end.
        let page = (inner.h - 2 * line_h).max(line_h);
        for key in &ui.input.keys {
            let by = match key.code {
                keys::DOWN => line_h,
                keys::UP => -line_h,
                keys::PAGEDOWN | keys::SPACE => page,
                keys::PAGEUP => -page,
                keys::END => i32::MAX / 2,
                keys::HOME => -i32::MAX / 2,
                _ => continue,
            };
            ui.scroll_by("licence-text", by);
        }
        let content_h = text.lines.len() as i32 * line_h + 24;
        ui.scroll_area(inner, "licence-text", content_h, |ui, offset| {
            let first = ((offset - 12).max(0) / line_h) as usize;
            let shown = (inner.h / line_h + 2) as usize;
            for (n, range) in text.lines.iter().enumerate().skip(first).take(shown) {
                let y = inner.y + 12 + n as i32 * line_h - offset;
                let line = text.body[range.clone()].trim_end();
                let baseline = (y as f32 + metrics.ascent).floor();
                ui.ctx.text.draw(&mut ui.canvas, font, text.size, (inner.x + 12) as f32, baseline, line, t.text_dim);
            }
        });
    }
}

/// `text`, cut short with an ellipsis to fit `width`.
fn fit(ui: &Ui, text: &str, font: Font, size: f32, width: i32) -> String {
    ui.ctx.text.ellipsize(ui.ctx.font(font), size, text, width as f32)
}

/// The files of a part's texts at `texts`: of a file, one, with no path; of
/// a directory, every file under it, by its path there.
fn texts_of(texts: &str) -> Vec<String> {
    let root = Path::new(texts);
    if !root.is_dir() {
        return vec![String::new()];
    }
    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if let Ok(rel) = path.strip_prefix(root) {
                files.push(rel.to_string_lossy().into_owned());
            }
        }
    }
    licences::ordered(files)
}

impl App for About {
    fn update(&mut self, ui: &mut Ui) {
        let now = ui.now();
        if now - self.last_refresh > 1_000_000_000 {
            if let Ok(i) = runtime::object::system_info() {
                self.info = i;
            }
            self.last_refresh = now;
        }
        ui.repaint_at(self.last_refresh + 1_000_000_000);
        let shown = self.page;
        // Esc goes back a page.
        if self.page != Page::About && ui.input.key(keys::ESC) {
            self.back();
        }
        // A page shown anew gives its list the keyboard.
        if self.page != self.last_page {
            match self.page {
                Page::Licences => ui.focus_list("parts"),
                Page::Part(_) => ui.focus_list("files"),
                Page::About | Page::Text(_) => {}
            }
            self.last_page = self.page;
        }
        let area = ui.rect().inset(32, 28, 32, 24);
        match self.page {
            Page::About => self.about_page(ui, area),
            Page::Licences => self.licences_page(ui, area),
            Page::Part(i) => self.part_page(ui, area, i),
            Page::Text(i) => self.text_page(ui, area, i),
        }
        let left = Rect::new(area.x, area.bottom() - 36, 110, 36);
        if self.page == Page::About {
            if ui.button(left, "Licences") {
                self.show_licences();
            }
        } else if ui.button(left, "Back") {
            self.back();
        }
        let br = Rect::new(area.right() - 110, area.bottom() - 36, 110, 36);
        if ui.primary_button(br, "OK") {
            ui.close_window();
        }
        if self.page != shown {
            ui.repaint();
        }
    }

    fn agent_info(&self) -> Option<AppAgentInfo> {
        Some(agent::info(
            "The About Veda window: the version of Veda, the processor, memory and uptime, and the licences of Veda \
             and of the system's other parts.",
            vec![
                Action::new(
                    "show_licences",
                    "Shows the licences in the window: the system's parts with their licences, or one part's \
                     licence texts (or hides them)",
                )
                .param("show", "boolean", "Show them (the default) or go back to the version", false)
                .param("part", "string", "A part whose texts to show, by its name in the list of licences", false)
                .build(),
            ],
        ))
    }

    fn agent_state(&self) -> Value {
        let i = &self.info;
        let mut v = json!({
            "version": cstr(&i.version),
            "edition": "microkernel edition",
            "processor": cstr(&i.cpu_brand),
            "cores": i.cpu_count,
            "memory": self.memory(),
            "uptime": uptime(i.uptime_ns),
            "processes": i.process_count,
            "threads": i.thread_count,
            "licences_shown": self.page != Page::About,
        });
        if self.page != Page::About {
            v["licences"] = json!(NOTICE);
            v["parts"] = self.parts_json();
        }
        if let Page::Part(p) | Page::Text(p) = self.page
            && let Some(part) = self.parts.get(p)
        {
            v["part"] = json!(part.title());
        }
        if let (Page::Text(_), Some(text)) = (self.page, &self.text) {
            v["text"] = json!(text.path);
        }
        v
    }

    fn agent_invoke(&mut self, action: &str, args: &Value) -> Result<Value, String> {
        match action {
            "show_licences" => {
                // As the Licences and Back buttons do.
                if !arg_bool(args, "show").unwrap_or(true) {
                    self.page = Page::About;
                    self.text = None;
                    return Ok(json!({ "licences_shown": false }));
                }
                self.show_licences();
                let mut v = json!({ "licences_shown": true, "licences": NOTICE, "parts": self.parts_json() });
                if let Some(name) = args.get("part").and_then(Value::as_str).filter(|n| !n.trim().is_empty()) {
                    let Some(i) = self.find_part(name) else {
                        let names: Vec<String> = self.parts.iter().map(|p| p.name.clone()).collect();
                        return Err(format!("no part called {name}: the parts are {}", names.join(", ")));
                    };
                    self.open_part(i);
                    let part = &self.parts[i];
                    v["part"] = json!({
                        "name": part.title(),
                        "licence": part.licence,
                        "what": part.what,
                        "texts": part.texts,
                    });
                    let files: Vec<&String> = self.files.iter().take(AGENT_FILES).collect();
                    v["files"] = json!(files);
                    if self.files.len() > AGENT_FILES {
                        v["more_files"] = json!(self.files.len() - AGENT_FILES);
                    }
                }
                Ok(v)
            }
            other => Err(format!("About Veda has no action called {other}")),
        }
    }
}

fn main() {
    let mut spec = WindowSpec::new("About Veda", 560, 540);
    spec.resizable = false;
    spec.app_id = "about".into();
    let mut about = About {
        info: abi::SystemInfo::default(),
        last_refresh: 0,
        page: Page::About,
        parts: Vec::new(),
        files: Vec::new(),
        text: None,
        part_row: None,
        file_row: None,
        last_page: Page::About,
    };
    if std::env::args().nth(1).is_some_and(|a| a.to_ascii_lowercase().starts_with("licen")) {
        about.show_licences();
    }
    std::process::exit(ui::run(spec, about));
}

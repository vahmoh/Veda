// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The library: the videos in `~/Videos` as cards (a picture from each, its
//! length, size and codec), with those left part way first under
//! "Continue watching".
//!
//! The rows of cards are worked out again only when the videos, where they
//! were left, or the number of columns change.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use vfiles::format::duration as time;
use vfiles::kind::{FileKind, TYPES, VIDEOS, is_video};
use vfiles::path::{HOME, display_path, join, natural_cmp};
use vgfx::{Align, Bitmap, Color, Rect};
use videos_core::text;
use vproto::fs::FsError;
use vproto::input::keys;
use vproto::media::MediaInfo;
use vproto::vfs;
use vui::{ButtonKind, Cursor, FileDialog, Font, Icon, Ui};

use crate::player::ACCENT;
use crate::thumbs;
use crate::{OpenAt, Videos};

/// The folder shown unless another is asked for: `~/Videos`.
pub fn videos_dir() -> String {
    join(HOME, "Videos")
}

const HEADER_H: i32 = 92;
const MARGIN: i32 = 32;
const MIN_GAP: i32 = 22;
const CARD_W: i32 = thumbs::CARD.0 as i32;
const CARD_H: i32 = thumbs::CARD.1 as i32;
/// A card: the picture and two lines of text.
const ITEM_H: i32 = CARD_H + 54;
const ROW_GAP: i32 = 20;
/// A section's heading.
const SECTION_H: i32 = 40;
const RADIUS: f32 = 10.0;
/// The banner saying playback is not available.
const BANNER_H: i32 = 64;

/// A card's picture.
pub enum Card {
    /// Not asked for yet.
    Missing,
    Requested,
    Ready(Bitmap),
    /// It plays, but there is no picture of it (sound only, or a picture
    /// that could not be made).
    NoPicture,
    /// The video could not be read.
    Failed,
}

/// One video file.
pub struct Entry {
    pub name: String,
    pub path: String,
    /// The file's name without its extension, or the title it carries.
    pub title: String,
    pub size: u64,
    pub modified: u64,
    /// What it is, once read (or why it cannot be played).
    pub info: Option<Result<MediaInfo, String>>,
    pub card: Card,
}

impl Entry {
    pub fn duration_ms(&self) -> Option<u64> {
        match &self.info {
            Some(Ok(i)) => i.duration_us.map(|d| d.max(0) as u64 / 1000),
            _ => None,
        }
    }

    /// Whether it is sound without pictures.
    pub fn sound_only(&self) -> bool {
        matches!(&self.info, Some(Ok(i)) if i.video_stream().is_none() && i.audio_stream().is_some())
    }

    /// "720p · AV1", or why it cannot be played.
    pub fn description(&self) -> String {
        match &self.info {
            Some(Ok(i)) => match (i.video_stream(), i.audio_stream()) {
                (Some(v), _) => {
                    let (w, h) = v.display_size();
                    let mut s = format!("{} · {}", text::resolution(w, h), text::codec(&v.codec));
                    if v.hdr {
                        s += " · HDR";
                    }
                    s
                }
                (None, Some(a)) => format!("Sound only · {}", text::codec(&a.codec)),
                (None, None) => String::from("Nothing to play"),
            },
            Some(Err(e)) => e.clone(),
            None => String::new(),
        }
    }
}

/// Lists the videos in `dir`, in natural name order.
pub fn list(vfs: &vfs::Client, dir: &str) -> Result<Vec<Entry>, String> {
    let items = match vfs.read_dir(dir.into()) {
        Ok(Ok(items)) => items,
        Ok(Err(FsError::NotFound)) => return Err(format!("The folder {dir} does not exist.")),
        Ok(Err(e)) => return Err(format!("The folder {dir} could not be read ({e}).")),
        Err(_) => return Err("The file system is not responding.".into()),
    };
    let mut entries: Vec<Entry> = items
        .into_iter()
        .filter(|e| !e.is_dir && is_video(&e.name))
        .map(|e| Entry {
            path: join(dir, &e.name),
            title: vfiles::path::file_stem(&e.name).into(),
            name: e.name,
            size: e.size,
            modified: e.modified,
            info: None,
            card: Card::Missing,
        })
        .collect();
    entries.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    Ok(entries)
}

/// The extensions of the files Videos plays (for the open dialog): the
/// system's file types that open in Videos.
fn video_extensions() -> Vec<&'static str> {
    TYPES
        .iter()
        .filter(|t| t.kind == FileKind::Video && t.app == Some(VIDEOS))
        .flat_map(|t| t.extensions.iter().copied())
        .collect()
}

/// A row of cards: its heading (if it starts a section) and its entries.
pub(crate) struct Row {
    heading: Option<&'static str>,
    items: Vec<usize>,
}

/// The rows of the grid as last worked out, and for what: the library's
/// version, the remembered positions' and the number of columns.
#[derive(Default)]
pub(crate) struct Rows {
    key: Option<(u64, u64, usize)>,
    rows: Vec<Row>,
}

impl Videos {
    /// The entries left part way, most recently first.
    pub(crate) fn continuing(&self) -> Vec<usize> {
        let mut v: Vec<(u64, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let r = self.state.resume_of(&e.path)?;
                (r.size == e.size && r.modified == e.modified).then_some((r.at_s, i))
            })
            .collect();
        v.sort_by_key(|(at, _)| core::cmp::Reverse(*at));
        v.into_iter().map(|(_, i)| i).collect()
    }

    /// The rows of the grid, `cols` cards wide (worked out again only when
    /// something they show changed).
    fn rows(&mut self, cols: usize) -> Vec<Row> {
        let key = (self.entries_version, self.state.version(), cols);
        if self.rows.key != Some(key) {
            let mut rows = Vec::new();
            let cont = self.continuing();
            if !cont.is_empty() {
                rows.push(Row { heading: Some("Continue watching"), items: cont.into_iter().take(cols).collect() });
            }
            let sectioned = !rows.is_empty();
            let all: Vec<usize> = (0..self.entries.len()).collect();
            for (n, chunk) in all.chunks(cols.max(1)).enumerate() {
                let heading = (sectioned && n == 0).then_some("All videos");
                rows.push(Row { heading, items: chunk.to_vec() });
            }
            self.rows = Rows { key: Some(key), rows };
        }
        core::mem::take(&mut self.rows.rows)
    }

    pub(crate) fn library(&mut self, ui: &mut Ui) {
        let full = ui.rect();
        let (header, mut body) = full.split_top(HEADER_H);
        self.library_header(ui, header);
        if let Some(reason) = self.unavailable.clone() {
            let banner = Rect::new(body.x + MARGIN, body.y, body.w - 2 * MARGIN, BANNER_H - 12);
            unavailable_banner(ui, banner, &reason);
            body = body.inset(0, BANNER_H, 0, 0);
        }
        if let Some(err) = self.list_error.clone() {
            empty_state(ui, body, Icon::Warning, "Can't show this folder", &err);
            return;
        }
        if self.entries.is_empty() {
            let hint = format!(
                "Videos you save in {} (MP4, MKV, WebM, MOV, AVI, Ogg or MPEG) appear here.",
                display_path(&self.dir)
            );
            empty_state(ui, body, Icon::Video, "No videos yet", &hint);
            return;
        }
        let avail = body.w - 2 * MARGIN;
        let cols = ((avail + MIN_GAP) / (CARD_W + MIN_GAP)).max(1);
        let gap = if cols > 1 { ((avail - cols * CARD_W) / (cols - 1)).clamp(MIN_GAP, 56) } else { 0 };
        let grid_w = cols * CARD_W + (cols - 1) * gap;
        let x0 = body.x + (body.w - grid_w) / 2;
        let rows = self.rows(cols as usize);
        // Where each row is (from the content's top).
        let mut tops = Vec::with_capacity(rows.len());
        let mut y = 8;
        for r in &rows {
            if r.heading.is_some() {
                y += SECTION_H;
            }
            tops.push(y);
            y += ITEM_H + ROW_GAP;
        }
        let content_h = y + 16;
        self.library_keys(ui, &rows, (body.h / (ITEM_H + ROW_GAP)).max(1) as usize);
        if self.reveal_selection {
            if let Some(row) = self.row_of(&rows, self.current) {
                let top = tops[row] - if rows[row].heading.is_some() { SECTION_H } else { 0 };
                ui.scroll_into_view("grid", body.h, top, tops[row] - top + ITEM_H + 16);
            }
            self.reveal_selection = false;
        }
        let mut open = None;
        let mut wanted = Vec::new();
        ui.scroll_area(body, "grid", content_h, |ui, offset| {
            let mut position = 0;
            for (r, row) in rows.iter().enumerate() {
                let top = body.y + tops[r] - offset;
                let visible = top + ITEM_H > body.y - ITEM_H && top < body.bottom() + ITEM_H;
                if let Some(h) = row.heading {
                    let t = ui.theme().clone();
                    let hr = Rect::new(x0, top - SECTION_H + 4, grid_w, SECTION_H - 12);
                    ui.label(hr, h, Font::Bold, t.heading_size, t.text, Align::Left);
                }
                for (c, &i) in row.items.iter().enumerate() {
                    if visible {
                        let x = x0 + c as i32 * (CARD_W + gap);
                        let continuing = row.heading == Some("Continue watching");
                        if self.card(ui, i, position, x, top, continuing) {
                            open = Some((i, position));
                        }
                        wanted.push(i);
                    }
                    position += 1;
                }
            }
        });
        // Pictures for the visible cards and those just below.
        for i in wanted {
            self.want_card(i);
        }
        // Kept for the next frame, unless what they show changed.
        if self.rows.rows.is_empty() {
            self.rows.rows = rows;
        }
        if let Some((i, position)) = open {
            self.current = position;
            self.keyboard_nav = false;
            self.open_entry(i, OpenAt::Resume);
        }
    }

    fn row_of(&self, rows: &[Row], position: usize) -> Option<usize> {
        let mut n = 0;
        for (r, row) in rows.iter().enumerate() {
            if position < n + row.items.len() {
                return Some(r);
            }
            n += row.items.len();
        }
        None
    }

    fn library_header(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme().clone();
        ui.label(
            Rect::new(r.x + MARGIN, r.y + 20, r.w - 2 * MARGIN - 200, 38),
            "Videos",
            Font::Bold,
            t.title_size + 2.0,
            t.text,
            Align::Left,
        );
        let count = self.entries.len();
        let what = if self.list_error.is_some() {
            String::from("Unavailable")
        } else if count == 0 {
            String::from("No videos")
        } else if count == 1 {
            String::from("1 video")
        } else {
            format!("{count} videos")
        };
        let total: u64 = self.entries.iter().filter_map(|e| e.duration_ms()).sum();
        let mut sub = what;
        if total > 0 {
            sub += &format!("  ·  {}", time(total));
        }
        sub += &format!("  ·  {}", display_path(&self.dir));
        ui.label(
            Rect::new(r.x + MARGIN + 1, r.y + 58, r.w - 2 * MARGIN - 200, 20),
            &sub,
            Font::Regular,
            13.0,
            t.text_dim,
            Align::Left,
        );
        let open = Rect::new(r.right() - MARGIN - 100, r.y + 28, 100, 34);
        if ui.button_full(open, Some(Icon::Open), "Open", ButtonKind::Secondary) {
            self.dialog = Some(FileDialog::open("Open a video", &self.dir).with_filter(&video_extensions()));
        }
        if ui.icon_button(Rect::new(open.x - 46, r.y + 26, 38, 38), Icon::Refresh, "Refresh (F5)") {
            self.reload();
        }
    }

    /// Draws card `i` (position `position` in keyboard order) with its
    /// top-left corner at (x, y); true if it was clicked.
    fn card(&mut self, ui: &mut Ui, i: usize, position: usize, x: i32, y: i32, continuing: bool) -> bool {
        let t = ui.theme().clone();
        let id = ui.id("card") ^ ((i as u64 + 1) << 24) ^ (continuing as u64) << 40;
        let item = Rect::new(x, y, CARD_W, ITEM_H);
        let resp = ui.interact(id, item.inflate(6));
        let hover = ui.animate(id, if resp.hovered { 1.0 } else { 0.0 }, 7.0);
        if resp.hovered {
            ui.set_cursor(Cursor::Hand);
        }
        if resp.pressed {
            self.current = position;
        }
        let lift = (hover * 3.0 + 0.5) as i32;
        let pic = Rect::new(x, y - lift, CARD_W, CARD_H);
        if hover > 0.01 {
            ui.canvas.fill_rounded_rect(
                item.inflate(8).translate(0, -lift),
                14.0,
                Color::rgba(255, 255, 255, (9.0 * hover) as u8),
            );
            self.card_shadow.draw(
                &mut ui.canvas,
                pic.translate(0, 6),
                Color::rgba(0, 0, 0, (130.0 * hover) as u8),
                false,
            );
        }
        let e = &self.entries[i];
        ui.canvas.fill_rounded_rect_gradient(pic, RADIUS, Color::hex(0x1E1E25), Color::hex(0x17171C));
        match &e.card {
            Card::Ready(b) => {
                let (bx, by) = (pic.x + (pic.w - b.width) / 2, pic.y + (pic.h - b.height) / 2);
                if b.width == pic.w && b.height == pic.h {
                    draw_rounded(ui, b, bx, by, RADIUS as i32);
                } else {
                    ui.canvas.draw_bitmap(b, bx, by, 255);
                }
            }
            Card::Failed => ui.icon(pic, Icon::Warning, 28.0, t.warning.fade(0.8)),
            Card::NoPicture if e.sound_only() => ui.icon(pic, Icon::Music, 30.0, t.text_faint),
            _ => ui.icon(pic, Icon::Video, 30.0, t.text_faint),
        }
        // Where it was left, or that it was watched.
        let resume = self.state.resume_of(&e.path).filter(|r| r.size == e.size && r.modified == e.modified);
        let duration = e.duration_ms().or(resume.map(|r| r.duration_ms));
        if let (Some(r), Some(d)) = (resume, duration.filter(|d| *d > 0)) {
            let track = Rect::new(pic.x + 10, pic.bottom() - 10, pic.w - 20, 4);
            ui.canvas.fill_rounded_rect(track, 2.0, Color::rgba(255, 255, 255, 70));
            let w = (track.w as u64 * r.position_ms.min(d) / d) as i32;
            ui.canvas.fill_rounded_rect(Rect::new(track.x, track.y, w.max(4), 4), 2.0, ACCENT);
        } else if self.state.is_watched(&e.path) {
            let badge = Rect::new(pic.right() - 30, pic.y + 8, 22, 22);
            ui.canvas.fill_rounded_rect(badge, 11.0, Color::rgba(0, 0, 0, 150));
            ui.icon(badge, Icon::Check, 13.0, ACCENT);
        }
        if let Some(d) = duration
            && !continuing
        {
            let label = time(d);
            let w = ui.measure(&label, Font::Bold, 11.5) as i32 + 12;
            let badge = Rect::new(pic.right() - w - 8, pic.bottom() - 30, w, 20);
            ui.canvas.fill_rounded_rect(badge, 6.0, Color::rgba(0, 0, 0, 170));
            ui.label(badge, &label, Font::Bold, 11.5, Color::WHITE, Align::Center);
        }
        if hover > 0.01 {
            let (cx, cy) = pic.center();
            let a = (hover * 255.0) as u8;
            ui.canvas.fill_circle(cx as f32, cy as f32, 24.0, Color::rgba(0, 0, 0, (a as f32 * 0.55) as u8));
            ui.icon(Rect::new(cx - 22, cy - 24, 48, 48), Icon::Play, 20.0, Color::rgba(255, 255, 255, a));
        }
        if self.keyboard_nav && position == self.current {
            ui.canvas.stroke_rounded_rect(pic.inflate(4), RADIUS + 4.0, 2.0, t.accent);
        }
        let e = &self.entries[i];
        ui.label(
            Rect::new(x + 2, pic.bottom() + 10, CARD_W - 4, 20),
            &e.title,
            Font::Regular,
            14.0,
            t.text,
            Align::Left,
        );
        let (line, color) = match (continuing, resume, duration) {
            (true, Some(r), Some(d)) => (format!("{} left", time(d.saturating_sub(r.position_ms))), t.text_dim),
            _ => match &e.info {
                Some(Err(_)) => (e.description(), t.warning),
                _ => (e.description(), t.text_dim),
            },
        };
        ui.label(Rect::new(x + 2, pic.bottom() + 31, CARD_W - 4, 16), &line, Font::Regular, 12.0, color, Align::Left);
        resp.clicked
    }

    /// Asks for card `i`'s picture (once).
    fn want_card(&mut self, i: usize) {
        if self.unavailable.is_some() {
            return;
        }
        let Some(thumbs) = &self.thumbs else { return };
        let e = &mut self.entries[i];
        if matches!(e.card, Card::Missing) {
            e.card = Card::Requested;
            thumbs.card(e.path.clone());
        }
    }

    /// A card the worker made.
    pub(crate) fn card_ready(&mut self, path: &str, info: Result<MediaInfo, String>, picture: Option<thumbs::Picture>) {
        let Some(e) = self.entries.iter_mut().find(|e| e.path == path) else { return };
        if let Ok(i) = &info
            && !i.title.is_empty()
        {
            e.title = i.title.clone();
        }
        e.card = match picture {
            Some(p) => Card::Ready(Bitmap::from_straight(p.width as i32, p.height as i32, p.pixels)),
            None if info.is_err() => Card::Failed,
            // It plays: sound only, or a video whose picture could not be
            // made.
            None => Card::NoPicture,
        };
        e.info = Some(info);
    }

    fn library_keys(&mut self, ui: &mut Ui, rows: &[Row], rows_visible: usize) {
        if ui.input.key(keys::F5) {
            self.reload();
            return;
        }
        let n: usize = rows.iter().map(|r| r.items.len()).sum();
        if n == 0 || self.dialog.is_some() || ui.input.keys.is_empty() {
            return;
        }
        let cur = self.current.min(n - 1);
        // The row and column of each position.
        let mut at = Vec::with_capacity(n);
        for (r, row) in rows.iter().enumerate() {
            for c in 0..row.items.len() {
                at.push((r, c));
            }
        }
        let start_of = |r: usize| rows[..r].iter().map(|row| row.items.len()).sum::<usize>();
        let in_row = |r: usize, c: usize| start_of(r) + c.min(rows[r].items.len() - 1);
        let mut next = None;
        let mut open = false;
        for k in &ui.input.keys {
            let pos = next.unwrap_or(cur);
            let (r, c) = at[pos];
            next = match k.code {
                keys::LEFT => Some(pos.saturating_sub(1)),
                keys::RIGHT => Some((pos + 1).min(n - 1)),
                keys::UP if r > 0 => Some(in_row(r - 1, c)),
                keys::DOWN if r + 1 < rows.len() => Some(in_row(r + 1, c)),
                keys::HOME => Some(0),
                keys::END => Some(n - 1),
                keys::PAGEUP => Some(in_row(r.saturating_sub(rows_visible), c)),
                keys::PAGEDOWN => Some(in_row((r + rows_visible).min(rows.len() - 1), c)),
                keys::ENTER | keys::KPENTER | keys::SPACE => {
                    open = true;
                    Some(pos)
                }
                _ => next,
            };
        }
        if let Some(p) = next {
            self.current = p;
            self.keyboard_nav = true;
            self.reveal_selection = true;
        }
        if open {
            let (r, c) = at[self.current.min(n - 1)];
            let i = rows[r].items[c];
            self.open_entry(i, OpenAt::Resume);
        }
    }
}

/// A picture with its corners rounded by drawing them over the card's
/// rounded background.
fn draw_rounded(ui: &mut Ui, b: &Bitmap, x: i32, y: i32, radius: i32) {
    let r = radius.clamp(0, b.height / 2);
    // The middle band is copied; the top and bottom bands are clipped to the
    // rounded rectangle, approximated by insetting their ends.
    ui.canvas.blit(b, Rect::new(0, r, b.width, b.height - 2 * r), x, y + r);
    for row in 0..r {
        // How far the curve is in at this row.
        let dy = (r - row) as f32 - 0.5;
        let inset = r - libm::floorf(libm::sqrtf((r * r) as f32 - dy * dy)) as i32;
        let w = b.width - 2 * inset;
        if w <= 0 {
            continue;
        }
        ui.canvas.blit(b, Rect::new(inset, row, w, 1), x + inset, y + row);
        let bottom = b.height - 1 - row;
        ui.canvas.blit(b, Rect::new(inset, bottom, w, 1), x + inset, y + bottom);
    }
}

/// The banner over the library when videos cannot be played.
fn unavailable_banner(ui: &mut Ui, r: Rect, reason: &str) {
    let t = ui.theme().clone();
    ui.canvas.fill_rounded_rect(r, 10.0, t.warning.with_alpha(26));
    ui.canvas.stroke_rounded_rect(r, 10.0, 1.0, t.warning.with_alpha(90));
    ui.icon(Rect::new(r.x + 12, r.y, 32, r.h), Icon::Warning, 20.0, t.warning);
    ui.label(
        Rect::new(r.x + 52, r.y + 7, r.w - 64, 18),
        "Video playback isn't available",
        Font::Bold,
        13.5,
        t.text,
        Align::Left,
    );
    ui.label(Rect::new(r.x + 52, r.y + 27, r.w - 64, 16), reason, Font::Regular, 12.0, t.text_dim, Align::Left);
}

/// A centred icon, heading and explanation.
pub(crate) fn empty_state(ui: &mut Ui, area: Rect, icon: Icon, title: &str, text: &str) {
    let t = ui.theme().clone();
    let w = area.w.min(460);
    let top = area.y + (area.h - 220).max(0) / 2;
    let circle = Rect::new(area.x + (area.w - 88) / 2, top, 88, 88);
    ui.canvas.fill_rounded_rect(circle, 44.0, Color::rgba(255, 255, 255, 12));
    ui.icon(circle, icon, 40.0, if icon == Icon::Warning { t.warning } else { t.text_dim });
    let x = area.x + (area.w - w) / 2;
    ui.label(Rect::new(x, circle.bottom() + 18, w, 28), title, Font::Bold, t.heading_size + 2.0, t.text, Align::Center);
    let f = ui.ctx.font(Font::Regular);
    ui.ctx.text.draw_wrapped(
        &mut ui.canvas,
        f,
        14.0,
        Rect::new(x, circle.bottom() + 54, w, 80),
        text,
        t.text_dim,
        Align::Center,
    );
}

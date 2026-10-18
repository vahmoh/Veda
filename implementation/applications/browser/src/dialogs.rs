// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the browser asks or shows over everything: a page's dialogs (an
//! alert, a confirmation, a prompt, leaving a page that asks), signing in
//! to a site, a site's request for its storage, choosing a file for a
//! page; and the browser's history, bookmarks, settings and about window.
//! A page's dialog waits for its answer: the engine hears it as the dialog
//! closes.

use browser_core::search::ENGINES;
use browser_core::settings::{Home, Settings, ZOOMS};
use browser_core::url;
use gfx::{Align, Rect};
use proto::input::keys as code;
use ui::{ButtonKind, FileDialog, FileDialogResult, Font, Icon, Ui};

use crate::browser::{Browser, Focus};
use crate::chrome::fit;
use crate::engine::{self, Dialog, Permission, Request};
use crate::store::{self, Kept};

pub enum Modal {
    Dialog { request: Request, kind: Dialog, message: String, text: String, site: String },
    Permission { request: Request, kind: Permission, site: String },
    Auth { request: Request, host: String, realm: String, proxy: bool, user: String, password: String },
    Files { request: Request, dialog: Box<FileDialog> },
    History { query: String },
    Bookmarks { query: String },
    Settings(Box<SettingsForm>),
    About,
}

/// The settings as the window edits them.
pub struct SettingsForm {
    engine: usize,
    home_page: bool,
    home: String,
    restore: bool,
    block_third_party_cookies: bool,
    acceleration: bool,
    zoom: usize,
    clear_history: bool,
    clear_cookies: bool,
    clear_cache: bool,
}

impl Modal {
    pub fn dialog(request: Request, kind: Dialog, message: &str, default: &str, site: String) -> Modal {
        Modal::Dialog { request, kind, message: message.to_string(), text: default.to_string(), site }
    }

    pub fn permission(request: Request, kind: Permission, site: &str) -> Modal {
        Modal::Permission { request, kind, site: site.to_string() }
    }

    pub fn auth(request: Request, host: &str, realm: &str, proxy: bool) -> Modal {
        Modal::Auth {
            request,
            host: host.to_string(),
            realm: realm.to_string(),
            proxy,
            user: String::new(),
            password: String::new(),
        }
    }

    pub fn files(request: Request, _multiple: bool) -> Modal {
        Modal::Files { request, dialog: Box::new(FileDialog::open("Choose a file", "/home/user")) }
    }

    pub fn settings(s: &Settings) -> Modal {
        Modal::Settings(Box::new(SettingsForm {
            engine: ENGINES.iter().position(|e| e.id == s.search).unwrap_or(0),
            home_page: matches!(s.home, Home::Page(_)),
            home: match &s.home {
                Home::Page(p) => p.clone(),
                Home::NewTab => String::new(),
            },
            restore: s.restore,
            block_third_party_cookies: s.block_third_party_cookies,
            acceleration: s.acceleration,
            zoom: ZOOMS.iter().position(|&z| z == s.zoom).unwrap_or(5),
            clear_history: false,
            clear_cookies: false,
            clear_cache: false,
        }))
    }
}

pub fn draw(b: &mut Browser, ui: &mut Ui) {
    let Some(modal) = b.modal.take() else { return };
    b.modal = match modal {
        Modal::Dialog { request, kind, message, text, site } => page_dialog(b, ui, request, kind, message, text, site),
        Modal::Permission { request, kind, site } => permission(b, ui, request, kind, site),
        Modal::Auth { request, host, realm, proxy, user, password } => {
            auth(b, ui, request, host, realm, proxy, user, password)
        }
        Modal::Files { request, mut dialog } => match dialog.show(ui) {
            Some(FileDialogResult::Chosen(path)) => {
                // A copy, which the network process, sandboxed, reaches.
                match store::upload_copy(&path) {
                    Ok(copy) => b.engine.files_reply(request, &[copy]),
                    Err(e) => {
                        b.note(format!("{path} cannot be uploaded: {e}"));
                        b.engine.files_reply(request, &[]);
                    }
                }
                None
            }
            Some(FileDialogResult::Cancelled) => {
                b.engine.files_reply(request, &[]);
                None
            }
            None => Some(Modal::Files { request, dialog }),
        },
        Modal::History { query } => history(b, ui, query),
        Modal::Bookmarks { query } => bookmarks(b, ui, query),
        Modal::Settings(form) => settings(b, ui, form),
        Modal::About => about(ui),
    };
    if b.modal.is_none() && b.focus == Focus::Page {
        ui.state.focus = None;
    }
}

/// The OK and Cancel buttons at a card's foot (Cancel only if `cancel`);
/// which was pressed: `Some(true)` OK (or Enter), `Some(false)` Cancel (or
/// Esc).
fn buttons(ui: &mut Ui, card: Rect, ok: &str, cancel: Option<&str>) -> Option<bool> {
    let inner = card.inset(24, 0, 24, 20);
    let size = ui.theme().font_size;
    let ok_w = (ui.measure(ok, Font::Bold, size) as i32 + 32).max(92);
    let mut x = inner.right() - ok_w;
    let mut picked = None;
    if ui.button_full(Rect::new(x, inner.bottom() - 36, ok_w, 36), None, ok, ButtonKind::Primary) {
        picked = Some(true);
    }
    if let Some(cancel) = cancel {
        let w = (ui.measure(cancel, Font::Bold, size) as i32 + 32).max(92);
        x -= w + 10;
        if ui.button_full(Rect::new(x, inner.bottom() - 36, w, 36), None, cancel, ButtonKind::Secondary) {
            picked = Some(false);
        }
    }
    if ui.input.key(code::ENTER) {
        picked = picked.or(Some(true));
    }
    if ui.input.key(code::ESC) {
        picked = picked.or(Some(cancel.is_none()));
    }
    picked
}

#[allow(clippy::too_many_arguments)]
fn page_dialog(
    b: &mut Browser,
    ui: &mut Ui,
    request: Request,
    kind: Dialog,
    message: String,
    mut text: String,
    site: String,
) -> Option<Modal> {
    let t = ui.theme().clone();
    let prompt = kind == Dialog::Prompt;
    let h = if prompt { 250 } else { 210 };
    let answer = ui.modal(480, h, |ui, card| {
        let inner = card.inset(24, 20, 24, 20);
        let title = match kind {
            Dialog::Leave => "Leave this page?".to_string(),
            _ if site.is_empty() => "The page says".to_string(),
            _ => format!("{site} says"),
        };
        ui.heading(Rect::new(inner.x, inner.y, inner.w, 26), &title);
        let body = if kind == Dialog::Leave { "Changes you made may not be saved." } else { message.as_str() };
        ui.paragraph(
            Rect::new(inner.x, inner.y + 36, inner.w, if prompt { 60 } else { 90 }),
            body,
            t.font_size,
            t.text_dim,
        );
        if prompt {
            let r = Rect::new(inner.x, inner.bottom() - 90, inner.w, 34);
            ui.text_input(r, "dialog-text", &mut text, "");
            ui.focus_text_input("dialog-text");
        }
        match kind {
            Dialog::Alert => buttons(ui, card, "OK", None),
            Dialog::Leave => buttons(ui, card, "Leave", Some("Stay")),
            Dialog::Confirm | Dialog::Prompt => buttons(ui, card, "OK", Some("Cancel")),
        }
    });
    match answer {
        Some(ok) => {
            b.engine.dialog_reply(request, ok, (prompt && ok).then_some(text.as_str()));
            None
        }
        None => Some(Modal::Dialog { request, kind, message, text, site }),
    }
}

fn permission(b: &mut Browser, ui: &mut Ui, request: Request, kind: Permission, site: String) -> Option<Modal> {
    let t = ui.theme().clone();
    let what = match kind {
        Permission::Storage => "use its cookies and data here, where it is part of another site's page",
        Permission::Geolocation => "know your location",
        Permission::Notifications => "show notifications",
        Permission::DeviceInfo => "list your cameras and microphones",
        Permission::Media => "use your camera or microphone",
        Permission::Other => "do something only you may allow",
    };
    let answer = ui.modal(480, 190, |ui, card| {
        let inner = card.inset(24, 20, 24, 20);
        ui.heading(Rect::new(inner.x, inner.y, inner.w, 26), &format!("{site} wants to"));
        ui.paragraph(Rect::new(inner.x, inner.y + 36, inner.w, 60), what, t.font_size, t.text_dim);
        buttons(ui, card, "Allow", Some("Block"))
    });
    match answer {
        Some(allow) => {
            b.engine.permission_reply(request, allow);
            None
        }
        None => Some(Modal::Permission { request, kind, site }),
    }
}

#[allow(clippy::too_many_arguments)]
fn auth(
    b: &mut Browser,
    ui: &mut Ui,
    request: Request,
    host: String,
    realm: String,
    proxy: bool,
    mut user: String,
    mut password: String,
) -> Option<Modal> {
    let t = ui.theme().clone();
    let answer = ui.modal(460, 300, |ui, card| {
        let inner = card.inset(24, 20, 24, 20);
        let title = if proxy { format!("Sign in to the proxy {host}") } else { format!("Sign in to {host}") };
        ui.heading(Rect::new(inner.x, inner.y, inner.w, 26), &title);
        if !realm.is_empty() {
            let realm = fit(ui, &format!("It says: \u{201c}{realm}\u{201d}"), Font::Regular, t.font_size, inner.w);
            ui.label(
                Rect::new(inner.x, inner.y + 32, inner.w, 22),
                &realm,
                Font::Regular,
                t.font_size,
                t.text_dim,
                Align::Left,
            );
        }
        ui.label(
            Rect::new(inner.x, inner.y + 62, inner.w, 20),
            "User name",
            Font::Regular,
            t.small_size,
            t.text_dim,
            Align::Left,
        );
        ui.text_input(Rect::new(inner.x, inner.y + 84, inner.w, 34), "auth-user", &mut user, "");
        ui.label(
            Rect::new(inner.x, inner.y + 126, inner.w, 20),
            "Password",
            Font::Regular,
            t.small_size,
            t.text_dim,
            Align::Left,
        );
        ui.password_input(Rect::new(inner.x, inner.y + 148, inner.w, 34), "auth-password", &mut password, "", false);
        if ui.state.focus.is_none() {
            ui.focus_text_input("auth-user");
        }
        buttons(ui, card, "Sign in", Some("Cancel"))
    });
    match answer {
        Some(true) => {
            b.engine.auth_reply(request, Some((&user, &password)));
            None
        }
        Some(false) => {
            b.engine.auth_reply(request, None);
            None
        }
        None => Some(Modal::Auth { request, host, realm, proxy, user, password }),
    }
}

/// A list of pages to pick from (history, bookmarks), with a box that
/// narrows it; the page picked, and the one to forget, if any.
struct Picked {
    open: Option<String>,
    forget: Option<String>,
    close: bool,
}

fn page_list(ui: &mut Ui, title: &str, query: &mut String, rows: &[(String, String, String)], empty: &str) -> Picked {
    let t = ui.theme().clone();
    let mut picked = Picked { open: None, forget: None, close: false };
    let (w, h) = ((ui.width - 80).min(720), (ui.height - 80).min(560));
    ui.modal(w, h, |ui, card| {
        let inner = card.inset(20, 16, 20, 16);
        ui.heading(Rect::new(inner.x, inner.y, inner.w - 40, 28), title);
        if ui.icon_button(Rect::new(inner.right() - 30, inner.y, 30, 30), Icon::Close, "Close (Esc)") {
            picked.close = true;
        }
        ui.text_input(Rect::new(inner.x, inner.y + 40, inner.w, 34), "list-query", query, "Search");
        if ui.state.focus.is_none() {
            ui.focus_text_input("list-query");
        }
        let list = Rect::new(inner.x, inner.y + 84, inner.w, inner.h - 84);
        if rows.is_empty() {
            ui.label(list.inset(0, 20, 0, 0), empty, Font::Regular, t.font_size, t.text_dim, Align::Left);
            if ui.input.key(code::ESC) {
                picked.close = true;
            }
            return;
        }
        let mut selected = None;
        let resp = ui.list(list, "pages", rows.len(), 48, &mut selected, |ui, i, r, state| {
            ui.row_background(r, state);
            let (address, name, when) = &rows[i];
            let name = fit(ui, if name.is_empty() { address } else { name }, Font::Regular, t.font_size, r.w - 170);
            ui.label(
                Rect::new(r.x + 12, r.y + 4, r.w - 170, 22),
                &name,
                Font::Regular,
                t.font_size,
                t.text,
                Align::Left,
            );
            let shown = fit(ui, &url::display(address), Font::Regular, t.small_size, r.w - 170);
            ui.label(
                Rect::new(r.x + 12, r.y + 24, r.w - 170, 20),
                &shown,
                Font::Regular,
                t.small_size,
                t.text_dim,
                Align::Left,
            );
            ui.label(
                Rect::new(r.right() - 150, r.y, 110, r.h),
                when,
                Font::Regular,
                t.small_size,
                t.text_faint,
                Align::Right,
            );
        });
        if let Some(i) = resp.clicked.or(resp.activated) {
            picked.open = Some(rows[i].0.clone());
        }
        if let Some((i, _, _)) = resp.context {
            picked.forget = Some(rows[i].0.clone());
        }
        if ui.input.key(code::ESC) {
            picked.close = true;
        }
    });
    picked
}

fn history(b: &mut Browser, ui: &mut Ui, mut query: String) -> Option<Modal> {
    let now = rt::time::unix_time_ns() / 1_000_000_000;
    let q = query.trim().to_lowercase();
    let rows: Vec<(String, String, String)> = b
        .store
        .history
        .entries()
        .iter()
        .filter(|e| q.is_empty() || e.url.to_lowercase().contains(&q) || e.title.to_lowercase().contains(&q))
        .take(500)
        .map(|e| (e.url.clone(), e.title.clone(), browser_core::history::ago(e.last, now)))
        .collect();
    let picked =
        page_list(ui, "History", &mut query, &rows, "No pages visited yet (right-click a page here to forget it).");
    if let Some(address) = picked.forget {
        b.store.history.remove(&address);
        b.store.changed(Kept::History, Browser::now());
    }
    if let Some(address) = picked.open {
        b.navigate(&address);
        return None;
    }
    (!picked.close).then_some(Modal::History { query })
}

fn bookmarks(b: &mut Browser, ui: &mut Ui, mut query: String) -> Option<Modal> {
    let q = query.trim().to_lowercase();
    let rows: Vec<(String, String, String)> = b
        .store
        .bookmarks
        .list()
        .iter()
        .rev()
        .filter(|e| q.is_empty() || e.url.to_lowercase().contains(&q) || e.title.to_lowercase().contains(&q))
        .map(|e| (e.url.clone(), e.title.clone(), String::new()))
        .collect();
    let picked =
        page_list(ui, "Bookmarks", &mut query, &rows, "No bookmarks yet: the star in the address bar keeps a page.");
    if let Some(address) = picked.forget {
        b.store.bookmarks.remove(&address);
        b.store.changed(Kept::Bookmarks, Browser::now());
    }
    if let Some(address) = picked.open {
        b.navigate(&address);
        return None;
    }
    (!picked.close).then_some(Modal::Bookmarks { query })
}

fn settings(b: &mut Browser, ui: &mut Ui, mut form: Box<SettingsForm>) -> Option<Modal> {
    let t = ui.theme().clone();
    let (w, h) = ((ui.width - 80).min(620), (ui.height - 60).min(640));
    let mut done = None;
    ui.modal(w, h, |ui, card| {
        let inner = card.inset(24, 18, 24, 18);
        let mut y = inner.y;
        ui.heading(Rect::new(inner.x, y, inner.w, 28), "Settings");
        y += 42;
        let label = |ui: &mut Ui, y: i32, text: &str| {
            ui.label(Rect::new(inner.x, y, inner.w, 22), text, Font::Bold, t.font_size, t.text, Align::Left);
        };
        label(ui, y, "Search with");
        y += 28;
        let names: Vec<&str> = ENGINES.iter().map(|e| e.name).collect();
        let row: Vec<&str> = names.iter().take(4).copied().collect();
        let mut chosen = if form.engine < 4 { form.engine } else { usize::MAX };
        if ui.tabs(Rect::new(inner.x, y, inner.w, 32), &row, &mut chosen) {
            form.engine = chosen;
        }
        y += 38;
        let more: Vec<&str> = names.iter().skip(4).copied().collect();
        let mut chosen = if form.engine >= 4 { form.engine - 4 } else { usize::MAX };
        if ui.tabs(Rect::new(inner.x, y, inner.w * more.len() as i32 / 4, 32), &more, &mut chosen) {
            form.engine = 4 + chosen;
        }
        y += 48;
        label(ui, y, "Home page");
        y += 28;
        let mut which = usize::from(form.home_page);
        if ui.tabs(Rect::new(inner.x, y, 260, 32), &["New tab page", "A page of mine"], &mut which) {
            form.home_page = which == 1;
        }
        if form.home_page {
            ui.text_input(Rect::new(inner.x + 272, y, inner.w - 272, 32), "home", &mut form.home, "https://...");
        }
        y += 46;
        ui.checkbox(
            Rect::new(inner.x, y, inner.w, 28),
            "Open the tabs I had open, as the browser starts",
            &mut form.restore,
        );
        y += 34;
        ui.checkbox(
            Rect::new(inner.x, y, inner.w, 28),
            "Refuse cookies from sites other than the page's (they follow you from site to site)",
            &mut form.block_third_party_cookies,
        );
        y += 34;
        ui.checkbox(
            Rect::new(inner.x, y, inner.w, 28),
            "Use hardware acceleration when available: smoother scrolling and animations",
            &mut form.acceleration,
        );
        y += 42;
        label(ui, y, &format!("Pages' zoom: {}%", ZOOMS[form.zoom]));
        y += 28;
        let mut z = form.zoom as f32;
        if ui.slider(Rect::new(inner.x, y, inner.w, 28), "zoom", &mut z, 0.0, (ZOOMS.len() - 1) as f32) {
            form.zoom = z.round() as usize;
        }
        y += 44;
        label(ui, y, "Clear what was kept");
        y += 28;
        ui.checkbox(Rect::new(inner.x, y, inner.w / 2, 28), "History", &mut form.clear_history);
        ui.checkbox(
            Rect::new(inner.x + inner.w / 2, y, inner.w / 2, 28),
            "Cookies and site data",
            &mut form.clear_cookies,
        );
        y += 32;
        ui.checkbox(Rect::new(inner.x, y, inner.w / 2, 28), "Cached pages and pictures", &mut form.clear_cache);
        let any = form.clear_history || form.clear_cookies || form.clear_cache;
        if any && ui.button_full(Rect::new(inner.right() - 120, y - 2, 120, 32), None, "Clear now", ButtonKind::Danger)
        {
            let mut what = 0;
            if form.clear_cookies {
                what |= engine::data::COOKIES | engine::data::STORAGE;
            }
            if form.clear_cache {
                what |= engine::data::CACHE;
            }
            b.clear(form.clear_history, what);
            form.clear_history = false;
            form.clear_cookies = false;
            form.clear_cache = false;
        }
        done = buttons(ui, card, "Save", Some("Cancel"));
    });
    match done {
        Some(true) => {
            let s = &mut b.store.settings;
            s.search = ENGINES[form.engine.min(ENGINES.len() - 1)].id.to_string();
            s.home = if form.home_page && !form.home.trim().is_empty() {
                Home::Page(browser_core::address::resolve(&form.home, s.engine()).unwrap_or_else(|| form.home.clone()))
            } else {
                Home::NewTab
            };
            s.restore = form.restore;
            let restart =
                s.block_third_party_cookies != form.block_third_party_cookies || s.acceleration != form.acceleration;
            s.block_third_party_cookies = form.block_third_party_cookies;
            s.acceleration = form.acceleration;
            s.zoom = ZOOMS[form.zoom];
            b.store.changed(Kept::Settings, Browser::now());
            if restart {
                b.note("The change applies from the browser's next start");
            }
            None
        }
        Some(false) => None,
        None => Some(Modal::Settings(form)),
    }
}

fn about(ui: &mut Ui) -> Option<Modal> {
    let t = ui.theme().clone();
    let closed = ui.modal(500, 300, |ui, card| {
        let inner = card.inset(24, 20, 24, 20);
        ui.heading(Rect::new(inner.x, inner.y, inner.w, 28), "Browser");
        ui.label(
            Rect::new(inner.x, inner.y + 36, inner.w, 22),
            &format!("Version {} — WPE WebKit 2.54.1", env!("CARGO_PKG_VERSION")),
            Font::Regular,
            t.font_size,
            t.text_dim,
            Align::Left,
        );
        let text = "Veda's web browser. Its engine is WPE WebKit, with the libraries it stands on (GLib, \
                    libsoup, ICU, HarfBuzz, FreeType, Skia, OpenSSL and others), each under its own licence: \
                    their texts are in /system/licences/web.";
        ui.paragraph(Rect::new(inner.x, inner.y + 72, inner.w, 120), text, t.font_size, t.text_dim);
        buttons(ui, card, "Close", None).is_some()
    });
    (!closed).then_some(Modal::About)
}

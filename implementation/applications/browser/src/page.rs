// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The page: the frame WebKit drew of the shown tab's view, or the
//! browser's own page in its place — the new tab's (the sites visited
//! most), a load's failure, a certificate's warning, a page whose process
//! ended — and over it the find bar, the link under the pointer, a passing
//! note, and the page's menus (a `<select>`'s options, its context menu).

use browser_core::url;
use gfx::{Align, Color, Picture, Rect};
use proto::input::keys as code;
use ui::{ButtonKind, Font, Icon, MenuItem, Ui};

use crate::browser::{Browser, Focus};
use crate::chrome::fit;
use crate::engine::{Crash, Failure, tls};
use crate::tab::Problem;

/// A page's own background, before it has drawn.
const PAPER: Color = Color::rgb(0xff, 0xff, 0xff);

pub fn draw(b: &mut Browser, ui: &mut Ui) {
    let area = b.page_rect;
    let Some(tab) = b.current() else { return };
    let (problem, is_new, view, cursor) = (tab.problem.clone(), tab.is_new(), tab.view, tab.cursor);
    match (problem, is_new, view) {
        (Some(problem), _, _) => problem_page(b, ui, area, &problem),
        (None, true, _) => new_tab_page(b, ui, area),
        (None, false, Some(view)) => {
            frame(b, ui, area, view);
            if b.pointer.is_some_and(|(x, y)| area.contains(x, y)) && b.modal.is_none() && b.page_menu.is_none() {
                ui.set_cursor(cursor);
            }
        }
        (None, false, None) => ui.canvas.fill_rect(area, PAPER),
    }
    find_bar(b, ui, area);
    hovered_link(b, ui, area);
    toast(b, ui, area);
    crate::chrome::overlays(b, ui);
    page_menu(b, ui);
}

/// The view's frame in the page's area, as a picture the window keeps
/// ([`Ui::retained`]): what WebKit drew again, and what was drawn over it
/// (the find bar, a menu), is all that is copied and presented of it. What
/// it does not cover (its page catching up with a new size) is blank.
fn frame(b: &mut Browser, ui: &mut Ui, area: Rect, view: crate::engine::ViewId) {
    let Some(mut frame) = b.engine.frame(view) else {
        ui.canvas.fill_rect(area, PAPER);
        return;
    };
    let (cw, ch) = (ui.canvas.width(), ui.canvas.height());
    let area = area.intersect(&Rect::new(0, 0, cw, ch));
    let (w, h) = (frame.width().min(area.w).max(0), frame.height().min(area.h).max(0));
    let stride = frame.width().max(0) as usize;
    if let Some(picture) = Picture::new(frame.pixels(), w, h, stride).filter(|_| w > 0 && h > 0) {
        ui.retained(view.number(), area.x, area.y, &picture, &[frame.changed()]);
    }
    if w < area.w {
        ui.canvas.fill_rect(Rect::new(area.x + w, area.y, area.w - w, h), PAPER);
    }
    if h < area.h {
        ui.canvas.fill_rect(Rect::new(area.x, area.y + h, area.w, area.h - h), PAPER);
    }
    frame.presented();
}

/// A tile's colour for a site: one of a few, the same for a host always.
fn tile_color(host: &str) -> Color {
    const COLORS: [Color; 8] = [
        Color::rgb(0x3b, 0x82, 0xf6),
        Color::rgb(0x10, 0xb9, 0x81),
        Color::rgb(0xf5, 0x9e, 0x0b),
        Color::rgb(0xef, 0x44, 0x44),
        Color::rgb(0x8b, 0x5c, 0xf6),
        Color::rgb(0x06, 0xb6, 0xd4),
        Color::rgb(0xec, 0x48, 0x99),
        Color::rgb(0x84, 0xcc, 0x16),
    ];
    let h = host.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    COLORS[(h % COLORS.len() as u32) as usize]
}

fn new_tab_page(b: &mut Browser, ui: &mut Ui, area: Rect) {
    let t = ui.theme().clone();
    ui.canvas.fill_rect(area, t.bg);
    let private = b.current().is_some_and(|t| t.private);
    let column = Rect::new(area.x + (area.w - area.w.min(760)) / 2, area.y, area.w.min(760), area.h);
    let mut y = area.y + (area.h / 6).clamp(24, 120);
    if private {
        ui.icon(Rect::new(column.x + column.w / 2 - 24, y, 48, 48), Icon::Moon, 40.0, Color::rgb(0xa0, 0x70, 0xe0));
        y += 64;
        ui.label(Rect::new(column.x, y, column.w, 36), "Private tab", Font::Bold, t.title_size, t.text, Align::Center);
        y += 48;
        let text = "Pages you visit in private tabs are not kept in your history, and their cookies and \
                    site data are forgotten when the browser closes. Downloads and bookmarks stay.";
        ui.paragraph(Rect::new(column.x + 40, y, column.w - 80, 80), text, t.font_size, t.text_dim);
        return;
    }
    ui.label(Rect::new(column.x, y, column.w, 40), "Where to?", Font::Bold, t.title_size, t.text, Align::Center);
    y += 52;
    let engine = b.store.settings.engine().name;
    ui.label(
        Rect::new(column.x, y, column.w, 24),
        &format!("Type an address, or words to search {engine} for, in the bar above."),
        Font::Regular,
        t.font_size,
        t.text_dim,
        Align::Center,
    );
    y += 56;
    let now = rt::time::unix_time_ns() / 1_000_000_000;
    let sites: Vec<(String, String)> =
        b.store.history.top(now, 8).iter().map(|e| (e.url.clone(), e.title.clone())).collect();
    if sites.is_empty() {
        ui.label(
            Rect::new(column.x, y, column.w, 24),
            "The sites you visit most will appear here.",
            Font::Regular,
            t.small_size,
            t.text_faint,
            Align::Center,
        );
    }
    let (tile_w, tile_h, gap) = (168, 112, 16);
    let per_row = ((column.w + gap) / (tile_w + gap)).clamp(1, 4);
    let row_w = per_row * (tile_w + gap) - gap;
    let mut go = None;
    for (i, (address, title)) in sites.iter().enumerate() {
        let (col, row) = (i as i32 % per_row, i as i32 / per_row);
        let r = Rect::new(
            column.x + (column.w - row_w) / 2 + col * (tile_w + gap),
            y + row * (tile_h + gap),
            tile_w,
            tile_h,
        );
        let id = ui.id(&format!("site-{i}"));
        let resp = ui.interact(id, r);
        let hover = ui.animate(id, if resp.hovered { 1.0 } else { 0.0 }, 10.0);
        ui.canvas.fill_rounded_rect(r, t.radius_large, t.surface.lerp(t.control_hover, hover));
        let host = url::short(address);
        let circle = Rect::new(r.x + (r.w - 44) / 2, r.y + 16, 44, 44);
        ui.canvas.fill_circle(circle.x as f32 + 22.0, circle.y as f32 + 22.0, 22.0, tile_color(&host));
        let letter = host.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
        ui.label(circle, &letter, Font::Bold, 20.0, Color::rgb(0xff, 0xff, 0xff), Align::Center);
        let name = if title.is_empty() { host.clone() } else { title.clone() };
        let name = fit(ui, &name, Font::Regular, t.small_size, r.w - 16);
        ui.label(Rect::new(r.x + 8, r.y + 70, r.w - 16, 22), &name, Font::Regular, t.small_size, t.text, Align::Center);
        let host = fit(ui, &host, Font::Regular, t.small_size - 1.0, r.w - 16);
        ui.label(
            Rect::new(r.x + 8, r.y + 88, r.w - 16, 18),
            &host,
            Font::Regular,
            t.small_size - 1.0,
            t.text_dim,
            Align::Center,
        );
        if resp.clicked {
            go = Some(address.clone());
        }
    }
    if let Some(address) = go {
        b.navigate(&address);
    }
}

/// What the browser shows in a page's place when it has none to show.
fn problem_page(b: &mut Browser, ui: &mut Ui, area: Rect, problem: &Problem) {
    let t = ui.theme().clone();
    ui.canvas.fill_rect(area, t.bg);
    let card =
        Rect::new(area.x + (area.w - area.w.min(620)) / 2, area.y + (area.h / 6).clamp(24, 110), area.w.min(620), 300);
    let x = card.x + 24;
    let w = card.w - 48;
    let mut y = card.y;
    let (icon, color, title, body): (Icon, Color, String, String) = match problem {
        Problem::Failed { uri, message, failure } => {
            let host = url::host(uri).unwrap_or(uri).to_string();
            match failure {
                Failure::Host => (
                    Icon::Search,
                    t.text_dim,
                    "This site can't be found".into(),
                    format!("No server has the name {host}. Check its spelling, or the network."),
                ),
                Failure::Network => (
                    Icon::Warning,
                    t.warning,
                    "This page could not be reached".into(),
                    format!("{host} did not answer: {message}"),
                ),
                _ => (Icon::Warning, t.warning, "This page could not be loaded".into(), message.clone()),
            }
        }
        Problem::Untrusted { host, errors, .. } => {
            let mut why = Vec::new();
            if errors & tls::UNKNOWN_CA != 0 {
                why.push("its issuer is not one this system trusts");
            }
            if errors & tls::BAD_IDENTITY != 0 {
                why.push("it is for another site");
            }
            if errors & tls::EXPIRED != 0 {
                why.push("it has expired");
            }
            if errors & tls::NOT_ACTIVATED != 0 {
                why.push("it is not valid yet");
            }
            if errors & tls::REVOKED != 0 {
                why.push("it was revoked");
            }
            if errors & tls::INSECURE != 0 {
                why.push("its encryption is too weak");
            }
            let why = if why.is_empty() { "it could not be checked".to_string() } else { why.join(", ") };
            (
                Icon::Lock,
                t.danger,
                "This connection is not private".into(),
                format!(
                    "{host}'s certificate cannot be trusted: {why}. Someone may be pretending to be {host} \
                     to read what you send it, such as passwords or card numbers."
                ),
            )
        }
        Problem::Crashed(crash) => (
            Icon::Warning,
            t.warning,
            match crash {
                Crash::Memory => "This page used too much memory".into(),
                _ => "This page stopped working".into(),
            },
            "Reload it to try again.".into(),
        ),
    };
    ui.icon(Rect::new(x, y, 48, 48), icon, 40.0, color);
    y += 64;
    ui.label(Rect::new(x, y, w, 34), &title, Font::Bold, t.title_size, t.text, Align::Left);
    y += 44;
    let used = ui.paragraph(Rect::new(x, y, w, 120), &body, t.font_size, t.text_dim);
    y += used.max(24) + 24;
    match problem {
        Problem::Untrusted { request, host, .. } => {
            let request = *request;
            if ui.button_full(Rect::new(x, y, 160, 38), None, "Back to safety", ButtonKind::Primary) {
                b.engine.tls_reply(request, false);
                if let Some(tab) = b.tabs.current_mut() {
                    tab.problem = None;
                }
                if b.current().is_some_and(|t| t.can_back) {
                    b.back();
                } else {
                    b.home();
                }
            }
            let label = format!("Go on to {host} anyway");
            let lw = ui.measure(&label, Font::Regular, t.font_size) as i32 + 32;
            if ui.button_full(Rect::new(x + 176, y, lw, 38), None, &label, ButtonKind::Danger) {
                b.engine.tls_reply(request, true);
                if let Some(tab) = b.tabs.current_mut() {
                    tab.problem = None;
                    tab.loading = true;
                }
            }
        }
        Problem::Failed { .. } | Problem::Crashed(_) => {
            let label = if matches!(problem, Problem::Crashed(_)) { "Reload" } else { "Try again" };
            if ui.button_full(Rect::new(x, y, 130, 38), Some(Icon::Refresh), label, ButtonKind::Primary) {
                b.reload(false);
            }
            // What was typed without a scheme, taken for a host, may have
            // been words.
            let typed = b.current().and_then(|t| t.typed.clone());
            if let (Problem::Failed { failure: Failure::Host, .. }, Some(typed)) = (problem, typed)
                && !typed.contains("://")
            {
                let engine = b.store.settings.engine();
                let label = format!("Search {} for \u{201c}{typed}\u{201d}", engine.name);
                let lw = (ui.measure(&label, Font::Regular, t.font_size) as i32 + 32).min(w - 146);
                if ui.button_full(Rect::new(x + 146, y, lw, 38), Some(Icon::Search), &label, ButtonKind::Secondary) {
                    let target = engine.results(&typed);
                    let i = b.tabs.shown();
                    b.go(i, &target);
                }
            }
        }
    }
}

fn find_bar(b: &mut Browser, ui: &mut Ui, area: Rect) {
    let Some((mut text, case)) = b.find.clone() else { return };
    let t = ui.theme().clone();
    let bar = Rect::new(area.right() - 392, area.y + 8, 380, 42);
    ui.cover(bar);
    b.overlays.push(bar);
    ui.canvas.draw_shadow(bar.translate(0, 3), 8, 12, Color::rgba(0, 0, 0, 110));
    ui.canvas.fill_rounded_rect(bar, t.radius_large, t.surface);
    ui.canvas.stroke_rounded_rect(bar, t.radius_large, 1.0, t.border_strong);
    let input = Rect::new(bar.x + 8, bar.y + 6, 200, 30);
    let resp = ui.text_input(input, "find", &mut text, "Find in page");
    if resp.focused {
        b.focus = Focus::Find;
    }
    if resp.changed {
        b.find(&text, case);
    }
    if resp.submitted {
        b.find_next(ui.input.shift());
    }
    let found = b.current().and_then(|t| t.found);
    let count = match found {
        _ if text.is_empty() => String::new(),
        Some(0) => "No matches".into(),
        Some(n) => format!("{n} found"),
        None => String::new(),
    };
    let color = if found == Some(0) { t.danger } else { t.text_dim };
    ui.label(Rect::new(bar.x + 214, bar.y, 70, bar.h), &count, Font::Regular, t.small_size, color, Align::Left);
    if ui.icon_button(Rect::new(bar.x + 282, bar.y + 6, 30, 30), Icon::ChevronUp, "Previous (Shift+F3)") {
        b.find_next(true);
    }
    if ui.icon_button(Rect::new(bar.x + 314, bar.y + 6, 30, 30), Icon::ChevronDown, "Next (F3)") {
        b.find_next(false);
    }
    if ui.icon_button(Rect::new(bar.x + 346, bar.y + 6, 30, 30), Icon::Close, "Close (Esc)") {
        b.close_find();
        return;
    }
    if let Some((stored, _)) = &mut b.find {
        *stored = text;
    }
}

/// Where the link under the pointer goes, at the page's foot.
fn hovered_link(b: &mut Browser, ui: &mut Ui, area: Rect) {
    let Some(link) = b.current().and_then(|t| t.hovered.clone()) else { return };
    if !b.pointer.is_some_and(|(x, y)| area.contains(x, y)) {
        return;
    }
    let t = ui.theme().clone();
    let text = fit(ui, &url::display(&link), Font::Regular, t.small_size, area.w / 2);
    let w = ui.measure(&text, Font::Regular, t.small_size) as i32 + 20;
    let r = Rect::new(area.x + 4, area.bottom() - 28, w, 24);
    ui.canvas.fill_rounded_rect(r, t.radius, t.surface.with_alpha(240));
    ui.canvas.stroke_rounded_rect(r, t.radius, 1.0, t.border);
    ui.label(r.inset(10, 0, 10, 0), &text, Font::Regular, t.small_size, t.text_dim, Align::Left);
}

fn toast(b: &mut Browser, ui: &mut Ui, area: Rect) {
    let Some((text, _)) = &b.toast else { return };
    let t = ui.theme().clone();
    let text = fit(ui, text, Font::Regular, t.font_size, area.w - 80);
    let w = ui.measure(&text, Font::Regular, t.font_size) as i32 + 36;
    let r = Rect::new(area.x + (area.w - w) / 2, area.bottom() - 64, w, 38);
    ui.canvas.draw_shadow(r.translate(0, 3), 8, 12, Color::rgba(0, 0, 0, 110));
    ui.canvas.fill_rounded_rect(r, r.h as f32 / 2.0, t.control);
    ui.label(r, &text, Font::Regular, t.font_size, t.text, Align::Center);
}

/// A label of WebKit's, without the mnemonics GTK marks with `_`.
fn plain(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            if chars.peek() == Some(&'_') {
                out.push('_');
                chars.next();
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// The page's menu, while it is open: the window's context menu, which the
/// engine hears the choice of (or that there was none).
fn page_menu(b: &mut Browser, ui: &mut Ui) {
    let Some(menu) = &mut b.page_menu else { return };
    const ID: &str = "page-menu";
    if !menu.opened {
        ui.open_context_menu(ID, menu.at.0, menu.at.1);
        menu.opened = true;
    }
    let items: Vec<MenuItem> = menu
        .items
        .iter()
        .map(|item| {
            if item.separator {
                return MenuItem::separator();
            }
            let label = if item.group_child { format!("    {}", plain(&item.label)) } else { plain(&item.label) };
            let mut m = MenuItem::new(&label).enabled(item.enabled && !item.group_label);
            if menu.options {
                m = m.checked(item.selected);
            }
            m
        })
        .collect();
    let request = menu.request;
    let chosen = ui.context_menu(ID, &items);
    // Esc closes it too.
    let escaped = ui.input.key(code::ESC);
    if let Some(i) = chosen {
        b.engine.menu_reply(request, Some(i));
        b.page_menu = None;
    } else if ui.state.context_menu.is_none() || escaped {
        b.engine.menu_reply(request, None);
        b.page_menu = None;
        ui.state.context_menu = None;
    }
}

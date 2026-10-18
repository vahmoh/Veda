// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The browser's own controls above the page: the tabs, and the toolbar
//! (back, forward, reload, home, the address bar with its padlock and
//! star, downloads, the menu); and what opens over the page from them (the
//! address bar's suggestions, the downloads, the menu).

use browser_core::downloads;
use browser_core::tabs::tab_width;
use browser_core::url;
use gfx::{Align, Color, Rect};
use proto::input::keys as code;
use ui::{Font, Icon, MenuItem, Ui};

use crate::browser::{Browser, Focus};
use crate::dialogs::Modal;
use crate::engine::Security;
use crate::tab::Tab;

/// The tab strip's height.
pub const STRIP: i32 = 38;
/// The toolbar's.
pub const TOOLBAR: i32 = 46;
const NARROWEST_TAB: i32 = 72;
const WIDEST_TAB: i32 = 236;

/// `text` cut to `width` pixels with an ellipsis.
pub fn fit(ui: &Ui, text: &str, font: Font, size: f32, width: i32) -> String {
    let f = ui.ctx.font(font);
    ui.ctx.text.ellipsize(f, size, text, width.max(0) as f32)
}

pub fn draw(b: &mut Browser, ui: &mut Ui) {
    let r = ui.rect();
    b.overlays.clear();
    if b.fullscreen {
        b.page_rect = r;
        return;
    }
    let t = ui.theme().clone();
    ui.canvas.fill_rect(Rect::new(0, 0, r.w, STRIP), t.bg);
    tab_strip(b, ui, Rect::new(0, 0, r.w, STRIP));
    let bar = Rect::new(0, STRIP, r.w, TOOLBAR);
    ui.canvas.fill_rect(bar, t.surface);
    toolbar(b, ui, bar);
    ui.canvas.fill_rect(Rect::new(0, STRIP + TOOLBAR - 1, r.w, 1), t.border);
    // A load's progress, along the toolbar's foot.
    if let Some(tab) = b.current().filter(|t| t.loading) {
        let w = (f64::from(r.w) * tab.progress.clamp(0.05, 1.0)) as i32;
        ui.canvas.fill_rect(Rect::new(0, STRIP + TOOLBAR - 2, w, 2), t.accent);
    }
    b.page_rect = Rect::new(0, STRIP + TOOLBAR, r.w, (r.h - STRIP - TOOLBAR).max(1));
}

fn tab_strip(b: &mut Browser, ui: &mut Ui, strip: Rect) {
    let t = ui.theme().clone();
    let n = b.tabs.len();
    let width = tab_width(n, strip.w - 56, NARROWEST_TAB, WIDEST_TAB);
    // As many as fit; the shown one always among them.
    let fits = ((strip.w - 56) / width.max(1)).max(1) as usize;
    let first = if b.tabs.shown() >= fits { b.tabs.shown() + 1 - fits } else { 0 };
    let mut x = 8;
    let mut close = None;
    let mut show = None;
    for i in first..n.min(first + fits) {
        let Some(tab) = b.tabs.get(i) else { break };
        let r = Rect::new(x, 6, width - 4, STRIP - 6);
        let shown = i == b.tabs.shown();
        let id = ui.id(&format!("tab-{}", tab.id));
        let resp = ui.interact(id, r);
        let hover = ui.animate(id, if resp.hovered { 1.0 } else { 0.0 }, 10.0);
        let bg = if shown {
            t.surface
        } else if tab.private {
            t.bg.lerp(Color::rgb(0x6a, 0x3d, 0x9a), 0.25 + 0.15 * hover)
        } else {
            t.bg.lerp(t.control_hover, hover)
        };
        ui.canvas.fill_rounded_rect(Rect::new(r.x, r.y, r.w, r.h + 8), t.radius, bg);
        if tab.private && shown {
            ui.canvas.fill_rect(Rect::new(r.x + 6, r.y, r.w - 12, 2), Color::rgb(0xa0, 0x70, 0xe0));
        }
        // Its icon, a spinner while it loads.
        let icon = Rect::new(r.x + 10, r.y + (r.h - 16) / 2, 16, 16);
        if tab.loading {
            ui.spinner(icon.x + 8, icon.y + 8, 6.0);
        } else if let Some(bitmap) = &tab.icon {
            ui.canvas.draw_bitmap(bitmap, icon.x, icon.y, 255);
        } else {
            let glyph = if tab.private {
                Icon::Moon
            } else if tab.is_new() {
                Icon::Star
            } else {
                Icon::Document
            };
            ui.icon(icon, glyph, 14.0, t.text_dim);
        }
        let close_rect = Rect::new(r.right() - 26, r.y + (r.h - 20) / 2, 20, 20);
        let text_w = r.w - 44 - if shown || resp.hovered { 22 } else { 0 };
        let name = fit(ui, &tab.name(), Font::Regular, t.small_size + 1.0, text_w);
        let color = if shown { t.text } else { t.text_dim };
        ui.label(Rect::new(r.x + 32, r.y, text_w, r.h), &name, Font::Regular, t.small_size + 1.0, color, Align::Left);
        if (shown || resp.hovered) && ui.icon_button(close_rect, Icon::Close, "Close tab (Ctrl+W)") {
            close = Some(i);
        } else if resp.clicked {
            show = Some(i);
        }
        // The middle button closes it.
        if resp.hovered && ui.input.pressed[2] {
            close = Some(i);
        }
        x += width;
    }
    let plus = Rect::new(x + 2, 8, 30, 26);
    if ui.icon_button(plus, Icon::Plus, "New tab (Ctrl+T)") {
        b.new_tab(false);
        ui.focus_text_input("address");
    }
    if let Some(i) = close {
        b.close_tab(i);
    } else if let Some(i) = show {
        b.show_tab(i);
    }
}

fn toolbar(b: &mut Browser, ui: &mut Ui, bar: Rect) {
    let t = ui.theme().clone();
    let y = bar.y + (bar.h - 32) / 2;
    let button = |x: i32| Rect::new(x, y, 32, 32);
    let (can_back, can_forward, loading) =
        b.current().map_or((false, false, false), |tab| (tab.can_back, tab.can_forward, tab.loading));
    let mut x = 8;
    if can_back && ui.icon_button(button(x), Icon::ChevronLeft, "Back (Alt+Left)") {
        b.back();
    } else if !can_back {
        ui.icon(button(x), Icon::ChevronLeft, 18.0, t.text_faint);
    }
    x += 34;
    if can_forward && ui.icon_button(button(x), Icon::ChevronRight, "Forward (Alt+Right)") {
        b.forward();
    } else if !can_forward {
        ui.icon(button(x), Icon::ChevronRight, 18.0, t.text_faint);
    }
    x += 34;
    if loading {
        if ui.icon_button(button(x), Icon::Close, "Stop (Esc)") {
            b.stop();
        }
    } else if ui.icon_button(button(x), Icon::Refresh, "Reload (Ctrl+R)") {
        b.reload(false);
    }
    x += 34;
    if ui.icon_button(button(x), Icon::Home, "Home (Alt+Home)") {
        b.home();
        ui.focus_text_input("address");
    }
    x += 42;
    // The menu and the downloads, at the right.
    let menu = Rect::new(bar.right() - 40, y, 32, 32);
    let downloads_button = Rect::new(menu.x - 36, y, 32, 32);
    let field = Rect::new(x, bar.y + 7, (downloads_button.x - 10 - x).max(120), bar.h - 14);
    address_bar(b, ui, field);
    let active = b.downloads.iter().filter(|d| d.end.is_none()).count();
    if ui.icon_button(downloads_button, Icon::Download, "Downloads (Ctrl+J)") {
        b.downloads_open = !b.downloads_open;
    }
    if active > 0 {
        let fraction: f64 =
            b.downloads.iter().filter(|d| d.end.is_none()).map(|d| d.fraction).sum::<f64>() / active as f64;
        let w = ((downloads_button.w - 8) as f64 * fraction) as i32;
        ui.canvas.fill_rect(Rect::new(downloads_button.x + 4, downloads_button.bottom() - 3, w.max(2), 2), t.accent);
    }
    if ui.icon_button(menu, Icon::List, "Menu") {
        ui.open_context_menu("main-menu", menu.x - 220, menu.bottom() + 4);
    }
}

fn address_bar(b: &mut Browser, ui: &mut Ui, field: Rect) {
    let t = ui.theme().clone();
    let tab = b.current();
    let security = tab.map_or(Security::Local, |t| t.security);
    let mixed = tab.is_some_and(|t| t.mixed_content);
    let is_new = tab.is_none_or(Tab::is_new);
    let bookmarked = tab.is_some_and(|t| !t.is_new() && b.store.bookmarks.contains(&t.url));
    let private = tab.is_some_and(|t| t.private);
    ui.canvas.fill_rounded_rect(field, field.h as f32 / 2.0, t.control);
    if b.focus == Focus::Address {
        ui.canvas.stroke_rounded_rect(field, field.h as f32 / 2.0, 1.5, t.accent);
    }
    // What the page's security is.
    let mark = Rect::new(field.x + 10, field.y + (field.h - 18) / 2, 18, 18);
    let (icon, color, tip) = match (security, mixed, is_new) {
        (_, _, true) => (if private { Icon::Moon } else { Icon::Search }, t.text_dim, ""),
        (Security::Secure, false, _) => (Icon::Lock, t.success, "The connection is encrypted"),
        (Security::Secure, true, _) => (Icon::Warning, t.warning, "Parts of this page are not encrypted"),
        (Security::Broken, _, _) => (Icon::Warning, t.danger, "The site's certificate is not trusted"),
        (Security::Insecure, _, _) => (Icon::Info, t.warning, "The connection is not encrypted"),
        (Security::Local, _, _) => (Icon::Info, t.text_dim, ""),
    };
    ui.icon(mark, icon, 15.0, color);
    if !tip.is_empty() && ui.hovered(mark) {
        let id = ui.id("security");
        ui.tooltip(id, mark, tip);
    }
    let star = Rect::new(field.right() - 34, field.y + (field.h - 26) / 2, 26, 26);
    let input = Rect::new(field.x + 34, field.y, star.x - field.x - 38, field.h);
    let placeholder = if private { "Search privately, or enter an address" } else { "Search, or enter an address" };
    // Keys that move among the suggestions, before the text box sees them.
    if b.focus == Focus::Address && !b.address.suggestions.is_empty() {
        let n = b.address.suggestions.len();
        if ui.input.key(code::DOWN) {
            b.address.selected = Some(b.address.selected.map_or(0, |s| (s + 1) % n));
        }
        if ui.input.key(code::UP) {
            b.address.selected = Some(b.address.selected.map_or(n - 1, |s| (s + n - 1) % n));
        }
    }
    // The browser's focus is the text box's, however it came there (a
    // shortcut, a new tab, the agent), and so is its leaving (the agent
    // went to an address: the page has the keyboard, as after Enter). Not
    // being edited, the box shows the address from its start: the host,
    // whose page it is.
    if b.focus == Focus::Address {
        ui.focus_text_input("address");
    } else {
        ui.blur_text_input("address");
        ui.set_text_input_selection("address", 0, 0);
    }
    let resp = ui.text_input(input, "address", &mut b.address.text, placeholder);
    if resp.focused && b.focus != Focus::Address {
        // Clicked into: the whole address, to replace or edit.
        b.focus_address(None);
        let len = b.address.text.len();
        ui.set_text_input_selection("address", 0, len);
    } else if !resp.focused && b.focus == Focus::Address && ui.input.pressed[0] {
        b.focus = Focus::Page;
        b.address.editing = false;
        b.sync_address();
    }
    if resp.changed {
        b.address.editing = true;
        b.suggest();
    }
    if resp.submitted {
        let chosen = b.address.selected.and_then(|i| b.address.suggestions.get(i)).map(|s| s.url.clone());
        let text = chosen.unwrap_or_else(|| b.address.text.clone());
        b.navigate(&text);
        ui.state.focus = None;
    }
    if !is_new {
        let icon_color = if bookmarked { t.warning } else { t.text_dim };
        let tip = if bookmarked { "Remove the bookmark (Ctrl+D)" } else { "Bookmark this page (Ctrl+D)" };
        let id = ui.id("star");
        let r = ui.interact(id, star);
        ui.icon(star, Icon::Star, 16.0, if r.hovered { t.text } else { icon_color });
        if r.hovered {
            ui.tooltip(id, star, tip);
        }
        if r.clicked {
            b.toggle_bookmark();
        }
    }
    b.address_rect = field;
}

/// What opens over the page: the address bar's suggestions, the
/// downloads, the menu.
pub fn overlays(b: &mut Browser, ui: &mut Ui) {
    if !b.fullscreen {
        suggestions(b, ui);
        if b.downloads_open {
            downloads_panel(b, ui);
        }
    }
    main_menu(b, ui);
}

fn suggestions(b: &mut Browser, ui: &mut Ui) {
    if b.focus != Focus::Address || b.address.suggestions.is_empty() {
        return;
    }
    let t = ui.theme().clone();
    let field = b.address_rect;
    let row = 40;
    let list = Rect::new(field.x, field.bottom() + 4, field.w, row * b.address.suggestions.len() as i32 + 8);
    ui.cover(list);
    b.overlays.push(list);
    ui.canvas.draw_shadow(list.translate(0, 4), 10, 16, Color::rgba(0, 0, 0, 120));
    ui.canvas.fill_rounded_rect(list, t.radius_large, t.surface);
    ui.canvas.stroke_rounded_rect(list, t.radius_large, 1.0, t.border_strong);
    let mut go = None;
    for (i, s) in b.address.suggestions.iter().enumerate() {
        let r = Rect::new(list.x + 4, list.y + 4 + row * i as i32, list.w - 8, row);
        let id = ui.id(&format!("suggestion-{i}"));
        let resp = ui.interact(id, r);
        if resp.hovered || b.address.selected == Some(i) {
            ui.canvas.fill_rounded_rect(r, t.radius, t.control_hover);
        }
        let icon = if s.search {
            Icon::Search
        } else if s.bookmarked {
            Icon::Star
        } else {
            Icon::Clock
        };
        ui.icon(Rect::new(r.x + 10, r.y + 11, 18, 18), icon, 15.0, t.text_dim);
        let text_w = r.w - 50;
        let (first, second) = if s.search {
            (s.title.clone(), format!("Search with {}", b.store.settings.engine().name))
        } else if s.title.is_empty() {
            (url::display(&s.url), String::new())
        } else {
            (s.title.clone(), url::display(&s.url))
        };
        let first = fit(ui, &first, Font::Regular, t.font_size, text_w * 6 / 10);
        let w1 = ui.measure(&first, Font::Regular, t.font_size) as i32;
        ui.label(Rect::new(r.x + 38, r.y, w1 + 4, r.h), &first, Font::Regular, t.font_size, t.text, Align::Left);
        if !second.is_empty() {
            let rest = text_w - w1 - 16;
            let second = fit(ui, &format!("— {second}"), Font::Regular, t.small_size, rest);
            ui.label(
                Rect::new(r.x + 46 + w1, r.y, rest, r.h),
                &second,
                Font::Regular,
                t.small_size,
                t.text_dim,
                Align::Left,
            );
        }
        if resp.clicked {
            go = Some(s.url.clone());
        }
    }
    if let Some(u) = go {
        b.navigate(&u);
        ui.state.focus = None;
    }
}

fn downloads_panel(b: &mut Browser, ui: &mut Ui) {
    let t = ui.theme().clone();
    let w = 380;
    let rows = b.downloads.len().clamp(1, 6) as i32;
    let panel = Rect::new(ui.width - w - 12, STRIP + TOOLBAR - 2, w, 52 + rows * 54);
    ui.cover(panel);
    b.overlays.push(panel);
    ui.canvas.draw_shadow(panel.translate(0, 4), 10, 16, Color::rgba(0, 0, 0, 120));
    ui.canvas.fill_rounded_rect(panel, t.radius_large, t.surface);
    ui.canvas.stroke_rounded_rect(panel, t.radius_large, 1.0, t.border_strong);
    let inner = panel.inset(14, 10, 14, 10);
    ui.label(Rect::new(inner.x, inner.y, inner.w - 80, 28), "Downloads", Font::Bold, t.font_size, t.text, Align::Left);
    let finished = b.downloads.iter().any(|d| d.end.is_some());
    if finished && ui.button(Rect::new(inner.right() - 70, inner.y, 70, 26), "Clear") {
        b.downloads.retain(|d| d.end.is_none());
    }
    if b.downloads.is_empty() {
        ui.label(
            Rect::new(inner.x, inner.y + 34, inner.w, 40),
            "Files you download go to Downloads.",
            Font::Regular,
            t.font_size,
            t.text_dim,
            Align::Left,
        );
    }
    let mut cancel = None;
    for (i, d) in b.downloads.iter().rev().take(6).enumerate() {
        let r = Rect::new(inner.x, inner.y + 36 + 54 * i as i32, inner.w, 50);
        let name = fit(ui, &d.name, Font::Regular, t.font_size, r.w - 50);
        ui.icon(Rect::new(r.x, r.y + 6, 20, 20), Icon::File, 16.0, t.text_dim);
        ui.label(Rect::new(r.x + 28, r.y, r.w - 60, 24), &name, Font::Regular, t.font_size, t.text, Align::Left);
        let status = match &d.end {
            None => match d.size {
                Some(size) => format!("{} of {}", downloads::size(d.received), downloads::size(size)),
                None => downloads::size(d.received),
            },
            Some(crate::engine::DownloadEnd::Done) => format!("{} — in Downloads", downloads::size(d.received)),
            Some(crate::engine::DownloadEnd::Cancelled) => "Cancelled".into(),
            Some(crate::engine::DownloadEnd::Failed(why)) => format!("Failed: {why}"),
        };
        let status = fit(ui, &status, Font::Regular, t.small_size, r.w - 60);
        ui.label(
            Rect::new(r.x + 28, r.y + 22, r.w - 60, 20),
            &status,
            Font::Regular,
            t.small_size,
            t.text_dim,
            Align::Left,
        );
        if d.end.is_none() {
            ui.progress(Rect::new(r.x + 28, r.y + 44, r.w - 70, 3), d.fraction as f32);
            if ui.icon_button(Rect::new(r.right() - 28, r.y + 8, 26, 26), Icon::Close, "Cancel") {
                cancel = Some(d.request);
            }
        }
    }
    if let Some(request) = cancel {
        b.engine.download_cancel(request);
    }
    // A click elsewhere closes it.
    if ui.input.pressed[0] && ui.input.pointer.is_some_and(|(x, y)| !panel.contains(x, y)) {
        b.downloads_open = false;
    }
}

fn main_menu(b: &mut Browser, ui: &mut Ui) {
    let zoom = b.current().map_or(100, |t| t.zoom);
    let has_page = b.current().is_some_and(|t| !t.is_new());
    let items = [
        MenuItem::new("New tab").shortcut("Ctrl+T"),
        MenuItem::new("New private tab").shortcut("Ctrl+Shift+N"),
        MenuItem::new("Reopen closed tab").shortcut("Ctrl+Shift+T"),
        MenuItem::separator(),
        MenuItem::new("History").shortcut("Ctrl+H"),
        MenuItem::new("Bookmarks").shortcut("Ctrl+Shift+O"),
        MenuItem::new("Downloads").shortcut("Ctrl+J"),
        MenuItem::separator(),
        MenuItem::new("Find in page").shortcut("Ctrl+F").enabled(has_page),
        MenuItem::new(&format!("Zoom in ({zoom}%)")).shortcut("Ctrl+=").enabled(has_page),
        MenuItem::new("Zoom out").shortcut("Ctrl+-").enabled(has_page),
        MenuItem::new("Actual size").shortcut("Ctrl+0").enabled(has_page && zoom != 100),
        MenuItem::new("Full screen").shortcut("F11").checked(b.fullscreen),
        MenuItem::separator(),
        MenuItem::new("Settings"),
        MenuItem::new("About Browser"),
    ];
    let Some(chosen) = ui.context_menu("main-menu", &items) else { return };
    match chosen {
        0 => {
            b.new_tab(false);
            ui.focus_text_input("address");
        }
        1 => {
            b.new_tab(true);
            ui.focus_text_input("address");
        }
        2 => b.reopen_closed(),
        4 => b.modal = Some(Modal::History { query: String::new() }),
        5 => b.modal = Some(Modal::Bookmarks { query: String::new() }),
        6 => b.downloads_open = true,
        8 => {
            b.find = Some((String::new(), false));
            b.focus = Focus::Find;
            ui.focus_text_input("find");
        }
        9 => b.zoom(Some(true)),
        10 => b.zoom(Some(false)),
        11 => b.zoom(None),
        12 => b.toggle_fullscreen(),
        14 => b.modal = Some(Modal::settings(&b.store.settings)),
        15 => b.modal = Some(Modal::About),
        _ => {}
    }
}

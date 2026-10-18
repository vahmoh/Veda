// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The browser: its tabs, what the engine says of their pages, and what
//! the user does with them. Drawing is [`crate::chrome`]'s (the tabs and
//! the toolbar), [`crate::page`]'s (the page, and the browser's own pages
//! in its place) and [`crate::dialogs`]'; the voice agent's hands are
//! [`crate::agent`]'s.
//!
//! Input goes to the page as it came ([`ui::Input::events`]), key releases
//! too, when the page has the keyboard: the browser's shortcuts first
//! (Ctrl+T, Ctrl+L...), which a page never sees.

use std::collections::BTreeMap;

use abi::RawHandle;
use browser_core::address;
use browser_core::downloads;
use browser_core::keys;
use browser_core::session::{SavedTab, Session};
use browser_core::settings::{self, Home};
use browser_core::tabs::Tabs;
use browser_core::url;
use gfx::Rect;
use proto::display::{WindowEvent, WindowState};
use proto::input::keys as code;
use ui::{App, Ui};

use crate::dialogs::Modal;
use crate::engine::{self, Engine, Event, Failure, MenuItem, Pointer, Request, ViewId};
use crate::instance::Instance;
use crate::network::Network;
use crate::store::{self, Kept, Store};
use crate::tab::{Problem, Tab};

/// Where the keyboard is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Page,
    Address,
    Find,
}

/// The address bar: what it shows, and what it suggests while it is
/// edited.
#[derive(Debug, Default)]
pub struct Address {
    pub text: String,
    /// It is being edited (its text is the user's, not the page's).
    pub editing: bool,
    pub suggestions: Vec<Suggestion>,
    pub selected: Option<usize>,
}

/// What the address bar suggests: a page visited or kept, or searching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub url: String,
    pub title: String,
    pub bookmarked: bool,
    /// Searching for the words typed, with the search engine.
    pub search: bool,
}

/// A menu of the page's, open: a `<select>`'s options, or its context
/// menu.
pub struct PageMenu {
    pub request: Request,
    pub items: Vec<MenuItem>,
    /// Where it opens, in the window.
    pub at: (i32, i32),
    /// It is a `<select>`'s (whose chosen option is marked).
    pub options: bool,
    /// It was opened in the frame before (the menu is the window's from
    /// the next).
    pub opened: bool,
}

pub struct Download {
    pub request: Request,
    pub name: String,
    pub path: String,
    pub received: u64,
    pub size: Option<u64>,
    pub fraction: f64,
    pub end: Option<engine::DownloadEnd>,
}

/// A script run for the agent, and what it gave.
pub enum Script {
    Waiting,
    Done(Result<String, String>),
}

pub struct Browser {
    pub engine: Engine,
    instance: Option<Instance>,
    /// The network's state: pages that failed for want of it load again
    /// once it is up.
    network: Network,
    pub tabs: Tabs<Tab>,
    next_tab: u64,
    /// Tabs closed, to open again (the latest last).
    closed: Vec<(String, String)>,
    pub store: Store,
    pub downloads: Vec<Download>,
    pub address: Address,
    pub focus: Focus,
    /// Finding in the page: what is found, and whether the case counts.
    pub find: Option<(String, bool)>,
    pub modal: Option<Modal>,
    pub page_menu: Option<PageMenu>,
    pub downloads_open: bool,
    /// What a page copied, for the clipboard at the next frame.
    copied: Option<String>,
    /// The window state to ask for at the next frame.
    want_state: Option<WindowState>,
    pub window_focused: bool,
    /// The window is full screen because a page asked.
    pub fullscreen: bool,
    /// The page's area in the window, as last drawn.
    pub page_rect: Rect,
    /// The address bar's, as last drawn.
    pub address_rect: Rect,
    /// What the browser drew over the page this frame (its menus and
    /// panels): the page gets no pointer there.
    pub overlays: Vec<Rect>,
    /// A button went down in the page: the pointer is its until it is up.
    captured: [bool; 3],
    pub pointer: Option<(i32, i32)>,
    /// What was told to the engine of the shown view: which it is, and its
    /// focus (active, focused).
    told_shown: Option<ViewId>,
    told_focus: Option<(ViewId, bool, bool)>,
    /// Keys whose presses were the browser's: their releases are too.
    swallowed: Vec<u16>,
    pub scripts: BTreeMap<u64, Script>,
    next_script: u64,
    /// A note shown for a moment at the bottom, and until when.
    pub toast: Option<(String, u64)>,
}

/// What the user agent adds of the browser's own.
const APPLICATION: &str = concat!("VedaBrowser/", env!("CARGO_PKG_VERSION"));

impl Browser {
    pub fn new(engine: Engine, instance: Option<Instance>, store: Store, urls: &[String]) -> Browser {
        let mut b = Browser {
            engine,
            instance,
            network: Network::new(),
            tabs: Tabs::new(),
            next_tab: 1,
            closed: Vec::new(),
            store,
            downloads: Vec::new(),
            address: Address::default(),
            focus: Focus::Address,
            find: None,
            modal: None,
            page_menu: None,
            downloads_open: false,
            copied: None,
            want_state: None,
            window_focused: true,
            fullscreen: false,
            page_rect: Rect::new(0, 0, 0, 0),
            address_rect: Rect::new(0, 0, 0, 0),
            overlays: Vec::new(),
            captured: [false; 3],
            pointer: None,
            told_shown: None,
            told_focus: None,
            swallowed: Vec::new(),
            scripts: BTreeMap::new(),
            next_script: 1,
            toast: None,
        };
        if urls.is_empty() && b.store.settings.restore {
            let session = Store::session();
            for t in &session.tabs {
                b.open(Some(&t.url), false, false, false);
            }
            b.tabs.show(session.shown);
        }
        for u in urls {
            b.open(Some(u), false, false, true);
        }
        if b.tabs.is_empty() {
            b.new_tab(false);
        }
        b.focus = if b.current().is_some_and(Tab::is_new) { Focus::Address } else { Focus::Page };
        b.sync_address();
        b
    }

    /// The engine's configuration as the browser starts it: its data,
    /// cache, downloads and temporary files' folders.
    pub fn engine_config_dirs() -> [String; 4] {
        [store::engine_data_dir(), store::cache_dir(), store::downloads_dir(), store::temporary_dir()]
            .map(|dir| dir.to_string_lossy().into_owned())
    }

    pub fn application() -> &'static str {
        APPLICATION
    }

    pub fn current(&self) -> Option<&Tab> {
        self.tabs.current()
    }

    fn tab_of(&mut self, view: ViewId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.view == Some(view))
    }

    fn index_of(&self, view: ViewId) -> Option<usize> {
        self.tabs.position(|t| t.view == Some(view))
    }

    pub fn now() -> u64 {
        rt::time::now_ns()
    }

    /// Seconds since 1970, which history keeps.
    fn wall_clock() -> u64 {
        rt::time::unix_time_ns() / 1_000_000_000
    }

    pub fn note(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Self::now() + 3_000_000_000));
    }

    // --- tabs -----------------------------------------------------------

    /// Opens a tab: at `url`, or the home page; next to the shown one (a
    /// page opened from it) or at the end; shown if `show`.
    pub fn open(&mut self, url: Option<&str>, private: bool, next_to_shown: bool, show: bool) -> usize {
        let tab = Tab::new(self.next_tab, private, self.store.settings.zoom);
        self.next_tab += 1;
        let i = self.tabs.open(tab, next_to_shown, show);
        let target = match url {
            Some(u) => Some(address::resolve(u, self.store.settings.engine()).unwrap_or_else(|| u.to_string())),
            None => match &self.store.settings.home {
                Home::Page(p) => Some(p.clone()),
                Home::NewTab => None,
            },
        };
        if let Some(target) = target.filter(|t| t != url::NEW_TAB) {
            self.go(i, &target);
        }
        if show {
            self.sync_address();
        }
        i
    }

    /// A new tab, shown, the address bar ready for what to go to.
    pub fn new_tab(&mut self, private: bool) {
        self.open(Some(url::NEW_TAB), private, false, true);
        self.focus_address(None);
    }

    pub fn close_tab(&mut self, i: usize) {
        let Some(tab) = self.tabs.close(i) else { return };
        if let Some(view) = tab.view {
            self.engine.destroy_view(view);
            if self.told_shown == Some(view) {
                self.told_shown = None;
            }
        }
        if let Some(menu) = self.page_menu.take() {
            self.engine.menu_reply(menu.request, None);
        }
        if !tab.private && !tab.is_new() {
            self.closed.push((tab.url.clone(), tab.title.clone()));
            if self.closed.len() > 25 {
                self.closed.remove(0);
            }
        }
        if self.tabs.is_empty() {
            self.new_tab(false);
        }
        self.after_switch();
    }

    pub fn reopen_closed(&mut self) {
        if let Some((url, _)) = self.closed.pop() {
            self.open(Some(&url), false, false, true);
            self.focus = Focus::Page;
        }
    }

    pub fn show_tab(&mut self, i: usize) {
        if self.tabs.show(i) {
            self.after_switch();
        }
    }

    pub fn cycle(&mut self, forward: bool) {
        self.tabs.cycle(forward);
        self.after_switch();
    }

    /// The shown tab changed: the address bar and the keyboard follow.
    fn after_switch(&mut self) {
        self.address.editing = false;
        self.sync_address();
        self.find = None;
        self.focus = if self.current().is_some_and(Tab::is_new) { Focus::Address } else { Focus::Page };
        if self.fullscreen {
            self.leave_fullscreen();
        }
    }

    /// The address bar shows the shown page's address (unless it is being
    /// edited).
    pub fn sync_address(&mut self) {
        if self.address.editing {
            return;
        }
        self.address.text = self.current().map(|t| url::display(&t.url)).unwrap_or_default();
        self.address.suggestions.clear();
        self.address.selected = None;
    }

    pub fn focus_address(&mut self, text: Option<&str>) {
        self.focus = Focus::Address;
        self.address.editing = true;
        match text {
            Some(t) => self.address.text = t.to_string(),
            // The full address, to edit.
            None => {
                self.address.text = self.current().filter(|t| !t.is_new()).map(|t| t.url.clone()).unwrap_or_default()
            }
        }
        self.suggest();
    }

    /// The tab's view, made if it has none.
    fn view_of(&mut self, i: usize) -> Option<ViewId> {
        let tab = self.tabs.get(i)?;
        if let Some(v) = tab.view {
            return Some(v);
        }
        let private = tab.private;
        let view = self.engine.create_view(private, None)?;
        let zoom = self.tabs.get(i).map_or(100, |t| t.zoom);
        let tab = self.tabs.get_mut(i)?;
        tab.view = Some(view);
        tab.size = (0, 0);
        if zoom != 100 {
            self.engine.zoom(view, f64::from(zoom) / 100.0);
        }
        Some(view)
    }

    /// Tab `i` goes to `target`, an address.
    pub fn go(&mut self, i: usize, target: &str) {
        if target == url::NEW_TAB {
            return;
        }
        let Some(view) = self.view_of(i) else {
            self.note("The page could not be opened");
            return;
        };
        if let Some(tab) = self.tabs.get_mut(i) {
            tab.url = target.to_string();
            tab.problem = None;
            tab.loading = true;
            tab.progress = 0.05;
        }
        self.engine.load(view, target);
    }

    /// What the address bar's text means, gone to in the shown tab.
    pub fn navigate(&mut self, text: &str) {
        let Some(target) = address::resolve(text, self.store.settings.engine()) else { return };
        let i = self.tabs.shown();
        if let Some(tab) = self.tabs.get_mut(i) {
            tab.typed =
                matches!(address::interpret(text), Some(address::Meaning::Go(_))).then(|| text.trim().to_string());
        }
        self.address.editing = false;
        self.go(i, &target);
        self.focus = Focus::Page;
        self.sync_address();
    }

    pub fn back(&mut self) {
        if let Some(v) = self.current().and_then(|t| t.view) {
            self.engine.go(v, -1);
        }
    }

    pub fn forward(&mut self) {
        if let Some(v) = self.current().and_then(|t| t.view) {
            self.engine.go(v, 1);
        }
    }

    pub fn reload(&mut self, bypass_cache: bool) {
        let Some(tab) = self.tabs.current_mut() else { return };
        match (&tab.problem, tab.view) {
            // An error page tries again where it failed.
            (Some(Problem::Failed { uri, .. }), _) => {
                let uri = uri.clone();
                let i = self.tabs.shown();
                self.go(i, &uri);
            }
            (_, Some(v)) => {
                tab.problem = None;
                self.engine.reload(v, bypass_cache);
            }
            _ => {}
        }
    }

    /// Loads again the pages that could not be loaded for want of a network
    /// (it has changed).
    fn retry_unreached(&mut self) {
        let unreached: Vec<(usize, String)> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(i, t)| match &t.problem {
                Some(Problem::Failed { uri, failure: Failure::Network | Failure::Host, .. }) => Some((i, uri.clone())),
                _ => None,
            })
            .collect();
        for (i, uri) in unreached {
            self.go(i, &uri);
        }
    }

    pub fn stop(&mut self) {
        if let Some(v) = self.current().and_then(|t| t.view) {
            self.engine.stop(v);
        }
    }

    pub fn home(&mut self) {
        let i = self.tabs.shown();
        match self.store.settings.home.clone() {
            Home::Page(p) => self.go(i, &p),
            Home::NewTab => {
                // The new tab page is the browser's: the view goes.
                if let Some(tab) = self.tabs.get_mut(i)
                    && let Some(view) = tab.view.take()
                {
                    self.engine.destroy_view(view);
                    let zoom = tab.zoom;
                    *tab = Tab::new(tab.id, tab.private, zoom);
                }
                self.focus_address(Some(""));
            }
        }
    }

    /// Zooms the shown page a step in (`Some(true)`), out, or back to the
    /// settings' (`None`).
    pub fn zoom(&mut self, step: Option<bool>) {
        let default = self.store.settings.zoom;
        let Some(tab) = self.tabs.current_mut() else { return };
        tab.zoom = match step {
            Some(up) => settings::zoom_step(tab.zoom, up),
            None => default,
        };
        let z = tab.zoom;
        if let Some(v) = tab.view {
            self.engine.zoom(v, f64::from(z) / 100.0);
        }
        self.note(format!("Zoom {z}%"));
    }

    pub fn toggle_bookmark(&mut self) {
        let Some(tab) = self.current().filter(|t| !t.is_new()) else { return };
        let (u, title) = (tab.url.clone(), tab.name());
        let kept = self.store.bookmarks.toggle(&u, &title);
        self.store.changed(Kept::Bookmarks, Self::now());
        self.note(if kept { "Bookmarked" } else { "Bookmark removed" });
    }

    pub fn find(&mut self, text: &str, case_sensitive: bool) {
        self.find = Some((text.to_string(), case_sensitive));
        if let Some(v) = self.current().and_then(|t| t.view) {
            self.engine.find(v, (!text.is_empty()).then_some(text), case_sensitive);
        }
    }

    pub fn find_next(&mut self, backwards: bool) {
        if let Some(v) = self.current().and_then(|t| t.view) {
            self.engine.find_next(v, backwards);
        }
    }

    pub fn close_find(&mut self) {
        if self.find.take().is_some() {
            if let Some(v) = self.current().and_then(|t| t.view) {
                self.engine.find(v, None, false);
            }
            if let Some(t) = self.tabs.current_mut() {
                t.found = None;
            }
        }
        self.focus = Focus::Page;
    }

    pub fn toggle_fullscreen(&mut self) {
        if self.fullscreen {
            self.leave_fullscreen();
        } else {
            self.fullscreen = true;
            self.want_state = Some(WindowState::Fullscreen);
        }
    }

    fn leave_fullscreen(&mut self) {
        self.fullscreen = false;
        self.want_state = Some(WindowState::Normal);
        for t in self.tabs.iter_mut() {
            if std::mem::take(&mut t.fullscreen)
                && let Some(v) = t.view
            {
                self.engine.fullscreen(v, false);
            }
        }
    }

    /// Runs `script` in the shown page; its result comes as
    /// `scripts[number]`.
    pub fn run_script(&mut self, script: &str) -> Option<u64> {
        let view = self.current().and_then(|t| t.view)?;
        let n = self.next_script;
        self.next_script += 1;
        self.scripts.insert(n, Script::Waiting);
        self.engine.run_script(view, n, script);
        Some(n)
    }

    // --- the address bar's suggestions -----------------------------------

    pub fn suggest(&mut self) {
        let typed = self.address.text.trim().to_string();
        self.address.selected = None;
        self.address.suggestions.clear();
        if typed.is_empty() || !self.address.editing {
            return;
        }
        let now = Self::wall_clock();
        let mut out: Vec<Suggestion> = Vec::new();
        if let Some(address::Meaning::Search(words)) = address::interpret(&typed) {
            out.push(Suggestion {
                url: self.store.settings.engine().results(&words),
                title: words,
                bookmarked: false,
                search: true,
            });
        }
        for b in self.store.bookmarks.suggest(&typed, 3) {
            out.push(Suggestion { url: b.url.clone(), title: b.title.clone(), bookmarked: true, search: false });
        }
        for e in self.store.history.suggest(&typed, now, 8) {
            if out.len() >= 7 {
                break;
            }
            if !out.iter().any(|s| s.url == e.url) {
                out.push(Suggestion { url: e.url.clone(), title: e.title.clone(), bookmarked: false, search: false });
            }
        }
        self.address.suggestions = out;
    }

    // --- the engine's events ---------------------------------------------

    pub fn engine_events(&mut self) {
        for event in self.engine.events() {
            self.event(event);
        }
    }

    fn event(&mut self, event: Event) {
        let shown_view = self.current().and_then(|t| t.view);
        match &event {
            Event::Title(view, title) => {
                let (url, private) = match self.tab_of(*view) {
                    Some(t) => {
                        t.apply(&event);
                        (t.url.clone(), t.private)
                    }
                    None => return,
                };
                if !private && !title.is_empty() {
                    self.store.history.retitle(&url, title);
                    self.store.changed(Kept::History, Self::now());
                }
            }
            Event::Load(view, engine::Load::Committed) => {
                let Some(tab) = self.tab_of(*view) else { return };
                tab.apply(&event);
                let (url, title, private) = (tab.url.clone(), tab.title.clone(), tab.private);
                if !private {
                    self.store.history.visit(&url, &title, Self::wall_clock());
                    self.store.changed(Kept::History, Self::now());
                }
                if shown_view == Some(*view) {
                    self.sync_address();
                }
            }
            Event::Uri(view, _) | Event::LoadFailed { view, .. } | Event::TlsError { view, .. } => {
                if let Some(t) = self.tab_of(*view) {
                    t.apply(&event);
                }
                if shown_view == Some(*view) {
                    self.sync_address();
                }
            }
            Event::NewView { view, opener } => {
                let private = self.tab_of(*opener).is_some_and(|t| t.private);
                let at_opener = self.index_of(*opener).is_some_and(|i| i == self.tabs.shown());
                let mut tab = Tab::new(self.next_tab, private, self.store.settings.zoom);
                self.next_tab += 1;
                tab.view = Some(*view);
                tab.url = String::new();
                self.tabs.open(tab, at_opener, true);
                self.after_switch();
            }
            Event::Close(view) => {
                if let Some(i) = self.index_of(*view) {
                    self.close_tab(i);
                }
            }
            Event::Fullscreen(view, on) => {
                let Some(tab) = self.tab_of(*view) else { return };
                tab.fullscreen = *on;
                if *on && shown_view == Some(*view) {
                    self.fullscreen = true;
                    self.want_state = Some(WindowState::Fullscreen);
                    self.engine.fullscreen(*view, true);
                } else if !*on {
                    self.engine.fullscreen(*view, false);
                    if self.fullscreen {
                        self.fullscreen = false;
                        self.want_state = Some(WindowState::Normal);
                    }
                }
            }
            Event::Dialog { view, request, kind, message, default } => {
                self.show_view(*view);
                self.modal = Some(Modal::dialog(*request, *kind, message, default, self.site_of(*view)));
            }
            Event::Permission { view, request, kind, site } => {
                // What Veda cannot give a page is refused at once: its
                // location, notifications, cameras and microphones.
                use engine::Permission as P;
                match kind {
                    P::Storage => {
                        self.show_view(*view);
                        self.modal = Some(Modal::permission(*request, *kind, site));
                    }
                    P::Geolocation | P::Notifications | P::DeviceInfo | P::Media | P::Other => {
                        self.engine.permission_reply(*request, false)
                    }
                }
            }
            Event::Auth { view, request, host, realm, proxy } => {
                self.show_view(*view);
                self.modal = Some(Modal::auth(*request, host, realm, *proxy));
            }
            Event::FileChooser { view, request, multiple, .. } => {
                self.show_view(*view);
                self.modal = Some(Modal::files(*request, *multiple));
            }
            Event::OptionMenu { view, request, items, at } => {
                if shown_view != Some(*view) {
                    self.engine.menu_reply(*request, None);
                    return;
                }
                let x = self.page_rect.x + at.0;
                let y = self.page_rect.y + at.1 + at.3;
                self.open_page_menu(*request, items.clone(), (x, y), true);
            }
            Event::ContextMenu { view, request, items, .. } => {
                if shown_view != Some(*view) {
                    self.engine.menu_reply(*request, None);
                    return;
                }
                let at = self.pointer.unwrap_or((self.page_rect.x + 8, self.page_rect.y + 8));
                self.open_page_menu(*request, items.clone(), at, false);
            }
            Event::MenuClosed(request) => {
                if self.page_menu.as_ref().is_some_and(|m| m.request == *request) {
                    self.page_menu = None;
                }
            }
            Event::Download { request, uri, suggested, size } => self.download(*request, uri, suggested, *size),
            Event::DownloadProgress { request, received, fraction } => {
                if let Some(d) = self.downloads.iter_mut().find(|d| d.request == *request) {
                    d.received = *received;
                    d.fraction = *fraction;
                }
            }
            Event::DownloadDone { request, end, path } => {
                let Some(d) = self.downloads.iter_mut().find(|d| d.request == *request) else { return };
                d.end = Some(end.clone());
                if let Some(p) = path {
                    d.path = p.clone();
                }
                let name = d.name.clone();
                match end {
                    engine::DownloadEnd::Done => self.note(format!("Downloaded {name}")),
                    engine::DownloadEnd::Failed(why) => self.note(format!("{name} could not be downloaded: {why}")),
                    engine::DownloadEnd::Cancelled => {}
                }
            }
            Event::ScriptResult { request, result, .. } => {
                if let Some(s) = self.scripts.get_mut(request) {
                    *s = Script::Done(result.clone());
                }
            }
            Event::Clipboard(text) => self.copied = Some(text.clone()),
            Event::Frame(_) => {}
            Event::Load(view, _)
            | Event::Progress(view, _)
            | Event::History { view, .. }
            | Event::Security(view, _)
            | Event::Cursor(view, _)
            | Event::Icon { view, .. }
            | Event::Hover(view, _)
            | Event::Crashed(view, _)
            | Event::Found(view, _)
            | Event::InsecureContent(view) => {
                if let Some(t) = self.tab_of(*view) {
                    t.apply(&event);
                }
            }
        }
    }

    /// The host of `view`'s page, for a dialog to say whose it is.
    fn site_of(&mut self, view: ViewId) -> String {
        self.tab_of(view).map(|t| url::short(&t.url)).unwrap_or_default()
    }

    /// Shows the tab of `view`, which asks the user something.
    fn show_view(&mut self, view: ViewId) {
        if let Some(i) = self.index_of(view)
            && i != self.tabs.shown()
        {
            self.show_tab(i);
        }
    }

    fn open_page_menu(&mut self, request: Request, items: Vec<MenuItem>, at: (i32, i32), options: bool) {
        if let Some(old) = self.page_menu.take() {
            self.engine.menu_reply(old.request, None);
        }
        self.page_menu = Some(PageMenu { request, items, at, options, opened: false });
    }

    /// A download begins: it goes to the Downloads folder, under a name
    /// of its own.
    fn download(&mut self, request: Request, uri: &str, suggested: &str, size: Option<u64>) {
        let dir = store::downloads_dir();
        let _ = std::fs::create_dir_all(&dir);
        let name = downloads::file_name(suggested, uri);
        // Not one that is there, nor one another download is to have.
        let taken = |n: &str| dir.join(n).exists() || self.downloads.iter().any(|d| d.name == n && d.end.is_none());
        let name = downloads::unique(&name, taken);
        let path = dir.join(&name).to_string_lossy().into_owned();
        self.engine.download_reply(request, Some(&path));
        self.note(format!("Downloading {name}"));
        self.downloads.push(Download { request, name, path, received: 0, size, fraction: 0.0, end: None });
    }

    // --- input to the page ------------------------------------------------

    /// The window's input this frame, as it came: the browser's shortcuts,
    /// and what goes to the page.
    pub fn input(&mut self, ui: &mut Ui) {
        let events = ui.input.events.clone();
        let menus = self.modal.is_some() || self.page_menu.is_some();
        for ev in &events {
            match ev {
                WindowEvent::Focus { focused } => self.window_focused = *focused,
                WindowEvent::Key { code, pressed, modifiers, text, .. } => {
                    if *pressed && self.shortcut(*code, *modifiers, ui) {
                        self.swallowed.push(*code);
                        continue;
                    }
                    if !*pressed && let Some(i) = self.swallowed.iter().position(|c| c == code) {
                        self.swallowed.swap_remove(i);
                        continue;
                    }
                    if self.focus == Focus::Page && !menus {
                        self.key_to_page(*code, *pressed, *modifiers, text, ui);
                    }
                }
                WindowEvent::PointerMove { x, y } => {
                    self.pointer = Some((*x, *y));
                    if !menus {
                        self.pointer_to_page(*x, *y, None, ui.input.modifiers);
                    }
                }
                WindowEvent::PointerButton { x, y, button, pressed, clicks } => {
                    self.pointer = Some((*x, *y));
                    if !menus || !*pressed {
                        self.pointer_to_page(*x, *y, Some((*button, *pressed, *clicks)), ui.input.modifiers);
                    }
                }
                WindowEvent::PointerLeave {} => {
                    if !self.captured.iter().any(|c| *c)
                        && let Some(v) = self.page_view()
                    {
                        self.engine.pointer(v, Pointer::Leave, 0.0, 0.0, 0);
                    }
                }
                WindowEvent::Scroll { dx, dy } => {
                    if menus {
                        continue;
                    }
                    let Some((x, y)) = self.pointer.filter(|&(x, y)| self.page_rect.contains(x, y)) else { continue };
                    let Some(v) = self.page_view() else { continue };
                    let m = ui.input.modifiers;
                    // Horizontal turns are to the right, Veda's way, and
                    // left, WebKit's; Shift turns the wheel sideways.
                    let (mut sx, mut sy) = (-f64::from(*dx), f64::from(*dy));
                    if m & keys::modifiers::SHIFT != 0 && sx == 0.0 {
                        (sx, sy) = (sy, 0.0);
                    }
                    let (px, py) = self.to_page(x, y);
                    self.engine.scroll(v, px, py, sx, sy, m);
                }
                _ => {}
            }
        }
    }

    /// The shown tab's view, if its page is what is shown (not an error
    /// page of the browser's).
    fn page_view(&self) -> Option<ViewId> {
        self.current().filter(|t| t.problem.is_none()).and_then(|t| t.view)
    }

    fn to_page(&self, x: i32, y: i32) -> (f64, f64) {
        (f64::from(x - self.page_rect.x), f64::from(y - self.page_rect.y))
    }

    fn pointer_to_page(&mut self, x: i32, y: i32, button: Option<(u8, bool, u8)>, modifiers: u32) {
        let inside = self.page_rect.contains(x, y) && !self.overlays.iter().any(|o| o.contains(x, y));
        let held = self.captured.iter().any(|c| *c);
        let Some(view) = self.page_view() else { return };
        let (px, py) = self.to_page(x, y);
        match button {
            None if inside || held => self.engine.pointer(view, Pointer::Move, px, py, modifiers),
            None => {}
            Some((b, pressed, clicks)) => {
                let i = usize::from(b.min(2));
                // Veda's 0, 1 and 2 are left, right and middle; WebKit's
                // 1, 3 and 2.
                let button = [1, 3, 2][i];
                if pressed && inside {
                    self.captured[i] = true;
                    if self.focus != Focus::Page {
                        self.focus = Focus::Page;
                        self.address.editing = false;
                        self.sync_address();
                    }
                    self.engine.pointer(
                        view,
                        Pointer::Down { button, clicks: u32::from(clicks.max(1)) },
                        px,
                        py,
                        modifiers,
                    );
                } else if !pressed && self.captured[i] {
                    self.captured[i] = false;
                    self.engine.pointer(view, Pointer::Up { button }, px, py, modifiers);
                }
            }
        }
    }

    fn key_to_page(&mut self, code: u16, pressed: bool, modifiers: u32, text: &str, ui: &mut Ui) {
        let Some(view) = self.page_view() else { return };
        // A paste pastes the system's clipboard.
        let ctrl = modifiers & keys::modifiers::CTRL != 0;
        let shift = modifiers & keys::modifiers::SHIFT != 0;
        if pressed && ((ctrl && code == code::V) || (shift && code == code::INSERT)) {
            let text = ui.clipboard();
            self.engine.clipboard(&text);
        }
        let k = keys::key(code, text, modifiers);
        self.engine.key(view, pressed, k.keycode, k.keysym, modifiers);
    }

    /// The browser's own shortcuts; `true` if `code` was one.
    fn shortcut(&mut self, code: u16, modifiers: u32, ui: &mut Ui) -> bool {
        use keys::modifiers as m;
        let ctrl = modifiers & m::CTRL != 0;
        let shift = modifiers & m::SHIFT != 0;
        let alt = modifiers & m::ALT != 0;
        if self.modal.is_some() {
            return false;
        }
        match (code, ctrl, shift, alt) {
            (code::T, true, true, false) => self.reopen_closed(),
            (code::T, true, false, false) => self.new_tab(false),
            (code::N, true, true, false) => self.new_tab(true),
            (code::W, true, false, false) | (code::F4, true, false, false) => self.close_tab(self.tabs.shown()),
            (code::TAB, true, s, false) => self.cycle(!s),
            (code::PAGEDOWN, true, false, false) => self.cycle(true),
            (code::PAGEUP, true, false, false) => self.cycle(false),
            (code::KEY_1..=code::KEY_8, true, false, false) => self.show_tab(usize::from(code - code::KEY_1)),
            (code::KEY_9, true, false, false) => self.show_tab(self.tabs.len().saturating_sub(1)),
            (code::L, true, false, false) | (code::D, false, false, true) | (code::F6, false, false, false) => {
                self.focus_address(None);
                ui.focus_text_input("address");
                let len = self.address.text.len();
                ui.set_text_input_selection("address", 0, len);
            }
            (code::R, true, s, false) => self.reload(s),
            (code::F5, false, s, false) => self.reload(s),
            (code::ESC, false, false, false) => {
                if self.find.is_some() {
                    self.close_find();
                } else if self.fullscreen {
                    self.leave_fullscreen();
                } else if self.current().is_some_and(|t| t.loading) {
                    self.stop();
                } else if self.focus == Focus::Address {
                    self.address.editing = false;
                    self.sync_address();
                    self.focus = Focus::Page;
                    ui.state.focus = None;
                } else {
                    // The page's own (closing its dialog, leaving a game).
                    return false;
                }
            }
            (code::LEFT, false, false, true) => self.back(),
            (code::RIGHT, false, false, true) => self.forward(),
            (code::HOME, false, false, true) => self.home(),
            (code::F, true, false, false) => {
                if self.find.is_none() {
                    self.find = Some((String::new(), false));
                }
                self.focus = Focus::Find;
                ui.focus_text_input("find");
            }
            (code::F3, false, s, false) | (code::G, true, s, false) => {
                if self.find.is_some() {
                    self.find_next(s);
                } else {
                    return false;
                }
            }
            (code::EQUAL, true, _, false) | (code::KPPLUS, true, _, false) => self.zoom(Some(true)),
            (code::MINUS, true, _, false) | (code::KPMINUS, true, _, false) => self.zoom(Some(false)),
            (code::KEY_0, true, false, false) | (code::KP0, true, false, false) => self.zoom(None),
            (code::F11, false, false, false) => self.toggle_fullscreen(),
            (code::D, true, false, false) => self.toggle_bookmark(),
            (code::H, true, false, false) => self.modal = Some(Modal::History { query: String::new() }),
            (code::O, true, true, false) | (code::B, true, true, false) => {
                self.modal = Some(Modal::Bookmarks { query: String::new() })
            }
            (code::J, true, false, false) => self.downloads_open = !self.downloads_open,
            _ => return false,
        }
        true
    }

    // --- keeping the engine's view as the window shows it ------------------

    /// Tells the engine what the window shows: which view, how large,
    /// whether it has the keyboard.
    pub fn sync_view(&mut self) {
        let shown = self.current().and_then(|t| t.view);
        if shown != self.told_shown {
            if let Some(old) = self.told_shown {
                self.engine.show(old, false);
                self.engine.focus(old, false, false);
            }
            if let Some(new) = shown {
                self.engine.show(new, true);
            }
            self.told_shown = shown;
            self.told_focus = None;
        }
        let Some(view) = shown else { return };
        let size = (self.page_rect.w.max(1), self.page_rect.h.max(1));
        if let Some(tab) = self.tabs.current_mut()
            && tab.size != size
        {
            tab.size = size;
            self.engine.resize(view, size.0, size.1, 1.0);
        }
        let active = self.window_focused;
        let focused = active && self.focus == Focus::Page && self.modal.is_none();
        if self.told_focus != Some((view, active, focused)) {
            self.engine.focus(view, active, focused);
            self.told_focus = Some((view, active, focused));
        }
    }

    // --- what the frame does after the drawing ------------------------------

    pub fn end_frame(&mut self, ui: &mut Ui) {
        if let Some(text) = self.copied.take() {
            ui.set_clipboard(&text);
        }
        if let Some(state) = self.want_state.take() {
            ui.set_window_state(state);
        }
        let now = Self::now();
        if self.toast.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.toast = None;
        }
        if self.network.rewatch(now) {
            self.retry_unreached();
        }
        let title = match self.current() {
            Some(t) if t.private => format!("{} — Private browsing", t.name()),
            Some(t) => format!("{} — Browser", t.name()),
            None => "Browser".into(),
        };
        ui.set_title(&title);
        self.store.save(now, false);
        // What moves on its own: a load's spinner, a toast, a write due, the
        // network service to watch again.
        let mut next = self.store.due();
        if self.tabs.iter().any(|t| t.loading) || self.downloads.iter().any(|d| d.end.is_none()) {
            next = Some(next.map_or(now + 50_000_000, |n| n.min(now + 50_000_000)));
        }
        if let Some((_, until)) = &self.toast {
            next = Some(next.map_or(*until, |n| n.min(*until)));
        }
        if let Some(at) = self.network.rewatch_due() {
            next = Some(next.map_or(at, |n| n.min(at)));
        }
        if let Some(at) = next {
            ui.repaint_at(at);
        }
    }

    /// What closes with the window: the tabs to open again, the files.
    pub fn save_all(&mut self) {
        let tabs: Vec<SavedTab> = self
            .tabs
            .iter()
            .filter(|t| !t.private && !t.is_new() && url::is_web(&t.url))
            .map(|t| SavedTab { url: t.url.clone(), title: t.title.clone() })
            .collect();
        let shown = self.current().map_or(0, |cur| {
            self.tabs
                .iter()
                .filter(|t| !t.private && !t.is_new() && url::is_web(&t.url))
                .position(|t| t.id == cur.id)
                .unwrap_or(0)
        });
        Store::save_session(&Session { tabs, shown });
        self.store.save(Self::now(), true);
    }

    /// Clears what the browser and its engine keep: `history`, and
    /// `what` of the engine's ([`engine::data`]).
    pub fn clear(&mut self, history: bool, what: u32) {
        if history {
            self.store.history.clear_since(0);
            self.store.changed(Kept::History, Self::now());
            self.closed.clear();
        }
        if what != 0 {
            self.engine.clear_data(what, 0);
        }
        self.note("Cleared");
    }
}

impl App for Browser {
    fn update(&mut self, ui: &mut Ui) {
        self.engine_events();
        self.input(ui);
        crate::chrome::draw(self, ui);
        crate::page::draw(self, ui);
        crate::dialogs::draw(self, ui);
        self.sync_view();
        self.end_frame(ui);
    }

    fn wait_handles(&self) -> Vec<(RawHandle, u32)> {
        let mut out = vec![(self.engine.wake_handle(), abi::signals::SIGNALED)];
        // The network's news: index 1, while there are any.
        if let Some(h) = self.network.handle() {
            out.push((h, abi::signals::READABLE | abi::signals::PEER_CLOSED));
        }
        if let Some(i) = &self.instance {
            out.extend(i.wait_handles());
        }
        out
    }

    fn handle_signaled(&mut self, index: usize, _observed: u32) {
        if index == 0 {
            self.engine_events();
            return;
        }
        if index == 1 && self.network.handle().is_some() {
            if self.network.events(Self::now()) {
                self.retry_unreached();
            }
            return;
        }
        let requests = self.instance.as_mut().map(|i| i.requests()).unwrap_or_default();
        for urls in requests {
            if urls.is_empty() {
                self.new_tab(false);
            }
            for u in &urls {
                self.open(Some(u), false, false, true);
                self.focus = Focus::Page;
            }
            self.want_state = self.want_state.or(Some(WindowState::Normal));
        }
    }

    fn close_requested(&mut self) -> bool {
        self.save_all();
        true
    }

    fn paints_every_pixel(&self) -> bool {
        true
    }

    fn agent_info(&self) -> Option<ui::agent::AppAgentInfo> {
        Some(crate::agent::info())
    }

    fn agent_state(&self) -> serde_json::Value {
        crate::agent::state(self)
    }

    fn agent_invoke(&mut self, action: &str, args: &serde_json::Value) -> Result<serde_json::Value, String> {
        crate::agent::invoke(self, action, args)
    }
}

// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A tab: its page (an engine view, once it has one) and what the browser
//! shows of it, kept as the engine tells it ([`Tab::apply`]).

use browser_core::url;
use gfx::Bitmap;
use proto::display::Cursor;

use crate::engine::{Crash, Event, Failure, Request, Security, ViewId};

/// What the tab shows in the page's place.
#[derive(Debug, Clone, PartialEq)]
pub enum Problem {
    /// The page could not be loaded.
    Failed { uri: String, message: String, failure: Failure },
    /// Its certificate is not trusted: the page waits for the user.
    Untrusted { request: Request, uri: String, host: String, subject: String, errors: u32 },
    /// Its process ended.
    Crashed(Crash),
}

pub struct Tab {
    /// The browser's own number for it.
    pub id: u64,
    /// Its page in the engine: none until it goes somewhere (a new tab's
    /// page is the browser's own).
    pub view: Option<ViewId>,
    /// It keeps nothing: no history, no cookies or storage after it.
    pub private: bool,
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub progress: f64,
    pub security: Security,
    pub mixed_content: bool,
    pub can_back: bool,
    pub can_forward: bool,
    /// The page's icon, 16 x 16.
    pub icon: Option<Bitmap>,
    pub cursor: Cursor,
    /// The link under the pointer.
    pub hovered: Option<String>,
    /// Its zoom, in percent.
    pub zoom: u32,
    pub problem: Option<Problem>,
    /// The page asked for the whole screen.
    pub fullscreen: bool,
    /// What finding in it found: how many matches.
    pub found: Option<u32>,
    /// What was typed to come here, if it was typed: on an error page,
    /// searching for it instead is offered.
    pub typed: Option<String>,
    /// The size the view was last given.
    pub size: (i32, i32),
    /// The network's changes as the browser last sent it somewhere
    /// (`network::Network::changes`): the engine's news of the load may
    /// come after the network's of a change that came while it loaded.
    pub network_seen: u64,
}

impl Tab {
    pub fn new(id: u64, private: bool, zoom: u32) -> Tab {
        Tab {
            id,
            view: None,
            private,
            url: url::NEW_TAB.into(),
            title: String::new(),
            loading: false,
            progress: 0.0,
            security: Security::Local,
            mixed_content: false,
            can_back: false,
            can_forward: false,
            icon: None,
            cursor: Cursor::Arrow,
            hovered: None,
            zoom,
            problem: None,
            fullscreen: false,
            found: None,
            typed: None,
            size: (0, 0),
            network_seen: 0,
        }
    }

    /// What its strip and the window's title call it.
    pub fn name(&self) -> String {
        if !self.title.trim().is_empty() {
            return self.title.trim().to_string();
        }
        url::short(&self.url)
    }

    /// Whether it shows the browser's new tab page.
    pub fn is_new(&self) -> bool {
        self.url == url::NEW_TAB && self.view.is_none()
    }

    /// Learns what `event`, one of its view's, says.
    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::Title(_, title) => self.title = title.clone(),
            Event::Uri(_, uri) => {
                if !uri.is_empty() {
                    self.url = uri.clone();
                }
            }
            Event::Load(_, load) => {
                use crate::engine::Load;
                match load {
                    Load::Started => {
                        self.loading = true;
                        self.progress = 0.1;
                        self.problem = None;
                        self.mixed_content = false;
                    }
                    Load::Committed => {
                        self.icon = None;
                        self.found = None;
                    }
                    Load::Finished => {
                        self.loading = false;
                        self.progress = 1.0;
                    }
                    Load::Redirected => {}
                }
            }
            Event::Progress(_, p) => self.progress = *p,
            Event::History { back, forward, .. } => {
                self.can_back = *back;
                self.can_forward = *forward;
            }
            Event::Security(_, s) => self.security = *s,
            Event::InsecureContent(_) => self.mixed_content = true,
            Event::LoadFailed { uri, message, failure, .. } => {
                self.loading = false;
                // Stopped, or downloaded: the page stays as it was.
                if !matches!(failure, Failure::Cancelled | Failure::Content) {
                    self.problem =
                        Some(Problem::Failed { uri: uri.clone(), message: message.clone(), failure: *failure });
                    self.url = uri.clone();
                }
            }
            Event::TlsError { request, uri, host, subject, errors, .. } => {
                self.loading = false;
                self.url = uri.clone();
                self.problem = Some(Problem::Untrusted {
                    request: *request,
                    uri: uri.clone(),
                    host: host.clone(),
                    subject: subject.clone(),
                    errors: *errors,
                });
            }
            Event::Crashed(_, crash) => {
                // For the system's log (not the page's address: the log is
                // not the history's to keep).
                let why = match crash {
                    Crash::Crashed => "its web process crashed",
                    Crash::Memory => "its web process ran out of memory",
                    Crash::Terminated => "its web process was ended",
                };
                eprintln!("browser: a page stopped working: {why}");
                self.loading = false;
                self.problem = Some(Problem::Crashed(*crash));
            }
            Event::Icon { width, height, pixels, .. } => {
                self.icon = (*width > 0 && *height > 0).then(|| icon(*width, *height, pixels));
            }
            Event::Hover(_, link) => self.hovered = link.clone(),
            Event::Cursor(_, name) => self.cursor = cursor(name),
            Event::Found(_, n) => self.found = Some(*n),
            _ => {}
        }
    }
}

/// The page's icon at 16 x 16 pixels, from `width` x `height`.
fn icon(width: i32, height: i32, pixels: &[u32]) -> Bitmap {
    let image = Bitmap { width, height, pixels: pixels.to_vec() };
    if (width, height) == (16, 16) { image } else { image.resized(16, 16) }
}

/// The pointer for a page's CSS cursor.
pub fn cursor(name: &str) -> Cursor {
    match name {
        "pointer" | "alias" | "context-menu" => Cursor::Hand,
        "text" | "vertical-text" => Cursor::Text,
        "wait" | "progress" => Cursor::Busy,
        "crosshair" | "cell" => Cursor::Crosshair,
        "move" | "all-scroll" | "grab" | "grabbing" => Cursor::Move,
        "ew-resize" | "col-resize" | "e-resize" | "w-resize" => Cursor::ResizeHorizontal,
        "ns-resize" | "row-resize" | "n-resize" | "s-resize" => Cursor::ResizeVertical,
        "nwse-resize" | "nw-resize" | "se-resize" => Cursor::ResizeDiagonal,
        "nesw-resize" | "ne-resize" | "sw-resize" => Cursor::ResizeAntiDiagonal,
        "none" => Cursor::Hidden,
        _ => Cursor::Arrow,
    }
}

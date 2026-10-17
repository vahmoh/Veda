// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Running UIs: [`Host`] drives one window (events, frame pacing, drawing);
//! [`run`] is the event loop for the common one-window [`App`], which also
//! serves the voice agent for apps that offer it something (see
//! [`crate::agent`]).
//!
//! A frame is drawn whole, and only what it changed of the frame on screen
//! is presented: a caret's blink, a fading hover or a clock's tick makes the
//! compositor redraw that, not the window.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use abi::{RawHandle, WaitItem, signals};
use gfx::DrawCache;
use proto::display::{Cursor, WindowEvent, WindowSpec};

pub use crate::fonts::load_fonts;

use crate::agent::{AgentServer, AppAgentInfo, Registration};
use crate::fonts::SharedText;
use crate::theme::Theme;
use crate::ui::{Context, Input, Ui, UiState};
use crate::window::{Display, Window, WindowError, connect};

/// An application with one main window.
pub trait App {
    /// Builds and draws one frame.
    fn update(&mut self, ui: &mut Ui);

    /// The user asked to close the window; return `true` to allow it.
    fn close_requested(&mut self) -> bool {
        true
    }

    /// Additional kernel objects to wait on (handle, signals).
    fn wait_handles(&self) -> Vec<(RawHandle, u32)> {
        Vec::new()
    }

    /// One of [`App::wait_handles`] was signalled; a frame follows.
    fn handle_signaled(&mut self, _index: usize, _observed: u32) {}

    /// Called once the window exists, before the first frame.
    fn init(&mut self, _window: &Window) {}

    /// Whether [`App::update`] paints every pixel of the window itself (a
    /// video's stage, a game's view): the frame is then not cleared to the
    /// theme's background first, which saves a write of the whole window.
    fn paints_every_pixel(&self) -> bool {
        false
    }

    /// What the app offers the voice agent: a summary and its actions.
    /// `None` (the default) keeps the app to itself.
    fn agent_info(&self) -> Option<AppAgentInfo> {
        None
    }

    /// What the app shows right now, for the agent (a JSON value).
    fn agent_state(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// Runs one of the actions from [`App::agent_info`] for the agent.
    /// `args` is a JSON object; the result is a JSON value for the agent
    /// (or a short explanation of what went wrong).
    fn agent_invoke(
        &mut self,
        _action: &str,
        _args: &serde_json::Value,
    ) -> Result<serde_json::Value, alloc::string::String> {
        Err("this app does not offer that".into())
    }
}

/// Serves the agent through an [`App`]'s methods.
struct AppAgent<'a, A: App>(&'a mut A);

impl<A: App> AgentServer for AppAgent<'_, A> {
    fn agent_info(&self) -> AppAgentInfo {
        self.0.agent_info().unwrap_or(AppAgentInfo {
            summary: alloc::string::String::new(),
            actions: Vec::new(),
            has_state: false,
        })
    }

    fn agent_state(&self) -> serde_json::Value {
        self.0.agent_state()
    }

    fn agent_invoke(
        &mut self,
        action: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, alloc::string::String> {
        self.0.agent_invoke(action, args)
    }
}

/// Drives one window: turns window events into UI input, paces frames to the
/// compositor and calls a drawing function.
pub struct Host {
    pub window: Window,
    pub ctx: Context,
    pub state: UiState,
    input: Input,
    /// What drawing keeps from frame to frame.
    cache: DrawCache,
    cursor: Cursor,
    dirty: bool,
    repaint_at: Option<u64>,
    pending: VecDeque<WindowEvent>,
    /// The UI asked to close the window (`Ui::close_window`).
    pub close_requested: bool,
    /// Frames start cleared to the theme's background (see
    /// [`App::paints_every_pixel`]).
    pub clear: bool,
}

impl Host {
    /// A window of `spec`, drawing with the process's text context
    /// ([`SharedText::process`]).
    pub fn new(display: &Display, spec: WindowSpec) -> Result<Host, WindowError> {
        let window = Window::new(display, spec)?;
        let (text, fonts) = SharedText::process();
        let ctx = Context { text, fonts, theme: Theme::dark(), display: display.clone(), window_id: window.id };
        Ok(Host {
            window,
            ctx,
            state: UiState::default(),
            input: Input { focused: true, ..Default::default() },
            cache: DrawCache::new(),
            cursor: Cursor::Arrow,
            dirty: true,
            repaint_at: None,
            pending: VecDeque::new(),
            close_requested: false,
            clear: true,
        })
    }

    /// Forces a redraw at the next opportunity.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// What to wait on for this window.
    pub fn wait_item(&self) -> WaitItem {
        WaitItem {
            handle: self.window.event_channel().raw(),
            signals: signals::READABLE | signals::PEER_CLOSED,
            ..Default::default()
        }
    }

    /// When the next frame is due (absent new events).
    pub fn deadline(&self) -> u64 {
        if (self.dirty || !self.pending.is_empty()) && self.window.can_draw() {
            // A frame is wanted and may be drawn now (e.g. after
            // `invalidate`): do not wait for further events.
            0
        } else if self.dirty {
            abi::DEADLINE_INFINITE // waiting for FrameDone
        } else {
            self.repaint_at.unwrap_or(abi::DEADLINE_INFINITE)
        }
    }

    fn frame(&mut self, draw: &mut impl FnMut(&mut Ui)) {
        self.input.now = rt::time::now_ns();
        let bg = self.ctx.theme.bg;
        let window_state = self.window.state;
        let frame_timing = self.window.frame_timing();
        // What widgets see while an open menu or a modal dialog has the keyboard.
        let quiet = self.input.without_keys();
        let Ok(mut canvas) = self.window.begin_frame_with(&mut self.cache) else { return };
        if self.clear {
            canvas.clear(bg);
        }
        let mut ui = Ui::for_frame(canvas, &mut self.ctx, &mut self.state, &self.input, &quiet);
        ui.window_state = window_state;
        ui.frame_timing = frame_timing;
        draw(&mut ui);
        ui.finish();
        let (repaint, close, cursor, skip) = (ui.repaint_at, ui.close_requested, ui.cursor, ui.skip_present);
        let unclaimed = ui.keys_unclaimed;
        drop(ui);
        if !skip {
            self.present();
        }
        if cursor != self.cursor {
            self.cursor = cursor;
            self.window.set_cursor(cursor);
        }
        self.input.end_frame(unclaimed);
        self.repaint_at = repaint;
        self.close_requested |= close;
    }

    /// Presents the frame just drawn: what it changed of the frame on screen,
    /// or nothing if it changed nothing.
    fn present(&mut self) {
        let _ = match self.window.changes() {
            None => self.window.present(&[]),
            Some(damage) if damage.is_empty() => Ok(()),
            Some(damage) => self.window.present(damage.rects()),
        };
    }

    /// Processes pending window events and draws if needed. Events the UI
    /// does not consume (close requests, shell notifications) are returned.
    pub fn pump(&mut self, mut draw: impl FnMut(&mut Ui)) -> Vec<WindowEvent> {
        let mut unhandled = Vec::new();
        self.pending.extend(self.window.poll_events());
        while let Some(ev) = self.pending.front() {
            if self.input.needs_flush_before(ev) {
                if !self.window.can_draw() {
                    break;
                }
                self.frame(&mut draw);
                continue;
            }
            let ev = self.pending.pop_front().unwrap();
            match &ev {
                WindowEvent::CloseRequested {}
                | WindowEvent::WindowsChanged {}
                | WindowEvent::StartMenuKey {}
                | WindowEvent::AgentKey {}
                | WindowEvent::Appearing {} => unhandled.push(ev.clone()),
                WindowEvent::FrameDone { .. } => {}
                WindowEvent::Configure { .. } => {}
                _ => self.input.apply(&ev),
            }
            if !matches!(ev, WindowEvent::FrameDone { .. }) {
                self.dirty = true;
            }
        }
        if self.repaint_at.is_some_and(|t| rt::time::now_ns() >= t) {
            self.repaint_at = None;
            self.dirty = true;
        }
        if self.dirty && self.window.can_draw() {
            self.dirty = false;
            self.frame(&mut draw);
        }
        unhandled
    }
}

/// Runs `app` in a window described by `spec` until it closes. Returns the
/// process exit code.
pub fn run<A: App>(spec: WindowSpec, mut app: A) -> i32 {
    let display = match connect() {
        Ok(d) => d,
        Err(e) => {
            rt::println!("cannot connect to the display: {:?}", e);
            return 1;
        }
    };
    let mut host = match Host::new(&display, spec) {
        Ok(h) => h,
        Err(e) => {
            rt::println!("cannot create a window: {:?}", e);
            return 1;
        }
    };
    app.init(&host.window);
    // Registered in the background, and again whenever the agent restarts.
    let mut agent = if app.agent_info().is_some() { Registration::start() } else { None };
    loop {
        host.clear = !app.paints_every_pixel();
        let events = host.pump(|ui| app.update(ui));
        if host.window.closed {
            return 0;
        }
        let wants_close = host.close_requested || events.iter().any(|e| matches!(e, WindowEvent::CloseRequested {}));
        if wants_close {
            host.close_requested = false;
            if app.close_requested() {
                return 0;
            }
            host.invalidate();
        }
        let extra = app.wait_handles();
        let mut items = Vec::with_capacity(2 + extra.len());
        items.push(host.wait_item());
        for (h, s) in &extra {
            items.push(WaitItem { handle: *h, signals: *s, ..Default::default() });
        }
        if let Some(reg) = &agent {
            let (handle, signals) = reg.wait_items();
            items.push(WaitItem { handle, signals, ..Default::default() });
        }
        let _ = rt::object::wait_many(&mut items, host.deadline());
        for (i, it) in items.iter().enumerate().skip(1).take(extra.len()) {
            if it.observed & it.signals != 0 {
                app.handle_signaled(i - 1, it.observed);
                host.invalidate();
            }
        }
        if let Some(reg) = &mut agent
            && items.last().is_some_and(|it| it.observed != 0)
        {
            // Registered, the agent asked something (the answer may change
            // what the window shows), or the agent service went away.
            reg.serve(&mut AppAgent(&mut app));
            host.invalidate();
        }
    }
}
